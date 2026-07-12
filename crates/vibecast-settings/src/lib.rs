//! Typed application settings with validated schemas and live effective snapshots.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{watch, Mutex as AsyncMutex};

/// Whether a setting is shared by an installation or overridden per player.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingScope {
    App,
    AppPlayer,
}

/// A setting value represented as a clean JSON primitive.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Integer(i64),
    Number(f64),
    String(String),
}

impl SettingValue {
    pub fn kind(&self) -> SettingValueKind {
        match self {
            Self::Bool(_) => SettingValueKind::Boolean,
            Self::Integer(_) => SettingValueKind::Integer,
            Self::Number(_) => SettingValueKind::Number,
            Self::String(_) => SettingValueKind::String,
        }
    }
}

/// The runtime kind of a setting value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingValueKind {
    Boolean,
    Integer,
    Number,
    String,
}

impl fmt::Display for SettingValueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean => formatter.write_str("boolean"),
            Self::Integer => formatter.write_str("integer"),
            Self::Number => formatter.write_str("number"),
            Self::String => formatter.write_str("string"),
        }
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A Rust type supported by [`SettingKey`].
pub trait SettingType: sealed::Sealed + Clone + Send + Sync + 'static {
    const KIND: SettingValueKind;

    fn into_value(self) -> SettingValue;
    fn from_value(value: &SettingValue) -> Option<Self>;
}

macro_rules! impl_setting_type {
    ($type:ty, $variant:ident, $kind:ident) => {
        impl sealed::Sealed for $type {}

        impl SettingType for $type {
            const KIND: SettingValueKind = SettingValueKind::$kind;

            fn into_value(self) -> SettingValue {
                SettingValue::$variant(self)
            }

            fn from_value(value: &SettingValue) -> Option<Self> {
                match value {
                    SettingValue::$variant(value) => Some(value.clone()),
                    _ => None,
                }
            }
        }
    };
}

impl_setting_type!(bool, Bool, Boolean);
impl_setting_type!(String, String, String);
impl_setting_type!(i64, Integer, Integer);
impl_setting_type!(f64, Number, Number);

/// A statically typed setting key.
#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SettingKey<T: SettingType> {
    name: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T: SettingType> SettingKey<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }

    pub const fn as_str(self) -> &'static str {
        self.name
    }
}

impl<T: SettingType> Clone for SettingKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: SettingType> Copy for SettingKey<T> {}

/// One allowed value for a choice setting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}

impl ChoiceOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A UI-facing setting descriptor and its validation constraints.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SettingDescriptor {
    Boolean {
        key: String,
        label: String,
        description: Option<String>,
        scope: SettingScope,
        default: bool,
    },
    Choice {
        key: String,
        label: String,
        description: Option<String>,
        scope: SettingScope,
        default: String,
        choices: Vec<ChoiceOption>,
    },
    Integer {
        key: String,
        label: String,
        description: Option<String>,
        scope: SettingScope,
        default: i64,
        min: Option<i64>,
        max: Option<i64>,
    },
    Number {
        key: String,
        label: String,
        description: Option<String>,
        scope: SettingScope,
        default: f64,
        min: Option<f64>,
        max: Option<f64>,
    },
    String {
        key: String,
        label: String,
        description: Option<String>,
        scope: SettingScope,
        default: String,
        min_length: Option<usize>,
        max_length: Option<usize>,
    },
}

impl SettingDescriptor {
    pub fn key(&self) -> &str {
        match self {
            Self::Boolean { key, .. }
            | Self::Choice { key, .. }
            | Self::Integer { key, .. }
            | Self::Number { key, .. }
            | Self::String { key, .. } => key,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Boolean { label, .. }
            | Self::Choice { label, .. }
            | Self::Integer { label, .. }
            | Self::Number { label, .. }
            | Self::String { label, .. } => label,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Boolean { description, .. }
            | Self::Choice { description, .. }
            | Self::Integer { description, .. }
            | Self::Number { description, .. }
            | Self::String { description, .. } => description.as_deref(),
        }
    }

    pub fn scope(&self) -> SettingScope {
        match self {
            Self::Boolean { scope, .. }
            | Self::Choice { scope, .. }
            | Self::Integer { scope, .. }
            | Self::Number { scope, .. }
            | Self::String { scope, .. } => *scope,
        }
    }

    pub fn default_value(&self) -> SettingValue {
        match self {
            Self::Boolean { default, .. } => SettingValue::Bool(*default),
            Self::Choice { default, .. } | Self::String { default, .. } => {
                SettingValue::String(default.clone())
            }
            Self::Integer { default, .. } => SettingValue::Integer(*default),
            Self::Number { default, .. } => SettingValue::Number(*default),
        }
    }

    pub fn value_kind(&self) -> SettingValueKind {
        match self {
            Self::Boolean { .. } => SettingValueKind::Boolean,
            Self::Choice { .. } | Self::String { .. } => SettingValueKind::String,
            Self::Integer { .. } => SettingValueKind::Integer,
            Self::Number { .. } => SettingValueKind::Number,
        }
    }

    pub fn validate_value(&self, value: &SettingValue) -> Result<(), ValueValidationError> {
        if value.kind() != self.value_kind() {
            return Err(ValueValidationError::WrongType {
                expected: self.value_kind(),
                actual: value.kind(),
            });
        }

        match (self, value) {
            (Self::Boolean { .. }, SettingValue::Bool(_)) => Ok(()),
            (Self::Choice { choices, .. }, SettingValue::String(value)) => {
                if choices.iter().any(|choice| choice.value == *value) {
                    Ok(())
                } else {
                    Err(ValueValidationError::InvalidChoice(value.clone()))
                }
            }
            (Self::Integer { min, max, .. }, SettingValue::Integer(value)) => {
                validate_range(*value, *min, *max)
            }
            (Self::Number { min, max, .. }, SettingValue::Number(value)) => {
                if !value.is_finite() {
                    return Err(ValueValidationError::NotFinite);
                }
                validate_range(*value, *min, *max)
            }
            (
                Self::String {
                    min_length,
                    max_length,
                    ..
                },
                SettingValue::String(value),
            ) => {
                let length = value.chars().count();
                if min_length.is_some_and(|min| length < min) {
                    return Err(ValueValidationError::TooShort {
                        min: min_length.unwrap_or_default(),
                        actual: length,
                    });
                }
                if max_length.is_some_and(|max| length > max) {
                    return Err(ValueValidationError::TooLong {
                        max: max_length.unwrap_or_default(),
                        actual: length,
                    });
                }
                Ok(())
            }
            _ => unreachable!("value kind was checked above"),
        }
    }

    fn validate_definition(&self) -> Result<(), CatalogError> {
        if self.key().is_empty() {
            return Err(CatalogError::EmptySettingKey);
        }
        if self.label().is_empty() {
            return Err(CatalogError::EmptyLabel {
                key: self.key().to_owned(),
            });
        }

        match self {
            Self::Choice { key, choices, .. } => {
                if choices.is_empty() {
                    return Err(CatalogError::EmptyChoices { key: key.clone() });
                }
                let mut values = BTreeSet::new();
                for choice in choices {
                    if !values.insert(&choice.value) {
                        return Err(CatalogError::DuplicateChoice {
                            key: key.clone(),
                            value: choice.value.clone(),
                        });
                    }
                }
            }
            Self::Integer { key, min, max, .. } => validate_bounds(key, *min, *max)?,
            Self::Number { key, min, max, .. } => {
                if min.is_some_and(|value| !value.is_finite())
                    || max.is_some_and(|value| !value.is_finite())
                {
                    return Err(CatalogError::InvalidConstraints {
                        key: key.clone(),
                        reason: "number bounds must be finite".to_owned(),
                    });
                }
                validate_bounds(key, *min, *max)?;
            }
            Self::String {
                key,
                min_length,
                max_length,
                ..
            } => validate_bounds(key, *min_length, *max_length)?,
            Self::Boolean { .. } => {}
        }

        self.validate_value(&self.default_value())
            .map_err(|source| CatalogError::InvalidDefault {
                key: self.key().to_owned(),
                source,
            })
    }
}

fn validate_range<T: PartialOrd + fmt::Display + Copy>(
    value: T,
    min: Option<T>,
    max: Option<T>,
) -> Result<(), ValueValidationError> {
    if min.is_some_and(|min| value < min) {
        return Err(ValueValidationError::BelowMinimum {
            min: min.unwrap().to_string(),
        });
    }
    if max.is_some_and(|max| value > max) {
        return Err(ValueValidationError::AboveMaximum {
            max: max.unwrap().to_string(),
        });
    }
    Ok(())
}

fn validate_bounds<T: PartialOrd>(
    key: &str,
    min: Option<T>,
    max: Option<T>,
) -> Result<(), CatalogError> {
    if matches!((min, max), (Some(min), Some(max)) if min > max) {
        return Err(CatalogError::InvalidConstraints {
            key: key.to_owned(),
            reason: "minimum exceeds maximum".to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq)]
pub enum ValueValidationError {
    #[error("expected {expected}, got {actual}")]
    WrongType {
        expected: SettingValueKind,
        actual: SettingValueKind,
    },
    #[error("{0:?} is not an allowed choice")]
    InvalidChoice(String),
    #[error("number must be finite")]
    NotFinite,
    #[error("value is below minimum {min}")]
    BelowMinimum { min: String },
    #[error("value is above maximum {max}")]
    AboveMaximum { max: String },
    #[error("string length {actual} is below minimum {min}")]
    TooShort { min: usize, actual: usize },
    #[error("string length {actual} exceeds maximum {max}")]
    TooLong { max: usize, actual: usize },
}

/// A validated schema for one app.
#[derive(Clone, Debug, PartialEq)]
pub struct AppSettingsSchema {
    app_id: String,
    display_name: String,
    settings: Vec<SettingDescriptor>,
}

impl AppSettingsSchema {
    pub fn new(
        app_id: impl Into<String>,
        settings: Vec<SettingDescriptor>,
    ) -> Result<Self, CatalogError> {
        let app_id = app_id.into();
        let display_name = app_id.clone();
        Self::with_display_name(app_id, display_name, settings)
    }

    pub fn with_display_name(
        app_id: impl Into<String>,
        display_name: impl Into<String>,
        settings: Vec<SettingDescriptor>,
    ) -> Result<Self, CatalogError> {
        let app_id = app_id.into();
        if app_id.is_empty() {
            return Err(CatalogError::EmptyAppId);
        }
        let display_name = display_name.into();
        if display_name.is_empty() {
            return Err(CatalogError::EmptyAppDisplayName { app_id });
        }

        let mut keys = BTreeSet::new();
        for setting in &settings {
            setting.validate_definition()?;
            if !keys.insert(setting.key()) {
                return Err(CatalogError::DuplicateSettingKey {
                    app_id: app_id.clone(),
                    key: setting.key().to_owned(),
                });
            }
        }

        Ok(Self {
            app_id,
            display_name,
            settings,
        })
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn settings(&self) -> &[SettingDescriptor] {
        &self.settings
    }

    pub fn setting(&self, key: &str) -> Option<&SettingDescriptor> {
        self.settings.iter().find(|setting| setting.key() == key)
    }
}

/// A validated collection of app schemas.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsCatalog {
    schemas: BTreeMap<String, AppSettingsSchema>,
}

impl SettingsCatalog {
    pub fn new(schemas: Vec<AppSettingsSchema>) -> Result<Self, CatalogError> {
        let mut by_app = BTreeMap::new();
        for schema in schemas {
            let app_id = schema.app_id.clone();
            if by_app.insert(app_id.clone(), schema).is_some() {
                return Err(CatalogError::DuplicateAppId(app_id));
            }
        }
        Ok(Self { schemas: by_app })
    }

    pub fn app(&self, app_id: &str) -> Option<&AppSettingsSchema> {
        self.schemas.get(app_id)
    }

    pub fn apps(&self) -> impl Iterator<Item = &AppSettingsSchema> {
        self.schemas.values()
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum CatalogError {
    #[error("app id cannot be empty")]
    EmptyAppId,
    #[error("app {app_id:?} must have a display name")]
    EmptyAppDisplayName { app_id: String },
    #[error("setting key cannot be empty")]
    EmptySettingKey,
    #[error("setting {key:?} must have a label")]
    EmptyLabel { key: String },
    #[error("app {app_id:?} has duplicate setting key {key:?}")]
    DuplicateSettingKey { app_id: String, key: String },
    #[error("duplicate app id {0:?}")]
    DuplicateAppId(String),
    #[error("choice setting {key:?} has no choices")]
    EmptyChoices { key: String },
    #[error("choice setting {key:?} has duplicate value {value:?}")]
    DuplicateChoice { key: String, value: String },
    #[error("invalid constraints for {key:?}: {reason}")]
    InvalidConstraints { key: String, reason: String },
    #[error("invalid default for {key:?}: {source}")]
    InvalidDefault {
        key: String,
        source: ValueValidationError,
    },
}

/// One immutable, effective app settings view.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSnapshot {
    app_id: String,
    player_id: Option<String>,
    revision: u64,
    values: BTreeMap<String, SettingValue>,
}

impl SettingsSnapshot {
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn player_id(&self) -> Option<&str> {
        self.player_id.as_deref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn values(&self) -> &BTreeMap<String, SettingValue> {
        &self.values
    }

    pub fn get<T: SettingType>(&self, key: SettingKey<T>) -> Result<Option<T>, SnapshotTypeError> {
        let Some(value) = self.values.get(key.as_str()) else {
            return Ok(None);
        };
        T::from_value(value)
            .map(Some)
            .ok_or_else(|| SnapshotTypeError {
                key: key.as_str().to_owned(),
                expected: T::KIND,
                actual: value.kind(),
            })
    }
}

#[derive(Debug, Error, PartialEq)]
#[error("setting {key:?} expected {expected}, got {actual}")]
pub struct SnapshotTypeError {
    pub key: String,
    pub expected: SettingValueKind,
    pub actual: SettingValueKind,
}

/// A live reader for an app's effective snapshots.
#[derive(Clone)]
pub struct AppSettingsReader {
    receiver: watch::Receiver<Arc<SettingsSnapshot>>,
}

impl AppSettingsReader {
    pub fn snapshot(&self) -> Arc<SettingsSnapshot> {
        self.receiver.borrow().clone()
    }

    pub async fn changed(&mut self) -> Result<Arc<SettingsSnapshot>, watch::error::RecvError> {
        self.receiver.changed().await?;
        Ok(self.snapshot())
    }
}

/// A typed set or reset operation. Reset removes an override, revealing its fallback.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingMutation {
    Set { key: String, value: SettingValue },
    Reset { key: String },
}

impl SettingMutation {
    pub fn set<T: SettingType>(key: SettingKey<T>, value: T) -> Self {
        Self::Set {
            key: key.as_str().to_owned(),
            value: value.into_value(),
        }
    }

    pub fn reset<T: SettingType>(key: SettingKey<T>) -> Self {
        Self::Reset {
            key: key.as_str().to_owned(),
        }
    }

    fn key(&self) -> &str {
        match self {
            Self::Set { key, .. } | Self::Reset { key } => key,
        }
    }
}

/// Serializable state owned by a persistence backend.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSettings {
    pub revision: u64,
    pub apps: BTreeMap<String, PersistedAppSettings>,
    pub players: BTreeMap<String, BTreeMap<String, PersistedAppSettings>>,
}

/// Persisted overrides and their last effective revision.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedAppSettings {
    pub revision: u64,
    pub values: BTreeMap<String, SettingValue>,
}

pub type PersistenceError = Box<dyn Error + Send + Sync + 'static>;

/// Storage abstraction. Implementations should replace all persisted state atomically.
#[async_trait]
pub trait SettingsPersistence: Send + Sync {
    async fn load(&self) -> Result<PersistedSettings, PersistenceError>;
    async fn save(&self, settings: &PersistedSettings) -> Result<(), PersistenceError>;
}

/// Volatile persistence useful for embedding and tests.
#[derive(Default)]
pub struct MemorySettingsPersistence {
    state: Mutex<PersistedSettings>,
}

impl MemorySettingsPersistence {
    pub fn new(state: PersistedSettings) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    pub fn state(&self) -> PersistedSettings {
        self.state.lock().unwrap().clone()
    }
}

#[async_trait]
impl SettingsPersistence for MemorySettingsPersistence {
    async fn load(&self) -> Result<PersistedSettings, PersistenceError> {
        Ok(self.state())
    }

    async fn save(&self, settings: &PersistedSettings) -> Result<(), PersistenceError> {
        *self.state.lock().unwrap() = settings.clone();
        Ok(())
    }
}

/// Shared settings service. Clone it or derive host/player handles from it.
#[derive(Clone)]
pub struct SettingsService {
    inner: Arc<ServiceInner>,
}

impl SettingsService {
    /// Build a reader with an empty snapshot for contexts that do not have a
    /// settings service, such as app unit tests.
    pub fn empty_reader(app_id: impl Into<String>) -> AppSettingsReader {
        let snapshot = Arc::new(SettingsSnapshot {
            app_id: app_id.into(),
            player_id: None,
            revision: 0,
            values: BTreeMap::new(),
        });
        let (_sender, receiver) = watch::channel(snapshot);
        AppSettingsReader { receiver }
    }

    pub async fn new(
        catalog: SettingsCatalog,
        persistence: Arc<dyn SettingsPersistence>,
    ) -> Result<Self, SettingsServiceError> {
        let persisted = persistence
            .load()
            .await
            .map_err(SettingsServiceError::Persistence)?;
        validate_persisted(&catalog, &persisted)?;
        Ok(Self {
            inner: Arc::new(ServiceInner {
                catalog: Arc::new(catalog),
                persistence,
                state: AsyncMutex::new(ServiceState {
                    persisted,
                    watchers: BTreeMap::new(),
                }),
            }),
        })
    }

    pub fn catalog(&self) -> &SettingsCatalog {
        &self.inner.catalog
    }

    pub fn host(&self) -> HostSettings {
        HostSettings {
            inner: self.inner.clone(),
        }
    }

    pub fn player(
        &self,
        player_id: impl Into<String>,
    ) -> Result<PlayerSettings, SettingsServiceError> {
        let player_id = player_id.into();
        if player_id.is_empty() {
            return Err(SettingsServiceError::EmptyPlayerId);
        }
        Ok(PlayerSettings {
            inner: self.inner.clone(),
            player_id,
        })
    }
}

/// Handle for installation-scoped app settings.
#[derive(Clone)]
pub struct HostSettings {
    inner: Arc<ServiceInner>,
}

impl HostSettings {
    pub async fn reader(&self, app_id: &str) -> Result<AppSettingsReader, SettingsServiceError> {
        self.inner.reader(app_id, None).await
    }

    pub async fn update(
        &self,
        app_id: &str,
        mutations: Vec<SettingMutation>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.inner
            .mutate(app_id, None, None, mutations, false)
            .await
    }

    pub async fn set<T: SettingType>(
        &self,
        app_id: &str,
        key: SettingKey<T>,
        value: T,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.update(app_id, vec![SettingMutation::set(key, value)])
            .await
    }

    pub async fn reset<T: SettingType>(
        &self,
        app_id: &str,
        key: SettingKey<T>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.update(app_id, vec![SettingMutation::reset(key)]).await
    }

    pub async fn reset_app(
        &self,
        app_id: &str,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.inner
            .mutate(app_id, None, None, Vec::new(), true)
            .await
    }
}

/// Handle for one player's app settings.
#[derive(Clone)]
pub struct PlayerSettings {
    inner: Arc<ServiceInner>,
    player_id: String,
}

impl PlayerSettings {
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    pub async fn reader(&self, app_id: &str) -> Result<AppSettingsReader, SettingsServiceError> {
        self.inner.reader(app_id, Some(&self.player_id)).await
    }

    pub async fn compare_and_set(
        &self,
        app_id: &str,
        expected_revision: u64,
        mutations: Vec<SettingMutation>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.inner
            .mutate(
                app_id,
                Some(&self.player_id),
                Some(expected_revision),
                mutations,
                false,
            )
            .await
    }

    pub async fn compare_and_set_value<T: SettingType>(
        &self,
        app_id: &str,
        expected_revision: u64,
        key: SettingKey<T>,
        value: T,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.compare_and_set(
            app_id,
            expected_revision,
            vec![SettingMutation::set(key, value)],
        )
        .await
    }

    pub async fn reset<T: SettingType>(
        &self,
        app_id: &str,
        expected_revision: u64,
        key: SettingKey<T>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.compare_and_set(app_id, expected_revision, vec![SettingMutation::reset(key)])
            .await
    }

    pub async fn reset_app(
        &self,
        app_id: &str,
        expected_revision: u64,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        self.inner
            .mutate(
                app_id,
                Some(&self.player_id),
                Some(expected_revision),
                Vec::new(),
                true,
            )
            .await
    }
}

struct ServiceInner {
    catalog: Arc<SettingsCatalog>,
    persistence: Arc<dyn SettingsPersistence>,
    state: AsyncMutex<ServiceState>,
}

struct ServiceState {
    persisted: PersistedSettings,
    watchers: BTreeMap<WatchKey, watch::Sender<Arc<SettingsSnapshot>>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WatchKey {
    app_id: String,
    player_id: Option<String>,
}

impl ServiceInner {
    async fn reader(
        &self,
        app_id: &str,
        player_id: Option<&str>,
    ) -> Result<AppSettingsReader, SettingsServiceError> {
        let schema = self.schema(app_id)?;
        let mut state = self.state.lock().await;
        let key = WatchKey {
            app_id: app_id.to_owned(),
            player_id: player_id.map(str::to_owned),
        };
        let receiver = if let Some(sender) = state.watchers.get(&key) {
            sender.subscribe()
        } else {
            let snapshot = Arc::new(build_snapshot(schema, &state.persisted, player_id));
            let (sender, receiver) = watch::channel(snapshot);
            state.watchers.insert(key, sender);
            receiver
        };
        Ok(AppSettingsReader { receiver })
    }

    async fn mutate(
        &self,
        app_id: &str,
        player_id: Option<&str>,
        expected_revision: Option<u64>,
        mutations: Vec<SettingMutation>,
        reset_app: bool,
    ) -> Result<Arc<SettingsSnapshot>, SettingsServiceError> {
        let schema = self.schema(app_id)?;
        validate_mutations(schema, player_id.is_some(), &mutations)?;
        let mut state = self.state.lock().await;
        let current = build_snapshot(schema, &state.persisted, player_id);
        if let Some(expected) = expected_revision {
            if current.revision != expected {
                return Err(SettingsServiceError::Conflict {
                    expected,
                    actual: current.revision,
                });
            }
        }

        let stored = stored_settings(&state.persisted, app_id, player_id);
        let mut values = stored
            .map(|stored| stored.values.clone())
            .unwrap_or_default();
        if reset_app {
            values.clear();
        } else {
            for mutation in mutations {
                match mutation {
                    SettingMutation::Set { key, value } => {
                        values.insert(key, value);
                    }
                    SettingMutation::Reset { key } => {
                        values.remove(&key);
                    }
                }
            }
        }

        if stored.is_some_and(|stored| stored.values == values)
            || (stored.is_none() && values.is_empty())
        {
            return Ok(Arc::new(current));
        }

        let mut proposed = state.persisted.clone();
        proposed.revision = proposed
            .revision
            .checked_add(1)
            .ok_or(SettingsServiceError::RevisionOverflow)?;
        let replacement = PersistedAppSettings {
            revision: proposed.revision,
            values,
        };
        if let Some(player_id) = player_id {
            proposed
                .players
                .entry(player_id.to_owned())
                .or_default()
                .insert(app_id.to_owned(), replacement);
        } else {
            proposed.apps.insert(app_id.to_owned(), replacement);
        }

        self.persistence
            .save(&proposed)
            .await
            .map_err(SettingsServiceError::Persistence)?;
        state.persisted = proposed;
        publish_app(&self.catalog, &mut state, app_id, player_id);
        Ok(Arc::new(build_snapshot(
            schema,
            &state.persisted,
            player_id,
        )))
    }

    fn schema(&self, app_id: &str) -> Result<&AppSettingsSchema, SettingsServiceError> {
        self.catalog
            .app(app_id)
            .ok_or_else(|| SettingsServiceError::UnknownApp(app_id.to_owned()))
    }
}

fn validate_mutations(
    schema: &AppSettingsSchema,
    player_scoped: bool,
    mutations: &[SettingMutation],
) -> Result<(), SettingsServiceError> {
    let required_scope = if player_scoped {
        SettingScope::AppPlayer
    } else {
        SettingScope::App
    };
    let mut keys = BTreeSet::new();
    for mutation in mutations {
        if !keys.insert(mutation.key()) {
            return Err(SettingsServiceError::DuplicateMutation(
                mutation.key().to_owned(),
            ));
        }
        let descriptor =
            schema
                .setting(mutation.key())
                .ok_or_else(|| SettingsServiceError::UnknownSetting {
                    app_id: schema.app_id.clone(),
                    key: mutation.key().to_owned(),
                })?;
        if descriptor.scope() != required_scope {
            return Err(SettingsServiceError::WrongScope {
                key: mutation.key().to_owned(),
                actual: descriptor.scope(),
                target: required_scope,
            });
        }
        if let SettingMutation::Set { value, .. } = mutation {
            descriptor.validate_value(value).map_err(|source| {
                SettingsServiceError::InvalidValue {
                    key: mutation.key().to_owned(),
                    source,
                }
            })?;
        }
    }
    Ok(())
}

fn build_snapshot(
    schema: &AppSettingsSchema,
    persisted: &PersistedSettings,
    player_id: Option<&str>,
) -> SettingsSnapshot {
    let mut values = schema
        .settings
        .iter()
        .map(|setting| (setting.key().to_owned(), setting.default_value()))
        .collect::<BTreeMap<_, _>>();
    let host = persisted.apps.get(schema.app_id());
    if let Some(host) = host {
        values.extend(host.values.clone());
    }
    let player = player_id.and_then(|player_id| {
        persisted
            .players
            .get(player_id)
            .and_then(|apps| apps.get(schema.app_id()))
    });
    if let Some(player) = player {
        values.extend(player.values.clone());
    }
    SettingsSnapshot {
        app_id: schema.app_id.clone(),
        player_id: player_id.map(str::to_owned),
        revision: host
            .map(|settings| settings.revision)
            .unwrap_or_default()
            .max(player.map(|settings| settings.revision).unwrap_or_default()),
        values,
    }
}

fn stored_settings<'a>(
    persisted: &'a PersistedSettings,
    app_id: &str,
    player_id: Option<&str>,
) -> Option<&'a PersistedAppSettings> {
    match player_id {
        Some(player_id) => persisted
            .players
            .get(player_id)
            .and_then(|apps| apps.get(app_id)),
        None => persisted.apps.get(app_id),
    }
}

fn publish_app(
    catalog: &SettingsCatalog,
    state: &mut ServiceState,
    app_id: &str,
    player_id: Option<&str>,
) {
    let Some(schema) = catalog.app(app_id) else {
        return;
    };
    let keys = state
        .watchers
        .keys()
        .filter(|key| {
            key.app_id == app_id
                && player_id.is_none_or(|player_id| key.player_id.as_deref() == Some(player_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        let snapshot = Arc::new(build_snapshot(
            schema,
            &state.persisted,
            key.player_id.as_deref(),
        ));
        if let Some(sender) = state.watchers.get_mut(&key) {
            sender.send_replace(snapshot);
        }
    }
}

fn validate_persisted(
    catalog: &SettingsCatalog,
    persisted: &PersistedSettings,
) -> Result<(), SettingsServiceError> {
    for (app_id, settings) in &persisted.apps {
        validate_stored(
            catalog,
            app_id,
            settings,
            SettingScope::App,
            persisted.revision,
        )?;
    }
    for (player_id, apps) in &persisted.players {
        if player_id.is_empty() {
            return Err(SettingsServiceError::InvalidPersisted(
                "player id cannot be empty".to_owned(),
            ));
        }
        for (app_id, settings) in apps {
            validate_stored(
                catalog,
                app_id,
                settings,
                SettingScope::AppPlayer,
                persisted.revision,
            )?;
        }
    }
    Ok(())
}

fn validate_stored(
    catalog: &SettingsCatalog,
    app_id: &str,
    settings: &PersistedAppSettings,
    scope: SettingScope,
    latest_revision: u64,
) -> Result<(), SettingsServiceError> {
    if settings.revision > latest_revision {
        return Err(SettingsServiceError::InvalidPersisted(format!(
            "app {app_id:?} revision exceeds the persisted revision"
        )));
    }
    let schema = catalog
        .app(app_id)
        .ok_or_else(|| SettingsServiceError::InvalidPersisted(format!("unknown app {app_id:?}")))?;
    for (key, value) in &settings.values {
        let descriptor = schema.setting(key).ok_or_else(|| {
            SettingsServiceError::InvalidPersisted(format!(
                "unknown setting {key:?} for app {app_id:?}"
            ))
        })?;
        if descriptor.scope() != scope {
            return Err(SettingsServiceError::InvalidPersisted(format!(
                "setting {key:?} is stored in the wrong scope"
            )));
        }
        descriptor.validate_value(value).map_err(|error| {
            SettingsServiceError::InvalidPersisted(format!(
                "invalid setting {key:?} for app {app_id:?}: {error}"
            ))
        })?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SettingsServiceError {
    #[error("settings persistence failed: {0}")]
    Persistence(#[source] PersistenceError),
    #[error("unknown app {0:?}")]
    UnknownApp(String),
    #[error("unknown setting {key:?} for app {app_id:?}")]
    UnknownSetting { app_id: String, key: String },
    #[error("setting {key:?} belongs to {actual:?} scope, not {target:?}")]
    WrongScope {
        key: String,
        actual: SettingScope,
        target: SettingScope,
    },
    #[error("duplicate mutation for setting {0:?}")]
    DuplicateMutation(String),
    #[error("invalid value for setting {key:?}: {source}")]
    InvalidValue {
        key: String,
        source: ValueValidationError,
    },
    #[error("settings revision conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("settings revision overflow")]
    RevisionOverflow,
    #[error("player id cannot be empty")]
    EmptyPlayerId,
    #[error("invalid persisted settings: {0}")]
    InvalidPersisted(String),
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    const ENABLED: SettingKey<bool> = SettingKey::new("enabled");
    const QUALITY: SettingKey<String> = SettingKey::new("quality");
    const RETRIES: SettingKey<i64> = SettingKey::new("retries");
    const VOLUME: SettingKey<f64> = SettingKey::new("volume");
    const NAME: SettingKey<String> = SettingKey::new("name");

    fn schema() -> AppSettingsSchema {
        AppSettingsSchema::new(
            "app",
            vec![
                SettingDescriptor::Boolean {
                    key: "enabled".to_owned(),
                    label: "Enabled".to_owned(),
                    description: Some("Enable the app".to_owned()),
                    scope: SettingScope::App,
                    default: true,
                },
                SettingDescriptor::Choice {
                    key: "quality".to_owned(),
                    label: "Quality".to_owned(),
                    description: None,
                    scope: SettingScope::AppPlayer,
                    default: "auto".to_owned(),
                    choices: vec![
                        ChoiceOption::new("auto", "Automatic"),
                        ChoiceOption::new("high", "High"),
                    ],
                },
                SettingDescriptor::Integer {
                    key: "retries".to_owned(),
                    label: "Retries".to_owned(),
                    description: None,
                    scope: SettingScope::App,
                    default: 2,
                    min: Some(0),
                    max: Some(5),
                },
                SettingDescriptor::Number {
                    key: "volume".to_owned(),
                    label: "Volume".to_owned(),
                    description: None,
                    scope: SettingScope::AppPlayer,
                    default: 0.5,
                    min: Some(0.0),
                    max: Some(1.0),
                },
                SettingDescriptor::String {
                    key: "name".to_owned(),
                    label: "Name".to_owned(),
                    description: None,
                    scope: SettingScope::App,
                    default: "vibecast".to_owned(),
                    min_length: Some(1),
                    max_length: Some(20),
                },
            ],
        )
        .unwrap()
    }

    fn catalog() -> SettingsCatalog {
        SettingsCatalog::new(vec![schema()]).unwrap()
    }

    async fn service() -> (SettingsService, Arc<MemorySettingsPersistence>) {
        let persistence = Arc::new(MemorySettingsPersistence::default());
        let service = SettingsService::new(catalog(), persistence.clone())
            .await
            .unwrap();
        (service, persistence)
    }

    #[test]
    fn setting_values_are_json_primitives() {
        let cases = [
            (SettingValue::Bool(true), "true"),
            (SettingValue::Integer(4), "4"),
            (SettingValue::Number(1.5), "1.5"),
            (SettingValue::String("x".to_owned()), "\"x\""),
        ];
        for (value, json) in cases {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<SettingValue>(json).unwrap(), value);
        }
    }

    #[test]
    fn schema_rejects_duplicate_keys_and_invalid_defaults() {
        let duplicate = AppSettingsSchema::new(
            "app",
            vec![
                SettingDescriptor::Boolean {
                    key: "same".to_owned(),
                    label: "One".to_owned(),
                    description: None,
                    scope: SettingScope::App,
                    default: false,
                },
                SettingDescriptor::Boolean {
                    key: "same".to_owned(),
                    label: "Two".to_owned(),
                    description: None,
                    scope: SettingScope::App,
                    default: true,
                },
            ],
        );
        assert!(matches!(
            duplicate,
            Err(CatalogError::DuplicateSettingKey { .. })
        ));

        let invalid_default = AppSettingsSchema::new(
            "app",
            vec![SettingDescriptor::Integer {
                key: "count".to_owned(),
                label: "Count".to_owned(),
                description: None,
                scope: SettingScope::App,
                default: 11,
                min: Some(0),
                max: Some(10),
            }],
        );
        assert!(matches!(
            invalid_default,
            Err(CatalogError::InvalidDefault { .. })
        ));
    }

    #[test]
    fn schema_rejects_invalid_choice_number_and_string_constraints() {
        let invalid_choice = SettingDescriptor::Choice {
            key: "choice".to_owned(),
            label: "Choice".to_owned(),
            description: None,
            scope: SettingScope::App,
            default: "missing".to_owned(),
            choices: vec![ChoiceOption::new("present", "Present")],
        };
        assert!(matches!(
            AppSettingsSchema::new("app", vec![invalid_choice]),
            Err(CatalogError::InvalidDefault { .. })
        ));

        let invalid_number = SettingDescriptor::Number {
            key: "number".to_owned(),
            label: "Number".to_owned(),
            description: None,
            scope: SettingScope::App,
            default: 1.0,
            min: Some(2.0),
            max: Some(1.0),
        };
        assert!(matches!(
            AppSettingsSchema::new("app", vec![invalid_number]),
            Err(CatalogError::InvalidConstraints { .. })
        ));

        let invalid_string = SettingDescriptor::String {
            key: "string".to_owned(),
            label: "String".to_owned(),
            description: None,
            scope: SettingScope::App,
            default: "abc".to_owned(),
            min_length: None,
            max_length: Some(2),
        };
        assert!(matches!(
            AppSettingsSchema::new("app", vec![invalid_string]),
            Err(CatalogError::InvalidDefault { .. })
        ));
    }

    #[test]
    fn catalog_rejects_duplicate_apps() {
        assert!(matches!(
            SettingsCatalog::new(vec![schema(), schema()]),
            Err(CatalogError::DuplicateAppId(app)) if app == "app"
        ));
    }

    #[tokio::test]
    async fn snapshots_have_defaults_and_typed_access() {
        let (service, _) = service().await;
        let snapshot = service.host().reader("app").await.unwrap().snapshot();
        assert_eq!(snapshot.revision(), 0);
        assert_eq!(snapshot.get(ENABLED).unwrap(), Some(true));
        assert_eq!(snapshot.get(RETRIES).unwrap(), Some(2));
        assert_eq!(snapshot.get(NAME).unwrap(), Some("vibecast".to_owned()));
        assert_eq!(
            snapshot.get(SettingKey::<bool>::new("missing")).unwrap(),
            None
        );
        assert!(snapshot.get(SettingKey::<i64>::new("enabled")).is_err());
    }

    #[tokio::test]
    async fn host_and_player_updates_publish_effective_snapshots() {
        let (service, persistence) = service().await;
        let host = service.host();
        let player = service.player("living-room").unwrap();
        let mut player_reader = player.reader("app").await.unwrap();

        let host_snapshot = host.set("app", ENABLED, false).await.unwrap();
        assert_eq!(host_snapshot.revision(), 1);
        let observed = player_reader.changed().await.unwrap();
        assert_eq!(observed.get(ENABLED).unwrap(), Some(false));
        assert_eq!(observed.revision(), 1);

        let updated = player
            .compare_and_set_value("app", observed.revision(), QUALITY, "high".to_owned())
            .await
            .unwrap();
        assert_eq!(updated.revision(), 2);
        assert_eq!(updated.get(QUALITY).unwrap(), Some("high".to_owned()));
        assert_eq!(persistence.state().revision, 2);

        let conflict = player.compare_and_set_value("app", 1, VOLUME, 0.8).await;
        assert!(matches!(
            conflict,
            Err(SettingsServiceError::Conflict {
                expected: 1,
                actual: 2
            })
        ));
    }

    #[tokio::test]
    async fn player_update_only_notifies_that_players_reader() {
        let (service, _) = service().await;
        let player_a = service.player("a").unwrap();
        let player_b = service.player("b").unwrap();
        let mut reader_a = player_a.reader("app").await.unwrap();
        let mut reader_b = player_b.reader("app").await.unwrap();
        let mut host_reader = service.host().reader("app").await.unwrap();

        player_a
            .compare_and_set_value("app", 0, QUALITY, "high".to_owned())
            .await
            .unwrap();

        assert_eq!(
            reader_a.changed().await.unwrap().get(QUALITY).unwrap(),
            Some("high".to_owned())
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reader_b.changed())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), host_reader.changed())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn resets_remove_overrides_and_preserve_fallbacks() {
        let (service, _) = service().await;
        let host = service.host();
        let player = service.player("bedroom").unwrap();
        let host_snapshot = host.set("app", NAME, "custom".to_owned()).await.unwrap();
        let player_snapshot = player
            .compare_and_set_value("app", host_snapshot.revision(), QUALITY, "high".to_owned())
            .await
            .unwrap();

        let reset_player = player
            .reset("app", player_snapshot.revision(), QUALITY)
            .await
            .unwrap();
        assert_eq!(reset_player.get(QUALITY).unwrap(), Some("auto".to_owned()));
        assert_eq!(reset_player.get(NAME).unwrap(), Some("custom".to_owned()));

        let reset_host = host.reset_app("app").await.unwrap();
        assert_eq!(reset_host.get(NAME).unwrap(), Some("vibecast".to_owned()));
        let effective = player.reader("app").await.unwrap().snapshot();
        assert_eq!(effective.get(NAME).unwrap(), Some("vibecast".to_owned()));
        assert_eq!(effective.get(QUALITY).unwrap(), Some("auto".to_owned()));
    }

    #[tokio::test]
    async fn rejects_wrong_scope_invalid_values_and_duplicate_mutations() {
        let (service, _) = service().await;
        let host = service.host();
        assert!(matches!(
            host.set("app", QUALITY, "high".to_owned()).await,
            Err(SettingsServiceError::WrongScope { .. })
        ));
        assert!(matches!(
            host.set("app", RETRIES, 10).await,
            Err(SettingsServiceError::InvalidValue { .. })
        ));
        assert!(matches!(
            host.update(
                "app",
                vec![
                    SettingMutation::set(ENABLED, false),
                    SettingMutation::reset(ENABLED),
                ],
            )
            .await,
            Err(SettingsServiceError::DuplicateMutation(key)) if key == "enabled"
        ));
    }

    struct FailingPersistence {
        state: Mutex<PersistedSettings>,
        fail_save: AtomicBool,
    }

    #[async_trait]
    impl SettingsPersistence for FailingPersistence {
        async fn load(&self) -> Result<PersistedSettings, PersistenceError> {
            Ok(self.state.lock().unwrap().clone())
        }

        async fn save(&self, settings: &PersistedSettings) -> Result<(), PersistenceError> {
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(Box::new(io::Error::other("save failed")));
            }
            *self.state.lock().unwrap() = settings.clone();
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_save_does_not_publish_or_change_state() {
        let persistence = Arc::new(FailingPersistence {
            state: Mutex::new(PersistedSettings::default()),
            fail_save: AtomicBool::new(true),
        });
        let service = SettingsService::new(catalog(), persistence.clone())
            .await
            .unwrap();
        let host = service.host();
        let reader = host.reader("app").await.unwrap();

        assert!(matches!(
            host.set("app", ENABLED, false).await,
            Err(SettingsServiceError::Persistence(_))
        ));
        assert_eq!(reader.snapshot().revision(), 0);
        assert_eq!(reader.snapshot().get(ENABLED).unwrap(), Some(true));
        assert!(!reader.receiver.has_changed().unwrap());
        assert_eq!(persistence.state.lock().unwrap().revision, 0);
    }

    #[tokio::test]
    async fn persisted_state_is_loaded_and_validated() {
        let mut state = PersistedSettings {
            revision: 3,
            ..PersistedSettings::default()
        };
        state.apps.insert(
            "app".to_owned(),
            PersistedAppSettings {
                revision: 3,
                values: BTreeMap::from([("enabled".to_owned(), SettingValue::Bool(false))]),
            },
        );
        let persistence = Arc::new(MemorySettingsPersistence::new(state));
        let service = SettingsService::new(catalog(), persistence).await.unwrap();
        let snapshot = service.host().reader("app").await.unwrap().snapshot();
        assert_eq!(snapshot.revision(), 3);
        assert_eq!(snapshot.get(ENABLED).unwrap(), Some(false));

        let mut invalid = PersistedSettings::default();
        invalid.apps.insert(
            "app".to_owned(),
            PersistedAppSettings {
                revision: 0,
                values: BTreeMap::from([(
                    "quality".to_owned(),
                    SettingValue::String("high".to_owned()),
                )]),
            },
        );
        let result =
            SettingsService::new(catalog(), Arc::new(MemorySettingsPersistence::new(invalid)))
                .await;
        assert!(matches!(
            result,
            Err(SettingsServiceError::InvalidPersisted(_))
        ));
    }
}
