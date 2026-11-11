//! Bounded asynchronous static-image decoding without presenter or GUI locks.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::vector_renderer::RasterImage;
use ::image::{DynamicImage, ImageDecoder as _, ImageFormat, ImageReader, Limits};
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

#[path = "images/color.rs"]
mod color;
#[path = "images/container.rs"]
mod container;

// One active decode and one queued/completed job; completion storage shares this bound.
const CAPACITY: usize = 2;
// Match the renderer's image/encoding ceiling. Intermediate frame conversion is
// separately preflighted, including Vec-to-Arc ownership conversion.
const MAX_BYTES: u64 = crate::vector_renderer::MAX_COMPILED_BYTES as u64;
const MAX_WORK_BYTES: u64 = MAX_BYTES * 2;
const MAX_AXIS: u32 = 8192;

pub(super) struct Job {
    pub source: TrackKey,
    pub id: u64,
    pub body: Arc<[u8]>,
    pub config: ImageConfiguration,
    pub token: Arc<()>,
}
pub(super) struct Done {
    pub source: TrackKey,
    pub id: u64,
    pub image: Result<Arc<RasterImage>, ()>,
}
pub(super) struct Decoder {
    jobs: Option<mpsc::SyncSender<Job>>,
    done: mpsc::Receiver<Done>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    in_flight: usize,
    next: u64,
    pub last: Option<(u64, u64, u64, u64)>,
}
impl Decoder {
    pub fn new() -> io::Result<Self> {
        Self::spawn(decode)
    }
    fn spawn(
        decode: impl Fn(&Job) -> Result<Arc<RasterImage>, ()> + Send + 'static,
    ) -> io::Result<Self> {
        let (jobs, input) = mpsc::sync_channel::<Job>(CAPACITY);
        let (output, done) = mpsc::sync_channel(CAPACITY);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::Builder::new()
            .name("vivid-image-decode".into())
            .spawn(move || {
                while let Ok(job) = input.recv() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let image = decode(&job);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // Admission counts completed results too, so this cannot fill
                    // while its receiver remains live. Never block host teardown.
                    if output
                        .try_send(Done {
                            source: job.source,
                            id: job.id,
                            image,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs: Some(jobs),
            done,
            stopped,
            worker: Some(worker),
            in_flight: 0,
            next: 0,
            last: None,
        })
    }
    pub fn available(&self) -> bool {
        self.in_flight < CAPACITY
    }
    pub fn submit(
        &mut self,
        source: TrackKey,
        body: Arc<[u8]>,
        config: ImageConfiguration,
        token: Arc<()>,
    ) -> io::Result<Option<u64>> {
        if !self.available() {
            return Ok(None);
        }
        let id = self
            .next
            .checked_add(1)
            .ok_or_else(|| invalid("image decode IDs exhausted"))?;
        self.jobs
            .as_ref()
            .expect("live decoder sender")
            .try_send(Job {
                source,
                id,
                body,
                config,
                token,
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "native image decode worker stopped",
                )
            })?;
        self.next = id;
        self.in_flight += 1;
        self.last = Some(order(source));
        Ok(Some(id))
    }
    pub fn poll(&mut self) -> io::Result<Option<Done>> {
        match self.done.try_recv() {
            Ok(done) => {
                self.in_flight -= 1;
                Ok(Some(done))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "native image decode worker stopped",
            )),
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub(super) fn order(source: TrackKey) -> (u64, u64, u64, u64) {
    (
        source.surface.session,
        source.surface.context,
        source.surface.surface,
        source.track,
    )
}
pub(super) fn output_bytes(config: &ImageConfiguration) -> Option<usize> {
    let bytes = u64::from(config.width)
        .checked_mul(u64::from(config.height))?
        .checked_mul(4)?;
    (config.width != 0
        && config.height != 0
        && config.width <= MAX_AXIS
        && config.height <= MAX_AXIS
        && bytes <= MAX_BYTES
        && u64::from(config.encoded_length) <= MAX_BYTES)
        .then(|| usize::try_from(bytes).ok())
        .flatten()
}
fn decode(job: &Job) -> Result<Arc<RasterImage>, ()> {
    let expected = output_bytes(&job.config).ok_or(())? as u64;
    if job.body.len() != job.config.encoded_length as usize {
        return Err(());
    }
    let (format, metadata) = match job.config.encoding {
        crate::IMAGE_ENCODING_PNG => (ImageFormat::Png, container::png(&job.body)?),
        crate::IMAGE_ENCODING_JPEG => {
            container::jpeg(&job.body)?;
            (ImageFormat::Jpeg, container::PngColor::default())
        }
        _ => return Err(()),
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_AXIS);
    limits.max_image_height = Some(MAX_AXIS);
    limits.max_alloc = Some(MAX_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(job.body.clone()), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| ())?;
    if decoder.dimensions() != (job.config.width, job.config.height)
        || decoder
            .total_bytes()
            .checked_add(expected)
            .is_none_or(|n| n > MAX_WORK_BYTES)
        || expected.checked_mul(2).is_none_or(|n| n > MAX_WORK_BYTES)
    {
        return Err(());
    }
    let grayscale = !decoder.color_type().has_color();
    let icc = decoder.icc_profile().map_err(|_| ())?;
    let profile = color::profile(&metadata, icc.as_deref())?;
    drop(icc);
    let mut rgba = DynamicImage::from_decoder(decoder)
        .map_err(|_| ())?
        .into_rgba8()
        .into_raw();
    if rgba.len() as u64 != expected {
        return Err(());
    }
    if let Some(profile) = profile {
        color::to_srgb(&mut rgba, job.config.width, grayscale, profile)?;
    }
    let mut image =
        RasterImage::new(job.config.width, job.config.height, rgba.into()).map_err(|_| ())?;
    image.retain_resource(job.token.clone()).map_err(|_| ())?;
    Ok(Arc::new(image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::ImageEncoder as _;
    use std::time::{Duration, Instant};

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = (data.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&crc32fast::hash(&bytes[4..]).to_be_bytes());
        bytes
    }
    fn png(icc: Option<Vec<u8>>) -> Vec<u8> {
        let mut body = Vec::new();
        let mut encoder = ::image::codecs::png::PngEncoder::new(&mut body);
        if let Some(icc) = icc {
            encoder.set_icc_profile(icc).unwrap();
        }
        encoder
            .write_image(
                &[128, 128, 128, 73],
                1,
                1,
                ::image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        body
    }
    fn jpeg(icc: Option<Vec<u8>>) -> Vec<u8> {
        let mut body = Vec::new();
        let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut body, 100);
        if let Some(icc) = icc {
            encoder.set_icc_profile(icc).unwrap();
        }
        encoder
            .write_image(&[128, 128, 128], 1, 1, ::image::ExtendedColorType::Rgb8)
            .unwrap();
        body
    }
    fn decode_bytes(body: Vec<u8>, encoding: u64) -> Result<Arc<RasterImage>, ()> {
        decode(&Job {
            source: TrackKey {
                surface: SurfaceKey {
                    session: 1,
                    context: 1,
                    surface: 1,
                },
                track: 1,
            },
            id: 1,
            config: ImageConfiguration {
                encoding,
                width: 1,
                height: 1,
                encoded_length: body.len() as u32,
                sha256: None,
                cache_lookup: false,
            },
            body: body.into(),
            token: Arc::new(()),
        })
    }
    fn pixel(body: Vec<u8>, encoding: u64) -> [u8; 4] {
        decode_bytes(body, encoding)
            .unwrap()
            .data
            .data
            .data()
            .try_into()
            .unwrap()
    }
    #[test]
    fn static_container_checks_reject_animation_mpo_trailing_pictures_and_bad_lengths() {
        let base = png(None);
        for kind in [b"acTL", b"fcTL", b"fdAT"] {
            let mut body = base.clone();
            body.splice(33..33, png_chunk(kind, &[0; 8]));
            assert!(decode_bytes(body, crate::IMAGE_ENCODING_PNG).is_err());
        }
        let mut concat = base.clone();
        concat.extend_from_slice(&base);
        assert!(decode_bytes(concat, crate::IMAGE_ENCODING_PNG).is_err());
        let mut bad_crc = base.clone();
        bad_crc[29] ^= 1;
        assert!(decode_bytes(bad_crc, crate::IMAGE_ENCODING_PNG).is_err());
        let mut overflow = base;
        overflow[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_bytes(overflow, crate::IMAGE_ENCODING_PNG).is_err());
        let base = jpeg(None);
        let mut mpo = base.clone();
        mpo.splice(2..2, [0xff, 0xe2, 0, 6, b'M', b'P', b'F', 0]);
        assert!(decode_bytes(mpo, crate::IMAGE_ENCODING_JPEG).is_err());
        let sof = base
            .windows(2)
            .position(|bytes| bytes == [0xff, 0xc0])
            .unwrap();
        let length = u16::from_be_bytes(base[sof + 2..sof + 4].try_into().unwrap()) as usize;
        let mut multiple_frames = base.clone();
        multiple_frames.splice(2..2, base[sof..sof + 2 + length].iter().copied());
        assert!(decode_bytes(multiple_frames, crate::IMAGE_ENCODING_JPEG).is_err());
        let mut concat = base.clone();
        concat.extend_from_slice(&base);
        assert!(decode_bytes(concat, crate::IMAGE_ENCODING_JPEG).is_err());
        assert_eq!(
            pixel(base, crate::IMAGE_ENCODING_JPEG),
            [128, 128, 128, 255]
        );
    }
    #[test]
    fn linear_png_metadata_becomes_srgb_without_changing_straight_alpha() {
        let base = png(None);
        assert_eq!(
            pixel(base.clone(), crate::IMAGE_ENCODING_PNG),
            [128, 128, 128, 73]
        );
        for chunk in [
            png_chunk(b"gAMA", &100_000u32.to_be_bytes()),
            png_chunk(b"cICP", &[1, 8, 0, 1]),
        ] {
            let mut body = base.clone();
            body.splice(33..33, chunk);
            let result = pixel(body, crate::IMAGE_ENCODING_PNG);
            assert!(
                result[..3].iter().all(|v| v.abs_diff(188) <= 1),
                "{result:?}"
            );
            assert_eq!(result[3], 73);
        }
        let mut srgb = base;
        let mut chunks = png_chunk(b"gAMA", &100_000u32.to_be_bytes());
        chunks.extend_from_slice(&png_chunk(b"sRGB", &[0]));
        srgb.splice(33..33, chunks);
        assert_eq!(pixel(srgb, crate::IMAGE_ENCODING_PNG), [128, 128, 128, 73]);
    }
    #[test]
    fn embedded_rgb_and_gray_icc_are_converted_and_bad_profiles_are_rejected() {
        let mut linear = moxcms::ColorProfile::new_srgb();
        linear.cicp = None;
        linear.red_trc = Some(moxcms::curve_from_gamma(1.));
        linear.green_trc = linear.red_trc.clone();
        linear.blue_trc = linear.red_trc.clone();
        let icc = linear.encode().unwrap();
        for (body, encoding) in [
            (png(Some(icc.clone())), crate::IMAGE_ENCODING_PNG),
            (jpeg(Some(icc)), crate::IMAGE_ENCODING_JPEG),
        ] {
            let result = pixel(body, encoding);
            assert!(
                result[..3].iter().all(|v| v.abs_diff(188) <= 2),
                "{result:?}"
            );
        }
        let mut bytes = Vec::new();
        let mut encoder = ::image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(
                moxcms::ColorProfile::new_gray_with_gamma(1.)
                    .encode()
                    .unwrap(),
            )
            .unwrap();
        encoder
            .write_image(&[128, 73], 1, 1, ::image::ExtendedColorType::La8)
            .unwrap();
        let result = pixel(bytes, crate::IMAGE_ENCODING_PNG);
        assert!(
            result[..3].iter().all(|v| v.abs_diff(188) <= 2),
            "{result:?}"
        );
        assert_eq!(result[3], 73);
        assert!(decode_bytes(png(Some(vec![0; 132])), crate::IMAGE_ENCODING_PNG).is_err());
    }
    #[test]
    fn chromaticities_are_checked_and_orientation_metadata_is_ignored() {
        let mut body = png(None);
        let values: [u32; 8] = [31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000];
        let mut chunks = png_chunk(
            b"cHRM",
            &values
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        chunks.extend_from_slice(&png_chunk(b"gAMA", &100_000u32.to_be_bytes()));
        body.splice(33..33, chunks);
        let result = pixel(body, crate::IMAGE_ENCODING_PNG);
        assert!(
            result[..3].iter().all(|v| v.abs_diff(188) <= 2),
            "{result:?}"
        );
        let mut bad = png(None);
        bad.splice(33..33, png_chunk(b"cHRM", &[0; 32]));
        assert!(decode_bytes(bad, crate::IMAGE_ENCODING_PNG).is_err());
        let mut body = Vec::new();
        let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut body, 100);
        // Little-endian TIFF IFD0: Orientation=6 (rotate 90 degrees clockwise).
        encoder
            .set_exif_metadata(vec![
                b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0,
                0, 0,
            ])
            .unwrap();
        encoder
            .write_image(&[128; 6], 1, 2, ::image::ExtendedColorType::Rgb8)
            .unwrap();
        let job = Job {
            source: TrackKey {
                surface: SurfaceKey {
                    session: 1,
                    context: 1,
                    surface: 1,
                },
                track: 1,
            },
            id: 1,
            config: ImageConfiguration {
                encoding: crate::IMAGE_ENCODING_JPEG,
                width: 1,
                height: 2,
                encoded_length: body.len() as u32,
                sha256: None,
                cache_lookup: false,
            },
            body: body.into(),
            token: Arc::new(()),
        };
        assert_eq!(decode(&job).unwrap().dimensions(), (1, 2));
        let excessive = container::PngColor {
            cicp: Some([255, 8, 0, 1]),
            ..Default::default()
        };
        assert!(color::profile(&excessive, None).is_err());
        let mut profile = moxcms::ColorProfile::new_srgb().encode().unwrap();
        profile[128..132].copy_from_slice(&129u32.to_be_bytes());
        assert!(color::profile(&Default::default(), Some(&profile)).is_err());
    }

    struct Release(mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
            let _ = self.0.send(());
        }
    }
    #[test]
    fn blocked_decode_does_not_block_host_polling_or_exceed_two_retained_jobs() {
        let (entered, ready) = mpsc::sync_channel(2);
        let (release, gate) = mpsc::channel();
        let mut decoder = Decoder::spawn(move |job| {
            entered.send(()).unwrap();
            gate.recv().map_err(|_| ())?;
            let mut image = RasterImage::new(1, 1, Arc::from([0, 0, 0, 255])).map_err(|_| ())?;
            image.retain_resource(job.token.clone()).map_err(|_| ())?;
            Ok(Arc::new(image))
        })
        .unwrap();
        let release = Release(release);
        let source = TrackKey {
            surface: SurfaceKey {
                session: 1,
                context: 1,
                surface: 1,
            },
            track: 1,
        };
        let config = ImageConfiguration {
            encoding: crate::IMAGE_ENCODING_PNG,
            width: 1,
            height: 1,
            encoded_length: 1,
            sha256: None,
            cache_lookup: false,
        };
        let token = Arc::new(());
        let weak = Arc::downgrade(&token);
        for _ in 0..2 {
            assert!(
                decoder
                    .submit(source, Arc::from([0]), config.clone(), token.clone())
                    .unwrap()
                    .is_some()
            );
        }
        drop(token);
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(decoder.poll().unwrap().is_none());
        assert!(
            decoder
                .submit(source, Arc::from([0]), config.clone(), Arc::new(()))
                .unwrap()
                .is_none()
        );
        release.0.send(()).unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            !decoder.available(),
            "completed results share admission with active jobs"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Some(done) = decoder.poll().unwrap() {
                break done;
            }
            assert!(
                Instant::now() < deadline,
                "decode completion did not arrive"
            );
            thread::yield_now();
        };
        assert!(decoder.available());
        drop(result);
        drop(release);
        drop(decoder);
        assert!(
            weak.upgrade().is_none(),
            "teardown must release queued/completed image charges"
        );
    }
    #[test]
    fn declared_decode_sizes_are_checked_without_allocating_pixels() {
        let mut config = ImageConfiguration {
            encoding: crate::IMAGE_ENCODING_PNG,
            width: 8192,
            height: 8192,
            encoded_length: 24,
            sha256: None,
            cache_lookup: false,
        };
        assert!(output_bytes(&config).is_none());
        config.width = u32::MAX;
        assert!(output_bytes(&config).is_none());
        config.width = 2;
        config.height = 2;
        assert_eq!(output_bytes(&config), Some(16));
    }
}
