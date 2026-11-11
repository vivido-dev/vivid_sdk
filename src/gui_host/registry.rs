//! Registered application factories and bounded, application-owned launch configuration.
//!
//! Registration never creates an application. Configuration validation must not
//! start runtimes or acquire platform resources. Each factory decodes its own
//! options; the SDK bounds the generic object before application code sees it.

// Rust guideline compliant 2026-10-07
use super::{AppFactory, AppHost, AppLaunch, InstanceId};
use gpui::{App, Window};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{collections::BTreeMap, fmt, io};

/// Maximum serialized JSON bytes retained by one application's launch options.
pub const MAX_CONFIG_BYTES: usize = 256 * 1024;
// Bound traversal, cloning, and application decoding independently of byte size.
const MAX_CONFIG_DEPTH: usize = 16;
const MAX_CONFIG_NODES: usize = 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 128;
const MAX_REASON_BYTES: usize = 512;

/// Validates a stable, bounded application registration name.
///
/// IDs start with a lowercase ASCII letter and contain lowercase letters,
/// digits, dots, underscores, or hyphens. They carry identity, never capabilities.
///
/// # Errors
/// Returns `InvalidInput` for empty, oversized, or malformed IDs.
pub fn validate_app_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > MAX_ID_BYTES
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(invalid("invalid application registration ID"));
    }
    Ok(())
}

/// Owns bounded per-instance configuration without revealing values in diagnostics.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct AppConfig(Value);
impl Default for AppConfig {
    fn default() -> Self {
        Self(Value::Object(Default::default()))
    }
}
impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig").finish_non_exhaustive()
    }
}
impl<'de> Deserialize<'de> for AppConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
impl AppConfig {
    /// Validates and retains an application's configuration object.
    ///
    /// # Errors
    /// Returns `InvalidInput` for a non-object, excess bytes, depth, or node count.
    pub fn new(value: Value) -> io::Result<Self> {
        if !value.is_object() {
            return Err(invalid("application options must be an object"));
        }
        let mut nodes = 0;
        validate_value(&value, 0, &mut nodes)?;
        struct Limit(usize);
        impl io::Write for Limit {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .filter(|n| *n <= MAX_CONFIG_BYTES)
                    .ok_or_else(|| invalid("application options exceed 256 KiB"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Limit(0), &value)
            .map_err(|_| invalid("application options exceed 256 KiB"))?;
        Ok(Self(value))
    }

    /// Decodes options using the registered application's own schema.
    ///
    /// Applications should reject unknown fields. Decode errors omit property
    /// values; applications may add their own bounded, content-free guidance.
    ///
    /// # Errors
    /// Returns `InvalidInput` when the options do not match the requested type.
    pub fn decode<T: DeserializeOwned>(&self) -> io::Result<T> {
        T::deserialize(&self.0).map_err(|_| invalid("invalid application options"))
    }

    /// Whether no per-instance properties were supplied.
    pub fn is_empty(&self) -> bool {
        self.0.as_object().is_some_and(|map| map.is_empty())
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn validate_value(value: &Value, depth: usize, nodes: &mut usize) -> io::Result<()> {
    if depth > MAX_CONFIG_DEPTH || *nodes >= MAX_CONFIG_NODES {
        return Err(invalid("application options exceed depth or node limits"));
    }
    *nodes += 1;
    match value {
        Value::Array(values) => {
            for value in values {
                validate_value(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key.len() > MAX_CONFIG_BYTES {
                    return Err(invalid("application option key exceeds byte limit"));
                }
                validate_value(value, depth + 1, nodes)?;
            }
        }
        Value::String(value) if value.len() > MAX_CONFIG_BYTES => {
            return Err(invalid("application option string exceeds byte limit"));
        }
        _ => (),
    }
    Ok(())
}

/// Identifies an application and its human-readable launcher name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDescriptor {
    /// Stable registration ID used by configuration and launch requests.
    pub id: String,
    /// Short display name used by host chrome.
    pub name: String,
}
impl AppDescriptor {
    /// Describes an application; registration validates these values before use.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        validate_app_id(&self.id)?;
        if self.name.len() > MAX_NAME_BYTES
            || self.name.trim().is_empty()
            || self.name.chars().any(char::is_control)
        {
            return Err(invalid("invalid application display name"));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Entry {
    descriptor: AppDescriptor,
    factory: Option<Box<dyn AppFactory>>,
    unavailable: Option<String>,
}

/// Resolves application names to factories without starting their runtimes.
#[derive(Debug)]
pub struct AppRegistry {
    entries: BTreeMap<String, Entry>,
    limit: usize,
}
impl AppRegistry {
    /// Creates an empty registry with an explicit registration capacity.
    pub fn new(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            limit,
        }
    }

    fn insert(&mut self, entry: Entry) -> io::Result<()> {
        entry.descriptor.validate()?;
        if self.entries.contains_key(&entry.descriptor.id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "application ID is already registered",
            ));
        }
        if self.entries.len() >= self.limit {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "application registration limit reached",
            ));
        }
        self.entries.insert(entry.descriptor.id.clone(), entry);
        Ok(())
    }
    /// Registers a factory without creating an application instance.
    ///
    /// # Errors
    /// Returns an error for malformed metadata, duplicate IDs, or capacity.
    pub fn register(&mut self, factory: Box<dyn AppFactory>) -> io::Result<()> {
        self.insert(Entry {
            descriptor: factory.descriptor(),
            factory: Some(factory),
            unavailable: None,
        })
    }
    /// Records a known application disabled in this build or host context.
    ///
    /// # Errors
    /// Returns an error for malformed metadata/reason, duplicate IDs, or capacity.
    pub fn register_unavailable(
        &mut self,
        descriptor: AppDescriptor,
        reason: impl Into<String>,
    ) -> io::Result<()> {
        let reason = reason.into();
        if reason.len() > MAX_REASON_BYTES
            || reason.trim().is_empty()
            || reason.chars().any(char::is_control)
        {
            return Err(invalid("invalid application availability reason"));
        }
        self.insert(Entry {
            descriptor,
            factory: None,
            unavailable: Some(reason),
        })
    }
    /// Lists all known applications in stable registration-ID order.
    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = &AppDescriptor> + '_ {
        self.entries.values().map(|entry| &entry.descriptor)
    }
    /// Lists launchable applications without including unavailable registrations.
    pub fn available(&self) -> impl Iterator<Item = &AppDescriptor> + '_ {
        self.entries
            .values()
            .filter(|entry| entry.factory.is_some())
            .map(|entry| &entry.descriptor)
    }
    /// Returns metadata for a known application, including unavailable ones.
    pub fn descriptor(&self, id: &str) -> Option<&AppDescriptor> {
        self.entries.get(id).map(|entry| &entry.descriptor)
    }
    /// Explains why a known application is unavailable, when applicable.
    pub fn unavailable_reason(&self, id: &str) -> Option<&str> {
        self.entries
            .get(id)
            .and_then(|entry| entry.unavailable.as_deref())
    }

    fn factory(&self, id: &str) -> io::Result<&dyn AppFactory> {
        validate_app_id(id)?;
        let entry = self.entries.get(id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("application {id:?} is not registered"),
            )
        })?;
        entry.factory.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                entry
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| "application is unavailable".into()),
            )
        })
    }
    /// Validates an application launch without creating GUI or runtime resources.
    ///
    /// # Errors
    /// Returns an error for unknown/unavailable applications or invalid options.
    pub fn validate_launch(&self, id: &str, config: &AppConfig) -> io::Result<()> {
        self.validate_restore(id, AppLaunch::new(config))
    }
    /// Validates options and optional state without creating application resources.
    ///
    /// # Errors
    /// Returns lookup, configuration, or saved-state validation errors.
    pub fn validate_restore(&self, id: &str, launch: AppLaunch<'_>) -> io::Result<()> {
        let factory = self.factory(id)?;
        factory.validate(launch.options)?;
        if let Some(state) = launch.state {
            factory.validate_state(launch.options, state)?;
        }
        Ok(())
    }
    /// Mounts a registered application with optional application-owned saved state.
    ///
    /// # Errors
    /// Returns lookup, capacity, launch validation, or initialization errors.
    pub fn mount_launch(
        &self,
        host: &mut AppHost,
        id: &str,
        launch: AppLaunch<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        host.mount_launch(self.factory(id)?, launch, window, cx)
    }
    /// Replaces one owner using optional validated application-owned saved state.
    ///
    /// # Errors
    /// Returns lookup, identity, launch validation, or creation errors atomically.
    pub fn replace_launch(
        &self,
        host: &mut AppHost,
        instance: InstanceId,
        id: &str,
        launch: AppLaunch<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        host.replace_launch(instance, self.factory(id)?, launch, window, cx)
    }
    /// Replaces an instance using a registered application's original launch settings.
    ///
    /// # Errors
    /// Returns lookup, configuration, identity, or creation errors without closing the old instance.
    pub fn replace(
        &self,
        host: &mut AppHost,
        instance: InstanceId,
        id: &str,
        config: &AppConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        host.replace(instance, self.factory(id)?, config, window, cx)
    }

    /// Mounts a registered application into the supplied owner-scoped host.
    ///
    /// # Errors
    /// Returns an error for lookup, configuration, host capacity, or initialization failure.
    pub fn mount(
        &self,
        host: &mut AppHost,
        id: &str,
        config: &AppConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        host.mount(self.factory(id)?, config, window, cx)
    }
}
