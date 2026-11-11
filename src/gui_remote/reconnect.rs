//! Bounded root replacement or lease resume without replacing application state.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::ProducerAuthentication;
use vivid_protocol::auth::Secret32;

// A presenter may disappear briefly during carrier recovery. Do not spin or keep
// retrying an unavailable principal indefinitely. Native establishment retains
// its own I/O deadline; this budget bounds admission of new attempts.
const RETRY_BUDGET: Duration = Duration::from_secs(30);
const FIRST_RETRY: Duration = Duration::from_millis(100);
const MAX_RETRY: Duration = Duration::from_secs(2);

struct Retry {
    since: Option<Instant>,
    delay: Duration,
}
impl Retry {
    fn new() -> Self {
        Self {
            since: None,
            delay: FIRST_RETRY,
        }
    }
    fn schedule(&mut self, now: Instant, presented: bool) -> io::Result<Instant> {
        if presented {
            *self = Self::new();
        }
        let since = *self.since.get_or_insert(now);
        let elapsed = now.saturating_duration_since(since);
        if elapsed >= RETRY_BUDGET {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "remote GUI reconnect budget expired",
            ));
        }
        let wait = self.delay.min(RETRY_BUDGET - elapsed);
        self.delay = self.delay.saturating_mul(2).min(MAX_RETRY);
        Ok(now + wait)
    }
    fn expired(&self, now: Instant) -> bool {
        self.since
            .is_some_and(|since| now.saturating_duration_since(since) >= RETRY_BUDGET)
    }
}

fn retryable(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::Interrupted
    )
}

fn failure_kind(error: &io::Error, transport: Option<io::ErrorKind>) -> io::ErrorKind {
    if let Some(reply) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<crate::PresenterError>())
    {
        return if !reply.fatal
            && matches!(
                reply.code,
                vivid_protocol::messages::ERROR_LIMIT_EXCEEDED
                    | vivid_protocol::messages::ERROR_RATE_LIMITED
                    | vivid_protocol::messages::ERROR_CHANNEL_BUSY
            ) {
            io::ErrorKind::WouldBlock
        } else {
            io::ErrorKind::Other
        };
    }
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    ) {
        error.kind()
    } else {
        transport.unwrap_or(error.kind())
    }
}

fn pin(config: &mut ProducerConfig) -> io::Result<()> {
    if config.is_offline() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote GUI requires a live connection",
        ));
    }
    if config.endpoint_control.is_none() {
        config.endpoint_control = Some(
            std::env::var(vivid_protocol::discovery::ENDPOINT_CONTROL).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "remote GUI control endpoint is absent",
                )
            })?,
        );
        for (value, name) in [
            (
                &mut config.endpoint_interactive,
                vivid_protocol::discovery::ENDPOINT_INTERACTIVE,
            ),
            (
                &mut config.endpoint_realtime,
                vivid_protocol::discovery::ENDPOINT_REALTIME,
            ),
            (
                &mut config.endpoint_bulk,
                vivid_protocol::discovery::ENDPOINT_BULK,
            ),
        ] {
            if value.is_none() {
                *value = std::env::var(name).ok();
            }
        }
    }
    if matches!(
        config.authentication,
        ProducerAuthentication::RootFromEnvironment
    ) {
        let secret = zeroize::Zeroizing::new(
            std::env::var(vivid_protocol::discovery::ROOT_SECRET).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "remote GUI root capability is absent",
                )
            })?,
        );
        config.authentication = ProducerAuthentication::root_hex(&secret)?;
    }
    config.validate()
}

// Root reconnect is a fresh session, never lease-activation replay or a silent
// downgrade from leased authority. Keep authentication non-Clone in the public API.
fn config_with_auth(
    config: &ProducerConfig,
    authentication: ProducerAuthentication,
) -> ProducerConfig {
    ProducerConfig {
        authentication,
        endpoint_control: config.endpoint_control.clone(),
        endpoint_interactive: config.endpoint_interactive.clone(),
        endpoint_realtime: config.endpoint_realtime.clone(),
        endpoint_bulk: config.endpoint_bulk.clone(),
        producer_name: config.producer_name.clone(),
        producer_version: config.producer_version.clone(),
        target_profile: config.target_profile.clone(),
        required_profiles: config.required_profiles.clone(),
        optional_profiles: config.optional_profiles.clone(),
        maximum_control_body: config.maximum_control_body,
        dry_run: config.dry_run,
        trace_dir: config.trace_dir.clone(),
    }
}

fn root_config(config: &ProducerConfig) -> Option<ProducerConfig> {
    let ProducerAuthentication::Root { root_secret } = &config.authentication else {
        return None;
    };
    Some(config_with_auth(
        config,
        ProducerAuthentication::Root {
            root_secret: Secret32::new(*root_secret.expose()),
        },
    ))
}

pub(super) fn control(shared: &Shared, mut config: ProducerConfig) -> io::Result<()> {
    config.target_profile = crate::DESKTOP_SURFACE.into();
    config.required_profiles = vec![
        crate::CORE_CONTROL.into(),
        crate::DESKTOP_SURFACE.into(),
        crate::LIVE_MEDIA.into(),
    ];
    config.required_profiles.sort();
    config.optional_profiles = vec![crate::DESKTOP_INPUT.into(), crate::OBSERVABILITY.into()];
    config.optional_profiles.sort();
    pin(&mut config)?;
    if matches!(config.authentication, ProducerAuthentication::Resume { .. }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote GUI startup requires root or lease activation authority",
        ));
    }
    let root = root_config(&config);
    let template = config_with_auth(&config, ProducerAuthentication::RootFromEnvironment);
    let mut attempt = crate::EstablishmentAttempt::new(config, RETRY_BUDGET)?;
    let mut connection = 0_u64;
    let mut media_id = 0_u64;
    let mut retry = Retry::new();
    let mut graph = recovery::Graph::default();
    loop {
        if shared.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut presented = false;
        let mut resume = None;
        let result = match attempt.connect() {
            Err(error) => Err(io::Error::new(
                failure_kind(&error, None),
                "remote GUI establishment failed",
            )),
            Ok(mut session) => {
                connection = connection
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("remote connection generation exhausted"))?;
                *shared.cancel.lock().expect("cancel mutex") =
                    Some((connection, session.cancel_handle()));
                *shared.media_failure.lock().expect("media failure mutex") = None;
                if root.is_some() {
                    graph = recovery::Graph::default();
                }
                let restored = if session.info().resume_generation > 0 {
                    graph.reconcile(&mut session)
                } else {
                    Ok(())
                };
                {
                    let mut injection = shared.injection.lock().expect("input mutex");
                    if root.is_some() {
                        injection.guard = InputBindingGuard::new();
                    }
                    injection.lifecycle = Some(session.lifecycle.clone());
                }
                let result = restored.and_then(|()| {
                    transport::connected(
                        shared,
                        &mut session,
                        connection,
                        &mut media_id,
                        &mut presented,
                        &mut graph,
                    )
                });
                graph.remember_allocator(&session);
                let failure = shared
                    .media_failure
                    .lock()
                    .expect("media failure mutex")
                    .take()
                    .filter(|(owner, _)| *owner == connection)
                    .map(|(_, kind)| kind);
                let result = result.map_err(|error| {
                    let transport = failure.or(*session
                        .lifecycle
                        .failure_kind
                        .lock()
                        .expect("failure mutex"));
                    io::Error::new(failure_kind(&error, transport), "remote GUI session ended")
                });
                if shared.stopped.load(Ordering::Acquire) {
                    let _ = session.close();
                } else if root.is_none()
                    && result.as_ref().is_err_and(|error| retryable(error.kind()))
                {
                    resume = Some(session.resume_authentication()?);
                }
                result
            }
        };
        shared.disconnect();
        if shared.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let error = result
            .err()
            .unwrap_or_else(|| io::Error::other("remote GUI session ended"));
        if !retryable(error.kind()) {
            return Err(error);
        }
        let until = retry.schedule(Instant::now(), presented)?;
        while Instant::now() < until {
            if shared.stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if retry.expired(Instant::now()) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "remote GUI reconnect budget expired",
            ));
        }
        if let Some(root) = &root {
            attempt = crate::EstablishmentAttempt::new(
                root_config(root).expect("root reconnect template"),
                RETRY_BUDGET,
            )?;
        } else if let Some(authentication) = resume {
            attempt = crate::EstablishmentAttempt::new(
                config_with_auth(&template, authentication),
                RETRY_BUDGET,
            )?;
        }
        // A failed activation/resume establishment retains the exact prepared
        // HELLO. Only a confirmed session produces the next generation's proof.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_authority_errors_cannot_be_reclassified_by_later_eof() {
        let reply = |code, fatal| {
            io::Error::other(crate::PresenterError {
                code,
                fatal,
                request_id: 1,
                detail: crate::ErrorDetail::new(vec![]).unwrap(),
                diagnostic: String::new(),
            })
        };
        let eof = Some(io::ErrorKind::UnexpectedEof);
        assert_eq!(
            failure_kind(
                &reply(vivid_protocol::messages::ERROR_AUTH_FAILED, true),
                eof
            ),
            io::ErrorKind::Other
        );
        assert_eq!(
            failure_kind(
                &reply(vivid_protocol::messages::ERROR_PRECONDITION_FAILED, false),
                eof
            ),
            io::ErrorKind::Other
        );
        assert_eq!(
            failure_kind(
                &reply(vivid_protocol::messages::ERROR_LIMIT_EXCEEDED, false),
                eof
            ),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn retry_budget_and_backoff_bound_attempt_admission() {
        let start = Instant::now();
        let mut retry = Retry::new();
        assert_eq!(retry.schedule(start, false).unwrap(), start + FIRST_RETRY);
        let mut now = start + FIRST_RETRY;
        while !retry.expired(now) {
            let next = retry.schedule(now, false).unwrap();
            assert!(next > now && next - now <= MAX_RETRY);
            assert!(next <= start + RETRY_BUDGET);
            now = next;
        }
        assert_eq!(now, start + RETRY_BUDGET);
        assert!(retry.schedule(now, false).is_err());
        assert_eq!(retry.schedule(now, true).unwrap(), now + FIRST_RETRY);
    }

    #[test]
    fn reconnect_never_replays_lease_activation_or_changes_root_authority() {
        let mut config = ProducerConfig {
            endpoint_control: Some("tcp:127.0.0.1:9".into()),
            authentication: ProducerAuthentication::Root {
                root_secret: Secret32::new([7; 32]),
            },
            ..ProducerConfig::desktop()
        };
        let copied = root_config(&config).unwrap();
        assert_eq!(copied.endpoint_control, config.endpoint_control);
        let ProducerAuthentication::Root { root_secret } = copied.authentication else {
            panic!("root authority changed");
        };
        assert!(vivid_protocol::auth::verify_proof(
            root_secret.expose(),
            &[7; 32]
        ));
        config.authentication =
            ProducerAuthentication::lease_activation_bytes(1, 2, Secret32::new([9; 32])).unwrap();
        assert!(root_config(&config).is_none());
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            assert!(!retryable(kind));
        }
    }

    #[test]
    fn equal_geometry_in_a_fresh_connection_still_requires_new_pixels() {
        let original = Geometry {
            connection: 1,
            generation: 2,
            width: 320,
            height: 240,
        };
        let recovered = Geometry {
            connection: 2,
            ..original
        };
        assert_ne!(original, recovered);
        let shared = Shared::new();
        *shared.geometry.lock().unwrap() = Some(original);
        shared.disconnect();
        assert!(shared.geometry.lock().unwrap().is_none());
        assert!(shared.frame.lock().unwrap().is_none());
        assert_eq!(shared.media_id.load(Ordering::Acquire), 0);
    }
}
