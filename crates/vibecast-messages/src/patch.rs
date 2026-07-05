//! Presence-tracking field wrapper for partial (patch) messages.

use serde::{Deserialize, Deserializer};

/// A field that may be omitted from a partial update message.
///
/// Unlike `Option<T>`, this distinguishes an *omitted* field ([`Patch::Missing`])
/// from one that was explicitly provided ([`Patch::Set`]). Pair each field with
/// `#[serde(default)]`: an absent key deserializes to `Missing`, while a present
/// key deserializes `T` directly — so an explicit JSON `null` is *rejected* for a
/// non-nullable `T` rather than being silently treated as "omitted". This lets a
/// partial update distinguish "leave unchanged" from "set to this value".
///
/// For genuinely nullable fields, use `Patch<Option<T>>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Patch<T> {
    /// The field was not present in the message.
    #[default]
    Missing,
    /// The field was present with this value.
    Set(T),
}

impl<T> Patch<T> {
    /// The value if the field was provided, else `None`.
    #[must_use]
    pub fn set(&self) -> Option<&T> {
        match self {
            Patch::Set(value) => Some(value),
            Patch::Missing => None,
        }
    }

    /// Whether the field was explicitly provided.
    #[must_use]
    pub const fn is_set(&self) -> bool {
        matches!(self, Patch::Set(_))
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Reached only when the key is present (fields use `#[serde(default)]`),
        // so any present value — including `null` — is deserialized as `T`.
        T::deserialize(deserializer).map(Patch::Set)
    }
}
