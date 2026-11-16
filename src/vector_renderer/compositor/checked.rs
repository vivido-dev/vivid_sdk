//! Verified native allocation status before tile copy or cache publication.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
};

// Bound GPU submissions and CPU scene replay per prepared output. This is a
// backend work budget, independent of encoded bytes or physical output size.
const MAX_ATTEMPTS: u64 = 64;
// Vello 0.11 uses 16-square raster tiles. Keep every subdivided origin aligned
// so subdivision does not introduce a new subpixel translation.
const ALIGN: u32 = 16;
// Vello stage bits: flatten(4) is independent of viewport subdivision; the
// other reported allocation stages can be reduced by a smaller dispatch area.
const FLATTEN: u32 = 4;

#[derive(Clone, Copy)]
struct Tile {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}
impl Tile {
    fn split(self) -> Option<[Self; 2]> {
        if self.width >= self.height && self.width > ALIGN {
            let first = ((self.width / 2) / ALIGN).max(1) * ALIGN;
            Some([
                Self {
                    width: first,
                    ..self
                },
                Self {
                    x: self.x + first,
                    width: self.width - first,
                    ..self
                },
            ])
        } else if self.height > ALIGN {
            let first = ((self.height / 2) / ALIGN).max(1) * ALIGN;
            Some([
                Self {
                    height: first,
                    ..self
                },
                Self {
                    y: self.y + first,
                    height: self.height - first,
                    ..self
                },
            ])
        } else {
            None
        }
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "one checked native composition job"
)]
pub(super) async fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut vivid_vello_backend::Renderer,
    scene: &Scene,
    scratch: &tiles::Scratch,
    target: &wgpu::Texture,
    params: &RenderParams,
    metrics: &mut RenderMetrics,
    cancelled: &AtomicBool,
    budget: Option<&WorkBudget>,
) -> Result<(), Error> {
    let mut tiles = VecDeque::new();
    for y in (0..params.height).step_by(tiles::EDGE as usize) {
        for x in (0..params.width).step_by(tiles::EDGE as usize) {
            tiles.push_back(Tile {
                x,
                y,
                width: (params.width - x).min(tiles::EDGE),
                height: (params.height - y).min(tiles::EDGE),
            });
        }
    }
    let mut attempts = 0;
    while let Some(tile) = tiles.pop_front() {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if attempts >= MAX_ATTEMPTS {
            return Err(Error::WorkLimit);
        }
        if budget.is_some_and(|budget| !budget.claim()) {
            // Caller-wide exhaustion grants no evidence against this scene.
            return Err(Error::Cancelled);
        }
        attempts += 1;
        let mut translated = Scene::new();
        translated.append(
            scene,
            Some(Affine::translate((-f64::from(tile.x), -f64::from(tile.y)))),
        );
        let status = checked_dispatch(
            device,
            queue,
            renderer,
            &translated,
            &scratch.view,
            &RenderParams {
                width: tile.width,
                height: tile.height,
                base_color: params.base_color,
                antialiasing_method: params.antialiasing_method,
            },
        )
        .await?;
        metrics.render_passes = metrics.render_passes.saturating_add(1);
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if status != 0 {
            if status & FLATTEN == 0
                && let Some([first, second]) = tile.split()
            {
                metrics.allocation_retries = metrics.allocation_retries.saturating_add(1);
                tiles.push_front(second);
                tiles.push_front(first);
                continue;
            }
            return Err(Error::Arena(status));
        }
        // Fine rendering is queued only after its allocation status is available.
        // Queue order completes this successful tile copy before scratch reuse.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vivid.vector.checked.copy"),
        });
        encoder.copy_texture_to_texture(
            scratch.texture.as_image_copy(),
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: tile.x,
                    y: tile.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: tile.width,
                height: tile.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
    }
    Ok(())
}
#[expect(
    deprecated,
    reason = "Vello 0.11 exposes actual allocation status only through this diagnostic API"
)]
async fn checked_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut vivid_vello_backend::Renderer,
    scene: &Scene,
    view: &wgpu::TextureView,
    params: &RenderParams,
) -> Result<u32, Error> {
    let params = vivid_vello_backend::RenderParams {
        width: params.width,
        height: params.height,
        base_color: params.base_color,
        antialiasing_method: match params.antialiasing_method {
            AaConfig::Area => vivid_vello_backend::AaConfig::Area,
            AaConfig::Msaa8 => vivid_vello_backend::AaConfig::Msaa8,
            AaConfig::Msaa16 => vivid_vello_backend::AaConfig::Msaa16,
        },
    };
    let status = renderer
        .render_to_texture_async(
            device,
            queue,
            scene,
            view,
            &params,
            vivid_vello_backend::low_level::DebugLayers::none(),
        )
        .await
        .map_err(|error| Error::Backend(Box::new(error)))?;
    let status = status.ok_or(Error::InvalidInput(
        "native allocation status is unavailable",
    ))?;
    // compose() always encodes the nonempty outer clip rectangle. A successful
    // tile therefore has line evidence even for transparent/empty output. A
    // zeroed download after a silent native command failure is not GPU proof.
    allocation_status(status.failed, status.lines)
}

fn allocation_status(failed: u32, lines: u32) -> Result<u32, Error> {
    if failed == 0 && lines == 0 {
        return Err(Error::Backend(Box::new(std::io::Error::other(
            "native GPU dispatch produced no clip-allocation evidence",
        ))));
    }
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroed_native_download_is_not_success_or_source_fault_evidence() {
        assert!(matches!(allocation_status(0, 0), Err(Error::Backend(_))));
        assert_eq!(allocation_status(0, 4).unwrap(), 0);
        // Real allocation failures retain their typed status for the checked
        // subdivision/refusal policy; a zeroed download cannot manufacture one.
        assert_eq!(allocation_status(2, 8200).unwrap(), 2);
        assert_eq!(allocation_status(4, 2_097_153).unwrap(), 4);
    }
}
