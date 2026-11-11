//! Bounded document snapshots under the ordinary authenticated overlay focus grant.

// Rust guideline compliant 2026-10-07
use super::*;
#[cfg(feature = "vector-render")]
use vivid_protocol::overlay::wire::text::editor::Edit;
use vivid_protocol::overlay::wire::text::editor::{Context, SetContext};

// Separate from glyph/layout resources: at most 256 KiB of current source text.
const MAX_CONTEXTS: usize = 64;

#[derive(Default)]
pub(super) struct Editors(HashMap<SurfaceIdentity, Slot>);
pub(super) struct Slot {
    context: Context,
    #[cfg(feature = "vector-render")]
    submission: Submission,
    #[cfg(feature = "vector-render")]
    focus: u64,
    active: bool,
}
impl Editors {
    pub(super) fn retain(&mut self, keep: impl FnMut(&SurfaceIdentity, &mut Slot) -> bool) {
        self.0.retain(keep);
    }
}
impl Overlays {
    pub(crate) fn document_host_available(&self) -> bool {
        #[cfg(feature = "vector-render")]
        {
            self.native.is_some()
        }
        #[cfg(not(feature = "vector-render"))]
        {
            false
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
        if !self.native_window_ready(id) {
            return Err("editor context requires displayed native geometry");
        }
        #[cfg(feature = "vector-render")]
        let pane = self
            .windows
            .pane(owner(session))
            .ok_or("editor pane is absent")?;
        #[cfg(feature = "vector-render")]
        let focus = self
            .windows
            .focus_generation(pane)
            .ok_or("editor focus is absent")?;
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
                    #[cfg(feature = "vector-render")]
                    submission: self
                        .displayed
                        .get(&id)
                        .expect("validated displayed scene")
                        .submission,
                    #[cfg(feature = "vector-render")]
                    focus,
                    active: true,
                },
            );
        } else if let Some(slot) = self.editors.0.get_mut(&id) {
            slot.active = false;
        }
        Ok((messages::OK, Vec::new()))
    }

    #[cfg(feature = "vector-render")]
    pub(crate) fn native_editor_context(&self, id: SurfaceIdentity) -> Option<Context> {
        let slot = self.editors.0.get(&id)?;
        let pane = self.windows.pane(id.context().session())?;
        let current = self.displayed.get(&id)?.submission;
        let original = slot.submission;
        (current.address == original.address
            && current.track_id == original.track_id
            && current.channel_generation == original.channel_generation
            && current.epoch == original.epoch
            && slot.active
            && self.input_enabled(id.context().session().session_id())
            && self.windows.focus(pane) == Some(id)
            && self.windows.focus_generation(pane) == Some(slot.focus)
            && self.windows.input_eligible(id))
        .then(|| slot.context.clone())
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
