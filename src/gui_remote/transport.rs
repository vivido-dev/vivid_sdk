//! Worker-owned surface lifecycle, media flow, and authenticated input reception.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{
    DesktopPreconditions, InputLane, InputLaneEvent, RequestMetadata, Session, SessionEvent,
    SlotBinding, Surface, Track, TrackChannel,
};
use vivid_protocol::{
    geometry::NodeGeometry,
    messages::LaneClass,
    registry,
    resource::Resource,
    scene::{Fit, SceneNode},
    surface::{
        CoordinateModel, DesktopSurfaceParameters, SurfaceDefinition, SurfaceDescriptor,
        SurfaceRole,
    },
    track::{
        KindConfiguration, MILESTONE_OUTPUT_READY, MILESTONE_PRESENTED, RasterConfiguration,
        TrackConfiguration, TrackMode,
    },
};

// Bound service work per turn and retry eligibility at most four times per second.
const SERVICE_INTERVAL: Duration = Duration::from_millis(5);
const BIND_RETRY_INTERVAL: Duration = Duration::from_millis(250);

pub(super) struct MediaJob {
    id: u64,
    channel: Arc<TrackChannel>,
    frame: Frame,
    frame_id: u64,
    interval: Duration,
}

pub(super) fn media(shared: &Shared) {
    let mut last = Instant::now() - FRAME_INTERVAL;
    while !shared.stopped.load(Ordering::Acquire) {
        let ready = shared
            .media
            .lock()
            .expect("media mutex")
            .as_ref()
            .is_some_and(|job| last.elapsed() >= job.interval);
        if !ready {
            std::thread::sleep(SERVICE_INTERVAL);
            continue;
        }
        let Some(job) = shared.media.lock().expect("media mutex").take() else {
            continue;
        };
        if shared.media_id.load(Ordering::Acquire) != job.id {
            continue;
        }
        if let Err(error) = job
            .channel
            .send_raster(0, job.frame_id, &job.frame.rgba, false)
            && shared.media_id.load(Ordering::Acquire) == job.id
            && !shared.stopped.load(Ordering::Acquire)
        {
            let cancel = shared.cancel.lock().expect("cancel mutex");
            if let Some((connection, cancel)) = cancel
                .as_ref()
                .filter(|(connection, _)| *connection == job.frame.geometry.connection)
            {
                shared
                    .media_failure
                    .lock()
                    .expect("media failure mutex")
                    .get_or_insert((*connection, error.kind()));
                cancel();
            }
        }
        last = Instant::now();
    }
}

fn geometry(session: &Session, connection: u64) -> io::Result<Geometry> {
    let target = session
        .info()
        .desktop_target()
        .ok_or_else(|| io::Error::other("remote GUI requires a desktop target"))?;
    if target.width == 0
        || target.height == 0
        || target.width > MAX_LOGICAL_DIMENSION
        || target.height > MAX_LOGICAL_DIMENSION
    {
        return Err(io::Error::other("remote GUI target extent exceeds bounds"));
    }
    Ok(Geometry {
        connection,
        generation: session.info().target_generation.get(),
        width: target.width,
        height: target.height,
    })
}

fn surface_definition(session: &Session, geometry: Geometry) -> SurfaceDefinition {
    SurfaceDefinition {
        context_id: session.info().root_context_id,
        surface_id: 1,
        semantic_profile: registry::DESKTOP_CONTENT.into(),
        coordinate_model: CoordinateModel::DesktopLogicalPixels,
        logical_width: u64::from(geometry.width),
        logical_height: u64::from(geometry.height),
        scale_numerator: 1,
        scale_denominator: 1,
        rotation: 0,
        descriptor: SurfaceDescriptor {
            role: SurfaceRole::ApplicationCanvas,
            title: "SDK application".into(),
            semantic_content_revision: 1,
            semantic_availability: 0,
            locator_hint: String::new(),
        },
        policy: 0,
        profile_parameters: DesktopSurfaceParameters {
            captured_origin_x: 0,
            captured_origin_y: 0,
            topology: vec![],
            semantic_generation: geometry.generation,
            input_capabilities: crate::INPUT_CLASS_KNOWN_MASK,
        }
        .encode(),
    }
}

struct Presentation {
    media_id: u64,
    surface: Surface,
    track: Track,
    channel: Arc<TrackChannel>,
    geometry: Geometry,
    physical: (u32, u32),
    interval: Duration,
    active: bool,
    old: Option<Track>,
    frame_id: u64,
}

fn configure(
    shared: &Shared,
    session: &mut Session,
    previous: Option<Presentation>,
    frame: &Frame,
    next_media_id: &mut u64,
    graph: &mut recovery::Graph,
) -> io::Result<Presentation> {
    let fresh_connection = previous.is_none();
    if let Some(previous) = previous {
        shared.media_id.store(0, Ordering::Release);
        previous.channel.close()?;
        shared.injection.lock().expect("input mutex").revoke();
        if let Some(old) = previous.old {
            graph.retire(session, &old)?;
        }
    }
    let mut desired = surface_definition(session, frame.geometry);
    let surface = graph.surface(session, desired.clone())?;
    desired.policy |= surface.definition()?.policy;
    if surface.definition()? != desired {
        session.update_surface(&surface, desired, &RequestMetadata::default())?;
    }
    let context = session.info().root_context_id;
    graph.node(
        session,
        &SceneNode {
            owning_context_id: context,
            node_id: 1,
            surface_context_id: context,
            surface_id: surface.id(),
            geometry: NodeGeometry::full_target().encode(),
            fit: Fit::Contain,
            linear_sampling: false,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        },
    )?;
    let bytes = u64::try_from(frame.rgba.len()).map_err(io::Error::other)?;
    let body = bytes
        .checked_add(72)
        .ok_or_else(|| io::Error::other("GUI raster record overflowed"))?;
    let bits = body
        .checked_mul(8)
        .ok_or_else(|| io::Error::other("GUI raster rate overflowed"))?;
    let fps = (session
        .info()
        .resource_contract
        .get(Resource::EncodedBitsPerSecond)
        / bits)
        .min(30);
    if fps == 0 {
        return Err(io::Error::other(
            "GUI frame exceeds the negotiated media rate",
        ));
    }
    let reusable = if fresh_connection {
        graph.reusable(session, (frame.width, frame.height))?
    } else {
        None
    };
    let track = if let Some(track) = reusable {
        track
    } else {
        graph.make_room(session)?;
        graph.create_track(
            session,
            TrackConfiguration {
                direction: Default::default(),
                context_id: surface.context_id(),
                surface_id: surface.id(),
                track_id: session.allocate_id()?,
                slot: 1,
                mode: TrackMode::Live,
                lane: LaneClass::Bulk,
                maximum_record_body: u32::try_from(body).map_err(io::Error::other)?,
                maximum_rate_millihertz: fps * 1000,
                maximum_encoded_bits_per_second: bits * fps,
                maximum_records_per_second: fps,
                maximum_inflight_body_bytes: body,
                kind: KindConfiguration::Raster(RasterConfiguration {
                    width: frame.width,
                    height: frame.height,
                    alpha_mode: 1,
                    delta_enabled: false,
                    maximum_delta_operations: 1,
                    zstd_enabled: false,
                }),
                target_latency_us: 0,
                maximum_latency_us: 0,
                retained_pixel_charge: bytes / 4,
            },
        )?
    };
    let old = graph.other(track.id());
    let sequence = track
        .inner
        .lock()
        .expect("track mutex")
        .media_sequence
        .clone();
    let frame_id = sequence.lock().expect("media sequence mutex").last_id;
    let channel = Arc::new(session.open_track_channel(&track)?);
    *next_media_id = next_media_id
        .checked_add(1)
        .ok_or_else(|| io::Error::other("remote media generation exhausted"))?;
    shared.media_id.store(*next_media_id, Ordering::Release);
    Ok(Presentation {
        media_id: *next_media_id,
        surface,
        track,
        channel,
        geometry: frame.geometry,
        physical: (frame.width, frame.height),
        interval: Duration::from_micros(1_000_000_u64.div_ceil(fps)),
        active: false,
        old,
        frame_id,
    })
}

fn receive_input(shared: &Shared, lane: &InputLane, current: &Presentation) -> io::Result<()> {
    for _ in 0..INPUT_CAPACITY {
        let Some(event) = lane.take_event()? else {
            break;
        };
        let mut injection = shared.injection.lock().expect("input mutex");
        let now = injection.now();
        match event {
            InputLaneEvent::Input { .. } => {
                let event = event.decode_input(
                    u64::from(current.geometry.width),
                    u64::from(current.geometry.height),
                )?;
                if injection.guard.is_armed(now)
                    && injection.guard.current_tag() == Some(event.binding())
                {
                    if injection.events.len() >= INPUT_CAPACITY {
                        injection.revoke();
                        return Err(io::Error::other("remote GUI input queue overflowed"));
                    }
                    injection.events.push_back(event);
                }
            }
            InputLaneEvent::Renew(renewal) => {
                let _ = injection.guard.handle_renewal(&renewal, now);
            }
            InputLaneEvent::Revoked(termination) | InputLaneEvent::Reset(termination) => {
                if injection.guard.handle_revocation(&termination).is_ok() {
                    injection.revoke();
                }
            }
            InputLaneEvent::LaneClosed { .. } => {
                injection.revoke();
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "remote GUI input lane ended",
                ));
            }
            InputLaneEvent::Error(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "remote GUI input protocol error",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn connected(
    shared: &Shared,
    session: &mut Session,
    connection: u64,
    next_media_id: &mut u64,
    presented: &mut bool,
    graph: &mut recovery::Graph,
) -> io::Result<()> {
    let mut requested = geometry(session, connection)?;
    *shared.geometry.lock().expect("geometry mutex") = Some(requested);
    let lane = if session.supports(crate::DESKTOP_INPUT) {
        Some(
            session.open_input_lane(
                session
                    .info()
                    .resume_generation
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("remote input lane generation exhausted"))?,
            )?,
        )
    } else {
        None
    };
    let mut presentation: Option<Presentation> = None;
    let mut last_binding = Instant::now() - BIND_RETRY_INTERVAL;
    let result = (|| {
        while !shared.stopped.load(Ordering::Acquire) {
            for _ in 0..INPUT_CAPACITY {
                let Some(event) = session.take_event()? else {
                    break;
                };
                match event {
                    SessionEvent::TargetChanged(payload) => {
                        shared.injection.lock().expect("input mutex").revoke();
                        session.apply_target_changed(&payload)?;
                        requested = geometry(session, connection)?;
                        *shared.geometry.lock().expect("geometry mutex") = Some(requested);
                    }
                    SessionEvent::ConnectionClosed { .. } => {
                        return Err(io::Error::new(
                            session
                                .lifecycle
                                .failure_kind
                                .lock()
                                .expect("failure mutex")
                                .unwrap_or(io::ErrorKind::Other),
                            "remote GUI control connection ended",
                        ));
                    }
                    _ => (),
                }
            }
            let frame = shared.frame.lock().expect("frame mutex").take();
            if let Some(frame) = frame.filter(|frame| frame.geometry == requested) {
                if presentation.as_ref().is_none_or(|old| {
                    old.geometry != frame.geometry || old.physical != (frame.width, frame.height)
                }) {
                    presentation = Some(configure(
                        shared,
                        session,
                        presentation.take(),
                        &frame,
                        next_media_id,
                        graph,
                    )?);
                }
                let current = presentation.as_mut().expect("configured presentation");
                current.frame_id = current
                    .frame_id
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("GUI frame ID exhausted"))?;
                *shared.media.lock().expect("media mutex") = Some(MediaJob {
                    id: current.media_id,
                    channel: current.channel.clone(),
                    frame,
                    frame_id: current.frame_id,
                    interval: current.interval,
                });
            }
            if let Some(current) = presentation.as_mut() {
                for _ in 0..INPUT_CAPACITY {
                    match current.channel.take_event()? {
                        Some(crate::ChannelEvent::Error(_)) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "remote GUI media protocol error",
                            ));
                        }
                        Some(crate::ChannelEvent::NeedFullFrame(_)) => {
                            return Err(io::Error::new(
                                io::ErrorKind::ConnectionReset,
                                "remote GUI requires a fresh full frame",
                            ));
                        }
                        Some(_) => (),
                        None => break,
                    }
                }
                // Static applications may never send another frame. Observe
                // reverse-channel closure independently of the media sender.
                if current
                    .channel
                    .flow
                    .state
                    .lock()
                    .expect("flow mutex")
                    .closed
                {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "remote GUI media channel ended",
                    ));
                }
                if !current.active {
                    let status = session.query_track(&current.track)?;
                    if status.milestones & MILESTONE_OUTPUT_READY != 0 {
                        session.activate_tracks(
                            &current.surface,
                            &[SlotBinding {
                                slot: 1,
                                track_id: current.track.id(),
                                expected_channel_generation: current.track.channel_generation(),
                                required_milestone: MILESTONE_OUTPUT_READY,
                            }],
                            &RequestMetadata::default(),
                        )?;
                        current.active = true;
                        graph.activated(current.track.id());
                        if let Some(old) = current.old.take() {
                            graph.retire(session, &old)?;
                        }
                    }
                }
                if current.active && !*presented {
                    *presented =
                        session.query_track(&current.track)?.milestones & MILESTONE_PRESENTED != 0;
                }
                if let Some(lane) = &lane {
                    receive_input(shared, lane, current)?;
                    let needs_binding = {
                        let mut injection = shared.injection.lock().expect("input mutex");
                        if injection.guard.is_expired(injection.now()) {
                            injection.revoke();
                        }
                        current.active
                            && current.geometry == requested
                            && injection.guard.grant().is_none()
                            && last_binding.elapsed() >= BIND_RETRY_INTERVAL
                    };
                    let ready = needs_binding
                        && session.query_track(&current.track)?.milestones & MILESTONE_PRESENTED
                            != 0;
                    if needs_binding && ready {
                        let binding = {
                            let mut injection = shared.injection.lock().expect("input mutex");
                            injection.guard.set_preconditions(DesktopPreconditions {
                                surface_present: true,
                                surface_generation: current.surface.generation(),
                                capability_mask: crate::INPUT_CLASS_KNOWN_MASK,
                                presented: true,
                                lane_live: true,
                            });
                            injection.guard.enable_for_context(
                                current.surface.context_id(),
                                current.surface.id(),
                                current.surface.generation(),
                                crate::INPUT_CLASS_KNOWN_MASK,
                                2_000_000,
                                6,
                            )?
                        };
                        let status = lane.set_binding(&binding)?;
                        let mut injection = shared.injection.lock().expect("input mutex");
                        let now = injection.now();
                        injection.guard.handle_bound_at(&status, now)?;
                        last_binding = Instant::now();
                    }
                }
            }
            std::thread::sleep(SERVICE_INTERVAL);
        }
        Ok(())
    })();
    if let Some(current) = presentation {
        let _ = current.channel.close();
    }
    result
}
