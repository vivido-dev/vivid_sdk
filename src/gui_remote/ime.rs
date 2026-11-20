//! Private process composition, pinned to authenticated input and native editor grants.

// Rust guideline compliant 2026-10-07
mod accessibility;
mod producer;
pub use accessibility::{AccessibilityTree, TreeAction};
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
const VERSION: u32 = 11;
const ENDPOINT: &str = "VIVID_GUI_IME_ENDPOINT";
const OWNER: &str = "VIVID_GUI_IME_OWNER";
const POLL: Duration = Duration::from_millis(10);
const BUDGET: Duration = Duration::from_secs(1);
const STARTUP: Duration = Duration::from_secs(15);
// Each of two bounded strings may use six JSON bytes per UTF-8 byte.
const MAX_EDITOR_FRAME: usize = MAX_TEXT_BYTES * 12 + 2048;
const MAX_FRAME: usize = MAX_EDITOR_FRAME + accessibility::MAX_ENCODED_BYTES;
const MAX_TICK: usize = MAX_EDITOR_FRAME + keyboard::MAX_LAYOUT_BYTES + INPUT_CAPACITY * 64;
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
    /// Standalone excerpt UTF-8 selection as (fixed anchor, active cursor).
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
                .is_some_and(|(a, b)| a.max(b) as usize > MAX_TEXT_BYTES)
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
        layout: Option<Mapping>,
        repeat: Option<KeyRepeat>,
        keyboard: Vec<KeyboardContext>,
        edit: Option<Operation>,
        request_tree: bool,
        tree_action: Option<AssistRequest>,
    },
    Snapshot {
        id: u64,
        generation: u64,
        binding: Option<Binding>,
        editor: Option<Native>,
        tree: TreePublication,
        keyboard_credit: usize,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TreePublication {
    Empty,
    Retained,
    Published(Arc<AccessibilityTree>),
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(super) enum Mapping {
    #[default]
    Launch,
    Host(Option<Arc<CharacterLayout>>),
    HostWithLocks(Option<Arc<CharacterLayout>>),
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyboardContext {
    key: Option<(u16, bool)>,
    caps_lock: bool,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssistRequest {
    node: u64,
    action: TreeAction,
    deadline_ms: u64,
}
impl AssistRequest {
    fn now_ms() -> Option<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis()
            .try_into()
            .ok()
    }
    fn remaining(self) -> Option<Duration> {
        let remaining = Duration::from_millis(self.deadline_ms.checked_sub(Self::now_ms()?)?);
        (!remaining.is_zero() && remaining <= BUDGET).then_some(remaining)
    }
}
struct AssistJob {
    tree: Arc<AccessibilityTree>,
    request: AssistRequest,
    end: Instant,
}
struct Bound {
    assist: VecDeque<AssistJob>,
    request_tree: bool,
    accessible_tree: Option<(Arc<AccessibilityTree>, Instant)>,
    layout: Mapping,
    repeat: Option<KeyRepeat>,
    keyboard: VecDeque<KeyboardContext>,
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
            assist: VecDeque::new(),
            request_tree: false,
            accessible_tree: None,
            layout: Mapping::Launch,
            repeat: None,
            keyboard: VecDeque::new(),
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
        bound.assist.clear();
        bound.accessible_tree = None;
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
        self.bind_mapping(tag, Mapping::Launch, None, None);
    }

    /// Pins the local host keyboard layout to a new input grant.
    /// A missing character map selects the SDK's US fallback.
    pub fn set_binding_with_layout(
        &self,
        tag: Option<crate::InputTuple>,
        layout: Option<Arc<CharacterLayout>>,
    ) {
        self.bind_mapping(tag, Mapping::Host(layout), None, None);
    }

    /// Pins a keyboard layout and the host window's current Caps Lock state to a new grant.
    pub fn set_binding_with_lock_state(
        &self,
        tag: Option<crate::InputTuple>,
        layout: Option<Arc<CharacterLayout>>,
        caps_lock: bool,
    ) {
        self.bind_mapping(tag, Mapping::HostWithLocks(layout), Some(caps_lock), None);
    }

    /// Supplies the presenter's current layout, lock state and repeat policy.
    /// Policy changes take effect on a held key without replacing its input grant.
    pub fn set_binding_with_keyboard_policy(
        &self,
        tag: Option<crate::InputTuple>,
        layout: Option<Arc<CharacterLayout>>,
        caps_lock: bool,
        repeat: KeyRepeat,
    ) {
        self.bind_mapping(
            tag,
            Mapping::HostWithLocks(layout),
            Some(caps_lock),
            Some(repeat),
        );
    }

    /// Records the lock state at a physical report, or a standalone lock change.
    /// The child matches key records against its authenticated input lane.
    ///
    /// # Errors
    /// Returns an error if the bounded per-grant queue is full or the usage is invalid.
    pub fn record_lock_state(
        &self,
        tag: crate::InputTuple,
        key: Option<(u16, bool)>,
        caps_lock: bool,
    ) -> io::Result<()> {
        let mut bound = self.state.bound.lock().expect("composition mutex");
        if bound.tag != Some(binding(tag)) || !matches!(bound.layout, Mapping::HostWithLocks(_)) {
            return Ok(());
        }
        if key.is_some_and(|(usage, _)| !(4..=231).contains(&usage)) {
            return Err(invalid());
        }
        if bound.keyboard.len() >= INPUT_CAPACITY {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        bound.keyboard.push_back(KeyboardContext { key, caps_lock });
        Ok(())
    }

    fn bind_mapping(
        &self,
        tag: Option<crate::InputTuple>,
        layout: Mapping,
        caps_lock: Option<bool>,
        repeat: Option<KeyRepeat>,
    ) {
        let mut bound = self.state.bound.lock().expect("composition mutex");
        let tag = tag.map(binding);
        bound.repeat = tag.and(repeat);
        if bound.tag != tag {
            let Some(next) = bound.generation.checked_add(1) else {
                drop(bound);
                self.close();
                return;
            };
            bound.request_tree = false;
            bound.generation = next;
            bound.tag = tag;
            bound.layout = layout;
            bound.keyboard.clear();
            if tag.is_some()
                && let Some(caps_lock) = caps_lock
            {
                bound.keyboard.push_back(KeyboardContext {
                    key: None,
                    caps_lock,
                });
            }
            bound.snapshot = None;
            bound.assist.clear();
            bound.accessible_tree = None;
            bound.last_editor = None;
            bound.jobs.clear();
        }
    }
    /// Requests the accessible subtree for one live presented keyboard
    /// grant. Passing false releases interest and drops its cached content.
    pub fn accessibility_tree(
        &self,
        tag: Option<crate::InputTuple>,
        requested: bool,
    ) -> Option<Arc<AccessibilityTree>> {
        let mut bound = self.state.bound.lock().expect("composition mutex");
        let live = requested
            && tag.is_some()
            && bound.tag == tag.map(binding)
            && !self.state.stopped.load(Ordering::Acquire);
        bound.request_tree = live;
        if !live {
            bound.assist.clear();
            bound.accessible_tree = None;
            return None;
        }
        bound
            .accessible_tree
            .as_ref()
            .filter(|(_, at)| at.elapsed() <= Duration::from_millis(500))
            .map(|(value, _)| value.clone())
    }
    /// Queues an assistive action for this owner's exact live tree publication.
    pub fn request_accessibility_action(
        &self,
        tag: crate::InputTuple,
        tree: &Arc<AccessibilityTree>,
        node: u64,
        action: TreeAction,
    ) -> bool {
        let mut bound = self.state.bound.lock().expect("composition mutex");
        if self.state.stopped.load(Ordering::Acquire)
            || !bound.request_tree
            || bound.tag != Some(binding(tag))
            || !bound.accessible_tree.as_ref().is_some_and(|(live, at)| {
                Arc::ptr_eq(live, tree) && at.elapsed() <= Duration::from_millis(500)
            })
            || !tree.supports(node, action)
            || bound.assist.len() >= INPUT_CAPACITY
        {
            return false;
        }
        let Some(deadline_ms) =
            AssistRequest::now_ms().and_then(|ms| ms.checked_add(BUDGET.as_millis() as u64))
        else {
            return false;
        };
        bound.assist.push_back(AssistJob {
            tree: tree.clone(),
            request: AssistRequest {
                node,
                action,
                deadline_ms,
            },
            end: Instant::now() + BUDGET,
        });
        true
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
        self.dispatch_bound(target, edit, None)
    }

    /// Queues a selection only while the original assistive tree and editor are both live.
    ///
    /// # Errors
    /// Returns an error for malformed edits or a full composition queue.
    pub fn dispatch_accessible_selection(
        &self,
        target: &Target,
        edit: &Edit,
        tree: &Arc<AccessibilityTree>,
    ) -> io::Result<bool> {
        if edit.document_selection.is_none() {
            return Ok(false);
        }
        self.dispatch_bound(target, edit, Some(tree))
    }

    fn dispatch_bound(
        &self,
        target: &Target,
        edit: &Edit,
        tree: Option<&Arc<AccessibilityTree>>,
    ) -> io::Result<bool> {
        edit.validate()?;
        let mut bound = self.state.bound.lock().expect("composition mutex");
        if let Some(tree) = tree
            && (!bound.request_tree
                || !tree.allows_editor_excerpt()
                || !bound.accessible_tree.as_ref().is_some_and(|(live, at)| {
                    Arc::ptr_eq(live, tree) && at.elapsed() <= Duration::from_millis(500)
                }))
        {
            return Ok(false);
        }
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
    let mut sent_generation = None;
    let mut previous_tree = None;
    let mut credit = INPUT_CAPACITY;
    while !state.stopped.load(Ordering::Acquire) {
        let (generation, tag, layout, repeat, keyboard, edit, request_tree, tree_action) = {
            let mut bound = state.bound.lock().expect("composition mutex");
            bound.jobs.retain(|job| Instant::now() < job.end);
            if sent_generation != Some(bound.generation) {
                credit = INPUT_CAPACITY;
                previous_tree = None;
            }
            bound.assist.retain(|job| {
                job.end > Instant::now()
                    && previous_tree
                        .as_ref()
                        .is_some_and(|tree| Arc::ptr_eq(tree, &job.tree))
            });
            let count = bound.keyboard.len().min(credit);
            (
                bound.generation,
                bound.tag,
                (bound.tag.is_some() && sent_generation != Some(bound.generation))
                    .then(|| bound.layout.clone()),
                bound.repeat,
                bound.keyboard.drain(..count).collect(),
                bound.jobs.pop_front().map(|j| j.operation),
                bound.request_tree,
                bound.assist.pop_front().map(|job| job.request),
            )
        };
        sent_generation = Some(generation);
        let end = Instant::now() + BUDGET;
        local::send(
            &mut stream,
            &Message::Tick {
                id,
                generation,
                binding: tag,
                layout,
                repeat,
                keyboard,
                edit,
                request_tree,
                tree_action,
            },
            MAX_TICK,
            end,
        )?;
        let Message::Snapshot {
            id: got,
            generation: scope,
            binding: reply_tag,
            editor,
            tree,
            keyboard_credit,
        } = local::receive(&mut stream, MAX_FRAME, end)?
        else {
            return Err(invalid());
        };
        if got != id
            || scope != generation
            || keyboard_credit > INPUT_CAPACITY
            || (!matches!(tree, TreePublication::Empty) && !request_tree)
            || reply_tag != tag
            || editor.as_ref().is_some_and(|s: &Native| !s.valid())
        {
            return Err(invalid());
        }
        match tree {
            TreePublication::Empty => previous_tree = None,
            TreePublication::Retained if previous_tree.is_none() => return Err(invalid()),
            TreePublication::Retained => (),
            TreePublication::Published(fresh) => previous_tree = Some(fresh),
        }
        let mut bound = state.bound.lock().expect("composition mutex");
        credit = keyboard_credit;
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
            bound.accessible_tree = bound
                .request_tree
                .then(|| previous_tree.clone())
                .flatten()
                .map(|v| (v, Instant::now()));
            bound.snapshot = tag.and(editor).map(|s| (s, Instant::now()));
        }
        drop(bound);
        id = id.checked_add(1).ok_or_else(invalid)?;
        thread::sleep(POLL);
    }
    Ok(())
}
