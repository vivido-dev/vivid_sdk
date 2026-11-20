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

mod picker;
pub use picker::PathPanels;

const ENDPOINT: &str = "VIVID_GUI_CLIPBOARD_ENDPOINT";
const OWNER: &str = "VIVID_GUI_CLIPBOARD_OWNER";
const FILES: &str = "VIVID_GUI_CLIPBOARD_FILES";
const IMAGE_COPY: &str = "VIVID_GUI_CLIPBOARD_IMAGE_COPY";
const MEDIA: &str = "VIVID_GUI_CLIPBOARD_MEDIA";
const WEB_URLS: &str = "VIVID_GUI_WEB_URLS";
const MAX_WEB_URL: usize = 8192;
const OFFERS: &str = "VIVID_GUI_CLIPBOARD_FILE_OFFERS";
const OFFER_INTERVAL: Duration = Duration::from_millis(200);
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
    Picker {
        id: u64,
        request: picker::Request,
    },
    PickerReply {
        id: u64,
        answer: picker::Answer,
    },
    OpenWebUrl {
        id: u64,
        binding: Binding,
        url: String,
    },
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
    WriteBitmap {
        id: u64,
        binding: Binding,
        bytes: u32,
        format: BitmapFormat,
    },
    BitmapBody {
        id: u64,
        active: bool,
    },
    SendBitmap {
        id: u64,
    },
    ReadMedia {
        id: u64,
        binding: Binding,
    },
    Png {
        id: u64,
        bytes: u32,
    },
    Empty {
        id: u64,
    },
    ReadFiles {
        id: u64,
        binding: Binding,
    },
    ProbeFiles {
        id: u64,
        binding: Binding,
    },
    FileOffer {
        id: u64,
        available: bool,
    },
    WriteFiles {
        id: u64,
        binding: Binding,
        uris: String,
    },
    Files {
        id: u64,
        uris: String,
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

fn web_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_WEB_URL
        || raw.contains(|c: char| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let (scheme, _) = raw.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")) {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    (url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.as_str().len() <= MAX_WEB_URL)
        .then(|| url.into())
}

struct Bound {
    picker: Option<picker::Panel>,
    pickers_enabled: bool,
    web_gesture: Option<Instant>,
    web_request: Option<(Instant, String)>,
    binding: Option<Arc<Binding>>,
    host: bridge::Host,
    file_offer: Option<(Instant, bool)>,
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
    file_lists: bool,
    media: bool,
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
        Self::bind_with_formats(endpoint, false)
    }

    /// Binds a private clipboard endpoint with optional bounded file-list support.
    ///
    /// # Errors
    /// Returns endpoint validation, entropy, transport or worker creation errors.
    pub fn bind_with_formats(endpoint: &Path, file_lists: bool) -> io::Result<(Self, Service)> {
        Self::bind_service(endpoint, file_lists, false)
    }

    /// Binds an owner-private endpoint supporting bounded explicit file and bitmap paste.
    ///
    /// # Errors
    /// Returns endpoint validation, entropy, transport or worker creation errors.
    pub fn bind_with_media(endpoint: &Path) -> io::Result<(Self, Service)> {
        Self::bind_service(endpoint, true, true)
    }
    fn bind_service(endpoint: &Path, file_lists: bool, media: bool) -> io::Result<(Self, Service)> {
        local::validate_endpoint(endpoint)?;
        let mut owner = zeroize::Zeroizing::new([0; 16]);
        getrandom::fill(owner.as_mut()).map_err(io::Error::other)?;
        let listener = LocalListener::bind(endpoint)?;
        let (host, client) = if media {
            bridge::Host::with_media(false)
        } else if file_lists {
            bridge::Host::with_file_lists(false)
        } else {
            bridge::Host::new(false)
        };
        let state = Arc::new(State {
            bound: Mutex::new(Bound {
                picker: None,
                pickers_enabled: false,
                binding: None,
                web_gesture: None,
                web_request: None,
                file_offer: None,
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
                let _ = serve(listener, &secret, &shared, &client, media);
                shared.stop();
            })?;
        Ok((
            Self {
                state: state.clone(),
                endpoint: endpoint.into(),
                file_lists,
                media,
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
            .env_remove(WEB_URLS)
            .env_remove(picker::ENV)
            .env(ENDPOINT, &self.endpoint)
            .env(OWNER, secret.as_str())
            .env(FILES, if self.file_lists { "1" } else { "0" })
            .env(IMAGE_COPY, if self.media { "1" } else { "0" })
            .env(MEDIA, if self.media { "1" } else { "0" })
            .env(OFFERS, if self.file_lists { "1" } else { "0" });
    }

    /// Configures a child whose host services gesture-scoped HTTP(S) URLs.
    pub fn configure_with_web_urls(&self, command: &mut Command) {
        self.configure(command);
        command.env(WEB_URLS, "1");
    }

    /// Allows one web URL after the presenter accepts the owner's input gesture.
    /// The delivery callback runs under the binding lock and must not reenter this host.
    ///
    /// # Errors
    /// Propagates errors from the input delivery callback.
    pub fn deliver_web_gesture(
        &self,
        tag: crate::InputTuple,
        deliver: impl FnOnce() -> io::Result<bool>,
    ) -> io::Result<bool> {
        let mut bound = self.state.bound.lock().expect("clipboard owner mutex");
        let delivered = deliver()?;
        if delivered && bound.binding.as_deref() == Some(&binding(tag)) {
            bound.web_gesture = Some(Instant::now() + BUDGET);
        }
        Ok(delivered)
    }

    /// Dispatches the owner's pending URL through the presenter's GUI platform.
    pub fn dispatch_web_url(&self, current: Option<crate::InputTuple>, open: impl FnOnce(&str)) {
        let mut bound = self.state.bound.lock().expect("clipboard owner mutex");
        if let Some((deadline, url)) = bound.web_request.take()
            && current.is_some_and(|tag| bound.binding.as_deref() == Some(&binding(tag)))
            && Instant::now() < deadline
            && !self.state.stopped.load(Ordering::Acquire)
        {
            // Claim under the binding lock. Native browser activation can reenter
            // focus handling and revoke the binding on this same GUI thread.
            drop(bound);
            open(&url);
        }
    }

    /// Installs an actually presented owner's binding, or revokes clipboard work.
    pub fn set_binding(&self, tag: Option<crate::InputTuple>) {
        let mut bound = self.state.bound.lock().expect("clipboard owner mutex");
        let tag = tag.map(binding);
        if bound.binding.as_deref() != tag.as_ref() {
            bound.host.set_active(false);
            bound.binding = tag.map(Arc::new);
            bound.web_gesture = None;
            bound.web_request = None;
            bound.file_offer = None;
            bound
                .host
                .set_active(tag.is_some() && !self.state.stopped.load(Ordering::Acquire));
        }
    }

    /// Samples native offer metadata at most five times a second for the current owner.
    pub fn refresh_file_offer(&self, sample: impl FnOnce() -> bool) {
        if !self.file_lists {
            return;
        }
        let tag = {
            let bound = self.state.bound.lock().expect("clipboard owner mutex");
            let Some(tag) = &bound.binding else { return };
            if bound
                .file_offer
                .is_some_and(|(when, _)| when.elapsed() < OFFER_INTERVAL)
            {
                return;
            }
            tag.clone()
        };
        let available = sample();
        let mut bound = self.state.bound.lock().expect("clipboard owner mutex");
        if bound
            .binding
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &tag))
        {
            bound.file_offer = Some((Instant::now(), available));
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum WireFormat {
    Media,
    Text,
    Files,
}

enum Operation {
    Read(WireFormat),
    WriteText(String),
    WriteFiles(io::Result<bridge::FileList>),
}

enum HostRead {
    Media(bridge::PendingRead<Option<gui::ClipboardPasteMedia>>),
    Text(bridge::PendingRead),
    Files(bridge::PendingFileRead),
}

fn serve(
    listener: LocalListener,
    owner: &[u8; 16],
    state: &State,
    client: &bridge::Client,
    media: bool,
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
    let version = match hello {
        Message::Hello {
            version,
            owner: got,
        } if (version == 1
            || (matches!(version, 2 | 3) && client.has_file_lists())
            || (matches!(version, 4 | 5) && media))
            && vivid_protocol::auth::verify_tag(owner, &got) =>
        {
            version
        }
        _ => return Err(invalid()),
    };
    local::send(&mut stream, &Message::Ready { version }, 1024, end)?;
    let mut expected = 1u64;
    while !state.stopped.load(Ordering::Acquire) {
        let Some(message) = local::receive_idle::<Message>(&mut stream, MAX_FRAME, BUDGET)? else {
            continue;
        };
        if let Message::Picker { id, request } = message {
            if id != expected {
                return Err(invalid());
            }
            expected = expected.checked_add(1).ok_or_else(invalid)?;
            let answer = picker::handle(
                &mut state.bound.lock().expect("clipboard owner mutex"),
                id,
                request,
            );
            local::send(
                &mut stream,
                &Message::PickerReply { id, answer },
                MAX_FRAME,
                Instant::now() + BUDGET,
            )?;
            continue;
        }
        if let Message::WriteBitmap {
            id,
            binding,
            bytes,
            format,
        } = message
        {
            if version < 5 || id != expected {
                return Err(invalid());
            }
            expected = expected.checked_add(1).ok_or_else(invalid)?;
            receive_bitmap_write(&mut stream, state, client, id, binding, bytes, format)?;
            continue;
        }
        if let Message::OpenWebUrl { id, binding, url } = message {
            if id != expected {
                return Err(invalid());
            }
            expected = expected.checked_add(1).ok_or_else(invalid)?;
            let url = web_url(&url);
            let accepted = {
                let mut bound = state.bound.lock().expect("clipboard owner mutex");
                if let Some(url) = url
                    && bound.binding.as_deref() == Some(&binding)
                    && let Some(deadline) = bound.web_gesture.take()
                    && Instant::now() < deadline
                    && bound.web_request.is_none()
                {
                    bound.web_request = Some((deadline, url));
                    true
                } else {
                    false
                }
            };
            let reply = if accepted {
                Message::Queued { id }
            } else {
                Message::Refused { id }
            };
            local::send(&mut stream, &reply, 1024, Instant::now() + BUDGET)?;
            continue;
        }
        if let Message::ProbeFiles { id, binding } = message {
            if version < 3 || id != expected {
                return Err(invalid());
            }
            expected = expected.checked_add(1).ok_or_else(invalid)?;
            let available = {
                let bound = state.bound.lock().expect("clipboard owner mutex");
                bound.binding.as_deref() == Some(&binding)
                    && !state.stopped.load(Ordering::Acquire)
                    && bound
                        .file_offer
                        .is_some_and(|(when, available)| available && when.elapsed() < BUDGET)
            };
            local::send(
                &mut stream,
                &Message::FileOffer { id, available },
                1024,
                Instant::now() + BUDGET,
            )?;
            continue;
        }
        let (id, tag, operation) = match message {
            Message::ReadMedia { id, binding } if version >= 4 => {
                (id, binding, Operation::Read(WireFormat::Media))
            }
            Message::Read { id, binding } => (id, binding, Operation::Read(WireFormat::Text)),
            Message::Write { id, binding, text } if text.len() <= bridge::MAX_TEXT_BYTES => {
                (id, binding, Operation::WriteText(text))
            }
            Message::ReadFiles { id, binding } if version >= 2 => {
                (id, binding, Operation::Read(WireFormat::Files))
            }
            Message::WriteFiles { id, binding, uris }
                if version >= 2 && uris.len() <= bridge::MAX_TEXT_BYTES =>
            {
                (
                    id,
                    binding,
                    Operation::WriteFiles(bridge::FileList::from_uri_list(&uris)),
                )
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
            if bound.binding.as_deref() != Some(&tag) || state.stopped.load(Ordering::Acquire) {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                match operation {
                    Operation::WriteText(text) => {
                        client.write(bridge::Target::Clipboard, text).map(|()| None)
                    }
                    Operation::WriteFiles(files) => files
                        .and_then(|files| client.write_files(files))
                        .map(|()| None),
                    Operation::Read(WireFormat::Media) => client
                        .begin_read_media()
                        .map(|read| Some(HostRead::Media(read))),
                    Operation::Read(WireFormat::Text) => client
                        .begin_read(bridge::Target::Clipboard)
                        .map(|read| Some(HostRead::Text(read))),
                    Operation::Read(WireFormat::Files) => client
                        .begin_read_files()
                        .map(|read| Some(HostRead::Files(read))),
                }
            }
        };
        let mut png = None;
        let response = match admitted {
            Ok(None) => Message::Queued { id },
            Ok(Some(mut pending)) => loop {
                let result = match &mut pending {
                    HostRead::Media(read) => read.poll().map(|result| {
                        result.and_then(|media| {
                            Ok(match media {
                                None => Message::Empty { id },
                                Some(gui::ClipboardPasteMedia::Files(uris)) => {
                                    Message::Files { id, uris }
                                }
                                Some(gui::ClipboardPasteMedia::Image(image)) => {
                                    let bytes = image.into_bytes();
                                    let len = u32::try_from(bytes.len()).map_err(|_| invalid())?;
                                    png = Some(bytes);
                                    Message::Png { id, bytes: len }
                                }
                            })
                        })
                    }),
                    HostRead::Text(read) => read
                        .poll()
                        .map(|result| result.map(|text| Message::Text { id, text })),
                    HostRead::Files(read) => read.poll().map(|result| {
                        result.map(|files| Message::Files {
                            id,
                            uris: files.into_uri_list(),
                        })
                    }),
                };
                if let Some(result) = result {
                    break result.unwrap_or(Message::Refused { id });
                }
                if state.stopped.load(Ordering::Acquire) {
                    return Ok(());
                }
                thread::sleep(POLL);
            },
            Err(_) => Message::Refused { id },
        };
        let end = Instant::now() + BUDGET;
        local::send(&mut stream, &response, MAX_FRAME, end)?;
        if let Some(png) = png {
            send_bitmap_bytes(&mut stream, &png, end)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BitmapFormat {
    Png,
    Jpeg,
    Tiff,
}

impl BitmapFormat {
    fn from_image(format: gui::ImageFormat) -> Option<Self> {
        match format {
            gui::ImageFormat::Png => Some(Self::Png),
            gui::ImageFormat::Jpeg => Some(Self::Jpeg),
            gui::ImageFormat::Tiff => Some(Self::Tiff),
            _ => None,
        }
    }
    fn image_format(self) -> gui::ImageFormat {
        match self {
            Self::Png => gui::ImageFormat::Png,
            Self::Jpeg => gui::ImageFormat::Jpeg,
            Self::Tiff => gui::ImageFormat::Tiff,
        }
    }
}

fn receive_bitmap_write(
    stream: &mut LocalStream,
    state: &State,
    client: &bridge::Client,
    id: u64,
    tag: Binding,
    bytes: u32,
    format: BitmapFormat,
) -> io::Result<()> {
    let end = Instant::now() + BUDGET;
    let grant = state
        .bound
        .lock()
        .expect("clipboard owner mutex")
        .binding
        .clone();
    let Some(grant) = grant.filter(|current| {
        **current == tag
            && !state.stopped.load(Ordering::Acquire)
            && valid_bitmap_size(bytes).is_ok()
    }) else {
        return local::send(stream, &Message::Refused { id }, 1024, end);
    };
    local::send(stream, &Message::SendBitmap { id }, 1024, end)?;
    match local::receive::<Message>(stream, 1024, end)? {
        Message::BitmapBody {
            id: got,
            active: true,
        } if got == id => (),
        Message::BitmapBody {
            id: got,
            active: false,
        } if got == id => return local::send(stream, &Message::Refused { id }, 1024, end),
        _ => return Err(invalid()),
    }
    let raw = receive_bitmap_bytes(stream, bytes, end)?;
    let image = gui::ClipboardPng::from_encoded(format.image_format(), &raw);
    let queued = {
        let bound = state.bound.lock().expect("clipboard owner mutex");
        bound
            .binding
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &grant))
            && !state.stopped.load(Ordering::Acquire)
            && Instant::now() < end
            && image.is_ok_and(|image| client.write_image(image).is_ok())
    };
    let response = if queued {
        Message::Queued { id }
    } else {
        Message::Refused { id }
    };
    local::send(stream, &response, 1024, Instant::now() + BUDGET)
}

// JSON frames poll a nonblocking socket so idle services can observe shutdown.
// A large binary tail instead lets the kernel wake the worker as space/data
// arrives. Short syscall timeouts keep the outer absolute deadline effective.
fn send_bitmap_bytes(stream: &mut LocalStream, bytes: &[u8], end: Instant) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(local::remaining(end)?.min(POLL)))?;
    local::write_all(stream, bytes, end)
}

fn valid_bitmap_size(size: u32) -> io::Result<usize> {
    let size = usize::try_from(size).map_err(|_| invalid())?;
    if !(1..=gui::MAX_CLIPBOARD_IMAGE_BYTES).contains(&size) {
        return Err(invalid());
    }
    Ok(size)
}

fn receive_bitmap_bytes(stream: &mut LocalStream, size: u32, end: Instant) -> io::Result<Vec<u8>> {
    let size = valid_bitmap_size(size)?;
    let timeout = local::remaining(end)?.min(POLL);
    match stream.set_read_timeout(Some(timeout)) {
        Ok(()) => stream.set_nonblocking(false)?,
        // Darwin may reject SO_RCVTIMEO after peer FIN. Read the queued tail
        // through the same deadline-aware loop, including EOF for a short tail.
        #[cfg(target_os = "macos")]
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            stream.set_nonblocking(true)?
        }
        Err(error) => return Err(error),
    }
    let mut bytes = vec![0; size];
    local::read_exact(stream, &mut bytes, end)?;
    Ok(bytes)
}

/// Producer-side mirror of its existing authenticated input watchdog.
#[derive(Default)]
pub(super) struct Gate(Mutex<Option<Arc<Grant>>>);
struct Grant {
    tag: Binding,
    deadline: Mutex<Instant>,
    lifecycle: Weak<crate::SessionLifecycle>,
    revoked: AtomicBool,
    file_offer_until: Mutex<Option<Instant>>,
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
                file_offer_until: Mutex::new(None),
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

enum Payload {
    String(String),
    Image(gui::ClipboardPng),
}
enum Transfer {
    OpenWebUrl(String),
    Read(WireFormat),
    WriteText(String),
    WriteFiles(String),
    WriteBitmap(BitmapFormat, Vec<u8>),
}
struct Job {
    transfer: Transfer,
    grant: Arc<Grant>,
    end: Instant,
    reply: async_channel::Sender<io::Result<Option<Payload>>>,
    _permit: Arc<ResourcePermit>,
}
struct Pending {
    receiver: async_channel::Receiver<io::Result<Option<Payload>>>,
    grant: Arc<Grant>,
    permit: Arc<ResourcePermit>,
}
pub(super) struct Producer {
    picker: Option<picker::Client>,
    web_urls: bool,
    image_copy: bool,
    media: bool,
    file_lists: bool,
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
                        web_urls: false,
                        picker: None,
                        image_copy: false,
                        media: false,
                        file_lists: false,
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
        let file_lists = std::env::var_os(FILES).is_some_and(|v| v == "1");
        let media = file_lists && std::env::var_os(MEDIA).is_some_and(|v| v == "1");
        let image_copy = media && std::env::var_os(IMAGE_COPY).is_some_and(|v| v == "1");
        let version = if image_copy {
            5
        } else if media {
            4
        } else if file_lists && std::env::var_os(OFFERS).is_some_and(|v| v == "1") {
            3
        } else if file_lists {
            2
        } else {
            1
        };
        let (picker, mut panel_worker) =
            picker::channel(std::env::var_os(picker::ENV).is_some_and(|v| v == "1"));
        let offer_gate = gate.clone();
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
                            version,
                            owner: *secret,
                        },
                        1024,
                        end,
                    )?;
                    if !matches!(
                        local::receive::<Message>(&mut stream, 1024, end)?,
                        Message::Ready { version: got } if got == version
                    ) {
                        return Err(invalid());
                    }
                    let mut id = 1u64;
                    let mut next_offer = Instant::now();
                    while !stop.load(Ordering::Acquire) {
                        if let Some(worker) = &mut panel_worker {
                            worker.pump(&mut stream, &mut id)?;
                        }
                        if version >= 3 && Instant::now() >= next_offer {
                            next_offer = Instant::now() + OFFER_INTERVAL;
                            if let Some(grant) = offer_gate.capture() {
                                let end = Instant::now() + BUDGET;
                                local::send(
                                    &mut stream,
                                    &Message::ProbeFiles {
                                        id,
                                        binding: grant.tag,
                                    },
                                    1024,
                                    end,
                                )?;
                                let Message::FileOffer { id: got, available } =
                                    local::receive(&mut stream, 1024, end)?
                                else {
                                    return Err(invalid());
                                };
                                if got != id {
                                    return Err(invalid());
                                }
                                *grant
                                    .file_offer_until
                                    .lock()
                                    .expect("clipboard offer mutex") = available.then_some(end);
                                id = id.checked_add(1).ok_or_else(invalid)?;
                            }
                        }
                        let job: Job = match receive.recv_timeout(POLL) {
                            Ok(job) => job,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        };
                        if !job.grant.live() || Instant::now() >= job.end {
                            continue;
                        }
                        let binding = job.grant.tag;
                        let mut bitmap = None;
                        let read_format = match &job.transfer {
                            Transfer::Read(format) => Some(*format),
                            _ => None,
                        };
                        let request = match job.transfer {
                            Transfer::OpenWebUrl(url) => Message::OpenWebUrl { id, binding, url },
                            Transfer::Read(WireFormat::Media) => Message::ReadMedia { id, binding },
                            Transfer::Read(WireFormat::Text) => Message::Read { id, binding },
                            Transfer::Read(WireFormat::Files) => Message::ReadFiles { id, binding },
                            Transfer::WriteText(text) => Message::Write { id, binding, text },
                            Transfer::WriteFiles(uris) => Message::WriteFiles { id, binding, uris },
                            Transfer::WriteBitmap(format, bytes) => {
                                let len = u32::try_from(bytes.len()).map_err(|_| invalid())?;
                                bitmap = Some(bytes);
                                Message::WriteBitmap {
                                    id,
                                    binding,
                                    bytes: len,
                                    format,
                                }
                            }
                        };
                        local::send(&mut stream, &request, MAX_FRAME, job.end)?;
                        if let Some(bytes) = bitmap {
                            match local::receive::<Message>(&mut stream, 1024, job.end)? {
                                Message::SendBitmap { id: got } if got == id => {
                                    let active = job.grant.live();
                                    local::send(
                                        &mut stream,
                                        &Message::BitmapBody { id, active },
                                        1024,
                                        job.end,
                                    )?;
                                    if active {
                                        send_bitmap_bytes(&mut stream, &bytes, job.end)?;
                                    }
                                }
                                Message::Refused { id: got } if got == id => {
                                    id = id.checked_add(1).ok_or_else(invalid)?;
                                    continue;
                                }
                                _ => return Err(invalid()),
                            }
                        }
                        let result =
                            match local::receive::<Message>(&mut stream, MAX_FRAME, job.end)? {
                                Message::Text { id: got, text }
                                    if got == id
                                        && read_format == Some(WireFormat::Text)
                                        && text.len() <= bridge::MAX_TEXT_BYTES =>
                                {
                                    Ok(Some(Payload::String(text)))
                                }
                                Message::Files { id: got, uris }
                                    if got == id
                                        && matches!(
                                            read_format,
                                            Some(WireFormat::Files | WireFormat::Media)
                                        ) =>
                                {
                                    Ok(Some(Payload::String(
                                        bridge::FileList::from_uri_list(&uris)?.into_uri_list(),
                                    )))
                                }
                                Message::Empty { id: got }
                                    if got == id && read_format == Some(WireFormat::Media) =>
                                {
                                    Ok(None)
                                }
                                Message::Png { id: got, bytes }
                                    if got == id && read_format == Some(WireFormat::Media) =>
                                {
                                    let png = receive_bitmap_bytes(&mut stream, bytes, job.end)?;
                                    gui::ClipboardPng::from_encoded(gui::ImageFormat::Png, &png)
                                        .map(|image| Some(Payload::Image(image)))
                                        .map_err(|_| invalid())
                                }
                                Message::Queued { id: got }
                                    if got == id && read_format.is_none() =>
                                {
                                    Ok(None)
                                }
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
                offer_gate.revoke();
                stop.store(true, Ordering::Release);
                cancel.lock().expect("clipboard cancel mutex").take();
            })?;
        Ok((
            Self {
                file_lists,
                media,
                image_copy,
                picker,
                web_urls: std::env::var_os(WEB_URLS).is_some_and(|v| v == "1"),
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
    pub(super) fn open_url(&self, raw: &str) {
        if self.web_urls
            && let Some(url) = web_url(raw)
        {
            let _ = self.begin(Transfer::OpenWebUrl(url));
        }
    }
    fn begin(&self, transfer: Transfer) -> io::Result<Pending> {
        let grant = self.gate.capture().ok_or(io::ErrorKind::PermissionDenied)?;
        let permit = Arc::new(self.budget.try_acquire()?);
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .try_send(Job {
                transfer,
                grant: grant.clone(),
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
    fn read_format<T: 'static>(
        &self,
        cx: &App,
        format: WireFormat,
        max_bytes: usize,
        convert: impl FnOnce(String) -> T + 'static,
    ) -> Task<Result<Option<T>, ClipboardReadError>> {
        self.read_payload(cx, format, move |payload| match payload {
            Payload::String(text) if text.len() <= max_bytes => {
                Ok((format != WireFormat::Files || !text.is_empty()).then(|| convert(text)))
            }
            _ => Err(ClipboardReadError::UnsupportedContent),
        })
    }
    fn read_payload<T: 'static>(
        &self,
        cx: &App,
        format: WireFormat,
        convert: impl FnOnce(Payload) -> Result<Option<T>, ClipboardReadError> + 'static,
    ) -> Task<Result<Option<T>, ClipboardReadError>> {
        let Ok(pending) = self.begin(Transfer::Read(format)) else {
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
            match result.map_err(|_| ClipboardReadError::Unavailable)? {
                Some(payload) => convert(payload),
                None => Ok(None),
            }
        })
    }
}
impl HostedClipboard for Producer {
    fn read(&self, cx: &App) -> Task<Result<Option<ClipboardItem>, ClipboardReadError>> {
        self.read_format(
            cx,
            WireFormat::Text,
            bridge::MAX_TEXT_BYTES,
            ClipboardItem::new_string,
        )
    }
    fn supports_file_lists(&self) -> bool {
        self.file_lists
    }
    fn has_file_offer(&self) -> bool {
        self.gate.capture().is_some_and(|grant| {
            grant
                .file_offer_until
                .lock()
                .expect("clipboard offer mutex")
                .is_some_and(|end| Instant::now() < end)
        })
    }
    fn read_file_uris(
        &self,
        cx: &App,
        max_bytes: usize,
    ) -> Task<Result<Option<String>, ClipboardReadError>> {
        if !self.file_lists || max_bytes > bridge::MAX_TEXT_BYTES {
            return Task::ready(Err(ClipboardReadError::Unavailable));
        }
        self.read_format(cx, WireFormat::Files, max_bytes, std::convert::identity)
    }
    fn read_media(
        &self,
        cx: &App,
    ) -> Task<Result<Option<gui::ClipboardPasteMedia>, ClipboardReadError>> {
        if !self.media {
            let read = self.read_file_uris(cx, bridge::MAX_TEXT_BYTES);
            return cx
                .foreground_executor()
                .spawn(async move { Ok(read.await?.map(gui::ClipboardPasteMedia::Files)) });
        }
        self.read_payload(cx, WireFormat::Media, |payload| {
            Ok(match payload {
                Payload::Image(image) => Some(gui::ClipboardPasteMedia::Image(image)),
                Payload::String(files) if !files.is_empty() => {
                    Some(gui::ClipboardPasteMedia::Files(files))
                }
                Payload::String(_) => None,
            })
        })
    }
    fn write(&self, mut item: ClipboardItem) {
        if let [gui::ClipboardEntry::Image(image)] = item.entries.as_slice() {
            if self.image_copy
                && (1..=gui::MAX_CLIPBOARD_IMAGE_BYTES).contains(&image.bytes.len())
                && let Some(format) = BitmapFormat::from_image(image.format)
                && let Some(gui::ClipboardEntry::Image(image)) = item.entries.pop()
            {
                let _ = self.begin(Transfer::WriteBitmap(format, image.bytes));
            }
            return;
        }
        if let [gui::ClipboardEntry::ExternalPaths(paths)] = item.entries.as_slice() {
            if self.file_lists
                && let Ok(files) = bridge::FileList::from_paths(&paths.0)
            {
                let _ = self.begin(Transfer::WriteFiles(files.into_uri_list()));
            }
        } else if !item.entries.is_empty()
            && item
                .entries
                .iter()
                .all(|entry| matches!(entry, gui::ClipboardEntry::String(_)))
            && let Ok(Some(text)) = gui::clipboard_text(Some(item), bridge::MAX_TEXT_BYTES)
        {
            let _ = self.begin(Transfer::WriteText(text));
        }
    }
}

#[cfg(all(test, unix, feature = "gui-testing"))]
mod tests;
