//! Real fractional-scale and viewport clients with exact pixel and input checks.

// Rust guideline compliant 2026-10-07
use super::*;
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self as fractional, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

pub(super) struct ViewportFixture {
    manager: Option<WpViewporter>,
    scales: Option<WpFractionalScaleManagerV1>,
    root: Option<WpViewport>,
    preference: Option<WpFractionalScaleV1>,
    child: Option<WpViewport>,
    pub configured: bool,
    preferred: bool,
    auto: bool,
    units: u32,
    raw: Option<[i32; 2]>,
    scale: i32,
    callback: Option<wl_callback::WlCallback>,
}
impl Default for ViewportFixture {
    fn default() -> Self {
        Self {
            manager: None,
            scales: None,
            root: None,
            preference: None,
            child: None,
            configured: false,
            preferred: false,
            auto: true,
            units: 120,
            raw: None,
            scale: 1,
            callback: None,
        }
    }
}
pub(super) fn global(
    state: &mut Fixture,
    registry: &wl_registry::WlRegistry,
    name: u32,
    interface: &str,
    qh: &QueueHandle<Fixture>,
) {
    match interface {
        "wp_viewporter" => state.viewport.manager = Some(registry.bind(name, 1, qh, ())),
        "wp_fractional_scale_manager_v1" => {
            state.viewport.scales = Some(registry.bind(name, 1, qh, ()))
        }
        _ => (),
    }
    if std::env::var_os("VIVID_TREE_VIEWPORT").is_some()
        && state.viewport.preference.is_none()
        && let (Some(manager), Some(root)) = (&state.viewport.scales, &state.root)
    {
        state.viewport.preference = Some(manager.get_fractional_scale(root, qh, ()));
    }
}
pub(super) fn viewport(state: &mut Fixture, qh: &QueueHandle<Fixture>) -> WpViewport {
    state
        .viewport
        .root
        .get_or_insert_with(|| {
            state.viewport.manager.as_ref().unwrap().get_viewport(
                state.root.as_ref().unwrap(),
                qh,
                (),
            )
        })
        .clone()
}
fn logical(state: &Fixture) -> [i32; 2] {
    if state.root_extent == [0, 0] {
        if std::env::var_os("VIVID_TREE_VIEWPORT_ODD").is_some() {
            [65, 49]
        } else {
            [64, 48]
        }
    } else {
        state.root_extent
    }
}
fn raw(state: &Fixture) -> [i32; 2] {
    state.viewport.raw.unwrap_or_else(|| {
        logical(state).map(|v| ((v as u32 * state.viewport.units + 60) / 120) as i32)
    })
}
pub(super) fn attach(
    state: &Fixture,
    surface: &wl_surface::WlSurface,
    size: [i32; 2],
    scale: i32,
    pixels: &[u8],
    damage: Option<[i32; 4]>,
    qh: &QueueHandle<Fixture>,
) {
    let [width, height] = size;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(pixels).unwrap();
    let pool = state
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
        1u8,
    );
    pool.destroy();
    surface.set_buffer_scale(scale);
    surface.attach(Some(&buffer), 0, 0);
    if let Some([x, y, w, h]) = damage {
        surface.damage(x, y, w, h);
    } else {
        surface.damage_buffer(0, 0, width, height);
    }
    if std::env::var_os("VIVID_TREE_ANIMATE").is_some() && surface.data::<u8>() == Some(&1) {
        surface.frame(qh, ());
    }
    surface.commit();
}
impl Dispatch<wl_buffer::WlBuffer, u8> for Fixture {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &u8,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}
fn pixels(state: &Fixture, size: [i32; 2]) -> Vec<u8> {
    let [width, height] = size;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, a] = state.root_color.unwrap_or([
                (x % 251) as u8,
                (y % 251) as u8,
                (state.viewport.units / 30) as u8,
                255,
            ]);
            pixels.extend_from_slice(&[b, g, r, a]);
        }
    }
    pixels
}
pub(super) fn draw(state: &mut Fixture, qh: &QueueHandle<Fixture>) {
    if !state.viewport.preferred {
        return;
    }
    let first = state.viewport.root.is_none();
    let viewport = viewport(state, qh);
    if first || state.viewport.auto {
        let [w, h] = logical(state);
        viewport.set_destination(w, h);
    }
    let raw = raw(state);
    attach(
        state,
        state.root.as_ref().unwrap(),
        raw,
        state.viewport.scale,
        &pixels(state, raw),
        None,
        qh,
    );
}
pub(super) fn command(state: &mut Fixture, code: u32, qh: &QueueHandle<Fixture>) {
    if [221, 222, 223, 224, 225, 226, 228, 229, 230, 234, 238].contains(&code) {
        state.viewport.auto = false;
    }
    let vp = viewport(state, qh);
    let root = state.root.as_ref().unwrap().clone();
    match code {
        220 => {
            state.root_color = None;
            draw(state, qh);
            return;
        }
        221 | 234 => {
            vp.set_source(8.5, 6.25, 16., 12.);
            vp.set_destination(32, 24);
            if code == 234 {
                state.viewport.callback = Some(root.frame(qh, ()));
            }
        }
        222 => {
            vp.set_source(-1., -1., -1., -1.);
            vp.set_destination(-1, -1);
        }
        223 => {
            vp.set_source(0.25, 0.5, 16.5, 12.25);
            vp.set_destination(33, 24);
        }
        224 => {
            vp.set_source(63.75, 0., 1., 1.);
        }
        225 => {
            vp.set_destination(i32::MAX, 48);
        }
        226 => {
            vp.set_source(0., 0., 16.5, 12.);
            vp.set_destination(-1, -1);
        }
        227 => {
            state.viewport.root.take().unwrap().destroy();
        }
        228 => {
            state.viewport.raw = Some([65, 49]);
            state.viewport.scale = 2;
            vp.set_source(-1., -1., -1., -1.);
            vp.set_destination(32, 24);
            draw(state, qh);
            return;
        }
        229 => {
            vp.set_source(32.25, 24.25, 0.25, 0.25);
            vp.set_destination(1, 1);
        }
        230 => {
            vp.set_source(32.5, 0., 0.25, 1.);
        }
        231 => {
            let child = state.compositor.as_ref().unwrap().create_surface(qh, 2);
            let role = state
                .subcompositor
                .as_ref()
                .unwrap()
                .get_subsurface(&child, &root, qh, ());
            role.set_position(4, 3);
            let viewport = state
                .viewport
                .manager
                .as_ref()
                .unwrap()
                .get_viewport(&child, qh, ());
            viewport.set_source(4., 2., 8., 6.);
            viewport.set_destination(8, 6);
            let mut pixels = Vec::with_capacity(16 * 12 * 4);
            for y in 0..12 {
                for x in 0..16 {
                    pixels.extend_from_slice(&[10, 20 + y, 200 + x, 255]);
                }
            }
            attach(state, &child, [16, 12], 1, &pixels, None, qh);
            state.child = Some(ChildSurface {
                surface: child,
                role,
            });
            state.viewport.child = Some(viewport);
        }
        232 => {
            state
                .viewport
                .child
                .as_ref()
                .unwrap()
                .set_destination(16, 12);
        }
        233 => {
            state.child.as_ref().unwrap().surface.commit();
        }
        235 => {
            let mut pixels = pixels(state, [64, 48]);
            pixels[(7 * 64 + 9) * 4..][..4].copy_from_slice(&[20, 50, 230, 255]);
            pixels[(40 * 64 + 60) * 4..][..4].copy_from_slice(&[0, 0, 250, 255]);
            attach(state, &root, [64, 48], 1, &pixels, Some([2, 2, 2, 2]), qh);
            return;
        }
        236 => {
            root.set_buffer_scale(2);
        }
        237 => {
            root.attach(None, 0, 0);
        }
        238 => {
            vp.set_source(8., 6., 16., 12.);
            vp.set_destination(32, 24);
        }
        239 => {
            state.viewport.preference.take().unwrap().destroy();
        }
        240 => {
            state.viewport.auto = false;
            vp.set_source(8., 6., 16., 12.);
            vp.set_destination(48, 16);
        }
        241 => {
            state.viewport.auto = true;
            state.viewport.raw = None;
            state.viewport.scale = 1;
            state.root_color = None;
            vp.set_source(-1., -1., -1., -1.);
            draw(state, qh);
            return;
        }
        242 => {
            state.viewport.auto = false;
            vp.set_source(-1., -1., -1., -1.);
            vp.set_destination(128, 96);
        }
        243 => {
            let mut pixels = pixels(state, [64, 48]);
            pixels[(64 + 1) * 4..][..4].copy_from_slice(&[20, 50, 230, 255]);
            attach(state, &root, [64, 48], 1, &pixels, Some([2, 2, 2, 2]), qh);
            return;
        }
        _ => unreachable!(),
    }
    root.commit();
}
pub(super) fn callback(
    state: &mut Fixture,
    callback: &wl_callback::WlCallback,
    qh: &QueueHandle<Fixture>,
) -> bool {
    if state.viewport.callback.as_ref() != Some(callback) {
        return false;
    }
    state.viewport.callback = None;
    state.root_color = Some([81, 82, 83, 255]);
    draw(state, qh);
    true
}
impl Dispatch<WpFractionalScaleV1, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: fractional::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let fractional::Event::PreferredScale { scale } = event {
            state.viewport.preferred = true;
            state.viewport.units = scale;
            if state.viewport.configured {
                draw(state, qh);
            }
        }
    }
}
delegate_noop!(Fixture: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Fixture: ignore WpViewporter);
delegate_noop!(Fixture: ignore WpViewport);
delegate_noop!(Fixture: ignore WpFractionalScaleManagerV1);

fn start() -> (WaylandHost, Client, Frame) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let child = client_with(&host, &[("VIVID_TREE_VIEWPORT", "1")]);
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    (host, child, frame)
}
pub(super) fn helper(
    connection: &Connection,
    queue: &mut wayland_client::EventQueue<Fixture>,
    state: &mut Fixture,
) {
    queue.roundtrip(state).unwrap();
    queue.roundtrip(state).unwrap();
    let qh = queue.handle();
    let vp = viewport(state, &qh);
    vp.set_source(63.75, 0., 1., 1.);
    vp.set_destination(8, 6);
    attach(
        state,
        state.root.as_ref().unwrap(),
        [16, 12],
        1,
        &[0, 0, 0, 255].repeat(16 * 12),
        None,
        &qh,
    );
    connection.flush().unwrap();
    queue
        .roundtrip(state)
        .expect_err("out-of-buffer helper viewport was accepted");
}
#[test]
fn odd_logical_sizes_use_protocol_rounding_and_keep_fractional_input_local() {
    let mut host = WaylandHost::new(65, 49).unwrap();
    host.resize_with_scale(65, 49, 180).unwrap();
    let _child = client_with(
        &host,
        &[
            ("VIVID_TREE_VIEWPORT", "1"),
            ("VIVID_TREE_VIEWPORT_ODD", "1"),
        ],
    );
    let first = next(&mut host);
    assert_eq!(
        (first.width, first.height, first.logical_size),
        (98, 74, [65, 49])
    );
    host.set_active(true);
    present(&mut host, &first);
    host.resize_with_scale(65, 49, 150).unwrap();
    let resized = next(&mut host);
    assert_eq!(
        (resized.width, resized.height, resized.logical_size),
        (81, 61, [65, 49])
    );
    present(&mut host, &resized);
    host.resize_with_scale(65, 49, 150).unwrap();
    assert_eq!(host.input_target().unwrap().0, resized.target);
    assert!(
        host.input(resized.target, Input::Motion { x: 64.9, y: 48.9 })
            .unwrap()
    );
    assert!(
        host.input(
            resized.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    assert_eq!(pixel(&next(&mut host), 0, 0), [101, 64, 48, 255]);
}
#[test]
fn malformed_helper_viewports_and_later_scale_notifications_preserve_the_root_owner() {
    let (mut host, _root, old) = start();
    let mut helper = client_with(&host, &[("VIVID_TREE_VIEWPORT_HELPER", "1")]);
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
        assert!(Instant::now() < deadline, "viewport helper did not retire");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!host.root_failed());
    assert_eq!(host.input_target().unwrap().0, old.target);
    host.resize_with_scale(64, 48, 180).unwrap();
    let resized = next(&mut host);
    assert_eq!((resized.width, resized.height), (96, 72));
    assert!(!host.root_failed());
}
#[test]
fn viewport_crops_and_metadata_changes_preserve_owned_pixels_and_revoke_old_input() {
    let (mut host, _child, old) = start();
    let stats = host.readback_stats();
    key(&mut host, old.target, 221);
    let crop = next(&mut host);
    assert_eq!(crop.logical_size, [32, 24]);
    assert_eq!((crop.width, crop.height), (64, 48));
    assert_eq!(pixel(&crop, 0, 0), [8, 6, 4, 255]);
    assert_eq!(pixel(&crop, 63, 47), [24, 18, 4, 255]);
    assert_eq!(host.readback_stats(), stats);
    assert_ne!(old.target, crop.target);
    assert!(
        !host
            .input(old.target, Input::Motion { x: 1., y: 1. })
            .unwrap()
    );
    assert_eq!(pixel(&old, 0, 0), [0, 0, 4, 255]);
    present(&mut host, &crop);
    key(&mut host, crop.target, 223);
    let fractional = next(&mut host);
    assert_eq!(fractional.logical_size, [33, 24]);
    present(&mut host, &fractional);
    key(&mut host, fractional.target, 227);
    let destroyed = next(&mut host);
    assert_eq!(destroyed.logical_size, [64, 48]);
    assert_eq!(destroyed.rgba, old.rgba);
    assert_eq!(host.readback_stats(), stats);
}
#[test]
fn fractional_scale_notifications_resize_real_buffers_and_require_fresh_presentation() {
    let (mut host, _child, old) = start();
    host.resize_with_scale(64, 48, 180).unwrap();
    assert!(host.input_target().is_none());
    let first = next(&mut host);
    assert_eq!(
        (first.width, first.height, first.logical_size),
        (96, 72, [64, 48])
    );
    assert_eq!(pixel(&first, 0, 0), [0, 0, 6, 255]);
    assert_ne!(old.target, first.target);
    assert!(
        !host
            .input(old.target, Input::Motion { x: 1., y: 1. })
            .unwrap()
    );
    present(&mut host, &first);
    host.resize_with_scale(64, 48, 150).unwrap();
    let next_scale = next(&mut host);
    assert_eq!((next_scale.width, next_scale.height), (80, 60));
    assert_eq!(pixel(&next_scale, 0, 0), [0, 0, 5, 255]);
    present(&mut host, &next_scale);
    for (width, height, scale) in [
        (0, 48, 180),
        (64, 48, 0),
        (64, 48, 481),
        (2048, 48, 180),
        (u32::MAX, 48, 480),
    ] {
        assert!(host.resize_with_scale(width, height, scale).is_err());
        assert_eq!(host.input_target().unwrap().0, next_scale.target);
    }
    key(&mut host, next_scale.target, 220);
    assert_eq!((next(&mut host).width, next_scale.height), (80, 60));
}
#[test]
fn fractional_buffer_extent_at_integer_scale_is_legal_with_a_viewport_destination() {
    let (mut host, _child, old) = start();
    key(&mut host, old.target, 228);
    let odd = next(&mut host);
    assert_eq!(
        (odd.width, odd.height, odd.logical_size),
        (65, 49, [32, 24])
    );
    present(&mut host, &odd);
    key(&mut host, odd.target, 229);
    let corner = next(&mut host);
    assert_eq!(corner.logical_size, [1, 1]);
    assert_eq!(pixel(&corner, 0, 0), [64, 48, 4, 255]);
    assert_eq!(pixel(&corner, 64, 48), [64, 48, 4, 255]);
}
#[test]
fn cropped_attachments_snapshot_ignored_pixels_before_viewport_only_revelation() {
    let (mut host, _child, old) = start();
    key(&mut host, old.target, 238);
    let cropped = next(&mut host);
    present(&mut host, &cropped);
    let before = host.readback_stats();
    key(&mut host, cropped.target, 235);
    let patch = next(&mut host);
    assert_eq!(patch.target, cropped.target);
    assert_eq!(
        host.readback_stats().pixel_bytes - before.pixel_bytes,
        64 * 48 * 4
    );
    assert_eq!(host.readback_stats().full_reads - before.full_reads, 1);
    assert_eq!(pixel(&patch, 4, 4), [230, 50, 20, 255]);
    assert_eq!(pixel(&cropped, 4, 4), [9, 7, 4, 255]);
    present(&mut host, &patch);
    key(&mut host, patch.target, 222);
    let whole = next(&mut host);
    assert_eq!(pixel(&whole, 60, 40), [250, 0, 0, 255]);
    assert_eq!(pixel(&whole, 9, 7), [230, 50, 20, 255]);
}
#[test]
fn full_source_viewport_damage_reads_only_the_changed_physical_pixel() {
    let (mut host, _child, old) = start();
    key(&mut host, old.target, 242);
    let scaled = next(&mut host);
    assert_eq!(scaled.logical_size, [128, 96]);
    present(&mut host, &scaled);
    let before = host.readback_stats();
    key(&mut host, scaled.target, 243);
    let patch = next(&mut host);
    assert_eq!(patch.target, scaled.target);
    assert_eq!(host.readback_stats().pixel_bytes - before.pixel_bytes, 4);
    assert_eq!(
        host.readback_stats().partial_reads - before.partial_reads,
        1
    );
    assert_eq!(pixel(&patch, 1, 1), [230, 50, 20, 255]);
    assert_eq!(pixel(&scaled, 1, 1), [1, 1, 4, 255]);
    assert_eq!(pixel(&patch, 60, 40), [60, 40, 4, 255]);
}
#[test]
fn viewport_callbacks_wait_for_the_hosts_presentation_acknowledgement() {
    let (mut host, _child, old) = start();
    key(&mut host, old.target, 234);
    let cropped = next(&mut host);
    for _ in 0..10 {
        assert!(host.dispatch().unwrap().is_none());
    }
    present(&mut host, &cropped);
    assert_eq!(pixel(&next(&mut host), 0, 0), [81, 82, 83, 255]);
}
#[test]
fn malformed_viewports_retire_only_their_owner_including_fractional_buffer_bounds() {
    let (mut peer, _peer, other) = start();
    for code in [224, 225, 226, 230] {
        let (mut host, _child, root) = start();
        let target = if code == 230 {
            key(&mut host, root.target, 228);
            let odd = next(&mut host);
            present(&mut host, &odd);
            odd.target
        } else {
            root.target
        };
        key(&mut host, target, code);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !host.root_failed() {
            host.dispatch().unwrap();
            assert!(Instant::now() < deadline);
        }
        assert_eq!(peer.input_target().unwrap().0, other.target);
        assert!(!peer.root_failed());
    }
    key(&mut peer, other.target, 221);
    assert_eq!(next(&mut peer).logical_size, [32, 24]);
}
#[test]
fn synchronized_child_viewports_keep_parent_commit_boundaries_and_local_input_coordinates() {
    let (mut host, _child, old) = start();
    key(&mut host, old.target, 231);
    let child = next(&mut host);
    assert_eq!(pixel(&child, 4, 3), [204, 22, 10, 255]);
    assert_eq!(pixel(&child, 12, 3), pixel(&old, 12, 3));
    present(&mut host, &child);
    key(&mut host, child.target, 232);
    let pending = next(&mut host);
    assert_eq!(pending.rgba, child.rgba);
    assert_eq!(pending.target, child.target);
    present(&mut host, &pending);
    key(&mut host, pending.target, 233);
    let applied = next(&mut host);
    assert_ne!(applied.target, pending.target);
    assert_eq!(pixel(&applied, 12, 3), [208, 22, 10, 255]);
    present(&mut host, &applied);
    assert!(
        host.input(applied.target, Input::Motion { x: 5., y: 4. })
            .unwrap()
    );
    assert!(
        host.input(
            applied.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    assert_eq!(pixel(&next(&mut host), 0, 0), [102, 1, 1, 255]);
}
