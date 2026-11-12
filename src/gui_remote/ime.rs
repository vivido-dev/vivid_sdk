//! Private process composition, pinned to authenticated input and native editor grants.

// Rust guideline compliant 2026-10-07
mod producer;
#[cfg(all(test, unix))]
mod tests;
use super::{local, *};
use crate::local_control::{LocalListener, LocalStream};
pub(super) use producer::Producer;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
    thread::{self, JoinHandle},
};

// Private service version; independent of the Vivid wire protocol.
const VERSION: u32 = 2;
const ENDPOINT: &str = "VIVID_GUI_IME_ENDPOINT";
const OWNER: &str = "VIVID_GUI_IME_OWNER";
const POLL: Duration = Duration::from_millis(10);
const BUDGET: Duration = Duration::from_secs(1);
const STARTUP: Duration = Duration::from_secs(15);
// Each of two bounded strings may use six JSON bytes per UTF-8 byte.
const MAX_FRAME: usize = MAX_TEXT_BYTES * 12 + 2048;
/// Maximum UTF-8 bytes in each native commit or preedit.
pub const MAX_TEXT_BYTES: usize = 4096;
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
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid GUI composition channel",
    )
}

/// One atomic commit and replacement composition, with UTF-8 selection offsets.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    /// Committed text; None leaves the current document selection untouched.
    pub commit: Option<String>,
    /// New preedit, or empty to clear composition.
    pub preedit: String,
    /// Ordered UTF-8 byte offsets within preedit, or no visible selection.
    pub selection: Option<(u32, u32)>,
    /// Snapshot revision for document operations; absent for composition-only editors.
    pub revision: Option<u64>,
    /// Ordered excerpt UTF-8 replacement covering the current selection.
    pub replacement: Option<(u32, u32)>,
    /// Standalone ordered excerpt UTF-8 selection.
    pub document_selection: Option<(u32, u32)>,
}
impl fmt::Debug for Edit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositionEdit")
            .field("commit_bytes", &self.commit.as_ref().map(String::len))
            .field("preedit_bytes", &self.preedit.len())
            .finish_non_exhaustive()
    }
}
impl Edit {
    fn validate(&self) -> io::Result<()> {
        if self
            .commit
            .as_ref()
            .is_some_and(|s| s.len() > MAX_TEXT_BYTES)
            || self.preedit.len() > MAX_TEXT_BYTES
            || self.revision == Some(0)
            || ((self.replacement.is_some() || self.document_selection.is_some())
                && self.revision.is_none())
            || self
                .replacement
                .is_some_and(|(a, b)| a > b || b as usize > MAX_TEXT_BYTES)
            || self
                .document_selection
                .is_some_and(|(a, b)| a > b || b as usize > MAX_TEXT_BYTES)
            || (self.document_selection.is_some()
                && (self.commit.is_some()
                    || !self.preedit.is_empty()
                    || self.selection.is_some()
                    || self.replacement.is_some()))
            || (self.replacement.is_some() && self.commit.is_none() && self.preedit.is_empty())
            || self.selection.is_some_and(|(a, b)| {
                a > b
                    || !self.preedit.is_char_boundary(a as usize)
                    || !self.preedit.is_char_boundary(b as usize)
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn absolute(&self) -> bool {
        self.replacement.is_some() || self.document_selection.is_some()
    }
    fn matches(&self, document: Option<&Document>) -> bool {
        match (self.revision, document) {
            (None, _) => !self.absolute(),
            (Some(revision), Some(document)) => {
                revision <= document.revision
                    && (!self.absolute()
                        || (revision == document.revision
                            && !document.sensitive
                            && self
                                .replacement
                                .iter()
                                .chain(self.document_selection.iter())
                                .all(|&(a, b)| {
                                    document.text.is_char_boundary(a as usize)
                                        && document.text.is_char_boundary(b as usize)
                                })
                            && self.replacement.is_none_or(|(a, b)| {
                                a <= document.cursor.min(document.anchor)
                                    && b >= document.cursor.max(document.anchor)
                            })))
            }
            _ => false,
        }
    }
}

/// A revision-scoped bounded excerpt, deliberately excluding producer-local offsets.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// Monotonic nonzero revision within the current editor incarnation.
    pub revision: u64,
    /// At most 4096 UTF-8 bytes of committed text, excluding preedit.
    pub text: String,
    /// Active endpoint in excerpt UTF-8 bytes.
    pub cursor: u32,
    /// Fixed endpoint in excerpt UTF-8 bytes.
    pub anchor: u32,
    /// Masked fields expose no committed text or absolute editing authority.
    pub sensitive: bool,
}
impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("revision", &self.revision)
            .field("bytes", &self.text.len())
            .field("sensitive", &self.sensitive)
            .finish_non_exhaustive()
    }
}
impl Document {
    fn valid(&self) -> bool {
        self.revision != 0
            && self.text.len() <= MAX_TEXT_BYTES
            && self.text.is_char_boundary(self.cursor as usize)
            && self.text.is_char_boundary(self.anchor as usize)
            && (!self.sensitive || (self.text.is_empty() && self.cursor == 0 && self.anchor == 0))
    }
}

/// An immutable editor destination belonging to one private process channel.
#[derive(Clone)]
pub struct Target {
    state: Arc<State>,
    generation: u64,
    context: (u64, u64),
}
impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositionTarget").finish_non_exhaustive()
    }
}
impl Target {
    /// Compares the owner, input incarnation and exact producer editor.
    pub fn same_editor(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
            && self.generation == other.generation
            && self.context.0 == other.context.0
            && self.context.1 == other.context.1
    }
}
/// Cached eligibility and pane-normalized native candidate rectangle.
#[derive(Clone, Debug)]
pub struct Editor {
    /// Destination captured by a native handler; old callbacks cannot retarget.
    pub target: Target,
    /// Clipped rectangle normalized to the producer canvas, when available.
    pub caret: Option<[f64; 4]>,
    /// Only present when the producer explicitly exports document context.
    pub document: Option<Document>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Native {
    editor: u64,
    reset: u64,
    caret: Option<[f64; 4]>,
    document: Option<Document>,
}
impl Native {
    fn valid(&self) -> bool {
        self.editor != 0
            && self.document.as_ref().is_none_or(Document::valid)
            && self.caret.is_none_or(|r| {
                r.iter().all(|n| n.is_finite() && (0.0..=1.0).contains(n))
                    && r[0] + r[2] <= 1.0 + f64::EPSILON
                    && r[1] + r[3] <= 1.0 + f64::EPSILON
            })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    editor: u64,
    reset: u64,
    edit: Edit,
}
struct Job {
    operation: Operation,
    end: Instant,
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
    Tick {
        id: u64,
        generation: u64,
        binding: Option<Binding>,
        edit: Option<Operation>,
    },
    Snapshot {
        id: u64,
        generation: u64,
        binding: Option<Binding>,
        editor: Option<Native>,
    },
}
struct Bound {
    generation: u64,
    tag: Option<Binding>,
    snapshot: Option<(Native, Instant)>,
    jobs: VecDeque<Job>,
    floor: (u64, u64),
    last_editor: Option<Native>,
}
impl Default for Bound {
    fn default() -> Self {
        Self {
            generation: 1,
            tag: None,
            snapshot: None,
            jobs: VecDeque::new(),
            floor: (0, 0),
            last_editor: None,
        }
    }
}
#[derive(Default)]
struct State {
    bound: Mutex<Bound>,
    stopped: AtomicBool,
    stream: Mutex<Option<LocalStream>>,
}
impl State {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let mut bound = self.bound.lock().expect("composition mutex");
        bound.snapshot = None;
        bound.last_editor = None;
        bound.jobs.clear();
        drop(bound);
        if let Some(stream) = self.stream.lock().expect("composition stream mutex").take() {
            let _ = stream.shutdown();
        }
    }
    fn install(&self, stream: &LocalStream) -> io::Result<()> {
        let mut slot = self.stream.lock().expect("composition stream mutex");
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        *slot = Some(stream.try_clone()?);
        Ok(())
    }
}

/// Nonblocking composition controller for one independently hosted GUI process.
#[derive(Clone)]
pub struct Host {
    state: Arc<State>,
    endpoint: PathBuf,
    owner: zeroize::Zeroizing<[u8; 16]>,
}
impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositionHost").finish_non_exhaustive()
    }
}
impl Host {
    /// Creates a private authenticated composition channel and native worker.
    ///
    /// # Errors
    /// Returns invalid-path, entropy, transport or worker creation errors.
    pub fn bind(endpoint: &Path) -> io::Result<(Self, Service)> {
        local::validate_endpoint(endpoint)?;
        let mut owner = zeroize::Zeroizing::new([0; 16]);
        getrandom::fill(owner.as_mut()).map_err(io::Error::other)?;
        let listener = LocalListener::bind(endpoint)?;
        let state = Arc::new(State::default());
        let shared = state.clone();
        let secret = owner.clone();
        let worker = thread::Builder::new()
            .name("vivid-gui-ime-host".into())
            .spawn(move || {
                let _ = serve(listener, &secret, &shared);
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
    /// Supplies owner-private discovery only through the associated child's environment.
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
    /// Installs an actually presented input binding, or revokes queued operations.
    pub fn set_binding(&self, tag: Option<crate::InputTuple>) {
        let mut bound = self.state.bound.lock().expect("composition mutex");
        let tag = tag.map(binding);
        if bound.tag != tag {
            let Some(next) = bound.generation.checked_add(1) else {
                drop(bound);
                self.close();
                return;
            };
            bound.generation = next;
            bound.tag = tag;
            bound.snapshot = None;
            bound.last_editor = None;
            bound.jobs.clear();
        }
    }
    /// Reads a fresh bounded editor snapshot without producer or platform I/O.
    pub fn editor(&self) -> Option<Editor> {
        let bound = self.state.bound.lock().expect("composition mutex");
        if self.state.stopped.load(Ordering::Acquire) || bound.tag.is_none() {
            return None;
        }
        let (context, at) = bound.snapshot.as_ref()?;
        // A stalled GUI/worker must not keep native input eligibility indefinitely.
        if at.elapsed() > Duration::from_millis(500) {
            return None;
        }
        Some(Editor {
            target: Target {
                state: self.state.clone(),
                generation: bound.generation,
                context: (context.editor, context.reset),
            },
            caret: context.caret,
            document: context.document.clone(),
        })
    }
    /// Queues composition only for the original still-live editor destination.
    ///
    /// # Errors
    /// Rejects malformed text or a full 64-operation queue. Stale targets return false.
    pub fn dispatch(&self, target: &Target, edit: &Edit) -> io::Result<bool> {
        edit.validate()?;
        let mut bound = self.state.bound.lock().expect("composition mutex");
        if !Arc::ptr_eq(&self.state, &target.state)
            || self.state.stopped.load(Ordering::Acquire)
            || bound.tag.is_none()
            || bound.generation != target.generation
            || !bound.snapshot.as_ref().is_some_and(|(s, at)| {
                s.editor == target.context.0
                    && s.reset == target.context.1
                    && edit.matches(s.document.as_ref())
                    && at.elapsed() <= Duration::from_millis(500)
            })
        {
            return Ok(false);
        }
        if let Some(last) = bound.jobs.back_mut()
            && last.operation.editor == target.context.0
            && last.operation.reset == target.context.1
            && last.operation.edit.commit.is_none()
            && edit.commit.is_none()
            && !last.operation.edit.absolute()
            && !edit.absolute()
            && last.operation.edit.revision == edit.revision
        {
            last.operation.edit = edit.clone();
            last.end = Instant::now() + BUDGET;
            return Ok(true);
        }
        if bound.jobs.len() >= INPUT_CAPACITY {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        bound.jobs.push_back(Job {
            operation: Operation {
                editor: target.context.0,
                reset: target.context.1,
                edit: edit.clone(),
            },
            end: Instant::now() + BUDGET,
        });
        Ok(true)
    }
    /// Permanently revokes the channel without waiting for its worker.
    pub fn close(&self) {
        self.state.stop();
    }
}
/// Cancels and joins private composition I/O on the process supervisor.
pub struct Service {
    state: Arc<State>,
    worker: Option<JoinHandle<()>>,
}
impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositionService").finish_non_exhaustive()
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
fn serve(listener: LocalListener, owner: &[u8; 16], state: &State) -> io::Result<()> {
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
    if !matches!(local::receive::<Message>(&mut stream,1024,end)?, Message::Hello { version:VERSION, owner: got } if vivid_protocol::auth::verify_tag(owner,&got))
    {
        return Err(invalid());
    }
    local::send(&mut stream, &Message::Ready { version: VERSION }, 1024, end)?;
    let mut id = 1u64;
    while !state.stopped.load(Ordering::Acquire) {
        let (generation, tag, edit) = {
            let mut bound = state.bound.lock().expect("composition mutex");
            bound.jobs.retain(|job| Instant::now() < job.end);
            (
                bound.generation,
                bound.tag,
                bound.jobs.pop_front().map(|j| j.operation),
            )
        };
        let end = Instant::now() + BUDGET;
        local::send(
            &mut stream,
            &Message::Tick {
                id,
                generation,
                binding: tag,
                edit,
            },
            MAX_FRAME,
            end,
        )?;
        let Message::Snapshot {
            id: got,
            generation: scope,
            binding: reply_tag,
            editor,
        } = local::receive(&mut stream, MAX_FRAME, end)?
        else {
            return Err(invalid());
        };
        if got != id
            || scope != generation
            || reply_tag != tag
            || editor.as_ref().is_some_and(|s: &Native| !s.valid())
        {
            return Err(invalid());
        }
        let mut bound = state.bound.lock().expect("composition mutex");
        if bound.generation == generation && bound.tag == tag {
            if let Some(context) = &editor {
                if context.editor < bound.floor.0 || context.reset < bound.floor.1 {
                    return Err(invalid());
                }
                if let Some(old) = &bound.last_editor
                    && old.editor == context.editor
                    && old.reset == context.reset
                    && (old.document.is_some() != context.document.is_some()
                        || old
                            .document
                            .as_ref()
                            .zip(context.document.as_ref())
                            .is_some_and(|(a, b)| {
                                a.sensitive != b.sensitive
                                    || b.revision < a.revision
                                    || (a.revision == b.revision && a != b)
                            }))
                {
                    return Err(invalid());
                }
                bound.floor = (context.editor, context.reset);
                // Retain one bounded context across temporary ineligibility so
                // an absent snapshot cannot reset revision or export-policy checks.
                bound.last_editor = Some(context.clone());
            }
            bound.snapshot = tag.and(editor).map(|s| (s, Instant::now()));
        }
        drop(bound);
        id = id.checked_add(1).ok_or_else(invalid)?;
        thread::sleep(POLL);
    }
    Ok(())
}
