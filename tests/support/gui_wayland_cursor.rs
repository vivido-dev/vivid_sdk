//! Real native cursor surfaces, presentation clocks and authority isolation.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_sdk::gui_wayland::CursorImage;

pub(super) fn helper(
    connection: &Connection,
    queue: &mut wayland_client::EventQueue<Fixture>,
    state: &mut Fixture,
) {
    queue.roundtrip(state).unwrap();
    queue.roundtrip(state).unwrap();
    command(state, 150, &queue.handle());
    if std::env::var_os("VIVID_TREE_SHAPE_HELPER").is_some() {
        super::shape_checks::command(state, 202, &queue.handle());
    }
    connection.flush().unwrap();
    queue.roundtrip(state).unwrap();
}

#[derive(Default)]
pub(super) struct CursorFixture {
    pub pointer: Option<wl_pointer::WlPointer>,
    pub serial: u32,
    pub previous: u32,
    surface: Option<wl_surface::WlSurface>,
    callback: Option<wl_callback::WlCallback>,
    child: Option<ChildSurface>,
}
pub(super) fn command(state: &mut Fixture, code: u32, qh: &QueueHandle<Fixture>) {
    let pointer = state.cursor.pointer.as_ref().unwrap().clone();
    let serial = state.cursor.serial;
    if code == 151 || code == 156 {
        pointer.set_cursor(
            if code == 156 {
                state.cursor.previous
            } else {
                serial
            },
            None,
            0,
            0,
        );
        if code == 156 {
            state.root_color = Some([156, 0, 0, 255]);
            state.draw(qh);
        }
        return;
    }
    if code == 152 {
        state.cursor.surface.take().unwrap().destroy();
        return;
    }
    if code == 160 {
        pointer.set_cursor(serial, state.root.as_ref(), 0, 0);
        return;
    }
    let surface = state
        .cursor
        .surface
        .get_or_insert_with(|| state.compositor.as_ref().unwrap().create_surface(qh, 4))
        .clone();
    if code == 165 {
        let child = state.compositor.as_ref().unwrap().create_surface(qh, 5);
        let role = state
            .subcompositor
            .as_ref()
            .unwrap()
            .get_subsurface(&child, &surface, qh, ());
        role.set_position(2, 1);
        state.buffer(&child, 8, 4, 2, [20, 30, 240, 255], qh);
        state.cursor.child = Some(ChildSurface {
            surface: child,
            role,
        });
        surface.commit();
        return;
    }
    if code == 166 {
        let child = state.cursor.child.take().unwrap();
        child.surface.destroy();
        child.role.destroy();
        return;
    }
    if code == 162 {
        surface.attach(None, 0, 0);
        surface.commit();
        return;
    }
    if code == 163 {
        surface.set_buffer_scale(2);
        surface.commit();
        return;
    }
    if code == 164 {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[10, 220, 250, 255].repeat(16 * 12))
            .unwrap();
        let pool = state
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), 16 * 12 * 4, qh, ());
        let buffer = pool.create_buffer(0, 16, 12, 64, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        surface.attach(Some(&buffer), 1, -2);
        surface.damage_buffer(0, 0, 16, 12);
        surface.commit();
        return;
    }
    let hotspot = if code == 155 {
        [i32::MIN, 0]
    } else if code == 161 {
        [-5, 7]
    } else {
        [2, 3]
    };
    if code == 158 {
        state.buffer(&surface, 258, 8, 1, [250, 220, 10, 255], qh);
    }
    pointer.set_cursor(serial, Some(&surface), hotspot[0], hotspot[1]);
    if code == 158 || code == 155 || code == 161 {
        return;
    }
    if code == 159 {
        state.cursor.callback = Some(surface.frame(qh, ()));
    }
    let (width, scale, color) = if code == 154 {
        (258, 1, [250, 220, 10, 255])
    } else if code == 169 {
        (16, 2, [128, 0, 0, 128])
    } else if code == 153 {
        (16, 1, [10, 200, 240, 255])
    } else {
        (16, 2, [250, 220, 10, 255])
    };
    state.buffer(&surface, width, 12, scale, color, qh);
}

pub(super) fn callback(
    state: &mut Fixture,
    callback: &wl_callback::WlCallback,
    qh: &QueueHandle<Fixture>,
) -> bool {
    if state.cursor.callback.as_ref() != Some(callback) {
        return false;
    }
    state.cursor.callback = None;
    state.root_color = Some([91, 92, 93, 255]);
    state.draw(qh);
    true
}
fn motion(host: &mut WaylandHost, frame: &Frame) {
    assert!(
        host.input(frame.target, Input::Motion { x: 1., y: 1. })
            .unwrap()
    );
    // The following key uses the same stream: cursor commands run after enter.
}
fn surface(frame: &Frame) -> &vivid_sdk::gui_wayland::CursorSurface {
    let CursorImage::Surface(surface) = &frame.cursor else {
        panic!("missing surface cursor: {:?}", frame.cursor)
    };
    surface
}
#[test]
fn cursor_pixels_scale_hotspot_updates_unmap_and_destruction_are_independent_of_the_root() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let root = next(&mut host);
    host.set_active(true);
    present(&mut host, &root);
    motion(&mut host, &root);
    key(&mut host, root.target, 150);
    let first = next(&mut host);
    let a = surface(&first);
    assert_eq!(
        (a.width, a.height, a.logical_size, a.hotspot),
        (16, 12, [8, 6], [2, 3])
    );
    assert_eq!(&a.rgba[..4], &[250, 220, 10, 255]);
    assert_eq!(first.rgba, root.rgba);
    assert_eq!(first.target, root.target);
    present(&mut host, &first);
    key(&mut host, first.target, 151);
    let hidden = next(&mut host);
    assert!(matches!(hidden.cursor, CursorImage::Hidden));
    present(&mut host, &hidden);
    key(&mut host, hidden.target, 150);
    let remapped = next(&mut host);
    surface(&remapped);
    present(&mut host, &remapped);
    key(&mut host, first.target, 153);
    let updated = next(&mut host);
    assert_eq!(surface(&updated).logical_size, [16, 12]);
    assert!(surface(&updated).revision > a.revision);
    assert_eq!(&surface(&updated).rgba[..4], &[10, 200, 240, 255]);
    assert_eq!(&a.rgba[..4], &[250, 220, 10, 255]);
    present(&mut host, &updated);
    key(&mut host, updated.target, 161);
    let negative = next(&mut host);
    assert_eq!(surface(&negative).hotspot, [-5, 7]);
    present(&mut host, &negative);
    key(&mut host, negative.target, 163);
    let scaled = next(&mut host);
    assert_eq!(surface(&scaled).logical_size, [8, 6]);
    present(&mut host, &scaled);
    key(&mut host, scaled.target, 164);
    let offset = next(&mut host);
    assert_eq!(surface(&offset).hotspot, [-6, 9]);
    present(&mut host, &offset);
    key(&mut host, offset.target, 162);
    let unmapped = next(&mut host);
    assert!(matches!(unmapped.cursor, CursorImage::Hidden));
    present(&mut host, &unmapped);
    key(&mut host, unmapped.target, 152);
    let destroyed = next(&mut host);
    assert!(matches!(destroyed.cursor, CursorImage::Default));
    assert_eq!(host.input_target().unwrap().0, root.target);
}
#[test]
fn cursor_callbacks_wait_for_host_presentation() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let root = next(&mut host);
    host.set_active(true);
    present(&mut host, &root);
    motion(&mut host, &root);
    key(&mut host, root.target, 159);
    let cursor = next(&mut host);
    surface(&cursor);
    for _ in 0..10 {
        assert!(host.dispatch().unwrap().is_none());
    }
    present(&mut host, &cursor);
    assert_eq!(pixel(&next(&mut host), 0, 0), [91, 92, 93, 255]);
}

#[test]
fn cursor_subsurfaces_compose_without_becoming_root_input_layers() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let root = next(&mut host);
    host.set_active(true);
    present(&mut host, &root);
    motion(&mut host, &root);
    key(&mut host, root.target, 150);
    let cursor = next(&mut host);
    present(&mut host, &cursor);
    key(&mut host, cursor.target, 165);
    let tree = next(&mut host);
    assert_eq!(
        &surface(&tree).rgba[(2 * 16 + 4) * 4..][..4],
        &[20, 30, 240, 255]
    );
    assert_eq!(tree.rgba, root.rgba);
    assert_eq!(tree.target, root.target);
    present(&mut host, &tree);
    key(&mut host, tree.target, 166);
    let removed = next(&mut host);
    assert_eq!(
        &surface(&removed).rgba[(2 * 16 + 4) * 4..][..4],
        &[250, 220, 10, 255]
    );
    assert_eq!(removed.target, root.target);
}
#[test]
fn old_enter_serials_and_deactivation_cannot_reuse_cursor_authority() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let root = next(&mut host);
    host.set_active(true);
    present(&mut host, &root);
    motion(&mut host, &root);
    key(&mut host, root.target, 150);
    let cursor = next(&mut host);
    surface(&cursor);
    present(&mut host, &cursor);
    host.set_active(false);
    let reset = next(&mut host);
    assert!(matches!(reset.cursor, CursorImage::Default));
    present(&mut host, &reset);
    host.set_active(true);
    motion(&mut host, &reset);
    key(&mut host, reset.target, 156);
    let stale = next(&mut host);
    assert_eq!(pixel(&stale, 0, 0), [156, 0, 0, 255]);
    assert!(matches!(stale.cursor, CursorImage::Default));
    present(&mut host, &stale);
    key(&mut host, stale.target, 150);
    let remapped = next(&mut host);
    surface(&remapped);
    present(&mut host, &remapped);
    host.resize(80, 60).unwrap();
    assert!(
        !host
            .input(remapped.target, Input::Motion { x: 1., y: 1. })
            .unwrap()
    );
    let resized = next(&mut host);
    assert_ne!(resized.target, remapped.target);
    assert!(matches!(resized.cursor, CursorImage::Default));
    present(&mut host, &resized);
    motion(&mut host, &resized);
    key(&mut host, resized.target, 150);
    surface(&next(&mut host));
}
#[test]
fn malformed_cursor_requests_retire_only_their_own_display() {
    let mut peer = WaylandHost::new(64, 48).unwrap();
    let _peer = client(&peer);
    let other = next(&mut peer);
    peer.set_active(true);
    present(&mut peer, &other);
    motion(&mut peer, &other);
    key(&mut peer, other.target, 150);
    let other = next(&mut peer);
    surface(&other);
    present(&mut peer, &other);
    for code in [154, 155, 158, 160] {
        let mut host = WaylandHost::new(64, 48).unwrap();
        let _child = client(&host);
        let root = next(&mut host);
        host.set_active(true);
        present(&mut host, &root);
        motion(&mut host, &root);
        key(&mut host, root.target, code);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !host.root_failed() {
            host.dispatch().unwrap();
            assert!(Instant::now() < deadline);
        }
        assert_eq!(peer.input_target().unwrap().0, other.target);
        assert!(!peer.root_failed());
    }
    key(&mut peer, other.target, 153);
    assert_eq!(&surface(&next(&mut peer)).rgba[..4], &[10, 200, 240, 255]);
}

#[test]
fn unfocused_helper_cursor_requests_and_teardown_preserve_the_rendered_cursor() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _root = client(&host);
    let root = next(&mut host);
    host.set_active(true);
    present(&mut host, &root);
    motion(&mut host, &root);
    key(&mut host, root.target, 150);
    let cursor = next(&mut host);
    let revision = surface(&cursor).revision;
    present(&mut host, &cursor);
    let mut helper = client_with(&host, &[("VIVID_TREE_CURSOR_HELPER", "1")]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            assert_eq!(surface(&frame).revision, revision);
            assert_eq!(frame.target, cursor.target);
            present(&mut host, &frame);
        }
        if let Some(status) = helper.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unfocused cursor helper did not complete"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..3 {
        if let Some(frame) = host.dispatch().unwrap() {
            assert_eq!(surface(&frame).revision, revision);
            present(&mut host, &frame);
        }
    }
    assert!(!host.root_failed());
    key(&mut host, cursor.target, 153);
    assert_eq!(&surface(&next(&mut host)).rgba[..4], &[10, 200, 240, 255]);
}
