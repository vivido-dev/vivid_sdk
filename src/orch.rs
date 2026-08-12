//! Orchestration types a desktop producer layers over the raw `Session` API.
use std::io;

use vivid_protocol::resource::{Resource, ResourceContract};
use vivid_protocol::revision::{ChannelGeneration, SurfaceGeneration, SurfaceRevision};
use vivid_protocol::surface::{DesktopSurfaceParameters, SurfaceDefinition};
use vivid_protocol::track::{KindConfiguration, TrackConfiguration, TrackMode};

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

pub struct TrackBuilder {
    surface: Surface,
    slot: u64,
    mode: TrackMode,
    lane: LaneClass,
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
        Self {
            surface: surface.clone(),
            slot,
            mode,
            lane,
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
            context_id: self.surface.context_id(),
            surface_id: self.surface.id(),
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
