//! Deferred source scene publication for an explicitly bound terminating relay.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::{
    Weak,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct Deferred {
    alive: Weak<AtomicBool>,
    pending: HashMap<SurfaceIdentity, PresentedScene>,
}
impl Overlays {
    pub(crate) fn install_presentation_relay(&mut self, alive: Weak<AtomicBool>) -> bool {
        if self.presentation.is_some() || self.local_native() {
            return false;
        }
        self.presentation = Some(Deferred {
            alive,
            pending: HashMap::new(),
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
        let scene = relay.pending.get(&window)?;
        (scene.submission.track_id == track
            && scene.submission.epoch == epoch
            && scene.revision == revision)
            .then_some(scene.submission)
    }
    pub(super) fn accept_presentation(
        &mut self,
        window: SurfaceIdentity,
        submission: Submission,
        canvas: Canvas,
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
            PresentedScene {
                window,
                revision: submission.revision,
                canvas,
                submission,
            },
        );
        if let Some(previous) = previous {
            self.resolve(window, previous.submission, PresentationOutcome::Superseded);
        }
        true
    }
    pub(crate) fn complete_presentation(
        &mut self,
        window: SurfaceIdentity,
        submission: Submission,
        outcome: PresentationOutcome,
    ) -> bool {
        if self
            .presentation
            .as_ref()
            .and_then(|relay| relay.pending.get(&window))
            .is_none_or(|pending| pending.submission != submission)
            || self
                .windows
                .get(window)
                .is_none_or(|current| current.generation != submission.address.generation)
        {
            return false;
        }
        let pending = self
            .presentation
            .as_mut()
            .unwrap()
            .pending
            .remove(&window)
            .unwrap();
        if outcome == PresentationOutcome::Presented {
            if self
                .windows
                .publish_scene(window, submission.address.generation, submission.revision)
                .is_err()
            {
                return false;
            }
            self.semantics.remove(&window);
            self.editor_carets.remove(&window);
            self.displayed.insert(window, pending);
        }
        self.resolve(window, submission, outcome);
        self.drain_events(owner(window.context().session().session_id()));
        true
    }
    pub(crate) fn retire_presentations(&mut self) {
        let pending = self
            .presentation
            .as_mut()
            .map(|relay| std::mem::take(&mut relay.pending))
            .unwrap_or_default();
        for (window, scene) in pending {
            self.resolve(window, scene.submission, PresentationOutcome::Superseded);
        }
    }
    pub(super) fn forget_presentation_track(&mut self, window: SurfaceIdentity, track: u64) {
        let remove = self
            .presentation
            .as_ref()
            .and_then(|relay| relay.pending.get(&window))
            .is_some_and(|scene| scene.submission.track_id == track);
        if remove {
            let scene = self
                .presentation
                .as_mut()
                .unwrap()
                .pending
                .remove(&window)
                .unwrap();
            self.resolve(window, scene.submission, PresentationOutcome::Superseded);
        }
    }
    pub(super) fn forget_presentations(&mut self, session: u64) {
        if let Some(relay) = &mut self.presentation {
            relay
                .pending
                .retain(|id, _| id.context().session().session_id() != session);
        }
    }
    pub(super) fn prune_presentations(&mut self) {
        let removed: Vec<_> = self
            .presentation
            .as_ref()
            .map(|relay| {
                relay
                    .pending
                    .keys()
                    .filter(|id| self.windows.get(**id).is_none())
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        for id in removed {
            let scene = self
                .presentation
                .as_mut()
                .unwrap()
                .pending
                .remove(&id)
                .unwrap();
            self.resolve(id, scene.submission, PresentationOutcome::Superseded);
        }
    }
}
