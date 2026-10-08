//! Orchestration types a desktop producer layers over the raw `Session` API.
use std::io;

use vivid_protocol::messages::PayloadMap;
use vivid_protocol::resource::{Resource, ResourceContract};
use vivid_protocol::revision::{ChannelGeneration, SurfaceGeneration, SurfaceRevision};
use vivid_protocol::surface::{
    CoordinateModel, DesktopSurfaceParameters, SurfaceDefinition, SurfaceDescriptor, SurfaceRole,
};
use vivid_protocol::track::{
    ImageConfiguration, KindConfiguration, TrackConfiguration, TrackDirection, TrackMode,
};

use crate::{LaneClass, RequestMetadata, Session, SlotBinding, Surface, Track};

#[derive(Debug, Clone)]
pub struct DesktopSurface {
    pub(crate) raw: Surface,
}
impl DesktopSurface {
    pub fn inner(&self) -> &Surface {
        &self.raw
    }
    pub fn id(&self) -> u64 {
        self.raw.id()
    }
    pub fn context_id(&self) -> u64 {
        self.raw.context_id()
    }
    pub fn revision(&self) -> SurfaceRevision {
        self.raw.revision()
    }
    pub fn generation(&self) -> SurfaceGeneration {
        self.raw.generation()
    }
    pub fn snapshot(&self) -> io::Result<SurfaceDefinition> {
        self.raw.definition()
    }
    pub fn desktop_params(&self) -> io::Result<DesktopSurfaceParameters> {
        let def = self.raw.definition()?;
        DesktopSurfaceParameters::decode(&def.profile_parameters).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("desktop surface params: {e}"),
            )
        })
    }
    pub fn topology(&self) -> io::Result<Vec<crate::OutputDescriptor>> {
        Ok(self.desktop_params()?.topology)
    }
    pub fn semantic_generation(&self) -> io::Result<u64> {
        Ok(self.desktop_params()?.semantic_generation)
    }
    pub fn input_capabilities(&self) -> io::Result<u64> {
        Ok(self.desktop_params()?.input_capabilities)
    }

    pub fn update(
        &self,
        session: &mut Session,
        replacement: SurfaceDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<DeskMutation> {
        let old = self.raw.definition()?;
        session.update_surface(&self.raw, replacement.clone(), metadata)?;
        Ok(DeskMutation {
            generation_changed: old.logical_width != replacement.logical_width
                || old.logical_height != replacement.logical_height
                || old.scale_numerator != replacement.scale_numerator
                || old.scale_denominator != replacement.scale_denominator
                || old.rotation != replacement.rotation
                || old.profile_parameters != replacement.profile_parameters,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeskMutation {
    pub generation_changed: bool,
}
impl DeskMutation {
    pub const fn input_must_be_revoked(&self) -> bool {
        self.generation_changed
    }
}

/// Builds a [`SurfaceDefinition`], defaulting the identity from a session.
///
/// A surface needs a context and an identifier before anything else can be said about it, and the
/// answer is almost always "the session's root context" and "the next allocated id". Leaving that
/// to each binding is how the Python package ended up owning the defaults; they belong here, where
/// every language gets the same ones.
#[derive(Debug)]
pub struct SurfaceBuilder {
    definition: SurfaceDefinition,
}

impl SurfaceBuilder {
    /// Start from the session's root context and a freshly allocated surface id.
    ///
    /// The surface is a generic-content surface in desktop logical pixels until told otherwise,
    /// which is what a producer showing an image or a figure wants.
    pub fn new(session: &Session, logical_width: u64, logical_height: u64) -> io::Result<Self> {
        Ok(Self {
            definition: SurfaceDefinition {
                context_id: session.info().root_context_id,
                surface_id: session.allocate_id()?,
                semantic_profile: crate::GENERIC_CONTENT.into(),
                coordinate_model: CoordinateModel::DesktopLogicalPixels,
                logical_width,
                logical_height,
                scale_numerator: 1,
                scale_denominator: 1,
                rotation: 0,
                descriptor: SurfaceDescriptor {
                    role: SurfaceRole::Unspecified,
                    title: String::new(),
                    semantic_content_revision: 0,
                    semantic_availability: 0,
                    locator_hint: String::new(),
                },
                policy: 0,
                profile_parameters: Vec::new(),
            },
        })
    }

    /// Place the surface in a context other than the session root, such as a delegated worker
    /// context.
    pub fn context(mut self, context_id: u64) -> Self {
        self.definition.context_id = context_id;
        self
    }

    /// Use a specific surface id instead of the allocated one, which is what adopting a surface
    /// across a resume requires.
    pub fn surface_id(mut self, surface_id: u64) -> Self {
        self.definition.surface_id = surface_id;
        self
    }

    /// Set the semantic profile and the coordinate model its geometry is expressed in.
    pub fn semantic(mut self, profile: &str, coordinate_model: CoordinateModel) -> Self {
        self.definition.semantic_profile = profile.into();
        self.definition.coordinate_model = coordinate_model;
        self
    }

    /// Set the descriptor a presenter shows and exports, subject to the surface policy.
    pub fn descriptor(mut self, descriptor: SurfaceDescriptor) -> Self {
        self.definition.descriptor = descriptor;
        self
    }

    /// Set the role and title without replacing the rest of the descriptor.
    pub fn titled(mut self, role: SurfaceRole, title: impl Into<String>) -> Self {
        self.definition.descriptor.role = role;
        self.definition.descriptor.title = title.into();
        self
    }

    /// Apply capture, export, retention, and diagnostic policy bits.
    pub fn policy(mut self, policy: u64) -> Self {
        self.definition.policy = policy;
        self
    }

    /// Set the pixel scale and rotation the logical geometry is presented at.
    pub fn scale(mut self, numerator: u64, denominator: u64, rotation: u16) -> Self {
        self.definition.scale_numerator = numerator;
        self.definition.scale_denominator = denominator;
        self.definition.rotation = rotation;
        self
    }

    /// Attach typed desktop parameters, which is what makes a surface a desktop surface.
    ///
    /// Without this a desktop-profile surface carries no captured origin, topology, or input
    /// capabilities, and a presenter has nothing to map input back onto.
    pub fn desktop(mut self, parameters: &DesktopSurfaceParameters) -> Self {
        self.definition.profile_parameters = parameters.encode();
        self
    }

    /// Attach already-encoded profile parameters, for a profile this crate does not model.
    ///
    /// Unknown canonical entries must survive a relay byte for byte, so a caller carrying
    /// parameters forward passes them through here rather than rebuilding them.
    pub fn profile_parameters(mut self, parameters: PayloadMap) -> Self {
        self.definition.profile_parameters = parameters;
        self
    }

    /// The definition, validated against the protocol before it reaches the wire.
    pub fn build(self) -> io::Result<SurfaceDefinition> {
        self.definition
            .validate()
            .map_err(|error| err(error.to_string()))?;
        Ok(self.definition)
    }
}

#[derive(Debug)]
pub struct TrackBuilder {
    context_id: u64,
    surface_id: u64,
    slot: u64,
    mode: TrackMode,
    lane: LaneClass,
    direction: TrackDirection,
    kind: Option<KindConfiguration>,
    max_rate_millihertz: u64,
    max_encoded_bits_per_second: u64,
    max_records_per_second: u64,
    maximum_record_body: u32,
    maximum_inflight_body_bytes: u64,
    decoded_pixels_per_second: u64,
    target_latency_us: u64,
    maximum_latency_us: u64,
    retained_pixel_charge: u64,
}
impl TrackBuilder {
    pub fn new(surface: &Surface, slot: u64, mode: TrackMode, lane: LaneClass) -> Self {
        Self::detached(surface.context_id(), surface.id(), slot, mode, lane)
    }

    /// A builder for a surface that is not yet created.
    ///
    /// A caller that is still assembling configuration — a binding building both a surface and
    /// its tracks before either exists — has the identity in hand and nothing to point at yet.
    /// The identity is what the configuration carries, so naming it directly is enough.
    pub fn detached(
        context_id: u64,
        surface_id: u64,
        slot: u64,
        mode: TrackMode,
        lane: LaneClass,
    ) -> Self {
        Self {
            context_id,
            surface_id,
            slot,
            mode,
            lane,
            direction: TrackDirection::Downlink,
            kind: None,
            max_rate_millihertz: 60_000,
            max_encoded_bits_per_second: 0,
            max_records_per_second: 60,
            maximum_record_body: 0,
            maximum_inflight_body_bytes: 0,
            decoded_pixels_per_second: 0,
            target_latency_us: 33_000,
            maximum_latency_us: 100_000,
            retained_pixel_charge: 0,
        }
    }

    pub fn video(mut self, cw: u32, ch: u32, codec: &str) -> Self {
        self.kind = Some(KindConfiguration::Video(
            vivid_protocol::track::VideoConfiguration {
                codec: codec.into(),
                packetization: format!("{codec}-annexb-au-v1"),
                extradata: vec![],
                coded_width: cw,
                coded_height: ch,
                profile: 0,
                level: 0,
                maximum_reorder_depth: 0,
                color_primaries: 1,
                transfer: 1,
                matrix: 1,
                signal_range: 1,
                aspect_numerator: 1,
                aspect_denominator: 1,
                maximum_access_unit_bytes: 8 * 1024 * 1024,
                codec_string: None,
                decoder_configuration: None,
            },
        ));
        self.maximum_record_body = self.maximum_record_body.max(10 * 1024 * 1024);
        self.maximum_inflight_body_bytes = self.maximum_inflight_body_bytes.max(20 * 1024 * 1024);
        self.max_encoded_bits_per_second = self.max_encoded_bits_per_second.max(50_000_000);
        self.decoded_pixels_per_second = self
            .decoded_pixels_per_second
            .max(u64::from(cw) * u64::from(ch) * 2);
        self.retained_pixel_charge = self
            .retained_pixel_charge
            .max(u64::from(cw) * u64::from(ch));
        self
    }

    pub fn audio(mut self, sample_rate: u32, channels: u8) -> Self {
        self.kind = Some(KindConfiguration::Audio(
            vivid_protocol::track::AudioConfiguration {
                codec: "opus".into(),
                packetization: "opus-packet-v1".into(),
                extradata: vec![],
                sample_rate,
                channels,
                channel_mask: 0,
                maximum_access_unit_bytes: 1024,
                codec_string: None,
            },
        ));
        self.max_encoded_bits_per_second = self
            .max_encoded_bits_per_second
            .max(u64::from(sample_rate) * u64::from(channels) * 10);
        self.max_records_per_second = self.max_records_per_second.max(100);
        self.maximum_record_body = self.maximum_record_body.max(16 * 1024);
        self.maximum_inflight_body_bytes = self.maximum_inflight_body_bytes.max(64 * 1024);
        self
    }

    /// A retained raster track that carries whole RGBA frames, and optionally deltas against a
    /// previously accepted frame.
    ///
    /// A full frame is a fixed size, so the claims follow from the geometry: one record must hold
    /// the packet header and `width * height * 4` bytes of pixels, and the surface retains one
    /// frame's worth of pixels. Both are computed with checked arithmetic — a geometry that
    /// overflows a claim is rejected here rather than silently wrapping into a small one.
    pub fn raster(mut self, width: u32, height: u32) -> io::Result<Self> {
        /// Raster packet header, ahead of the pixels.
        const RASTER_PACKET_OVERHEAD: u64 = 72;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| err("raster geometry overflows a resource claim"))?;
        let body = pixels
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(RASTER_PACKET_OVERHEAD))
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or_else(|| err("raster geometry overflows a single media record"))?;

        self.kind = Some(KindConfiguration::Raster(
            vivid_protocol::track::RasterConfiguration {
                width,
                height,
                alpha_mode: 1,
                delta_enabled: false,
                maximum_delta_operations: 1,
                zstd_enabled: false,
            },
        ));
        self.maximum_record_body = self.maximum_record_body.max(body);
        self.maximum_inflight_body_bytes = self
            .maximum_inflight_body_bytes
            .max(u64::from(body).saturating_mul(2));
        self.max_encoded_bits_per_second = self.max_encoded_bits_per_second.max(
            u64::from(body)
                .saturating_mul(8)
                .saturating_mul(self.max_records_per_second),
        );
        self.decoded_pixels_per_second = self
            .decoded_pixels_per_second
            .max(pixels.saturating_mul(self.max_records_per_second));
        self.retained_pixel_charge = self.retained_pixel_charge.max(pixels);
        self.target_latency_us = self.target_latency_us.min(16_000);
        Ok(self)
    }

    /// Enable raster delta frames against a previously accepted base frame.
    ///
    /// Only meaningful after [`TrackBuilder::raster`]; on any other kind this is ignored, because
    /// the kind is what decides whether deltas exist at all.
    pub fn raster_deltas(mut self, maximum_operations: u8, zstd_enabled: bool) -> Self {
        if let Some(KindConfiguration::Raster(raster)) = &mut self.kind {
            raster.delta_enabled = maximum_operations > 0;
            raster.maximum_delta_operations = maximum_operations.max(1);
            raster.zstd_enabled = zstd_enabled;
        }
        self
    }

    /// A one-shot encoded-image track carrying exactly one PNG or JPEG.
    ///
    /// The configuration comes from [`probe_encoded_image`](crate::probe_encoded_image), so the
    /// declared length is the container's real length and the image is sent once, whole.
    pub fn image(mut self, image: ImageConfiguration) -> io::Result<Self> {
        let pixels = u64::from(image.width)
            .checked_mul(u64::from(image.height))
            .ok_or_else(|| err("image geometry overflows a resource claim"))?;
        let encoded_length = image.encoded_length;

        self.maximum_record_body = self.maximum_record_body.max(encoded_length);
        self.maximum_inflight_body_bytes = self
            .maximum_inflight_body_bytes
            .max(u64::from(encoded_length));
        self.max_encoded_bits_per_second = self
            .max_encoded_bits_per_second
            .max(u64::from(encoded_length).saturating_mul(8));
        self.retained_pixel_charge = self.retained_pixel_charge.max(pixels);
        // One record, once. A rate claim above one buys nothing and only raises the admission
        // cost of a track that can never send a second frame.
        self.max_rate_millihertz = 1;
        self.max_records_per_second = 1;
        self.target_latency_us = 0;
        self.maximum_latency_us = 0;
        self.kind = Some(KindConfiguration::EncodedImage(image));
        Ok(self)
    }

    /// Declare the track as uplink — microphone audio toward the producer side of the surface.
    ///
    /// The protocol restricts uplink to live realtime audio in slot zero, and
    /// [`TrackBuilder::build`] lets the protocol's own validation enforce that, so this only sets
    /// the direction and leaves the rest to the kind the caller attaches.
    pub fn uplink(mut self) -> Self {
        self.direction = TrackDirection::Uplink;
        self
    }

    pub fn max_rate_millihertz(mut self, v: u64) -> Self {
        self.max_rate_millihertz = v;
        self
    }
    pub fn max_encoded_bps(mut self, v: u64) -> Self {
        self.max_encoded_bits_per_second = v;
        self
    }

    pub fn build(
        mut self,
        contract: &ResourceContract,
        track_id: u64,
    ) -> io::Result<TrackConfiguration> {
        let mut kind = self
            .kind
            .take()
            .ok_or_else(|| err("a track must have a media kind"))?;
        let record_ceiling = u32::try_from(
            contract
                .get(Resource::MediaRecordBody)
                .min(u64::from(u32::MAX)),
        )
        .map_err(|_| err("MediaRecordBody ceiling does not fit u32"))?;
        if record_ceiling == 0 {
            return Err(err("MediaRecordBody contract denies track media"));
        }
        self.maximum_record_body = self.maximum_record_body.max(1).min(record_ceiling);
        if let KindConfiguration::Video(video) = &mut kind {
            const VIDEO_PACKET_OVERHEAD: u32 = 48;
            video.maximum_access_unit_bytes = video.maximum_access_unit_bytes.min(
                self.maximum_record_body
                    .saturating_sub(VIDEO_PACKET_OVERHEAD),
            );
            if video.maximum_access_unit_bytes == 0 {
                return Err(err("MediaRecordBody ceiling cannot carry a video packet"));
            }
        }
        let inflight_ceiling = contract.get(Resource::InflightMediaBytes);
        if inflight_ceiling < u64::from(self.maximum_record_body) {
            return Err(err(
                "InflightMediaBytes contract cannot carry one maximum media record",
            ));
        }
        self.maximum_inflight_body_bytes = self
            .maximum_inflight_body_bytes
            .max(u64::from(self.maximum_record_body))
            .min(inflight_ceiling);
        checks(&self, contract)?;
        Ok(TrackConfiguration {
            direction: self.direction,
            context_id: self.context_id,
            surface_id: self.surface_id,
            track_id,
            slot: self.slot,
            mode: self.mode,
            lane: self.lane,
            maximum_record_body: self.maximum_record_body.max(1),
            maximum_rate_millihertz: self.max_rate_millihertz.max(1),
            maximum_encoded_bits_per_second: self.max_encoded_bits_per_second,
            maximum_records_per_second: self.max_records_per_second.max(1),
            maximum_inflight_body_bytes: self.maximum_inflight_body_bytes,
            kind,
            target_latency_us: self.target_latency_us,
            maximum_latency_us: self.maximum_latency_us,
            retained_pixel_charge: self.retained_pixel_charge,
        })
    }
}

fn checks(b: &TrackBuilder, c: &ResourceContract) -> io::Result<()> {
    check(
        c,
        Resource::EncodedBitsPerSecond,
        b.max_encoded_bits_per_second,
        "EncodedBitsPerSecond",
    )?;
    check(
        c,
        Resource::MediaRecordsPerSecond,
        b.max_records_per_second,
        "MediaRecordsPerSecond",
    )?;
    check(
        c,
        Resource::MediaRecordBody,
        u64::from(b.maximum_record_body),
        "MediaRecordBody",
    )?;
    check(
        c,
        Resource::DecodedPixelsPerSecond,
        b.decoded_pixels_per_second,
        "DecodedPixelsPerSecond",
    )?;
    check(
        c,
        Resource::InflightMediaBytes,
        b.maximum_inflight_body_bytes,
        "InflightMediaBytes",
    )?;
    Ok(())
}
fn check(c: &ResourceContract, r: Resource, claim: u64, name: &str) -> io::Result<()> {
    if claim == 0 {
        return Ok(());
    }
    if claim > c.get(r) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} claim {claim} exceeds contract ceiling {}", c.get(r)),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct SurfaceSlots {
    surface: Surface,
    bindings: Vec<SlotBinding>,
}
impl SurfaceSlots {
    pub fn new(s: &Surface) -> Self {
        Self {
            surface: s.clone(),
            bindings: vec![],
        }
    }
    pub fn require(
        &mut self,
        slot: u64,
        track: &Track,
        generation: ChannelGeneration,
        milestone: u64,
    ) -> io::Result<&mut Self> {
        if generation != track.channel_generation() {
            return Err(err(format!(
                "track {} generation {} != binding generation {}",
                track.id(),
                track.channel_generation().get(),
                generation.get()
            )));
        }
        if milestone > vivid_protocol::track::MILESTONE_KNOWN_MASK {
            return Err(err("unassigned milestone bits"));
        }
        self.bindings.push(SlotBinding {
            slot,
            track_id: track.id(),
            expected_channel_generation: generation,
            required_milestone: milestone,
        });
        Ok(self)
    }
    pub fn activate(&self, session: &mut Session) -> io::Result<()> {
        if self.bindings.is_empty() {
            return Err(err("at least one slot binding required"));
        }
        session
            .activate_tracks(&self.surface, &self.bindings, &RequestMetadata::default())
            .map(|_| ())
    }
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoordinateModel, ProducerConfig, Session, SurfaceDescriptor, SurfaceRole};

    fn ts() -> SurfaceDefinition {
        SurfaceDefinition {
            context_id: 1,
            surface_id: 1,
            semantic_profile: crate::DESKTOP_CONTENT.into(),
            coordinate_model: CoordinateModel::DesktopLogicalPixels,
            logical_width: 1920,
            logical_height: 1080,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: 0,
            descriptor: SurfaceDescriptor {
                role: SurfaceRole::Desktop,
                title: "test".into(),
                semantic_content_revision: 1,
                semantic_availability: 0,
                locator_hint: String::new(),
            },
            policy: 0,
            profile_parameters: DesktopSurfaceParameters {
                captured_origin_x: 0,
                captured_origin_y: 0,
                topology: vec![],
                semantic_generation: 1,
                input_capabilities: 0,
            }
            .encode(),
        }
    }

    fn big_contract() -> ResourceContract {
        let mut c = ResourceContract::new([1_000_000_000; 33]);
        c.set(
            Resource::ControlRecordBody,
            u64::from(vivid_protocol::CONTROL_MAX_RECORD_BODY),
        );
        c
    }

    #[test]
    fn mapping_change_revokes_input() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let r = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        let ds = DesktopSurface { raw: r };
        let mut d = ts();
        d.logical_width = 2560;
        assert!(
            ds.update(&mut s, d, &RequestMetadata::default())
                .unwrap()
                .input_must_be_revoked()
        );
        assert!(ds.generation() > SurfaceGeneration::ONE);
    }
    #[test]
    fn descriptor_only_keeps_generation() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let r = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        let ds = DesktopSurface { raw: r };
        let g = ds.generation();
        let mut d = ts();
        d.descriptor.title = "updated".into();
        assert!(
            !ds.update(&mut s, d, &RequestMetadata::default())
                .unwrap()
                .input_must_be_revoked()
        );
        assert_eq!(ds.generation(), g);
    }
    #[test]
    fn excessive_claims_refused() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let c = s.info().resource_contract.clone();
        let r = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        assert!(
            TrackBuilder::new(&r, 1, TrackMode::Live, LaneClass::Bulk)
                .video(65536, 65536, "h264")
                .build(&c, 7)
                .is_err()
        );
    }
    #[test]
    fn valid_track_builds() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let c = big_contract();
        let r = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        let t = TrackBuilder::new(&r, 1, TrackMode::Live, LaneClass::Bulk)
            .video(1920, 1080, "h264")
            .build(&c, 7)
            .unwrap();
        assert_eq!(t.surface_id, 1);
        assert_eq!(t.slot, 1);
    }

    /// The claim defaults a retained raster track needs follow from its geometry, and they used
    /// to be recomputed by hand in every language binding. The builder is the one place now, so
    /// these pin the arithmetic the bindings inherit.
    #[test]
    fn raster_claims_follow_from_geometry() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let contract = big_contract();
        let surface = session
            .create_surface(
                SurfaceBuilder::new(&session, 640, 480)
                    .unwrap()
                    .build()
                    .unwrap(),
                &RequestMetadata::default(),
            )
            .unwrap();
        let track = TrackBuilder::new(
            &surface,
            crate::SLOT_RASTER,
            TrackMode::Live,
            LaneClass::Bulk,
        )
        .raster(640, 480)
        .unwrap()
        .build(&contract, 7)
        .unwrap();
        // 72 bytes of packet header ahead of width * height * 4 bytes of pixels.
        assert_eq!(track.maximum_record_body, 72 + 640 * 480 * 4);
        assert_eq!(track.maximum_inflight_body_bytes, 2 * (72 + 640 * 480 * 4));
        assert_eq!(track.retained_pixel_charge, 640 * 480);
        assert!(matches!(
            track.kind,
            KindConfiguration::Raster(raster)
                if raster.width == 640 && raster.height == 480 && !raster.delta_enabled
        ));
    }

    #[test]
    fn raster_deltas_and_geometry_overflow() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let contract = big_contract();
        let surface = session
            .create_surface(
                SurfaceBuilder::new(&session, 64, 64)
                    .unwrap()
                    .build()
                    .unwrap(),
                &RequestMetadata::default(),
            )
            .unwrap();
        let track = TrackBuilder::new(
            &surface,
            crate::SLOT_RASTER,
            TrackMode::Live,
            LaneClass::Bulk,
        )
        .raster(64, 64)
        .unwrap()
        .raster_deltas(8, true)
        .build(&contract, 7)
        .unwrap();
        assert!(matches!(
            track.kind,
            KindConfiguration::Raster(raster)
                if raster.delta_enabled
                    && raster.maximum_delta_operations == 8
                    && raster.zstd_enabled
        ));

        // A geometry whose pixel count leaves u32 record territory must be refused at the call
        // rather than wrapping into a small claim the contract would happily admit.
        assert!(
            TrackBuilder::new(
                &surface,
                crate::SLOT_RASTER,
                TrackMode::Live,
                LaneClass::Bulk
            )
            .raster(u32::MAX, u32::MAX)
            .is_err()
        );
    }

    /// An image track exists to send exactly one container once, so its claims and rate are the
    /// minimum that admits a single record.
    #[test]
    fn image_track_claims_one_record_once() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let contract = session.info().resource_contract.clone();
        let surface = session
            .create_surface(
                SurfaceBuilder::new(&session, 32, 32)
                    .unwrap()
                    .build()
                    .unwrap(),
                &RequestMetadata::default(),
            )
            .unwrap();
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&32_u32.to_be_bytes());
        png.extend_from_slice(&32_u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        let image = crate::probe_encoded_image(&png).unwrap();
        let track = TrackBuilder::new(
            &surface,
            crate::SLOT_POSTER,
            TrackMode::Live,
            LaneClass::Bulk,
        )
        .image(image)
        .unwrap()
        .build(&contract, 9)
        .unwrap();
        assert_eq!(track.maximum_record_body as usize, png.len());
        assert_eq!(track.maximum_rate_millihertz, 1);
        assert_eq!(track.maximum_records_per_second, 1);
        assert_eq!(track.retained_pixel_charge, 32 * 32);
        assert!(matches!(
            track.kind,
            KindConfiguration::EncodedImage(image) if image.encoded_length as usize == png.len()
        ));
    }

    /// The builder owns the identity defaults so bindings do not have to: root context, next
    /// allocated id, generic content, desktop logical pixels. Everything else is opt-in.
    #[test]
    fn surface_builder_defaults_and_desktop_parameters() {
        let session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = SurfaceBuilder::new(&session, 800, 600)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(surface.context_id, session.info().root_context_id);
        assert!(session.info().root_context_id > 0);
        assert_eq!(surface.semantic_profile, crate::GENERIC_CONTENT);
        assert_eq!(
            surface.coordinate_model,
            CoordinateModel::DesktopLogicalPixels
        );
        assert_eq!((surface.logical_width, surface.logical_height), (800, 600));
        assert_eq!(surface.descriptor.role, SurfaceRole::Unspecified);
        assert_eq!(surface.profile_parameters, Vec::new());

        let topology = crate::OutputDescriptor {
            output_id: 1,
            origin_x: 0,
            origin_y: 0,
            width: 1920,
            height: 1080,
            scale_numerator: 2,
            scale_denominator: 1,
            rotation: crate::Rotation::None,
            primary: true,
        };
        let parameters = DesktopSurfaceParameters {
            captured_origin_x: -1920,
            captured_origin_y: 0,
            topology: vec![topology],
            semantic_generation: 4,
            input_capabilities: 3,
        };
        let desktop = SurfaceBuilder::new(&session, 1920, 1080)
            .unwrap()
            .titled(SurfaceRole::Desktop, "screen")
            .desktop(&parameters)
            .build()
            .unwrap();
        let decoded = crate::DesktopSurfaceParameters::decode(&desktop.profile_parameters)
            .expect("desktop parameters must round-trip");
        assert_eq!(decoded.captured_origin_x, -1920);
        assert_eq!(decoded.semantic_generation, 4);
        assert_eq!(decoded.topology.len(), 1);

        // An invalid surface is refused by the builder, before it can become a request.
        let broken = SurfaceBuilder::new(&session, 0, 600).unwrap().build();
        assert!(broken.is_err());
    }

    #[test]
    fn web_contract_narrows_video_records_and_access_units_before_create_track() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let mut contract = big_contract();
        contract.set(
            Resource::MediaRecordBody,
            u64::from(vivid_protocol::web::MAX_MEDIA_RECORD_BODY),
        );
        contract.set(
            Resource::InflightMediaBytes,
            vivid_protocol::web::MAX_AGGREGATE_REASSEMBLY / 2,
        );
        let surface = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        let track = TrackBuilder::new(&surface, 1, TrackMode::Live, LaneClass::Bulk)
            .video(1920, 1080, "h264")
            .build(&contract, 7)
            .unwrap();
        assert_eq!(
            track.maximum_record_body,
            vivid_protocol::web::MAX_MEDIA_RECORD_BODY
        );
        assert_eq!(
            track.maximum_inflight_body_bytes,
            vivid_protocol::web::MAX_AGGREGATE_REASSEMBLY / 2
        );
        let KindConfiguration::Video(video) = track.kind else {
            panic!("video builder returned another track kind")
        };
        assert_eq!(
            video.maximum_access_unit_bytes,
            vivid_protocol::web::MAX_MEDIA_RECORD_BODY - 48
        );
    }

    #[test]
    fn rate_and_bitrate_claims_are_enforced_before_track_creation() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let mut contract = big_contract();
        contract.set(Resource::EncodedBitsPerSecond, 1);
        contract.set(Resource::MediaRecordsPerSecond, 1);
        let surface = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        assert!(
            TrackBuilder::new(&surface, 1, TrackMode::Live, LaneClass::Bulk)
                .video(1920, 1080, "h264")
                .build(&contract, 7)
                .is_err()
        );
    }
    #[test]
    fn slot_gen_must_match_current() {
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let c = big_contract();
        let r = s.create_surface(ts(), &RequestMetadata::default()).unwrap();
        let t = TrackBuilder::new(&r, 1, TrackMode::Live, LaneClass::Bulk)
            .video(1920, 1080, "h264")
            .build(&c, 7)
            .unwrap();
        let tk = s.create_track(t, &RequestMetadata::default()).unwrap();
        let generation = tk.channel_generation();
        assert_ne!(generation.get(), 0);
        let mut slots = SurfaceSlots::new(&r);
        assert!(slots.require(1, &tk, generation, 1 << 4).is_ok());
        assert!(
            slots
                .require(1, &tk, ChannelGeneration::new(999), 1 << 4)
                .is_err()
        );
    }
}
