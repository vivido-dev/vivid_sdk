//! Bounded document snapshots under the ordinary authenticated overlay focus grant.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_protocol::overlay::wire::text::editor::Edit;
use vivid_protocol::overlay::wire::text::editor::{Context, SetContext};

// Separate from glyph/layout resources: at most 256 KiB of current source text.
const MAX_CONTEXTS: usize = 64;

#[derive(Default)]
pub(super) struct Editors(HashMap<SurfaceIdentity, Slot>);
pub(super) struct Slot {
    context: Context,
    submission: Submission,
    focus: u64,
    lane: u64,
    active: bool,
}
impl Editors {
    pub(super) fn retain(&mut self, keep: impl FnMut(&SurfaceIdentity, &mut Slot) -> bool) {
        self.0.retain(keep);
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    submission: Submission,
    focus: u64,
    lane: u64,
    editor: u64,
}
impl Overlays {
    pub(crate) fn document_host_available(&self) -> bool {
        #[cfg(feature = "vector-render")]
        {
            self.native.is_some() || self.document_relay
        }
        #[cfg(not(feature = "vector-render"))]
        {
            self.document_relay
        }
    }
    pub(crate) fn set_text_context(
        &mut self,
        session: u64,
        object: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let request = SetContext::decode(object, &Value::Map(payload.clone()))
            .map_err(|_| "invalid overlay text context")?;
        let id = request
            .address
            .identity(owner(session))
            .map_err(|_| "invalid overlay identity")?;
        let window = self.windows.get(id).ok_or("overlay window is absent")?;
        if window.generation != request.address.generation
            || !self.input_enabled(session)
            || !self.windows.input_eligible(id)
            || self.windows.focus_of(id) != Some(id)
            || !self
                .displayed
                .get(&id)
                .is_some_and(|s| s.revision == request.scene_revision)
        {
            return Err("editor context requires the focused displayed scene");
        }
        #[cfg(feature = "vector-render")]
        if self.native.is_some() && !self.native_window_ready(id) {
            return Err("editor context requires displayed native geometry");
        }
        let pane = self
            .windows
            .pane(owner(session))
            .ok_or("editor pane is absent")?;
        let focus = self
            .windows
            .focus_generation(pane)
            .ok_or("editor focus is absent")?;
        let lane = self
            .input_generation(session)
            .ok_or("editor input lane is absent")?;
        if let Some(context) = request.context {
            if let Some(old) = self.editors.0.get(&id) {
                if context.editor < old.context.editor
                    || (context.editor == old.context.editor
                        && (!old.active
                            || context.sensitive != old.context.sensitive
                            || context.revision < old.context.revision
                            || (context.revision == old.context.revision
                                && context != old.context)))
                {
                    return Err("editor context did not advance its identity or revision");
                }
            } else if self.editors.0.len() >= MAX_CONTEXTS {
                return Err("editor context capacity exhausted");
            }
            self.editors.0.insert(
                id,
                Slot {
                    context,
                    submission: self
                        .displayed
                        .get(&id)
                        .expect("validated displayed scene")
                        .submission,
                    focus,
                    lane,
                    active: true,
                },
            );
        } else if let Some(slot) = self.editors.0.get_mut(&id) {
            slot.active = false;
        }
        Ok((messages::OK, Vec::new()))
    }

    pub(crate) fn editor_context(&self, id: SurfaceIdentity) -> Option<Context> {
        let slot = self.editors.0.get(&id)?;
        let pane = self.windows.pane(id.context().session())?;
        let current = self.displayed.get(&id)?.submission;
        let original = slot.submission;
        (current.address == original.address
            && current.track_id == original.track_id
            && current.channel_generation == original.channel_generation
            && current.epoch == original.epoch
            && self.input_generation(id.context().session().session_id()) == Some(slot.lane)
            && slot.active
            && self.input_enabled(id.context().session().session_id())
            && self.windows.focus(pane) == Some(id)
            && self.windows.focus_generation(pane) == Some(slot.focus)
            && self.windows.input_eligible(id))
        .then(|| slot.context.clone())
    }

    pub(crate) fn editor_stamp(&self, session: u64, context: u64, surface: u64) -> Option<Stamp> {
        let id = owner(session)
            .context(context)
            .ok()?
            .surface(surface)
            .ok()?;
        self.editor_context(id)?;
        let slot = self.editors.0.get(&id)?;
        Some(Stamp {
            submission: slot.submission,
            focus: slot.focus,
            lane: slot.lane,
            editor: slot.context.editor,
        })
    }

    pub(crate) fn set_document_relay(&mut self, enabled: bool) {
        if self.document_relay != enabled {
            self.editors
                .0
                .values_mut()
                .for_each(|slot| slot.active = false);
            self.document_relay = enabled;
        }
    }

    pub(crate) fn relay_editor_edit(&mut self, session: u64, edit: &Edit) -> bool {
        let Ok(id) = edit.address.identity(owner(session)) else {
            return false;
        };
        let Some(context) = self.editor_context(id) else {
            return false;
        };
        if !self.document_relay
            || edit.validate_context(&context).is_err()
            || self.displayed.get(&id).is_none_or(|displayed| {
                displayed.submission.address != edit.address
                    || displayed.revision != edit.scene_revision
                    || self
                        .editors
                        .0
                        .get(&id)
                        .is_none_or(|slot| slot.submission != displayed.submission)
            })
        {
            return false;
        }
        let Ok(payload) = edit.payload() else {
            return false;
        };
        self.queue_for(
            session,
            LaneRecord {
                record_type: messages::OVERLAY_TEXT_EDIT,
                object_id: id.surface_id(),
                payload,
            },
        );
        true
    }

    #[cfg(feature = "vector-render")]
    pub(crate) fn native_editor_context(&self, id: SurfaceIdentity) -> Option<Context> {
        self.editor_context(id)
    }

    #[cfg(feature = "vector-render")]
    pub(crate) fn native_editor_edit(
        &mut self,
        id: SurfaceIdentity,
        mut edit: Edit,
    ) -> Option<bool> {
        let context = self.native_editor_context(id)?;
        if edit.validate_context(&context).is_err() {
            return Some(false);
        }
        let slot = self.editors.0.get(&id)?;
        let displayed = self.displayed.get(&id)?;
        if !self.native_window_ready(id) || slot.submission != displayed.submission {
            return None;
        }
        edit.scene_revision = displayed.revision;
        let payload = match edit.payload() {
            Ok(p) => p,
            Err(_) => return Some(false),
        };
        self.queue_for(
            id.context().session().session_id(),
            LaneRecord {
                record_type: messages::OVERLAY_TEXT_EDIT,
                object_id: id.surface_id(),
                payload,
            },
        );
        Some(true)
    }
}
