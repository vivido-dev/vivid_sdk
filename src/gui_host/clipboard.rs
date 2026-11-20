//! Bounded clipboard requests between hosted workers and the platform thread.
//!
//! Each channel belongs to one hosted application. The host controls activation
//! and closure; clients cannot enable themselves. Reads wait only on the worker,
//! while the platform thread handles requests without holding application locks.

// Rust guideline compliant 2026-10-07
use std::{
    collections::VecDeque,
    fmt, io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Maximum UTF-8 bytes in one hosted clipboard transfer.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Maximum queued and in-flight clipboard requests per application owner.
pub const MAX_PENDING_REQUESTS: usize = 4;
// A stalled platform request must not retain a terminal worker indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const CANCEL_INTERVAL: Duration = Duration::from_millis(10);
// Bound nested native/platform authority scopes independently of request count.
const MAX_SCOPES: usize = 8;

/// Selects the system clipboard or the optional primary selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// The ordinary copy/paste clipboard.
    Clipboard,
    /// The platform's primary-selection buffer.
    Primary,
}

/// Selects text or an explicit local file-list clipboard representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// Bounded UTF-8 text.
    Text,
    /// Bounded local file URI metadata, never file contents.
    Files,
    /// One explicit paste offer, preferring paths to a normalized bitmap.
    Media,
}

/// Canonical bounded local file metadata without filesystem authority.
#[derive(Clone, PartialEq, Eq)]
pub struct FileList {
    uris: String,
}
impl FileList {
    /// Validates local file URIs and normalizes their CRLF-separated representation.
    ///
    /// No path is opened, resolved or checked for existence.
    ///
    /// # Errors
    /// Rejects remote/ambiguous URIs, invalid paths and excess files or bytes.
    pub fn from_uri_list(value: &str) -> io::Result<Self> {
        let paths =
            gpui::parse_file_uri_list(value, MAX_TEXT_BYTES).map_err(|_| invalid_files())?;
        Self::from_paths(&paths)
    }
    /// Creates a file list from bounded absolute native paths.
    ///
    /// # Errors
    /// Rejects relative/NUL paths, excess path lengths/counts or encoded bytes.
    pub fn from_paths(paths: &[std::path::PathBuf]) -> io::Result<Self> {
        Ok(Self {
            uris: gpui::file_uri_list(paths, MAX_TEXT_BYTES).map_err(|_| invalid_files())?,
        })
    }
    /// Borrows canonical URI metadata without reading files.
    pub fn as_uri_list(&self) -> &str {
        &self.uris
    }
    /// Consumes the canonical URI metadata.
    pub fn into_uri_list(self) -> String {
        self.uris
    }
    /// Returns native absolute paths without filesystem access.
    ///
    /// # Errors
    /// Returns an error if the retained URI representation is invalid.
    pub fn paths(&self) -> io::Result<Vec<std::path::PathBuf>> {
        gpui::parse_file_uri_list(&self.uris, MAX_TEXT_BYTES).map_err(|_| invalid_files())
    }
}
impl fmt::Debug for FileList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardFileList")
            .field("bytes", &self.uris.len())
            .finish_non_exhaustive()
    }
}
fn invalid_files() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or oversized local clipboard file list",
    )
}

enum Contents {
    Image(gpui::ClipboardPng),
    Text(String),
    Files(FileList),
}

struct State {
    access: Mutex<()>,
    queue: Mutex<VecDeque<RequestData>>,
    active: AtomicBool,
    closed: AtomicBool,
    generation: AtomicU64,
    in_flight: AtomicBool,
    primary: bool,
    files: bool,
    media: bool,
    files_available: AtomicBool,
}
impl State {
    fn current(&self, generation: u64) -> bool {
        !self.closed.load(Ordering::SeqCst)
            && self.active.load(Ordering::SeqCst)
            && self.generation.load(Ordering::SeqCst) == generation
    }
}

/// Owns one application's platform-side clipboard service.
pub struct Host {
    state: Arc<State>,
}
/// Sends clipboard operations from an application worker.
#[derive(Clone)]
pub struct Client {
    state: Arc<State>,
    scopes: Vec<Arc<ScopeState>>,
}

struct ScopeState {
    gate: Mutex<()>,
    active: AtomicBool,
}
/// Revokes a child clipboard authority independently of its GUI owner.
///
/// Dropping this guard cancels its requests and descendants. Recreating focus
/// requires a fresh scope; a revoked scope cannot be reactivated.
pub struct Scope(Arc<ScopeState>);
impl Scope {
    /// Revokes requests after any already admitted bounded operation finishes.
    pub fn revoke(&self) {
        let _gate = self.0.gate.lock().expect("clipboard scope gate poisoned");
        self.0.active.store(false, Ordering::SeqCst);
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.revoke();
    }
}
impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardScope").finish_non_exhaustive()
    }
}

/// A cancellable clipboard read polled without blocking an application worker.
pub struct PendingRead<T = String> {
    receiver: mpsc::Receiver<io::Result<T>>,
    lease: Lease,
    finished: bool,
}
/// A cancellable file-list read sharing the owner's text request limits.
#[derive(Debug)]
pub struct PendingFileRead {
    inner: PendingRead,
}
impl PendingFileRead {
    /// Takes one validated file list, or returns `None` while pending/consumed.
    ///
    /// Revocation, timeout, unsupported formats and service loss return errors.
    pub fn poll(&mut self) -> Option<io::Result<FileList>> {
        self.inner
            .poll()
            .map(|result| result.map(|uris| FileList { uris }))
    }
}
impl<T> PendingRead<T> {
    /// Takes a completed result, or returns `None` while waiting or after consumption.
    ///
    /// Revocation, timeout and service loss complete with an error. Late replies
    /// cannot restore a cancelled request. Dropping the read cancels platform work.
    pub fn poll(&mut self) -> Option<io::Result<T>> {
        if self.finished {
            return None;
        }
        let now = Instant::now();
        let result = if !self.lease.live_at(now) {
            Err(io::Error::new(
                if now >= self.lease.deadline {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::PermissionDenied
                },
                "clipboard read expired or was revoked",
            ))
        } else {
            match self.receiver.try_recv() {
                Ok(result) if self.lease.live_at(Instant::now()) => result,
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "clipboard read was revoked",
                )),
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "clipboard service ended",
                )),
            }
        };
        self.finished = true;
        Some(result)
    }
}
impl<T> Drop for PendingRead<T> {
    fn drop(&mut self) {
        self.lease.cancelled.store(true, Ordering::SeqCst);
    }
}
impl<T> fmt::Debug for PendingRead<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingClipboardRead")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

/// Guards a request against focus changes, timeout, closure, and abandonment.
#[derive(Clone)]
pub struct Lease {
    state: Arc<State>,
    generation: u64,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    scopes: Vec<Arc<ScopeState>>,
}
impl Lease {
    /// Whether platform work may still access the clipboard for this request.
    pub fn is_live(&self) -> bool {
        self.live_at(Instant::now()) && !self.finished.load(Ordering::SeqCst)
    }
    fn live_at(&self, now: Instant) -> bool {
        self.state.current(self.generation)
            && now < self.deadline
            && !self.cancelled.load(Ordering::SeqCst)
            && self
                .scopes
                .iter()
                .all(|scope| scope.active.load(Ordering::SeqCst))
    }
    /// Runs a bounded platform operation while serialized with authority revocation.
    ///
    /// The operation must not wait, revoke this owner/scope, or service callbacks
    /// that revoke it. Start asynchronous reads here and release all guards before
    /// awaiting them; revalidate the request when completing its result.
    pub fn while_live<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let _access = self
            .state
            .access
            .lock()
            .expect("clipboard access gate poisoned");
        let _scopes: Vec<_> = self
            .scopes
            .iter()
            .map(|scope| scope.gate.lock().expect("clipboard scope gate poisoned"))
            .collect();
        self.is_live().then(operation)
    }
}
enum Reply {
    Text(mpsc::SyncSender<io::Result<String>>),
    Media(mpsc::SyncSender<io::Result<Option<gpui::ClipboardPasteMedia>>>),
}
struct RequestData {
    target: Target,
    format: Format,
    write: Option<Contents>,
    reply: Option<Reply>,
    lease: Lease,
}
/// Holds the sole in-flight platform operation for an owner.
pub struct Request {
    data: RequestData,
}
impl Request {
    /// Returns the clipboard buffer selected by the caller.
    pub fn target(&self) -> Target {
        self.data.target
    }
    /// Returns the requested representation; providers must handle it explicitly.
    pub fn format(&self) -> Format {
        self.data.format
    }
    /// Borrows write contents, or returns `None` for a read.
    pub fn write_text(&self) -> Option<&str> {
        match &self.data.write {
            Some(Contents::Text(text)) => Some(text),
            _ => None,
        }
    }
    /// Borrows validated file metadata for a write, or returns `None` otherwise.
    pub fn write_files(&self) -> Option<&FileList> {
        match &self.data.write {
            Some(Contents::Files(files)) => Some(files),
            _ => None,
        }
    }
    /// Takes a normalized PNG for a native bitmap write without cloning its pixels.
    pub fn take_write_image(&mut self) -> Option<gpui::Image> {
        if matches!(self.data.write, Some(Contents::Image(_)))
            && let Some(Contents::Image(image)) = self.data.write.take()
        {
            return Some(image.into_image());
        }
        None
    }
    /// Returns a cancellation guard for an asynchronous platform task.
    pub fn lease(&self) -> Lease {
        self.data.lease.clone()
    }
    /// Whether this operation still has the owner's clipboard permission.
    pub fn is_live(&self) -> bool {
        self.data.lease.is_live()
    }
    /// Completes a read, rejecting stale or oversized platform responses.
    ///
    /// For a write, pass an empty successful response after performing the write.
    pub fn complete(mut self, result: io::Result<String>) {
        if !self.is_live() {
            return;
        }
        let result = result.and_then(|text| {
            if self.data.format == Format::Files && self.data.write.is_none() {
                FileList::from_uri_list(&text).map(FileList::into_uri_list)
            } else if text.len() > MAX_TEXT_BYTES {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "clipboard text exceeds limit",
                ))
            } else {
                Ok(text)
            }
        });
        if let Some(Reply::Text(reply)) = self.data.reply.take() {
            let _ = reply.try_send(result);
        }
    }
    /// Completes a media read with validated local files or a bounded raster image.
    pub fn complete_media(mut self, result: io::Result<Option<gpui::ClipboardPasteMedia>>) {
        if !self.is_live() {
            return;
        }
        if let Some(Reply::Media(reply)) = self.data.reply.take() {
            let result = result.and_then(|media| match media {
                Some(gpui::ClipboardPasteMedia::Files(uris)) => FileList::from_uri_list(&uris)
                    .map(|files| Some(gpui::ClipboardPasteMedia::Files(files.into_uri_list()))),
                other => Ok(other),
            });
            let _ = reply.try_send(result);
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.data.lease.finished.store(true, Ordering::SeqCst);
        self.data
            .lease
            .state
            .in_flight
            .store(false, Ordering::SeqCst);
    }
}

impl Host {
    /// Creates an inactive, isolated channel with optional primary-selection support.
    pub fn new(primary: bool) -> (Self, Client) {
        Self::create(primary, false, false)
    }
    /// Creates an inactive service whose provider handles local file lists too.
    ///
    /// Providers must dispatch [`Request::format`] and perform only bounded
    /// platform operations under the request lease. Primary remains text-only.
    pub fn with_file_lists(primary: bool) -> (Self, Client) {
        Self::create(primary, true, false)
    }
    /// Creates a provider for explicit media paste, file lists and text.
    pub fn with_media(primary: bool) -> (Self, Client) {
        Self::create(primary, true, true)
    }
    fn create(primary: bool, files: bool, media: bool) -> (Self, Client) {
        let state = Arc::new(State {
            access: Mutex::new(()),
            queue: Mutex::new(VecDeque::new()),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            in_flight: AtomicBool::new(false),
            primary,
            files,
            media,
            files_available: AtomicBool::new(false),
        });
        (
            Self {
                state: state.clone(),
            },
            Client {
                state,
                scopes: Vec::new(),
            },
        )
    }
    /// Updates file MIME availability without reading or granting clipboard access.
    ///
    /// File-capable platform providers refresh this snapshot from native offer
    /// metadata. A text-only clipboard must not advertise file-list MIME types.
    pub fn set_file_list_available(&self, available: bool) {
        self.state
            .files_available
            .store(self.state.files && available, Ordering::SeqCst);
    }

    /// Enables or revokes this owner's clipboard permission, invalidating old requests.
    pub fn set_active(&self, active: bool) {
        let access = self
            .state
            .access
            .lock()
            .expect("clipboard access gate poisoned");
        let active = active && !self.state.closed.load(Ordering::SeqCst);
        if self.state.active.swap(active, Ordering::SeqCst) != active {
            if self
                .state
                .generation
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
                .is_err()
            {
                self.state.closed.store(true, Ordering::SeqCst);
                self.state.active.store(false, Ordering::SeqCst);
                self.state
                    .queue
                    .lock()
                    .expect("clipboard queue poisoned")
                    .clear();
            } else {
                self.state
                    .queue
                    .lock()
                    .expect("clipboard queue poisoned")
                    .clear();
            }
        }
        drop(access);
    }
    /// Permanently revokes the owner and discards its queued operations.
    pub fn close(&self) {
        let _access = self
            .state
            .access
            .lock()
            .expect("clipboard access gate poisoned");
        self.state.closed.store(true, Ordering::SeqCst);
        self.state.active.store(false, Ordering::SeqCst);
        self.state
            .queue
            .lock()
            .expect("clipboard queue poisoned")
            .clear();
    }
    /// Takes the next live operation without blocking the platform thread.
    ///
    /// Only one request can be in flight; dropping it admits the next one.
    pub fn next_request(&self) -> Option<Request> {
        let mut queue = self.state.queue.lock().expect("clipboard queue poisoned");
        if self.state.in_flight.load(Ordering::SeqCst) {
            return None;
        }
        while let Some(data) = queue.pop_front() {
            if data.lease.is_live() {
                self.state.in_flight.store(true, Ordering::SeqCst);
                return Some(Request { data });
            }
        }
        None
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.close();
    }
}

impl Client {
    /// Creates an independently revocable child authority for native focus or requests.
    ///
    /// # Errors
    /// Returns an error when more than eight scopes would be nested.
    pub fn scoped(&self) -> io::Result<(Scope, Self)> {
        if self.scopes.len() >= MAX_SCOPES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "clipboard scope limit exceeded",
            ));
        }
        let scope = Arc::new(ScopeState {
            gate: Mutex::new(()),
            active: AtomicBool::new(true),
        });
        let mut scopes = self.scopes.clone();
        scopes.push(scope.clone());
        Ok((
            Scope(scope),
            Self {
                state: self.state.clone(),
                scopes,
            },
        ))
    }
    /// Starts a bounded read without waiting for the platform thread.
    ///
    /// # Errors
    /// Returns an error for inactive authority, unsupported targets or exhausted capacity.
    pub fn begin_read(&self, target: Target) -> io::Result<PendingRead> {
        self.begin_read_format(target, Format::Text)
    }
    /// Starts a cancellable file-list read without blocking the worker.
    ///
    /// # Errors
    /// Rejects unavailable file support, inactive authority or exhausted capacity.
    pub fn begin_read_files(&self) -> io::Result<PendingFileRead> {
        self.begin_read_format(Target::Clipboard, Format::Files)
            .map(|inner| PendingFileRead { inner })
    }
    /// Starts one cancellable media paste without waiting on the platform thread.
    ///
    /// # Errors
    /// Rejects inactive owners, unsupported media or exhausted request capacity.
    pub fn begin_read_media(&self) -> io::Result<PendingRead<Option<gpui::ClipboardPasteMedia>>> {
        self.begin_reply(Target::Clipboard, Format::Media, Reply::Media)
    }
    fn begin_read_format(&self, target: Target, format: Format) -> io::Result<PendingRead> {
        self.begin_reply(target, format, Reply::Text)
    }
    fn begin_reply<T>(
        &self,
        target: Target,
        format: Format,
        wrap: fn(mpsc::SyncSender<io::Result<T>>) -> Reply,
    ) -> io::Result<PendingRead<T>> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let lease = self.enqueue(target, format, None, Some(wrap(reply)))?;
        Ok(PendingRead {
            receiver,
            lease,
            finished: false,
        })
    }
    /// Whether this host provides a primary selection.
    pub fn has_primary(&self) -> bool {
        self.state.primary
    }
    /// Whether this provider explicitly supports bounded local file lists.
    pub fn has_file_lists(&self) -> bool {
        self.state.files
    }
    /// Reports the provider's latest file-list MIME availability snapshot.
    pub fn file_list_available(&self) -> bool {
        self.state.files_available.load(Ordering::SeqCst)
    }
    fn enqueue(
        &self,
        target: Target,
        format: Format,
        write: Option<Contents>,
        reply: Option<Reply>,
    ) -> io::Result<Lease> {
        if format == Format::Media && (!self.state.media || target != Target::Clipboard) {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        if format == Format::Files && (!self.state.files || target != Target::Clipboard) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "clipboard file lists are unavailable",
            ));
        }
        if target == Target::Primary && !self.state.primary {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "primary selection is unavailable",
            ));
        }
        if write.as_ref().is_some_and(|data| match data {
            Contents::Image(_) => false, // ClipboardPng already bounds encoded and decoded sizes.
            Contents::Text(text) => text.len() > MAX_TEXT_BYTES,
            Contents::Files(files) => files.as_uri_list().len() > MAX_TEXT_BYTES,
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "clipboard text exceeds limit",
            ));
        }
        let lease = Lease {
            state: self.state.clone(),
            generation: self.state.generation.load(Ordering::SeqCst),
            deadline: Instant::now() + REQUEST_TIMEOUT,
            cancelled: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            scopes: self.scopes.clone(),
        };
        let mut queue = self.state.queue.lock().expect("clipboard queue poisoned");
        if !lease.is_live() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "clipboard owner is inactive",
            ));
        }
        queue.retain(|request| request.lease.is_live());
        if queue.len() + usize::from(self.state.in_flight.load(Ordering::SeqCst))
            >= MAX_PENDING_REQUESTS
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "clipboard queue is full",
            ));
        }
        queue.push_back(RequestData {
            target,
            format,
            write,
            reply,
            lease: lease.clone(),
        });
        Ok(lease)
    }
    /// Queues a bounded clipboard write without waiting on the worker.
    ///
    /// Success means admission; activation changes or timeout can still cancel the write.
    ///
    /// # Errors
    /// Returns an error for inactive owners, unsupported targets, excess bytes, or full queues.
    pub fn write(&self, target: Target, text: String) -> io::Result<()> {
        self.enqueue(target, Format::Text, Some(Contents::Text(text)), None)
            .map(|_| ())
    }
    /// Queues an already normalized image for the ordinary clipboard.
    ///
    /// # Errors
    /// Rejects an inactive owner, a provider without media support or exhausted queue capacity.
    pub fn write_image(&self, image: gpui::ClipboardPng) -> io::Result<()> {
        self.enqueue(
            Target::Clipboard,
            Format::Media,
            Some(Contents::Image(image)),
            None,
        )
        .map(|_| ())
    }
    /// Queues validated local file metadata for the ordinary clipboard.
    ///
    /// # Errors
    /// Rejects unsupported providers, inactive authority or full request queues.
    pub fn write_files(&self, files: FileList) -> io::Result<()> {
        self.enqueue(
            Target::Clipboard,
            Format::Files,
            Some(Contents::Files(files)),
            None,
        )
        .map(|_| ())
    }
    /// Reads text while the platform thread services the request independently.
    ///
    /// Call only from a worker. Waiting is bounded to one second and checks owner
    /// revocation at ten-millisecond intervals. Late replies cannot restore permission.
    ///
    /// # Errors
    /// Returns an error on revocation, timeout, service loss, capacity, or an oversized response.
    pub fn read(&self, target: Target) -> io::Result<String> {
        self.read_format(target, Format::Text)
    }
    /// Reads a bounded local file list while the platform thread services it.
    ///
    /// Call from a worker; timeout and cancellation match ordinary text reads.
    ///
    /// # Errors
    /// Rejects unavailable support, revocation, timeout, service loss or invalid data.
    pub fn read_files(&self) -> io::Result<FileList> {
        self.read_format(Target::Clipboard, Format::Files)
            .map(|uris| FileList { uris })
    }
    /// Reads files or image pixels from one explicit clipboard paste.
    ///
    /// # Errors
    /// Rejects unsupported providers, invalid media, timeouts and revoked authority.
    pub fn read_media(&self) -> io::Result<Option<gpui::ClipboardPasteMedia>> {
        self.read_reply(Target::Clipboard, Format::Media, Reply::Media)
    }

    fn read_format(&self, target: Target, format: Format) -> io::Result<String> {
        self.read_reply(target, format, Reply::Text)
    }

    fn read_reply<T>(
        &self,
        target: Target,
        format: Format,
        wrap: fn(mpsc::SyncSender<io::Result<T>>) -> Reply,
    ) -> io::Result<T> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let lease = self.enqueue(target, format, None, Some(wrap(reply)))?;
        struct Cancel(Arc<AtomicBool>);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _cancel = Cancel(lease.cancelled.clone());
        loop {
            if !lease.live_at(Instant::now()) {
                let kind = if Instant::now() >= lease.deadline {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::PermissionDenied
                };
                return Err(io::Error::new(
                    kind,
                    "clipboard request expired or was revoked",
                ));
            }
            match receiver.recv_timeout(CANCEL_INTERVAL) {
                Ok(result) => {
                    if !lease.live_at(Instant::now()) {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "clipboard request was revoked",
                        ));
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "clipboard service ended",
                    ));
                }
            }
        }
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardHost").finish_non_exhaustive()
    }
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardClient").finish_non_exhaustive()
    }
}
impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardLease")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardRequest")
            .field("target", &self.target())
            .field("write_bytes", &self.write_text().map(str::len))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    fn next(host: &Host) -> Request {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(request) = host.next_request() {
                return request;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }
    #[test]
    fn focus_generations_and_owner_teardown_reject_late_replies() {
        let (first, a) = Host::new(false);
        let (second, b) = Host::new(false);
        first.set_active(true);
        second.set_active(true);
        let worker_a = thread::spawn(move || a.read(Target::Clipboard));
        let worker_b = thread::spawn(move || b.read(Target::Clipboard));
        let stale = next(&first);
        let current = next(&second);
        first.set_active(false);
        first.set_active(true);
        assert!(!stale.is_live());
        stale.complete(Ok("old owner contents".into()));
        drop(first);
        current.complete(Ok("second owner contents".into()));
        assert!(worker_a.join().unwrap().is_err());
        assert_eq!(worker_b.join().unwrap().unwrap(), "second owner contents");
    }
    #[test]
    fn a_stale_media_reply_cannot_cross_focus_or_cancel_the_peer_read() {
        let (one, a) = Host::with_media(false);
        let (two, b) = Host::with_media(false);
        one.set_active(true);
        two.set_active(true);
        let first = thread::spawn(move || a.read_media());
        let second = thread::spawn(move || b.read_media());
        let stale = next(&one);
        let peer = next(&two);
        assert_eq!(stale.format(), Format::Media);
        one.set_active(false);
        one.set_active(true);
        stale.complete_media(Ok(None));
        peer.complete_media(Ok(None));
        assert!(first.join().unwrap().is_err());
        assert!(matches!(second.join().unwrap(), Ok(None)));
        let (_, text_only) = Host::with_file_lists(false);
        assert!(matches!(text_only.read_media(), Err(e) if e.kind() == io::ErrorKind::Unsupported));
    }
    #[test]
    fn capacity_includes_in_flight_work_and_diagnostics_redact_contents() {
        let (host, client) = Host::new(true);
        host.set_active(true);
        client
            .write(Target::Clipboard, "private clipboard contents".into())
            .unwrap();
        let request = next(&host);
        assert!(!format!("{request:?}").contains("private clipboard contents"));
        for _ in 1..MAX_PENDING_REQUESTS {
            client.write(Target::Primary, "x".into()).unwrap();
        }
        assert!(client.write(Target::Clipboard, "x".into()).is_err());
        assert!(host.next_request().is_none());
        drop(request);
        assert!(host.next_request().is_some());
        assert!(
            client
                .write(Target::Clipboard, "x".repeat(MAX_TEXT_BYTES + 1))
                .is_err()
        );
        host.close();
        host.set_active(true);
        assert!(client.write(Target::Clipboard, "x".into()).is_err());
    }
    #[test]
    fn abandoned_and_oversized_reads_are_bounded() {
        let (host, client) = Host::new(false);
        host.set_active(true);
        assert!(client.read(Target::Primary).is_err());
        let worker = thread::spawn(move || client.read(Target::Clipboard));
        let request = next(&host);
        let lease = request.lease();
        assert!(!lease.live_at(lease.deadline));
        request.complete(Ok("x".repeat(MAX_TEXT_BYTES + 1)));
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    #[test]
    fn unserviced_reads_time_out_and_queued_writes_do_not_survive_focus_loss() {
        let (host, client) = Host::new(false);
        host.set_active(true);
        let started = Instant::now();
        assert_eq!(
            client.read(Target::Clipboard).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(host.next_request().is_none());
        client
            .write(Target::Clipboard, "old focus contents".into())
            .unwrap();
        host.set_active(false);
        host.set_active(true);
        assert!(host.next_request().is_none());
        let weak = Arc::downgrade(&host.state);
        client
            .write(Target::Clipboard, "queued contents".into())
            .unwrap();
        drop(client);
        drop(host);
        assert!(
            weak.upgrade().is_none(),
            "queued leases retained the closed owner's state"
        );
    }
}
