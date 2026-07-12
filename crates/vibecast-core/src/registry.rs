//! App registry and the proxy-registration seam.

use std::collections::HashMap;
use std::sync::Arc;

use vibecast_sdk::{AppManifest, AppProvider};

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
    /// Two providers claim the same stable app key.
    #[error("duplicate app key {app_key:?} registered by {existing:?} and {duplicate:?}")]
    DuplicateAppKey {
        /// The conflicting stable app key.
        app_key: String,
        /// Display name of the already-registered provider.
        existing: String,
        /// Display name of the provider that tried to re-register it.
        duplicate: String,
    },
}

/// A provider paired with the manifest captured when it was registered.
pub struct RegisteredApp {
    /// The app session factory.
    pub provider: Arc<dyn AppProvider>,
    /// The provider's identity, protocol declarations, and settings schema.
    pub manifest: AppManifest,
}

/// Maps Cast application ids to explicitly registered apps.
///
/// Cheaply cloneable, so one validated registry can be shared across multiple
/// receiver instances. Each provider's manifest is captured exactly once.
#[derive(Clone)]
pub struct AppRegistry {
    by_id: HashMap<String, Arc<RegisteredApp>>,
    all: Vec<Arc<RegisteredApp>>,
}

impl AppRegistry {
    /// Build a registry from an explicit list of providers.
    ///
    /// Returns an error if two providers claim the same Cast application id or
    /// stable app key, rather than silently overwriting one.
    pub fn new(providers: Vec<Arc<dyn AppProvider>>) -> Result<Self, RegistryError> {
        let mut by_id: HashMap<String, Arc<RegisteredApp>> = HashMap::new();
        let mut by_key: HashMap<&'static str, Arc<RegisteredApp>> = HashMap::new();
        let mut all = Vec::with_capacity(providers.len());
        for provider in providers {
            let app = Arc::new(RegisteredApp {
                manifest: provider.manifest(),
                provider,
            });
            if let Some(existing) = by_key.get(app.manifest.app_key) {
                return Err(RegistryError::DuplicateAppKey {
                    app_key: app.manifest.app_key.to_string(),
                    existing: existing.manifest.display_name.to_string(),
                    duplicate: app.manifest.display_name.to_string(),
                });
            }
            for app_id in app.manifest.app_ids {
                if let Some(existing) = by_id.get(*app_id) {
                    return Err(RegistryError::DuplicateAppId {
                        app_id: (*app_id).to_string(),
                        existing: existing.manifest.display_name.to_string(),
                        duplicate: app.manifest.display_name.to_string(),
                    });
                }
                by_id.insert((*app_id).to_string(), app.clone());
            }
            by_key.insert(app.manifest.app_key, app.clone());
            all.push(app);
        }
        Ok(Self { by_id, all })
    }

    /// Look up the registered app for a Cast application id.
    #[must_use]
    pub fn get(&self, app_id: &str) -> Option<Arc<RegisteredApp>> {
        self.by_id.get(app_id).cloned()
    }

    /// All registered apps.
    #[must_use]
    pub fn all(&self) -> &[Arc<RegisteredApp>] {
        &self.all
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use vibecast_sdk::{AppContext, AppSession, LaunchCredentials, LaunchError};

    use super::*;

    struct FakeProvider {
        app_key: &'static str,
        app_ids: &'static [&'static str],
        display_name: &'static str,
        manifest_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AppProvider for FakeProvider {
        fn manifest(&self) -> AppManifest {
            self.manifest_calls.fetch_add(1, Ordering::Relaxed);
            AppManifest::without_settings(self.app_key, self.app_ids, self.display_name)
        }

        async fn launch(
            &self,
            _ctx: &AppContext,
            _credentials: LaunchCredentials,
        ) -> Result<Arc<dyn AppSession>, LaunchError> {
            unreachable!("registry tests do not launch apps")
        }
    }

    fn provider(
        app_key: &'static str,
        app_ids: &'static [&'static str],
        display_name: &'static str,
    ) -> (Arc<dyn AppProvider>, Arc<AtomicUsize>) {
        let manifest_calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(FakeProvider {
                app_key,
                app_ids,
                display_name,
                manifest_calls: manifest_calls.clone(),
            }),
            manifest_calls,
        )
    }

    #[test]
    fn captures_manifest_once_and_exposes_it_with_provider() {
        let (provider, manifest_calls) = provider("one", &["APP1"], "One");
        let registry = AppRegistry::new(vec![provider.clone()]).expect("registry");

        let app = registry.get("APP1").expect("registered app");
        assert!(Arc::ptr_eq(&app.provider, &provider));
        assert_eq!(app.manifest.app_key, "one");
        assert_eq!(registry.all().len(), 1);
        assert_eq!(manifest_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejects_duplicate_cast_app_ids() {
        let (one, _) = provider("one", &["SHARED"], "One");
        let (two, _) = provider("two", &["SHARED"], "Two");

        assert!(matches!(
            AppRegistry::new(vec![one, two]),
            Err(RegistryError::DuplicateAppId { app_id, .. }) if app_id == "SHARED"
        ));
    }

    #[test]
    fn rejects_duplicate_app_keys() {
        let (one, _) = provider("shared", &["APP1"], "One");
        let (two, _) = provider("shared", &["APP2"], "Two");

        assert!(matches!(
            AppRegistry::new(vec![one, two]),
            Err(RegistryError::DuplicateAppKey { app_key, .. }) if app_key == "shared"
        ));
    }
}
