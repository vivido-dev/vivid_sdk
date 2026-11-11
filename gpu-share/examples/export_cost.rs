//! Isolated native export costs, excluding source rendering and GUI presentation.

// Rust guideline compliant 2026-10-07
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;
    use vivid_gpu_share::Pool;

    // Fixed samples keep before/after runs comparable; warmup excludes first-use compilation.
    const SAMPLES: usize = 24;
    const WARMUP: usize = 4;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL,
        flags: wgpu::InstanceFlags::default(),
        backend_options: wgpu::BackendOptions::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let notification = failed.clone();
    device.on_uncaptured_error(std::sync::Arc::new(move |_| {
        notification.store(true, std::sync::atomic::Ordering::Release);
    }));
    let notification = failed.clone();
    device.set_device_lost_callback(move |_, _| {
        notification.store(true, std::sync::atomic::Ordering::Release);
    });
    println!(
        "{{\"debug_assertions\":{},\"samples_per_case\":{SAMPLES},\"warmup\":{WARMUP},\"cases\":[",
        cfg!(debug_assertions)
    );
    let mut first = true;
    for (name, format) in [
        ("rgba", wgpu::TextureFormat::Rgba8Unorm),
        ("bgra", wgpu::TextureFormat::Bgra8Unorm),
    ] {
        for [width, height] in [[640, 360], [1280, 720], [1920, 1080]] {
            let source = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("export-cost-source"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let pixel: [u8; 4] = if name == "rgba" {
                [80, 30, 10, 128]
            } else {
                [10, 30, 80, 128]
            };
            queue.write_texture(
                source.as_image_copy(),
                &pixel.repeat((width * height) as usize),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                source.size(),
            );
            // Finish the fixed source upload before the measured export operation.
            let upload = queue.submit([]);
            device.poll(wgpu::PollType::Wait {
                submission_index: Some(upload),
                timeout: Some(std::time::Duration::from_secs(5)),
            })?;
            let pool = Pool::new(1)?;
            let mut submit = Vec::with_capacity(SAMPLES);
            let mut complete = Vec::with_capacity(SAMPLES);
            let mut cold = [0.; 2];
            for sample in 0..WARMUP + SAMPLES {
                let started = Instant::now();
                let pending = pool.begin_copy(&device, &queue, &source)?;
                let submitted = started.elapsed().as_secs_f64() * 1000.;
                let frame = pending.wait()?;
                let completed = started.elapsed().as_secs_f64() * 1000.;
                assert_eq!(frame.size(), [width, height]);
                drop(frame);
                assert_eq!(pool.retained(), 0);
                assert!(!failed.load(std::sync::atomic::Ordering::Acquire));
                if sample == 0 {
                    cold = [submitted, completed];
                }
                if sample >= WARMUP {
                    submit.push(submitted);
                    complete.push(completed);
                }
            }
            if !first {
                println!(",");
            }
            first = false;
            print!(
                "{{\"format\":\"{name}\",\"size\":[{width},{height}],\"cold_ms\":{cold:?},\"submission_ms\":{submit:?},\"completion_ms\":{complete:?}}}"
            );
        }
    }
    println!("]}}");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("the export-cost diagnostic requires native macOS Metal".into())
}
