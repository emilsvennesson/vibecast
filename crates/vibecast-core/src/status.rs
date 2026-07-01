//! RECEIVER_STATUS construction.

use vibecast_messages::{ReceiverStatus, ReceiverStatusResponse, Volume};

/// Build a RECEIVER_STATUS response from current device state.
///
/// Phase 4 has no running app sessions, so `applications` is empty; Phase 6 will
/// populate it from the session registry.
#[must_use]
pub fn build_receiver_status(request_id: i64, volume: &Volume) -> ReceiverStatusResponse {
    ReceiverStatusResponse::new(
        request_id,
        ReceiverStatus {
            applications: Vec::new(),
            volume: volume.clone(),
            is_active_input: Some(true),
            is_stand_by: Some(false),
        },
    )
}
