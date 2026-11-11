//! Real named cursor requests, protocol errors and focused-owner isolation.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_sdk::{gui::CursorStyle, gui_wayland::CursorImage};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{self as device, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};

#[derive(Default)]
pub(super) struct ShapeFixture {
    manager: Option<WpCursorShapeManagerV1>,
    legacy: Option<WpCursorShapeManagerV1>,
    device: Option<WpCursorShapeDeviceV1>,
}
pub(super) fn global(
    state: &mut Fixture,
    registry: &wl_registry::WlRegistry,
    name: u32,
    interface: &str,
    version: u32,
    qh: &QueueHandle<Fixture>,
) {
    if interface == "wp_cursor_shape_manager_v1" {
        assert_eq!(version, 2);
        state.shapes.manager = Some(registry.bind(name, 2, qh, ()));
        state.shapes.legacy = Some(registry.bind(name, 1, qh, ()));
    }
}
pub(super) fn command(state: &mut Fixture, code: u32, qh: &QueueHandle<Fixture>) {
    if code == 213 {
        state.draw(qh);
        return;
    }
    if code == 208 {
        state.shapes.device.take().unwrap().destroy();
    } else {
        let legacy = [209, 212, 214].contains(&code);
        let pointer = state.cursor.pointer.as_ref().unwrap();
        let manager = if legacy {
            state.shapes.legacy.as_ref().unwrap()
        } else {
            state.shapes.manager.as_ref().unwrap()
        };
        let device = if legacy {
            manager.get_pointer(pointer, qh, ())
        } else {
            state
                .shapes
                .device
                .get_or_insert_with(|| manager.get_pointer(pointer, qh, ()))
                .clone()
        };
        let value = match code {
            206 => 9,  // Text with a retired enter serial.
            207 => 0,  // No valid shape has value zero.
            209 => 35, // Version two on a version-one device.
            210 => 9,  // Text with a future serial.
            211 => u32::MAX,
            212 => 36, // The other version-two shape.
            214 => 9,  // A valid version-one shape.
            _ => code - 169,
        };
        let serial = match code {
            206 => state.cursor.previous,
            210 => state.cursor.serial.wrapping_add(1),
            _ => state.cursor.serial,
        };
        device
            .send_request(device::Request::SetShape {
                serial,
                shape: WEnum::from(value),
            })
            .unwrap();
    }
    // A pixel-identical root commit acknowledges even an ignored/no-op request.
    // A helper has no xdg role and must not manufacture a root buffer.
    if std::env::var_os("VIVID_TREE_CURSOR_HELPER").is_none() {
        state.draw(qh);
    }
}
delegate_noop!(Fixture: ignore WpCursorShapeManagerV1);
delegate_noop!(Fixture: ignore WpCursorShapeDeviceV1);

fn start() -> (WaylandHost, Client, Frame) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let child = client(&host);
    let root = next(&mut host);
    host.set_active(true);
    present(&mut host, &root);
    enter(&mut host, &root);
    (host, child, root)
}
fn enter(host: &mut WaylandHost, root: &Frame) {
    assert!(
        host.input(root.target, Input::Motion { x: 1., y: 1. })
            .unwrap()
    );
}
fn style(frame: &Frame) -> CursorStyle {
    match frame.cursor {
        CursorImage::Default => CursorStyle::Arrow,
        CursorImage::Named(style) => style,
        _ => panic!("expected named cursor, got {:?}", frame.cursor),
    }
}
#[test]
fn all_version_two_shapes_reach_the_host_without_changing_root_pixels_or_geometry() {
    let (mut host, _child, root) = start();
    let expected = [
        CursorStyle::Arrow,
        CursorStyle::ContextualMenu,
        CursorStyle::Help,
        CursorStyle::PointingHand,
        CursorStyle::Progress,
        CursorStyle::Wait,
        CursorStyle::Cell,
        CursorStyle::Crosshair,
        CursorStyle::IBeam,
        CursorStyle::IBeamCursorForVerticalLayout,
        CursorStyle::DragLink,
        CursorStyle::DragCopy,
        CursorStyle::Move,
        CursorStyle::NoDrop,
        CursorStyle::OperationNotAllowed,
        CursorStyle::OpenHand,
        CursorStyle::ClosedHand,
        CursorStyle::ResizeRight,
        CursorStyle::ResizeUp,
        CursorStyle::ResizeUpRight,
        CursorStyle::ResizeUpLeft,
        CursorStyle::ResizeDown,
        CursorStyle::ResizeDownRight,
        CursorStyle::ResizeDownLeft,
        CursorStyle::ResizeLeft,
        CursorStyle::ResizeLeftRight,
        CursorStyle::ResizeUpDown,
        CursorStyle::ResizeUpRightDownLeft,
        CursorStyle::ResizeUpLeftDownRight,
        CursorStyle::ResizeColumn,
        CursorStyle::ResizeRow,
        CursorStyle::AllScroll,
        CursorStyle::ZoomIn,
        CursorStyle::ZoomOut,
        CursorStyle::DragAsk,
        CursorStyle::ResizeAll,
    ];
    for (index, expected) in expected.into_iter().enumerate() {
        key(&mut host, root.target, 170 + index as u32);
        let frame = next(&mut host);
        assert_eq!(style(&frame), expected);
        assert_eq!(frame.target, root.target);
        assert_eq!(frame.rgba, root.rgba);
        present(&mut host, &frame);
    }
}
#[test]
fn named_and_bitmap_cursors_replace_each_other_and_device_destroy_keeps_the_shape() {
    let (mut host, _child, root) = start();
    key(&mut host, root.target, 150);
    let bitmap = next(&mut host);
    assert!(matches!(bitmap.cursor, CursorImage::Surface(_)));
    present(&mut host, &bitmap);
    key(&mut host, root.target, 178);
    let text = next(&mut host);
    assert_eq!(style(&text), CursorStyle::IBeam);
    present(&mut host, &text);
    key(&mut host, root.target, 152);
    key(&mut host, root.target, 213);
    let destroyed_bitmap = next(&mut host);
    assert_eq!(style(&destroyed_bitmap), CursorStyle::IBeam);
    present(&mut host, &destroyed_bitmap);
    key(&mut host, root.target, 208);
    let destroyed_device = next(&mut host);
    assert_eq!(style(&destroyed_device), CursorStyle::IBeam);
    present(&mut host, &destroyed_device);
    key(&mut host, root.target, 150);
    let bitmap = next(&mut host);
    assert!(matches!(bitmap.cursor, CursorImage::Surface(_)));
    present(&mut host, &bitmap);
    key(&mut host, root.target, 151);
    let hidden = next(&mut host);
    assert!(matches!(hidden.cursor, CursorImage::Hidden));
    present(&mut host, &hidden);
    key(&mut host, root.target, 202);
    assert_eq!(style(&next(&mut host)), CursorStyle::ZoomIn);
}
#[test]
fn version_one_devices_still_accept_their_own_shapes() {
    let (mut host, _child, root) = start();
    key(&mut host, root.target, 214);
    let frame = next(&mut host);
    assert_eq!(style(&frame), CursorStyle::IBeam);
    assert_eq!(frame.target, root.target);
    assert!(!host.root_failed());
}
#[test]
fn named_cursor_authority_is_revoked_on_blur_resize_and_stale_or_future_serials() {
    let (mut host, _child, root) = start();
    key(&mut host, root.target, 178);
    let text = next(&mut host);
    assert_eq!(style(&text), CursorStyle::IBeam);
    present(&mut host, &text);
    host.set_active(false);
    let blur = next(&mut host);
    assert_eq!(style(&blur), CursorStyle::Arrow);
    present(&mut host, &blur);
    host.set_active(true);
    enter(&mut host, &blur);
    for code in [206, 210] {
        key(&mut host, root.target, code);
        let stale = next(&mut host);
        assert_eq!(style(&stale), CursorStyle::Arrow);
        present(&mut host, &stale);
    }
    key(&mut host, root.target, 178);
    let fresh = next(&mut host);
    assert_eq!(style(&fresh), CursorStyle::IBeam);
    present(&mut host, &fresh);
    host.resize(80, 60).unwrap();
    let resized = next(&mut host);
    assert_ne!(resized.target, root.target);
    assert_eq!(style(&resized), CursorStyle::Arrow);
    present(&mut host, &resized);
    enter(&mut host, &resized);
    key(&mut host, resized.target, 176);
    assert_eq!(style(&next(&mut host)), CursorStyle::Cell);
}
#[test]
fn invalid_and_too_new_shapes_fail_only_the_requesting_owner() {
    let (mut peer, _peer, root) = start();
    key(&mut peer, root.target, 178);
    let peer_frame = next(&mut peer);
    present(&mut peer, &peer_frame);
    for code in [207, 209, 211, 212] {
        let (mut host, _child, bad) = start();
        key(&mut host, bad.target, code);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !host.root_failed() {
            host.dispatch().unwrap();
            assert!(Instant::now() < deadline, "invalid shape was not rejected");
        }
        assert_eq!(peer.input_target().unwrap().0, peer_frame.target);
        assert!(!peer.root_failed());
    }
    key(&mut peer, root.target, 202);
    assert_eq!(style(&next(&mut peer)), CursorStyle::ZoomIn);
}
#[test]
fn an_implicit_pointer_grab_does_not_accept_a_mismatched_shape_serial() {
    let (mut host, _child, root) = start();
    key(&mut host, root.target, 177);
    let crosshair = next(&mut host);
    assert_eq!(style(&crosshair), CursorStyle::Crosshair);
    present(&mut host, &crosshair);
    assert!(
        host.input(
            root.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    let held = next(&mut host);
    present(&mut host, &held);
    for code in [206, 210] {
        key(&mut host, root.target, code);
        let ignored = next(&mut host);
        assert_eq!(style(&ignored), CursorStyle::Crosshair);
        present(&mut host, &ignored);
    }
    assert!(
        host.input(
            root.target,
            Input::Button {
                code: 0x110,
                pressed: false
            }
        )
        .unwrap()
    );
    key(&mut host, root.target, 178);
    assert_eq!(style(&next(&mut host)), CursorStyle::IBeam);
}
#[test]
fn unfocused_shape_helpers_and_their_teardown_cannot_replace_another_clients_cursor() {
    let (mut host, _root, root) = start();
    key(&mut host, root.target, 178);
    let text = next(&mut host);
    present(&mut host, &text);
    let mut helper = client_with(
        &host,
        &[
            ("VIVID_TREE_CURSOR_HELPER", "1"),
            ("VIVID_TREE_SHAPE_HELPER", "1"),
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            assert_eq!(style(&frame), CursorStyle::IBeam);
            assert_eq!(frame.target, text.target);
            present(&mut host, &frame);
        }
        if let Some(status) = helper.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "shape helper did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, text.target, 176);
    assert_eq!(style(&next(&mut host)), CursorStyle::Cell);
    assert!(!host.root_failed());
}
