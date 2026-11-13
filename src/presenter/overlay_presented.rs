//! Bounded original scenes awaiting explicit downstream physical outcomes.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::{
    Weak,
    atomic::{AtomicBool, Ordering},
};

// Match the bounded scene-history scale while preventing one producer from
// retaining every original scene or copy. These also bound receipt metadata.
pub(crate) const MAX_RELAY_FRAMES: usize = 64;
pub(crate) const MAX_RELAY_OWNER_FRAMES: usize = 4;
pub(crate) enum Claim {
    Forwarded,
    Replay,
    Discard,
}
struct Scene {
    value: PresentedScene,
    bytes: u64,
    copies: usize,
}
pub(super) struct Deferred {
    alive: Weak<AtomicBool>,
    pending: HashMap<SurfaceIdentity, Scene>,
    forwarded: HashMap<(SurfaceIdentity, u64), Scene>,
    bytes: u64,
    byte_limit: u64,
}
impl Deferred {
    fn remove(&mut self, key: (SurfaceIdentity, u64)) -> Option<Scene> {
        let scene = self.forwarded.remove(&key)?;
        self.bytes = self
            .bytes
            .checked_sub(scene.bytes)
            .expect("charged relay scene");
        Some(scene)
    }
}
fn matches(scene: &Scene, track: u64, epoch: u32, revision: u64) -> bool {
    scene.value.submission.track_id == track
        && scene.value.submission.epoch == epoch
        && scene.value.revision == revision
}
impl Overlays {
    pub(crate) fn install_presentation_relay(
        &mut self,
        alive: Weak<AtomicBool>,
        byte_limit: u64,
    ) -> bool {
        if self.presentation.is_some() || self.local_native() {
            return false;
        }
        self.presentation = Some(Deferred {
            alive,
            pending: HashMap::new(),
            forwarded: HashMap::new(),
            bytes: 0,
            byte_limit,
        });
        true
    }
    pub(crate) fn presentation_submission(
        &self,
        window: SurfaceIdentity,
        track: u64,
        epoch: u32,
        revision: u64,
    ) -> Option<Submission> {
        let relay = self.presentation.as_ref()?;
        if !relay.alive.upgrade()?.load(Ordering::Acquire) {
            return None;
        }
        relay
            .forwarded
            .get(&(window, revision))
            .or_else(|| relay.pending.get(&window))
            .filter(|scene| matches(scene, track, epoch, revision))
            .map(|scene| scene.value.submission)
            .or_else(|| {
                self.displayed
                    .get(&window)
                    .filter(|scene| {
                        scene.submission.track_id == track
                            && scene.submission.epoch == epoch
                            && scene.revision == revision
                    })
                    .map(|scene| scene.submission)
            })
    }
    pub(crate) fn claim_presentation(
        &mut self,
        window: SurfaceIdentity,
        submission: Submission,
        credit: bool,
    ) -> Claim {
        if self
            .displayed
            .get(&window)
            .is_some_and(|scene| scene.revision > submission.revision)
        {
            return Claim::Discard;
        }
        if self
            .displayed
            .get(&window)
            .is_some_and(|scene| scene.submission == submission)
        {
            // An older replay must not overtake a newer already-forwarded scene.
            return if self.presentation.as_ref().is_some_and(|relay| {
                relay
                    .forwarded
                    .keys()
                    .any(|(id, revision)| *id == window && *revision > submission.revision)
            }) {
                Claim::Discard
            } else {
                Claim::Replay
            };
        }
        let relay = self
            .presentation
            .as_mut()
            .expect("installed presentation relay");
        let owner_id = window.context().session().session_id();
        let count: usize = relay.forwarded.values().map(|scene| scene.copies).sum();
        let owner_count: usize = relay
            .forwarded
            .iter()
            .filter(|((id, _), _)| id.context().session().session_id() == owner_id)
            .map(|(_, scene)| scene.copies)
            .sum();
        if let Some(scene) = relay.forwarded.get_mut(&(window, submission.revision)) {
            if !credit || count >= MAX_RELAY_FRAMES || owner_count >= MAX_RELAY_OWNER_FRAMES {
                return Claim::Discard;
            }
            scene.copies += 1;
            return Claim::Forwarded;
        }
        let Some(scene) = relay.pending.get(&window) else {
            return Claim::Discard;
        };
        if scene.value.submission != submission {
            return Claim::Discard;
        }
        let bytes = relay
            .bytes
            .checked_add(scene.bytes)
            .filter(|bytes| *bytes <= relay.byte_limit);
        let scene = relay.pending.remove(&window).unwrap();
        if !credit
            || count >= MAX_RELAY_FRAMES
            || owner_count >= MAX_RELAY_OWNER_FRAMES
            || bytes.is_none()
        {
            self.resolve(window, submission, PresentationOutcome::Superseded);
            return Claim::Discard;
        }
        relay.bytes = bytes.unwrap();
        relay
            .forwarded
            .insert((window, submission.revision), Scene { copies: 1, ..scene });
        Claim::Forwarded
    }
    pub(super) fn accept_presentation(
        &mut self,
        window: SurfaceIdentity,
        submission: Submission,
        canvas: Canvas,
        bytes: u64,
    ) -> bool {
        let relay = self
            .presentation
            .as_mut()
            .expect("installed presentation relay");
        if relay
            .alive
            .upgrade()
            .is_none_or(|alive| !alive.load(Ordering::Acquire))
        {
            return false;
        }
        let previous = relay.pending.insert(
            window,
            Scene {
                value: PresentedScene {
                    window,
                    revision: submission.revision,
                    canvas,
                    submission,
                },
                bytes,
                copies: 0,
            },
        );
        // A scene already handed to a physical host keeps its own outcome.
        if let Some(previous) = previous {
            self.resolve(
                window,
                previous.value.submission,
                PresentationOutcome::Superseded,
            );
        }
        true
    }
    pub(crate) fn complete_presentation(
        &mut self,
        window: SurfaceIdentity,
        submission: Submission,
        mut outcome: PresentationOutcome,
    ) -> bool {
        let Some(relay) = self.presentation.as_mut() else {
            return false;
        };
        let Some(scene) = relay.forwarded.get_mut(&(window, submission.revision)) else {
            return false;
        };
        if scene.value.submission != submission
            || self
                .windows
                .get(window)
                .is_none_or(|current| current.generation != submission.address.generation)
        {
            return false;
        }
        if outcome == PresentationOutcome::Superseded && scene.copies > 1 {
            scene.copies -= 1;
            return true;
        }
        let scene = relay.remove((window, submission.revision)).unwrap();
        if outcome == PresentationOutcome::Presented {
            if self
                .windows
                .publish_scene(window, submission.address.generation, submission.revision)
                .is_ok()
            {
                self.semantics.remove(&window);
                self.editor_carets.remove(&window);
                self.displayed.insert(window, scene.value);
            } else {
                // A late older physical outcome cannot move display identity backward.
                outcome = PresentationOutcome::Superseded;
            }
        }
        self.resolve(window, submission, outcome);
        self.drain_events(owner(window.context().session().session_id()));
        true
    }
    pub(crate) fn abandon_presentation(
        &mut self,
        window: SurfaceIdentity,
        submission: Submission,
    ) -> bool {
        let Some(relay) = self.presentation.as_mut() else {
            return false;
        };
        let Some(scene) = relay.forwarded.get_mut(&(window, submission.revision)) else {
            return false;
        };
        if scene.value.submission != submission {
            return false;
        }
        if scene.copies > 1 {
            scene.copies -= 1;
            return true;
        }
        let mut scene = relay.remove((window, submission.revision)).unwrap();
        if self.accepted.get(&window) == Some(&submission.revision)
            && self
                .windows
                .get(window)
                .is_some_and(|current| current.generation == submission.address.generation)
        {
            scene.copies = 0;
            relay.pending.insert(window, scene);
        } else {
            self.resolve(window, submission, PresentationOutcome::Superseded);
        }
        true
    }
    pub(crate) fn retire_presentations(&mut self) {
        let mut scenes = Vec::new();
        if let Some(relay) = &mut self.presentation {
            scenes.extend(std::mem::take(&mut relay.pending).into_values());
            scenes.extend(std::mem::take(&mut relay.forwarded).into_values());
            relay.bytes = 0;
        }
        for scene in scenes {
            self.resolve(
                scene.value.window,
                scene.value.submission,
                PresentationOutcome::Superseded,
            );
        }
    }
    pub(super) fn forget_presentation_track(&mut self, window: SurfaceIdentity, track: u64) {
        let mut scenes = Vec::new();
        if let Some(relay) = &mut self.presentation {
            if relay
                .pending
                .get(&window)
                .is_some_and(|scene| scene.value.submission.track_id == track)
            {
                scenes.push(relay.pending.remove(&window).unwrap());
            }
            let remove: Vec<_> = relay
                .forwarded
                .iter()
                .filter(|((id, _), scene)| {
                    *id == window && scene.value.submission.track_id == track
                })
                .map(|(key, _)| *key)
                .collect();
            for key in remove {
                scenes.push(relay.remove(key).unwrap());
            }
        }
        for scene in scenes {
            self.resolve(
                window,
                scene.value.submission,
                PresentationOutcome::Superseded,
            );
        }
    }
    pub(super) fn forget_presentations(&mut self, session: u64) {
        if let Some(relay) = &mut self.presentation {
            relay
                .pending
                .retain(|id, _| id.context().session().session_id() != session);
            let remove: Vec<_> = relay
                .forwarded
                .keys()
                .filter(|(id, _)| id.context().session().session_id() == session)
                .copied()
                .collect();
            for key in remove {
                relay.remove(key);
            }
        }
    }
    pub(super) fn prune_presentations(&mut self) {
        let mut scenes = Vec::new();
        if let Some(relay) = &mut self.presentation {
            let remove: Vec<_> = relay
                .pending
                .keys()
                .filter(|id| self.windows.get(**id).is_none())
                .copied()
                .collect();
            for id in remove {
                scenes.push(relay.pending.remove(&id).unwrap());
            }
            let remove: Vec<_> = relay
                .forwarded
                .keys()
                .filter(|(id, _)| self.windows.get(*id).is_none())
                .copied()
                .collect();
            for key in remove {
                scenes.push(relay.remove(key).unwrap());
            }
        }
        for scene in scenes {
            self.resolve(
                scene.value.window,
                scene.value.submission,
                PresentationOutcome::Superseded,
            );
        }
    }
}
