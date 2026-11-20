//! GUI-thread admission and worker-owned transport for private native composition.

// Rust guideline compliant 2026-10-07
use super::*;
#[cfg(test)]
mod tests;
use crate::gui::{App, HostedTextContext, HostedTextEdit, HostedTextTarget, Window};

pub(crate) struct Producer {
    state: Arc<State>,
    configured: bool,
    accessible_tree: Option<std::rc::Rc<std::cell::RefCell<Option<Arc<AccessibilityTree>>>>>,
    target: Option<HostedTextTarget>,
    editor: u64,
    generation: u64,
    reset: u64,
    tag: Option<Binding>,
    marked: bool,
    document: Option<HostedTextContext>,
    revision: u64,
}
impl Producer {
    pub(in crate::gui_remote) fn from_env() -> io::Result<(Self, Option<Service>)> {
        let endpoint = std::env::var_os(ENDPOINT);
        let owner = std::env::var_os(OWNER);
        let state = Arc::new(State::default());
        let configured = endpoint.is_some() || owner.is_some();
        let service = match (endpoint, owner) {
            (None, None) => None,
            (Some(endpoint), Some(owner)) => {
                let owner = zeroize::Zeroizing::new(owner.into_string().map_err(|_| invalid())?);
                if owner.len() != 32 {
                    return Err(invalid());
                }
                let mut secret = zeroize::Zeroizing::new([0; 16]);
                for (out, hex) in secret.iter_mut().zip(owner.as_bytes().as_chunks::<2>().0) {
                    *out = u8::from_str_radix(std::str::from_utf8(hex).map_err(|_| invalid())?, 16)
                        .map_err(|_| invalid())?;
                }
                let shared = state.clone();
                let worker = thread::Builder::new()
                    .name("vivid-gui-ime-app".into())
                    .spawn(move || {
                        let _ = connected(&PathBuf::from(endpoint), &secret, &shared);
                        shared.stop();
                    })?;
                Some(Service {
                    state: state.clone(),
                    worker: Some(worker),
                })
            }
            _ => return Err(invalid()),
        };
        Ok((
            Self {
                state,
                configured,
                accessible_tree: None,
                target: None,
                editor: 0,
                generation: 0,
                reset: 0,
                tag: None,
                marked: false,
                document: None,
                revision: 0,
            },
            service,
        ))
    }
    pub(in crate::gui_remote) fn enabled(&self) -> bool {
        self.configured
    }

    pub(in crate::gui_remote) fn mapping(
        &self,
        tag: crate::InputTuple,
    ) -> Option<(Mapping, Option<KeyRepeat>)> {
        if !self.configured {
            return Some((Mapping::Launch, None));
        }
        let bound = self.state.bound.lock().expect("composition mutex");
        (!self.state.stopped.load(Ordering::Acquire) && bound.tag == Some(binding(tag)))
            .then(|| (bound.layout.clone(), bound.repeat))
    }

    // Consume lock changes only up to the next physical report. Metadata alone cannot generate a key.
    pub(in crate::gui_remote) fn lock_state(
        &self,
        tag: crate::InputTuple,
        key: Option<(u16, bool)>,
    ) -> io::Result<(Option<bool>, bool)> {
        let mut bound = self.state.bound.lock().expect("composition mutex");
        if self.state.stopped.load(Ordering::Acquire) || bound.tag != Some(binding(tag)) {
            return Ok((None, false));
        }
        let mut caps = None;
        while bound
            .keyboard
            .front()
            .is_some_and(|context| context.key.is_none())
        {
            caps = bound.keyboard.pop_front().map(|context| context.caps_lock);
        }
        let Some(key) = key else {
            return Ok((caps, true));
        };
        let Some(context) = bound.keyboard.pop_front() else {
            return Ok((caps, false));
        };
        if context.key != Some(key) {
            return Err(invalid());
        }
        Ok((Some(context.caps_lock), true))
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut App) {
        if self.marked
            && let Some(target) = &self.target
        {
            window.dispatch_hosted_composition(target, None, "", None, cx);
        }
        self.marked = false;
        self.target = None;
        self.document = None;
        self.revision = 0;
    }
    fn observe(&mut self, document: Option<HostedTextContext>) -> io::Result<()> {
        if self.document != document {
            self.revision = self.revision.checked_add(1).ok_or_else(invalid)?;
            self.document = document;
        }
        Ok(())
    }
    fn snapshot(&self) -> Option<Document> {
        self.document.as_ref().map(|d| Document {
            revision: self.revision,
            text: d.text.clone(),
            cursor: d.cursor,
            anchor: d.anchor,
            sensitive: d.sensitive,
        })
    }
    pub(in crate::gui_remote) fn pump(
        &mut self,
        input: &Injection,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<()> {
        if !self.configured {
            return Ok(());
        }
        if self.state.stopped.load(Ordering::Acquire) {
            self.cancel(window, cx);
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let state = self.state.clone();
        let mut bound = state.bound.lock().expect("composition mutex");
        let tag = input.guard.current_tag().map(binding);
        let live = input.guard.is_armed(input.now())
            && input
                .guard
                .grant()
                .is_some_and(|g| g.effective_classes & crate::INPUT_CLASS_KEYBOARD != 0)
            && input
                .lifecycle
                .as_ref()
                .is_some_and(|s| !s.closed.load(Ordering::Acquire))
            && tag.is_some()
            && bound.tag == tag;
        let changed =
            self.generation != bound.generation || self.reset != input.reset || self.tag != tag;
        if (!live || !bound.request_tree || changed) && self.accessible_tree.take().is_some() {
            window.stop_observing_hosted_accessibility();
        }
        if live && bound.request_tree && self.accessible_tree.is_none() {
            let accessible_tree = std::rc::Rc::new(std::cell::RefCell::new(None));
            let latest = accessible_tree.clone();
            let allow_click = input
                .guard
                .grant()
                .is_some_and(|g| g.effective_classes & crate::INPUT_CLASS_POINTER_BUTTON != 0);
            window.observe_hosted_accessibility(move |tree, size| {
                let captured = AccessibilityTree::capture(tree, size, allow_click);
                let mut old = latest.borrow_mut();
                if old.as_deref() != captured.as_ref() {
                    *old = captured.map(Arc::new);
                }
            });
            self.accessible_tree = Some(accessible_tree);
        }
        bound.accessible_tree = self
            .accessible_tree
            .as_ref()
            .and_then(|latest| latest.borrow().clone())
            .map(|v| (v, Instant::now()));
        if !live || changed {
            self.cancel(window, cx);
            self.generation = bound.generation;
            self.reset = input.reset;
            self.tag = tag;
            bound.jobs.clear();
            bound.snapshot = None;
            if !live {
                bound.assist.clear();
                return Ok(());
            }
        }
        if changed || !bound.request_tree {
            bound.assist.clear();
        }
        if let Some(job) = bound.assist.pop_front() {
            // Render intervening app changes before comparing the original publication.
            if window.needs_draw() {
                window.draw(cx).clear(cx);
            }
            let request = job.request;
            if job.end > Instant::now()
                && request.remaining().is_some()
                && !state.stopped.load(Ordering::Acquire)
                && input
                    .lifecycle
                    .as_ref()
                    .is_some_and(|s| !s.closed.load(Ordering::Acquire))
                && input.guard.is_armed(input.now())
                && input.guard.grant().is_some_and(|grant| {
                    !matches!(request.action, TreeAction::Click)
                        || grant.effective_classes & crate::INPUT_CLASS_POINTER_BUTTON != 0
                })
                && self.accessible_tree.as_ref().is_some_and(|slot| {
                    slot.borrow()
                        .as_ref()
                        .is_some_and(|tree| Arc::ptr_eq(tree, &job.tree))
                })
            {
                window.dispatch_hosted_accessibility_action(
                    crate::gui::accesskit::NodeId(request.node),
                    request.action.native(),
                    cx,
                );
            }
        }
        let current = window.hosted_text_state(cx);
        if !self
            .target
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(a, b)| {
                a.same_editor(&b.target)
                    && self.document.as_ref().map(|d| d.sensitive)
                        == b.document.as_ref().map(|d| d.sensitive)
            })
        {
            self.cancel(window, cx);
            bound.jobs.clear();
            bound.snapshot = None;
            self.target = current.as_ref().map(|s| s.target.clone());
            if self.target.is_some() {
                self.editor = self.editor.checked_add(1).ok_or_else(invalid)?;
            }
        }
        self.observe(current.and_then(|s| s.document))?;
        for _ in 0..INPUT_CAPACITY {
            let Some(job) = bound.jobs.pop_front() else {
                break;
            };
            let current = window.hosted_text_state(cx);
            if !self
                .target
                .as_ref()
                .zip(current.as_ref())
                .is_some_and(|(a, b)| {
                    a.same_editor(&b.target)
                        && self.document.as_ref().map(|d| d.sensitive)
                            == b.document.as_ref().map(|d| d.sensitive)
                })
            {
                self.cancel(window, cx);
                bound.jobs.clear();
                bound.snapshot = None;
                break;
            }
            self.observe(current.and_then(|s| s.document))?;
            let Some(target) = &self.target else {
                continue;
            };
            let operation = job.operation;
            if Instant::now() >= job.end
                || operation.editor != self.editor
                || operation.reset != input.reset
            {
                continue;
            }
            operation.edit.validate()?;
            if !operation.edit.matches(self.snapshot().as_ref()) {
                continue;
            }
            let edit = operation.edit;
            let selection = edit.selection.map(|(a, b)| a as usize..b as usize);
            let applied = if edit.absolute() {
                let Some(document) = &self.document else {
                    continue;
                };
                if let Some((anchor, cursor)) = edit.document_selection {
                    window.dispatch_hosted_selection(
                        target,
                        document,
                        anchor as usize,
                        cursor as usize,
                        cx,
                    )
                } else {
                    window.dispatch_hosted_document(
                        target,
                        document,
                        &HostedTextEdit {
                            commit: edit.commit.as_deref(),
                            preedit: &edit.preedit,
                            selection,
                            replacement: edit.replacement.map(|(a, b)| a as usize..b as usize),
                            document_selection: None,
                        },
                        cx,
                    )
                }
            } else {
                window.dispatch_hosted_composition(
                    target,
                    edit.commit.as_deref(),
                    &edit.preedit,
                    selection,
                    cx,
                )
            };
            if applied {
                self.marked = !edit.preedit.is_empty();
            }
        }
        // A callback may change focus or export policy. Do not publish its new
        // contents under the old editor; the next pump retires that incarnation.
        let current = window.hosted_text_state(cx).filter(|s| {
            self.target
                .as_ref()
                .is_some_and(|old| old.same_editor(&s.target))
                && self.document.as_ref().map(|d| d.sensitive)
                    == s.document.as_ref().map(|d| d.sensitive)
        });
        if let Some(current) = &current {
            self.observe(current.document.clone())?;
        }
        bound.snapshot = current.map(|s| {
            let size = window.viewport_size();
            let w = f64::from(f32::from(size.width));
            let h = f64::from(f32::from(size.height));
            let caret = s.caret.and_then(|r| {
                let x = f64::from(f32::from(r.origin.x));
                let y = f64::from(f32::from(r.origin.y));
                let rw = f64::from(f32::from(r.size.width));
                let rh = f64::from(f32::from(r.size.height));
                if ![x, y, rw, rh, w, h].iter().all(|n| n.is_finite())
                    || w <= 0.
                    || h <= 0.
                    || rw < 0.
                    || rh < 0.
                {
                    return None;
                }
                let left = x.clamp(0., w);
                let top = y.clamp(0., h);
                Some([
                    left / w,
                    top / h,
                    ((x + rw).clamp(left, w) - left) / w,
                    ((y + rh).clamp(top, h) - top) / h,
                ])
            });
            (
                Native {
                    editor: self.editor,
                    reset: input.reset,
                    caret,
                    document: self.snapshot(),
                },
                Instant::now(),
            )
        });
        Ok(())
    }
}

fn connected(endpoint: &Path, secret: &[u8; 16], state: &State) -> io::Result<()> {
    let mut stream = LocalStream::connect(endpoint)?;
    state.install(&stream)?;
    let end = Instant::now() + BUDGET;
    local::send(
        &mut stream,
        &Message::Hello {
            version: VERSION,
            owner: *secret,
        },
        1024,
        end,
    )?;
    if !matches!(
        local::receive::<Message>(&mut stream, 1024, end)?,
        Message::Ready { version: VERSION }
    ) {
        return Err(invalid());
    }
    let mut expected = 1u64;
    let mut previous_tree: Option<Arc<AccessibilityTree>> = None;
    while !state.stopped.load(Ordering::Acquire) {
        let message = match local::receive_idle(&mut stream, MAX_TICK, BUDGET)? {
            Some(message) => message,
            None => continue,
        };
        let Message::Tick {
            id,
            generation,
            binding,
            layout,
            repeat,
            keyboard,
            edit,
            request_tree,
            tree_action,
        } = message
        else {
            return Err(invalid());
        };
        if id != expected || generation == 0 || binding.is_some_and(|tag| tag.contains(&0)) {
            return Err(invalid());
        }
        expected = expected.checked_add(1).ok_or_else(invalid)?;
        let (editor, keyboard_credit, accessible_tree) = {
            let mut bound = state.bound.lock().expect("composition mutex");
            if generation < bound.generation
                || (generation == bound.generation && binding != bound.tag)
            {
                return Err(invalid());
            }
            let changed = generation != bound.generation || binding != bound.tag;
            if layout.is_some() != (changed && binding.is_some()) {
                return Err(invalid());
            }
            if let Some(Mapping::Host(Some(map)) | Mapping::HostWithLocks(Some(map))) = &layout {
                map.encode()?;
            }
            if (request_tree || repeat.is_some()) && binding.is_none() {
                return Err(invalid());
            }
            if changed || !request_tree {
                bound.assist.clear();
                previous_tree = None;
                bound.accessible_tree = None;
            }
            if let Some(request) = tree_action
                && let Some(tree) = &previous_tree
                && tree.supports(request.node, request.action)
                && let Some(remaining) = request.remaining()
                && bound.assist.len() < INPUT_CAPACITY
            {
                bound.assist.push_back(AssistJob {
                    tree: tree.clone(),
                    request,
                    end: Instant::now() + remaining,
                });
            }
            bound.request_tree = request_tree;
            bound.repeat = repeat;
            if changed {
                bound.layout = layout.unwrap_or_default();
                bound.keyboard.clear();
                bound.generation = generation;
                bound.tag = binding;
                bound.snapshot = None;
                bound.jobs.clear();
            }
            if !keyboard.is_empty() {
                if binding.is_none()
                    || !matches!(bound.layout, Mapping::HostWithLocks(_))
                    || keyboard.len() > INPUT_CAPACITY.saturating_sub(bound.keyboard.len())
                    || keyboard.iter().any(|context| {
                        context
                            .key
                            .is_some_and(|(usage, _)| !(4..=231).contains(&usage))
                    })
                {
                    return Err(invalid());
                }
                bound.keyboard.extend(keyboard);
            }
            if let Some(operation) = edit {
                operation.edit.validate()?;
                if binding.is_some()
                    && bound.snapshot.as_ref().is_some_and(|(s, at)| {
                        s.editor == operation.editor
                            && s.reset == operation.reset
                            && operation.edit.matches(s.document.as_ref())
                            && at.elapsed() <= Duration::from_millis(500)
                    })
                {
                    if bound.jobs.len() >= INPUT_CAPACITY {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    bound.jobs.push_back(Job {
                        operation,
                        end: Instant::now() + BUDGET,
                    });
                }
            }
            let editor = bound
                .snapshot
                .as_ref()
                .filter(|(_, at)| at.elapsed() <= Duration::from_millis(500))
                .map(|(s, _)| s.clone());
            (
                editor,
                INPUT_CAPACITY - bound.keyboard.len(),
                bound
                    .accessible_tree
                    .as_ref()
                    .filter(|(_, at)| at.elapsed() <= Duration::from_millis(500))
                    .map(|(v, _)| v.clone()),
            )
        };
        let tree = match (&previous_tree, &accessible_tree) {
            (_, None) => TreePublication::Empty,
            (Some(old), Some(new)) if Arc::ptr_eq(old, new) => TreePublication::Retained,
            (_, Some(new)) => TreePublication::Published(new.clone()),
        };
        previous_tree = accessible_tree;
        local::send(
            &mut stream,
            &Message::Snapshot {
                id,
                generation,
                binding,
                editor,
                keyboard_credit,
                tree,
            },
            MAX_FRAME,
            Instant::now() + BUDGET,
        )?;
    }
    Ok(())
}
