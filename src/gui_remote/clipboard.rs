//! Private local-process clipboard bridge with presentation-qualified owner scopes.

// Rust guideline compliant 2026-10-07
use super::{local, *};
use crate::{
    gui::{App, ClipboardItem, ClipboardReadError, HostedClipboard, Task},
    gui_host::{ResourceBudget, ResourcePermit, clipboard as bridge},
    local_control::{LocalListener, LocalStream},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Weak, mpsc},
    thread::{self, JoinHandle},
};

const ENDPOINT: &str = "VIVID_GUI_CLIPBOARD_ENDPOINT";
const OWNER: &str = "VIVID_GUI_CLIPBOARD_OWNER";
const CAPACITY: usize = bridge::MAX_PENDING_REQUESTS;
const POLL: Duration = Duration::from_millis(10);
const BUDGET: Duration = Duration::from_secs(1);
const STARTUP: Duration = Duration::from_secs(15);
// JSON can encode each UTF-8 byte as a six-byte escape. Decoded strings have
// the existing 1-MiB clipboard ceiling; framing has a separate allocation bound.
const MAX_FRAME: usize = bridge::MAX_TEXT_BYTES * 6 + 1024;
type Binding = [u64; 5];
fn binding(tag: crate::InputTuple) -> Binding {
    [
        tag.producer_epoch.get(),
        tag.grant_generation.get(),
        tag.context_id,
        tag.surface_id,
        tag.surface_generation.get(),
    ]
}
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid GUI clipboard channel")
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Hello {
        version: u32,
        owner: [u8; 16],
    },
    Ready {
        version: u32,
    },
    Read {
        id: u64,
        binding: Binding,
    },
    Write {
        id: u64,
        binding: Binding,
        text: String,
    },
    Text {
        id: u64,
        text: String,
    },
    Queued {
        id: u64,
    },
    Refused {
        id: u64,
    },
}

struct Bound {
    binding: Option<Binding>,
    host: bridge::Host,
}
struct State {
    bound: Mutex<Bound>,
    stopped: AtomicBool,
    stream: Mutex<Option<LocalStream>>,
}
impl State {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.bound
            .lock()
            .expect("clipboard owner mutex")
            .host
            .close();
        if let Some(stream) = self.stream.lock().expect("clipboard stream mutex").take() {
            let _ = stream.shutdown();
        }
    }
    fn install(&self, stream: &LocalStream) -> io::Result<()> {
        let mut slot = self.stream.lock().expect("clipboard stream mutex");
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        *slot = Some(stream.try_clone()?);
        Ok(())
    }
}

/// Controls the text clipboard of one independently hosted GUI process.
///
/// Supply a fresh private endpoint to each child. GUI callbacks install only
/// its exact current input binding and service `next_request` using the existing
/// clipboard lease API. None, focus loss and replacement revoke queued work.
#[derive(Clone)]
pub struct Host {
    state: Arc<State>,
    endpoint: PathBuf,
    owner: zeroize::Zeroizing<[u8; 16]>,
}
impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteClipboardHost")
            .finish_non_exhaustive()
    }
}
impl Host {
    /// Binds an owner-private endpoint and starts a bounded native worker.
    ///
    /// Retain the returned service on the process supervisor, outside GUI code.
    /// On Unix the parent directory must be private and the endpoint absent.
    ///
    /// # Errors
    /// Returns invalid-path, entropy, native transport or worker errors.
    pub fn bind(endpoint: &Path) -> io::Result<(Self, Service)> {
        local::validate_endpoint(endpoint)?;
        let mut owner = zeroize::Zeroizing::new([0; 16]);
        getrandom::fill(owner.as_mut()).map_err(io::Error::other)?;
        let listener = LocalListener::bind(endpoint)?;
        let (host, client) = bridge::Host::new(false);
        let state = Arc::new(State {
            bound: Mutex::new(Bound {
                binding: None,
                host,
            }),
            stopped: AtomicBool::new(false),
            stream: Mutex::new(None),
        });
        let shared = state.clone();
        let secret = owner.clone();
        let worker = thread::Builder::new()
            .name("vivid-gui-clipboard-host".into())
            .spawn(move || {
                let _ = serve(listener, &secret, &shared, &client);
                shared.stop();
            })?;
        Ok((
            Self {
                state: state.clone(),
                endpoint: endpoint.into(),
                owner,
            },
            Service {
                state,
                worker: Some(worker),
            },
        ))
    }

    /// Supplies private discovery only to the associated child.
    pub fn configure(&self, command: &mut Command) {
        let secret = zeroize::Zeroizing::new(
            self.owner
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        );
        command
            .env(ENDPOINT, &self.endpoint)
            .env(OWNER, secret.as_str());
    }

    /// Installs an actually presented owner's binding, or revokes clipboard work.
    pub fn set_binding(&self, tag: Option<crate::InputTuple>) {
        let mut bound = self.state.bound.lock().expect("clipboard owner mutex");
        let tag = tag.map(binding);
        if bound.binding != tag {
            bound.host.set_active(false);
            bound.binding = tag;
            bound
                .host
                .set_active(tag.is_some() && !self.state.stopped.load(Ordering::Acquire));
        }
    }

    /// Takes one bounded platform request without waiting on native I/O.
    pub fn next_request(&self) -> Option<bridge::Request> {
        self.state
            .bound
            .lock()
            .expect("clipboard owner mutex")
            .host
            .next_request()
    }

    /// Permanently revokes this process's requests without joining its worker.
    pub fn close(&self) {
        self.state.stop();
    }
}

/// Cancels and joins the clipboard worker on its native process supervisor.
pub struct Service {
    state: Arc<State>,
    worker: Option<JoinHandle<()>>,
}
impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteClipboardService")
            .finish_non_exhaustive()
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.state.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(
    listener: LocalListener,
    owner: &[u8; 16],
    state: &State,
    client: &bridge::Client,
) -> io::Result<()> {
    let end = Instant::now() + STARTUP;
    let mut stream = loop {
        if state.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        match listener.accept_timeout(POLL.min(local::remaining(end)?)) {
            Ok(stream) => break stream,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::PermissionDenied
                ) => {}
            Err(e) => return Err(e),
        }
    };
    state.install(&stream)?;
    let hello: Message = local::receive(&mut stream, 1024, end)?;
    if !matches!(hello, Message::Hello { version: 1, owner: got } if vivid_protocol::auth::verify_tag(owner, &got))
    {
        return Err(invalid());
    }
    local::send(&mut stream, &Message::Ready { version: 1 }, 1024, end)?;
    let mut expected = 1u64;
    while !state.stopped.load(Ordering::Acquire) {
        let Some(message) = local::receive_idle::<Message>(&mut stream, MAX_FRAME, BUDGET)? else {
            continue;
        };
        let (id, tag, text) = match message {
            Message::Read { id, binding } => (id, binding, None),
            Message::Write { id, binding, text } if text.len() <= bridge::MAX_TEXT_BYTES => {
                (id, binding, Some(text))
            }
            _ => return Err(invalid()),
        };
        if id != expected {
            return Err(invalid());
        }
        expected = expected.checked_add(1).ok_or_else(invalid)?;
        // Bind admission to this exact tuple while serialized with revocation.
        // Releasing the lock before enqueue could let an old request borrow a
        // newer owner's clipboard generation after a rapid focus round trip.
        let admitted = {
            let bound = state.bound.lock().expect("clipboard owner mutex");
            if bound.binding != Some(tag) || state.stopped.load(Ordering::Acquire) {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else if let Some(text) = text {
                client.write(bridge::Target::Clipboard, text).map(|()| None)
            } else {
                client.begin_read(bridge::Target::Clipboard).map(Some)
            }
        };
        let response = match admitted {
            Ok(None) => Message::Queued { id },
            Ok(Some(mut pending)) => loop {
                if let Some(result) = pending.poll() {
                    break match result {
                        Ok(text) => Message::Text { id, text },
                        Err(_) => Message::Refused { id },
                    };
                }
                if state.stopped.load(Ordering::Acquire) {
                    return Ok(());
                }
                thread::sleep(POLL);
            },
            Err(_) => Message::Refused { id },
        };
        local::send(&mut stream, &response, MAX_FRAME, Instant::now() + BUDGET)?;
    }
    Ok(())
}

/// Producer-side mirror of its existing authenticated input watchdog.
#[derive(Default)]
pub(super) struct Gate(Mutex<Option<Arc<Grant>>>);
struct Grant {
    tag: Binding,
    deadline: Mutex<Instant>,
    lifecycle: Weak<crate::SessionLifecycle>,
    revoked: AtomicBool,
}
impl Grant {
    fn live(&self) -> bool {
        !self.revoked.load(Ordering::Acquire)
            && Instant::now() < *self.deadline.lock().expect("clipboard deadline mutex")
            && self
                .lifecycle
                .upgrade()
                .is_some_and(|s| !s.closed.load(Ordering::Acquire))
    }
}
impl Gate {
    pub(super) fn revoke(&self) {
        if let Some(old) = self.0.lock().expect("clipboard grant mutex").take() {
            old.revoked.store(true, Ordering::Release);
        }
    }
    pub(super) fn refresh(&self, input: &Injection) {
        let Some(tag) = input.guard.current_tag() else {
            self.revoke();
            return;
        };
        let Some(_grant) = input
            .guard
            .grant()
            .filter(|g| g.effective_classes & crate::INPUT_CLASS_KEYBOARD != 0)
        else {
            self.revoke();
            return;
        };
        let Some(lifecycle) = input.lifecycle.as_ref() else {
            self.revoke();
            return;
        };
        let Some(deadline) = input.guard.watchdog_deadline().and_then(|deadline| {
            input
                .origin
                .checked_add(Duration::from_micros(deadline.as_micros()))
        }) else {
            self.revoke();
            return;
        };
        let tag = binding(tag);
        let mut slot = self.0.lock().expect("clipboard grant mutex");
        if let Some(current) = slot
            .as_ref()
            .filter(|g| g.tag == tag && g.lifecycle.ptr_eq(&Arc::downgrade(lifecycle)))
        {
            *current.deadline.lock().expect("clipboard deadline mutex") = deadline;
        } else {
            if let Some(old) = slot.take() {
                old.revoked.store(true, Ordering::Release);
            }
            *slot = Some(Arc::new(Grant {
                tag,
                deadline: Mutex::new(deadline),
                lifecycle: Arc::downgrade(lifecycle),
                revoked: AtomicBool::new(false),
            }));
        }
    }
    fn capture(&self) -> Option<Arc<Grant>> {
        self.0
            .lock()
            .expect("clipboard grant mutex")
            .as_ref()
            .filter(|g| g.live())
            .cloned()
    }
}

struct Job {
    grant: Arc<Grant>,
    text: Option<String>,
    end: Instant,
    reply: async_channel::Sender<io::Result<Option<String>>>,
    _permit: Arc<ResourcePermit>,
}
struct Pending {
    receiver: async_channel::Receiver<io::Result<Option<String>>>,
    grant: Arc<Grant>,
    permit: Arc<ResourcePermit>,
}
pub(super) struct Producer {
    sender: Option<mpsc::SyncSender<Job>>,
    gate: Arc<Gate>,
    budget: ResourceBudget,
}
impl fmt::Debug for Producer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteClipboard").finish_non_exhaustive()
    }
}
pub(super) struct ProducerService {
    control: Arc<Mutex<Option<LocalStream>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for ProducerService {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(stream) = self.control.lock().expect("clipboard cancel mutex").take() {
            let _ = stream.shutdown();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Producer {
    pub(super) fn from_env(gate: Arc<Gate>) -> io::Result<(Self, Option<ProducerService>)> {
        let endpoint = std::env::var_os(ENDPOINT);
        let owner = std::env::var_os(OWNER);
        let (endpoint, owner) = match (endpoint, owner) {
            (None, None) => {
                return Ok((
                    Self {
                        sender: None,
                        gate,
                        budget: ResourceBudget::new(CAPACITY),
                    },
                    None,
                ));
            }
            (Some(endpoint), Some(owner)) => (
                PathBuf::from(endpoint),
                zeroize::Zeroizing::new(owner.into_string().map_err(|_| invalid())?),
            ),
            _ => return Err(invalid()),
        };
        if owner.len() != 32 {
            return Err(invalid());
        }
        let mut secret = zeroize::Zeroizing::new([0; 16]);
        for (out, hex) in secret.iter_mut().zip(owner.as_bytes().as_chunks::<2>().0) {
            *out = u8::from_str_radix(std::str::from_utf8(hex).map_err(|_| invalid())?, 16)
                .map_err(|_| invalid())?;
        }
        let (sender, receive) = mpsc::sync_channel(CAPACITY);
        let control = Arc::new(Mutex::new(None));
        let cancel = control.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::Builder::new()
            .name("vivid-gui-clipboard-app".into())
            .spawn(move || {
                let result = (|| -> io::Result<()> {
                    let mut stream = LocalStream::connect(&endpoint)?;
                    *cancel.lock().expect("clipboard cancel mutex") = Some(stream.try_clone()?);
                    if stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let end = Instant::now() + BUDGET;
                    local::send(
                        &mut stream,
                        &Message::Hello {
                            version: 1,
                            owner: *secret,
                        },
                        1024,
                        end,
                    )?;
                    if !matches!(
                        local::receive::<Message>(&mut stream, 1024, end)?,
                        Message::Ready { version: 1 }
                    ) {
                        return Err(invalid());
                    }
                    let mut id = 1u64;
                    while !stop.load(Ordering::Acquire) {
                        let job: Job = match receive.recv_timeout(POLL) {
                            Ok(job) => job,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        };
                        if !job.grant.live() || Instant::now() >= job.end {
                            continue;
                        }
                        let request = match job.text {
                            Some(text) => Message::Write {
                                id,
                                binding: job.grant.tag,
                                text,
                            },
                            None => Message::Read {
                                id,
                                binding: job.grant.tag,
                            },
                        };
                        local::send(&mut stream, &request, MAX_FRAME, job.end)?;
                        let result =
                            match local::receive::<Message>(&mut stream, MAX_FRAME, job.end)? {
                                Message::Text { id: got, text }
                                    if got == id && text.len() <= bridge::MAX_TEXT_BYTES =>
                                {
                                    Ok(Some(text))
                                }
                                Message::Queued { id: got } if got == id => Ok(None),
                                Message::Refused { id: got } if got == id => {
                                    Err(io::ErrorKind::PermissionDenied.into())
                                }
                                _ => return Err(invalid()),
                            };
                        if job.grant.live() && Instant::now() < job.end {
                            let _ = job.reply.try_send(result);
                        }
                        id = id.checked_add(1).ok_or_else(invalid)?;
                    }
                    Ok(())
                })();
                let _ = result;
                stop.store(true, Ordering::Release);
                cancel.lock().expect("clipboard cancel mutex").take();
            })?;
        Ok((
            Self {
                sender: Some(sender),
                gate,
                budget: ResourceBudget::new(CAPACITY),
            },
            Some(ProducerService {
                control,
                stopped,
                worker: Some(worker),
            }),
        ))
    }
    fn begin(&self, text: Option<String>) -> io::Result<Pending> {
        let grant = self.gate.capture().ok_or(io::ErrorKind::PermissionDenied)?;
        let permit = Arc::new(self.budget.try_acquire()?);
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .try_send(Job {
                grant: grant.clone(),
                text,
                end: Instant::now() + BUDGET,
                reply,
                _permit: permit.clone(),
            })
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        Ok(Pending {
            receiver: receive,
            grant,
            permit,
        })
    }
}
impl HostedClipboard for Producer {
    fn read(&self, cx: &App) -> Task<Result<Option<ClipboardItem>, ClipboardReadError>> {
        let Ok(pending) = self.begin(None) else {
            return Task::ready(Err(ClipboardReadError::Unavailable));
        };
        cx.foreground_executor().spawn(async move {
            let _permit = pending.permit;
            let result = pending
                .receiver
                .recv()
                .await
                .map_err(|_| ClipboardReadError::Unavailable)?;
            if !pending.grant.live() {
                return Err(ClipboardReadError::Unavailable);
            }
            result
                .map(|text| text.map(ClipboardItem::new_string))
                .map_err(|_| ClipboardReadError::Unavailable)
        })
    }
    fn write(&self, item: ClipboardItem) {
        if let Ok(Some(text)) = gui::clipboard_text(Some(item), bridge::MAX_TEXT_BYTES) {
            let _ = self.begin(Some(text));
        }
    }
}

#[cfg(all(test, unix, feature = "gui-testing"))]
mod tests;
