//! App registry and the proxy-registration seam.

use std::collections::HashMap;
use std::sync::Arc;

use vibecast_sdk::AppProvider;

/// A configuration error building an [`AppRegistry`].
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// Two providers claim the same Cast application id.
    #[error("duplicate app id {app_id:?} registered by {existing:?} and {duplicate:?}")]
    DuplicateAppId {
        /// The conflicting Cast application id.
        app_id: String,
        /// Display name of the already-registered provider.
        existing: String,
        /// Display name of the provider that tried to re-register it.
        duplicate: String,
    },
}

/// Maps Cast application ids to their providers (explicit registration).
///
/// Cheaply cloneable (all providers are `Arc`), so one validated registry can be
/// shared across multiple receiver instances.
#[derive(Clone)]
pub struct AppRegistry {
    by_id: HashMap<String, Arc<dyn AppProvider>>,
    all: Vec<Arc<dyn AppProvider>>,
}

impl AppRegistry {
    /// Build a registry from an explicit list of providers.
    ///
    /// Returns [`RegistryError::DuplicateAppId`] if two providers claim the
    /// same Cast application id, rather than silently overwriting one — a
    /// misconfiguration should fail loudly at startup.
    pub fn new(providers: Vec<Arc<dyn AppProvider>>) -> Result<Self, RegistryError> {
        let mut by_id: HashMap<String, Arc<dyn AppProvider>> = HashMap::new();
        for provider in &providers {
            for app_id in provider.app_ids() {
                if let Some(existing) = by_id.get(*app_id) {
                    return Err(RegistryError::DuplicateAppId {
                        app_id: (*app_id).to_string(),
                        existing: existing.display_name().to_string(),
                        duplicate: provider.display_name().to_string(),
                    });
                }
                by_id.insert((*app_id).to_string(), provider.clone());
            }
        }
        Ok(Self {
            by_id,
            all: providers,
        })
    }

    /// Look up the provider for an app id.
    #[must_use]
    pub fn get(&self, app_id: &str) -> Option<Arc<dyn AppProvider>> {
        self.by_id.get(app_id).cloned()
    }

    /// All registered providers.
    #[must_use]
    pub fn all(&self) -> &[Arc<dyn AppProvider>] {
        &self.all
    }
}
