//! Correlate headed animation fixture sprites with native Metal frame timestamps.
// Rust guideline compliant 2026-10-07

use crate::metal_atlas::MetalAtlas;
use block2::RcBlock;
use gpui::Scene;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandBufferStatus, MTLDrawable, MTLOrigin, MTLPixelFormat, MTLRegion,
    MTLSize, MTLTexture,
};
use objc2_quartz_core::CAMetalDrawable;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

fn marker(scene: &Scene, atlas: &MetalAtlas) -> Option<u16> {
    let mut sprites = scene.polychrome_sprites.iter().filter(|sprite| {
        sprite.tile.bounds.size.width.0 >= 300 && sprite.tile.bounds.size.height.0 >= 200
    });
    let tile = sprites.next()?.tile;
    if sprites.next().is_some() {
        return None;
    }
    let texture = atlas.metal_texture(tile.texture_id)?;
    let bounds = tile.bounds;
    let x = usize::try_from(bounds.origin.x.0)
        .ok()?
        .checked_add(usize::try_from(bounds.size.width.0).ok()? * 3 / 4)?;
    let y = usize::try_from(bounds.origin.y.0)
        .ok()?
        .checked_add(usize::try_from(bounds.size.height.0).ok()? / 2)?;
    if x >= texture.width()
        || y >= texture.height()
        || texture.pixelFormat() != MTLPixelFormat::BGRA8Unorm
    {
        return None;
    }
    let mut pixel = [0u8; 4];
    let region = MTLRegion {
        origin: MTLOrigin { x, y, z: 0 },
        size: MTLSize {
            width: 1,
            height: 1,
            depth: 1,
        },
    };
    // SAFETY: The CPU-uploaded atlas is shader-read-only, and this in-bounds BGRA8
    // texel fits in the four-byte destination for the duration of the call.
    unsafe {
        texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            ptr::NonNull::from(&mut pixel).cast(),
            4,
            region,
            0,
        );
    }
    let [b, g, r, _] = pixel;
    ([b, g, r].iter().all(|v| v % 32 == 16) && r.max(g).max(b) - r.min(g).min(b) >= 96)
        .then(|| u16::from(r / 32) * 64 + u16::from(g / 32) * 8 + u16::from(b / 32))
}

pub(super) fn record(
    scene: &Scene,
    atlas: &MetalAtlas,
    commands: &ProtocolObject<dyn MTLCommandBuffer>,
    drawable: &ProtocolObject<dyn CAMetalDrawable>,
) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    static FRAMES: AtomicUsize = AtomicUsize::new(0);
    const LIMIT: usize = 1500;
    if !*ENABLED.get_or_init(|| {
        std::env::var_os("VIVID_PROFILE_ANIMATION_METAL").as_deref()
            == Some(std::ffi::OsStr::new("1"))
    }) || FRAMES.load(Ordering::Relaxed) >= LIMIT
    {
        return;
    }
    let Some(color) = marker(scene, atlas) else {
        return;
    };
    let Ok(id) = FRAMES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        (n < LIMIT).then_some(n + 1)
    }) else {
        return;
    };
    let completed = RcBlock::new(
        move |buffer: ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            // SAFETY: Metal supplies the live command buffer throughout its completion callback.
            let buffer = unsafe { buffer.as_ref() };
            if buffer.status() == MTLCommandBufferStatus::Completed {
                eprintln!(
                    "vv-animation-gpu {id} {:.9} {:.9}",
                    buffer.GPUStartTime(),
                    buffer.GPUEndTime()
                );
            }
        },
    );
    let presented = RcBlock::new(
        move |frame: ptr::NonNull<ProtocolObject<dyn MTLDrawable>>| {
            // SAFETY: Metal supplies the live drawable throughout its presentation callback.
            let stamp = unsafe { frame.as_ref() }.presentedTime();
            eprintln!("vv-animation-present {id} {stamp:.9}");
        },
    );
    // SAFETY: Metal copies both correctly typed blocks before submission.
    // The callbacks own their numeric IDs and borrow no application state.
    unsafe {
        commands.addCompletedHandler(RcBlock::as_ptr(&completed));
        drawable.addPresentedHandler(RcBlock::as_ptr(&presented));
    }
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: The uptime clock writes a timespec to the exclusive reference.
    if unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut time) } == 0 {
        eprintln!(
            "vv-animation-submit {id} {color} {}{:09}",
            time.tv_sec, time.tv_nsec
        );
    }
}
