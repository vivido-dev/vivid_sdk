//! Witness old control EOF before allowing its leased state to retire.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{
    ProducerAuthentication, ProducerConfig, RequestMetadata, Session, SessionLeaseBuilder,
};
use std::io::Read;
use tests::{producer, raster, surface};

#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    entered: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct EofReader {
    inner: Box<dyn Read + Send>,
    gate: Arc<Gate>,
}
impl Read for EofReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        if count == 0 && self.gate.armed.load(Ordering::Acquire) {
            self.gate.entered.store(true, Ordering::Release);
            let released = self.gate.released.lock().unwrap();
            let (released, _) = self
                .gate
                .changed
                .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                .unwrap();
            assert!(*released, "test did not release witnessed control EOF");
        }
        Ok(count)
    }
}
struct Listener {
    inner: crate::presenter::SocketListener,
    first: AtomicBool,
    gate: Arc<Gate>,
}
impl PresenterListener for Listener {
    fn endpoint(&self) -> String {
        self.inner.endpoint()
    }
    fn accept(&self) -> io::Result<Transport> {
        let mut transport = self.inner.accept()?;
        if !self.first.swap(true, Ordering::AcqRel) {
            let inner = std::mem::replace(&mut transport.reader, Box::new(io::empty()));
            transport.reader = Box::new(EofReader {
                inner,
                gate: self.gate.clone(),
            });
        }
        Ok(transport)
    }
}
fn fixture() -> (
    Arc<VirtualVivid>,
    Arc<Gate>,
    Session,
    Session,
    ProducerConfig,
) {
    let gate = Arc::new(Gate::default());
    let presenter = Arc::new(
        VirtualVivid::start(
            Listener {
                inner: crate::presenter::SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
                first: AtomicBool::new(false),
                gate: gate.clone(),
            },
            MediaConfig::default(),
        )
        .unwrap(),
    );
    let mut clients = Vec::new();
    let mut replacement = None;
    for pane in [1, 2] {
        presenter.update_metrics(pane, 80, 24, (8, 16));
        let mut config = producer(presenter.endpoint(), &"11".repeat(32));
        let mut profiles = config.required_profiles.clone();
        profiles.extend(config.optional_profiles.clone());
        profiles.sort();
        let (definition, mut activation) = SessionLeaseBuilder::new(7, pane)
            .permitted_profiles(profiles)
            .contract(ResourceContract::new(
                [u64::MAX / 4; vivid_protocol::resource::RESOURCE_COUNT],
            ))
            .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
            .disconnect_grace_us(10_000_000)
            .build()
            .unwrap();
        presenter.issue_lease(pane, definition).unwrap();
        config.authentication =
            ProducerAuthentication::lease_activation_bytes(7, pane, activation.take().unwrap())
                .unwrap();
        let mut client = Session::connect(config).unwrap();
        client
            .create_surface(surface(7, 9), &RequestMetadata::default())
            .unwrap();
        client
            .create_track(raster(7, 9, 11), &RequestMetadata::default())
            .unwrap();
        if pane == 1 {
            let mut config = producer(presenter.endpoint(), &"11".repeat(32));
            config.authentication = client.resume_authentication().unwrap();
            replacement = Some(config);
        }
        clients.push(client);
    }
    let first = clients.remove(0);
    let peer = clients.remove(0);
    (presenter, gate, first, peer, replacement.unwrap())
}
fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    while !ready() {
        assert!(Instant::now() < end, "resume transition was not witnessed");
        thread::sleep(Duration::from_millis(1));
    }
}
fn local_surface(client: &Session) -> crate::Surface {
    crate::Surface {
        inner: client.surfaces[&(7, 9)].clone(),
    }
}
fn local_track(client: &Session) -> crate::Track {
    crate::Track {
        inner: client.tracks.lock().unwrap()[&(7, 9, 11)].clone(),
    }
}
#[test]
fn authenticated_resume_waits_for_witnessed_control_eof_without_blocking_same_id_peer() {
    let (presenter, gate, first, peer, config) = fixture();
    let _release = Release(gate.clone());
    let owner = first.info().session_id;
    let other = peer.info().session_id;
    gate.armed.store(true, Ordering::Release);
    first.cancel_handle()();
    wait(|| gate.entered.load(Ordering::Acquire));
    assert_eq!(
        lock(&presenter.state).leases[&(7, 1)].machine.state(),
        LeaseState::Active
    );
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        tx.send(Session::connect(config)).unwrap();
    });
    wait(|| {
        if let Ok(result) = rx.try_recv() {
            panic!(
                "resume completed before old control retirement: {}",
                result.err().map_or("success".into(), |e| e.to_string())
            );
        }
        lock(&presenter.state).waiting_resumes == 1
    });
    assert!(!lock(&presenter.state).sessions[&other].closed);
    // Same context/surface/track IDs, distinct owners: control remains usable.
    peer.query_surface(&local_surface(&peer)).unwrap();
    gate.release();
    let mut resumed = rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!(resumed.info().session_id, owner);
    assert_eq!(resumed.info().resume_generation, 1);
    let retained = local_surface(&first);
    let track = local_track(&first);
    resumed.adopt_surface(&retained).unwrap();
    resumed.adopt_track(&track).unwrap();
    resumed
        .advance_channel(&track, 1, &RequestMetadata::default())
        .unwrap();
    peer.query_track(&local_track(&peer)).unwrap();
    assert_eq!(lock(&presenter.state).waiting_resumes, 0);
    assert!(!lock(&presenter.state).sessions[&other].closed);
    drop(first);
    resumed.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn invalid_resume_proof_never_parks_or_disturbs_the_active_same_id_owners() {
    let (presenter, _gate, first, peer, mut config) = fixture();
    let ProducerAuthentication::Resume {
        prior_resume_key, ..
    } = &mut config.authentication
    else {
        panic!("fixture must prepare resume authentication");
    };
    *prior_resume_key = Secret32::new([0x91; 32]);
    assert!(Session::connect(config).is_err());
    assert_eq!(lock(&presenter.state).waiting_resumes, 0);
    for client in [&first, &peer] {
        client.query_surface(&local_surface(client)).unwrap();
        assert!(!lock(&presenter.state).sessions[&client.info().session_id].closed);
    }
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn authenticated_resume_cannot_take_over_a_live_control_and_expires_with_its_handshake() {
    let (presenter, _gate, first, peer, config) = fixture();
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        tx.send(Session::connect(config)).unwrap();
    });
    wait(|| lock(&presenter.state).waiting_resumes == 1);
    for client in [&first, &peer] {
        client.query_surface(&local_surface(client)).unwrap();
    }
    assert!(
        rx.recv_timeout(HANDSHAKE_TIMEOUT + Duration::from_secs(1))
            .unwrap()
            .is_err()
    );
    worker.join().unwrap();
    wait(|| lock(&presenter.state).waiting_resumes == 0);
    assert_eq!(
        lock(&presenter.state).leases[&(7, 1)].machine.state(),
        LeaseState::Active
    );
    assert_eq!(
        lock(&presenter.state).leases[&(7, 1)]
            .machine
            .resume_generation(),
        ResumeGeneration::ZERO
    );
    for client in [&first, &peer] {
        client.query_surface(&local_surface(client)).unwrap();
    }
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn competing_authenticated_waiters_cannot_both_advance_the_same_resume_generation() {
    let (presenter, gate, first, peer, config) = fixture();
    let _release = Release(gate.clone());
    let owner = first.info().session_id;
    let mut second = producer(presenter.endpoint(), &"11".repeat(32));
    second.authentication = first.resume_authentication().unwrap();
    gate.armed.store(true, Ordering::Release);
    first.cancel_handle()();
    wait(|| gate.entered.load(Ordering::Acquire));
    let (tx, rx) = mpsc::sync_channel(2);
    let workers = [config, second].map(|config| {
        let tx = tx.clone();
        thread::spawn(move || {
            tx.send(Session::connect(config)).unwrap();
        })
    });
    wait(|| lock(&presenter.state).waiting_resumes == 2);
    peer.query_surface(&local_surface(&peer)).unwrap();
    gate.release();
    let results = [
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
    ];
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let mut winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(winner.info().session_id, owner);
    assert_eq!(winner.info().resume_generation, 1);
    let retained = local_surface(&first);
    winner.adopt_surface(&retained).unwrap();
    // Repeating a retired cancellation cannot close the winning incarnation.
    first.cancel_handle()();
    drop(first);
    winner.query_surface(&retained).unwrap();
    peer.query_surface(&local_surface(&peer)).unwrap();
    assert_eq!(lock(&presenter.state).waiting_resumes, 0);
    winner.close().unwrap();
    peer.close().unwrap();
}
