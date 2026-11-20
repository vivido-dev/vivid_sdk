//! Bounded open and save requests for a local raster child.

// Rust guideline compliant 2026-10-07
use super::*;
use futures_channel::oneshot;

pub(super) const ENV: &str = "VIVID_GUI_PATH_PANELS";
const PANEL_TIMEOUT: Duration = Duration::from_secs(600);
const PANEL_POLL: Duration = Duration::from_millis(50);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Options {
    Open {
        files: bool,
        directories: bool,
        multiple: bool,
        prompt: Option<String>,
    },
    Save {
        directory: String,
        name: Option<String>,
    },
}

impl Options {
    fn valid(&self) -> bool {
        let label = |s: &str| s.len() <= 256 && !s.chars().any(char::is_control);
        match self {
            Self::Open {
                files,
                directories,
                prompt,
                ..
            } => (*files || *directories) && prompt.as_deref().is_none_or(label),
            Self::Save { directory, name } => {
                directory.len() <= 4096
                    && !directory.contains('\0')
                    && Path::new(directory).is_absolute()
                    && name
                        .as_deref()
                        .is_none_or(|s| label(s) && Path::new(s).file_name() == Some(s.as_ref()))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Request {
    Start { binding: Binding, options: Options },
    Poll(u64),
    Cancel(u64),
}

#[derive(Serialize, Deserialize)]
pub(super) enum Answer {
    Pending,
    Closed,
    Selected(String),
    Refused,
}

pub(super) struct Panel {
    id: u64,
    end: Instant,
    // Queued panels must still have their originating grant when shown.
    queued: Option<(Arc<Binding>, Instant, Options)>,
    answer: Option<Answer>,
}

pub(super) fn handle(bound: &mut Bound, id: u64, request: Request) -> Answer {
    let now = Instant::now();
    if bound.picker.as_ref().is_some_and(|p| now >= p.end) {
        bound.picker = None;
    }
    match request {
        Request::Start { binding, options } => {
            if !bound.pickers_enabled || bound.picker.is_some() || !options.valid() {
                return Answer::Refused;
            }
            let Some(tag) = bound
                .binding
                .as_ref()
                .filter(|tag| tag.as_ref() == &binding)
            else {
                return Answer::Refused;
            };
            let Some(deadline) = bound.web_gesture.take().filter(|end| now < *end) else {
                return Answer::Refused;
            };
            bound.picker = Some(Panel {
                id,
                end: now + PANEL_TIMEOUT,
                queued: Some((tag.clone(), deadline, options)),
                answer: None,
            });
            Answer::Pending
        }
        Request::Poll(token) => {
            let Some(panel) = bound.picker.as_mut().filter(|p| p.id == token) else {
                return Answer::Refused;
            };
            if let Some(answer) = panel.answer.take() {
                bound.picker = None;
                answer
            } else {
                Answer::Pending
            }
        }
        Request::Cancel(token) => {
            if bound.picker.as_ref().is_some_and(|p| p.id == token) {
                bound.picker = None;
            }
            Answer::Closed
        }
    }
}

impl Host {
    /// Enables presenter-owned file dialogs for this child on the private channel.
    pub fn enable_path_panels(&self, command: &mut Command) {
        self.state
            .bound
            .lock()
            .expect("clipboard owner mutex")
            .pickers_enabled = true;
        command.env(ENV, "1");
    }
    /// Dismisses the retiring child session's panel and discards its reply.
    pub fn retire_path_panel(&self) {
        self.state
            .bound
            .lock()
            .expect("clipboard owner mutex")
            .picker = None;
    }
}

type Selection<T> = oneshot::Receiver<gui::Result<Option<T>>>;
enum Native {
    Open(Selection<Vec<PathBuf>>),
    Save(Selection<PathBuf>),
}
impl Native {
    fn answer(&mut self) -> Option<Answer> {
        let result = match self {
            Self::Open(rx) => rx.try_recv().transpose()?,
            Self::Save(rx) => rx
                .try_recv()
                .transpose()?
                .map(|r| r.map(|p| p.map(|p| vec![p]))),
        };
        Some(match result {
            Ok(Ok(Some(paths))) => bridge::FileList::from_paths(&paths)
                .map_or(Answer::Refused, |list| {
                    Answer::Selected(list.into_uri_list())
                }),
            Ok(Ok(None)) => Answer::Closed,
            _ => Answer::Refused,
        })
    }
}

/// Drives one raster child's native file panels on the presenter's GUI thread.
#[derive(Default)]
pub struct PathPanels {
    active: Option<(Arc<State>, u64, Native)>,
}
impl PathPanels {
    /// Presents a gesture-qualified request and hands its result to its original session.
    pub fn poll(&mut self, host: &Host, current: Option<crate::InputTuple>, cx: &App) {
        let mut bound = host.state.bound.lock().expect("clipboard owner mutex");
        let grant = bound.binding.clone();
        let Some(panel) = bound
            .picker
            .as_mut()
            .filter(|p| !host.state.stopped.load(Ordering::Acquire) && Instant::now() < p.end)
        else {
            self.active = None;
            return;
        };
        if let Some((owner, id, native)) = &mut self.active {
            if Arc::ptr_eq(owner, &host.state) && *id == panel.id {
                if let Some(answer) = native.answer() {
                    panel.answer = Some(answer);
                    self.active = None;
                }
                return;
            }
            self.active = None;
        }
        let Some((tag, deadline, options)) = panel.queued.take() else {
            return;
        };
        if Instant::now() >= deadline
            || !grant.is_some_and(|g| Arc::ptr_eq(&g, &tag))
            || current.map(binding).as_ref() != Some(&*tag)
        {
            panel.answer = Some(Answer::Refused);
            return;
        }
        let id = panel.id;
        drop(bound);
        let native = match options {
            Options::Open {
                files,
                directories,
                multiple,
                prompt,
            } => Native::Open(cx.prompt_for_paths(gui::PathPromptOptions {
                files,
                directories,
                multiple,
                prompt: prompt.map(Into::into),
            })),
            Options::Save { directory, name } => {
                Native::Save(cx.prompt_for_new_path(Path::new(&directory), name.as_deref()))
            }
        };
        self.active = Some((host.state.clone(), id, native));
    }
}

enum Reply {
    Open {
        multiple: bool,
        tx: oneshot::Sender<gui::Result<Option<Vec<PathBuf>>>>,
    },
    Save(oneshot::Sender<gui::Result<Option<PathBuf>>>),
}
impl Reply {
    fn cancelled(&self) -> bool {
        match self {
            Self::Open { tx, .. } => tx.is_canceled(),
            Self::Save(tx) => tx.is_canceled(),
        }
    }
    fn finish(self, result: io::Result<Option<Vec<PathBuf>>>) {
        match self {
            Self::Open { multiple, tx } => {
                let result = result.and_then(|paths| {
                    if paths
                        .as_ref()
                        .is_some_and(|p| p.is_empty() || (!multiple && p.len() != 1))
                    {
                        Err(invalid())
                    } else {
                        Ok(paths)
                    }
                });
                let _ = tx.send(result.map_err(Into::into));
            }
            Self::Save(tx) => {
                let result = result.and_then(|paths| match paths {
                    Some(mut paths) if paths.len() == 1 => Ok(paths.pop()),
                    Some(_) => Err(invalid()),
                    None => Ok(None),
                });
                let _ = tx.send(result.map_err(Into::into));
            }
        }
    }
}
struct Job {
    grant: Arc<Grant>,
    options: Option<Options>,
    reply: Reply,
    _permit: ResourcePermit,
    end: Instant,
}
pub(super) struct Client {
    sender: mpsc::SyncSender<Job>,
    budget: ResourceBudget,
}
pub(super) struct Worker {
    jobs: mpsc::Receiver<Job>,
    active: Option<(u64, Job)>,
    next: Instant,
}
pub(super) fn channel(enabled: bool) -> (Option<Client>, Option<Worker>) {
    if !enabled {
        return (None, None);
    }
    let (sender, jobs) = mpsc::sync_channel(1);
    (
        Some(Client {
            sender,
            budget: ResourceBudget::new(1),
        }),
        Some(Worker {
            jobs,
            active: None,
            next: Instant::now(),
        }),
    )
}

impl Producer {
    fn queue_panel(&self, options: Options, reply: Reply) {
        let result = (|| {
            if !options.valid() {
                return Err(invalid());
            }
            let client = self.picker.as_ref().ok_or(io::ErrorKind::Unsupported)?;
            let grant = self.gate.capture().ok_or(io::ErrorKind::PermissionDenied)?;
            Ok((client, grant, client.budget.try_acquire()?))
        })();
        match result {
            Ok((client, grant, permit)) => {
                let job = Job {
                    grant,
                    options: Some(options),
                    reply,
                    _permit: permit,
                    end: Instant::now() + PANEL_TIMEOUT,
                };
                if let Err(error) = client.sender.try_send(job) {
                    let job = match error {
                        mpsc::TrySendError::Full(j) | mpsc::TrySendError::Disconnected(j) => j,
                    };
                    job.reply.finish(Err(io::ErrorKind::NotConnected.into()));
                }
            }
            Err(error) => reply.finish(Err(error)),
        }
    }
}
impl gui::HostedPathPrompts for Producer {
    fn open(&self, options: gui::PathPromptOptions) -> Selection<Vec<PathBuf>> {
        let (tx, rx) = oneshot::channel();
        let reply = Reply::Open {
            multiple: options.multiple,
            tx,
        };
        if options.prompt.as_ref().is_some_and(|s| s.len() > 256) {
            reply.finish(Err(invalid()));
        } else {
            self.queue_panel(
                Options::Open {
                    files: options.files,
                    directories: options.directories,
                    multiple: options.multiple,
                    prompt: options.prompt.map(|p| p.to_string()),
                },
                reply,
            );
        }
        rx
    }
    fn save(&self, directory: &Path, name: Option<&str>) -> Selection<PathBuf> {
        let (tx, rx) = oneshot::channel();
        if let Some(path) = directory.to_str().filter(|p| p.len() <= 4096)
            && name.is_none_or(|n| n.len() <= 256)
        {
            self.queue_panel(
                Options::Save {
                    directory: path.to_owned(),
                    name: name.map(str::to_owned),
                },
                Reply::Save(tx),
            );
        } else {
            Reply::Save(tx).finish(Err(invalid()));
        }
        rx
    }
}

impl Worker {
    pub(super) fn pump(&mut self, stream: &mut LocalStream, id: &mut u64) -> io::Result<()> {
        let (token, mut job) = if self.active.is_some() && Instant::now() < self.next {
            return Ok(());
        } else if let Some(active) = self.active.take() {
            active
        } else if let Ok(job) = self.jobs.try_recv() {
            (*id, job)
        } else {
            return Ok(());
        };
        let live = || {
            job.grant
                .lifecycle
                .upgrade()
                .is_some_and(|s| !s.closed.load(Ordering::Acquire))
        };
        let cancelled = job.reply.cancelled() || !live() || Instant::now() >= job.end;
        let request = if let Some(options) = job.options.take() {
            if cancelled || !job.grant.live() {
                job.reply.finish(Err(io::ErrorKind::Interrupted.into()));
                return Ok(());
            }
            Request::Start {
                binding: job.grant.tag,
                options,
            }
        } else if cancelled {
            Request::Cancel(token)
        } else {
            Request::Poll(token)
        };
        let deadline = Instant::now() + BUDGET;
        local::send(
            stream,
            &Message::Picker { id: *id, request },
            MAX_FRAME,
            deadline,
        )?;
        let Message::PickerReply { id: got, answer } = local::receive(stream, MAX_FRAME, deadline)?
        else {
            return Err(invalid());
        };
        if got != *id {
            return Err(invalid());
        }
        *id = id.checked_add(1).ok_or_else(invalid)?;
        match answer {
            Answer::Pending if !cancelled => {
                // If the session closed during this exchange, the next turn
                // explicitly retires the outstanding request on the host.
                self.active = Some((token, job));
                self.next = Instant::now() + PANEL_POLL;
            }
            _ if cancelled || !live() => job.reply.finish(Err(io::ErrorKind::Interrupted.into())),
            Answer::Closed => job.reply.finish(Ok(None)),
            Answer::Selected(uris) => job.reply.finish(
                bridge::FileList::from_uri_list(&uris)
                    .and_then(|list| list.paths())
                    .map(Some),
            ),
            Answer::Refused => job
                .reply
                .finish(Err(io::ErrorKind::PermissionDenied.into())),
            Answer::Pending => return Err(invalid()),
        }
        Ok(())
    }
}
