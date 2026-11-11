//! Bounded, nonblocking native selection transfers with scoped platform authority.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::gui_host::clipboard as platform;
use smithay::{
    input::Seat,
    reexports::wayland_server::{Client, DisplayHandle, backend::ClientId},
    utils::Serial,
    wayland::selection::{
        SelectionSource, SelectionTarget,
        data_device::{DataDeviceState, set_data_device_focus, set_data_device_selection},
        primary_selection::{PrimarySelectionState, set_primary_focus, set_primary_selection},
    },
};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    os::{fd::OwnedFd, unix::net::UnixStream},
    time::Duration,
};

const MAX_TRANSFERS: usize = 4;
const IO_BUDGET: usize = 16 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_SERIALS: usize = 16;
const SERIAL_LIFETIME: Duration = Duration::from_secs(5);
pub(super) const MIME_TYPES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "UTF8_STRING",
    "text/plain",
];
const FILE_MIME: &str = "text/uri-list";
enum Pending {
    Text(platform::PendingRead),
    Files(platform::PendingFileRead),
}
impl Pending {
    fn poll(&mut self) -> Option<io::Result<String>> {
        match self {
            Self::Text(read) => read.poll(),
            Self::Files(read) => read
                .poll()
                .map(|result| result.map(platform::FileList::into_uri_list)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    epoch: u64,
    target: Target,
    revision: u64,
    kind: platform::Target,
}
struct Focus {
    owner: ClientId,
    target: Target,
    _scope: platform::Scope,
    client: platform::Client,
}
#[derive(Default)]
struct Buffer {
    revision: u64,
    scope: Option<(platform::Scope, platform::Client)>,
    publish: bool,
}
enum Transfer {
    Copy {
        reader: UnixStream,
        bytes: Vec<u8>,
        files: bool,
    },
    Paste {
        writer: File,
        pending: Option<Pending>,
        bytes: Option<String>,
        offset: usize,
    },
}
struct Job {
    stamp: Stamp,
    deadline: Instant,
    transfer: Transfer,
}
pub(super) struct Clipboard {
    pub(super) device: DataDeviceState,
    pub(super) primary: Option<PrimarySelectionState>,
    display: DisplayHandle,
    base: platform::Client,
    focus: Option<Focus>,
    epoch: u64,
    buffers: [Buffer; 2],
    serials: Vec<(Serial, Instant)>,
    jobs: Vec<Job>,
    files_available: bool,
}
impl Clipboard {
    pub(super) fn new(display: &DisplayHandle, client: platform::Client) -> Self {
        let files_available = client.file_list_available();
        Self {
            device: DataDeviceState::new::<state::State>(display),
            primary: client
                .has_primary()
                .then(|| PrimarySelectionState::new::<state::State>(display)),
            display: display.clone(),
            base: client,
            focus: None,
            epoch: 0,
            buffers: Default::default(),
            serials: Vec::new(),
            jobs: Vec::new(),
            files_available,
        }
    }
    pub(super) fn focus(
        &mut self,
        seat: &Seat<state::State>,
        client: Option<Client>,
        target: Option<Target>,
    ) {
        if self
            .focus
            .as_ref()
            .map(|focus| (focus.owner.clone(), focus.target))
            == client
                .as_ref()
                .zip(target)
                .map(|(client, target)| (client.id(), target))
        {
            return;
        }
        self.revoke();
        // Remove the old client first: it must never receive a new owner's offer.
        set_data_device_focus(&self.display, seat, None);
        if self.primary.is_some() {
            set_primary_focus(&self.display, seat, None);
        }
        let Some(epoch) = self.epoch.checked_add(1) else {
            return;
        };
        self.epoch = epoch;
        if let (Some(client), Some(target)) = (client, target)
            && let Ok((scope, scoped)) = self.base.scoped()
        {
            self.focus = Some(Focus {
                owner: client.id(),
                target,
                _scope: scope,
                client: scoped,
            });
            self.renew_buffer(platform::Target::Clipboard);
            if self.primary.is_some() {
                self.renew_buffer(platform::Target::Primary);
            }
            self.publish(seat);
            set_data_device_focus(&self.display, seat, Some(client.clone()));
            if self.primary.is_some() {
                set_primary_focus(&self.display, seat, Some(client));
            }
        }
    }
    fn revoke(&mut self) {
        self.focus.take();
        for buffer in &mut self.buffers {
            buffer.scope.take();
            buffer.publish = false;
        }
        self.jobs.clear();
        self.serials.clear();
    }
    pub(super) fn note_serial(&mut self, serial: Serial) {
        if self.focus.is_none() {
            return;
        }
        self.serials
            .retain(|(_, at)| at.elapsed() <= SERIAL_LIFETIME);
        if self.serials.len() >= MAX_SERIALS {
            self.serials.remove(0);
        }
        self.serials.push((serial, Instant::now()));
    }
    pub(super) fn allow_selection(
        &self,
        client: &Client,
        serial: Serial,
        current: Option<Target>,
    ) -> bool {
        self.focus
            .as_ref()
            .is_some_and(|focus| focus.owner == client.id() && Some(focus.target) == current)
            && self
                .serials
                .iter()
                .any(|(known, at)| *known == serial && at.elapsed() <= SERIAL_LIFETIME)
    }
    fn renew_buffer(&mut self, kind: platform::Target) {
        let buffer = &mut self.buffers[index(kind)];
        buffer.scope.take();
        buffer.publish = false;
        self.jobs.retain(|job| job.stamp.kind != kind);
        let Some(revision) = buffer.revision.checked_add(1) else {
            return;
        };
        buffer.revision = revision;
        buffer.scope = self
            .focus
            .as_ref()
            .and_then(|focus| focus.client.scoped().ok());
        buffer.publish = buffer.scope.is_some();
    }
    fn stamp(&self, kind: platform::Target) -> Option<Stamp> {
        let focus = self.focus.as_ref()?;
        let buffer = &self.buffers[index(kind)];
        buffer.scope.as_ref()?;
        Some(Stamp {
            epoch: self.epoch,
            target: focus.target,
            revision: buffer.revision,
            kind,
        })
    }
    fn current(&self, stamp: Stamp) -> bool {
        self.stamp(stamp.kind) == Some(stamp)
    }
    pub(super) fn selected(&mut self, kind: SelectionTarget, source: Option<SelectionSource>) {
        let kind = target(kind);
        if self.stamp(kind).is_none() {
            return;
        }
        self.renew_buffer(kind);
        // Do not cancel the source before its requested bytes have arrived.
        self.buffers[index(kind)].publish = false;
        let Some(source) = source else {
            if let Some((_, client)) = &self.buffers[index(kind)].scope {
                let _ = client.write(kind, String::new());
            }
            self.buffers[index(kind)].publish = true;
            return;
        };
        let types = source.mime_types();
        let files = kind == platform::Target::Clipboard
            && self.base.has_file_lists()
            && types.iter().any(|mime| mime == FILE_MIME);
        let Some(mime) = (if files {
            Some(FILE_MIME)
        } else {
            MIME_TYPES
                .iter()
                .find(|mime| types.iter().any(|offered| offered == **mime))
                .copied()
        }) else {
            self.buffers[index(kind)].publish = true;
            return;
        };
        if self.jobs.len() >= MAX_TRANSFERS {
            self.buffers[index(kind)].publish = true;
            return;
        }
        if let Ok((reader, writer)) = UnixStream::pair()
            && reader.set_nonblocking(true).is_ok()
            && source.request_data(mime.to_string(), writer.into())
            && let Some(stamp) = self.stamp(kind)
        {
            self.jobs.push(Job {
                stamp,
                deadline: Instant::now() + TRANSFER_TIMEOUT,
                transfer: Transfer::Copy {
                    reader,
                    bytes: Vec::new(),
                    files,
                },
            });
        } else {
            self.buffers[index(kind)].publish = true;
        }
    }
    pub(super) fn send(&mut self, stamp: Stamp, mime: &str, fd: OwnedFd, kind: SelectionTarget) {
        let files = mime == FILE_MIME
            && self.base.file_list_available()
            && stamp.kind == platform::Target::Clipboard;
        if !self.current(stamp)
            || stamp.kind != target(kind)
            || (!files && !MIME_TYPES.contains(&mime))
            || self.jobs.len() >= MAX_TRANSFERS
        {
            return;
        }
        let Ok(writer) = nonblocking_writer(fd) else {
            return;
        };
        let Some((_, client)) = &self.buffers[index(stamp.kind)].scope else {
            return;
        };
        let pending = if files {
            client.begin_read_files().map(Pending::Files)
        } else {
            client.begin_read(stamp.kind).map(Pending::Text)
        };
        let Ok(pending) = pending else {
            return;
        };
        self.jobs.push(Job {
            stamp,
            deadline: Instant::now() + TRANSFER_TIMEOUT,
            transfer: Transfer::Paste {
                writer,
                pending: Some(pending),
                bytes: None,
                offset: 0,
            },
        });
    }
    pub(super) fn poll(&mut self, seat: &Seat<state::State>) {
        let files_available = self.base.file_list_available();
        if self.files_available != files_available {
            self.files_available = files_available;
            // A withdrawn file offer must not regain authority if files return.
            self.renew_buffer(platform::Target::Clipboard);
        }
        let mut jobs = std::mem::take(&mut self.jobs);
        for mut job in jobs.drain(..) {
            if !self.current(job.stamp) {
                continue;
            }
            if Instant::now() >= job.deadline {
                if matches!(job.transfer, Transfer::Copy { .. }) {
                    self.buffers[index(job.stamp.kind)].publish = true;
                }
                continue;
            }
            let complete = match &mut job.transfer {
                Transfer::Copy {
                    reader,
                    bytes,
                    files,
                } => {
                    let mut chunk = [0; IO_BUDGET];
                    match reader.read(&mut chunk) {
                        Ok(0) => {
                            if let Ok(text) = String::from_utf8(std::mem::take(bytes))
                                && let Some((_, client)) =
                                    &self.buffers[index(job.stamp.kind)].scope
                            {
                                if *files {
                                    if let Ok(files) = platform::FileList::from_uri_list(&text) {
                                        let _ = client.write_files(files);
                                    }
                                } else {
                                    let _ = client.write(job.stamp.kind, text);
                                }
                            }
                            self.buffers[index(job.stamp.kind)].publish = true;
                            true
                        }
                        Ok(count) => {
                            if bytes
                                .len()
                                .checked_add(count)
                                .is_none_or(|size| size > platform::MAX_TEXT_BYTES)
                            {
                                true
                            } else {
                                bytes.extend_from_slice(&chunk[..count]);
                                false
                            }
                        }
                        Err(error) => {
                            error.kind() != io::ErrorKind::WouldBlock
                                && error.kind() != io::ErrorKind::Interrupted
                        }
                    }
                }
                Transfer::Paste {
                    writer,
                    pending,
                    bytes,
                    offset,
                } => {
                    if let Some(read) = pending.as_mut()
                        && let Some(result) = read.poll()
                    {
                        pending.take();
                        match result {
                            Ok(text) => *bytes = Some(text),
                            Err(_) => continue,
                        }
                    }
                    if let Some(text) = bytes {
                        let end = offset.saturating_add(IO_BUDGET).min(text.len());
                        match writer.write(&text.as_bytes()[*offset..end]) {
                            Ok(0) => true,
                            Ok(count) => {
                                *offset += count;
                                *offset == text.len()
                            }
                            Err(error) => {
                                error.kind() != io::ErrorKind::WouldBlock
                                    && error.kind() != io::ErrorKind::Interrupted
                            }
                        }
                    } else {
                        false
                    }
                }
            };
            if !complete {
                self.jobs.push(job);
            } else if matches!(job.transfer, Transfer::Copy { .. }) {
                self.buffers[index(job.stamp.kind)].publish = true;
            }
        }
        self.publish(seat);
    }
    fn publish(&mut self, seat: &Seat<state::State>) {
        for kind in [platform::Target::Clipboard, platform::Target::Primary] {
            let buffer = &mut self.buffers[index(kind)];
            if !buffer.publish {
                continue;
            }
            buffer.publish = false;
            if let Some(stamp) = self.stamp(kind) {
                let mut types: Vec<String> =
                    MIME_TYPES.iter().map(|mime| (*mime).to_string()).collect();
                if kind == platform::Target::Clipboard && self.base.file_list_available() {
                    types.push(FILE_MIME.into());
                }
                match kind {
                    platform::Target::Clipboard => {
                        set_data_device_selection(&self.display, seat, types, stamp)
                    }
                    platform::Target::Primary => {
                        set_primary_selection(&self.display, seat, types, stamp)
                    }
                }
            }
        }
    }
}
fn nonblocking_writer(fd: OwnedFd) -> io::Result<File> {
    use rustix::fs::{FileType, OFlags, fcntl_getfl, fcntl_setfl, fstat};
    let stat = fstat(&fd)?;
    if !matches!(
        FileType::from_raw_mode(stat.st_mode),
        FileType::Fifo | FileType::Socket
    ) {
        return Err(invalid("selection output must be a pipe or socket"));
    }
    let flags = fcntl_getfl(&fd)?;
    fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;
    Ok(File::from(fd))
}
pub(super) fn target(kind: SelectionTarget) -> platform::Target {
    match kind {
        SelectionTarget::Clipboard => platform::Target::Clipboard,
        SelectionTarget::Primary => platform::Target::Primary,
    }
}
fn index(kind: platform::Target) -> usize {
    match kind {
        platform::Target::Clipboard => 0,
        platform::Target::Primary => 1,
    }
}

impl state::State {
    pub(super) fn clipboard_focus(
        &mut self,
        seat: &Seat<Self>,
        focus: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    ) {
        let target = focus.and(self.target());
        if let Some(clipboard) = &mut self.clipboard {
            clipboard.focus(seat, focus.and_then(|surface| surface.client()), target);
        }
    }
}
impl smithay::wayland::selection::SelectionHandler for state::State {
    type SelectionUserData = Stamp;
    fn allow_selection(
        &mut self,
        _: SelectionTarget,
        client: &Client,
        seat: &Seat<Self>,
        serial: Serial,
    ) -> bool {
        seat == &self.seat
            && self
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.allow_selection(client, serial, self.target()))
    }
    fn allow_mime_type(&mut self, mime: &str, offered: usize) -> bool {
        offered < 32
            && !mime.is_empty()
            && mime.len() <= 128
            && mime.is_ascii()
            && !mime.bytes().any(|byte| byte.is_ascii_control())
    }
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        seat: Seat<Self>,
    ) {
        if seat == self.seat
            && let Some(clipboard) = &mut self.clipboard
        {
            clipboard.selected(ty, source);
        }
    }
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        seat: Seat<Self>,
        user_data: &Stamp,
    ) {
        if seat == self.seat
            && self.target() == Some(user_data.target)
            && let Some(clipboard) = &mut self.clipboard
        {
            clipboard.send(*user_data, &mime_type, fd, ty);
        }
    }
}
impl smithay::wayland::selection::data_device::DataDeviceHandler for state::State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self
            .clipboard
            .as_ref()
            .expect("registered clipboard backend")
            .device
    }
    fn allow_drag(&mut self, _: &Client, _: &Seat<Self>, _: Serial) -> bool {
        false
    }
}
impl smithay::wayland::selection::data_device::ClientDndGrabHandler for state::State {}
impl smithay::wayland::selection::data_device::ServerDndGrabHandler for state::State {}
impl smithay::wayland::selection::primary_selection::PrimarySelectionHandler for state::State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        self.clipboard
            .as_ref()
            .and_then(|clipboard| clipboard.primary.as_ref())
            .expect("registered primary-selection backend")
    }
}
smithay::delegate_data_device!(state::State);
smithay::delegate_primary_selection!(state::State);
