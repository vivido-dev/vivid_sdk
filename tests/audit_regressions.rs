//! Regressions for SDK audit findings (synthetic credentials only).
#![cfg(feature = "presenter")]

use std::{
    io::{self, Cursor, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use vivid_protocol::{
    auth::Secret32,
    messages::{Hello, LaneClass},
    wire::{Connection, ConnectionKind},
};
use vivid_sdk::presenter::*;
use vivid_sdk::*;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl ConnectionFactory for Capture {
    fn open(&self, kind: ConnectionKind, _: Option<LaneClass>) -> io::Result<Connection> {
        Connection::from_streams(
            Box::new(Cursor::new(Vec::<u8>::new())),
            Box::new(self.clone()),
            kind,
        )
    }
}
fn config() -> ProducerConfig {
    ProducerConfig {
        authentication: ProducerAuthentication::Resume {
            context_id: 1,
            lease_id: 2,
            session_id: 3,
            resume_generation: 0,
            attempt_id: [4; 16],
            prior_resume_key: Secret32::new([5; 32]),
        },
        ..ProducerConfig::default()
    }
}
#[test]
fn prepared_resume_retains_exact_transcript() {
    let capture = Capture::default();
    let mut bodies = Vec::new();
    let mut attempt = EstablishmentAttempt::new(config(), Duration::from_secs(5)).unwrap();
    for _ in 0..2 {
        capture.0.lock().unwrap().clear();
        assert!(
            attempt
                .connect_with_factory(Arc::new(capture.clone()))
                .is_err()
        );
        bodies.push(capture.0.lock().unwrap()[40..].to_vec());
    }
    let a = Hello::decode(&bodies[0]).unwrap().1;
    let b = Hello::decode(&bodies[1]).unwrap().1;
    assert_eq!(a.client_nonce, b.client_nonce);
    assert_eq!(bodies[0], bodies[1]);
}
#[test]
fn dropping_producer_releases_root_resources() {
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::terminal(MediaConfig::default()),
        None,
    )
    .unwrap();
    presenter.update_metrics(1, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(1).unwrap();
    let session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap();
    let mut pane = PaneSession::from_session(session).unwrap();
    pane.show_rgba(1, 1, &[255; 4]).unwrap();
    assert!(presenter.wait_for_retained_media(1, Duration::from_secs(2)));
    drop(pane);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !presenter.pane_media_summary(1).tracks.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "root session was not cleaned up"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    presenter.revoke_pane(1);
}
struct InterruptedOnce(bool);
impl Write for InterruptedOnce {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if !self.0 {
            self.0 = true;
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Ok(b.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn presenter_writer_retries_interruption() {
    let preface = vivid_protocol::wire::encode_preface(ConnectionKind::Control, 1_048_576);
    let transport = Transport::new(
        Box::new(Cursor::new(preface)),
        Box::new(InterruptedOnce(false)),
        ConnectionCancel::inert(),
        Arc::new(|_| Ok(())),
    );
    let (reader, _, _) = Reader::new(transport).unwrap();
    assert_eq!(
        reader
            .writer()
            .write_record(vivid_protocol::messages::PONG, 0, &[0xa0])
            .unwrap(),
        1
    );
    reader.writer().flush().unwrap();
    assert_eq!(
        reader
            .writer()
            .write_record_parts(vivid_protocol::messages::PONG, 0, &[&[], &[0xa0], &[]])
            .unwrap(),
        2
    );
    reader.writer().flush().unwrap();
}

fn armed_guard() -> InputBindingGuard {
    use vivid_protocol::{input::INPUT_CLASS_KEYBOARD, revision::SurfaceGeneration};
    let mut guard = InputBindingGuard::new();
    guard.set_preconditions(DesktopPreconditions {
        surface_present: true,
        surface_generation: SurfaceGeneration::ONE,
        capability_mask: INPUT_CLASS_KEYBOARD,
        presented: true,
        lane_live: true,
    });
    guard
        .enable_for_context(
            1,
            2,
            SurfaceGeneration::ONE,
            INPUT_CLASS_KEYBOARD,
            1_000_000,
            6,
        )
        .unwrap();
    guard
        .handle_bound(&InputBindingStatus {
            producer_epoch: 1,
            grant_generation: 7,
            context_id: 1,
            surface_id: 2,
            surface_generation: 1,
            effective_classes: INPUT_CLASS_KEYBOARD,
            state: 1,
            reason: 6,
            watchdog_timeout_us: 1_000_000,
        })
        .unwrap();
    guard
}
#[test]
fn unrelated_input_renewal_and_revocation_are_rejected() {
    use vivid_protocol::time::Monotonic;
    let mut guard = armed_guard();
    let mut foreign = guard.current_tag().unwrap();
    foreign.context_id = 9;
    guard
        .handle_renewal(
            &InputLeaseRenewal {
                binding: foreign,
                renewal_sequence: 1,
                watchdog_timeout_us: 1_000_000,
            },
            Monotonic::from_micros(0),
        )
        .unwrap_err();
    assert!(guard.is_armed(Monotonic::from_micros(1)));
    guard
        .handle_revocation(&InputGrantTermination {
            binding: foreign,
            reason: 0,
        })
        .unwrap_err();
    assert!(guard.grant().is_some());
}
#[test]
fn new_binding_waits_for_its_own_grant() {
    use vivid_protocol::{
        input::INPUT_CLASS_KEYBOARD, revision::SurfaceGeneration, time::Monotonic,
    };
    let mut guard = armed_guard();
    guard
        .enable_for_context(
            9,
            3,
            SurfaceGeneration::ONE,
            INPUT_CLASS_KEYBOARD,
            1_000_000,
            6,
        )
        .unwrap();
    assert!(!guard.is_armed(Monotonic::from_micros(0)));
    assert!(guard.current_tag().is_none());
}
#[test]
fn invalid_file_drop_enable_preserves_epoch() {
    use vivid_protocol::{file_drop::*, revision::SurfaceGeneration};
    let mut guard = FileDropBindingGuard::new();
    assert!(
        guard
            .enable(
                0,
                0,
                SurfaceGeneration::ZERO,
                FileDropDestination::ShellCwd,
                1,
                1,
                1,
                65536,
                1_000_000,
                1_000_000
            )
            .is_err()
    );
    assert_eq!(guard.epoch().get(), 0);
}
struct StallingWriter {
    stream: std::net::TcpStream,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
impl Write for StallingWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if b.len() >= 24
            && u16::from_be_bytes([b[4], b[5]]) == vivid_protocol::messages::TARGET_CHANGED
        {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        self.stream.write(b)
    }
    fn write_vectored(&mut self, slices: &[io::IoSlice<'_>]) -> io::Result<usize> {
        if let Some(b) = slices.first() {
            self.write(b)
        } else {
            Ok(0)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
struct StallingListener {
    tcp: std::net::TcpListener,
    entered: std::sync::mpsc::Sender<()>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
impl PresenterListener for StallingListener {
    fn endpoint(&self) -> String {
        format!("tcp:{}", self.tcp.local_addr().unwrap())
    }
    fn accept(&self) -> io::Result<Transport> {
        let (stream, _) = self.tcp.accept()?;
        stream.set_nonblocking(false)?;
        let read = stream.try_clone()?;
        let cancel = stream.try_clone()?;
        let timeout = stream.try_clone()?;
        Ok(Transport::new(
            Box::new(read),
            Box::new(StallingWriter {
                stream,
                entered: self.entered.clone(),
                release: self.release.lock().unwrap().take().unwrap(),
            }),
            ConnectionCancel::new(move || {
                let _ = cancel.shutdown(std::net::Shutdown::Both);
            }),
            Arc::new(move |d| timeout.set_read_timeout(d)),
        ))
    }
}
#[test]
fn one_blocked_presenter_write_does_not_hold_other_owner_state() {
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let listener = StallingListener {
        tcp,
        entered: entered_tx,
        release: Mutex::new(Some(release_rx)),
    };
    let endpoint = listener.endpoint();
    let presenter = Arc::new(
        VirtualVivid::start_configured(
            listener,
            PresenterConfig::terminal(MediaConfig::default()),
            None,
        )
        .unwrap(),
    );
    presenter.update_metrics(1, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(1).unwrap();
    let session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap();
    let p = presenter.clone();
    let update = std::thread::spawn(move || p.update_metrics(1, 81, 24, (8, 16)));
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let p = presenter.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let query = std::thread::spawn(move || {
        let _ = p.pane_media_summary(2);
        done_tx.send(()).unwrap();
    });
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    release_tx.send(()).unwrap();
    update.join().unwrap();
    query.join().unwrap();
    session.close().unwrap();
}
#[test]
fn identical_root_hello_is_rejected() {
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::terminal(MediaConfig::default()),
        None,
    )
    .unwrap();
    presenter.update_metrics(1, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(1).unwrap();
    let capture = Capture::default();
    assert!(
        Session::connect_with_factory(
            ProducerConfig {
                authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
                ..ProducerConfig::default()
            },
            Arc::new(capture.clone())
        )
        .is_err()
    );
    let body = capture.0.lock().unwrap()[40..].to_vec();
    let endpoint = vivid_protocol::wire::Endpoint::parse(&endpoint).unwrap();
    let mut first = Connection::open(&endpoint, ConnectionKind::Control).unwrap();
    first
        .write_record(vivid_protocol::messages::HELLO, 0, 0, &body)
        .unwrap();
    assert_eq!(
        first.read_record().unwrap().record_type,
        vivid_protocol::messages::WELCOME
    );
    let mut second = Connection::open(&endpoint, ConnectionKind::Control).unwrap();
    second
        .write_record(vivid_protocol::messages::HELLO, 0, 0, &body)
        .unwrap();
    let rejected = second.read_record().unwrap();
    assert_eq!(rejected.record_type, vivid_protocol::messages::ERROR);
    assert_eq!(
        vivid_protocol::messages::parse_error_reply(&rejected.body)
            .unwrap()
            .code,
        vivid_protocol::messages::ERROR_AUTH_FAILED
    );
    first
        .write_record(
            vivid_protocol::messages::PING,
            0,
            0,
            &vivid_protocol::messages::empty(2),
        )
        .unwrap();
    assert_eq!(
        first.read_record().unwrap().record_type,
        vivid_protocol::messages::PONG
    );
    // The same nonce under an independent capability must not revoke or collide with owner 1.
    presenter.update_metrics(2, 80, 24, (8, 16));
    let other_secret = presenter.issue_pane_capability(2).unwrap();
    let mut other_hello = Hello::decode(&body).unwrap().1;
    other_hello
        .authenticate_root(
            &Secret32::from_hex(&other_secret).unwrap(),
            &vivid_protocol::wire::encode_preface(
                ConnectionKind::Control,
                vivid_protocol::CONTROL_MAX_RECORD_BODY,
            ),
        )
        .unwrap();
    let mut other = Connection::open(&endpoint, ConnectionKind::Control).unwrap();
    other
        .write_record(
            vivid_protocol::messages::HELLO,
            0,
            0,
            &other_hello.encode(1).unwrap(),
        )
        .unwrap();
    assert_eq!(
        other.read_record().unwrap().record_type,
        vivid_protocol::messages::WELCOME
    );
    other.writer().shutdown().unwrap();
    first.writer().shutdown().unwrap();
    second.writer().shutdown().unwrap();
}

struct LoseWelcome {
    endpoint: String,
    lose: Arc<std::sync::atomic::AtomicBool>,
    welcome: Arc<Mutex<Vec<u8>>>,
}
struct WelcomeReader {
    stream: std::net::TcpStream,
    lose: Arc<std::sync::atomic::AtomicBool>,
    welcome: Arc<Mutex<Vec<u8>>>,
    first: bool,
}
impl std::io::Read for WelcomeReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        use std::sync::atomic::Ordering;
        if self.first {
            self.first = false;
            let mut header = [0; 24];
            self.stream.read_exact(&mut header)?;
            let header = vivid_protocol::wire::RecordHeader::decode(header);
            let mut body = vec![0; header.body_length as usize];
            self.stream.read_exact(&mut body)?;
            if header.record_type == vivid_protocol::messages::WELCOME {
                *self.welcome.lock().unwrap() = body;
                self.lose.store(false, Ordering::Release);
            }
            self.stream.shutdown(std::net::Shutdown::Both)?;
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.stream.read(bytes)
    }
}
impl ConnectionFactory for LoseWelcome {
    fn open(&self, kind: ConnectionKind, _: Option<LaneClass>) -> io::Result<Connection> {
        use std::sync::atomic::Ordering;
        if !self.lose.load(Ordering::Acquire) {
            return Connection::open(
                &vivid_protocol::wire::Endpoint::parse(&self.endpoint)?,
                kind,
            );
        }
        let stream = std::net::TcpStream::connect(self.endpoint.strip_prefix("tcp:").unwrap())?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        Connection::from_streams(
            Box::new(WelcomeReader {
                stream: stream.try_clone()?,
                lose: self.lose.clone(),
                welcome: self.welcome.clone(),
                first: true,
            }),
            Box::new(stream),
            kind,
        )
    }
}
fn recover_attempt(mut attempt: EstablishmentAttempt, endpoint: &str) -> Session {
    let factory = Arc::new(LoseWelcome {
        endpoint: endpoint.into(),
        lose: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        welcome: Arc::new(Mutex::new(Vec::new())),
    });
    assert!(attempt.connect_with_factory(factory.clone()).is_err());
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let session = loop {
        match attempt.connect_with_factory(factory.clone()) {
            Ok(session) => break session,
            Err(_) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let original = vivid_protocol::messages::Welcome::decode(&factory.welcome.lock().unwrap())
        .unwrap()
        .1;
    assert_eq!(session.info().session_id, original.session_id);
    assert_eq!(session.info().resume_generation, original.resume_generation);
    assert!(
        attempt.connect_with_factory(factory).is_err(),
        "completed attempt was reused"
    );
    session
}
#[test]
fn public_attempt_recovers_lost_activation_and_resume_welcomes() {
    use vivid_protocol::{
        lease::CleanupPolicy,
        resource::{RESOURCE_COUNT, ResourceContract},
    };
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::terminal(MediaConfig::default()),
        None,
    )
    .unwrap();
    presenter.update_metrics(1, 80, 24, (8, 16));
    presenter.update_metrics(2, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(2).unwrap();
    let other = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint.clone()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap();
    let mut config = ProducerConfig {
        endpoint_control: Some(endpoint.clone()),
        ..ProducerConfig::default()
    };
    let mut profiles = config.required_profiles.clone();
    profiles.extend(config.optional_profiles.clone());
    profiles.sort();
    let (definition, mut secret) = SessionLeaseBuilder::new(1, 9)
        .permitted_profiles(profiles)
        .contract(ResourceContract::new([u64::MAX / 4; RESOURCE_COUNT]))
        .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
        .disconnect_grace_us(5_000_000)
        .build()
        .unwrap();
    presenter.issue_lease(1, definition).unwrap();
    config.authentication =
        ProducerAuthentication::lease_activation_bytes(1, 9, secret.take().unwrap()).unwrap();
    let session = recover_attempt(
        EstablishmentAttempt::new(config, Duration::from_secs(5)).unwrap(),
        &endpoint,
    );
    // This presenter reports QUERY_SESSION as unsupported; a typed reply proves admission.
    assert!(
        session
            .query_session()
            .unwrap_err()
            .get_ref()
            .unwrap()
            .is::<PresenterError>()
    );
    let id = session.info().session_id;
    let authentication = session.resume_authentication().unwrap();
    drop(session);
    let config = ProducerConfig {
        endpoint_control: Some(endpoint.clone()),
        authentication,
        ..ProducerConfig::default()
    };
    let resumed = recover_attempt(
        EstablishmentAttempt::new(config, Duration::from_secs(5)).unwrap(),
        &endpoint,
    );
    assert_eq!(
        (resumed.info().session_id, resumed.info().resume_generation),
        (id, 1)
    );
    assert!(
        other
            .query_session()
            .unwrap_err()
            .get_ref()
            .unwrap()
            .is::<PresenterError>()
    );
    resumed.close().unwrap();
    other.close().unwrap();
}

#[test]
fn rebinding_checks_identity_and_resets_renewal_history() {
    use vivid_protocol::{
        input::INPUT_CLASS_KEYBOARD, revision::SurfaceGeneration, time::Monotonic,
    };
    let mut guard = armed_guard();
    let other = armed_guard();
    let old = guard.current_tag().unwrap();
    guard
        .handle_renewal(
            &InputLeaseRenewal {
                binding: old,
                renewal_sequence: 10,
                watchdog_timeout_us: 1_000_000,
            },
            Monotonic::from_micros(0),
        )
        .unwrap();
    guard
        .enable_for_context(
            9,
            2,
            SurfaceGeneration::ONE,
            INPUT_CLASS_KEYBOARD,
            1_000_000,
            6,
        )
        .unwrap();
    let mut status = InputBindingStatus {
        producer_epoch: 2,
        grant_generation: 8,
        context_id: 1,
        surface_id: 2,
        surface_generation: 1,
        effective_classes: INPUT_CLASS_KEYBOARD,
        state: 1,
        reason: 6,
        watchdog_timeout_us: 1_000_000,
    };
    assert!(guard.handle_bound(&status).is_err());
    assert!(guard.current_tag().is_none());
    status.context_id = 9;
    guard.handle_bound(&status).unwrap();
    guard
        .handle_renewal(
            &InputLeaseRenewal {
                binding: guard.current_tag().unwrap(),
                renewal_sequence: 1,
                watchdog_timeout_us: 1_000_000,
            },
            Monotonic::from_micros(1),
        )
        .unwrap();
    assert!(
        guard
            .handle_revocation(&InputGrantTermination {
                binding: old,
                reason: 0
            })
            .is_err()
    );
    assert!(guard.is_armed(Monotonic::from_micros(2)));
    assert_eq!(other.current_tag(), Some(old));
}
