//! Native visibility holds and durable lease reconciliation.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::Weak;
#[path = "native_hold/recovery.rs"]
mod recovery;
pub(super) use recovery::{arm as arm_play, validate_play};

// One checkpoint per admitted surface; weak identities retain no media resources.
#[derive(Default)]
pub(super) struct Checkpoint {
    observed: Option<Observation>,
    blocked: bool,
    recovery: Option<recovery::Recovery>,
}

#[derive(Clone)]
struct Observation {
    key: TrackKey,
    generation: ChannelGeneration,
    epoch: u32,
    reset: u64,
    play: u64,
    surface: SurfaceGeneration,
    resume: u64,
    incarnation: Weak<()>,
    position: crate::HeldPosition,
}

fn clock_track(state: &State, key: SurfaceKey) -> Option<TrackKey> {
    state
        .surfaces
        .get(&key)?
        .active_slots
        .values()
        .filter_map(|id| {
            let key = TrackKey {
                surface: key,
                track: *id,
            };
            let track = state.tracks.get(&key)?;
            (track.configuration.mode == TrackMode::Timed
                && !track.state.lost
                && identity(track).is_some())
            .then_some((key, track))
        })
        // Active audio owns the clock. After its EOS, continuing video owns
        // software time; drained audio is only a fallback when no video remains.
        .max_by_key(|(key, track)| {
            (
                if matches!(track.configuration.kind, KindConfiguration::Audio(_)) {
                    if track.state.milestones & MILESTONE_BUFFERED_ENDED == 0 {
                        2
                    } else {
                        0
                    }
                } else {
                    1
                },
                std::cmp::Reverse(key.track),
            )
        })
        .map(|(key, _)| key)
}

// Suspension persists metadata after transport loss, without trying to enqueue
// on that dead writer. Connected transitions must admit an actionable record.
fn store(state: &mut State, key: SurfaceKey, mut hold: crate::PlaybackHold, notify: bool) -> bool {
    let previous = state
        .surfaces
        .get(&key)
        .and_then(|surface| surface.hold.as_ref());
    hold.serial = previous.map_or(1, |hold| hold.serial);
    if previous == Some(&hold) {
        return !state.surfaces[&key].native_checkpoint.blocked;
    }
    if let Some(previous) = previous {
        let Some(serial) = previous.serial.checked_add(1) else {
            state
                .surfaces
                .get_mut(&key)
                .unwrap()
                .native_checkpoint
                .blocked = true;
            if let Some(session) = state.sessions.get(&key.session) {
                session.writer.close();
            }
            return false;
        };
        hold.serial = serial;
    }
    let notified = if notify {
        let Some(session) = state.sessions.get(&key.session) else {
            return false;
        };
        let body = hold
            .payload()
            .and_then(|payload| Envelope::new(0, payload).encode());
        let accepted = body.is_ok_and(|body| {
            session
                .writer
                .write_record(messages::PLAYBACK_HOLD, key.surface, &body)
                .is_ok()
        });
        if !accepted {
            session.writer.close();
        }
        accepted
    } else {
        true
    };
    // A lost control notification still leaves reconciliation and local pause
    // state intact. The failed connection cannot retain presentation authority.
    let surface = state
        .surfaces
        .get_mut(&key)
        .expect("admitted native surface");
    surface.hold = Some(hold);
    surface.native_checkpoint.blocked = false;
    state.wake_waits();
    notified
}

pub(super) fn blocked(state: &State, key: SurfaceKey) -> bool {
    state
        .surfaces
        .get(&key)
        .is_some_and(|s| s.native_checkpoint.blocked)
}

fn retire(state: &mut State, keys: Vec<SurfaceKey>, loss: recovery::Loss, notify: bool) -> bool {
    let mut refining = Vec::new();
    let mut affected = Vec::new();
    let mut success = true;
    for key in keys {
        if !state.overlays.local_native()
            || !state
                .sessions
                .get(&key.session)
                .is_some_and(|s| s.accepted_profiles.contains(registry::TIMED_MEDIA_SYNC))
        {
            continue;
        }
        let previous = state.surfaces[&key].hold.clone();
        if clock_track(state, key).is_none() && previous.is_none() {
            continue;
        }
        let recovering = previous.as_ref().is_some_and(|hold| hold.recovery_required);
        let recovery = recovery::capture(state, key, loss);
        let observed = clock_track(state, key)
            .and_then(|clock| Observation::capture(state, clock))
            .or_else(|| {
                state.surfaces[&key]
                    .native_checkpoint
                    .observed
                    .as_ref()
                    .filter(|last| last.current(state))
                    .cloned()
                    .map(|mut last| {
                        last.position.estimated = true;
                        last
                    })
            });
        let position = if recovering {
            previous.as_ref().and_then(|hold| hold.position)
        } else {
            observed.map(|observation| observation.position)
        };
        let playing_intent = if recovering {
            previous.as_ref().is_some_and(|hold| hold.playing_intent)
        } else {
            state.tracks.iter().any(|(track, entry)| {
                track.surface == key
                    && entry.configuration.mode == TrackMode::Timed
                    && entry.playing
            })
        };
        let pane = state.sessions[&key.session].pane;
        let reason = match loss {
            recovery::Loss::Device => vivid_protocol::timed::HOLD_DOWNSTREAM,
            _ => vivid_protocol::timed::HOLD_DETACHED,
        };
        let reasons = (previous.as_ref().map_or(0, |hold| hold.reasons)
            & !vivid_protocol::timed::HOLD_NOT_VISIBLE)
            | reason
            | if state.native_pane_hidden(pane) {
                vivid_protocol::timed::HOLD_NOT_VISIBLE
            } else {
                0
            };
        let stored = store(
            state,
            key,
            crate::PlaybackHold {
                context_id: key.context,
                surface_id: key.surface,
                serial: 1,
                held: true,
                reasons,
                playing_intent,
                recovery_required: true,
                position,
            },
            notify,
        );
        success &= stored;
        affected.push(key);
        if !blocked(state, key) {
            state
                .surfaces
                .get_mut(&key)
                .unwrap()
                .native_checkpoint
                .recovery = Some(recovery);
            if !recovering {
                refining.push(key);
            }
        }
    }
    if affected.is_empty() {
        return success;
    }
    #[cfg(feature = "vector-audio")]
    native_audio::tick(state);
    #[cfg(feature = "vector-video")]
    native_video::tick(state);
    for key in refining {
        let position = clock_track(state, key).and_then(|clock| Observation::capture(state, clock));
        if let Some(observed) = position {
            let mut hold = state.surfaces[&key]
                .hold
                .clone()
                .expect("retired native hold");
            hold.position = Some(observed.position);
            success &= store(state, key, hold, notify);
        }
        state
            .surfaces
            .get_mut(&key)
            .unwrap()
            .native_checkpoint
            .observed = None;
    }
    // Saved producer intent lives in the hold. Retired PLAY operations cannot
    // restart merely because the frontend later releases the hold reasons.
    let now = Instant::now();
    for (key, track) in &mut state.tracks {
        if affected.contains(&key.surface)
            && track.configuration.mode == TrackMode::Timed
            && identity(track).is_some()
        {
            track.playing = false;
            #[cfg(feature = "vector-video")]
            if let Some(playback) = &mut track.video_playback {
                playback.pause(now);
            }
        }
    }
    #[cfg(not(feature = "vector-video"))]
    let _ = now;
    state.wake_waits();
    success
}

pub(super) fn suspend(state: &mut State, session_id: u64) -> bool {
    let keys = state
        .surfaces
        .keys()
        .filter(|key| key.session == session_id)
        .copied()
        .collect();
    retire(state, keys, recovery::Loss::Lease, false)
}

pub(super) fn host_detached(state: &mut State) {
    let keys = state.surfaces.keys().copied().collect();
    retire(state, keys, recovery::Loss::Host, true);
}

pub(super) fn device_failed(state: &mut State, key: TrackKey) {
    if state.tracks.get(&key).is_some_and(|t| {
        !t.state.lost && identity(t).is_some() && t.configuration.mode == TrackMode::Timed
    }) {
        retire(state, vec![key.surface], recovery::Loss::Device, true);
    }
}

pub(super) fn pane_recovery_pending(state: &State, pane: PaneId) -> bool {
    state.surfaces.iter().any(|(key, surface)| {
        state
            .sessions
            .get(&key.session)
            .is_some_and(|session| session.pane == pane && !session.closed)
            && surface.native_checkpoint.recovery.is_some()
            && surface.hold.as_ref().is_some_and(|hold| {
                hold.recovery_required && hold.reasons & vivid_protocol::timed::HOLD_DOWNSTREAM != 0
            })
    })
}

pub(super) fn recover_pane(state: &mut State, pane: PaneId) -> bool {
    let keys = state
        .surfaces
        .keys()
        .filter(|key| {
            state
                .sessions
                .get(&key.session)
                .is_some_and(|s| s.pane == pane && !s.closed)
        })
        .copied()
        .collect::<Vec<_>>();
    let mut changed = false;
    for key in keys {
        changed |= recovery::recover_device(state, key);
    }
    if changed {
        tick(state);
        state.wake_waits();
    }
    changed
}

pub(super) fn intent(state: &mut State, key: SurfaceKey, playing: bool, reposition: bool) -> bool {
    if !state.overlays.local_native() {
        return true;
    }
    let Some(mut hold) = state
        .surfaces
        .get(&key)
        .and_then(|surface| surface.hold.clone())
        .filter(|hold| hold.recovery_required)
    else {
        return true;
    };
    hold.playing_intent = playing;
    if reposition {
        recovery::reposition(state, key);
        hold.position = None;
    }
    store(state, key, hold, true)
}

fn identity(track: &TrackEntry) -> Option<(Weak<()>, u64)> {
    #[cfg(feature = "vector-audio")]
    if let Some(identity) = native_audio::hold_identity(track) {
        return Some(identity);
    }
    #[cfg(feature = "vector-video")]
    if let Some(identity) = native_video::hold_identity(track) {
        return Some(identity);
    }
    None
}

impl Observation {
    fn current(&self, state: &State) -> bool {
        state
            .sessions
            .get(&self.key.surface.session)
            .is_some_and(|session| !session.closed && session.resume_generation == self.resume)
            && state
                .surfaces
                .get(&self.key.surface)
                .is_some_and(|surface| {
                    surface.state.generation == self.surface
                        && surface
                            .active_slots
                            .values()
                            .any(|id| *id == self.key.track)
                })
            && state.tracks.get(&self.key).is_some_and(|track| {
                track.state.channel_generation == self.generation
                    && track.state.media_epoch == self.epoch
                    && track.decoder_reset_serial == self.reset
                    && identity(track).is_some_and(|(incarnation, play)| {
                        play == self.play && Weak::ptr_eq(&incarnation, &self.incarnation)
                    })
            })
    }

    fn capture(state: &State, key: TrackKey) -> Option<Self> {
        let track = state.tracks.get(&key)?;
        if track.state.lost || track.state.milestones & MILESTONE_CLOCK_STARTED == 0 {
            return None;
        }
        #[cfg(feature = "vector-audio")]
        let position = native_audio::hold_position(state, key);
        #[cfg(not(feature = "vector-audio"))]
        let position = None;
        #[cfg(feature = "vector-video")]
        let position = position.or_else(|| native_video::hold_position(state, key));
        let position: super::super::BridgePositionSnapshot = position?;
        if position.decoder_reset_serial != track.decoder_reset_serial
            || !matches!(position.state, 2..=4)
        {
            return None;
        }
        let (incarnation, play) = identity(track)?;
        Some(Self {
            key,
            generation: track.state.channel_generation,
            epoch: track.state.media_epoch,
            reset: track.decoder_reset_serial,
            play,
            surface: state.surfaces.get(&key.surface)?.state.generation,
            resume: state.sessions.get(&key.surface.session)?.resume_generation,
            incarnation,
            position: crate::HeldPosition {
                track_id: key.track,
                channel_generation: track.state.channel_generation.get(),
                epoch: track.state.media_epoch,
                pts_us: position.clock_pts_us?,
                estimated: !matches!(position.state, 3 | 4),
            },
        })
    }
}

pub(super) fn tick(state: &mut State) {
    if !state.overlays.local_native()
        || !state.sessions.values().any(|session| {
            !session.closed
                && session
                    .accepted_profiles
                    .contains(registry::TIMED_MEDIA_SYNC)
        })
    {
        return;
    }
    let keys = state.surfaces.keys().copied().collect::<Vec<_>>();
    for key in keys {
        let Some(session) = state.sessions.get(&key.session).filter(|session| {
            !session.closed
                && session
                    .accepted_profiles
                    .contains(registry::TIMED_MEDIA_SYNC)
        }) else {
            continue;
        };
        let held = state.native_pane_hidden(session.pane);
        if recovery::tick(state, key, held) {
            continue;
        }
        if let Some(mut hold) = state.surfaces[&key]
            .hold
            .clone()
            .filter(|hold| hold.recovery_required)
        {
            // Reconciliation carries the original qualified frozen position;
            // replacement channels must not relabel it as their own clock.
            hold.reasons = if held {
                hold.reasons | vivid_protocol::timed::HOLD_NOT_VISIBLE
            } else {
                hold.reasons & !vivid_protocol::timed::HOLD_NOT_VISIBLE
            };
            hold.held = hold.reasons != 0;
            store(state, key, hold, true);
            continue;
        }
        let surface = &state.surfaces[&key];
        let Some(clock) = clock_track(state, key) else {
            continue;
        };
        let fresh = Observation::capture(state, clock);
        let valid = surface
            .native_checkpoint
            .observed
            .as_ref()
            .is_some_and(|last| last.key == clock && last.current(state));
        let playing_intent = state.tracks.iter().any(|(track, entry)| {
            track.surface == key && entry.configuration.mode == TrackMode::Timed && entry.playing
        });
        let surface = state.surfaces.get_mut(&key).expect("listed native surface");
        if !valid {
            surface.native_checkpoint.observed = None;
        }
        if let Some(fresh) = fresh {
            surface.native_checkpoint.observed = Some(fresh);
        } else if let Some(last) = &mut surface.native_checkpoint.observed {
            // A vanished physical observation never becomes an exact frozen clock.
            last.position.estimated = true;
        }
        let previous = surface.hold.as_ref();
        if previous.is_none() && !held {
            continue;
        }
        let position = if held {
            surface
                .native_checkpoint
                .observed
                .as_ref()
                .map(|last| last.position)
        } else if valid {
            // A released hold preserves its frozen checkpoint, not a stream of
            // advancing observations. New PLAY/channel/epoch identities clear it.
            previous.and_then(|hold| hold.position)
        } else {
            None
        };
        let hold = crate::PlaybackHold {
            context_id: key.context,
            surface_id: key.surface,
            serial: previous.map_or(1, |hold| hold.serial),
            held,
            reasons: if held {
                vivid_protocol::timed::HOLD_NOT_VISIBLE
            } else {
                0
            },
            playing_intent,
            recovery_required: false,
            position,
        };
        store(state, key, hold, true);
    }
}
