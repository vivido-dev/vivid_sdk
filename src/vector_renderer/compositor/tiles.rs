//! Bounded scratch allocation for the checked tile renderer.

// Rust guideline compliant 2026-10-07

// Vello 0.11 reserves 64 command words per 16x16 tile in an 8-Mword arena.
// A 4096-square dispatch leaves half that arena for dynamic scene commands.
// Reuse one scratch texture; the full target keeps its existing 256-MiB bound.
pub(super) const EDGE: u32 = 4096;

pub(super) struct Scratch {
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
}
impl Scratch {
    pub(super) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vivid.vector.tile"),
            size: wgpu::Extent3d {
                width: width.min(EDGE),
                height: height.min(EDGE),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self { texture, view }
    }
}
