//! The device hub: a single-task actor that owns receiver state and routes
//! platform-namespace messages addressed to `receiver-0`.
//!
//! Running everything in one task (fed by the transport's [`ServerEvent`]
//! stream) preserves the serialized semantics of the original asyncio design
//! without any shared-mutable locking.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use tokio::sync::mpsc;

use vibecast_cast::{namespace as ns, ConnectionHandle, ServerEvent};
use vibecast_messages::{
    extract_request_id, AppAvailabilityResponse, ConnectionMessage, DeviceInfoResponse,
    GetDeviceInfoRequest, InvalidRequestResponse, LaunchErrorResponse, MultizoneGetStatusRequest,
    MultizoneStatusResponse, ReceiverRequest, SetupRequest, SetupResponse, Volume,
};
use vibecast_proto::CastMessage;

use crate::identity::DeviceIdentity;
use crate::status::build_receiver_status;

const RECEIVER_0: &str = "receiver-0";

/// Central hub for connection/subscription tracking and platform message
/// handling. Drive it with [`DeviceHub::run`].
pub struct DeviceHub {
    identity: DeviceIdentity,
    volume: Volume,
    connections: HashMap<u64, ConnectionHandle>,
    /// Senders subscribed to the `receiver-0` transport, keyed by (connection, sender id).
    receiver_subscribers: HashSet<(u64, String)>,
}

impl DeviceHub {
    /// Create a hub with the receiver's default volume (attenuation control).
    #[must_use]
    pub fn new(identity: DeviceIdentity) -> Self {
        Self {
            identity,
            volume: Volume {
                level: 1.0,
                muted: false,
                control_type: Some("attenuation".into()),
                step_interval: Some(0.05),
            },
            connections: HashMap::new(),
            receiver_subscribers: HashSet::new(),
        }
    }

    /// Run the hub, consuming transport events until the channel closes.
    pub async fn run(mut self, mut events: mpsc::Receiver<ServerEvent>) {
        while let Some(event) = events.recv().await {
            match event {
                ServerEvent::Connected(handle) => {
                    self.connections.insert(handle.id(), handle);
                }
                ServerEvent::Disconnected { id, .. } => {
                    self.connections.remove(&id);
                    self.receiver_subscribers.retain(|(conn, _)| *conn != id);
                }
                ServerEvent::Message { handle, message } => {
                    self.handle_message(&handle, message).await;
                }
            }
        }
    }

    async fn handle_message(&mut self, handle: &ConnectionHandle, message: CastMessage) {
        // Phase 4 only serves the platform transport; app-session transports
        // arrive in Phase 6.
        if message.destination_id != RECEIVER_0 {
            tracing::debug!(dest = %message.destination_id, "ignoring message for unknown transport");
            return;
        }

        let Some(payload) = parse_payload(&message) else {
            return;
        };

        match message.namespace.as_str() {
            ns::CONNECTION => self.handle_connection(handle, &message, &payload),
            ns::RECEIVER => self.handle_receiver(handle, &message, payload).await,
            ns::DISCOVERY => self.handle_discovery(handle, &message, &payload).await,
            ns::MULTIZONE => self.handle_multizone(handle, &message, &payload).await,
            ns::SETUP => self.handle_setup(handle, &message, &payload).await,
            other => tracing::warn!(namespace = %other, "unhandled platform namespace"),
        }
    }

    fn handle_connection(
        &mut self,
        handle: &ConnectionHandle,
        message: &CastMessage,
        payload: &serde_json::Value,
    ) {
        let key = (handle.id(), message.source_id.clone());
        match serde_json::from_value::<ConnectionMessage>(payload.clone()) {
            Ok(ConnectionMessage::Connect(_)) => {
                self.receiver_subscribers.insert(key);
            }
            // CLOSE or an unrecognized connection message drops the subscription.
            _ => {
                self.receiver_subscribers.remove(&key);
            }
        }
    }

    async fn handle_receiver(
        &mut self,
        handle: &ConnectionHandle,
        message: &CastMessage,
        payload: serde_json::Value,
    ) {
        let request = match serde_json::from_value::<ReceiverRequest>(payload.clone()) {
            Ok(request) => request,
            Err(_) => {
                let response = InvalidRequestResponse::new(
                    extract_request_id(&payload),
                    "Invalid receiver request",
                );
                self.reply(handle, &message.source_id, ns::RECEIVER, &response)
                    .await;
                return;
            }
        };

        match request {
            ReceiverRequest::GetStatus(r) => {
                let response = build_receiver_status(r.request_id, &self.volume);
                self.reply(handle, &message.source_id, ns::RECEIVER, &response)
                    .await;
            }
            ReceiverRequest::GetAppAvailability(r) => {
                let response = AppAvailabilityResponse::available(r.request_id, &r.app_id);
                self.reply(handle, &message.source_id, ns::RECEIVER, &response)
                    .await;
            }
            ReceiverRequest::Launch(r) => {
                // Sessions/apps are wired in Phase 6.
                let response = LaunchErrorResponse::new(r.request_id, "Application not available");
                self.reply(handle, &message.source_id, ns::RECEIVER, &response)
                    .await;
            }
            ReceiverRequest::Stop(r) => {
                let response = build_receiver_status(r.request_id, &self.volume);
                self.broadcast(ns::RECEIVER, &response).await;
            }
            ReceiverRequest::SetVolume(r) => {
                self.volume.apply_update(&r.volume);
                let response = build_receiver_status(r.request_id, &self.volume);
                self.broadcast(ns::RECEIVER, &response).await;
            }
        }
    }

    async fn handle_discovery(
        &self,
        handle: &ConnectionHandle,
        message: &CastMessage,
        payload: &serde_json::Value,
    ) {
        if let Ok(request) = serde_json::from_value::<GetDeviceInfoRequest>(payload.clone()) {
            let response = DeviceInfoResponse::new(
                request.request_id,
                self.identity.device_id.clone(),
                self.identity.device_model.clone(),
                self.identity.friendly_name.clone(),
            );
            self.reply(handle, &message.source_id, ns::DISCOVERY, &response)
                .await;
        }
    }

    async fn handle_multizone(
        &self,
        handle: &ConnectionHandle,
        message: &CastMessage,
        payload: &serde_json::Value,
    ) {
        if let Ok(request) = serde_json::from_value::<MultizoneGetStatusRequest>(payload.clone()) {
            let response = MultizoneStatusResponse::empty(request.request_id);
            self.reply(handle, &message.source_id, ns::MULTIZONE, &response)
                .await;
        }
    }

    async fn handle_setup(
        &self,
        handle: &ConnectionHandle,
        message: &CastMessage,
        payload: &serde_json::Value,
    ) {
        if let Ok(request) = serde_json::from_value::<SetupRequest>(payload.clone()) {
            let response = SetupResponse::ok(
                request.request_id,
                self.identity.friendly_name.clone(),
                self.identity.ssdp_udn.clone(),
            );
            self.reply(handle, &message.source_id, ns::SETUP, &response)
                .await;
        }
    }

    /// Send a response to one sender on one connection.
    async fn reply<T: Serialize>(
        &self,
        handle: &ConnectionHandle,
        dest_id: &str,
        namespace: &str,
        message: &T,
    ) {
        match serde_json::to_value(message) {
            Ok(value) => {
                let _ = handle
                    .send_json(RECEIVER_0, dest_id, namespace, &value)
                    .await;
            }
            Err(error) => tracing::error!(%error, "failed to serialize platform response"),
        }
    }

    /// Broadcast a message to every sender subscribed to `receiver-0`.
    async fn broadcast<T: Serialize>(&self, namespace: &str, message: &T) {
        let Ok(value) = serde_json::to_value(message) else {
            return;
        };
        let connection_ids: HashSet<u64> = self
            .receiver_subscribers
            .iter()
            .map(|(id, _)| *id)
            .collect();
        for id in connection_ids {
            if let Some(handle) = self.connections.get(&id) {
                let _ = handle.send_json(RECEIVER_0, "*", namespace, &value).await;
            }
        }
    }
}

fn parse_payload(message: &CastMessage) -> Option<serde_json::Value> {
    let text = message.payload_utf8.as_deref()?;
    serde_json::from_str(text).ok()
}
