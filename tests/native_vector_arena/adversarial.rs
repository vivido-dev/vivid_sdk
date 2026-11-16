//! Checked pressure near distinct wire ceilings, with an independent interior oracle.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_sdk::overlay::Command;
use vivid_sdk::vector_renderer::compositor::{Drawing, Error};

#[derive(Clone, Copy, Debug)]
enum Shape {
    Cubics,
    Clips,
    Segments,
}

fn circle() -> Path {
    Path::ellipse(Rect::new(128., 128., 768., 768.).unwrap()).unwrap()
}

fn winding(loops: usize) -> Path {
    assert!((1..=682).contains(&loops));
    let single = circle();
    assert_eq!(single.segments.len(), 6);
    let mut path = Path {
        segments: Vec::with_capacity(4096),
        even_odd: false,
    };
    for _ in 0..loops {
        path.segments.extend(single.segments.iter().cloned());
    }
    if loops == 682 {
        let degenerate = Path::builder()
            .move_to(512., 512.)
            .line_to(512., 512.)
            .line_to(512., 512.)
            .close()
            .build()
            .unwrap();
        path.segments.extend(degenerate.segments);
        assert_eq!(
            path.segments.len(),
            vivid_protocol::vector::MAX_PATH_SEGMENTS
        );
    }
    path.validate().unwrap();
    path
}

fn pressure(shape: Shape) -> Canvas {
    let mut canvas = Canvas::new();
    match shape {
        Shape::Cubics => {
            let path = circle();
            // Approach the 256 KiB helper record ceiling with real cubic edges.
            // The exact encoded size is checked below, independently of commands.
            for _ in 0..960 {
                canvas
                    .fill(path.clone(), Brush::Solid(Color(0x00ff0080)))
                    .unwrap();
            }
        }
        Shape::Clips => {
            // Thirty-two live stack frames and 4,096 commands, including restores.
            // Their intersection is the exactly aligned central 512-square region.
            for index in 1..=32 {
                canvas.push(Command::Save).unwrap();
                let inset = f64::from(index * 8);
                canvas
                    .push(Command::Clip(
                        Path::rectangle(
                            Rect::new(inset, inset, 1024. - 2. * inset, 1024. - 2. * inset)
                                .unwrap(),
                        )
                        .unwrap(),
                    ))
                    .unwrap();
            }
            for _ in 0..4000 {
                canvas
                    .fill(
                        Path::rectangle(Rect::new(0., 0., 1024., 1024.).unwrap()).unwrap(),
                        Brush::Solid(Color(0x0000ff80)),
                    )
                    .unwrap();
            }
            for _ in 0..32 {
                canvas.push(Command::Restore).unwrap();
            }
            assert_eq!(
                canvas.commands().len(),
                vivid_protocol::vector::MAX_COMMANDS
            );
        }
        Shape::Segments => {
            // Repeated same-direction contours preserve nonzero circle coverage
            // while exhausting one path's segment allowance, rather than fills.
            canvas
                .fill(winding(682), Brush::Solid(Color(0x00ff00ff)))
                .unwrap();
        }
    }
    canvas.validate().unwrap();
    let bytes = canvas.encode().unwrap().len();
    assert!(bytes + 12 <= 256 * 1024, "{shape:?} helper bytes={bytes}");
    let segments: usize = canvas
        .commands()
        .iter()
        .map(|c| match c {
            Command::Fill(p, _) | Command::Clip(p) => p.segments.len(),
            _ => 0,
        })
        .sum();
    eprintln!(
        "Adversarial scene: shape={shape:?}, commands={}, path_segments={segments}, encoded_bytes={bytes}",
        canvas.commands().len()
    );
    canvas
}

#[test]
fn circle_oracle_band_bounds_the_actual_cubic_geometry() {
    use vivid_protocol::vector::Segment;
    let mut start = (896., 512.);
    let radius = 384.;
    for segment in circle().segments {
        let Segment::Cubic(a, b, end) = segment else {
            continue;
        };
        let controls = [
            start,
            (a.x.get(), a.y.get()),
            (b.x.get(), b.y.get()),
            (end.x.get(), end.y.get()),
        ];
        for pair in controls.windows(2) {
            assert!((pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1) <= radius);
        }
        let mut deviation: f64 = 0.;
        for index in 0..=16384 {
            let t = f64::from(index) / 16384.;
            let u = 1. - t;
            let weights = [u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t];
            let x: f64 = controls
                .iter()
                .zip(weights)
                .map(|(p, w)| (p.0 - 512.) * w)
                .sum();
            let y: f64 = controls
                .iter()
                .zip(weights)
                .map(|(p, w)| (p.1 - 512.) * w)
                .sum();
            deviation = deviation.max((x.hypot(y) - radius).abs());
        }
        // The derivative is a convex combination of three control deltas,
        // each bounded by radius. This covers every point between samples.
        assert!(deviation + 3. * radius / 16384. < 0.001 * radius);
        start = controls[3];
    }
}

struct Fixture {
    _presenter: Arc<VirtualVivid>,
    host: NativeVectorHost,
    sessions: Vec<OverlaySession>,
    windows: Vec<Arc<OverlayWindow>>,
}
impl Fixture {
    fn new(heavy: usize) -> io::Result<Self> {
        let presenter = Arc::new(VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0")?,
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )?);
        presenter.update_metrics(1, 1024, 1024, (1, 1));
        let host = NativeVectorHost::attach(presenter.clone(), "")?;
        let mut sessions = Vec::with_capacity(heavy + 1);
        let mut windows = Vec::with_capacity(heavy + 1);
        for index in 0..=heavy {
            let session = connect(&presenter)?;
            let bounds = if index == heavy {
                Rect::new(960., 0., 64., 64.).unwrap()
            } else {
                Rect::new(0., 0., 1024., 1024.).unwrap()
            };
            windows.push(Arc::new(session.create_window(
                OverlayWindowOptions::new(bounds, WindowMode::Floating),
            )?));
            sessions.push(session);
        }
        Ok(Self {
            _presenter: presenter,
            host,
            sessions,
            windows,
        })
    }
    fn frame(&mut self, revisions: &[Option<u64>], scale: f64) -> io::Result<VectorFrame> {
        let end = Instant::now() + WAIT;
        loop {
            let frame = self.host.prepare(1, scale)?;
            if frame.drawings().len() == revisions.iter().flatten().count()
                && frame.drawings().iter().all(|drawing| {
                    let owner = drawing.window.context().session().session_id();
                    revisions.iter().enumerate().any(|(index, revision)| {
                        owner == index as u64 + 1 && *revision == Some(drawing.submission.revision)
                    })
                })
            {
                return Ok(frame);
            }
            assert!(Instant::now() < end, "adversarial source frame stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn poll(gpu: &mut Offscreen, drawings: &[Drawing], side: u32) -> io::Result<Pixels> {
    assert!(gpu.render(drawings, side, side)?);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pixels) = gpu.poll()? {
            return Ok(pixels);
        }
        assert!(
            Instant::now() < end,
            "adversarial checked completion stalled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn checked(
    gpu: &mut Offscreen,
    drawings: &[Drawing],
    side: u32,
    peer: &OverlayWindow,
    label: &str,
) -> io::Result<Option<Pixels>> {
    let before = gpu.metrics();
    let stop = AtomicBool::new(false);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let start = Instant::now();
    let result = std::thread::scope(|scope| {
        let thread = scope.spawn(|| -> io::Result<(usize, u128)> {
            let mut count = 0usize;
            let mut maximum = 0;
            let mut ready = Some(ready_tx);
            while !stop.load(Ordering::Acquire) {
                let begin = Instant::now();
                let status = peer.reconcile()?;
                assert_eq!(
                    (status.accepted_revision, status.presented_revision),
                    (2, 1)
                );
                maximum = maximum.max(begin.elapsed().as_micros());
                count += 1;
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok((count, maximum))
        });
        ready_rx
            .recv_timeout(WAIT)
            .expect("peer control did not start");
        let result = poll(gpu, drawings, side);
        stop.store(true, Ordering::Release);
        let controls = thread.join().expect("peer control panicked")?;
        Ok::<_, io::Error>((result, controls))
    })?;
    let passes = gpu.metrics().render_passes - before.render_passes;
    let retries = gpu.metrics().allocation_retries - before.allocation_retries;
    assert!((1..=64).contains(&passes));
    assert!(!gpu.is_failed());
    assert!(
        result.1.0 > 1,
        "peer control did not advance during checked work"
    );
    let status = match result.0.as_ref() {
        Ok(_) => "complete".to_owned(),
        Err(error) => match error.get_ref().and_then(|e| e.downcast_ref::<Error>()) {
            Some(Error::WorkLimit) => {
                assert_eq!(passes, 64);
                "work-limit".to_owned()
            }
            Some(Error::Arena(flags)) => format!("arena-{flags}"),
            _ => {
                return Err(io::Error::other(format!(
                    "unexpected checked failure for {label}: {error}"
                )));
            }
        },
    };
    eprintln!(
        "Adversarial checked job: label={label}, side={side}, status={status}, passes={passes}, retries={retries}, controls={}, maximum_control_us={}, elapsed_ms={}",
        result.1.0,
        result.1.1,
        start.elapsed().as_millis()
    );
    match result.0 {
        Ok(image) => Ok(Some(image)),
        Err(_) => {
            assert!(gpu.poll()?.is_none(), "refusal returned stale pixels");
            Ok(None)
        }
    }
}

fn pixels(image: &Pixels, shapes: &[Shape], peer: Option<[u8; 4]>, fill: Option<[u8; 4]>) {
    assert_eq!(image.width, image.height);
    assert!([1024, 8192].contains(&image.width));
    let side = image.width;
    let scale = f64::from(side) / 1024.;
    let radius = 384. * scale;
    // A cubic circle deviates by less than 0.001 radius from an ideal circle.
    // Exclude only that conservative band plus two physical antialias pixels.
    // Exact edge-AA equivalence is deliberately not inferred from this oracle.
    let band = 2. + 0.001 * radius;
    let has_circle = shapes.iter().any(|s| !matches!(s, Shape::Clips));
    let has_clips = shapes.iter().any(|s| matches!(s, Shape::Clips));
    let (mut classified, mut excluded) = (0usize, 0usize);
    assert_eq!(image.rgba.len(), side as usize * side as usize * 4);
    for (index, actual) in image.rgba.as_chunks::<4>().0.iter().enumerate() {
        let x = index % side as usize;
        let y = index / side as usize;
        let expected = if let Some(color) =
            peer.filter(|_| x >= (960. * scale) as usize && y < (64. * scale) as usize)
        {
            color
        } else if let Some(fill) = fill {
            fill
        } else {
            let dx = x as f64 + 0.5 - 512. * scale;
            let dy = y as f64 + 0.5 - 512. * scale;
            let distance = dx * dx + dy * dy;
            let clipped = has_clips
                && (256. * scale..768. * scale).contains(&(x as f64 + 0.5))
                && (256. * scale..768. * scale).contains(&(y as f64 + 0.5));
            if has_circle && distance < (radius - band).powi(2) {
                [0, 255, 0, 255]
            } else if clipped {
                [0, 0, 255, 255]
            } else if has_circle && distance <= (radius + band).powi(2) {
                excluded += 1;
                continue;
            } else {
                [0; 4]
            }
        };
        // Straight-alpha RGB at zero alpha is unspecified and invisible. Keep
        // the geometry oracle strict about alpha, including outside the circle.
        if expected[3] == 0 {
            assert_eq!(
                actual[3], 0,
                "outside alpha at {x},{y} in {side}-square output: {actual:?}"
            );
        } else {
            assert_eq!(
                *actual, expected,
                "analytical pixel {x},{y} in {side}-square output"
            );
        }
        classified += 1;
    }
    assert!(classified * 100 >= side as usize * side as usize * 98);
    eprintln!(
        "Adversarial pixel oracle: side={side}, classified={classified}, excluded_edge_pixels={excluded}, peer={}, solid={}",
        peer.is_some(),
        fill.is_some()
    );
}

fn exercise(shapes: &[Shape]) -> io::Result<()> {
    let mut f = Fixture::new(shapes.len())?;
    let heavy = shapes.len();
    let mut gpu = pollster::block_on(Offscreen::new())?;
    let mut private = gpu.fork()?;
    let initial: Vec<_> = f
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            window.submit(scene(
                1,
                if index == heavy {
                    0x0000ffff
                } else {
                    0xff0000ff
                },
                1024.,
            ))
        })
        .collect::<io::Result<_>>()?;
    let before = f.frame(&vec![Some(1); heavy + 1], 1.)?;
    let image = poll(&mut gpu, before.drawings(), 1024)?;
    pixels(&image, &[], Some([0, 0, 255, 255]), Some([255, 0, 0, 255]));
    assert!(f.host.presented(&before)?);
    for receipt in &initial {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
    }
    drop(before);
    let pending: Vec<_> = f
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            window.submit(if index == heavy {
                scene(1, 0xff00ffff, 64.)
            } else {
                pressure(shapes[index])
            })
        })
        .collect::<io::Result<_>>()?;
    let current = f.frame(&vec![Some(2); heavy + 1], 1.)?;
    let first = current.drawings()[0].submission;
    for drawing in current.drawings() {
        assert_eq!(drawing.submission.address, first.address);
        assert_eq!(drawing.submission.track_id, first.track_id);
    }
    assert_eq!(
        current
            .drawings()
            .iter()
            .map(|d| d.window)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        heavy + 1
    );
    if let Some(image) = checked(
        &mut gpu,
        current.drawings(),
        1024,
        &f.windows[heavy],
        "mixed-small",
    )? {
        pixels(&image, shapes, Some([255, 0, 255, 255]), None);
    }
    // Successful validation grants no display authority. Retain every original
    // pending while exercising fresh target-specific private proofs.
    for receipt in &pending {
        assert_eq!(receipt.wait(Duration::ZERO)?, None);
    }
    drop(current);
    let large = f.frame(&vec![Some(2); heavy + 1], 8.)?;
    if let Some(image) = checked(
        &mut private,
        large.drawings(),
        8192,
        &f.windows[heavy],
        "mixed-large",
    )? {
        pixels(&image, shapes, Some([255, 0, 255, 255]), None);
    }
    let mut refused = None;
    for (index, shape) in shapes.iter().enumerate() {
        let drawing = large
            .drawings()
            .iter()
            .find(|d| d.window.context().session().session_id() == index as u64 + 1)
            .unwrap();
        if let Some(image) = checked(
            &mut private,
            std::slice::from_ref(drawing),
            8192,
            &f.windows[heavy],
            &format!("isolated-{shape:?}"),
        )? {
            pixels(&image, std::slice::from_ref(shape), None, None);
        } else {
            refused.get_or_insert((index, drawing.window));
        }
    }
    for receipt in &pending {
        assert_eq!(receipt.wait(Duration::ZERO)?, None);
    }
    if let Some((index, bad)) = refused {
        assert!(f.host.reject_drawing(&large, bad)?);
        assert!(!f.host.presented(&large)?);
        assert_eq!(
            pending[index].wait(WAIT)?,
            Some(PresentationOutcome::Superseded)
        );
        assert!(f.windows[index].reconcile().is_err());
    }
    let rejected = refused.map(|(index, _)| index);
    for (other, window) in f.windows.iter().enumerate() {
        if Some(other) != rejected {
            let status = window.reconcile()?;
            assert_eq!(
                (status.accepted_revision, status.presented_revision),
                (2, 1)
            );
            assert_eq!(pending[other].wait(Duration::ZERO)?, None);
        }
    }
    drop(large);
    let mut fresh = Vec::with_capacity(heavy);
    let mut revisions = vec![Some(3); heavy + 1];
    if let Some(index) = rejected {
        revisions[index] = None;
    }
    revisions[heavy] = Some(2);
    for other in 0..heavy {
        if Some(other) != rejected {
            fresh.push(f.windows[other].submit(scene(1, 0xffff00ff, 1024.))?);
        }
    }
    let frame = f.frame(&revisions, 1.)?;
    let image = poll(&mut gpu, frame.drawings(), 1024)?;
    pixels(
        &image,
        &[],
        Some([255, 0, 255, 255]),
        (heavy > usize::from(rejected.is_some())).then_some([255, 255, 0, 255]),
    );
    assert!(f.host.presented(&frame)?);
    assert_eq!(
        pending[heavy].wait(WAIT)?,
        Some(PresentationOutcome::Presented)
    );
    for receipt in &pending[..heavy] {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Superseded));
    }
    for receipt in fresh {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
    }
    assert!(!gpu.is_failed() && !private.is_failed());
    eprintln!(
        "Adversarial isolation: heavy_owners={heavy}, refused_owner={:?}, original_peer_presented=2, same_id_survivors={}, device_failed=false",
        rejected.map(|index| index + 1),
        heavy + 1 - usize::from(rejected.is_some())
    );
    drop(frame);
    for session in &f.sessions {
        session.close()?;
    }
    Ok(())
}

#[test]
#[ignore = "requires native GPU; concentrated cubic overdraw near the helper record bound"]
fn cubic_pressure_preserves_original_peer_receipts() -> io::Result<()> {
    exercise(&[Shape::Cubics])
}

#[test]
#[ignore = "requires native GPU; maximum commands and clip depth"]
fn maximum_clip_and_command_pressure_preserves_original_peer_receipts() -> io::Result<()> {
    exercise(&[Shape::Clips])
}

#[test]
#[ignore = "requires native GPU; maximum segments in one concentrated winding path"]
fn maximum_path_segment_pressure_preserves_original_peer_receipts() -> io::Result<()> {
    exercise(&[Shape::Segments])
}

#[test]
#[ignore = "requires native GPU; four-owner mixed curve/clip/path arena pressure"]
fn four_owner_pressure_requires_isolated_proof_before_cleanup() -> io::Result<()> {
    exercise(&[Shape::Cubics, Shape::Clips, Shape::Segments])
}

#[test]
#[ignore = "requires native GPU; valid winding counts exceed packed MSAA subword limits"]
fn nonzero_winding_beyond_eight_bits_preserves_circle_coverage() -> io::Result<()> {
    let mut f = Fixture::new(1)?;
    let mut gpu = pollster::block_on(Offscreen::new())?;
    let mut reference: Option<Arc<[u8]>> = None;
    for (index, loops) in [1, 127, 128, 255, 256, 682].into_iter().enumerate() {
        let mut canvas = Canvas::new();
        canvas
            .fill(winding(loops), Brush::Solid(Color(0x00ff00ff)))
            .unwrap();
        let bytes = canvas.encode().unwrap().len();
        assert!(bytes + 12 <= 256 * 1024);
        let a = f.windows[0].submit(canvas)?;
        let b = f.windows[1].submit(scene(1, 0xff00ffff, 64.))?;
        let revision = index as u64 + 1;
        let frame = f.frame(&[Some(revision), Some(revision)], 1.)?;
        let image = poll(&mut gpu, frame.drawings(), 1024)?;
        pixels(&image, &[Shape::Segments], Some([255, 0, 255, 255]), None);
        if let Some(reference) = &reference {
            equivalent(&image.rgba, reference);
        } else {
            reference = Some(image.rgba.clone());
        }
        assert_eq!(a.wait(Duration::ZERO)?, None);
        assert_eq!(b.wait(Duration::ZERO)?, None);
        assert!(f.host.presented(&frame)?);
        for receipt in [&a, &b] {
            assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
        }
        eprintln!(
            "Nonzero winding coverage: loops={loops}, scene_bytes={bytes}, revision={revision}, original_receipts=presented"
        );
    }
    for session in &f.sessions {
        session.close()?;
    }
    Ok(())
}

fn equivalent(actual: &[u8], reference: &[u8]) {
    assert_eq!(actual.len(), reference.len());
    for (index, (a, b)) in actual
        .as_chunks::<4>()
        .0
        .iter()
        .zip(reference.as_chunks::<4>().0)
        .enumerate()
    {
        assert_eq!(a[3], b[3], "equivalent contour alpha at pixel {index}");
        if b[3] != 0 {
            assert_eq!(a, b, "equivalent contour RGBA at pixel {index}");
        }
    }
}

#[test]
#[ignore = "requires native GPU; signed wide-winding cancellation and unchanged even-odd parity"]
fn wide_winding_preserves_signed_cancellation_and_evenodd_parity() -> io::Result<()> {
    let positive = Path::builder()
        .move_to(256.25, 256.25)
        .line_to(767.75, 256.25)
        .line_to(767.75, 767.75)
        .line_to(256.25, 767.75)
        .close()
        .build()
        .unwrap();
    let negative = Path::builder()
        .move_to(256.25, 256.25)
        .line_to(256.25, 767.75)
        .line_to(767.75, 767.75)
        .line_to(767.75, 256.25)
        .close()
        .build()
        .unwrap();
    let cases = [
        (1usize, 0usize, false),
        (128, 0, false),
        (0, 128, false),
        (256, 0, false),
        (128, 128, false),
        (409, 409, false),
        (128, 127, false),
        (127, 128, false),
        (255, 0, true),
        (256, 0, true),
    ];
    let mut f = Fixture::new(1)?;
    let mut gpu = pollster::block_on(Offscreen::new())?;
    let mut reference: Option<Arc<[u8]>> = None;
    for (index, (forward, backward, even_odd)) in cases.into_iter().enumerate() {
        let mut path = Path {
            even_odd,
            segments: Vec::with_capacity((forward + backward) * 5),
        };
        for _ in 0..forward {
            path.segments.extend(positive.segments.iter().cloned());
        }
        for _ in 0..backward {
            path.segments.extend(negative.segments.iter().cloned());
        }
        let mut canvas = Canvas::new();
        canvas.fill(path, Brush::Solid(Color(0x0000ffff))).unwrap();
        canvas.validate().unwrap();
        assert!(canvas.encode().unwrap().len() + 12 <= 256 * 1024);
        let a = f.windows[0].submit(canvas)?;
        let b = f.windows[1].submit(scene(1, 0xff00ffff, 64.))?;
        let revision = index as u64 + 1;
        let frame = f.frame(&[Some(revision), Some(revision)], 1.)?;
        let image = poll(&mut gpu, frame.drawings(), 1024)?;
        let empty = if even_odd {
            (forward + backward) % 2 == 0
        } else {
            forward == backward
        };
        if empty {
            pixels(&image, &[], Some([255, 0, 255, 255]), None);
        } else if let Some(reference) = &reference {
            equivalent(&image.rgba, reference);
        } else {
            let at = (512 * 1024 + 512) * 4;
            assert_eq!(&image.rgba[at..at + 4], &[0, 0, 255, 255]);
            assert_eq!(image.rgba[(128 * 1024 + 128) * 4 + 3], 0);
            reference = Some(image.rgba.clone());
        }
        assert_eq!(a.wait(Duration::ZERO)?, None);
        assert_eq!(b.wait(Duration::ZERO)?, None);
        assert!(f.host.presented(&frame)?);
        for receipt in [&a, &b] {
            assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
        }
        eprintln!(
            "Wide signed/parity coverage: positive={forward}, negative={backward}, even_odd={even_odd}, empty={empty}, revision={revision}"
        );
    }
    for session in &f.sessions {
        session.close()?;
    }
    Ok(())
}
