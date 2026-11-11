//! Real SHM rotations and reflections across pixels, damage, cursors and input.

// Rust guideline compliant 2026-10-07
use super::*;
use wayland_client::protocol::wl_output::Transform;

#[derive(Default)]
pub(super) struct TransformFixture {
    cursor: Option<wl_surface::WlSurface>,
    scale: i32,
}
const TRANSFORMS: [Transform; 8] = [
    Transform::Normal,
    Transform::_90,
    Transform::_180,
    Transform::_270,
    Transform::Flipped,
    Transform::Flipped90,
    Transform::Flipped180,
    Transform::Flipped270,
];
pub(super) fn command(state: &mut Fixture, code: u32, qh: &QueueHandle<Fixture>) {
    let root = state.root.as_ref().unwrap().clone();
    match code {
        244..=251 => {
            root.set_buffer_transform(TRANSFORMS[(code - 244) as usize]);
            viewport_checks::command(state, 222, qh);
            return;
        }
        252 => {
            let vp = viewport_checks::viewport(state, qh);
            vp.set_source(1., 2., 3., 4.);
            vp.set_destination(6, 8);
        }
        253 => {
            state.transform.scale = 2;
            root.set_buffer_scale(2);
        }
        254 => {
            patch(state, false, qh);
            return;
        }
        255 => {
            root.set_buffer_transform(Transform::Normal);
            state.transform.scale = 1;
            viewport_checks::command(state, 241, qh);
            return;
        }
        40..=47 => {
            let surface = state
                .transform
                .cursor
                .get_or_insert_with(|| state.compositor.as_ref().unwrap().create_surface(qh, 4))
                .clone();
            state.cursor.pointer.as_ref().unwrap().set_cursor(
                state.cursor.serial,
                Some(&surface),
                1,
                2,
            );
            surface.set_buffer_transform(TRANSFORMS[(code - 40) as usize]);
            let mut pixels = Vec::with_capacity(6 * 4 * 4);
            for y in 0..4 {
                for x in 0..6 {
                    pixels.extend_from_slice(&[17, y, x, 255]);
                }
            }
            viewport_checks::attach(state, &surface, [6, 4], 1, &pixels, None, qh);
            return;
        }
        48 => {
            viewport_checks::command(state, 242, qh);
            viewport_checks::command(state, 231, qh);
            state.child.as_ref().unwrap().role.set_position(5, 3);
        }
        49 => {
            let child = &state.child.as_ref().unwrap().surface;
            child.set_buffer_transform(Transform::Flipped90);
            child.commit();
            // A synchronized child must wait for a later parent commit.
            return;
        }
        50 => (),
        51 => {
            patch(state, true, qh);
            return;
        }
        52 => {
            root.send_request(wl_surface::Request::SetBufferTransform {
                transform: WEnum::Unknown(8),
            })
            .unwrap();
            return;
        }
        53 => {
            let vp = viewport_checks::viewport(state, qh);
            vp.set_source(60., 0., 4., 1.);
            vp.set_destination(4, 1);
        }
        54 => root.set_buffer_transform(Transform::_90),
        _ => unreachable!(),
    }
    root.commit();
}

fn patch(state: &Fixture, buffer_damage: bool, qh: &QueueHandle<Fixture>) {
    let root = state.root.as_ref().unwrap();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&[233, 232, 231, 255].repeat(64 * 48))
        .unwrap();
    let pool = state
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 64 * 48 * 4, qh, ());
    let buffer = pool.create_buffer(0, 64, 48, 256, wl_shm::Format::Argb8888, qh, 1u8);
    pool.destroy();
    root.set_buffer_scale(state.transform.scale.max(1));
    root.attach(Some(&buffer), 0, 0);
    if buffer_damage {
        root.damage_buffer(1, 2, 1, 1);
    } else {
        root.damage(1, 2, 1, 1);
    }
    root.commit();
}

pub(super) fn helper(
    connection: &Connection,
    queue: &mut wayland_client::EventQueue<Fixture>,
    state: &mut Fixture,
) {
    queue.roundtrip(state).unwrap();
    queue.roundtrip(state).unwrap();
    command(state, 52, &queue.handle());
    connection.flush().unwrap();
    queue
        .roundtrip(state)
        .expect_err("invalid helper transform was accepted");
}

fn start() -> (WaylandHost, Client, Frame) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let child = client_with(
        &host,
        &[("VIVID_TREE_VIEWPORT", "1"), ("VIVID_TREE_TRANSFORM", "1")],
    );
    let first = next(&mut host);
    host.set_active(true);
    present(&mut host, &first);
    (host, child, first)
}
// Independent raster oracle: raw corner positions for each declared transform.
// A non-square buffer distinguishes quarter turns and both diagonal reflections.
fn raw(index: usize, x: u32, y: u32, w: u32, h: u32) -> [u32; 2] {
    match index {
        0 => [x, y],
        1 => [w - 1 - y, x],
        2 => [w - 1 - x, h - 1 - y],
        3 => [y, h - 1 - x],
        4 => [w - 1 - x, y],
        5 => [y, x],
        6 => [x, h - 1 - y],
        7 => [w - 1 - y, h - 1 - x],
        _ => unreachable!(),
    }
}
#[test]
fn all_buffer_transforms_preserve_every_pixel_and_revoke_old_geometry_without_readback() {
    let (mut host, _child, mut frame) = start();
    let initial = frame.clone();
    let stats = host.readback_stats();
    for index in 0..8 {
        let old = frame.target;
        key(&mut host, old, 244 + index as u32);
        frame = next(&mut host);
        let size = if index % 2 == 0 { [64, 48] } else { [48, 64] };
        assert_eq!([frame.width, frame.height], size);
        assert_eq!(frame.logical_size, size);
        assert_eq!(host.readback_stats(), stats);
        for y in 0..size[1] {
            for x in 0..size[0] {
                let [rx, ry] = raw(index, x, y, 64, 48);
                assert_eq!(
                    pixel(&frame, x, y),
                    [rx as u8, ry as u8, 4, 255],
                    "transform {index} at {x},{y}"
                );
            }
        }
        if index != 0 {
            assert_ne!(frame.target, old);
            assert!(!host.input(old, Input::Motion { x: 1., y: 1. }).unwrap());
            assert!(host.input_target().is_none());
        }
        present(&mut host, &frame);
    }
    assert_eq!(pixel(&initial, 0, 0), [0, 0, 4, 255]);
}
#[test]
fn transformed_surface_damage_patches_only_the_declared_pixels_at_both_integer_scales() {
    for index in 0..8 {
        for scale in [1, 2] {
            let (mut host, _child, first) = start();
            key(&mut host, first.target, 244 + index as u32);
            let mut frame = next(&mut host);
            present(&mut host, &frame);
            if scale == 2 {
                key(&mut host, frame.target, 253);
                frame = next(&mut host);
                present(&mut host, &frame);
            }
            let before = frame.clone();
            let stats = host.readback_stats();
            key(&mut host, frame.target, 254);
            frame = next(&mut host);
            assert_eq!(frame.target, before.target);
            assert_eq!(host.readback_stats().partial_reads, stats.partial_reads + 1);
            assert_eq!(
                host.readback_stats().pixel_bytes - stats.pixel_bytes,
                (scale * scale * 4) as u64
            );
            for y in 0..frame.height {
                for x in 0..frame.width {
                    let damaged =
                        (scale..2 * scale).contains(&x) && (2 * scale..3 * scale).contains(&y);
                    assert_eq!(
                        pixel(&frame, x, y),
                        if damaged {
                            [231, 232, 233, 255]
                        } else {
                            pixel(&before, x, y)
                        },
                        "transform {index}, scale {scale}, at {x},{y}"
                    );
                }
            }
            assert_eq!(pixel(&before, 0, 0), pixel(&frame, 0, 0));
        }
    }
}
#[test]
fn raw_buffer_damage_stays_in_buffer_coordinates_under_every_transform() {
    for index in 0..8 {
        let (mut host, _child, first) = start();
        key(&mut host, first.target, 244 + index as u32);
        let before = next(&mut host);
        present(&mut host, &before);
        let stats = host.readback_stats();
        key(&mut host, before.target, 51);
        let frame = next(&mut host);
        assert_eq!(frame.target, before.target);
        assert_eq!(host.readback_stats().pixel_bytes - stats.pixel_bytes, 4);
        for y in 0..frame.height {
            for x in 0..frame.width {
                let p = raw(index, x, y, 64, 48);
                assert_eq!(
                    pixel(&frame, x, y),
                    if p == [1, 2] {
                        [231, 232, 233, 255]
                    } else {
                        pixel(&before, x, y)
                    }
                );
            }
        }
    }
}
#[test]
fn malformed_helper_transform_does_not_fail_the_existing_root_with_reused_surface_ids() {
    let (mut host, _root, old) = start();
    let mut helper = client_with(&host, &[("VIVID_TREE_TRANSFORM_HELPER", "1")]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            assert_eq!(frame.target, old.target);
            assert_eq!(frame.rgba, old.rgba);
            present(&mut host, &frame);
        }
        if let Some(status) = helper.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "transform helper did not retire");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!host.root_failed());
    assert_eq!(host.input_target().unwrap().0, old.target);
    key(&mut host, old.target, 245);
    assert_eq!(next(&mut host).logical_size, [48, 64]);
}
#[test]
fn rotation_revalidates_a_previously_legal_viewport_against_the_swapped_extent() {
    let (mut host, _child, first) = start();
    key(&mut host, first.target, 53);
    let crop = next(&mut host);
    assert_eq!(pixel(&crop, 0, 0), [60, 0, 4, 255]);
    present(&mut host, &crop);
    key(&mut host, crop.target, 54);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !host.root_failed() {
        let _ = host.dispatch().unwrap();
        assert!(
            Instant::now() < deadline,
            "invalid transform did not fail the root"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(host.input_target().is_none());
}
#[test]
fn transformed_viewport_crops_use_surface_axes_and_keep_pointer_coordinates_logical() {
    for index in 0..8 {
        let (mut host, _child, first) = start();
        key(&mut host, first.target, 244 + index as u32);
        let transformed = next(&mut host);
        present(&mut host, &transformed);
        key(&mut host, transformed.target, 252);
        let crop = next(&mut host);
        assert_eq!(crop.logical_size, [6, 8]);
        for (x, y) in [(0, 0), (crop.width - 1, crop.height - 1)] {
            let sx = 1 + x * 3 / crop.width;
            let sy = 2 + y * 4 / crop.height;
            let [rx, ry] = raw(index, sx, sy, 64, 48);
            assert_eq!(pixel(&crop, x, y), [rx as u8, ry as u8, 4, 255]);
        }
        present(&mut host, &crop);
        assert!(
            host.input(crop.target, Input::Motion { x: 4.5, y: 6.25 })
                .unwrap()
        );
        assert!(
            host.input(
                crop.target,
                Input::Button {
                    code: 0x110,
                    pressed: true
                }
            )
            .unwrap()
        );
        assert_eq!(pixel(&next(&mut host), 0, 0), [101, 4, 6, 255]);
    }
}
#[test]
fn rotated_and_reflected_cursor_pixels_leave_hotspots_in_surface_coordinates() {
    let (mut host, _child, mut root) = start();
    assert!(
        host.input(root.target, Input::Motion { x: 1., y: 1. })
            .unwrap()
    );
    for index in 0..8 {
        key(&mut host, root.target, 40 + index as u32);
        let frame = next(&mut host);
        assert_eq!(frame.target, root.target);
        assert_eq!(frame.rgba, root.rgba);
        let vivid_sdk::gui_wayland::CursorImage::Surface(cursor) = &frame.cursor else {
            panic!("missing transformed cursor")
        };
        let size = if index % 2 == 0 { [6, 4] } else { [4, 6] };
        assert_eq!([cursor.width, cursor.height], size);
        assert_eq!(cursor.logical_size, size);
        assert_eq!(cursor.hotspot, [1, 2]);
        for y in 0..size[1] {
            for x in 0..size[0] {
                let [rx, ry] = raw(index, x, y, 6, 4);
                let at = ((y * size[0] + x) * 4) as usize;
                assert_eq!(&cursor.rgba[at..at + 4], &[rx as u8, ry as u8, 17, 255]);
            }
        }
        present(&mut host, &frame);
        root = frame;
    }
}
#[test]
fn synchronized_child_transform_waits_for_parent_and_upscaled_edges_do_not_overflow() {
    let (mut host, _child, first) = start();
    key(&mut host, first.target, 48);
    let child = next(&mut host);
    assert_eq!(child.logical_size, [128, 96]);
    assert_eq!(pixel(&child, 2, 1), [204, 22, 10, 255]);
    present(&mut host, &child);
    key(&mut host, child.target, 49);
    for _ in 0..15 {
        assert!(host.dispatch().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, child.target, 50);
    let rotated = next(&mut host);
    assert_ne!(rotated.target, child.target);
    assert_eq!(pixel(&rotated, 2, 1), [202, 24, 10, 255]);
    assert_eq!(pixel(&child, 2, 1), [204, 22, 10, 255]);
}
