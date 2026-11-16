//! Bounded original scenes awaiting explicit downstream physical outcomes.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::{
    Arc, Weak,
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
    Blocked,
}
struct Scene {
    value: PresentedScene,
    origin: Arc<()>,
    bytes: u64,
    copies: usize,
}
pub(super) struct Deferred {
    alive: Weak<AtomicBool>,
    pending: HashMap<SurfaceIdentity, Scene>,
    forwarded: HashMap<(SurfaceIdentity, u64), Scene>,
    // One identity per currently displayed window, never a scene history.
    displayed_origins: HashMap<SurfaceIdentity, (Submission, Arc<()>)>,
    bytes: u64,
    byte_limit: u64,
    #[cfg(any(test, feature = "testing"))]
    blocked_captures: u64,
}
impl Deferred {
    fn blocked(&mut self) -> Claim {
        #[cfg(any(test, feature = "testing"))]
        {
            self.blocked_captures = self.blocked_captures.saturating_add(1);
        }
        Claim::Blocked
    }
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
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn presentation_blocked_captures(&self) -> u64 {
        self.presentation
            .as_ref()
            .map_or(0, |relay| relay.blocked_captures)
    }
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
            displayed_origins: HashMap::new(),
            bytes: 0,
            byte_limit,
            #[cfg(any(test, feature = "testing"))]
            blocked_captures: 0,
        });
        true
    }
    pub(crate) fn presentation_origin(
        &self,
        window: SurfaceIdentity,
        track: u64,
        epoch: u32,
        revision: u64,
    ) -> Option<(Submission, Weak<()>)> {
        let relay = self.presentation.as_ref()?;
        if !relay.alive.upgrade()?.load(Ordering::Acquire) {
            return None;
        }
        relay
            .forwarded
            .get(&(window, revision))
            .or_else(|| relay.pending.get(&window))
            .filter(|scene| matches(scene, track, epoch, revision))
            .map(|scene| (scene.value.submission, Arc::downgrade(&scene.origin)))
            .or_else(|| {
                self.displayed
                    .get(&window)
                    .filter(|scene| {
                        scene.submission.track_id == track
                            && scene.submission.epoch == epoch
                            && scene.revision == revision
                    })
                    .and_then(|scene| {
                        relay
                            .displayed_origins
                            .get(&window)
                            .filter(|(submission, _)| *submission == scene.submission)
                    })
                    .map(|(submission, origin)| (*submission, Arc::downgrade(origin)))
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
                return relay.blocked();
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
        // Permanent oversize cannot be relieved by downstream progress. Temporary
        // receipt/byte pressure leaves the current pending scene retryable.
        if scene.bytes > relay.byte_limit {
            relay.pending.remove(&window);
            self.resolve(window, submission, PresentationOutcome::Superseded);
            return Claim::Discard;
        }
        if !credit
            || count >= MAX_RELAY_FRAMES
            || owner_count >= MAX_RELAY_OWNER_FRAMES
            || bytes.is_none()
        {
            return relay.blocked();
        }
        let scene = relay.pending.remove(&window).unwrap();
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
                origin: Arc::new(()),
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
                relay
                    .displayed_origins
                    .insert(window, (submission, scene.origin));
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
            relay.displayed_origins.clear();
        }
        for scene in scenes {
            self.resolve(
                scene.value.window,
                scene.value.submission,
                PresentationOutcome::Superseded,
            );
        }
    }
    pub(crate) fn forget_presentation_track(&mut self, window: SurfaceIdentity, track: u64) {
        let mut scenes = Vec::new();
        if let Some(relay) = &mut self.presentation {
            if relay
                .displayed_origins
                .get(&window)
                .is_some_and(|(submission, _)| submission.track_id == track)
            {
                relay.displayed_origins.remove(&window);
            }
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
    pub(crate) fn forget_presentations(&mut self, session: u64) {
        if let Some(relay) = &mut self.presentation {
            relay
                .displayed_origins
                .retain(|id, _| id.context().session().session_id() != session);
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
            relay.displayed_origins.retain(|id, (submission, _)| {
                self.windows
                    .get(*id)
                    .is_some_and(|window| window.generation == submission.address.generation)
                    && self
                        .displayed
                        .get(id)
                        .is_some_and(|scene| scene.submission == *submission)
            });
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
