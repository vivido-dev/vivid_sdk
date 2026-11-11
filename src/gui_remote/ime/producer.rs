//! GUI-thread admission and worker-owned transport for private native composition.

// Rust guideline compliant 2026-10-07
use super::*;
#[cfg(test)]
mod tests;
use crate::gui::{App, HostedTextTarget, Window};

pub(crate) struct Producer {
    state: Arc<State>,
    configured: bool,
    target: Option<HostedTextTarget>,
    editor: u64,
    generation: u64,
    reset: u64,
    tag: Option<Binding>,
    marked: bool,
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
                target: None,
                editor: 0,
                generation: 0,
                reset: 0,
                tag: None,
                marked: false,
            },
            service,
        ))
    }
    pub(in crate::gui_remote) fn enabled(&self) -> bool {
        self.configured
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut App) {
        if self.marked
            && let Some(target) = &self.target
        {
            window.dispatch_hosted_composition(target, None, "", None, cx);
        }
        self.marked = false;
        self.target = None;
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
        if !live
            || self.generation != bound.generation
            || self.reset != input.reset
            || self.tag != tag
        {
            self.cancel(window, cx);
            self.generation = bound.generation;
            self.reset = input.reset;
            self.tag = tag;
            bound.jobs.clear();
            bound.snapshot = None;
            if !live {
                return Ok(());
            }
        }
        let current = window.hosted_text_state(cx);
        if !self
            .target
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(a, b)| a.same_editor(&b.target))
        {
            self.cancel(window, cx);
            bound.jobs.clear();
            bound.snapshot = None;
            self.target = current.map(|s| s.target);
            if self.target.is_some() {
                self.editor = self.editor.checked_add(1).ok_or_else(invalid)?;
            }
        }
        for _ in 0..INPUT_CAPACITY {
            let Some(job) = bound.jobs.pop_front() else {
                break;
            };
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
            let selection = operation
                .edit
                .selection
                .map(|(a, b)| a as usize..b as usize);
            if window.dispatch_hosted_composition(
                target,
                operation.edit.commit.as_deref(),
                &operation.edit.preedit,
                selection,
                cx,
            ) {
                self.marked = !operation.edit.preedit.is_empty();
            }
        }
        let current = window.hosted_text_state(cx);
        // A callback may have changed fields. Never publish the old target's
        // identity with another field's geometry; the next turn installs it.
        bound.snapshot = current
            .filter(|s| {
                self.target
                    .as_ref()
                    .is_some_and(|old| old.same_editor(&s.target))
            })
            .map(|s| {
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
    let mut expected = 1u64;
    while !state.stopped.load(Ordering::Acquire) {
        let message = match local::receive_idle(&mut stream, MAX_FRAME, BUDGET)? {
            Some(message) => message,
            None => continue,
        };
        let Message::Tick {
            id,
            generation,
            binding,
            edit,
        } = message
        else {
            return Err(invalid());
        };
        if id != expected || generation == 0 || binding.is_some_and(|tag| tag.contains(&0)) {
            return Err(invalid());
        }
        expected = expected.checked_add(1).ok_or_else(invalid)?;
        let editor = {
            let mut bound = state.bound.lock().expect("composition mutex");
            if generation < bound.generation
                || (generation == bound.generation && binding != bound.tag)
            {
                return Err(invalid());
            }
            if generation != bound.generation || binding != bound.tag {
                bound.generation = generation;
                bound.tag = binding;
                bound.snapshot = None;
                bound.jobs.clear();
            }
            if let Some(operation) = edit {
                operation.edit.validate()?;
                if binding.is_some()
                    && bound.snapshot.is_some_and(|(s, at)| {
                        s.editor == operation.editor
                            && s.reset == operation.reset
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
            bound
                .snapshot
                .filter(|(_, at)| at.elapsed() <= Duration::from_millis(500))
                .map(|(s, _)| s)
        };
        local::send(
            &mut stream,
            &Message::Snapshot {
                id,
                generation,
                binding,
                editor,
            },
            MAX_FRAME,
            Instant::now() + BUDGET,
        )?;
    }
    Ok(())
}
