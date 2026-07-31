//! The desktop session orchestrator.
use std::io;

use vivid_protocol::surface::SurfaceDefinition;
use vivid_protocol::track::TrackConfiguration;

use crate::{
    DesktopSurface, InputBindingGuard, InputLane, InputQueue, RequestMetadata, Session,
    SurfaceSlots, TrackSender,
};

pub struct DesktopSession {
    session: Session,
    surface: DesktopSurface,
    video_track: crate::Track,
    video_sender: TrackSender,
    audio_track: Option<crate::Track>,
    audio_sender: Option<TrackSender>,
    guard: InputBindingGuard,
    lane: Option<InputLane>,
    input_queue: Option<InputQueue>,
}

impl DesktopSession {
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }
    pub fn desktop_surface(&self) -> &DesktopSurface {
        &self.surface
    }
    pub fn video_track(&self) -> &crate::Track {
        &self.video_track
    }
    pub fn video_sender(&self) -> &TrackSender {
        &self.video_sender
    }
    pub fn audio_track(&self) -> Option<&crate::Track> {
        self.audio_track.as_ref()
    }
    pub fn audio_sender(&self) -> Option<&TrackSender> {
        self.audio_sender.as_ref()
    }
    pub fn guard(&self) -> &InputBindingGuard {
        &self.guard
    }
    pub fn guard_mut(&mut self) -> &mut InputBindingGuard {
        &mut self.guard
    }
    pub fn lane(&self) -> Option<&InputLane> {
        self.lane.as_ref()
    }
    pub fn input_queue(&self) -> Option<&InputQueue> {
        self.input_queue.as_ref()
    }

    pub fn establish(
        mut session: Session,
        surface_def: SurfaceDefinition,
        video_cfg: TrackConfiguration,
        audio_cfg: Option<TrackConfiguration>,
    ) -> io::Result<Self> {
        let contract = session.info().resource_contract.clone();
        let raw = session.create_surface(surface_def, &RequestMetadata::default())?;
        let surface = DesktopSurface { raw };

        let geom = vivid_protocol::geometry::NodeGeometry::full_target();
        let node = vivid_protocol::scene::SceneNode {
            owning_context_id: session.info().root_context_id,
            node_id: 1,
            surface_context_id: surface.context_id(),
            surface_id: surface.id(),
            geometry: geom.encode(),
            fit: vivid_protocol::scene::Fit::Contain,
            linear_sampling: false,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        };
        session.create_node(&node, &RequestMetadata::default())?;

        let video_track = Self::create_track(&mut session, &surface, &video_cfg, &contract)?;
        let video_channel = session.open_track_channel(&video_track)?;
        let video_sender = TrackSender::new(video_channel);

        let (audio_track, audio_sender) = if let Some(cfg) = &audio_cfg {
            let t = Self::create_track(&mut session, &surface, cfg, &contract)?;
            let ch = session.open_track_channel(&t)?;
            (Some(t), Some(TrackSender::new(ch)))
        } else {
            (None, None)
        };

        let mut slots = SurfaceSlots::new(surface.inner());
        slots.require(1, &video_track, video_sender.generation(), 1 << 4)?;
        if let (Some(at), Some(asender)) = (&audio_track, &audio_sender) {
            slots.require(2, at, asender.generation(), 1 << 4)?;
        }
        slots.activate(&mut session)?;

        let guard = InputBindingGuard::new();
        let (lane, input_queue) = if session.supports(crate::DESKTOP_INPUT) {
            let l = session.open_input_lane(1)?;
            (Some(l), Some(InputQueue::new(1000)))
        } else {
            (None, None)
        };

        Ok(Self {
            session,
            surface,
            video_track,
            video_sender,
            audio_track,
            audio_sender,
            guard,
            lane,
            input_queue,
        })
    }

    pub fn replace_video_track(
        &mut self,
        new_cfg: TrackConfiguration,
        key_unit: &[u8],
    ) -> io::Result<()> {
        let contract = self.session.info().resource_contract.clone();
        let new_track = Self::create_track(&mut self.session, &self.surface, &new_cfg, &contract)?;
        let new_channel = self.session.open_track_channel(&new_track)?;
        let new_sender = TrackSender::new(new_channel);
        new_sender.send(&crate::EncodedPacket::Video(
            crate::pipeline::VideoPacketData {
                epoch: 1,
                packet_id: new_sender.next_packet_id(),
                pts_us: 0,
                dts_us: 0,
                duration_us: 0,
                key: true,
                data: key_unit.to_vec(),
            },
        ))?;
        let mut slots = SurfaceSlots::new(self.surface.inner());
        slots.require(1, &new_track, new_sender.generation(), 1 << 4)?;
        if let (Some(at), Some(a_sender)) = (&self.audio_track, &self.audio_sender) {
            slots.require(2, at, a_sender.generation(), 1 << 4)?;
        }
        slots.activate(&mut self.session)?;
        self.session
            .destroy_track(&self.video_track, &RequestMetadata::default())?;
        self.video_track = new_track;
        self.video_sender = new_sender;
        Ok(())
    }

    pub fn close(mut self) -> io::Result<()> {
        self.guard.release();
        if let Some(l) = self.lane.take() {
            l.close()?;
        }
        if let Some(s) = &self.audio_sender {
            s.detach();
        }
        self.video_sender.detach();
        self.session.close()
    }

    fn create_track(
        s: &mut Session,
        _surf: &DesktopSurface,
        cfg: &TrackConfiguration,
        _contract: &vivid_protocol::resource::ResourceContract,
    ) -> io::Result<crate::Track> {
        s.create_track(cfg.clone(), &RequestMetadata::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use vivid_protocol::track::TrackMode;

    fn surf() -> SurfaceDefinition {
        SurfaceDefinition {
            context_id: 1,
            surface_id: 1,
            semantic_profile: DESKTOP_CONTENT.into(),
            coordinate_model: CoordinateModel::DesktopLogicalPixels,
            logical_width: 1920,
            logical_height: 1080,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: 0,
            descriptor: SurfaceDescriptor {
                role: SurfaceRole::Desktop,
                title: "t".into(),
                semantic_content_revision: 1,
                semantic_availability: 0,
                locator_hint: String::new(),
            },
            policy: 0,
            profile_parameters: vec![],
        }
    }
    fn vcfg(sid: u64, tid: u64) -> TrackConfiguration {
        TrackConfiguration {
            context_id: 1,
            surface_id: sid,
            track_id: tid,
            slot: 1,
            mode: TrackMode::Live,
            lane: LaneClass::Bulk,
            maximum_record_body: 65536,
            maximum_rate_millihertz: 30000,
            maximum_encoded_bits_per_second: 50_000_000,
            maximum_records_per_second: 30,
            maximum_inflight_body_bytes: 131072,
            kind: KindConfiguration::Video(VideoConfiguration {
                codec: "h264".into(),
                packetization: "h264-annexb-au-v1".into(),
                extradata: vec![],
                coded_width: 1920,
                coded_height: 1080,
                profile: 0,
                level: 0,
                maximum_reorder_depth: 0,
                color_primaries: 1,
                transfer: 1,
                matrix: 1,
                signal_range: 1,
                aspect_numerator: 1,
                aspect_denominator: 1,
                maximum_access_unit_bytes: 4096,
                codec_string: None,
                decoder_configuration: None,
            }),
            target_latency_us: 33000,
            maximum_latency_us: 100000,
            retained_pixel_charge: 2073600,
        }
    }

    #[test]
    fn establish_creates_surface_and_tracks() {
        let s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let ds = DesktopSession::establish(s, surf(), vcfg(1, 7), None).unwrap();
        assert_eq!(ds.desktop_surface().id(), 1);
        assert_eq!(ds.video_track().id(), 7);
        assert!(ds.audio_track().is_none());
        assert!(ds.close().is_ok());
    }
    #[test]
    fn replace_video_track_leaves_surface() {
        let s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let mut ds = DesktopSession::establish(s, surf(), vcfg(1, 7), None).unwrap();
        let g = ds.desktop_surface().generation();
        ds.replace_video_track(vcfg(1, 9), &[0, 0, 0, 1, 0x67])
            .unwrap();
        assert_eq!(ds.desktop_surface().generation(), g);
        assert_eq!(ds.desktop_surface().id(), 1);
        assert_eq!(ds.video_track().id(), 9);
        assert!(ds.close().is_ok());
    }
    #[test]
    fn shutdown_releases_and_closes() {
        let s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let ds = DesktopSession::establish(s, surf(), vcfg(1, 7), None).unwrap();
        assert!(ds.close().is_ok());
    }
}
