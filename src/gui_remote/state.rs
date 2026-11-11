//! Private, owner-authenticated process state capture, separate from Vivid sessions.
//!
//! This channel carries bounded application-owned restoration data, never runtime
//! identities or Vivid authority. Hosts must give each process a fresh endpoint.

// Rust guideline compliant 2026-10-07
use crate::{
    gui::{self, Entity, Render},
    gui_host::{AppState, StateResponder, automation::Error},
    local_control::{LocalListener, LocalStream},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// Keep one native service's queue within the SDK's per-owner capture budget.
const CAPACITY: usize = 4;
const POLL: Duration = Duration::from_millis(25);
const BUDGET: Duration = Duration::from_secs(2);
// Native GUI/GPU construction precedes the ready handshake, as with first media.
const STARTUP: Duration = Duration::from_secs(15);
// The envelope adds only fixed metadata to the validated 256-KiB state object.
const MAX_FRAME: usize = crate::gui_host::MAX_CONFIG_BYTES + 1024;
const ENDPOINT_ENV: &str = "VIVID_GUI_STATE_ENDPOINT";
const OWNER_ENV: &str = "VIVID_GUI_STATE_OWNER";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    message: Message,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Hello {
        owner: [u8; 16],
    },
    Initial {
        owner: [u8; 16],
        state: Option<AppState>,
    },
    Ready {
        owner: [u8; 16],
    },
    Capture {
        owner: [u8; 16],
        id: u64,
    },
    Captured {
        owner: [u8; 16],
        id: u64,
        state: Option<AppState>,
    },
    Failed {
        owner: [u8; 16],
        id: u64,
    },
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid application state channel",
    )
}
fn remaining(end: Instant) -> io::Result<Duration> {
    let time = end.saturating_duration_since(Instant::now());
    if time.is_zero() {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(time)
    }
}
fn read_exact(stream: &mut LocalStream, mut bytes: &mut [u8], end: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(end)?;
        #[cfg(not(unix))]
        stream.set_read_timeout(Some(remaining(end)?))?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(POLL.min(remaining(end)?))
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn write_message(stream: &mut LocalStream, message: Message, end: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let body = serde_json::to_vec(&Envelope {
        version: 1,
        message,
    })
    .map_err(|_| invalid())?;
    if body.len() > MAX_FRAME {
        return Err(invalid());
    }
    let header = u32::try_from(body.len())
        .map_err(|_| invalid())?
        .to_be_bytes();
    for mut bytes in [&header[..], &body[..]] {
        while !bytes.is_empty() {
            remaining(end)?;
            #[cfg(not(unix))]
            stream.set_write_timeout(Some(remaining(end)?))?;
            match stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    thread::sleep(POLL.min(remaining(end)?))
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}
fn read_message(stream: &mut LocalStream, end: Instant) -> io::Result<Message> {
    // Unix reads are nonblocking under one absolute deadline. Reinstalling a
    // socket timeout after peer FIN can fail with EINVAL on macOS even when a
    // complete final reply remains buffered. Windows uses cancellable pipe I/O.
    stream.set_nonblocking(true)?;
    let mut header = [0; 4];
    read_exact(stream, &mut header, end)?;
    read_body(stream, header, end)
}
fn read_body(stream: &mut LocalStream, header: [u8; 4], end: Instant) -> io::Result<Message> {
    let len = usize::try_from(u32::from_be_bytes(header)).map_err(|_| invalid())?;
    if len == 0 || len > MAX_FRAME {
        return Err(invalid());
    }
    let mut body = vec![0; len];
    read_exact(stream, &mut body, end)?;
    let envelope: Envelope = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if envelope.version != 1 {
        return Err(invalid());
    }
    Ok(envelope.message)
}
fn read_idle(stream: &mut LocalStream) -> io::Result<Option<Message>> {
    stream.set_nonblocking(true)?;
    let mut header = [0; 4];
    #[cfg(not(unix))]
    stream.set_read_timeout(Some(POLL))?;
    match stream.read(&mut header[..1]) {
        Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
        Ok(_) => (),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            #[cfg(unix)]
            thread::sleep(POLL);
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let end = Instant::now() + BUDGET;
    read_exact(stream, &mut header[1..], end)?;
    read_body(stream, header, end).map(Some)
}

#[derive(Default)]
struct Control {
    stopped: AtomicBool,
    ready: AtomicBool,
    stream: Mutex<Option<LocalStream>>,
}
impl Control {
    fn install(&self, stream: &LocalStream) -> io::Result<()> {
        let mut slot = self.stream.lock().expect("state channel mutex poisoned");
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        *slot = Some(stream.try_clone()?);
        Ok(())
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        // A retained GUI proxy must not retain a retired native descriptor.
        if let Some(stream) = self
            .stream
            .lock()
            .expect("state channel mutex poisoned")
            .take()
        {
            let _ = stream.shutdown();
        }
    }
}

/// Queues bounded read-only captures for one independently running process.
///
/// Clones address the same immutable channel owner. A host must not reuse this
/// proxy for a replacement process. The service owns native I/O and deadlines.
#[derive(Clone)]
pub struct Host {
    owner: [u8; 16],
    endpoint: PathBuf,
    requests: mpsc::SyncSender<StateResponder>,
    control: Arc<Control>,
}
impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateHost").finish_non_exhaustive()
    }
}
impl Host {
    /// Reports initial-state delivery and producer schema acceptance.
    pub fn is_ready(&self) -> bool {
        self.control.ready.load(Ordering::Acquire)
    }

    /// Reports that this channel cannot admit further captures.
    pub fn is_stopped(&self) -> bool {
        self.control.stopped.load(Ordering::Acquire)
    }

    /// Binds a fresh owner-private endpoint and starts its bounded service.
    ///
    /// The caller retains `Service` on its native supervisor, and drops it only
    /// after terminating the associated child. On Unix use an owner-private
    /// temporary directory; on Windows use a fresh local named-pipe name.
    ///
    /// # Errors
    /// Returns an error for invalid/private-directory policy, entropy, bind or worker failure.
    pub fn bind(endpoint: &Path, initial: Option<AppState>) -> io::Result<(Self, Service)> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if !endpoint.is_absolute() {
                return Err(invalid());
            }
            let parent = endpoint.parent().ok_or_else(invalid)?;
            let metadata = std::fs::symlink_metadata(parent)?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(invalid());
            }
            if endpoint.try_exists()? {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
        }
        let mut owner = [0; 16];
        getrandom::fill(&mut owner).map_err(io::Error::other)?;
        let listener = LocalListener::bind(endpoint)?;
        let (requests, receive) = mpsc::sync_channel(CAPACITY);
        let control = Arc::new(Control::default());
        let state = control.clone();
        let worker = thread::Builder::new()
            .name("vivid-gui-state-host".into())
            .spawn(move || {
                let _ = host_loop(listener, owner, initial, receive, &state);
                state.stop();
            })?;
        let host = Self {
            owner,
            endpoint: endpoint.into(),
            requests,
            control: control.clone(),
        };
        Ok((
            host,
            Service {
                control,
                worker: Some(worker),
            },
        ))
    }
    /// Configures only the associated child, without changing the parent's environment.
    pub fn configure(&self, command: &mut Command) {
        let owner: String = self.owner.iter().map(|b| format!("{b:02x}")).collect();
        command
            .env(ENDPOINT_ENV, &self.endpoint)
            .env(OWNER_ENV, owner);
    }
    /// Queues one cancellation-aware state capture without blocking.
    pub fn capture(&self, response: StateResponder) {
        if self.control.stopped.load(Ordering::Acquire) {
            response.run(|| Err(Error::Unavailable));
        } else if let Err(error) = self.requests.try_send(response) {
            match error {
                mpsc::TrySendError::Full(response) => response.run(|| Err(Error::Busy)),
                mpsc::TrySendError::Disconnected(response) => {
                    response.run(|| Err(Error::Unavailable))
                }
            }
        }
    }
}

/// Owns the state service's I/O cancellation and worker retirement.
///
/// Drop on the native supervisor, never a GUI callback. Drop interrupts any
/// accepted stream and joins the worker; idle accept checks cancellation at 25 ms.
pub struct Service {
    control: Arc<Control>,
    worker: Option<JoinHandle<()>>,
}
impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateService").finish_non_exhaustive()
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn host_loop(
    listener: LocalListener,
    owner: [u8; 16],
    initial: Option<AppState>,
    requests: mpsc::Receiver<StateResponder>,
    control: &Control,
) -> io::Result<()> {
    let end = Instant::now() + STARTUP;
    let mut stream = loop {
        if control.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        match listener.accept_timeout(POLL.min(remaining(end)?)) {
            Ok(stream) => break stream,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::PermissionDenied
                ) => {}
            Err(e) => return Err(e),
        }
    };
    stream.set_nonblocking(true)?;
    control.install(&stream)?;
    if !matches!(read_message(&mut stream, end)?, Message::Hello { owner: got } if got == owner) {
        return Err(invalid());
    }
    write_message(
        &mut stream,
        Message::Initial {
            owner,
            state: initial,
        },
        end,
    )?;
    if !matches!(read_message(&mut stream, end)?, Message::Ready { owner: got } if got == owner) {
        return Err(invalid());
    }
    control.ready.store(true, Ordering::Release);
    let mut serial = 1u64;
    while !control.stopped.load(Ordering::Acquire) {
        let response = match requests.recv_timeout(POLL) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let mut failure = false;
        response.run(|| {
            let result = (|| {
                let end = Instant::now() + BUDGET;
                write_message(&mut stream, Message::Capture { owner, id: serial }, end)?;
                match read_message(&mut stream, end)? {
                    Message::Captured {
                        owner: got,
                        id,
                        state,
                    } if got == owner && id == serial => Ok(Ok(state)),
                    Message::Failed { owner: got, id } if got == owner && id == serial => {
                        Ok(Err(Error::Unavailable))
                    }
                    _ => Err(invalid()),
                }
            })();
            failure = result.is_err();
            result.unwrap_or(Err(Error::Unavailable))
        });
        if failure {
            return Err(invalid());
        }
        serial = serial.checked_add(1).ok_or_else(invalid)?;
    }
    Ok(())
}

pub(super) struct Connection {
    owner: [u8; 16],
    stream: LocalStream,
    pub initial: Option<AppState>,
}
impl Connection {
    pub(super) fn from_env() -> io::Result<Option<Self>> {
        let endpoint = std::env::var_os(ENDPOINT_ENV);
        let owner = std::env::var_os(OWNER_ENV);
        let (endpoint, owner) = match (endpoint, owner) {
            (None, None) => return Ok(None),
            (Some(endpoint), Some(owner)) => (
                PathBuf::from(endpoint),
                owner.into_string().map_err(|_| invalid())?,
            ),
            _ => return Err(invalid()),
        };
        if owner.len() != 32 {
            return Err(invalid());
        }
        let mut identity = [0; 16];
        for (out, part) in identity.iter_mut().zip(owner.as_bytes().as_chunks::<2>().0) {
            *out = u8::from_str_radix(std::str::from_utf8(part).map_err(|_| invalid())?, 16)
                .map_err(|_| invalid())?;
        }
        let mut stream = LocalStream::connect(&endpoint)?;
        stream.set_nonblocking(true)?;
        let end = Instant::now() + BUDGET;
        write_message(&mut stream, Message::Hello { owner: identity }, end)?;
        match read_message(&mut stream, end)? {
            Message::Initial { owner, state } if owner == identity => Ok(Some(Self {
                owner,
                stream,
                initial: state,
            })),
            _ => Err(invalid()),
        }
    }
    pub(super) fn attach<V: Render + 'static>(
        self,
        view: Entity<V>,
        capture: impl Fn(&V, &gui::App) -> io::Result<Option<AppState>> + 'static,
        cx: &mut gui::App,
    ) -> io::Result<Service> {
        let (send, receive) = async_channel::bounded::<Job>(CAPACITY);
        let control = Arc::new(Control::default());
        control.install(&self.stream)?;
        let state = control.clone();
        let worker = thread::Builder::new()
            .name("vivid-gui-state-app".into())
            .spawn(move || {
                let _ = app_loop(self, send, &state);
                state.stop();
            })?;
        let view = view.downgrade();
        cx.spawn(async move |cx| {
            while let Ok(job) = receive.recv().await {
                if job.live.load(Ordering::Acquire) && Instant::now() < job.deadline {
                    let result = view
                        .update(cx, |view, cx| capture(view, cx))
                        .unwrap_or_else(|_| Err(io::Error::other("application view unavailable")));
                    let _ = job.reply.try_send(result);
                }
            }
        })
        .detach();
        Ok(Service {
            control,
            worker: Some(worker),
        })
    }
}
struct Job {
    deadline: Instant,
    live: Arc<AtomicBool>,
    reply: mpsc::SyncSender<io::Result<Option<AppState>>>,
}
fn app_loop(
    mut connection: Connection,
    send: async_channel::Sender<Job>,
    control: &Control,
) -> io::Result<()> {
    let owner = connection.owner;
    write_message(
        &mut connection.stream,
        Message::Ready { owner },
        Instant::now() + BUDGET,
    )?;
    let mut serial = 1;
    while !control.stopped.load(Ordering::Acquire) {
        let Some(message) = read_idle(&mut connection.stream)? else {
            continue;
        };
        let Message::Capture { owner: got, id } = message else {
            return Err(invalid());
        };
        if owner != got || id != serial {
            return Err(invalid());
        }
        let end = Instant::now() + BUDGET;
        let (reply, result) = mpsc::sync_channel(1);
        let live = Arc::new(AtomicBool::new(true));
        let admitted = send
            .try_send(Job {
                deadline: end,
                live: live.clone(),
                reply,
            })
            .is_ok();
        let state = admitted
            .then(|| result.recv_timeout(BUDGET))
            .and_then(Result::ok)
            .and_then(Result::ok);
        live.store(false, Ordering::Release);
        let message = match state {
            Some(state) => Message::Captured { owner, id, state },
            None => Message::Failed { owner, id },
        };
        write_message(&mut connection.stream, message, end)?;
        serial = serial.checked_add(1).ok_or_else(invalid)?;
    }
    Ok(())
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests;
