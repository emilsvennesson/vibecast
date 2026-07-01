//! Receiver namespace messages (GET_STATUS, LAUNCH, STOP, SET_VOLUME, ...).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::{ReceiverStatus, VolumeUpdate};

// --- Inbound (sender -> receiver) -----------------------------------------

/// GET_STATUS request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStatusRequest {
    /// Request id echoed in the response.
    pub request_id: i64,
}

/// Credentials nested under `appParams.launchCheckerParams.credentialsData`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsData {
    /// Credentials blob.
    pub credentials: Option<String>,
    /// Credentials type.
    pub credentials_type: Option<String>,
}

/// Nested launch checker params.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchCheckerParams {
    /// Nested credentials.
    pub credentials_data: Option<CredentialsData>,
}

/// Optional structured app launch parameters.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppParams {
    /// Launch checker params carrying credentials.
    pub launch_checker_params: Option<LaunchCheckerParams>,
}

/// LAUNCH request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequest {
    /// Request id.
    pub request_id: i64,
    /// App id to launch.
    pub app_id: String,
    /// Top-level credentials.
    #[serde(default)]
    pub credentials: Option<String>,
    /// Top-level credentials type.
    #[serde(default)]
    pub credentials_type: Option<String>,
    /// Requested language.
    #[serde(default)]
    pub language: Option<String>,
    /// Structured app params (may carry nested credentials).
    #[serde(default)]
    pub app_params: Option<AppParams>,
    /// App-specific custom data.
    #[serde(default)]
    pub custom_data: Option<Value>,
}

impl LaunchRequest {
    /// Resolve credentials from the top level, falling back to the nested
    /// `appParams.launchCheckerParams.credentialsData`.
    #[must_use]
    pub fn resolved_credentials(&self) -> (Option<String>, Option<String>) {
        let nested = self
            .app_params
            .as_ref()
            .and_then(|p| p.launch_checker_params.as_ref())
            .and_then(|p| p.credentials_data.as_ref());
        let credentials = self
            .credentials
            .clone()
            .or_else(|| nested.and_then(|n| n.credentials.clone()));
        let credentials_type = self
            .credentials_type
            .clone()
            .or_else(|| nested.and_then(|n| n.credentials_type.clone()));
        (credentials, credentials_type)
    }
}

/// STOP request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopRequest {
    /// Request id.
    pub request_id: i64,
    /// Session id to stop.
    pub session_id: String,
}

/// GET_APP_AVAILABILITY request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAppAvailabilityRequest {
    /// Request id.
    pub request_id: i64,
    /// App ids being queried.
    pub app_id: Vec<String>,
}

/// SET_VOLUME request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetVolumeRequest {
    /// Request id.
    pub request_id: i64,
    /// Partial volume update.
    pub volume: VolumeUpdate,
}

/// Discriminated union of inbound receiver requests.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ReceiverRequest {
    /// GET_STATUS.
    #[serde(rename = "GET_STATUS")]
    GetStatus(GetStatusRequest),
    /// LAUNCH.
    #[serde(rename = "LAUNCH")]
    Launch(LaunchRequest),
    /// STOP.
    #[serde(rename = "STOP")]
    Stop(StopRequest),
    /// GET_APP_AVAILABILITY.
    #[serde(rename = "GET_APP_AVAILABILITY")]
    GetAppAvailability(GetAppAvailabilityRequest),
    /// SET_VOLUME.
    #[serde(rename = "SET_VOLUME")]
    SetVolume(SetVolumeRequest),
}

// --- Outbound (receiver -> sender) ----------------------------------------

/// RECEIVER_STATUS response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverStatusResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id echoed back (0 for unsolicited broadcasts).
    pub request_id: i64,
    /// The receiver status.
    pub status: ReceiverStatus,
}

impl ReceiverStatusResponse {
    /// Build a RECEIVER_STATUS response.
    #[must_use]
    pub fn new(request_id: i64, status: ReceiverStatus) -> Self {
        Self {
            kind: "RECEIVER_STATUS",
            request_id,
            status,
        }
    }
}

/// GET_APP_AVAILABILITY response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppAvailabilityResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Map of app id -> availability string.
    pub availability: BTreeMap<String, String>,
}

impl AppAvailabilityResponse {
    /// Build a response marking all `app_ids` as `APP_AVAILABLE`.
    #[must_use]
    pub fn available(request_id: i64, app_ids: &[String]) -> Self {
        let availability = app_ids
            .iter()
            .map(|id| (id.clone(), "APP_AVAILABLE".to_string()))
            .collect();
        Self {
            kind: "GET_APP_AVAILABILITY",
            request_id,
            availability,
        }
    }
}

/// LAUNCH_ERROR response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchErrorResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Failure reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl LaunchErrorResponse {
    /// Build a LAUNCH_ERROR response.
    #[must_use]
    pub fn new(request_id: i64, reason: impl Into<String>) -> Self {
        Self {
            kind: "LAUNCH_ERROR",
            request_id,
            reason: Some(reason.into()),
        }
    }
}

/// INVALID_REQUEST response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidRequestResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Failure reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl InvalidRequestResponse {
    /// Build an INVALID_REQUEST response.
    #[must_use]
    pub fn new(request_id: i64, reason: impl Into<String>) -> Self {
        Self {
            kind: "INVALID_REQUEST",
            request_id,
            reason: Some(reason.into()),
        }
    }
}
