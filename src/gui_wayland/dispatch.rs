//! Connection-specific liveness guard for the native backend's request drain.
//!
//! Socket shutdown interrupts continuous request reads without taking the
//! backend's dispatch mutex. It does not preempt renderer or application code.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

// A private client must yield for supervision/input between bounded SHM batches.
pub(super) const MAX_DISPATCH_TIME: Duration = Duration::from_millis(250);

pub(super) struct Connection {
    pub(super) client: smithay::reexports::wayland_server::Client,
    pub(super) socket: Arc<UnixStream>,
}
impl Connection {
    pub(super) fn ready(&self) -> io::Result<bool> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut descriptor = [PollFd::new(self.socket.as_ref(), PollFlags::IN)];
        match poll(&mut descriptor, Some(&Timespec::default())) {
            Ok(count) => Ok(count != 0),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}
struct Active {
    sequence: u64,
    deadline: Instant,
    socket: Arc<UnixStream>,
    expired: Arc<AtomicBool>,
}
#[derive(Default)]
struct State {
    next: u64,
    active: Option<Active>,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    cancelled: AtomicBool,
}

/// Permanently cancels one native display's dispatch and input admission.
///
/// Clones retain their original display lifetime. Cancellation shuts down only
/// its currently draining connection, without acquiring the protocol mutex.
/// Drop the host to finish resource cleanup. Renderer callbacks are not preempted.
#[derive(Clone)]
pub struct DispatchControl {
    shared: Arc<Shared>,
}
impl fmt::Debug for DispatchControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DispatchControl")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}
impl DispatchControl {
    /// Rejects new input and interrupts the current request drain.
    ///
    /// Operations already admitted may finish. The embedding owner must also
    /// revoke its platform services and drop the host to finish cleanup.
    pub fn cancel(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("dispatch watchdog poisoned");
        self.shared.cancelled.store(true, Ordering::Release);
        if let Some(active) = state.active.take() {
            let _ = active.socket.shutdown(Shutdown::Both);
            active.expired.store(true, Ordering::Release);
        }
        self.shared.changed.notify_one();
    }
    /// Whether this display lifetime has been permanently cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Acquire)
    }
}

pub(super) struct Watchdog {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    timeout: Duration,
}
pub(super) struct Guard {
    shared: Arc<Shared>,
    sequence: u64,
    expired: Arc<AtomicBool>,
    armed: bool,
}
impl Watchdog {
    pub(super) fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Acquire)
    }
    pub(super) fn control(&self) -> DispatchControl {
        DispatchControl {
            shared: self.shared.clone(),
        }
    }
    pub(super) fn new() -> io::Result<Self> {
        Self::with_timeout(MAX_DISPATCH_TIME)
    }
    fn with_timeout(timeout: Duration) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let watched = shared.clone();
        let worker = thread::Builder::new()
            .name("vivid-wayland-dispatch".into())
            .spawn(move || watch(watched))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            timeout,
        })
    }
    pub(super) fn arm(&self, socket: Arc<UnixStream>) -> io::Result<Guard> {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("dispatch watchdog poisoned");
        if self.shared.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "native dispatch cancelled",
            ));
        }
        if state.active.is_some() {
            return Err(io::Error::other("dispatch watchdog unavailable"));
        }
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("dispatch identity exhausted"))?;
        let sequence = state.next;
        let expired = Arc::new(AtomicBool::new(false));
        state.active = Some(Active {
            sequence,
            deadline: Instant::now() + self.timeout,
            socket,
            expired: expired.clone(),
        });
        self.shared.changed.notify_one();
        Ok(Guard {
            shared: self.shared.clone(),
            sequence,
            expired,
            armed: true,
        })
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("dispatch watchdog poisoned");
            self.shared.cancelled.store(true, Ordering::Release);
            state.active.take();
            self.shared.changed.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("dispatch watchdog panicked");
        }
    }
}
impl Guard {
    pub(super) fn finish(mut self) -> bool {
        self.disarm();
        self.expired.load(Ordering::Acquire)
    }
    fn disarm(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self
            .shared
            .state
            .lock()
            .expect("dispatch watchdog poisoned");
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.sequence == self.sequence)
        {
            state.active.take();
            self.shared.changed.notify_one();
        }
        self.armed = false;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.disarm();
    }
}
fn watch(shared: Arc<Shared>) {
    let mut state = shared.state.lock().expect("dispatch watchdog poisoned");
    loop {
        if shared.cancelled.load(Ordering::Acquire) {
            return;
        }
        let Some(active) = state.active.as_ref() else {
            state = shared
                .changed
                .wait(state)
                .expect("dispatch watchdog poisoned");
            continue;
        };
        if let Some(remaining) = active.deadline.checked_duration_since(Instant::now()) {
            state = shared
                .changed
                .wait_timeout(state, remaining)
                .expect("dispatch watchdog poisoned")
                .0;
        } else {
            let active = state.active.take().expect("checked active dispatch");
            let _ = active.socket.shutdown(Shutdown::Both);
            active.expired.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pending_connections_are_accepted_or_rejected_in_bounded_turns() {
        let mut host = WaylandHost::new(16, 16).unwrap();
        let mut peers = (0..MAX_CLIENTS * 2)
            .map(|_| UnixStream::connect(host.socket_path()).unwrap())
            .collect::<Vec<_>>();
        for peer in &peers {
            peer.set_nonblocking(true).unwrap();
        }
        host.dispatch().unwrap();
        assert_eq!(host.clients.len(), MAX_CLIENTS);
        for peer in &mut peers[MAX_CLIENTS..] {
            assert_eq!(
                peer.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock,
                "one dispatch drained the full connection backlog"
            );
        }
        host.dispatch().unwrap();
        for peer in &mut peers[MAX_CLIENTS..] {
            assert_eq!(
                peer.read(&mut [0]).unwrap(),
                0,
                "later turns did not retire excess connections"
            );
        }
    }
    use super::*;
    use std::io::{Read as _, Write as _};

    #[test]
    fn increasing_core_callback_ids_cannot_grow_an_unbounded_retired_id_table() {
        let mut host = WaylandHost::new(16, 16).unwrap();
        let mut socket = UnixStream::connect(host.socket_path()).unwrap();
        let bytes = (2..=MAX_PROTOCOL_OBJECT_SLOTS as u32 + 1)
            .flat_map(|id| {
                [1u32, 12u32 << 16, id]
                    .into_iter()
                    .flat_map(u32::to_ne_bytes)
            })
            .collect::<Vec<_>>();
        socket.write_all(&bytes).unwrap();
        host.dispatch().unwrap();
        assert!(
            host.clients[0]
                .client
                .get_data::<state::ClientState>()
                .unwrap()
                .closed
                .load(Ordering::Acquire),
            "sequential callback IDs escaped the retained protocol table budget"
        );
    }
    #[test]
    fn expiration_and_old_guards_cannot_close_or_disarm_another_connection() {
        let watchdog = Watchdog::with_timeout(Duration::from_millis(100)).unwrap();
        let (a, mut peer_a) = UnixStream::pair().unwrap();
        peer_a
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let old = watchdog.arm(Arc::new(a)).unwrap();
        assert_eq!(peer_a.read(&mut [0; 1]).unwrap(), 0);
        let (mut b, mut peer_b) = UnixStream::pair().unwrap();
        let new = watchdog.arm(Arc::new(b.try_clone().unwrap())).unwrap();
        assert!(old.finish());
        b.write_all(b"b").unwrap();
        peer_b.read_exact(&mut [0; 1]).unwrap();
        assert!(!new.finish());
        // Cancellation must remain effective after the old deadline passes.
        drop(watchdog);
        b.write_all(b"c").unwrap();
        peer_b.read_exact(&mut [0; 1]).unwrap();
    }
    #[test]
    fn stopping_the_controller_retires_its_thread_before_returning() {
        let watchdog = Watchdog::with_timeout(Duration::from_millis(25)).unwrap();
        let (mut server, mut peer) = UnixStream::pair().unwrap();
        let guard = watchdog.arm(Arc::new(server.try_clone().unwrap())).unwrap();
        drop(watchdog);
        thread::sleep(Duration::from_millis(50));
        server.write_all(b"live").unwrap();
        peer.read_exact(&mut [0; 4]).unwrap();
        assert!(!guard.finish());
    }
    #[test]
    fn host_generated_disconnects_clean_idle_protocol_objects_before_retiring_the_connection() {
        let mut host = WaylandHost::new(16, 16).unwrap();
        let mut socket = UnixStream::connect(host.socket_path()).unwrap();
        let bytes = [1u32, (12u32 << 16) | 1, 2u32]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect::<Vec<_>>();
        socket.write_all(&bytes).unwrap();
        host.dispatch().unwrap();
        let client = host.clients[0].client.id();
        let handle = host.display.handle().backend_handle();
        let mut objects = 0;
        handle
            .with_all_objects_for(client.clone(), |_| objects += 1)
            .unwrap();
        assert!(objects > 1);
        handle.kill_client(client.clone(), DisconnectReason::ConnectionClosed);
        host.dispatch().unwrap();
        assert!(host.clients.is_empty());
        assert!(handle.get_client_data(client).is_err());
    }
}
