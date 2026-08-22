//! Producer configuration, authentication material, and the carrier factory.
//!
//! Nothing here touches a socket; these are the inputs [`Session::connect`] validates before a
//! preface is written.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

use vivid_protocol::auth::Secret32;
use vivid_protocol::messages::{Envelope, PayloadMap};
use vivid_protocol::wire::Connection;
use vivid_protocol::{auth, messages};

use crate::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestMetadata {
    pub preconditions: PayloadMap,
    pub idempotency_key: Option<[u8; messages::IDEMPOTENCY_KEY_BYTES]>,
    pub causation_id: Option<[u8; messages::CAUSATION_ID_BYTES]>,
}

impl RequestMetadata {
    pub fn validate(&self) -> io::Result<()> {
        if self
            .preconditions
            .windows(2)
            .any(|pair| pair[0].0 >= pair[1].0)
            || self.preconditions.iter().any(|(key, _)| *key > 9)
        {
            return Err(invalid_input(
                "preconditions must be sorted, unique, and use keys 0 through 9",
            ));
        }
        Ok(())
    }

    pub(crate) fn apply(&self, envelope: &mut Envelope) -> io::Result<()> {
        self.validate()?;
        envelope.preconditions.clone_from(&self.preconditions);
        envelope.idempotency_key = self.idempotency_key;
        envelope.causation_id = self.causation_id;
        Ok(())
    }
}

/// A typed presenter rejection. Diagnostic text is display-only and never drives protocol state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterError {
    pub code: u64,
    pub request_id: u64,
    pub detail: ErrorDetail,
    pub fatal: bool,
    pub diagnostic: String,
}

impl std::fmt::Display for PresenterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Vivid presenter rejected request {} with error {}: {}",
            self.request_id, self.code, self.diagnostic
        )
    }
}

impl std::error::Error for PresenterError {}

/// A terminal track failure reported asynchronously by the presenter.
///
/// Unlike [`PresenterError`], this is not a rejection of a correlated request. The presenter may
/// report it while the producer is sending media, so subsequent operations on the handle return
/// this stored cause instead of the less useful fact that the track is no longer live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackLostError {
    /// Stable presenter error code describing the failure class.
    pub code: u64,
    /// Validated structured failure detail supplied by the presenter.
    pub detail: ErrorDetail,
    /// Bounded display-only diagnostic supplied by the presenter.
    pub diagnostic: String,
}

impl std::fmt::Display for TrackLostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Vivid presenter lost the track with error {}: {}",
            self.code, self.diagnostic
        )
    }
}

impl std::error::Error for TrackLostError {}

impl From<messages::ErrorReply> for PresenterError {
    fn from(value: messages::ErrorReply) -> Self {
        Self {
            code: value.code,
            request_id: value.request_id,
            detail: value.detail,
            fatal: value.fatal,
            diagnostic: value.diagnostic,
        }
    }
}

/// Authentication material for a new or resumed 1.5 session.
///
/// This type intentionally implements neither `Debug` nor `Display`.
pub enum ProducerAuthentication {
    /// Read `VIVID_ROOT_SECRET` at connect time.
    RootFromEnvironment,
    Root {
        root_secret: Secret32,
    },
    LeaseActivation {
        context_id: u64,
        lease_id: u64,
        activation_secret: Secret32,
        attempt_id: [u8; auth::ATTEMPT_ID_BYTES],
        proof_of_possession: Option<Vec<u8>>,
    },
    Resume {
        context_id: u64,
        lease_id: u64,
        session_id: u64,
        resume_generation: u64,
        attempt_id: [u8; auth::ATTEMPT_ID_BYTES],
        prior_resume_key: Secret32,
    },
}

/// Opens one fresh Vivid transport for the requested 1.5 connection kind.
///
/// The carrier remains byte-transparent: the SDK still performs the Vivid handshake, derives
/// session and channel keys, authenticates track channels, and enforces sequencing and flow.
pub trait ConnectionFactory: Send + Sync {
    fn open(&self, kind: ConnectionKind, lane: Option<LaneClass>) -> io::Result<Connection>;

    /// Channel binding shared by every Vivid connection opened by this factory generation.
    ///
    /// Native sockets, SSH forwarding, WebSocket, vvmux, transparent relays, and existing test
    /// factories are intentionally unbound. A terminating WebTransport factory overrides this
    /// with the TLS-exporter-derived key for its carrier session.
    fn carrier_binding_key(&self) -> [u8; 32] {
        [0; 32]
    }
}

impl ProducerAuthentication {
    pub fn root_hex(value: &str) -> io::Result<Self> {
        Ok(Self::Root {
            root_secret: Secret32::from_hex(value)
                .map_err(|error| invalid_input(error.to_string()))?,
        })
    }

    pub fn lease_activation_hex(context_id: u64, lease_id: u64, value: &str) -> io::Result<Self> {
        let mut attempt_id = [0; auth::ATTEMPT_ID_BYTES];
        random_bytes(&mut attempt_id)?;
        Ok(Self::LeaseActivation {
            context_id,
            lease_id,
            activation_secret: Secret32::from_hex(value)
                .map_err(|error| invalid_input(error.to_string()))?,
            attempt_id,
            proof_of_possession: None,
        })
    }

    /// Activate a leased session from raw bytes.
    ///
    /// The vvdesk path uses this and only this: the activation secret reaches the worker as raw
    /// bytes over a protected channel and never passes through a `String`. The hex constructor
    /// stays for compatibility with hand-driven sessions.
    pub fn lease_activation_bytes(
        context_id: u64,
        lease_id: u64,
        activation_secret: Secret32,
    ) -> io::Result<Self> {
        let mut attempt_id = [0; auth::ATTEMPT_ID_BYTES];
        random_bytes(&mut attempt_id)?;
        Ok(Self::LeaseActivation {
            context_id,
            lease_id,
            activation_secret,
            attempt_id,
            proof_of_possession: None,
        })
    }
}

/// Connection and negotiation policy. Secret-bearing fields are deliberately non-debuggable.
pub struct ProducerConfig {
    pub endpoint_control: Option<String>,
    pub endpoint_interactive: Option<String>,
    pub endpoint_realtime: Option<String>,
    pub endpoint_bulk: Option<String>,
    pub authentication: ProducerAuthentication,
    pub producer_name: String,
    pub producer_version: String,
    pub target_profile: String,
    pub required_profiles: Vec<String>,
    pub optional_profiles: Vec<String>,
    pub maximum_control_body: u32,
    pub dry_run: bool,
    pub trace_dir: Option<PathBuf>,
}

impl Default for ProducerConfig {
    fn default() -> Self {
        Self {
            endpoint_control: None,
            endpoint_interactive: None,
            endpoint_realtime: None,
            endpoint_bulk: None,
            authentication: ProducerAuthentication::RootFromEnvironment,
            producer_name: "vivid-sdk".into(),
            producer_version: env!("CARGO_PKG_VERSION").into(),
            target_profile: TERMINAL_SURFACE.into(),
            required_profiles: vec![TERMINAL_SURFACE.into(), CORE_CONTROL.into()],
            optional_profiles: vec![LIVE_MEDIA.into(), OBSERVABILITY.into(), TIMED_MEDIA.into()],
            maximum_control_body: DEFAULT_CONTROL_BODY,
            dry_run: false,
            trace_dir: None,
        }
    }
}

impl ProducerConfig {
    pub fn offline() -> Self {
        Self {
            dry_run: true,
            ..Self::default()
        }
    }

    /// A desktop producer's profile set: `desktop-surface-v1` as the target, with live media
    /// required and input and observability offered.
    ///
    /// `desktop-input-v1` is optional rather than required because a presenter that presents a
    /// desktop but refuses injection is a legitimate peer — view-only is a supported deployment,
    /// and requiring the profile would close the session instead of degrading to it.
    pub fn desktop() -> Self {
        let mut required = vec![
            CORE_CONTROL.to_owned(),
            DESKTOP_SURFACE.to_owned(),
            LIVE_MEDIA.to_owned(),
        ];
        required.sort();
        let mut optional = vec![DESKTOP_INPUT.to_owned(), OBSERVABILITY.to_owned()];
        optional.sort();
        Self {
            target_profile: DESKTOP_SURFACE.into(),
            required_profiles: required,
            optional_profiles: optional,
            ..Self::default()
        }
    }

    /// A dry-run desktop producer.
    pub fn offline_desktop() -> Self {
        Self {
            dry_run: true,
            ..Self::desktop()
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.producer_name.len() > 256 || self.producer_version.len() > 128 {
            return Err(invalid_input("producer name or version is too long"));
        }
        if self.maximum_control_body == 0
            || self.maximum_control_body > vivid_protocol::CONTROL_MAX_RECORD_BODY
        {
            return Err(invalid_input("maximum control body must be in 1..=1048576"));
        }
        validate_profiles(&self.required_profiles)?;
        validate_profiles(&self.optional_profiles)?;
        if self
            .required_profiles
            .iter()
            .any(|profile| self.optional_profiles.contains(profile))
        {
            return Err(invalid_input("required and optional profile lists overlap"));
        }
        if !self
            .required_profiles
            .iter()
            .any(|value| value == CORE_CONTROL)
            || !self
                .required_profiles
                .iter()
                .any(|value| value == &self.target_profile)
        {
            return Err(invalid_input(
                "required profiles must contain core and the selected target profile",
            ));
        }
        let offered: BTreeSet<&str> = self
            .required_profiles
            .iter()
            .chain(&self.optional_profiles)
            .map(String::as_str)
            .collect();
        for profile in offered.iter().copied() {
            if let Some(prerequisites) = vivid_protocol::registry::prerequisites(profile) {
                if prerequisites
                    .iter()
                    .any(|required| !offered.contains(required))
                {
                    return Err(invalid_input(format!(
                        "profile {profile:?} is missing a prerequisite"
                    )));
                }
            } else if self.required_profiles.iter().any(|value| value == profile) {
                return Err(invalid_input(format!(
                    "required profile {profile:?} is not registered"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn is_offline(&self) -> bool {
        self.dry_run || self.trace_dir.is_some()
    }
}
