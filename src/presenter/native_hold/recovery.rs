//! Correlated replacement requests and fresh native group admission.

// Rust guideline compliant 2026-10-07
use super::*;

pub(super) struct Recovery {
    release: Release,
    original: Vec<Original>,
    requests: HashMap<u64, Request>,
    play: Option<Vec<Scope>>,
}
#[derive(Clone, Copy, Default)]
struct Release {
    resume: Option<u64>,
    host: Option<u64>,
    device: Option<Device>,
}
#[derive(Clone, Copy)]
struct Device {
    host: u64,
    available: bool,
}
#[derive(Clone, Copy)]
pub(super) enum Loss {
    Lease,
    Host,
    Device,
}
struct Original {
    track: u64,
    generation: ChannelGeneration,
    incarnation: Weak<()>,
}
struct Request {
    serial: u64,
    generation: ChannelGeneration,
    resume: u64,
    writer: Weak<Writer>,
}
struct Scope {
    key: TrackKey,
    generation: ChannelGeneration,
    epoch: u32,
    reset: u64,
    play: u64,
    resume: u64,
    surface: SurfaceGeneration,
    incarnation: Weak<()>,
    writer: Option<Weak<Writer>>,
}
impl Scope {
    fn capture(state: &State, key: TrackKey) -> Option<Self> {
        let track = state.tracks.get(&key)?;
        let (incarnation, play) = identity(track)?;
        Some(Self {
            key,
            generation: track.state.channel_generation,
            epoch: track.state.media_epoch,
            reset: track.decoder_reset_serial,
            play,
            resume: state.sessions.get(&key.surface.session)?.resume_generation,
            surface: state.surfaces.get(&key.surface)?.state.generation,
            incarnation,
            writer: track.channel_writer.as_ref().map(Arc::downgrade),
        })
    }
    fn current(&self, state: &State) -> bool {
        state
            .sessions
            .get(&self.key.surface.session)
            .is_some_and(|s| !s.closed && s.resume_generation == self.resume)
            && state
                .surfaces
                .get(&self.key.surface)
                .is_some_and(|s| s.state.generation == self.surface)
            && state.tracks.get(&self.key).is_some_and(|t| {
                !t.state.lost
                    && t.state.channel_generation == self.generation
                    && t.state.media_epoch == self.epoch
                    && t.decoder_reset_serial == self.reset
                    && t.accepted_surface_generation == Some((self.surface, self.generation))
                    && match (&self.writer, &t.channel_writer) {
                        (Some(old), Some(current)) => Weak::ptr_eq(old, &Arc::downgrade(current)),
                        (_, None) => t.eos_epoch == Some(self.epoch),
                        (None, Some(_)) => false,
                    }
                    && identity(t).is_some_and(|(token, play)| {
                        play == self.play && Weak::ptr_eq(&token, &self.incarnation)
                    })
            })
    }
}
fn members(state: &State, key: SurfaceKey) -> Vec<TrackKey> {
    state
        .surfaces
        .get(&key)
        .into_iter()
        .flat_map(|s| s.active_slots.values())
        .filter_map(|id| {
            let key = TrackKey {
                surface: key,
                track: *id,
            };
            state
                .tracks
                .get(&key)
                .filter(|t| t.configuration.mode == TrackMode::Timed && identity(t).is_some())
                .map(|_| key)
        })
        .collect()
}
pub(super) fn capture(state: &State, key: SurfaceKey, loss: Loss) -> Recovery {
    let mut release = state
        .surfaces
        .get(&key)
        .and_then(|s| s.native_checkpoint.recovery.as_ref())
        .map_or_else(Release::default, |r| r.release);
    match loss {
        Loss::Lease => release.resume = Some(state.sessions[&key.session].resume_generation),
        Loss::Host => release.host = Some(state.native_host_generation),
        Loss::Device => {
            release.device = Some(Device {
                host: state.native_host_generation,
                available: false,
            });
        }
    }
    Recovery {
        release,
        original: members(state, key)
            .into_iter()
            .filter_map(|key| {
                let track = state.tracks.get(&key)?;
                Some(Original {
                    track: key.track,
                    generation: track.state.channel_generation,
                    incarnation: identity(track)?.0,
                })
            })
            .collect(),
        requests: HashMap::new(),
        play: None,
    }
}
pub(super) fn recover_device(state: &mut State, key: SurfaceKey) -> bool {
    let Some(r) = state
        .surfaces
        .get_mut(&key)
        .and_then(|s| s.native_checkpoint.recovery.as_mut())
    else {
        return false;
    };
    if let Some(device) = &mut r.release.device
        && !device.available
    {
        device.available = true;
        return true;
    }
    false
}
pub(in super::super) fn validate_play(
    state: &State,
    key: SurfaceKey,
    request: PlayRequest,
) -> io::Result<()> {
    let Some(recovery) = state
        .surfaces
        .get(&key)
        .and_then(|s| s.native_checkpoint.recovery.as_ref())
    else {
        return Ok(());
    };
    if request.start_policy != 2 {
        return Err(invalid(
            "native decoder replacement requires synchronized PLAY",
        ));
    }
    let current = members(state, key);
    if current.is_empty() {
        return Err(invalid("native replacement has no timed members"));
    }
    for key in current {
        let track = &state.tracks[&key];
        if track.state.lost {
            return Err(invalid("native replacement member is lost"));
        }
        if track.channel_writer.is_none() && track.eos_epoch != Some(track.state.media_epoch) {
            return Err(invalid("native replacement member has no channel"));
        }
        if recovery.original.iter().any(|old| {
            old.track == key.track
                && identity(track).is_some_and(|(token, _)| Weak::ptr_eq(&token, &old.incarnation))
                && track.state.channel_generation <= old.generation
        }) {
            return Err(invalid(
                "native replacement member retains its retired generation",
            ));
        }
    }
    Ok(())
}
pub(in super::super) fn arm(state: &mut State, key: SurfaceKey) {
    let scopes = members(state, key)
        .into_iter()
        .filter_map(|key| Scope::capture(state, key))
        .collect();
    if let Some(recovery) = state
        .surfaces
        .get_mut(&key)
        .and_then(|s| s.native_checkpoint.recovery.as_mut())
    {
        recovery.play = Some(scopes);
    }
}
pub(super) fn reposition(state: &mut State, key: SurfaceKey) {
    if let Some(recovery) = state
        .surfaces
        .get_mut(&key)
        .and_then(|s| s.native_checkpoint.recovery.as_mut())
    {
        recovery.play = None;
    }
}
fn prepared(state: &State, key: SurfaceKey, active: &[TrackKey]) -> bool {
    #[cfg(feature = "vector-video")]
    if !active
        .iter()
        .all(|key| native_video::recovery_ready(state, *key))
    {
        return false;
    }
    #[cfg(feature = "vector-audio")]
    if !active
        .iter()
        .all(|key| native_audio::recovery_ready(state, *key))
    {
        return false;
    }
    !active.is_empty() && state.surfaces.contains_key(&key)
}
pub(super) fn tick(state: &mut State, key: SurfaceKey, hidden: bool) -> bool {
    let Some(mut recovery) = state
        .surfaces
        .get_mut(&key)
        .and_then(|s| s.native_checkpoint.recovery.take())
    else {
        return false;
    };
    let mut hold = state.surfaces[&key]
        .hold
        .clone()
        .expect("native recovery hold");
    let host_live = state
        .native_host_alive
        .upgrade()
        .is_some_and(|alive| alive.load(Ordering::Acquire));
    // A replacement releases only failures from an older attachment. A new
    // failure during that replacement transaction needs its own recovery signal.
    if let Some(device) = &mut recovery.release.device
        && state.native_host_generation > device.host
    {
        device.available = true;
    }
    if host_live
        && recovery
            .release
            .resume
            .is_none_or(|old| state.sessions[&key.session].resume_generation != old)
        && recovery
            .release
            .host
            .is_none_or(|old| state.native_host_generation > old)
    {
        hold.reasons &= !vivid_protocol::timed::HOLD_DETACHED;
    }
    if host_live
        && recovery
            .release
            .device
            .is_some_and(|device| device.available)
    {
        hold.reasons &= !vivid_protocol::timed::HOLD_DOWNSTREAM;
    }
    hold.reasons = if hidden {
        hold.reasons | vivid_protocol::timed::HOLD_NOT_VISIBLE
    } else {
        hold.reasons & !vivid_protocol::timed::HOLD_NOT_VISIBLE
    };
    hold.held = hold.reasons != 0;
    if !store(state, key, hold, true) {
        state
            .surfaces
            .get_mut(&key)
            .unwrap()
            .native_checkpoint
            .recovery = Some(recovery);
        return true;
    }
    let mut hold = state.surfaces[&key].hold.clone().unwrap();
    let active = members(state, key);
    let admitted = recovery.play.as_ref().is_some_and(|scopes| {
        !scopes.is_empty()
            && scopes.len() == active.len()
            && active.iter().all(|key| {
                scopes
                    .iter()
                    .any(|scope| scope.key == *key && scope.current(state))
            })
    });
    let observed = clock_track(state, key).and_then(|clock| Observation::capture(state, clock));
    if !hold.held
        && admitted
        && (if hold.playing_intent {
            observed.is_some()
        } else {
            prepared(state, key, &active)
        })
    {
        hold.recovery_required = false;
        if store(state, key, hold, true) {
            state
                .surfaces
                .get_mut(&key)
                .unwrap()
                .native_checkpoint
                .observed = observed;
            return true;
        }
    } else if !hold.held && !admitted {
        let videos = active
            .iter()
            .filter(|key| {
                matches!(
                    state.tracks[key].configuration.kind,
                    KindConfiguration::Video(_)
                )
            })
            .copied()
            .collect::<Vec<_>>();
        recovery
            .requests
            .retain(|id, _| videos.iter().any(|key| key.track == *id));
        for video in videos {
            let track = &state.tracks[&video];
            let Some(writer) = &track.channel_writer else {
                continue;
            };
            let resume = state.sessions[&key.session].resume_generation;
            if recovery.requests.get(&video.track).is_some_and(|r| {
                r.serial == hold.serial
                    && r.generation == track.state.channel_generation
                    && r.resume == resume
                    && Weak::ptr_eq(&r.writer, &Arc::downgrade(writer))
            }) {
                continue;
            }
            let mut payload = vivid_protocol::track::need_keyframe_payload(
                track_address(video, track),
                track.state.media_epoch,
                vivid_protocol::timed::PRESENTATION_RESUMED,
            );
            if let Some(position) = hold.position {
                payload.push((7, signed(position.pts_us)));
            }
            payload.push((8, Value::Unsigned(hold.serial)));
            let body = Envelope::new(0, payload).encode();
            if !body.is_ok_and(|body| {
                writer
                    .write_record(messages::NEED_KEYFRAME, video.track, &body)
                    .is_ok()
            }) {
                state.sessions[&key.session].writer.close();
                break;
            }
            recovery.requests.insert(
                video.track,
                Request {
                    serial: hold.serial,
                    generation: track.state.channel_generation,
                    resume,
                    writer: Arc::downgrade(writer),
                },
            );
        }
    }
    state
        .surfaces
        .get_mut(&key)
        .unwrap()
        .native_checkpoint
        .recovery = Some(recovery);
    true
}
