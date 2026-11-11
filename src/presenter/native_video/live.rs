//! A bounded latest live picture gated by its admitted surface audio clock.

// Rust guideline compliant 2026-10-07
use super::*;

pub(crate) struct Pending {
    scope: Admission,
    frame: Frame,
}

fn clock(state: &State, key: TrackKey) -> Option<native_audio::Clock> {
    let t = state.tracks.get(&key)?;
    if t.configuration.mode != TrackMode::Live
        || !state
            .surfaces
            .get(&key.surface)?
            .active_slots
            .values()
            .any(|id| *id == key.track)
    {
        return None;
    }
    native_audio::live_clock(state, key.surface).filter(|c| c.started && !c.ended)
}

fn due(clock: Option<native_audio::Clock>, pts: i64) -> bool {
    clock.is_none_or(|c| c.started && c.playing && pts <= c.pts)
}

pub(super) fn publish(state: &mut State, scope: &Admission, frame: Frame) -> bool {
    let ready = due(clock(state, scope.key), frame.pts);
    let track = state
        .tracks
        .get_mut(&scope.key)
        .expect("admitted live video");
    if !owns(track, &frame.incarnation) {
        return false;
    }
    if ready {
        track.native_video = Some(frame);
        track.live_video_scope = Some(scope.clone());
    } else {
        track.live_video_pending = Some(Pending {
            scope: scope.clone(),
            frame,
        });
    }
    ready
}

pub(super) fn tick(state: &mut State) {
    if !state.video_host {
        return;
    }
    let keys = state
        .tracks
        .iter()
        .filter_map(|(k, t)| {
            (t.video_reservation.is_some() && t.configuration.mode == TrackMode::Live).then_some(*k)
        })
        .collect::<Vec<_>>();
    let mut changed = false;
    let mut observed = false;
    for key in keys {
        let master = clock(state, key);
        let valid = state.tracks[&key]
            .live_video_pending
            .as_ref()
            .is_none_or(|p| {
                p.scope.retained(state) && owns(&state.tracks[&key], &p.frame.incarnation)
            });
        let current = state.tracks[&key]
            .live_video_scope
            .as_ref()
            .is_none_or(|s| s.retained(state))
            && state.tracks[&key].native_video.as_ref().is_none_or(|f| {
                state
                    .surfaces
                    .get(&key.surface)
                    .is_some_and(|s| s.state.generation == f.surface)
                    && f.epoch == state.tracks[&key].state.media_epoch
                    && f.reset == state.tracks[&key].decoder_reset_serial
                    && owns(&state.tracks[&key], &f.incarnation)
            });
        let track = state.tracks.get_mut(&key).expect("listed live video");
        let milestones = track.state.milestones;
        if !valid {
            track.live_video_pending = None;
        }
        if !current {
            track.native_video = None;
            track.live_video_scope = None;
            changed = true;
        }
        if track.state.lost {
            track.live_video_pending = None;
            continue;
        }
        // A primed but undisplayed future image must not bypass a newly
        // activated audio master. An already displayed poster may stay visible.
        let held = track.native_video.as_ref().is_some_and(|f| {
            !due(master, f.pts)
                && !track.native_visual_presented.is_some_and(|(c, e, id)| {
                    c == track.state.channel_generation && e == f.epoch && id >= f.id
                })
        });
        if held {
            let frame = track.native_video.take().expect("held live preview");
            // Preserve publication's original admission even if a clean EOS
            // transport closed before audio activation. Never recapture a new
            // writer, generation or reservation for already decoded pixels.
            if let Some(scope) = track.live_video_scope.take()
                && track
                    .live_video_pending
                    .as_ref()
                    .is_none_or(|p| p.frame.pts < frame.pts)
            {
                track.live_video_pending = Some(Pending { scope, frame });
            }
            changed = true;
        }
        if track
            .live_video_pending
            .as_ref()
            .is_some_and(|p| due(master, p.frame.pts))
        {
            let pending = track.live_video_pending.take().expect("due live video");
            track.native_video = Some(pending.frame);
            track.live_video_scope = Some(pending.scope);
            changed = true;
        }
        if master.is_some_and(|c| c.started) {
            track.state.milestones |= MILESTONE_CLOCK_STARTED;
        }
        if ended(track) {
            track.state.milestones |= MILESTONE_BUFFERED_ENDED;
        }
        observed |= milestones != track.state.milestones;
        if let Some(position) = &mut track.outer_position {
            let prior = *position;
            position.clock_pts_us = master.map(|c| c.pts);
            if let Some(master) = master {
                position.state = if master.started { 2 } else { 1 };
            }
            observed |= *position != prior;
        }
    }
    if changed {
        advance_projection(state);
    }
    if changed || observed {
        state.wake_waits();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_live_picture_waits_for_real_audio_and_its_own_pts() {
        let mut c = native_audio::Clock {
            pts: 40_000,
            started: false,
            playing: true,
            paused: false,
            ended: false,
        };
        assert!(!due(Some(c), 0));
        c.started = true;
        assert!(due(Some(c), 40_000));
        assert!(!due(Some(c), 40_001));
        c.playing = false;
        assert!(!due(Some(c), 0));
        assert!(due(None, i64::MAX));
    }
}
