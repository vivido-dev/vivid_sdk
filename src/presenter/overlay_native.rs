//! Deferred local presentation and matching compiled input geometry.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::vector_renderer::CompiledScene;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(crate) struct Native {
    pub pending: HashMap<SurfaceIdentity, Pending>,
    // Only live native frame reservations remain here. They retain metadata,
    // while their already-budgeted frame objects own the encoded scene bytes.
    pub prepared: HashMap<(SurfaceIdentity, u64), Prepared>,
    pub displayed: HashMap<SurfaceIdentity, Displayed>,
    pub committed: HashMap<SurfaceIdentity, Submission>,
    pub compositions: HashMap<u64, Composition>,
    invalidated: std::collections::HashSet<SurfaceIdentity>,
    // At most one newly requested gain per pane. Loss is always immediate.
    focus_gains: HashMap<PaneId, FocusGain>,
}
struct FocusGain {
    window: SurfaceIdentity,
    submission: Submission,
    focus: u64,
    lane: u64,
}
pub(crate) type Stamp = Vec<(SurfaceIdentity, u64, u64)>;
pub(crate) struct Composition {
    pub owners: Vec<u64>,
    pub stamp: Stamp,
}
pub(crate) struct Pending {
    pub submission: Submission,
}
pub(crate) struct Prepared {
    pub submission: Submission,
    pub body: Weak<[u8]>,
    pub pin: Weak<()>,
    pub resolved: bool,
}
pub(crate) struct Displayed {
    pub submission: Submission,
    pub placement: u64,
    pub viewport: u64,
    pub compiled: Arc<CompiledScene>,
    // Cached displayed geometry owns its reservation even after the caller drops
    // the acknowledged frame, so a peer repaint needs no fresh byte admission.
    _reservation: Arc<()>,
}
impl Native {
    pub fn remove_owner(&mut self, session: u64) {
        self.focus_gains
            .retain(|_, gain| gain.window.context().session().session_id() != session);
        self.pending
            .retain(|id, _| id.context().session().session_id() != session);
        self.prepared
            .retain(|(id, _), _| id.context().session().session_id() != session);
        self.displayed
            .retain(|id, _| id.context().session().session_id() != session);
        self.committed
            .retain(|id, _| id.context().session().session_id() != session);
        self.compositions
            .retain(|_, frame| !frame.owners.contains(&session));
        self.invalidated
            .retain(|id| id.context().session().session_id() != session);
    }
    fn valid(
        &self,
        windows: &Windows,
        viewports: &HashMap<u64, SessionViewport>,
        id: SurfaceIdentity,
    ) -> bool {
        let Some(display) = self.displayed.get(&id) else {
            return false;
        };
        windows.get(id).is_some_and(|window| {
            window.revision == display.placement
                && window.generation == display.submission.address.generation
                && window.scene_revision == display.submission.revision
        }) && viewports
            .get(&id.context().session().session_id())
            .is_some_and(|viewport| viewport.revision == display.viewport)
    }
}
impl Overlays {
    pub(crate) fn defer_action_focus(&mut self, pane: PaneId, window: SurfaceIdentity) {
        let Some(lane) = self.input_generation(window.context().session().session_id()) else {
            return;
        };
        let Some(focus) = self.windows.focus_generation(pane) else {
            return;
        };
        let Some(native) = &mut self.native else {
            return;
        };
        let Some(display) = native.displayed.get(&window) else {
            return;
        };
        native.focus_gains.insert(
            pane,
            FocusGain {
                window,
                submission: display.submission,
                focus,
                lane,
            },
        );
    }

    pub(crate) fn awaiting_action_focus(&self, window: SurfaceIdentity) -> bool {
        let owner = window.context().session();
        let Some(pane) = self.windows.pane(owner) else {
            return false;
        };
        let Some(gain) = self.native.as_ref().and_then(|n| n.focus_gains.get(&pane)) else {
            return false;
        };
        gain.window == window
            && self.windows.focus_generation(pane) == Some(gain.focus)
            && self.input_generation(owner.session_id()) == Some(gain.lane)
            && self
                .windows
                .get(window)
                .is_some_and(|w| w.generation == gain.submission.address.generation)
    }

    pub(crate) fn publish_action_focus(&mut self, pane: PaneId) {
        let Some(window) = self
            .native
            .as_ref()
            .and_then(|native| native.focus_gains.get(&pane))
            .map(|g| g.window)
        else {
            return;
        };
        let current =
            self.windows.focus(pane) == Some(window) && self.awaiting_action_focus(window);
        if current && !self.native_window_ready(window) {
            return;
        }
        let gain = self
            .native
            .as_mut()
            .expect("native focus gain")
            .focus_gains
            .remove(&pane)
            .expect("pending gain");
        if current {
            let binding = self
                .native
                .as_ref()
                .and_then(|native| native.displayed.get(&window))
                .map(|d| d.submission);
            if binding.is_some_and(|s| {
                s.address == gain.submission.address
                    && s.track_id == gain.submission.track_id
                    && s.channel_generation == gain.submission.channel_generation
                    && s.epoch == gain.submission.epoch
            }) {
                self.native_emit(window, Event::Focus(true));
            } else {
                self.windows.clear_focus(window);
                self.drain_events(window.context().session());
            }
        }
    }

    pub(crate) fn discard_action_focus(&mut self, pane: PaneId) {
        if let Some(gain) = self
            .native
            .as_mut()
            .and_then(|native| native.focus_gains.remove(&pane))
            && self.windows.focus_generation(pane) == Some(gain.focus)
            && self.windows.focus(pane) == Some(gain.window)
        {
            self.windows.clear_focus(gain.window);
            self.drain_events(gain.window.context().session());
        }
    }
    pub(crate) fn retire_action_focus(&mut self, session: u64) {
        if let Some(native) = &mut self.native {
            native
                .focus_gains
                .retain(|_, gain| gain.window.context().session().session_id() != session);
        }
    }

    pub(crate) fn note_native_cancel(&mut self, id: SurfaceIdentity) {
        if let Some(native) = &mut self.native {
            native.invalidated.insert(id);
        }
    }
    pub(crate) fn note_native_press(&mut self, id: SurfaceIdentity) {
        if let Some(native) = &mut self.native {
            native.invalidated.remove(&id);
        }
    }
    pub(crate) fn native_focus(&self, pane: crate::presenter::PaneId) -> Option<SurfaceIdentity> {
        self.windows.focus(pane)
    }
    pub(crate) fn native_focus_generation(&self, pane: crate::presenter::PaneId) -> Option<u64> {
        self.windows.focus_generation(pane)
    }
    pub(crate) fn native_capture(&self, pane: crate::presenter::PaneId) -> Option<SurfaceIdentity> {
        self.windows.captured_window(pane)
    }
    pub(crate) fn native_eligible(&self, id: SurfaceIdentity) -> bool {
        self.windows.input_eligible(id)
    }
    pub(crate) fn native_window_ready(&self, id: SurfaceIdentity) -> bool {
        self.native_owner_ready(id) && self.native_input_valid(id)
    }
    pub(crate) fn native_emit(&mut self, id: SurfaceIdentity, mut event: Event) -> bool {
        let Some(submission) = self
            .native
            .as_ref()
            .and_then(|native| native.displayed.get(&id))
            .map(|display| display.submission)
        else {
            return false;
        };
        self.record_pointer_event(id, &mut event);
        if matches!(
            event,
            Event::Pointer {
                button: Some((_, true)),
                ..
            } | Event::Key {
                down: true,
                repeat: false,
                ..
            }
        ) {
            self.note_native_press(id);
        }
        let event = vivid_protocol::overlay::wire::InputEvent {
            address: submission.address,
            scene_revision: submission.revision,
            event,
        };
        let Ok(payload) = event.payload() else {
            return false;
        };
        self.queue_for(
            id.context().session().session_id(),
            LaneRecord {
                retention: None,
                record_type: messages::OVERLAY_INPUT_EVENT,
                object_id: id.surface_id(),
                payload,
            },
        );
        true
    }
    pub(crate) fn native_cancel(&mut self, id: SurfaceIdentity, generation: u64) {
        // A peer's focus action can leave this owner's Cancel queued in Windows.
        // Observe it before deciding whether a synthetic cancellation is needed.
        self.drain_events(id.context().session());
        if self
            .windows
            .pane(id.context().session())
            .and_then(|pane| self.windows.focus_generation(pane))
            != Some(generation)
        {
            return;
        }
        self.windows.release_pointer(id);
        self.last_gesture.retain(|_, (target, _, _)| *target != id);
        if self
            .native
            .as_mut()
            .is_some_and(|native| native.invalidated.insert(id))
        {
            self.native_emit(id, Event::Cancel);
        }
    }
    pub(crate) fn native_key(&mut self, id: SurfaceIdentity, event: Event, escape: bool) -> bool {
        if self.windows.focus_of(id) != Some(id) || !self.windows.input_eligible(id) {
            return false;
        }
        if matches!(event, Event::Key { down: true, .. }) {
            self.record_gesture(
                self.windows
                    .pane(id.context().session())
                    .expect("bound overlay"),
                Some(id),
            );
        }
        let consumed = self.windows.keyboard(
            self.windows
                .pane(id.context().session())
                .expect("bound overlay"),
            event,
            escape,
        );
        self.drain_events_of(&[id.context().session().session_id()]);
        consumed
    }
    pub(crate) fn native_swallowed_owner(
        &self,
        pane: crate::presenter::PaneId,
        button: u16,
    ) -> Option<SessionIdentity> {
        self.windows.swallowed_button_owner(pane, button)
    }
    pub(crate) fn native_swallowed_release(&mut self, button: u16, owner: SessionIdentity) -> bool {
        if self
            .windows
            .pane(owner)
            .and_then(|pane| self.windows.swallowed_button_owner(pane, button))
            != Some(owner)
        {
            return false;
        }
        self.windows.pointer(
            self.windows.pane(owner).expect("bound overlay"),
            PointerReport {
                position: Point::default(),
                button: Some((button, false)),
                modifiers: 0,
                clicks: 0,
                pressure: None,
            },
            |_, _| None,
        )
    }
    pub(crate) fn native_swallowed_key(&mut self, owner: SessionIdentity) -> bool {
        self.windows
            .pane(owner)
            .and_then(|pane| self.windows.swallowed_escape_owner(pane))
            == Some(owner)
            && self.windows.keyboard(
                self.windows.pane(owner).expect("bound overlay"),
                Event::Key {
                    physical: 0x29,
                    down: false,
                    repeat: false,
                    modifiers: 0,
                },
                true,
            )
    }
    pub(crate) fn native_escape_swallowed(&self, owner: SessionIdentity) -> bool {
        self.windows
            .pane(owner)
            .and_then(|pane| self.windows.swallowed_escape_owner(pane))
            == Some(owner)
    }
    pub(crate) fn native_pointer_start(
        &mut self,
        sessions: &[u64],
        allowed: &[SurfaceIdentity],
        position: Point,
        button: Option<(u16, bool)>,
        modifiers: u32,
    ) -> (bool, Option<(SurfaceIdentity, u64)>) {
        let Some(pane) = self.windows.pane_of_sessions(sessions) else {
            return (false, None);
        };
        if self
            .windows
            .captured_window(pane)
            .is_some_and(|id| !allowed.contains(&id))
        {
            return (false, None);
        }
        for owner in sessions {
            self.last_pointer.insert(*owner, position);
        }
        self.drain_events_of(sessions);
        self.native_press = None;
        let hits = self.native_hits(sessions).unwrap_or_default();
        let clicks = 0;
        let consumed = self.windows.pointer(
            pane,
            PointerReport {
                position,
                button,
                modifiers,
                clicks,
                pressure: None,
            },
            |id, point| {
                allowed
                    .contains(&id)
                    .then(|| hits.get(&id).and_then(|scene| scene.hit(point)))
                    .flatten()
            },
        );
        self.drain_events_of(sessions);
        let pressed = self.native_press.take();
        if let Some((id, _)) = pressed {
            self.record_gesture(
                self.windows
                    .pane(id.context().session())
                    .expect("bound overlay"),
                Some(id),
            );
        }
        (consumed, pressed)
    }
    pub(crate) fn native_pointer_held(
        &mut self,
        sessions: &[u64],
        allowed: &[SurfaceIdentity],
        id: SurfaceIdentity,
        region: u64,
        report: PointerReport,
    ) -> bool {
        let PointerReport {
            position,
            button,
            modifiers,
            ..
        } = report;
        let Some(pane) = self.windows.pane(id.context().session()) else {
            return false;
        };
        self.last_pointer
            .insert(id.context().session().session_id(), position);
        if let Some(captured) = self.windows.captured_window(pane) {
            if captured != id {
                return false;
            }
            let hits = self.native_hits(sessions).unwrap_or_default();
            let clicks = 0;
            let consumed = self.windows.pointer(
                pane,
                PointerReport {
                    position,
                    button,
                    modifiers,
                    clicks,
                    pressure: None,
                },
                |target, point| {
                    allowed
                        .contains(&target)
                        .then(|| hits.get(&target).and_then(|scene| scene.hit(point)))
                        .flatten()
                },
            );
            self.drain_events_of(sessions);
            return consumed;
        }
        let Some(window) = self.windows.get(id) else {
            return false;
        };
        let Ok(local) = Point::new(
            position.x.get() - window.options.bounds.origin.x.get(),
            position.y.get() - window.options.bounds.origin.y.get(),
        ) else {
            return false;
        };
        let clicks = 0;
        self.native_emit(
            id,
            Event::Pointer {
                position: local,
                region,
                button,
                modifiers,
                clicks,
                pressure: None,
            },
        )
    }
    pub(crate) fn native_refresh_hover(
        &mut self,
        pane: crate::presenter::PaneId,
        sessions: &[u64],
        allowed: &[SurfaceIdentity],
        position: Point,
    ) {
        let hits = self.native_hits(sessions).unwrap_or_default();
        self.windows.refresh_hover(pane, position, |id, point| {
            allowed
                .contains(&id)
                .then(|| hits.get(&id).and_then(|scene| scene.hit(point)))
                .flatten()
        });
        self.drain_events_of(sessions);
    }
    pub(crate) fn native_wheel_scoped(
        &mut self,
        sessions: &[u64],
        allowed: &[SurfaceIdentity],
        position: Point,
        scroll: Scroll,
        modifiers: u32,
    ) -> bool {
        let Some(pane) = self.windows.pane_of_sessions(sessions) else {
            return false;
        };
        let hits = self.native_hits(sessions).unwrap_or_default();
        let consumed = self
            .windows
            .wheel(pane, position, scroll, modifiers, |id, point| {
                if !allowed.contains(&id) {
                    return None;
                }
                hits.get(&id)
                    .and_then(|scene| scene.hit(point))
                    .map(|region| (region.id, region.role))
            });
        self.drain_events_of(sessions);
        consumed
    }
    pub(crate) fn native_cursor(
        &self,
        pane: crate::presenter::PaneId,
    ) -> Option<vivid_protocol::vector::CursorShape> {
        let hovered = self.windows.hovered_window(pane)?;
        (self.native_owner_ready(hovered) && self.native_input_valid(hovered))
            .then(|| self.windows.cursor(pane))
            .flatten()
    }
    pub(crate) fn suspend_native(&mut self, owner: u64) {
        self.retire_action_focus(owner);
        if let Some(native) = &mut self.native {
            native
                .compositions
                .retain(|_, frame| !frame.owners.contains(&owner));
        }
        self.clear_owner_gesture(owner);
    }
    pub(crate) fn native_stamp(&self, sessions: &[u64]) -> Stamp {
        self.windows
            .visible()
            .filter(|window| sessions.contains(&window.identity.context().session().session_id()))
            .map(|window| (window.identity, window.generation, window.revision))
            .collect()
    }

    pub(crate) fn native_ready(&self, sessions: &[u64]) -> bool {
        self.native.as_ref().is_none_or(|native| {
            let mut owners = sessions.to_vec();
            owners.sort_unstable();
            let stamp = self.native_stamp(sessions);
            native
                .compositions
                .values()
                .any(|frame| frame.owners == owners && frame.stamp == stamp)
        })
    }

    pub(crate) fn prune_native(&mut self) {
        let mut retired = Vec::new();
        if let Some(native) = &mut self.native {
            native.prepared.retain(|(id, _), prepared| {
                let valid = prepared.pin.strong_count() != 0
                    && self.windows.get(*id).is_some_and(|window| {
                        window.generation == prepared.submission.address.generation
                            && window.scene_revision <= prepared.submission.revision
                    })
                    && self.channels.get(&(*id, prepared.submission.track_id))
                        == Some(&prepared.submission.channel_generation);
                if !valid
                    && !prepared.resolved
                    && !native
                        .pending
                        .get(id)
                        .is_some_and(|pending| pending.submission == prepared.submission)
                    && native.committed.get(id) != Some(&prepared.submission)
                {
                    retired.push((*id, prepared.submission));
                }
                valid
            });
        }
        for (id, submission) in retired {
            self.native_outcome(submission, id, PresentationOutcome::Superseded);
        }

        let expired: Vec<_> = self
            .native
            .as_ref()
            .into_iter()
            .flat_map(|native| &native.pending)
            .filter(|(id, pending)| {
                self.windows
                    .get(**id)
                    .is_none_or(|window| window.generation != pending.submission.address.generation)
            })
            .map(|(id, pending)| (*id, pending.submission))
            .collect();
        for (id, submission) in expired {
            self.native_outcome(submission, id, PresentationOutcome::Superseded);
        }
        let Some(native) = &mut self.native else {
            return;
        };
        native.pending.retain(|id, value| {
            self.windows
                .get(*id)
                .is_some_and(|window| window.generation == value.submission.address.generation)
        });
        native.displayed.retain(|id, value| {
            self.windows
                .get(*id)
                .is_some_and(|window| window.generation == value.submission.address.generation)
        });
        native.committed.retain(|id, value| {
            self.windows
                .get(*id)
                .is_some_and(|window| window.generation == value.address.generation)
        });
        self.displayed
            .retain(|id, _| self.windows.get(*id).is_some());
        self.editor_carets
            .retain(|id, _| self.windows.get(*id).is_some());
        self.accepted
            .retain(|id, _| self.windows.get(*id).is_some());
    }

    pub(crate) fn revoke_native_input(&mut self, sessions: &[u64]) {
        let ready = self.native_ready(sessions);
        let stale: Vec<_> = self
            .native
            .as_ref()
            .into_iter()
            .flat_map(|native| &native.displayed)
            .filter(|(id, _)| sessions.contains(&id.context().session().session_id()))
            .filter(|(id, _)| !ready || !self.native_input_valid(**id))
            .map(|(id, display)| (*id, display.submission))
            .collect();
        for (id, submission) in stale {
            if !self
                .native
                .as_mut()
                .expect("native input state")
                .invalidated
                .insert(id)
            {
                continue;
            }
            self.windows.release_pointer(id);
            self.last_gesture.retain(|_, (target, _, _)| *target != id);
            let event = vivid_protocol::overlay::wire::InputEvent {
                address: submission.address,
                scene_revision: submission.revision,
                event: Event::Cancel,
            };
            if let Ok(payload) = event.payload() {
                self.queue_for(
                    id.context().session().session_id(),
                    LaneRecord {
                        retention: None,
                        record_type: messages::OVERLAY_INPUT_EVENT,
                        object_id: id.surface_id(),
                        payload,
                    },
                );
            }
        }
    }

    pub(crate) fn close_native(&mut self, id: SurfaceIdentity) {
        if let Some(pending) = self.native.as_mut().and_then(|native| {
            let pending = native.pending.remove(&id)?;
            native.prepared.remove(&(id, pending.submission.revision));
            Some(pending)
        }) {
            self.native_outcome(pending.submission, id, PresentationOutcome::Superseded);
        }
        self.windows
            .close(id, vivid_protocol::overlay::DismissReason::Closed);
        self.drain_events(owner(id.context().session().session_id()));
        self.prune_native();
    }
    pub(crate) fn native_windows(&self) -> Vec<vivid_protocol::overlay::Window> {
        self.windows.visible().cloned().collect()
    }

    pub(crate) fn native_input_valid(&self, id: SurfaceIdentity) -> bool {
        self.native
            .as_ref()
            .is_none_or(|native| native.valid(&self.windows, &self.viewports, id))
    }

    pub(crate) fn native_owner_ready(&self, id: SurfaceIdentity) -> bool {
        self.native.as_ref().is_none_or(|native| {
            native.compositions.values().any(|frame| {
                frame.owners.contains(&id.context().session().session_id())
                    && self.native_ready(&frame.owners)
            })
        })
    }

    pub(crate) fn native_hits(
        &self,
        sessions: &[u64],
    ) -> Option<HashMap<SurfaceIdentity, Arc<CompiledScene>>> {
        self.native.as_ref().map(|native| {
            if !self.native_ready(sessions) {
                HashMap::new()
            } else {
                native
                    .displayed
                    .iter()
                    .filter(|(id, _)| {
                        sessions.contains(&id.context().session().session_id())
                            && native.valid(&self.windows, &self.viewports, **id)
                    })
                    .map(|(id, scene)| (*id, scene.compiled.clone()))
                    .collect()
            }
        })
    }

    pub(super) fn accept_native(
        &mut self,
        window: SurfaceIdentity,
        track: u64,
        channel: u64,
        frame: Frame,
    ) -> bool {
        self.prune_native();
        let Some(state) = self.windows.get(window) else {
            return false;
        };
        let submission = Submission {
            address: WindowAddress {
                context_id: window.context().context_id(),
                surface_id: window.surface_id(),
                generation: state.generation,
            },
            track_id: track,
            channel_generation: channel,
            epoch: frame.epoch,
            revision: frame.revision,
        };
        let old = self
            .native
            .as_mut()
            .and_then(|native| native.pending.insert(window, Pending { submission }));
        if let Some(old) = old
            && !self.native.as_ref().is_some_and(|native| {
                native
                    .prepared
                    .contains_key(&(window, old.submission.revision))
            })
        {
            self.native_outcome(old.submission, window, PresentationOutcome::Superseded);
        }
        true
    }

    pub(crate) fn native_outcome(
        &mut self,
        submission: Submission,
        window: SurfaceIdentity,
        outcome: PresentationOutcome,
    ) {
        self.resolve(window, submission, outcome);
    }

    pub(crate) fn present_native(
        &mut self,
        window: SurfaceIdentity,
        placement: u64,
        submission: Submission,
        compiled: Arc<CompiledScene>,
        canvas: Canvas,
        reservation: Arc<()>,
    ) -> bool {
        let Some(state) = self.windows.get(window) else {
            return false;
        };
        if state.revision != placement || state.generation != submission.address.generation {
            return false;
        }
        let viewport = self
            .viewports
            .get(&window.context().session().session_id())
            .map_or(0, |v| v.revision);
        let Some(native) = &mut self.native else {
            return false;
        };
        if native.committed.get(&window) != Some(&submission) {
            if !native
                .prepared
                .get(&(window, submission.revision))
                .is_some_and(|prepared| {
                    prepared.submission == submission && prepared.pin.strong_count() != 0
                })
            {
                return false;
            }
            if native
                .pending
                .get(&window)
                .is_some_and(|pending| pending.submission == submission)
            {
                native.pending.remove(&window);
            }
            native
                .prepared
                .get_mut(&(window, submission.revision))
                .expect("reserved native scene")
                .resolved = true;
            if self
                .windows
                .publish_scene(window, submission.address.generation, submission.revision)
                .is_err()
            {
                return false;
            }
            self.displayed.insert(
                window,
                PresentedScene {
                    window,
                    revision: submission.revision,
                    canvas,
                    submission,
                },
            );
            self.semantics.remove(&window);
            self.editor_carets.remove(&window);
            self.native_outcome(submission, window, PresentationOutcome::Presented);
        }
        let native = self.native.as_mut().expect("native presenter attached");
        native.committed.insert(window, submission);
        native.invalidated.remove(&window);
        native.displayed.insert(
            window,
            Displayed {
                submission,
                placement,
                viewport,
                compiled,
                _reservation: reservation,
            },
        );
        self.drain_events(owner(window.context().session().session_id()));
        true
    }
}
