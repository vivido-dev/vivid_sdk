//! Native damage readback with immutable old frames and qualified input.

// Rust guideline compliant 2026-10-07
use super::*;

#[derive(Default)]
pub(super) struct DamageFixture {
    pixels: Vec<u8>,
    extent: [i32; 2],
}

struct Attachment<'a> {
    extent: [i32; 2],
    scale: i32,
    pixels: &'a [u8],
    format: wl_shm::Format,
    damage: Option<(bool, [i32; 4])>,
}

fn publish(
    state: &Fixture,
    surface: &wl_surface::WlSurface,
    attachment: Attachment<'_>,
    qh: &QueueHandle<Fixture>,
) {
    let Attachment {
        extent: [width, height],
        scale,
        pixels,
        format,
        damage,
    } = attachment;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(pixels).unwrap();
    let pool = state
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), width * height * 4, qh, ());
    let buffer = pool.create_buffer(0, width, height, width * 4, format, qh, ());
    pool.destroy();
    surface.set_buffer_scale(scale);
    surface.attach(Some(&buffer), 0, 0);
    if let Some((buffer, [x, y, w, h])) = damage {
        if buffer {
            surface.damage_buffer(x, y, w, h);
        } else {
            surface.damage(x, y, w, h);
        }
    }
    surface.commit();
}

pub(super) fn fixture_key(state: &mut Fixture, code: u32, qh: &QueueHandle<Fixture>) {
    let scale =
        state
            .root_scale
            .unwrap_or(if std::env::var_os("VIVID_TREE_ROOT_SCALE").is_some() {
                2
            } else {
                1
            });
    if code == 145 {
        let new_scale = if scale == 1 { 2 } else { 1 };
        state.root_scale = Some(new_scale);
        let root = state.root.as_ref().unwrap();
        root.set_buffer_scale(new_scale);
        root.commit();
        return;
    }
    if code == 147 {
        let child = &state.child.as_ref().unwrap().surface;
        let mut pixels = [0, 0, 128, 128].repeat(16 * 12);
        for x in 0..2 {
            pixels[x * 4..x * 4 + 4].copy_from_slice(&[0, 0, 240, 255]);
        }
        publish(
            state,
            child,
            Attachment {
                extent: [16, 12],
                scale: 2,
                pixels: &pixels,
                format: wl_shm::Format::Argb8888,
                damage: Some((false, [0, 0, 1, 1])),
            },
            qh,
        );
        let at = (8 * 16 + 10) * 4;
        pixels[at..at + 4].copy_from_slice(&[0, 240, 0, 255]);
        publish(
            state,
            child,
            Attachment {
                extent: [16, 12],
                scale: 1,
                pixels: &pixels,
                format: wl_shm::Format::Argb8888,
                damage: Some((false, [10, 8, 1, 1])),
            },
            qh,
        );
        state.root.as_ref().unwrap().commit();
        return;
    }
    let extent = if state.root_extent == [0, 0] {
        [64, 48]
    } else {
        state.root_extent
    };
    let [width, height] = extent.map(|value| value * scale);
    if state.damage.extent != [width, height] {
        let [r, g, b, a] = state.root_color.unwrap_or([10, 20, 30, 255]);
        state.damage.pixels = [b, g, r, a].repeat(width as usize * height as usize);
        state.damage.extent = [width, height];
    }
    let damage = match code {
        140 => Some((true, [1, 1, 1, 1])),
        142 => Some((false, [1, 1, 1, 1])),
        _ => None,
    };
    if let Some((buffer, [x, y, w, h])) = damage {
        let scale = if buffer { 1 } else { scale };
        for y in y * scale..(y + h) * scale {
            for x in x * scale..(x + w) * scale {
                let at = (y * width + x) as usize * 4;
                state.damage.pixels[at..at + 4].copy_from_slice(&[0, 0, 240, 255]);
            }
        }
    }
    let format = if code == 148 {
        wl_shm::Format::Xrgb8888
    } else {
        wl_shm::Format::Argb8888
    };
    let mut pixels = state.damage.pixels.clone();
    if code == 148 {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 0;
        }
    }
    publish(
        state,
        state.root.as_ref().unwrap(),
        Attachment {
            extent: [width, height],
            scale,
            pixels: &pixels,
            format,
            damage,
        },
        qh,
    );
}

#[test]
fn one_pixel_damage_reads_only_that_pixel_and_preserves_the_old_frame() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    let before = host.readback_stats();
    key(&mut host, initial.target, 140);
    let changed = next(&mut host);
    assert_eq!(pixel(&changed, 1, 1), [240, 0, 0, 255]);
    assert_eq!(pixel(&initial, 1, 1), [10, 20, 30, 255]);
    assert_eq!(changed.target, initial.target);
    let after = host.readback_stats();
    assert_eq!(after.pixel_bytes - before.pixel_bytes, 4);
    assert_eq!(after.partial_reads - before.partial_reads, 1);
    present(&mut host, &changed);
    key(&mut host, changed.target, 141);
    let unchanged = next(&mut host);
    assert_eq!(unchanged.rgba, changed.rgba);
    assert_eq!(host.readback_stats().pixel_bytes, after.pixel_bytes);
    assert_eq!(
        host.readback_stats().reused_buffers,
        after.reused_buffers + 1
    );
}

#[test]
fn surface_damage_scales_to_pixels_but_buffer_damage_does_not() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client_with(&host, &[("VIVID_TREE_ROOT_SCALE", "1")]);
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    let before = host.readback_stats();
    key(&mut host, frame.target, 142);
    let changed = next(&mut host);
    assert_eq!(pixel(&changed, 2, 2), [240, 0, 0, 255]);
    assert_eq!(pixel(&changed, 3, 3), [240, 0, 0, 255]);
    assert_eq!(pixel(&changed, 4, 4), [10, 20, 30, 255]);
    assert_eq!(host.readback_stats().pixel_bytes - before.pixel_bytes, 16);
    present(&mut host, &changed);
    let before = host.readback_stats();
    key(&mut host, changed.target, 140);
    let changed = next(&mut host);
    assert_eq!(pixel(&changed, 1, 1), [240, 0, 0, 255]);
    assert_eq!(pixel(&changed, 2, 2), [240, 0, 0, 255]);
    assert_eq!(host.readback_stats().pixel_bytes - before.pixel_bytes, 4);
}

#[test]
fn metadata_scale_changes_remap_owned_pixels_and_new_layouts_force_full_reads() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let original = next(&mut host);
    host.set_active(true);
    present(&mut host, &original);
    let before = host.readback_stats();
    key(&mut host, original.target, 145);
    let remapped = next(&mut host);
    assert_eq!(remapped.logical_size, [32, 24]);
    assert_ne!(remapped.target, original.target);
    assert!(host.input_target().is_none());
    assert_eq!(host.readback_stats(), before);
    present(&mut host, &remapped);
    key(&mut host, remapped.target, 140);
    let resized = next(&mut host);
    assert_eq!([resized.width, resized.height], [128, 96]);
    assert_eq!(host.readback_stats().full_reads, before.full_reads + 1);
    present(&mut host, &resized);
    let before = host.readback_stats();
    key(&mut host, resized.target, 148);
    let opaque = next(&mut host);
    assert_eq!(pixel(&opaque, 1, 1), [240, 0, 0, 255]);
    assert_eq!(host.readback_stats().full_reads, before.full_reads + 1);
}

#[test]
fn coalesced_child_scale_changes_fall_back_to_the_complete_final_buffer() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client(&host);
    let original = next(&mut host);
    host.set_active(true);
    present(&mut host, &original);
    let before = host.readback_stats();
    key(&mut host, original.target, 147);
    let changed = next(&mut host);
    assert_eq!(pixel(&changed, 9, 6), [240, 0, 0, 255]);
    assert_ne!(pixel(&original, 9, 6), [240, 0, 0, 255]);
    assert_eq!(host.readback_stats().full_reads, before.full_reads + 1);
    assert_eq!(
        host.readback_stats().pixel_bytes - before.pixel_bytes,
        16 * 12 * 4
    );
}

#[test]
fn retiring_one_display_preserves_the_other_owners_patch_base() {
    let mut first = WaylandHost::new(64, 48).unwrap();
    let first_child = client(&first);
    let mut second = WaylandHost::new(64, 48).unwrap();
    let _second_child = client(&second);
    let a = next(&mut first);
    let b = next(&mut second);
    assert_ne!(a.target, b.target);
    first.set_active(true);
    second.set_active(true);
    present(&mut first, &a);
    present(&mut second, &b);
    let before = second.readback_stats();
    key(&mut first, a.target, 145);
    let remapped = next(&mut first);
    assert_ne!(remapped.target, a.target);
    assert_eq!(second.input_target().unwrap().0, b.target);
    assert_eq!(second.readback_stats(), before);
    drop(first_child);
    drop(first);
    key(&mut second, b.target, 140);
    let changed = next(&mut second);
    assert_eq!(changed.target, b.target);
    assert_eq!(pixel(&changed, 1, 1), [240, 0, 0, 255]);
    assert_eq!(pixel(&b, 1, 1), [10, 20, 30, 255]);
    assert_eq!(second.readback_stats().pixel_bytes - before.pixel_bytes, 4);
    assert_eq!(
        second.readback_stats().partial_reads,
        before.partial_reads + 1
    );
}
