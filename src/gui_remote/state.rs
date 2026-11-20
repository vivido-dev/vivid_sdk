//! Private process state capture and model calls scoped to one child.
//!
//! Hosts give each process a fresh endpoint. Capture and registered model calls
//! share bounded native I/O and a monotonic transaction sequence.

// Rust guideline compliant 2026-10-07
use crate::{
    gui::{self, Entity, Render},
    gui_host::{
        AppState, StateResponder,
        automation::{self, Data, Error, Method, Request, Responder},
    },
    local_control::{LocalListener, LocalStream},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
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
const METHODS_ENV: &str = "VIVID_GUI_APP_METHODS";
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
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "read_methods"
        )]
        methods: Vec<Method<String>>,
    },
    Offer {
        owner: [u8; 16],
        id: u64,
        name: String,
        data: Data,
    },
    Claim {
        owner: [u8; 16],
        id: u64,
    },
    Decision {
        owner: [u8; 16],
        id: u64,
        remaining_us: u64,
    },
    Result {
        owner: [u8; 16],
        id: u64,
        result: Result<Data, Error>,
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
// Enforce the registration count during decoding as well as the overall
// envelope's byte limit. The child cannot choose an allocation capacity.
fn read_methods<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<Method<String>>, D::Error> {
    struct Methods;
    impl<'de> serde::de::Visitor<'de> for Methods {
        type Value = Vec<Method<String>>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a bounded method list")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut methods = Vec::new();
            while let Some(method) = sequence.next_element()? {
                if methods.len() == automation::MAX_METHODS {
                    return Err(serde::de::Error::custom("too many app methods"));
                }
                methods.push(method);
            }
            automation::methods(&methods).map_err(serde::de::Error::custom)?;
            Ok(methods)
        }
    }
    decoder.deserialize_seq(Methods)
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid application state channel",
    )
}
use super::local::remaining;
use vivid_protocol::auth::verify_tag;
fn write_message(stream: &mut LocalStream, message: Message, end: Instant) -> io::Result<()> {
    super::local::send(
        stream,
        &Envelope {
            version: 1,
            message,
        },
        MAX_FRAME,
        end,
    )
}
fn read_message(stream: &mut LocalStream, end: Instant) -> io::Result<Message> {
    let envelope: Envelope = super::local::receive(stream, MAX_FRAME, end)?;
    if envelope.version != 1 {
        return Err(invalid());
    }
    Ok(envelope.message)
}
fn read_idle(stream: &mut LocalStream) -> io::Result<Option<Message>> {
    let envelope: Option<Envelope> = super::local::receive_idle(stream, MAX_FRAME, BUDGET)?;
    envelope
        .map(|envelope| {
            if envelope.version != 1 {
                return Err(invalid());
            }
            Ok(envelope.message)
        })
        .transpose()
}

#[derive(Default)]
struct Control {
    stopped: AtomicBool,
    ready: AtomicBool,
    methods: OnceLock<Vec<Method<String>>>,
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

enum HostRequest {
    Capture(StateResponder),
    Call(Request, Responder),
}

impl HostRequest {
    fn fail(self, error: Error) {
        match self {
            Self::Capture(reply) => reply.run(|| Err(error)),
            Self::Call(_, reply) => reply.run(|| Err(error)),
        }
    }
}

/// Queues bounded captures and registered calls for one independently running process.
///
/// Clones address the same immutable channel owner. A host must not reuse this
/// proxy for a replacement process. The service owns native I/O and deadlines.
#[derive(Clone)]
pub struct Host {
    owner: [u8; 16],
    endpoint: PathBuf,
    requests: mpsc::SyncSender<HostRequest>,
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
        super::local::validate_endpoint(endpoint)?;
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
        self.configure_methods(command, false);
    }
    /// Configures state discovery and enables optional method registration for one child.
    pub fn configure_methods(&self, command: &mut Command, enable_methods: bool) {
        if enable_methods {
            command.env(METHODS_ENV, "1");
        } else {
            command.env_remove(METHODS_ENV);
        }
        let owner: String = self.owner.iter().map(|b| format!("{b:02x}")).collect();
        command
            .env(ENDPOINT_ENV, &self.endpoint)
            .env(OWNER_ENV, owner);
    }
    /// Queues one cancellation-aware state capture without blocking.
    pub fn capture(&self, response: StateResponder) {
        self.enqueue(HostRequest::Capture(response));
    }

    /// Borrows the live producer's validated registration.
    pub fn methods(&self) -> &[Method<String>] {
        if self.is_stopped() {
            return &[];
        }
        self.control.methods.get().map_or(&[], Vec::as_slice)
    }

    /// Queues a call for this process after local method discovery.
    pub fn call(&self, request: Request, reply: Responder) {
        if self.methods().iter().any(|m| m.name == request.method()) {
            self.enqueue(HostRequest::Call(request, reply));
        } else {
            reply.run(|| Err(Error::Unsupported));
        }
    }

    fn enqueue(&self, job: HostRequest) {
        if self.is_stopped() {
            job.fail(Error::Unavailable);
        } else if let Err(error) = self.requests.try_send(job) {
            match error {
                mpsc::TrySendError::Full(job) => job.fail(Error::Busy),
                mpsc::TrySendError::Disconnected(job) => job.fail(Error::Unavailable),
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
    requests: mpsc::Receiver<HostRequest>,
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
    if !matches!(read_message(&mut stream, end)?, Message::Hello { owner: got } if verify_tag(&owner, &got))
    {
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
    let Message::Ready {
        owner: got,
        methods,
    } = read_message(&mut stream, end)?
    else {
        return Err(invalid());
    };
    if !verify_tag(&owner, &got) {
        return Err(invalid());
    }
    control.methods.set(methods).map_err(|_| invalid())?;
    control.ready.store(true, Ordering::Release);
    let mut serial = 1u64;
    while !control.stopped.load(Ordering::Acquire) {
        let response = match requests.recv_timeout(POLL) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        match response {
            HostRequest::Call(request, response) => {
                let Ok(deadline) = response.admission_deadline() else {
                    continue;
                };
                host_call(&mut stream, owner, serial, request, response, deadline)?
            }
            HostRequest::Capture(response) => {
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
                            } if verify_tag(&owner, &got) && id == serial => Ok(Ok(state)),
                            Message::Failed { owner: got, id }
                                if verify_tag(&owner, &got) && id == serial =>
                            {
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
            }
        }
        serial = serial.checked_add(1).ok_or_else(invalid)?;
    }
    Ok(())
}

fn host_call(
    stream: &mut LocalStream,
    owner: [u8; 16],
    id: u64,
    request: Request,
    response: Responder,
    deadline: Instant,
) -> io::Result<()> {
    let end = Instant::now() + BUDGET;
    write_message(
        stream,
        Message::Offer {
            owner,
            id,
            name: request.method().into(),
            data: request.data().clone(),
        },
        end,
    )?;
    if !matches!(read_message(stream, end)?, Message::Claim { owner: got, id: serial } if verify_tag(&owner, &got) && serial == id)
    {
        return Err(invalid());
    }
    // The producer's model loop has reached the offer. Recheck host focus,
    // lifetime and cancellation immediately before granting this one execution.
    let mut granted = false;
    let mut failure = None;
    response.run(|| {
        let remaining_us = deadline
            .saturating_duration_since(Instant::now())
            .as_micros() as u64;
        if remaining_us == 0 {
            return Err(Error::Expired);
        }
        granted = true;
        let result = write_message(
            stream,
            Message::Decision {
                owner,
                id,
                remaining_us,
            },
            end,
        )
        .and_then(|()| read_message(stream, end))
        .and_then(|message| match message {
            Message::Result {
                owner: got,
                id: serial,
                result,
            } if verify_tag(&owner, &got) && serial == id => Ok(result),
            _ => Err(invalid()),
        });
        result.unwrap_or_else(|error| {
            failure = Some(error);
            Err(Error::Unavailable)
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    if !granted {
        write_message(
            stream,
            Message::Decision {
                owner,
                id,
                remaining_us: 0,
            },
            end,
        )?;
    }
    Ok(())
}

pub(super) struct Connection {
    owner: [u8; 16],
    stream: LocalStream,
    pub initial: Option<AppState>,
    methods_enabled: bool,
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
            Message::Initial { owner, state } if verify_tag(&identity, &owner) => Ok(Some(Self {
                owner,
                stream,
                initial: state,
                methods_enabled: std::env::var_os(METHODS_ENV).as_deref()
                    == Some(std::ffi::OsStr::new("1")),
            })),
            _ => Err(invalid()),
        }
    }
    pub(super) fn attach<V: Render + 'static>(
        self,
        view: Entity<V>,
        capture: impl Fn(&V, &gui::App) -> io::Result<Option<AppState>> + 'static,
        methods: &'static [Method],
        call: impl Fn(&mut V, Request, &mut gui::Context<V>) -> Result<Data, Error> + 'static,
        cx: &mut gui::App,
    ) -> io::Result<Service> {
        let (service, receive) = self.start_io(methods)?;
        let view = view.downgrade();
        cx.spawn(async move |cx| {
            while let Ok(job) = receive.recv().await {
                job.service(
                    cx,
                    |cx| {
                        view.update(cx, |view, cx| capture(view, cx))
                            .unwrap_or_else(|_| {
                                Err(io::Error::other("application view unavailable"))
                            })
                    },
                    |cx, request| {
                        view.update(cx, |view, cx| call(view, request, cx))
                            .unwrap_or(Err(Error::Closed))
                    },
                );
            }
        })
        .detach();
        Ok(service)
    }

    fn start_io(
        self,
        methods: &'static [Method],
    ) -> io::Result<(Service, async_channel::Receiver<Job>)> {
        let methods = if self.methods_enabled { methods } else { &[] };
        let registrations = automation::methods(methods)
            .map_err(|_| invalid())?
            .into_iter()
            .map(|m| Method {
                name: m.name.to_owned(),
                access: m.access,
                description: m.description.to_owned(),
            })
            .collect();
        let (send, receive) = async_channel::bounded::<Job>(CAPACITY);
        let control = Arc::new(Control::default());
        control.install(&self.stream)?;
        let state = control.clone();
        let worker = thread::Builder::new()
            .name("vivid-gui-state-app".into())
            .spawn(move || {
                let _ = app_loop(self, send, &state, registrations);
                state.stop();
            })?;
        Ok((
            Service {
                control,
                worker: Some(worker),
            },
            receive,
        ))
    }
}
/// State channel serviced by a native producer's own event loop.
pub struct Producer {
    receive: async_channel::Receiver<Job>,
    _service: Service,
}

impl Producer {
    /// Restores the producer before acknowledging the host's state handshake.
    ///
    /// Returns `None` when launched without a state endpoint. The callback
    /// validates the application's schema and installs only durable model data.
    ///
    /// # Errors
    /// Returns an error if the connection, restoration or worker startup fails.
    pub fn from_env(
        restore: impl FnOnce(Option<AppState>) -> io::Result<()>,
    ) -> io::Result<Option<Self>> {
        Self::with_methods(restore, &[])
    }

    /// Restores the model and registers methods for this process lifetime.
    ///
    /// # Errors
    /// Returns connection, restoration or method registration errors.
    pub fn with_methods(
        restore: impl FnOnce(Option<AppState>) -> io::Result<()>,
        methods: &'static [Method],
    ) -> io::Result<Option<Self>> {
        let Some(mut connection) = Connection::from_env()? else {
            return Ok(None);
        };
        restore(connection.initial.take())?;
        let (service, receive) = connection.start_io(methods)?;
        Ok(Some(Self {
            receive,
            _service: service,
        }))
    }

    /// Services at most one live capture on the producer's model thread.
    ///
    /// Call between event-loop turns. The callback must take a bounded,
    /// nonblocking snapshot. The channel worker performs serialization and I/O.
    pub fn poll(&self, capture: impl FnOnce() -> io::Result<Option<AppState>>) {
        self.poll_with_methods(capture, |_| Err(Error::Unsupported));
    }

    /// Services one pending capture, admission request or admitted application call.
    pub fn poll_with_methods(
        &self,
        capture: impl FnOnce() -> io::Result<Option<AppState>>,
        call: impl FnOnce(Request) -> Result<Data, Error>,
    ) {
        if let Ok(job) = self.receive.try_recv() {
            job.service(&mut (), |_| capture(), |_, request| call(request));
        }
    }
}

struct Job {
    deadline: Instant,
    live: Arc<AtomicBool>,
    work: Work,
}
enum Work {
    Capture(mpsc::SyncSender<io::Result<Option<AppState>>>),
    Claim(mpsc::SyncSender<()>),
    Call(Request, mpsc::SyncSender<Result<Data, Error>>),
}
impl Job {
    fn service<C>(
        self,
        cx: &mut C,
        capture: impl FnOnce(&mut C) -> io::Result<Option<AppState>>,
        call: impl FnOnce(&mut C, Request) -> Result<Data, Error>,
    ) {
        if !self.live.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return;
        }
        match self.work {
            Work::Capture(reply) => {
                let _ = reply.try_send(capture(cx));
            }
            Work::Claim(reply) => {
                let _ = reply.try_send(());
            }
            Work::Call(request, reply) => {
                let _ = reply.try_send(call(cx, request));
            }
        }
    }
}

fn app_loop(
    mut connection: Connection,
    send: async_channel::Sender<Job>,
    control: &Control,
    methods: Vec<Method<String>>,
) -> io::Result<()> {
    let owner = connection.owner;
    let registered: Vec<_> = methods.iter().map(|m| m.name.clone()).collect();
    write_message(
        &mut connection.stream,
        Message::Ready { owner, methods },
        Instant::now() + BUDGET,
    )?;
    let mut serial = 1;
    while !control.stopped.load(Ordering::Acquire) {
        let Some(message) = read_idle(&mut connection.stream)? else {
            continue;
        };
        let end = Instant::now() + BUDGET;
        let live = Arc::new(AtomicBool::new(true));
        let submit = |work, deadline| {
            send.try_send(Job {
                deadline,
                live: live.clone(),
                work,
            })
            .map_err(|_| invalid())
        };
        match message {
            Message::Capture { owner: got, id } if verify_tag(&owner, &got) && id == serial => {
                let (reply, result) = mpsc::sync_channel(1);
                submit(Work::Capture(reply), end)?;
                let message = match result.recv_timeout(remaining(end)?) {
                    Ok(Ok(state)) => Message::Captured { owner, id, state },
                    _ => Message::Failed { owner, id },
                };
                live.store(false, Ordering::Release);
                write_message(&mut connection.stream, message, end)?;
            }
            Message::Offer {
                owner: got,
                id,
                name,
                data,
            } if verify_tag(&owner, &got) && id == serial => {
                if !registered.contains(&name) {
                    return Err(invalid());
                }
                let request = Request::new(name, data).map_err(|_| invalid())?;
                let (ready, received) = mpsc::sync_channel(1);
                submit(Work::Claim(ready), end)?;
                received
                    .recv_timeout(remaining(end)?)
                    .map_err(|_| invalid())?;
                let claimed_at = Instant::now();
                write_message(&mut connection.stream, Message::Claim { owner, id }, end)?;
                let Message::Decision {
                    owner: got,
                    id: serial,
                    remaining_us,
                } = read_message(&mut connection.stream, end)?
                else {
                    return Err(invalid());
                };
                if !verify_tag(&owner, &got)
                    || serial != id
                    || u128::from(remaining_us) > automation::CALL_TIMEOUT.as_micros()
                {
                    return Err(invalid());
                }
                if remaining_us > 0 {
                    // Count from before sending the claim. Wire latency cannot
                    // extend a host deadline on another process's local clock.
                    let execution_end = end.min(claimed_at + Duration::from_micros(remaining_us));
                    let (reply, result) = mpsc::sync_channel(1);
                    submit(Work::Call(request, reply), execution_end)?;
                    let result = result
                        .recv_timeout(execution_end.saturating_duration_since(Instant::now()))
                        .unwrap_or(Err(Error::Expired));
                    live.store(false, Ordering::Release);
                    write_message(
                        &mut connection.stream,
                        Message::Result { owner, id, result },
                        end,
                    )?;
                }
                live.store(false, Ordering::Release);
            }
            _ => return Err(invalid()),
        }
        serial = serial.checked_add(1).ok_or_else(invalid)?;
    }
    Ok(())
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests;
