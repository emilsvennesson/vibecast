//! The player command sink.

use async_trait::async_trait;

use crate::protocol::PlayerCommand;

/// A player that plays media in response to bridge commands.
///
/// The browser bridge is the default implementation; a future native player
/// can implement this trait without touching the coordinator.
#[async_trait]
pub trait Player: Send + Sync {
    /// Deliver a command to the player(s).
    async fn send(&self, command: PlayerCommand);
}
