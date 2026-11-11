//! Bounded application-owned persistence without runtime identities or authority.

// Rust guideline compliant 2026-10-07
use super::AppConfig;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{fmt, io};

/// Completes bounded state capture, sharing application-call cancellation semantics.
///
/// Run capture on the owning GUI thread or a bounded native worker. The host
/// cancels it on close/replacement; hide does not cancel this read-only request.
pub type StateResponder = super::automation::Responder<Option<AppState>>;

/// Awaits an owner-scoped state snapshot without blocking the GUI thread.
///
/// Capture has the application-call two-second deadline and independent
/// four-per-owner/64-per-host admission budget. Dropping this cancels queued work.
pub type StateReply = super::automation::Reply<Option<AppState>>;

/// Holds an application's optional bounded, serializable restoration state.
///
/// Bounds match `AppConfig`. State is private application data, never a place
/// for capability secrets, input grants, session keys, or live runtime handles.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppState(AppConfig);
impl fmt::Debug for AppState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}
impl AppState {
    /// Validates application-owned state using the SDK's object and size bounds.
    ///
    /// # Errors
    /// Returns `InvalidInput` for excess bytes, depth, nodes, or a non-object.
    pub fn new(value: Value) -> io::Result<Self> {
        AppConfig::new(value).map(Self)
    }
    /// Decodes state without including its contents in error diagnostics.
    ///
    /// # Errors
    /// Returns `InvalidInput` when state does not match the requested schema.
    pub fn decode<T: DeserializeOwned>(&self) -> io::Result<T> {
        self.0.decode()
    }
}

/// Borrows launch options and optional application-owned restoration state.
#[derive(Clone, Copy, Debug)]
pub struct AppLaunch<'a> {
    /// Validated configuration shared with fresh application creation.
    pub options: &'a AppConfig,
    /// Optional state whose schema is validated by the registered factory.
    pub state: Option<&'a AppState>,
}
impl<'a> AppLaunch<'a> {
    /// Creates a fresh launch without saved application state.
    pub fn new(options: &'a AppConfig) -> Self {
        Self {
            options,
            state: None,
        }
    }
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests;
