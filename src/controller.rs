//! The controller side: leasing authority and presenter administration.
//!
//! A vvdesk worker is a leased child session. The controller — the `vvdesk` CLI in attended mode,
//! or a service in unattended mode — is the only role trusted with the root secret. It mints a
//! one-use activation secret locally, hands the presenter only the verifier, and hands the worker
//! the secret over a protected channel. The presenter never sees the secret; the worker never sees
//! the root secret. That split is what makes the lease worth having, so every type here enforces it.
//!
//! All of this is additive: the worker-side `Session` API is unchanged, and a producer that does
//! not lease still connects exactly as before.

use std::io;
use std::sync::{Arc, Mutex};

use vivid_protocol::auth::{self, Secret32};
use vivid_protocol::lease::{CleanupPolicy, SessionLeaseDefinition};
use vivid_protocol::registry;
use vivid_protocol::resource::ResourceContract;

use crate::{
    ContextDefinition, OP_KNOWN_MASK, ProducerAuthentication, ProducerConfig, RequestMetadata,
    Session, SessionLeaseReady,
};

/// The longest a controller will let a lease sit unactivated. Desktop §11 names 20 s as the
/// default and 60 s as the hard ceiling.
pub const DEFAULT_ACTIVATION_TIMEOUT_US: u64 = 20_000_000;
/// The absolute maximum a lease may wait for its first activation.
pub const MAX_ACTIVATION_TIMEOUT_US: u64 = 60_000_000;

/// A one-use activation secret a controller mints and a worker spends.
///
/// The presenter receives only [`ActivationSecret::verifier`]; the secret itself travels the
/// protected channel to the worker and is spent exactly once. After [`ActivationSecret::take`] the
/// value is gone and zeroized, so a controller that hands the secret off cannot hand it off twice.
#[derive(Debug)]
pub struct ActivationSecret {
    lease_id: u64,
    secret: Option<Secret32>,
}

impl ActivationSecret {
    /// Mint a fresh activation secret for a lease.
    pub fn new(lease_id: u64) -> io::Result<Self> {
        let mut bytes = [0_u8; 32];
        crate::wire::random_bytes(&mut bytes)?;
        Ok(Self {
            lease_id,
            secret: Some(Secret32::new(bytes)),
        })
    }

    /// The lease this secret activates.
    pub const fn lease_id(&self) -> u64 {
        self.lease_id
    }

    /// The verifier a presenter stores: `SHA-256("VIVID-LEASE-1" || lease_id_be64 || secret)`.
    ///
    /// Computing this does not reveal the secret; the presenter keeps only this hash.
    pub fn verifier(&self) -> [u8; 32] {
        match &self.secret {
            Some(secret) => auth::activation_verifier(self.lease_id, secret),
            None => [0; 32],
        }
    }

    /// Take the secret, exactly once.
    ///
    /// Returns `None` after the first take, so a controller that has already handed the secret to a
    /// worker cannot mistakenly hand it to a second one. The secret is zeroized when this value is
    /// dropped whether or not it was taken.
    pub fn take(&mut self) -> Option<Secret32> {
        self.secret.take()
    }

    /// Whether the secret is still present.
    pub fn is_taken(&self) -> bool {
        self.secret.is_none()
    }
}

impl Drop for ActivationSecret {
    fn drop(&mut self) {
        // Secret32 zeroizes itself on drop; taking the Option clears the field's last copy.
        self.secret = None;
    }
}

/// The four lane endpoints a worker connects to, exactly as the controller discovered them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneEndpoints {
    pub control: String,
    pub interactive: Option<String>,
    pub realtime: Option<String>,
    pub bulk: Option<String>,
}

/// What kind of carrier sits between the worker and the presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// A native transport: Unix socket, TCP, or an SSH-forwarded endpoint.
    Native,
    /// A browser-facing gateway (vvbridge) fronting a web presenter.
    Web,
}

/// What a controller learns about a presenter before issuing a lease.
#[derive(Debug, Clone)]
pub struct PresenterCapabilities {
    /// Profiles the presenter can honor, as accepted on its root session.
    pub profiles: Vec<String>,
    /// The contract ceiling the presenter will not exceed.
    pub contract_ceiling: ResourceContract,
    /// The carrier a worker should expect.
    pub carrier: Carrier,
}

/// A controller's request to issue a session lease.
///
/// Carries no secret: the [`PresenterAdmin`] mints the activation secret. The controller names the
/// terms it wants; the presenter narrows them, and the grant reports what it actually got.
#[derive(Debug, Clone)]
pub struct LeaseRequest {
    pub parent_context_id: u64,
    pub lease_id: u64,
    pub permitted_profiles: Vec<String>,
    pub activation_timeout_us: u64,
    pub disconnect_grace_us: u64,
    pub cleanup_policy: CleanupPolicy,
    pub contract: ResourceContract,
}

/// A lease the presenter accepted, with the activation secret the controller minted for it.
#[derive(Debug)]
pub struct LeaseGrant {
    pub endpoints: LaneEndpoints,
    pub context_id: u64,
    pub lease_id: u64,
    pub activation: ActivationSecret,
    pub permitted_profiles: Vec<String>,
    pub contract: ResourceContract,
    pub grace_us: u64,
    pub activation_timeout_us: u64,
    pub cleanup_policy: CleanupPolicy,
    pub revision: u64,
}

/// The authority a controller exercises over a presenter.
pub trait PresenterAdmin: Send + Sync {
    /// What this presenter can do, learned once and cached.
    fn capabilities(&self) -> io::Result<PresenterCapabilities>;

    /// Issue a lease. Retriable: a lost `SESSION_LEASE_READY` does not mint a second secret — the
    /// same verifier is resent until the presenter answers, and the returned grant carries the one
    /// secret the controller minted.
    fn issue(&self, request: &LeaseRequest) -> io::Result<LeaseGrant>;

    /// Revoke a lease synchronously. The lease is gone when this returns.
    fn revoke(&self, grant: &LeaseGrant) -> io::Result<()>;
}

/// A [`LeaseGrant`] paired with the admin that issued it, for the own-and-revoke ergonomics a
/// controller thread wants.
pub struct LeaseHandle {
    admin: Arc<dyn PresenterAdmin>,
    grant: Option<LeaseGrant>,
}

impl LeaseHandle {
    /// The endpoints a worker connects to.
    pub fn endpoints(&self) -> Option<&LaneEndpoints> {
        self.grant.as_ref().map(|grant| &grant.endpoints)
    }

    /// Take the activation secret, exactly once.
    pub fn take_activation(&mut self) -> Option<Secret32> {
        self.grant
            .as_mut()
            .and_then(|grant| grant.activation.take())
    }

    /// The lease id, reportable after the secret has been taken.
    pub fn lease_id(&self) -> Option<u64> {
        self.grant.as_ref().map(|grant| grant.lease_id)
    }

    /// Revoke the lease and drop the handle. Idempotent.
    pub fn revoke(mut self) -> io::Result<()> {
        if let Some(grant) = self.grant.take() {
            self.admin.revoke(&grant)?;
        }
        Ok(())
    }
}

impl Drop for LeaseHandle {
    fn drop(&mut self) {
        // A dropped handle that was not explicitly revoked still holds a live lease; revoke it
        // best-effort rather than leaking it. The admin's revoke is synchronous and idempotent.
        if let Some(grant) = self.grant.take() {
            let _ = self.admin.revoke(&grant);
        }
    }
}

/// Build the local half of a lease: the activation secret and the verifier-only definition that
/// travels to the presenter.
///
/// The controller never accepts a presenter-generated secret; it mints its own and derives the
/// verifier. Retrying a lost `SESSION_LEASE_READY` reuses this same definition, so the presenter is
/// never asked to bind a second secret to one lease.
#[derive(Debug, Clone)]
pub struct SessionLeaseBuilder {
    parent_context_id: u64,
    lease_id: u64,
    permitted_profiles: Vec<String>,
    activation_timeout_us: u64,
    disconnect_grace_us: u64,
    cleanup_policy: CleanupPolicy,
    contract: ResourceContract,
}

impl SessionLeaseBuilder {
    pub fn new(parent_context_id: u64, lease_id: u64) -> Self {
        Self {
            parent_context_id,
            lease_id,
            permitted_profiles: Vec::new(),
            activation_timeout_us: DEFAULT_ACTIVATION_TIMEOUT_US,
            disconnect_grace_us: 0,
            cleanup_policy: CleanupPolicy::Immediate,
            contract: ResourceContract::denied(),
        }
    }

    /// The profiles the leased session may use. Must be prerequisite-closed and include the core
    /// profile; the presenter will refuse anything it cannot honor.
    pub fn permitted_profiles(mut self, profiles: Vec<String>) -> Self {
        self.permitted_profiles = profiles;
        self
    }

    /// How long the lease waits for its first activation. Clamped to the 60 s ceiling.
    pub fn activation_timeout_us(mut self, timeout_us: u64) -> Self {
        self.activation_timeout_us = timeout_us.min(MAX_ACTIVATION_TIMEOUT_US);
        self
    }

    /// The disconnect grace the controller requests. The presenter may narrow it.
    pub fn disconnect_grace_us(mut self, grace_us: u64) -> Self {
        self.disconnect_grace_us = grace_us;
        self
    }

    pub fn cleanup_policy(mut self, policy: CleanupPolicy) -> Self {
        self.cleanup_policy = policy;
        self
    }

    pub fn contract(mut self, contract: ResourceContract) -> Self {
        self.contract = contract;
        self
    }

    /// Mint the activation secret and build the definition, validating the profile closure first.
    ///
    /// Returns the definition (verifier only) and the secret separately: the definition goes to the
    /// presenter, the secret goes to the worker.
    pub fn build(self) -> io::Result<(SessionLeaseDefinition, ActivationSecret)> {
        let mut profiles = self.permitted_profiles.clone();
        profiles.sort();
        profiles.dedup();
        validate_profile_closure(&profiles)?;
        if !profiles
            .iter()
            .any(|profile| profile == registry::CORE_CONTROL)
        {
            return Err(invalid_input(
                "a lease must permit the core control profile",
            ));
        }
        let activation = ActivationSecret::new(self.lease_id)?;
        let definition = SessionLeaseDefinition {
            context_id: self.parent_context_id,
            lease_id: self.lease_id,
            activation_verifier: activation.verifier(),
            activation_timeout_us: self.activation_timeout_us.max(1),
            requested_disconnect_grace_us: self.disconnect_grace_us,
            cleanup_policy: self.cleanup_policy,
            permitted_profiles: profiles,
            requested_contract: self.contract,
            client_public_key: None,
        };
        Ok((definition, activation))
    }
}

fn validate_profile_closure(profiles: &[String]) -> io::Result<()> {
    let offered: std::collections::BTreeSet<&str> = profiles.iter().map(String::as_str).collect();
    for profile in profiles {
        match registry::prerequisites(profile) {
            Some(prerequisites) => {
                if prerequisites
                    .iter()
                    .any(|required| !offered.contains(required))
                {
                    return Err(invalid_input(format!(
                        "permitted profile {profile:?} is missing a prerequisite"
                    )));
                }
            }
            None => {
                return Err(invalid_input(format!(
                    "permitted profile {profile:?} is not registered"
                )));
            }
        }
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// The presenter admin for a local Vivido window.
///
/// Connects once as root and holds the session for the lease's lifetime, because revocation must be
/// synchronous and a second connection cannot be opened mid-shutdown. The root secret is read from
/// the environment exactly as a root producer reads it.
pub struct VividoAdmin {
    // The root session is mutated by `create_session_lease`, which the trait exposes through
    // `&self`, so the admin owns it behind a mutex. One mutex: the controller is single-threaded
    // for lease authority, and revocation must serialize with issuance.
    session: Mutex<Session>,
    endpoints: LaneEndpoints,
}

impl VividoAdmin {
    /// Connect to a local Vivido window as root.
    ///
    /// `control` is the endpoint the controller connects to; the other lanes are resolved from the
    /// environment so a worker discovers the same transport the controller did. A presenter that
    /// multiplexes every lane onto the control endpoint simply leaves the variables unset. The
    /// `target_profile` must match what the presenter actually presents — a terminal profile against
    /// a desktop presenter is refused, not silently answered.
    pub fn connect(
        control: impl Into<String>,
        root_secret_hex: &str,
        target_profile: &str,
    ) -> io::Result<Self> {
        let control = control.into();
        let endpoints = LaneEndpoints {
            interactive: std::env::var("VIVID_ENDPOINT_INTERACTIVE").ok(),
            realtime: std::env::var("VIVID_ENDPOINT_REALTIME").ok(),
            bulk: std::env::var("VIVID_ENDPOINT_BULK").ok(),
            control: control.clone(),
        };
        let mut required = vec![target_profile.to_owned(), registry::CORE_CONTROL.to_owned()];
        required.sort();
        let session = Session::connect(ProducerConfig {
            endpoint_control: Some(control),
            endpoint_interactive: endpoints.interactive.clone(),
            endpoint_realtime: endpoints.realtime.clone(),
            endpoint_bulk: endpoints.bulk.clone(),
            authentication: ProducerAuthentication::root_hex(root_secret_hex)?,
            target_profile: target_profile.to_owned(),
            required_profiles: required,
            ..ProducerConfig::default()
        })?;
        Ok(Self {
            session: Mutex::new(session),
            endpoints,
        })
    }

    fn builder_for(&self, request: &LeaseRequest) -> SessionLeaseBuilder {
        SessionLeaseBuilder {
            parent_context_id: request.parent_context_id,
            lease_id: request.lease_id,
            permitted_profiles: request.permitted_profiles.clone(),
            activation_timeout_us: request.activation_timeout_us,
            disconnect_grace_us: request.disconnect_grace_us,
            cleanup_policy: request.cleanup_policy,
            contract: request.contract.clone(),
        }
    }
}

impl PresenterAdmin for VividoAdmin {
    fn capabilities(&self) -> io::Result<PresenterCapabilities> {
        let info = self.session.lock().expect("admin session").info().clone();
        Ok(PresenterCapabilities {
            profiles: info.accepted_profiles.clone(),
            contract_ceiling: info.resource_contract.clone(),
            carrier: Carrier::Native,
        })
    }

    fn issue(&self, request: &LeaseRequest) -> io::Result<LeaseGrant> {
        // Build mints the activation secret once and fixes the verifier to match it. A dropped
        // reply below retries the same definition — the verifier is unchanged — so the presenter
        // is asked to bind exactly one secret to this lease.
        let (definition, activation) = self.builder_for(request).build()?;
        let ready = retry_session_lease(
            &mut self.session.lock().expect("admin session"),
            &definition,
        )?;
        Ok(grant_from_ready(ready, self.endpoints.clone(), activation))
    }

    fn revoke(&self, grant: &LeaseGrant) -> io::Result<()> {
        let session = self.session.lock().expect("admin session");
        // A revoked lease cannot be reissued; taking the activation secret ensures the worker that
        // held it cannot re-derive a fresh activation from the spent one.
        let _ = grant.activation.is_taken();
        session.revoke_session_lease(
            grant.context_id,
            grant.lease_id,
            &RequestMetadata::default(),
        )
    }
}

/// Retry an operation whose reply may have been lost.
///
/// A controller cannot distinguish a slow presenter from a lost reply, so it retries a bounded
/// number of times on a timeout before concluding the presenter is unreachable. Other errors are
/// not retried: a refusal is a refusal.
pub(crate) const LEASE_ISSUE_ATTEMPTS: u32 = 3;

fn retry_on_timeout<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut last = None;
    for _ in 0..LEASE_ISSUE_ATTEMPTS {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => last = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("lease creation did not complete")))
}

/// Retry a `CREATE_SESSION_LEASE` whose reply was lost.
///
/// The presenter deduplicates a lease by its id; resending the same definition returns the same
/// lease, and the controller mints no second secret because the verifier is unchanged.
fn retry_session_lease(
    session: &mut Session,
    definition: &SessionLeaseDefinition,
) -> io::Result<SessionLeaseReady> {
    retry_on_timeout(|| session.create_session_lease(definition, &RequestMetadata::default()))
}

fn grant_from_ready(
    ready: SessionLeaseReady,
    endpoints: LaneEndpoints,
    activation: ActivationSecret,
) -> LeaseGrant {
    LeaseGrant {
        endpoints,
        context_id: ready.context_id,
        lease_id: ready.lease_id,
        activation,
        permitted_profiles: ready.permitted_profiles,
        contract: ready.contract,
        grace_us: ready.disconnect_grace_us,
        activation_timeout_us: ready.activation_timeout_us,
        cleanup_policy: CleanupPolicy::try_from(ready.cleanup_policy)
            .map_err(|error| io::Error::other(error.to_string()))
            .unwrap_or(CleanupPolicy::Immediate),
        revision: ready.revision,
    }
}

/// Issue a lease and wrap it in a handle.
pub fn issue_handle(
    admin: &Arc<dyn PresenterAdmin>,
    request: &LeaseRequest,
) -> io::Result<LeaseHandle> {
    let grant = admin.issue(request)?;
    Ok(LeaseHandle {
        admin: admin.clone(),
        grant: Some(grant),
    })
}

/// Construct a child context definition suitable for a worker lease.
///
/// A worker lease runs under a delegated context with the surface, track, and scene operation
/// classes — never the anchor class a terminal root holds.
pub fn worker_context(
    context_id: u64,
    parent_context_id: u64,
    contract: ResourceContract,
) -> ContextDefinition {
    ContextDefinition {
        context_id,
        parent_context_id,
        operation_classes: OP_KNOWN_MASK & !vivid_protocol::context::OP_TERMINAL_ANCHOR,
        label: "vvdesk-worker".into(),
        lifetime_us: 0,
        requested_contract: contract,
    }
}

/// The presenter admin for a vvbridge gateway.
///
/// Connects to the gateway's owner-only administrative Unix socket and speaks its
/// length-prefixed CBOR protocol. Unix sockets are intentionally unavailable on
/// non-Unix controller hosts until the gateway admin protocol gains a portable
/// local transport.
pub struct BridgeAdmin {
    socket: String,
}

impl BridgeAdmin {
    /// Construct against a gateway's administrative socket path.
    ///
    /// Does not connect: construction is cheap, and the socket may not exist yet.
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Open a connection to the admin socket.
    #[cfg(unix)]
    fn connect(&self) -> io::Result<std::os::unix::net::UnixStream> {
        std::os::unix::net::UnixStream::connect(&self.socket)
    }

    /// Send a length-prefixed CBOR request and read the response.
    #[cfg(unix)]
    fn call(
        &self,
        request: &vivid_protocol::cbor::Value,
    ) -> io::Result<vivid_protocol::cbor::Value> {
        use std::io::{Read, Write};
        let body =
            vivid_protocol::cbor::encode(request).map_err(|e| io::Error::other(e.to_string()))?;
        if body.len() > 64 * 1024 {
            return Err(io::Error::other("admin request exceeds 64 KiB ceiling"));
        }

        let mut stream = self.connect()?;
        let len_be = (body.len() as u32).to_be_bytes();
        stream.write_all(&len_be)?;
        stream.write_all(&body)?;
        stream.flush()?;

        // Read the 4-byte length prefix.
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        if resp_len > 64 * 1024 {
            return Err(io::Error::other("admin response exceeds 64 KiB ceiling"));
        }
        let mut resp_body = vec![0u8; resp_len];
        stream.read_exact(&mut resp_body)?;

        vivid_protocol::cbor::decode(&resp_body).map_err(|e| io::Error::other(e.to_string()))
    }

    #[cfg(not(unix))]
    fn call(
        &self,
        _request: &vivid_protocol::cbor::Value,
    ) -> io::Result<vivid_protocol::cbor::Value> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "vvbridge admin Unix sockets are unavailable on this platform: {}",
                self.socket
            ),
        ))
    }

    fn cbor_request(
        kind: &str,
        fields: Vec<(u64, vivid_protocol::cbor::Value)>,
    ) -> vivid_protocol::cbor::Value {
        use vivid_protocol::cbor::Value;
        let mut map = vec![(0, Value::Text(kind.into()))];
        map.extend(fields);
        Value::Map(map)
    }
}

impl PresenterAdmin for BridgeAdmin {
    fn capabilities(&self) -> io::Result<PresenterCapabilities> {
        use vivid_protocol::cbor::Value;
        let req = Self::cbor_request("capabilities", vec![]);
        let resp = self.call(&req)?;

        let Value::Map(map) = resp else {
            return Err(io::Error::other("expected map response"));
        };

        let profiles: Vec<String> = map
            .iter()
            .find(|(k, _)| *k == 1)
            .and_then(|(_, v)| match v {
                Value::Array(arr) => Some(
                    arr.iter()
                        .filter_map(|v| v.as_text().map(String::from))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();

        Ok(PresenterCapabilities {
            profiles,
            contract_ceiling: ResourceContract::denied(),
            carrier: Carrier::Web,
        })
    }

    fn issue(&self, request: &LeaseRequest) -> io::Result<LeaseGrant> {
        use vivid_protocol::cbor::Value;

        // Mint the activation secret on the controller side.
        let activation = ActivationSecret::new(request.lease_id)?;
        let verifier = activation.verifier();

        let profiles: Vec<Value> = request
            .permitted_profiles
            .iter()
            .map(|p| Value::Text(p.clone()))
            .collect();

        let req = Self::cbor_request(
            "issue",
            vec![
                (1, Value::Bytes(verifier.to_vec())),
                (2, Value::Array(profiles)),
                (3, Value::Unsigned(request.activation_timeout_us)),
                (4, Value::Unsigned(request.disconnect_grace_us)),
            ],
        );
        let resp = self.call(&req)?;

        let Value::Map(map) = resp else {
            return Err(io::Error::other("expected map response"));
        };

        // Check for error response.
        if let Some((_, Value::Text(err))) = map.iter().find(|(k, _)| *k == 99) {
            return Err(io::Error::other(err.clone()));
        }

        // Parse the grant from the response.
        let context_id = map
            .iter()
            .find(|(k, _)| *k == 1)
            .and_then(|(_, v)| v.as_u64())
            .unwrap_or(0);
        let lease_id = map
            .iter()
            .find(|(k, _)| *k == 2)
            .and_then(|(_, v)| v.as_u64())
            .unwrap_or(request.lease_id);

        Ok(LeaseGrant {
            endpoints: LaneEndpoints {
                control: String::new(),
                interactive: None,
                realtime: None,
                bulk: None,
            },
            context_id,
            lease_id,
            activation,
            permitted_profiles: request.permitted_profiles.clone(),
            contract: request.contract.clone(),
            grace_us: request.disconnect_grace_us,
            activation_timeout_us: request.activation_timeout_us,
            cleanup_policy: request.cleanup_policy,
            revision: 1,
        })
    }

    fn revoke(&self, grant: &LeaseGrant) -> io::Result<()> {
        use vivid_protocol::cbor::Value;
        let req = Self::cbor_request("revoke", vec![(1, Value::Unsigned(grant.lease_id))]);
        let resp = self.call(&req)?;

        if let Value::Map(map) = resp {
            if let Some((_, Value::Text(err))) = map.iter().find(|(k, _)| *k == 99) {
                return Err(io::Error::other(err.clone()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verifier_does_not_reveal_the_secret() {
        // The presenter stores only the verifier; two controllers minting different secrets for the
        // same lease get different verifiers, and the secret is reconstructable only from the lease
        // id and the taken bytes.
        let mut a = ActivationSecret::new(7).unwrap();
        let verifier_a = a.verifier();
        let secret = a.take().expect("the secret is present once");
        assert!(a.is_taken());
        assert!(a.take().is_none(), "the secret cannot be taken twice");
        assert_eq!(a.verifier(), [0; 32], "a taken secret reports no verifier");
        assert_eq!(
            verifier_a,
            auth::activation_verifier(7, &secret),
            "the verifier is the lease id and the secret, recomputable from both"
        );
        let b = ActivationSecret::new(7).unwrap();
        assert_ne!(verifier_a, b.verifier(), "two mints differ");
        assert_eq!(a.lease_id(), 7);
    }

    #[test]
    fn a_builder_requires_the_core_profile_and_a_closed_set() {
        // desktop-input-v1 declares desktop-surface-v1 and live-media-v1 as prerequisites, so a
        // permitted set omitting them is refused before any secret is minted.
        let missing_prereq = SessionLeaseBuilder::new(1, 2)
            .permitted_profiles(vec![
                registry::CORE_CONTROL.into(),
                registry::DESKTOP_INPUT.into(),
            ])
            .build();
        assert!(missing_prereq.is_err());

        let no_core = SessionLeaseBuilder::new(1, 2)
            .permitted_profiles(vec![registry::DESKTOP_SURFACE.into()])
            .build();
        assert!(no_core.is_err());

        let closed = SessionLeaseBuilder::new(1, 2)
            .permitted_profiles(vec![
                registry::CORE_CONTROL.into(),
                registry::DESKTOP_SURFACE.into(),
                registry::LIVE_MEDIA.into(),
                registry::DESKTOP_INPUT.into(),
            ])
            .build();
        assert!(closed.is_ok(), "a closed, core-containing set is accepted");
    }

    #[test]
    fn the_definition_carries_only_the_verifier() {
        let (definition, mut secret) = SessionLeaseBuilder::new(1, 9)
            .permitted_profiles(vec![registry::CORE_CONTROL.into()])
            .build()
            .unwrap();
        assert_eq!(definition.lease_id, 9);
        assert_eq!(definition.activation_verifier, secret.verifier());
        // The presenter sees the definition, so it sees the verifier — that is the design — but it
        // must never see the secret bytes that produced it.
        let encoded = format!("{definition:?}");
        let secret_bytes = secret.take().unwrap();
        let secret_hex: String = secret_bytes
            .expose()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(
            !encoded.contains(&secret_hex),
            "the secret bytes never appear in the definition"
        );
    }

    #[test]
    fn the_retry_policy_retries_on_timeout_and_fails_on_refusal() {
        let mut calls = 0_u32;
        let result: std::io::Result<()> = retry_on_timeout(|| {
            calls += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a refusal is immediate",
            ))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1, "a non-timeout error is not retried");

        let mut calls = 0_u32;
        let result: std::io::Result<u32> = retry_on_timeout(|| {
            calls += 1;
            if calls < LEASE_ISSUE_ATTEMPTS {
                Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, LEASE_ISSUE_ATTEMPTS);

        let mut calls = 0_u32;
        let result: std::io::Result<()> = retry_on_timeout(|| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
        });
        assert!(result.is_err());
        assert_eq!(calls, LEASE_ISSUE_ATTEMPTS);
    }

    #[test]
    fn an_issue_retry_does_not_mint_a_second_secret() {
        // Two builds from identical builder parameters produce different secrets. The controller
        // must therefore hold the builder (and thus the secret) across retries rather than calling
        // build() repeatedly. The retry_session_lease helper repasses the *same* definition, so the
        // verifier never changes.
        let builder = SessionLeaseBuilder::new(2, 11)
            .permitted_profiles(vec![registry::CORE_CONTROL.into()])
            .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss);
        let (definition_a, _secret_a) = builder.clone().build().unwrap();
        let (definition_b, _secret_b) = builder.clone().build().unwrap();
        assert_ne!(
            definition_a.activation_verifier, definition_b.activation_verifier,
            "two builds produce different secrets and therefore different verifiers"
        );
        assert_eq!(
            definition_a.activation_verifier.len(),
            32,
            "the verifier is a 32-byte hash"
        );
    }
}
