//! Native synchronized trees, stacking, alpha, DPI and input-region routing.
#![cfg(all(feature = "gui-wayland", target_os = "linux"))]

// Rust guideline compliant 2026-10-07
use std::{
    io::Write as _,
    os::{fd::AsFd as _, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use vivid_sdk::gui_wayland::{Frame, Input, Target, WaylandHost};
use wayland_client::{
    Connection, Dispatch, Proxy as _, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_region, wl_registry,
        wl_seat, wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[path = "support/gui_wayland_cursor.rs"]
mod cursor_checks;
#[path = "support/gui_wayland_damage.rs"]
mod damage_checks;
#[path = "support/gui_wayland_descriptors.rs"]
mod descriptor_checks;
#[path = "support/gui_wayland_dispatch.rs"]
mod dispatch_checks;
#[path = "support/gui_wayland_objects.rs"]
mod object_checks;
#[path = "support/gui_wayland_pending.rs"]
mod pending_checks;
#[path = "support/gui_wayland_popup.rs"]
mod popup_checks;
#[path = "support/gui_wayland_rate.rs"]
mod rate_checks;
#[path = "support/gui_wayland_clipboard.rs"]
mod selection_checks;
#[path = "support/gui_wayland_shapes.rs"]
mod shape_checks;
#[path = "support/gui_wayland_shm_tree.rs"]
mod shm_checks;
#[path = "support/gui_wayland_shm_client.rs"]
#[expect(
    dead_code,
    reason = "Other shared driver commands are exercised by gui_wayland_shm"
)]
mod shm_client;
#[path = "support/gui_wayland_text.rs"]
mod text_checks;
#[path = "support/gui_wayland_transform.rs"]
mod transform_checks;
#[path = "support/gui_wayland_viewport.rs"]
mod viewport_checks;

struct Client(Child);
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn client(host: &WaylandHost) -> Client {
    client_with(host, &[])
}
fn client_with(host: &WaylandHost, environment: &[(&str, &str)]) -> Client {
    Client(
        Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "tree_client", "--nocapture"])
            .env("VIVID_TREE_SOCKET", host.socket_path())
            .envs(environment.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn next(host: &mut WaylandHost) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            return frame;
        }
        assert!(Instant::now() < deadline, "native surface tree timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn present(host: &mut WaylandHost, frame: &Frame) {
    assert!(host.presented(frame.id, frame.target));
}
fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
    let at = (y * frame.width + x) as usize * 4;
    frame.rgba[at..at + 4].try_into().unwrap()
}
fn key(host: &mut WaylandHost, target: Target, code: u32) {
    for pressed in [true, false] {
        assert!(host.input(target, Input::Key { code, pressed }).unwrap());
    }
}

#[test]
fn tree_pixels_sync_stacking_regions_and_generations_agree() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client(&host);
    let initial = next(&mut host);
    assert_eq!(pixel(&initial, 0, 0), [10, 20, 30, 255]);
    assert_eq!(
        pixel(&initial, 9, 7),
        [133, 10, 15, 255],
        "premultiplied child alpha is wrong"
    );
    assert_eq!(
        pixel(&initial, 12, 9),
        [0, 0, 200, 255],
        "nested scale-two child is not composed"
    );
    host.set_active(true);
    present(&mut host, &initial);
    assert!(
        host.input(initial.target, Input::Motion { x: 9., y: 7. })
            .unwrap()
    );
    assert!(
        host.input(
            initial.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    let clicked = next(&mut host);
    assert_eq!(
        pixel(&clicked, 0, 0),
        [102, 1, 1, 255],
        "pointer did not reach the child's local coordinates"
    );
    present(&mut host, &clicked);
    assert!(
        host.input(
            clicked.target,
            Input::Button {
                code: 0x110,
                pressed: false
            }
        )
        .unwrap()
    );
    key(&mut host, clicked.target, 32);
    for _ in 0..25 {
        assert!(
            host.dispatch().unwrap().is_none(),
            "synchronized child escaped its parent commit"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, clicked.target, 33);
    let green = next(&mut host);
    assert_eq!(pixel(&green, 9, 7), [0, 200, 0, 255]);
    assert_eq!(
        green.target, clicked.target,
        "pixel-only updates must preserve mapping"
    );
    present(&mut host, &green);
    key(&mut host, green.target, 34);
    let moved = next(&mut host);
    assert_ne!(moved.target, green.target);
    assert_eq!(pixel(&moved, 9, 7), [102, 1, 1, 255]);
    assert_eq!(pixel(&moved, 21, 11), [0, 200, 0, 255]);
    assert!(host.input_target().is_none());
    assert!(
        !host
            .input(green.target, Input::Motion { x: 21., y: 11. })
            .unwrap()
    );
    present(&mut host, &moved);
    key(&mut host, moved.target, 35);
    let below = next(&mut host);
    assert_eq!(
        pixel(&below, 21, 11),
        [102, 1, 1, 255],
        "child below parent overpainted its parent"
    );
    present(&mut host, &below);
    key(&mut host, below.target, 36);
    let hole = next(&mut host);
    assert_ne!(hole.target, below.target);
    present(&mut host, &hole);
    assert!(
        host.input(hole.target, Input::Motion { x: 21., y: 11. })
            .unwrap()
    );
    assert!(
        host.input(
            hole.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    let through = next(&mut host);
    assert_eq!(
        pixel(&through, 0, 0),
        [101, 21, 11, 255],
        "empty child input region did not pass input to root"
    );
    present(&mut host, &through);
    assert!(
        host.input(
            through.target,
            Input::Button {
                code: 0x110,
                pressed: false
            }
        )
        .unwrap()
    );
    key(&mut host, through.target, 37);
    let desync = next(&mut host);
    assert_eq!(
        pixel(&desync, 21, 11),
        [0, 0, 150, 255],
        "desynchronized child waited for a parent commit"
    );
    present(&mut host, &desync);
    key(&mut host, desync.target, 38);
    let clipped = next(&mut host);
    assert_eq!(pixel(&clipped, 0, 1), [0, 0, 150, 255]);
    assert_eq!(pixel(&clipped, 14, 1), [101, 21, 11, 255]);
    present(&mut host, &clipped);
    key(&mut host, clipped.target, 40);
    let uncommitted = next(&mut host);
    assert_eq!(
        pixel(&uncommitted, 0, 1),
        [0, 0, 150, 255],
        "parent committed a child's pending buffer"
    );
    present(&mut host, &uncommitted);
    key(&mut host, uncommitted.target, 41);
    let committed = next(&mut host);
    assert_eq!(pixel(&committed, 0, 1), [250, 250, 0, 255]);
    present(&mut host, &committed);
    key(&mut host, committed.target, 42);
    let pending_order = next(&mut host);
    assert_eq!(
        pixel(&pending_order, 0, 1),
        [90, 40, 20, 255],
        "child commit applied parent-owned stacking early"
    );
    present(&mut host, &pending_order);
    key(&mut host, pending_order.target, 43);
    let ordered = next(&mut host);
    assert_eq!(pixel(&ordered, 0, 1), [101, 21, 11, 255]);
    present(&mut host, &ordered);
    key(&mut host, ordered.target, 46);
    let orphaned = next(&mut host);
    assert_ne!(orphaned.target, ordered.target);
    present(&mut host, &orphaned);
    key(&mut host, orphaned.target, 47);
    let replaced = next(&mut host);
    assert_ne!(replaced.target, orphaned.target);
    assert_eq!(pixel(&replaced, 3, 3), [90, 40, 20, 255]);
    assert!(
        !host
            .input(ordered.target, Input::Motion { x: 3., y: 3. })
            .unwrap()
    );
    present(&mut host, &replaced);
    key(&mut host, replaced.target, 48);
    let same_mapping = next(&mut host);
    assert_ne!(
        same_mapping.target, replaced.target,
        "role replacement reused presented input authority"
    );
    assert_eq!(
        pixel(&same_mapping, 3, 3),
        [90, 40, 20, 255],
        "old-role reorder reached its replacement"
    );
}

#[test]
fn invalid_child_geometry_disconnects_only_its_display() {
    rejected_client_preserves_peer(39);
}

#[test]
fn desync_transition_applies_only_explicitly_committed_state() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    key(&mut host, initial.target, 32);
    for _ in 0..25 {
        assert!(host.dispatch().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, initial.target, 53);
    let desync = next(&mut host);
    assert_eq!(pixel(&desync, 9, 7), [0, 200, 0, 255]);
    present(&mut host, &desync);
    key(&mut host, desync.target, 40);
    let pending = next(&mut host);
    assert_eq!(pixel(&pending, 9, 7), [0, 200, 0, 255]);
    present(&mut host, &pending);
    key(&mut host, pending.target, 53);
    for _ in 0..25 {
        assert!(
            host.dispatch().unwrap().is_none(),
            "desync consumed uncommitted state"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, pending.target, 41);
    let committed = next(&mut host);
    assert_eq!(pixel(&committed, 9, 7), [250, 250, 0, 255]);
}

#[test]
fn grandchild_waits_for_its_direct_parent_commit_or_desync_transition() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    key(&mut host, initial.target, 54);
    for _ in 0..25 {
        assert!(host.dispatch().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, initial.target, 33);
    let unchanged = next(&mut host);
    assert_eq!(
        pixel(&unchanged, 12, 9),
        [0, 0, 200, 255],
        "root bypassed direct child commit"
    );
    present(&mut host, &unchanged);
    key(&mut host, unchanged.target, 55);
    let committed = next(&mut host);
    assert_eq!(pixel(&committed, 12, 9), [0, 180, 180, 255]);
    present(&mut host, &committed);
    key(&mut host, committed.target, 56);
    for _ in 0..25 {
        assert!(host.dispatch().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, committed.target, 53);
    let released = next(&mut host);
    assert_eq!(
        pixel(&released, 12, 9),
        [180, 0, 180, 255],
        "inherited desync did not release committed state"
    );
}

#[test]
fn new_desynchronized_child_waits_for_parent_membership_commit() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    key(&mut host, initial.target, 57);
    let pending = next(&mut host);
    assert_eq!(
        pixel(&pending, 0, 0),
        [10, 20, 30, 255],
        "child membership applied before parent commit"
    );
    assert_eq!(pending.target, initial.target);
    present(&mut host, &pending);
    key(&mut host, pending.target, 33);
    let visible = next(&mut host);
    assert_eq!(pixel(&visible, 0, 0), [70, 80, 90, 255]);
    assert_ne!(visible.target, pending.target);
    present(&mut host, &visible);
    key(&mut host, visible.target, 58);
    let replacement = next(&mut host);
    assert_eq!(pixel(&replacement, 0, 0), [10, 20, 30, 255]);
    assert_ne!(replacement.target, visible.target);
    present(&mut host, &replacement);
    key(&mut host, replacement.target, 33);
    let restored = next(&mut host);
    assert_eq!(pixel(&restored, 0, 0), [70, 80, 90, 255]);
    assert_ne!(restored.target, replacement.target);
}

#[test]
fn scaled_root_preserves_physical_pixels_and_mixed_child_scales() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client_with(
        &host,
        &[("VIVID_TREE_ROOT_SCALE", "2"), ("VIVID_TREE_PATTERN", "1")],
    );
    let frame = next(&mut host);
    assert_eq!([frame.width, frame.height], [128, 96]);
    assert_eq!(frame.logical_size, [64, 48]);
    assert_eq!(pixel(&frame, 1, 1), [11, 20, 30, 255]);
    assert_eq!(pixel(&frame, 2, 1), [12, 20, 30, 255]);
    assert_eq!(
        pixel(&frame, 25, 19),
        [0, 1, 201, 255],
        "scale-two grandchild lost physical pixels"
    );
    assert_eq!(
        pixel(&frame, 18, 14),
        [134, 10, 15, 255],
        "scale-one child blend changed root pixel coordinates"
    );
}

#[test]
fn equal_logical_size_with_new_buffer_scale_requires_fresh_presentation() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    key(&mut host, initial.target, 59);
    let scaled = next(&mut host);
    assert_eq!(scaled.logical_size, initial.logical_size);
    assert_eq!([scaled.width, scaled.height], [128, 96]);
    assert_ne!(scaled.target, initial.target);
    assert!(host.input_target().is_none());
    assert!(
        !host
            .input(initial.target, Input::Motion { x: 9., y: 7. })
            .unwrap()
    );
    present(&mut host, &scaled);
    assert!(
        host.input(scaled.target, Input::Motion { x: 9., y: 7. })
            .unwrap()
    );
}

#[test]
fn exhausted_tree_budgets_disconnect_only_the_offending_display() {
    for command in [49, 50, 51] {
        rejected_client_preserves_peer(command);
    }
}

fn rejected_client_preserves_peer(command: u32) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let mut peer = WaylandHost::new(64, 48).unwrap();
    let mut bad = client(&host);
    let _peer = client(&peer);
    let frame = next(&mut host);
    let other = next(&mut peer);
    host.set_active(true);
    peer.set_active(true);
    present(&mut host, &frame);
    present(&mut peer, &other);
    key(&mut host, frame.target, command);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        host.dispatch().unwrap();
        if bad.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "client command {command} was not rejected"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        peer.input(other.target, Input::Motion { x: 9., y: 7. })
            .unwrap()
    );
    assert!(
        peer.input(
            other.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    let survivor = next(&mut peer);
    assert_eq!(pixel(&survivor, 0, 0)[0], 102);
}

#[test]
fn clipped_child_callbacks_wait_until_the_child_is_presented() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = client_with(&host, &[("VIVID_TREE_CALLBACK", "1")]);
    let hidden = next(&mut host);
    host.set_active(true);
    present(&mut host, &hidden);
    for _ in 0..25 {
        assert!(
            host.dispatch().unwrap().is_none(),
            "clipped child received a manufactured callback"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, hidden.target, 34);
    let visible = next(&mut host);
    assert_eq!(pixel(&visible, 0, 0), [10, 20, 30, 255]);
    for _ in 0..10 {
        assert!(host.dispatch().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    present(&mut host, &visible);
    let callback = next(&mut host);
    assert_eq!(pixel(&callback, 0, 0), [80, 81, 82, 255]);
}

#[test]
#[ignore = "native child fixture invoked by surface-tree regressions"]
fn tree_client() {
    let connection = Connection::from_socket(
        UnixStream::connect(
            std::env::var_os("VIVID_TREE_SOCKET")
                .or_else(|| std::env::var_os("WAYLAND_DISPLAY"))
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut state = Fixture::default();
    if std::env::var_os("VIVID_TREE_TRANSFORM_HELPER").is_some() {
        transform_checks::helper(&connection, &mut queue, &mut state);
        return;
    }
    if std::env::var_os("VIVID_TREE_VIEWPORT_HELPER").is_some() {
        viewport_checks::helper(&connection, &mut queue, &mut state);
        return;
    }
    if std::env::var_os("VIVID_TREE_CURSOR_HELPER").is_some() {
        cursor_checks::helper(&connection, &mut queue, &mut state);
        return;
    }
    while queue.blocking_dispatch(&mut state).is_ok() {}
}
struct ChildSurface {
    surface: wl_surface::WlSurface,
    role: wl_subsurface::WlSubsurface,
}
#[derive(Default)]
struct Fixture {
    transform: transform_checks::TransformFixture,
    viewport: viewport_checks::ViewportFixture,
    shapes: shape_checks::ShapeFixture,
    cursor: cursor_checks::CursorFixture,
    damage: damage_checks::DamageFixture,
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    root: Option<wl_surface::WlSurface>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    xdg: Option<xdg_surface::XdgSurface>,
    child: Option<ChildSurface>,
    grandchild: Option<ChildSurface>,
    extra: Option<ChildSurface>,
    root_color: Option<[u8; 4]>,
    root_scale: Option<i32>,
    root_extent: [i32; 2],
    popups: Vec<popup_checks::NativePopup>,
    seat: Option<wl_seat::WlSeat>,
    keyboard_focus: u8,
    saved_serial: u32,
    popup_done_tag: Option<u8>,
    popup_order_valid: bool,
    clipboard: selection_checks::ClipboardFixture,
    deferred_first_frame: bool,
    ime: text_checks::ImeFixture,
    pointer: (u8, f64, f64),
}
impl Fixture {
    fn setup(&mut self, qh: &QueueHandle<Self>) {
        if std::env::var_os("VIVID_TREE_CURSOR_HELPER").is_some()
            || std::env::var_os("VIVID_TREE_VIEWPORT_HELPER").is_some()
            || std::env::var_os("VIVID_TREE_TRANSFORM_HELPER").is_some()
        {
            return;
        }
        if self.xdg.is_none()
            && let (Some(root), Some(shell)) = (&self.root, &self.shell)
        {
            let surface = shell.get_xdg_surface(root, qh, 1u8);
            surface.get_toplevel(qh, ());
            self.xdg = Some(surface);
            root.commit();
        }
    }
    fn buffer(
        &self,
        surface: &wl_surface::WlSurface,
        width: i32,
        height: i32,
        scale: i32,
        rgba: [u8; 4],
        qh: &QueueHandle<Self>,
    ) {
        let mut file = tempfile::tempfile().unwrap();
        let mut pixels =
            [rgba[2], rgba[1], rgba[0], rgba[3]].repeat(width as usize * height as usize);
        if std::env::var_os("VIVID_TREE_PATTERN").is_some() {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let at = (y * width as usize + x) * 4;
                    match surface.data::<u8>() {
                        Some(1) => pixels[at + 2] += (x % 8) as u8,
                        Some(3) => {
                            pixels[at] += (x % 8) as u8;
                            pixels[at + 1] += (y % 8) as u8;
                        }
                        _ => (),
                    }
                }
            }
        }
        file.write_all(&pixels).unwrap();
        let pool = self
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), width * height * 4, qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        pool.destroy();
        surface.set_buffer_scale(scale);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        if std::env::var_os("VIVID_TREE_ANIMATE").is_some() && surface.data::<u8>() == Some(&1) {
            surface.frame(qh, ());
        }
        surface.commit();
    }
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if std::env::var_os("VIVID_TREE_VIEWPORT").is_some() {
            viewport_checks::draw(self, qh);
            return;
        }
        if self.child.is_none() {
            let root = self.root.as_ref().unwrap();
            let comp = self.compositor.as_ref().unwrap();
            let sub = self.subcompositor.as_ref().expect("subcompositor global");
            let surface = comp.create_surface(qh, 2);
            let role = sub.get_subsurface(&surface, root, qh, ());
            role.set_position(8, 6);
            if std::env::var_os("VIVID_TREE_CALLBACK").is_some() {
                role.set_position(128, 0);
                surface.frame(qh, ());
            }
            self.buffer(&surface, 16, 12, 1, [128, 0, 0, 128], qh);
            let grandchild = comp.create_surface(qh, 3);
            let grandrole = sub.get_subsurface(&grandchild, &surface, qh, ());
            grandrole.set_position(4, 3);
            self.buffer(&grandchild, 8, 8, 2, [0, 0, 200, 255], qh);
            surface.commit();
            self.child = Some(ChildSurface { surface, role });
            self.grandchild = Some(ChildSurface {
                surface: grandchild,
                role: grandrole,
            });
        }
        let scale = self.root_scale.unwrap_or_else(|| {
            if std::env::var_os("VIVID_TREE_ROOT_SCALE").is_some() {
                2
            } else {
                1
            }
        });
        let [width, height] = if self.root_extent == [0, 0] {
            [64, 48]
        } else {
            self.root_extent
        };
        self.buffer(
            self.root.as_ref().unwrap(),
            width * scale,
            height * scale,
            scale,
            self.root_color.unwrap_or([10, 20, 30, 255]),
            qh,
        );
    }
    fn key(&mut self, key: u32, qh: &QueueHandle<Self>) {
        let child = self.child.as_ref().unwrap();
        let root = self.root.as_ref().unwrap();
        match key {
            32 => self.buffer(&child.surface, 16, 12, 1, [0, 200, 0, 255], qh),
            33 => root.commit(),
            34 => {
                child.role.set_position(20, 10);
                root.commit();
            }
            35 => {
                child.role.place_below(root);
                root.commit();
            }
            36 => {
                child.role.place_above(root);
                let region = self.compositor.as_ref().unwrap().create_region(qh, ());
                child.surface.set_input_region(Some(&region));
                region.destroy();
                child.surface.commit();
                root.commit();
            }
            37 => {
                child.role.set_desync();
                self.buffer(&child.surface, 16, 12, 1, [0, 0, 150, 255], qh);
            }
            38 => {
                child.role.set_position(-4, 0);
                root.commit();
            }
            39 => {
                child.role.set_position(i32::MAX, 0);
                root.commit();
            }
            40 => {
                child.role.set_sync();
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&[0, 250, 250, 255].repeat(16 * 12)).unwrap();
                let pool =
                    self.shm
                        .as_ref()
                        .unwrap()
                        .create_pool(file.as_fd(), 16 * 12 * 4, qh, ());
                let buffer =
                    pool.create_buffer(0, 16, 12, 16 * 4, wl_shm::Format::Argb8888, qh, ());
                pool.destroy();
                child.surface.attach(Some(&buffer), 0, 0);
                child.surface.damage_buffer(0, 0, 16, 12);
                root.commit();
            }
            41 => {
                child.surface.commit();
                root.commit();
            }
            42 => {
                child.role.place_below(root);
                child.role.set_desync();
                self.buffer(&child.surface, 16, 12, 1, [90, 40, 20, 255], qh);
            }
            43 => root.commit(),
            46 => child.role.destroy(),
            47 | 48 => {
                if key == 48 {
                    child.role.place_below(root);
                    child.role.destroy();
                }
                let surface = child.surface.clone();
                let role =
                    self.subcompositor
                        .as_ref()
                        .unwrap()
                        .get_subsurface(&surface, root, qh, ());
                role.set_position(2, 2);
                surface.commit();
                root.commit();
                self.child.as_mut().unwrap().role = role;
            }
            49 => {
                for _ in 0..257 {
                    child.role.place_below(root);
                }
                root.commit();
            }
            50 => {
                child.role.set_position(128, 0);
                root.commit();
                // Separate legal-size commits must still obey the retained
                // per-surface callback budget while the child is clipped.
                for _ in 0..2 {
                    for _ in 0..33 {
                        child.surface.frame(qh, ());
                    }
                    child.surface.commit();
                    root.commit();
                }
            }
            51 => {
                // Four maximum-size surfaces plus the existing root/tree exceed
                // the display's retained-pixel budget before the fourth import.
                for _ in 0..4 {
                    let surface = self.compositor.as_ref().unwrap().create_surface(qh, 4);
                    self.buffer(&surface, 2048, 2048, 1, [1, 2, 3, 255], qh);
                }
            }
            53 => child.role.set_desync(),
            54 | 56 => {
                let grandchild = self.grandchild.as_ref().unwrap();
                grandchild.role.set_desync();
                self.buffer(
                    &grandchild.surface,
                    8,
                    8,
                    2,
                    if key == 54 {
                        [0, 180, 180, 255]
                    } else {
                        [180, 0, 180, 255]
                    },
                    qh,
                );
            }
            55 => {
                child.surface.commit();
                root.commit();
            }
            57 => {
                let surface = self.compositor.as_ref().unwrap().create_surface(qh, 4);
                let role =
                    self.subcompositor
                        .as_ref()
                        .unwrap()
                        .get_subsurface(&surface, root, qh, ());
                role.set_desync();
                self.buffer(&surface, 4, 4, 1, [70, 80, 90, 255], qh);
                self.extra = Some(ChildSurface { surface, role });
            }
            58 => {
                let extra = self.extra.as_ref().unwrap();
                extra.role.destroy();
                let surface = extra.surface.clone();
                let role =
                    self.subcompositor
                        .as_ref()
                        .unwrap()
                        .get_subsurface(&surface, root, qh, ());
                role.set_desync();
                surface.commit();
                self.extra.as_mut().unwrap().role = role;
            }
            59 => {
                self.root_scale = Some(2);
                self.draw(qh);
            }
            _ => (),
        }
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for Fixture {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    let comp = registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(4),
                        qh,
                        (),
                    );
                    state.root = Some(comp.create_surface(qh, 1));
                    state.compositor = Some(comp);
                }
                "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.shell = Some(registry.bind(name, 3, qh, ())),
                "wl_seat" => {
                    state.seat =
                        Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(5), qh, ()));
                }
                _ => (),
            }
            state.clipboard_global(registry, name, &interface, version, qh);
            state.ime_global(registry, name, &interface, qh);
            shape_checks::global(state, registry, name, &interface, version, qh);
            viewport_checks::global(state, registry, name, &interface, qh);
            state.clipboard_setup(qh);
            state.setup(qh);
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Fixture {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, u8> for Fixture {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        tag: &u8,
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            if *tag != 1 {
                state.popup_configure(*tag, serial, qh);
                return;
            }
            surface.ack_configure(serial);
            state.viewport.configured = true;
            if let Some(path) = std::env::var_os("VIVID_TREE_FIRST_FRAME_GATE")
                && state.child.is_none()
            {
                if !state.deferred_first_frame {
                    let mut gate = UnixStream::connect(path).unwrap();
                    gate.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    gate.write_all(&[1]).unwrap();
                    let mut reply = [0];
                    std::io::Read::read_exact(&mut gate, &mut reply).unwrap();
                    state.deferred_first_frame = true;
                    connection.display().sync(qh, ());
                }
            } else {
                state.draw(qh);
            }
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for Fixture {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Pointer) {
                state.cursor.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { surface, .. } = &event {
            state.keyboard_focus = *surface.data::<u8>().unwrap();
        }
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && (key == 130 || key == 59 && std::env::var_os("VIVID_WAYLAND_CHECK_FLOOD").is_some())
        {
            dispatch_checks::flood_root(connection, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && (key == 132
                || key == 59 && std::env::var_os("VIVID_WAYLAND_CHECK_OBJECT_LIMIT").is_some())
        {
            object_checks::exceed_object_limit(state, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && (key == 138
                || key == 59 && std::env::var_os("VIVID_WAYLAND_CHECK_DESCRIPTOR_LIMIT").is_some())
        {
            descriptor_checks::exceed_root_limit(connection, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && (key == 139
                || key == 59 && std::env::var_os("VIVID_WAYLAND_CHECK_RATE_LIMIT").is_some())
        {
            rate_checks::exceed_root_limit(connection, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && ((140..=149).contains(&key)
                || key == 60 && std::env::var_os("VIVID_WAYLAND_CHECK_DAMAGE").is_some())
        {
            damage_checks::fixture_key(state, if key == 60 { 140 } else { key }, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && ((244..=255).contains(&key)
                || (40..=54).contains(&key) && std::env::var_os("VIVID_TREE_TRANSFORM").is_some()
                || [105, 106].contains(&key)
                    && std::env::var_os("VIVID_WAYLAND_CHECK_TRANSFORM").is_some())
        {
            transform_checks::command(
                state,
                match key {
                    106 => 245,
                    105 => 255,
                    other => other,
                },
                qh,
            );
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && ((220..=243).contains(&key)
                || [110, 14].contains(&key)
                    && std::env::var_os("VIVID_WAYLAND_CHECK_VIEWPORT").is_some())
        {
            viewport_checks::command(
                state,
                match key {
                    110 => 240,
                    14 => 241,
                    other => other,
                },
                qh,
            );
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && ((170..=214).contains(&key)
                || [65, 66, 104, 109].contains(&key)
                    && std::env::var_os("VIVID_WAYLAND_CHECK_CURSOR").is_some())
        {
            shape_checks::command(
                state,
                match key {
                    65 => 178,
                    66 => 176,
                    104 => 174,
                    109 => 204,
                    other => other,
                },
                qh,
            );
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && ((150..=169).contains(&key)
                || [60, 111, 102, 107].contains(&key)
                    && std::env::var_os("VIVID_WAYLAND_CHECK_CURSOR").is_some())
        {
            cursor_checks::command(
                state,
                match key {
                    60 => 150,
                    111 => 151,
                    102 => 152,
                    107 => 169,
                    other => other,
                },
                qh,
            );
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && ((134..=137).contains(&key)
                || key == 60 && std::env::var_os("VIVID_WAYLAND_CHECK_PENDING_LIMIT").is_some())
        {
            pending_checks::exceed_root_limit(state, if key == 60 { 134 } else { key }, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && (key == 131
                || key == 60 && std::env::var_os("VIVID_WAYLAND_CHECK_SHM_LIMIT").is_some())
        {
            shm_checks::exceed_pool_limit(state, qh);
        } else if let wl_keyboard::Event::Key {
            key,
            serial,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
            && !state.ime_key(key, qh)
            && !state.clipboard_key(key, serial, connection, qh)
            && !state.popup_key(key, serial, qh)
        {
            state.key(key, qh);
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.cursor.previous = state.cursor.serial;
                state.cursor.serial = serial;
                state.pointer = (*surface.data::<u8>().unwrap(), surface_x, surface_y);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer.1 = surface_x;
                state.pointer.2 = surface_y;
            }
            wl_pointer::Event::Button {
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                state.root_color = Some([
                    100 + state.pointer.0,
                    state.pointer.1 as u8,
                    state.pointer.2 as u8,
                    255,
                ]);
                state.draw(qh);
            }
            _ => (),
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for Fixture {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}
delegate_noop!(Fixture: ignore wl_compositor::WlCompositor);
delegate_noop!(Fixture: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(Fixture: ignore wl_subsurface::WlSubsurface);
delegate_noop!(Fixture: ignore wl_region::WlRegion);
impl Dispatch<wl_surface::WlSurface, u8> for Fixture {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &u8,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(Fixture: ignore wl_shm::WlShm);
delegate_noop!(Fixture: ignore wl_shm_pool::WlShmPool);
impl Dispatch<wl_callback::WlCallback, ()> for Fixture {
    fn event(
        state: &mut Self,
        callback: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if state.deferred_first_frame {
                assert_eq!(
                    state.keyboard_focus, 0,
                    "focus arrived before the first frame was presented"
                );
                state.deferred_first_frame = false;
                state.draw(qh);
                return;
            }
            if viewport_checks::callback(state, callback, qh) {
                return;
            }
            if cursor_checks::callback(state, callback, qh) {
                return;
            }
            if std::env::var_os("VIVID_TREE_CALLBACK").is_some()
                || std::env::var_os("VIVID_POPUP_CALLBACK").is_some()
            {
                state.root_color = Some([80, 81, 82, 255]);
            }
            state.draw(qh);
        }
    }
}
