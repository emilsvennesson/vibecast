//! App registry and the proxy-registration seam.

use std::collections::HashMap;
use std::sync::Arc;

use vibecast_bridge::{LicenseHandler, ManifestHandler, PlayerBridge};
use vibecast_sdk::AppProvider;

/// Maps Cast application ids to their providers (explicit registration).
pub struct AppRegistry {
    by_id: HashMap<String, Arc<dyn AppProvider>>,
    all: Vec<Arc<dyn AppProvider>>,
}

impl AppRegistry {
    /// Build a registry from an explicit list of providers.
    #[must_use]
    pub fn new(providers: Vec<Arc<dyn AppProvider>>) -> Self {
        let mut by_id: HashMap<String, Arc<dyn AppProvider>> = HashMap::new();
        for provider in &providers {
            for app_id in provider.app_ids() {
                by_id.insert((*app_id).to_string(), provider.clone());
            }
        }
        Self {
            by_id,
            all: providers,
        }
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

    /// All handled app ids (for mDNS/discovery advertisement).
    #[must_use]
    pub fn app_ids(&self) -> Vec<String> {
        self.by_id.keys().cloned().collect()
    }
}

/// Abstraction over the bridge's session-scoped proxy registration, so the hub
/// can be tested with a fake bridge.
pub trait ProxyRegistrar: Send + Sync {
    /// Register a session license handler; returns its proxy URL.
    fn register_license(&self, session_id: &str, handler: Arc<dyn LicenseHandler>) -> String;
    /// Unregister a session license handler.
    fn unregister_license(&self, session_id: &str);
    /// Register a session manifest handler; returns its proxy URL prefix.
    fn register_manifest(&self, session_id: &str, handler: Arc<dyn ManifestHandler>) -> String;
    /// Unregister a session manifest handler.
    fn unregister_manifest(&self, session_id: &str);
}

impl ProxyRegistrar for PlayerBridge {
    fn register_license(&self, session_id: &str, handler: Arc<dyn LicenseHandler>) -> String {
        self.register_license_handler(session_id.to_string(), handler)
    }

    fn unregister_license(&self, session_id: &str) {
        self.unregister_license_handler(session_id);
    }

    fn register_manifest(&self, session_id: &str, handler: Arc<dyn ManifestHandler>) -> String {
        self.register_manifest_handler(session_id.to_string(), handler)
    }

    fn unregister_manifest(&self, session_id: &str) {
        self.unregister_manifest_handler(session_id);
    }
}
