//! Native clipboard clients and a controlled platform-side service.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{fs::File, io::Read as _, os::fd::OwnedFd};
use vivid_sdk::gui_host::clipboard as platform;
use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1 as primary_manager,
    zwp_primary_selection_device_v1 as primary_device,
    zwp_primary_selection_offer_v1 as primary_offer,
    zwp_primary_selection_source_v1 as primary_source,
};

pub(super) const HOST_TEXT: &str = "host clipboard λ 👋";
pub(super) const PRIMARY_TEXT: &str = "host primary β";
pub(super) const NATIVE_TEXT: &str = "native selection 世界";
const MIME: &str = "text/plain;charset=utf-8";
const FILE_MIME: &str = "text/uri-list";
pub(super) const FILE_URIS: &str = "file:///tmp/vivid%20%23%CE%BB.txt\r\nfile:///tmp/second\r\n";
#[derive(Debug, Default)]
enum FileHandshake {
    #[default]
    InitialOffer,
    Files,
    Text,
    CopiedFiles,
    Done,
}
#[derive(Default)]
pub(super) struct ClipboardFixture {
    manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    primary: Option<primary_manager::ZwpPrimarySelectionDeviceManagerV1>,
    device: Option<wl_data_device::WlDataDevice>,
    primary_device: Option<primary_device::ZwpPrimarySelectionDeviceV1>,
    offer: Option<wl_data_offer::WlDataOffer>,
    file_offer: Option<wl_data_offer::WlDataOffer>,
    file_handshake: FileHandshake,
    late_bind: bool,
    late_ready: bool,
    primary_offer: Option<primary_offer::ZwpPrimarySelectionOfferV1>,
    stale: Option<wl_data_offer::WlDataOffer>,
    serial: u32,
    held: Vec<OwnedFd>,
}
struct Platform {
    text: [String; 2],
    reads: usize,
    writes: usize,
    files: platform::FileList,
    file_reads: usize,
    file_writes: usize,
}
impl Platform {
    fn new() -> Self {
        Self {
            text: [HOST_TEXT.into(), PRIMARY_TEXT.into()],
            reads: 0,
            writes: 0,
            files: platform::FileList::from_uri_list(FILE_URIS).unwrap(),
            file_reads: 0,
            file_writes: 0,
        }
    }
    fn service(&mut self, host: &platform::Host) {
        while let Some(request) = host.next_request() {
            if request.format() == platform::Format::Files {
                assert_eq!(request.target(), platform::Target::Clipboard);
                if let Some(files) = request.write_files() {
                    let _ = request.lease().while_live(|| {
                        self.files = files.clone();
                        self.file_writes += 1;
                    });
                    request.complete(Ok(String::new()));
                } else {
                    self.file_reads += 1;
                    request.complete(Ok(self.files.as_uri_list().to_owned()));
                }
                continue;
            }
            let index = match request.target() {
                platform::Target::Clipboard => 0,
                platform::Target::Primary => 1,
            };
            if let Some(text) = request.write_text() {
                let lease = request.lease();
                let _ = lease.while_live(|| {
                    self.text[index] = text.to_owned();
                    self.writes += 1;
                });
                request.complete(Ok(String::new()));
            } else {
                self.reads += 1;
                request.complete(Ok(self.text[index].clone()));
            }
        }
    }
}
fn start() -> (WaylandHost, platform::Host, Platform, Client, Frame) {
    start_with_files(false)
}
fn start_with_files(files: bool) -> (WaylandHost, platform::Host, Platform, Client, Frame) {
    start_options(files, false)
}
fn start_options(
    files: bool,
    late: bool,
) -> (WaylandHost, platform::Host, Platform, Client, Frame) {
    let (platform, client_side) = if files {
        platform::Host::with_file_lists(true)
    } else {
        platform::Host::new(true)
    };
    platform.set_file_list_available(files);
    platform.set_active(true);
    let mut host = WaylandHost::with_clipboard(64, 48, client_side).unwrap();
    let child = if late {
        client_with(&host, &[("VIVID_TREE_LATE_CLIPBOARD", "1")])
    } else {
        client(&host)
    };
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    (host, platform, Platform::new(), child, frame)
}

#[test]
fn data_devices_created_after_focus_receive_current_offers_without_refocusing_or_touching_peers() {
    let (mut host, platform, mut service, _child, initial) = start_options(true, true);
    let (mut peer, peer_platform, mut peer_service, _peer, other) = start_with_files(true);
    assert_eq!(initial.id, other.id, "owners should reuse local frame IDs");
    assert_ne!(
        initial.target, other.target,
        "native owners must be distinct"
    );
    key(&mut host, initial.target, 103);
    let ready = wait(&mut host, &platform, &mut service, [222, 0, 3, 255]);
    present(&mut host, &ready);
    key(&mut host, ready.target, 215);
    let files = wait(
        &mut host,
        &platform,
        &mut service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut host, &files);
    key(&mut host, files.target, 92);
    let primary = wait(
        &mut host,
        &platform,
        &mut service,
        [220, PRIMARY_TEXT.len() as u8, 2, 255],
    );
    present(&mut host, &primary);
    assert_eq!((service.file_reads, service.reads), (1, 1));
    key(&mut peer, other.target, 215);
    let survivor = wait(
        &mut peer,
        &peer_platform,
        &mut peer_service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut peer, &survivor);
    assert_eq!(peer_service.file_reads, 1);
}

#[test]
fn activation_before_first_frame_defers_focus_and_clipboard_until_presentation() {
    let (platform, client_side) = platform::Host::with_file_lists(true);
    platform.set_file_list_available(true);
    platform.set_active(true);
    let mut host = WaylandHost::with_clipboard(64, 48, client_side).unwrap();
    let mut service = Platform::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("first-frame.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let _child = client_with(
        &host,
        &[("VIVID_TREE_FIRST_FRAME_GATE", path.to_str().unwrap())],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut gate = loop {
        assert!(
            host.dispatch().unwrap().is_none(),
            "client painted before the gate"
        );
        match listener.accept() {
            Ok((gate, _)) => break gate,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(error) => panic!("first-frame gate failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "client did not configure before its first frame"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    gate.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut ready = [0];
    gate.read_exact(&mut ready).unwrap();
    assert!(host.input_target().is_none());
    host.set_active(true);
    gate.write_all(&[1]).unwrap();
    let first = next(&mut host);
    assert!(host.input_target().is_none());
    present(&mut host, &first);
    key(&mut host, first.target, 215);
    let pasted = wait(
        &mut host,
        &platform,
        &mut service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut host, &pasted);
    let (mut peer, peer_platform, mut peer_service, _peer, other) = start_with_files(true);
    assert_eq!(first.id, other.id, "owners should reuse local frame IDs");
    assert_ne!(first.target, other.target, "native owners must be distinct");
    key(&mut peer, other.target, 215);
    let survivor = wait(
        &mut peer,
        &peer_platform,
        &mut peer_service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut peer, &survivor);
    assert_eq!((service.file_reads, peer_service.file_reads), (1, 1));
}

#[test]
fn native_file_lists_round_trip_without_changing_text_or_primary_selection() {
    let (mut host, platform, mut service, _child, initial) = start_with_files(true);
    key(&mut host, initial.target, 215);
    let pasted = wait(
        &mut host,
        &platform,
        &mut service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut host, &pasted);
    service.files = platform::FileList::from_uri_list("file:///tmp/previous\r\n").unwrap();
    key(&mut host, pasted.target, 216);
    let copied = wait(&mut host, &platform, &mut service, [216, 20, 30, 255]);
    present(&mut host, &copied);
    pump(&mut host, &platform, &mut service, 30);
    assert_eq!(service.files.as_uri_list(), FILE_URIS);
    assert_eq!((service.file_reads, service.file_writes), (1, 1));
    assert_eq!((service.reads, service.writes), (0, 0));
    key(&mut host, copied.target, 92);
    let primary = wait(
        &mut host,
        &platform,
        &mut service,
        [220, PRIMARY_TEXT.len() as u8, 2, 255],
    );
    present(&mut host, &primary);
    assert_eq!(service.text, [HOST_TEXT, PRIMARY_TEXT]);
    platform.set_file_list_available(false);
    pump(&mut host, &platform, &mut service, 30);
    key(&mut host, primary.target, 89);
    let text_only = wait(&mut host, &platform, &mut service, [89, 20, 30, 255]);
    present(&mut host, &text_only);
}

#[test]
fn invalid_native_file_lists_leave_platform_data_and_peer_owners_usable() {
    let (mut host, platform, mut service, _child, initial) = start_with_files(true);
    let (mut peer, peer_platform, mut peer_service, _peer, other) = start_with_files(true);
    assert_eq!(initial.id, other.id, "owners should reuse local frame IDs");
    assert_ne!(
        initial.target, other.target,
        "native owners must be distinct"
    );
    for code in [217, 218] {
        key(&mut host, initial.target, code);
        let rejected = wait(
            &mut host,
            &platform,
            &mut service,
            [code as u8, 20, 30, 255],
        );
        present(&mut host, &rejected);
        pump(
            &mut host,
            &platform,
            &mut service,
            if code == 218 { 600 } else { 30 },
        );
        assert_eq!(service.file_writes, 0);
        assert_eq!(service.files.as_uri_list(), FILE_URIS);
    }
    key(&mut peer, other.target, 215);
    let survivor = wait(
        &mut peer,
        &peer_platform,
        &mut peer_service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut peer, &survivor);
    assert_eq!(peer_service.file_reads, 1);
    key(&mut host, initial.target, 215);
    let restored = wait(
        &mut host,
        &platform,
        &mut service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut host, &restored);
}

#[test]
fn stale_native_file_offers_cannot_read_a_new_focus_scope() {
    let (mut host, platform, mut service, _child, initial) = start_with_files(true);
    key(&mut host, initial.target, 97);
    let saved = wait(&mut host, &platform, &mut service, [97, 20, 30, 255]);
    present(&mut host, &saved);
    host.set_active(false);
    host.set_active(true);
    key(&mut host, saved.target, 219);
    let denied = wait(&mut host, &platform, &mut service, [220, 0, 5, 255]);
    present(&mut host, &denied);
    assert_eq!((service.file_reads, service.file_writes), (0, 0));
}

#[test]
fn withdrawn_file_offers_stay_revoked_when_files_return_and_a_peer_owner_survives() {
    let (mut host, platform, mut service, _child, initial) = start_with_files(true);
    let (mut peer, peer_platform, mut peer_service, _peer, other) = start_with_files(true);
    assert_eq!(initial.id, other.id, "owners should reuse local frame IDs");
    assert_ne!(
        initial.target, other.target,
        "native owners must be distinct"
    );
    key(&mut host, initial.target, 97);
    let saved = wait(&mut host, &platform, &mut service, [97, 20, 30, 255]);
    present(&mut host, &saved);
    platform.set_file_list_available(false);
    pump(&mut host, &platform, &mut service, 30);
    platform.set_file_list_available(true);
    pump(&mut host, &platform, &mut service, 30);
    key(&mut host, saved.target, 219);
    let stale = wait(&mut host, &platform, &mut service, [220, 0, 5, 255]);
    present(&mut host, &stale);
    assert_eq!(service.file_reads, 0);
    key(&mut peer, other.target, 215);
    let survivor = wait(
        &mut peer,
        &peer_platform,
        &mut peer_service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut peer, &survivor);
    assert_eq!(peer_service.file_reads, 1);
    key(&mut host, stale.target, 215);
    let fresh = wait(
        &mut host,
        &platform,
        &mut service,
        [220, FILE_URIS.len() as u8, 4, 255],
    );
    present(&mut host, &fresh);
    assert_eq!(service.file_reads, 1);
}
fn wait(
    host: &mut WaylandHost,
    platform: &platform::Host,
    service: &mut Platform,
    color: [u8; 4],
) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            if pixel(&frame, 0, 0) == color {
                return frame;
            }
            present(host, &frame);
        }
        service.service(platform);
        assert!(
            Instant::now() < deadline,
            "native clipboard pixels timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn pump(host: &mut WaylandHost, platform: &platform::Host, service: &mut Platform, turns: usize) {
    for _ in 0..turns {
        if let Some(frame) = host.dispatch().unwrap() {
            present(host, &frame);
        }
        service.service(platform);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn native_clipboard_and_primary_transfer_exact_utf8_through_the_platform_service() {
    let (mut host, platform, mut service, _child, initial) = start();
    key(&mut host, initial.target, 90);
    let pasted = wait(
        &mut host,
        &platform,
        &mut service,
        [220, HOST_TEXT.len() as u8, 1, 255],
    );
    present(&mut host, &pasted);
    key(&mut host, pasted.target, 92);
    let primary = wait(
        &mut host,
        &platform,
        &mut service,
        [220, PRIMARY_TEXT.len() as u8, 2, 255],
    );
    present(&mut host, &primary);
    key(&mut host, primary.target, 91);
    let copied = wait(&mut host, &platform, &mut service, [91, 20, 30, 255]);
    present(&mut host, &copied);
    pump(&mut host, &platform, &mut service, 30);
    assert_eq!(service.text[0], NATIVE_TEXT);
    key(&mut host, copied.target, 93);
    let selected = wait(&mut host, &platform, &mut service, [93, 20, 30, 255]);
    present(&mut host, &selected);
    pump(&mut host, &platform, &mut service, 30);
    assert_eq!(service.text[1], NATIVE_TEXT);
    assert_eq!(service.reads, 2);
    assert_eq!(service.writes, 2);
}

#[test]
fn stale_native_offers_and_selection_serials_cannot_reuse_a_new_focus_scope() {
    let (mut host, platform, mut service, _child, initial) = start();
    key(&mut host, initial.target, 97);
    let saved = wait(&mut host, &platform, &mut service, [97, 20, 30, 255]);
    present(&mut host, &saved);
    host.set_active(false);
    host.set_active(true);
    key(&mut host, saved.target, 98);
    let stale = wait(&mut host, &platform, &mut service, [220, 0, 3, 255]);
    present(&mut host, &stale);
    key(&mut host, stale.target, 99);
    let denied = wait(&mut host, &platform, &mut service, [99, 20, 30, 255]);
    present(&mut host, &denied);
    pump(&mut host, &platform, &mut service, 20);
    assert_eq!((service.reads, service.writes), (0, 0));
    assert_eq!(service.text[0], HOST_TEXT);
}

#[test]
fn stalled_invalid_and_oversized_native_sources_do_not_block_input_or_update_the_platform() {
    for code in [94, 95, 96] {
        let (mut host, platform, mut service, _child, initial) = start();
        key(&mut host, initial.target, code);
        let copied = wait(
            &mut host,
            &platform,
            &mut service,
            [code as u8, 20, 30, 255],
        );
        present(&mut host, &copied);
        pump(
            &mut host,
            &platform,
            &mut service,
            if code == 94 {
                600
            } else if code == 96 {
                180
            } else {
                20
            },
        );
        key(&mut host, copied.target, 100);
        let responsive = wait(&mut host, &platform, &mut service, [1, 10, 20, 255]);
        present(&mut host, &responsive);
        key(&mut host, responsive.target, 90);
        let restored = wait(
            &mut host,
            &platform,
            &mut service,
            [220, HOST_TEXT.len() as u8, 1, 255],
        );
        present(&mut host, &restored);
        host.set_active(false);
        host.set_active(true);
        pump(&mut host, &platform, &mut service, 20);
        assert_eq!(service.writes, 0);
        assert_eq!(service.text[0], HOST_TEXT);
    }
}

#[test]
fn regular_file_receivers_never_start_a_platform_read() {
    let (mut host, platform, mut service, _child, initial) = start();
    key(&mut host, initial.target, 102);
    let denied = wait(&mut host, &platform, &mut service, [102, 20, 30, 255]);
    present(&mut host, &denied);
    pump(&mut host, &platform, &mut service, 20);
    assert_eq!(service.reads, 0);
    key(&mut host, denied.target, 90);
    let valid = wait(
        &mut host,
        &platform,
        &mut service,
        [220, HOST_TEXT.len() as u8, 1, 255],
    );
    assert_eq!(pixel(&valid, 0, 0)[0], 220);
}

#[test]
fn a_replaced_native_selection_revokes_its_queued_platform_write() {
    let (mut host, platform, mut service, _child, initial) = start();
    key(&mut host, initial.target, 91);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = loop {
        if let Some(frame) = host.dispatch().unwrap() {
            present(&mut host, &frame);
        }
        if let Some(request) = platform.next_request() {
            break request;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(pending.write_text(), Some(NATIVE_TEXT));
    // Replacing with a stalled source still cancels the old platform operation.
    key(&mut host, initial.target, 94);
    let replaced = wait(&mut host, &platform, &mut service, [94, 20, 30, 255]);
    present(&mut host, &replaced);
    assert!(!pending.is_live());
    assert!(
        pending
            .lease()
            .while_live(|| panic!("replaced source wrote OS data"))
            .is_none()
    );
    drop(pending);
    assert_eq!(service.writes, 0);
}

#[test]
fn stalled_platform_reads_are_revoked_without_consuming_a_peer_owners_capacity() {
    let (mut host, platform, mut service, _child, initial) = start();
    let (mut peer, peer_platform, mut peer_service, _peer, other) = start();
    key(&mut host, initial.target, 90);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = loop {
        if let Some(frame) = host.dispatch().unwrap() {
            present(&mut host, &frame);
        }
        if let Some(request) = platform.next_request() {
            break request;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    host.set_active(false);
    host.set_active(true);
    assert!(!pending.is_live());
    pending.complete(Ok("late platform reply".into()));
    key(&mut peer, other.target, 90);
    let survivor = wait(
        &mut peer,
        &peer_platform,
        &mut peer_service,
        [220, HOST_TEXT.len() as u8, 1, 255],
    );
    present(&mut peer, &survivor);
    let cancelled = wait(&mut host, &platform, &mut service, [220, 0, 1, 255]);
    present(&mut host, &cancelled);
    assert_eq!(peer_service.reads, 1);
}

#[test]
fn native_mime_metadata_limits_disconnect_only_the_offending_display() {
    let (mut host, platform, mut service, mut bad, initial) = start();
    let (mut peer, peer_platform, mut peer_service, _peer, other) = start();
    key(&mut host, initial.target, 101);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = host.dispatch().unwrap();
        service.service(&platform);
        if bad.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut peer, other.target, 90);
    let survivor = wait(
        &mut peer,
        &peer_platform,
        &mut peer_service,
        [220, HOST_TEXT.len() as u8, 1, 255],
    );
    assert_eq!(pixel(&survivor, 0, 0)[0], 220);
    assert_eq!(service.writes, 0);
}

impl Fixture {
    pub(super) fn clipboard_global(
        &mut self,
        registry: &wl_registry::WlRegistry,
        name: u32,
        interface: &str,
        version: u32,
        qh: &QueueHandle<Self>,
    ) {
        match interface {
            "wl_data_device_manager" => {
                self.clipboard.manager = Some(registry.bind(name, version.min(3), qh, ()))
            }
            "zwp_primary_selection_device_manager_v1" => {
                self.clipboard.primary = Some(registry.bind(name, 1, qh, ()))
            }
            _ => (),
        }
    }
    pub(super) fn clipboard_setup(&mut self, qh: &QueueHandle<Self>) {
        if std::env::var_os("VIVID_TREE_LATE_CLIPBOARD").is_some() && !self.clipboard.late_bind {
            return;
        }
        if let Some(seat) = &self.seat {
            if self.clipboard.device.is_none()
                && let Some(manager) = &self.clipboard.manager
            {
                self.clipboard.device = Some(manager.get_data_device(seat, qh, ()));
            }
            if self.clipboard.primary_device.is_none()
                && let Some(manager) = &self.clipboard.primary
            {
                self.clipboard.primary_device = Some(manager.get_device(seat, qh, ()));
            }
        }
    }
    pub(super) fn clipboard_key(
        &mut self,
        code: u32,
        serial: u32,
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) -> bool {
        // Rendered shell acceptance uses ordinary supported function keys.
        let code = if self.clipboard.manager.is_some() {
            match code {
                62 if std::env::var_os("VIVID_WAYLAND_CHECK_FILES").is_some() => 215,
                63 if std::env::var_os("VIVID_WAYLAND_CHECK_FILES").is_some() => 216,
                87 => 90,
                88 => 91,
                other => other,
            }
        } else {
            code
        };
        match code {
            103 => {
                self.clipboard.late_bind = true;
                self.clipboard_setup(qh);
            }
            89 => {
                assert!(
                    self.clipboard.file_offer.is_none()
                        || self.clipboard.file_offer != self.clipboard.offer,
                    "text-only clipboard advertised file MIME"
                );
                self.root_color = Some([89, 20, 30, 255]);
                self.draw(qh);
            }
            133 => {
                let manager = self.clipboard.manager.as_ref().unwrap();
                for _ in 0..8 {
                    manager.get_data_device(self.seat.as_ref().unwrap(), qh, ());
                }
                // Fewer than 300 client objects produce over 2,000 server offers.
                // Offers are deliberately retained to exercise server-ID admission.
                for _ in 0..256 {
                    let source = manager.create_data_source(qh, code);
                    source.offer(MIME.to_string());
                    self.clipboard
                        .device
                        .as_ref()
                        .unwrap()
                        .set_selection(Some(&source), serial);
                }
            }
            102 => {
                let file = tempfile::tempfile().unwrap();
                self.clipboard
                    .offer
                    .as_ref()
                    .unwrap()
                    .receive(MIME.to_string(), file.as_fd());
                self.root_color = Some([102, 20, 30, 255]);
                self.draw(qh);
            }
            90 | 92 | 98 | 215 | 219 => {
                if code == 215 {
                    assert_eq!(
                        self.clipboard.file_offer, self.clipboard.offer,
                        "file clipboard did not advertise file MIME"
                    );
                }
                let (mut reader, writer) = UnixStream::pair().unwrap();
                reader
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                match code {
                    215 => self
                        .clipboard
                        .offer
                        .as_ref()
                        .expect("file clipboard offer")
                        .receive(FILE_MIME.to_owned(), writer.as_fd()),
                    219 => self
                        .clipboard
                        .stale
                        .as_ref()
                        .expect("saved file offer")
                        .receive(FILE_MIME.to_owned(), writer.as_fd()),
                    90 => self
                        .clipboard
                        .offer
                        .as_ref()
                        .expect("clipboard offer")
                        .receive(MIME.to_string(), writer.as_fd()),
                    92 => self
                        .clipboard
                        .primary_offer
                        .as_ref()
                        .expect("primary offer")
                        .receive(MIME.to_string(), writer.as_fd()),
                    _ => self
                        .clipboard
                        .stale
                        .as_ref()
                        .expect("saved offer")
                        .receive(MIME.to_string(), writer.as_fd()),
                }
                connection.flush().unwrap();
                drop(writer);
                let mut text = String::new();
                reader.read_to_string(&mut text).unwrap();
                if code == 98 || code == 219 {
                    assert!(text.is_empty(), "stale offer returned platform bytes");
                } else if !text.is_empty() {
                    assert_eq!(
                        text,
                        match code {
                            92 => PRIMARY_TEXT,
                            215 => FILE_URIS,
                            _ => HOST_TEXT,
                        }
                    );
                }
                self.root_color = Some([
                    220,
                    text.len() as u8,
                    if code == 215 {
                        4
                    } else if code == 219 {
                        5
                    } else if code == 90 {
                        1
                    } else if code == 92 {
                        2
                    } else {
                        3
                    },
                    255,
                ]);
                self.draw(qh);
            }
            91 | 94..=96 | 99 | 101 | 216..=218 => {
                let source = self
                    .clipboard
                    .manager
                    .as_ref()
                    .unwrap()
                    .create_data_source(qh, code);
                source.offer(
                    if (216..=218).contains(&code) {
                        FILE_MIME
                    } else {
                        MIME
                    }
                    .to_string(),
                );
                if code == 101 {
                    for i in 0..32 {
                        source.offer(format!("application/x-test-{i}"));
                    }
                }
                self.clipboard.device.as_ref().unwrap().set_selection(
                    Some(&source),
                    if code == 99 {
                        self.clipboard.serial
                    } else {
                        serial
                    },
                );
                self.root_color = Some([code as u8, 20, 30, 255]);
                self.draw(qh);
            }
            93 => {
                let source = self
                    .clipboard
                    .primary
                    .as_ref()
                    .unwrap()
                    .create_source(qh, code);
                source.offer(MIME.to_string());
                self.clipboard
                    .primary_device
                    .as_ref()
                    .unwrap()
                    .set_selection(Some(&source), serial);
                self.root_color = Some([93, 20, 30, 255]);
                self.draw(qh);
            }
            97 => {
                self.clipboard.stale = self.clipboard.offer.clone();
                self.clipboard.serial = serial;
                self.root_color = Some([97, 20, 30, 255]);
                self.draw(qh);
            }
            _ => return false,
        }
        true
    }
}
impl Dispatch<wl_data_device::WlDataDevice, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_data_device::Event::Selection { id } = event {
            state.clipboard.offer = id;
            if std::env::var_os("VIVID_CLIPBOARD_CHECK_TRACE").is_some() {
                eprintln!(
                    "clipboard offer: present={} files={} phase={:?}",
                    state.clipboard.offer.is_some(),
                    state.clipboard.offer.is_some()
                        && state.clipboard.offer == state.clipboard.file_offer,
                    state.clipboard.file_handshake
                );
            }
            state.clipboard_late_ready(qh);
            if (std::env::var_os("VIVID_WAYLAND_CHECK_FILES").is_some()
                || std::env::var_os("VIVID_WAYLAND_CHECK_CLIPBOARD").is_some())
                && state.clipboard.offer.is_some()
            {
                let files = state.clipboard.offer == state.clipboard.file_offer;
                let check_files = std::env::var_os("VIVID_WAYLAND_CHECK_FILES").is_some();
                // Pixel input readiness precedes asynchronous clipboard offers.
                // Acknowledge the first offer and only the requested file steps.
                let next = match (&state.clipboard.file_handshake, files) {
                    (FileHandshake::InitialOffer, _) if !check_files => Some(FileHandshake::Done),
                    (FileHandshake::InitialOffer, false) => Some(FileHandshake::Files),
                    (FileHandshake::InitialOffer, true) => Some(FileHandshake::Text),
                    (FileHandshake::Files, true) => Some(FileHandshake::Text),
                    (FileHandshake::Text, false) => Some(FileHandshake::CopiedFiles),
                    (FileHandshake::CopiedFiles, true) => Some(FileHandshake::Done),
                    _ => None,
                };
                if let Some(next) = next {
                    state.clipboard.file_handshake = next;
                    state.root_color = Some([221, 0, if files { 4 } else { 0 }, 255]);
                    state.draw(qh);
                }
            }
        }
    }
    wayland_client::event_created_child!(Fixture, wl_data_device::WlDataDevice, [0 => (wl_data_offer::WlDataOffer, ())]);
}
impl Dispatch<primary_device::ZwpPrimarySelectionDeviceV1, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &primary_device::ZwpPrimarySelectionDeviceV1,
        event: primary_device::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let primary_device::Event::Selection { id } = event {
            state.clipboard.primary_offer = id;
            state.clipboard_late_ready(qh);
        }
    }
    wayland_client::event_created_child!(Fixture, primary_device::ZwpPrimarySelectionDeviceV1, [0 => (primary_offer::ZwpPrimarySelectionOfferV1, ())]);
}
impl Fixture {
    fn clipboard_late_ready(&mut self, qh: &QueueHandle<Self>) {
        if std::env::var_os("VIVID_TREE_LATE_CLIPBOARD").is_some()
            && !self.clipboard.late_ready
            && self.clipboard.offer.is_some()
            && self.clipboard.primary_offer.is_some()
        {
            self.clipboard.late_ready = true;
            self.root_color = Some([222, 0, 3, 255]);
            self.draw(qh);
        }
    }
}
impl Dispatch<wl_data_source::WlDataSource, u32> for Fixture {
    fn event(
        state: &mut Self,
        source: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        code: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                if *code == 94 || *code == 218 {
                    state.clipboard.held.push(fd);
                } else {
                    let bytes = if *code == 95 {
                        vec![0xff]
                    } else if *code == 96 {
                        vec![b'x'; platform::MAX_TEXT_BYTES + 1]
                    } else if *code == 216 {
                        FILE_URIS.as_bytes().to_vec()
                    } else if *code == 217 {
                        format!("{FILE_URIS}file://remote/forbidden\r\n").into_bytes()
                    } else {
                        NATIVE_TEXT.as_bytes().to_vec()
                    };
                    let _ = File::from(fd).write_all(&bytes);
                }
            }
            wl_data_source::Event::Cancelled => source.destroy(),
            _ => (),
        }
    }
}
impl Dispatch<primary_source::ZwpPrimarySelectionSourceV1, u32> for Fixture {
    fn event(
        _: &mut Self,
        source: &primary_source::ZwpPrimarySelectionSourceV1,
        event: primary_source::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            primary_source::Event::Send { fd, .. } => {
                let _ = File::from(fd).write_all(NATIVE_TEXT.as_bytes());
            }
            primary_source::Event::Cancelled => source.destroy(),
            _ => (),
        }
    }
}
delegate_noop!(Fixture: ignore wl_data_device_manager::WlDataDeviceManager);
impl Dispatch<wl_data_offer::WlDataOffer, ()> for Fixture {
    fn event(
        state: &mut Self,
        offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && mime_type == FILE_MIME
        {
            state.clipboard.file_offer = Some(offer.clone());
        }
    }
}
delegate_noop!(Fixture: ignore primary_manager::ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(Fixture: ignore primary_offer::ZwpPrimarySelectionOfferV1);

#[test]
fn exhausted_server_selection_offers_fail_the_root_and_preserve_another_owner() {
    let (mut host, platform, mut service, mut bad, frame) = start();
    pump(&mut host, &platform, &mut service, 20);
    let mut peer = WaylandHost::new(64, 48).unwrap();
    let _peer = client(&peer);
    let other = next(&mut peer);
    peer.set_active(true);
    present(&mut peer, &other);
    key(&mut host, frame.target, 133);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = host.dispatch().unwrap();
        if host.root_failed() && bad.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "selection offer quota did not retire its root"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!host.is_mapped());
    assert!(host.input_target().is_none());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
}
