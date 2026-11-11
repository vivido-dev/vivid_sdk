//! Bounded text-input-v3 bridge to an embedding platform's native input method.
//!
//! Smithay's text-input helper requires a separate privileged input-method client.
//! Here the embedding GUI is the input method, so only the ordinary client-side
//! text-input global is exposed through the existing typed protocol dispatcher.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    input::Seat,
    reexports::{
        wayland_protocols::wp::text_input::zv3::server::{
            zwp_text_input_manager_v3::{self as manager, ZwpTextInputManagerV3},
            zwp_text_input_v3::{self as input, ZwpTextInputV3},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::ClientId,
            protocol::wl_surface::WlSurface,
        },
    },
};
use std::ops::Range;

/// Maximum UTF-8 bytes retained or sent in one native text-input string.
pub const MAX_IME_BYTES: usize = 4000;
// Bound protocol objects as well as each current/pending surrounding-text buffer.
const MAX_INPUTS: usize = 16;

/// Opaque authority for one focused editor and its surrounding-text revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextTarget {
    pub(super) root: Target,
    editor: u64,
    revision: u64,
}
impl TextTarget {
    /// Whether two snapshots identify the same editor, focus and rendered mapping.
    pub fn same_editor(self, other: Self) -> bool {
        self.root == other.root && self.editor == other.editor
    }
}
/// Bounded UTF-8 surrounding text, excluding any current preedit.
#[derive(Clone, PartialEq, Eq)]
pub struct TextContext {
    /// Text around the current selection.
    pub text: String,
    /// Cursor byte offset at a UTF-8 boundary.
    pub cursor: usize,
    /// Selection anchor byte offset at a UTF-8 boundary.
    pub anchor: usize,
}
impl fmt::Debug for TextContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextContext")
            .field("bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}
/// Committed editor metadata for the embedding GUI's native input handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextInputState {
    /// Authority to retain on each queued edit.
    pub target: TextTarget,
    /// Optional client-provided surrounding text.
    pub context: Option<TextContext>,
    /// Candidate rectangle in root logical coordinates, clipped to its canvas.
    pub caret: Option<[i32; 4]>,
    /// The editor requests hidden or sensitive input.
    pub sensitive: bool,
    /// The editor accepts multiple lines.
    pub multiline: bool,
    /// Bounded platform text-assistance preferences, suppressed for sensitive fields.
    pub configuration: crate::gui::TextInputConfiguration,
}
/// One atomic native edit, using UTF-8 byte offsets.
///
/// A replacement must contain the complete current selection. `None` replaces
/// the selection/preedit normally. Preedit indices refer to its own string;
/// `None` hides its cursor. Empty/default edits cancel an existing preedit.
#[derive(Clone, Default)]
pub struct TextEdit {
    /// Optional range in the snapshot's surrounding text, excluding preedit.
    pub replacement: Option<Range<usize>>,
    /// Committed text inserted once when present.
    pub commit: Option<String>,
    /// New preedit and its optional cursor/selection byte range.
    pub preedit: Option<(String, Option<Range<usize>>)>,
}
impl fmt::Debug for TextEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextEdit")
            .field("commit_bytes", &self.commit.as_ref().map(String::len))
            .field(
                "preedit_bytes",
                &self.preedit.as_ref().map(|(text, _)| text.len()),
            )
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Default, PartialEq, Eq)]
struct Values {
    context: Option<TextContext>,
    caret: Option<[i32; 4]>,
    hint: u32,
    purpose: u32,
}
#[derive(Default)]
struct Pending {
    enable: Option<bool>,
    context: Option<TextContext>,
    caret: Option<[i32; 4]>,
    content: Option<(u32, u32)>,
    external: bool,
}
struct Instance {
    object: ZwpTextInputV3,
    entered: Option<WlSurface>,
    // Native Wayland's done serial is the modulo-u32 number of object commits.
    serial: u32,
    editor: u64,
    revision: u64,
    enabled: bool,
    preedit: bool,
    current: Values,
    pending: Pending,
}
#[derive(Default)]
pub(super) struct TextInputs {
    instances: Vec<Instance>,
    focus: Option<WlSurface>,
    next: u64,
}
impl TextInputs {
    pub(super) fn register(display: &DisplayHandle) {
        display.create_global::<state::State, ZwpTextInputManagerV3, _>(1, ());
    }
    pub(super) fn focus(&mut self, focus: Option<&WlSurface>) {
        if self.focus.as_ref() == focus {
            return;
        }
        for instance in &mut self.instances {
            if let Some(old) = instance.entered.take() {
                instance.object.leave(&old);
            }
            instance.enabled = false;
            instance.preedit = false;
            instance.current = Values::default();
            instance.pending = Pending::default();
            if let Some(surface) =
                focus.filter(|surface| instance.object.id().same_client_as(&surface.id()))
            {
                instance.entered = Some(surface.clone());
                instance.object.enter(surface);
            }
        }
        self.focus = focus.cloned();
    }
    fn issue(&mut self) -> io::Result<u64> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| invalid("native text identity exhausted"))?;
        Ok(self.next)
    }
    fn active(&self) -> Option<&Instance> {
        self.instances.iter().find(|instance| {
            instance.enabled && instance.entered.is_some() && instance.object.is_alive()
        })
    }
    pub(super) fn snapshot(
        &self,
        root: Target,
        scene: &scene::Scene,
        size: [u32; 2],
    ) -> Option<TextInputState> {
        let instance = self.active()?;
        let origin = scene.origin(instance.entered.as_ref()?)?;
        let caret = instance.current.caret.and_then(|[x, y, width, height]| {
            let left = (i64::from(x) + i64::from(origin[0])).max(0);
            let top = (i64::from(y) + i64::from(origin[1])).max(0);
            let right = (i64::from(x) + i64::from(origin[0]) + i64::from(width.max(1)))
                .min(i64::from(size[0]));
            let bottom = (i64::from(y) + i64::from(origin[1]) + i64::from(height.max(1)))
                .min(i64::from(size[1]));
            (right > left && bottom > top).then(|| {
                [
                    left as i32,
                    top as i32,
                    (right - left) as i32,
                    (bottom - top) as i32,
                ]
            })
        });
        let sensitive = instance.current.hint
            & (input::ContentHint::HiddenText | input::ContentHint::SensitiveData).bits()
            != 0
            || instance.current.purpose == input::ContentPurpose::Password as u32
            || instance.current.purpose == input::ContentPurpose::Pin as u32;
        let multiline = instance.current.hint & input::ContentHint::Multiline.bits() != 0
            || instance.current.purpose == input::ContentPurpose::Terminal as u32;
        let hint = input::ContentHint::from_bits_retain(instance.current.hint);
        let capitalization = if sensitive {
            crate::gui::Autocapitalize::None
        } else if hint.contains(input::ContentHint::Uppercase) {
            crate::gui::Autocapitalize::Characters
        } else if hint.contains(input::ContentHint::Titlecase) {
            crate::gui::Autocapitalize::Words
        } else if hint.contains(input::ContentHint::AutoCapitalization) {
            crate::gui::Autocapitalize::Sentences
        } else {
            crate::gui::Autocapitalize::None
        };
        Some(TextInputState {
            target: TextTarget {
                root,
                editor: instance.editor,
                revision: instance.revision,
            },
            context: instance.current.context.clone(),
            caret,
            sensitive,
            multiline,
            configuration: crate::gui::TextInputConfiguration {
                autocorrect: !sensitive && hint.contains(input::ContentHint::Spellcheck),
                autocapitalize: capitalization,
                suggestions: !sensitive
                    && hint.intersects(
                        input::ContentHint::Completion | input::ContentHint::Spellcheck,
                    ),
                input_action: if multiline {
                    crate::gui::TextInputAction::Enter
                } else {
                    crate::gui::TextInputAction::Unspecified
                },
            },
        })
    }
    pub(super) fn edit(&mut self, target: TextTarget, edit: TextEdit) -> io::Result<bool> {
        let Some(instance) = self.instances.iter_mut().find(|instance| {
            instance.enabled
                && instance.entered.is_some()
                && instance.object.is_alive()
                && instance.editor == target.editor
        }) else {
            return Ok(false);
        };
        for text in edit
            .commit
            .iter()
            .chain(edit.preedit.iter().map(|(text, _)| text))
        {
            if text.len() > MAX_IME_BYTES || text.contains('\0') {
                return Err(invalid("native text exceeds string bounds"));
            }
        }
        let (before, after) = if let Some(range) = &edit.replacement {
            if target.revision != instance.revision {
                return Ok(false);
            }
            let context = instance
                .current
                .context
                .as_ref()
                .ok_or_else(|| invalid("surrounding text unavailable"))?;
            let low = context.cursor.min(context.anchor);
            let high = context.cursor.max(context.anchor);
            if range.start > low
                || range.end < high
                || range.end > context.text.len()
                || !context.text.is_char_boundary(range.start)
                || !context.text.is_char_boundary(range.end)
            {
                return Err(invalid(
                    "native text replacement exceeds surrounding selection",
                ));
            }
            ((low - range.start) as u32, (range.end - high) as u32)
        } else {
            (0, 0)
        };
        let preedit_range = match &edit.preedit {
            Some((text, Some(range))) => {
                if range.start > range.end
                    || range.end > text.len()
                    || !text.is_char_boundary(range.start)
                    || !text.is_char_boundary(range.end)
                {
                    return Err(invalid("invalid native preedit selection"));
                }
                (range.start as i32, range.end as i32)
            }
            Some((_, None)) => (-1, -1),
            None => (0, 0),
        };
        if before != 0 || after != 0 {
            instance.object.delete_surrounding_text(before, after);
        }
        if let Some(text) = edit.commit {
            instance.object.commit_string(Some(text));
        }
        instance.preedit = edit
            .preedit
            .as_ref()
            .is_some_and(|(text, _)| !text.is_empty());
        instance.object.preedit_string(
            edit.preedit.map(|(text, _)| text),
            preedit_range.0,
            preedit_range.1,
        );
        instance.object.done(instance.serial);
        Ok(true)
    }
    fn request(&mut self, object: &ZwpTextInputV3, request: input::Request) -> io::Result<()> {
        let Some(index) = self
            .instances
            .iter()
            .position(|instance| instance.object == *object)
        else {
            return Ok(());
        };
        if matches!(request, input::Request::Commit) {
            self.instances[index].serial = self.instances[index].serial.wrapping_add(1);
        }
        if self.instances[index].entered.is_none() {
            return Ok(());
        }
        let instance = &mut self.instances[index];
        match request {
            input::Request::Enable => {
                instance.pending = Pending {
                    enable: Some(true),
                    ..Pending::default()
                }
            }
            input::Request::Disable => {
                instance.pending = Pending {
                    enable: Some(false),
                    ..Pending::default()
                }
            }
            input::Request::SetSurroundingText {
                text,
                cursor,
                anchor,
            } => {
                let (cursor, anchor) = (
                    usize::try_from(cursor).map_err(|_| invalid("invalid surrounding cursor"))?,
                    usize::try_from(anchor).map_err(|_| invalid("invalid surrounding anchor"))?,
                );
                if text.len() > MAX_IME_BYTES
                    || text.contains('\0')
                    || !text.is_char_boundary(cursor)
                    || !text.is_char_boundary(anchor)
                {
                    return Err(invalid("invalid surrounding text"));
                }
                instance.pending.context = Some(TextContext {
                    text,
                    cursor,
                    anchor,
                });
            }
            input::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => {
                // Bound native candidate arithmetic independently of the client buffer.
                if x.unsigned_abs() > 8192
                    || y.unsigned_abs() > 8192
                    || !(0..=8192).contains(&width)
                    || !(0..=8192).contains(&height)
                {
                    return Err(invalid("invalid native candidate rectangle"));
                }
                instance.pending.caret = Some([x, y, width, height]);
            }
            input::Request::SetContentType { hint, purpose } => {
                let (hint, purpose) = (
                    hint.into_result()
                        .map_err(|_| invalid("invalid native content hint"))?,
                    purpose
                        .into_result()
                        .map_err(|_| invalid("invalid native content purpose"))?,
                );
                instance.pending.content = Some((hint.bits(), purpose as u32));
            }
            input::Request::SetTextChangeCause { cause } => {
                instance.pending.external = cause
                    .into_result()
                    .map_err(|_| invalid("invalid native text change cause"))?
                    == input::ChangeCause::Other
            }
            input::Request::Commit => {
                let pending = std::mem::take(&mut instance.pending);
                if pending.enable == Some(true)
                    && self
                        .instances
                        .iter()
                        .enumerate()
                        .any(|(other, instance)| other != index && instance.enabled)
                {
                    return Ok(());
                }
                let instance = &mut self.instances[index];
                if pending.enable == Some(false) {
                    instance.enabled = false;
                    instance.preedit = false;
                    instance.current = Values::default();
                    return Ok(());
                }
                if pending.enable != Some(true) && !instance.enabled {
                    return Ok(());
                }
                let mut values = if pending.enable == Some(true) {
                    Values::default()
                } else {
                    instance.current.clone()
                };
                if let Some(context) = pending.context {
                    values.context = Some(context);
                }
                if let Some(caret) = pending.caret {
                    values.caret = Some(caret);
                }
                if let Some((hint, purpose)) = pending.content {
                    values.hint = hint;
                    values.purpose = purpose;
                }
                let new_editor = pending.enable == Some(true)
                    || pending.external
                    || values.hint != instance.current.hint
                    || values.purpose != instance.current.purpose;
                let changed = new_editor
                    || values.context != instance.current.context
                    || values.hint != instance.current.hint
                    || values.purpose != instance.current.purpose;
                let cancel_preedit = new_editor && instance.preedit && pending.enable != Some(true);
                let editor = if new_editor {
                    Some(self.issue()?)
                } else {
                    None
                };
                let revision = if changed { Some(self.issue()?) } else { None };
                let instance = &mut self.instances[index];
                instance.enabled = true;
                instance.current = values;
                if let Some(editor) = editor {
                    instance.editor = editor;
                    instance.preedit = false;
                }
                if let Some(revision) = revision {
                    instance.revision = revision;
                }
                if cancel_preedit {
                    instance.object.preedit_string(None, 0, 0);
                    instance.object.done(instance.serial);
                }
            }
            input::Request::Destroy => (),
            _ => (),
        }
        Ok(())
    }
}
impl GlobalDispatch<ZwpTextInputManagerV3, ()> for state::State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        object: New<ZwpTextInputManagerV3>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(object, ());
    }
}
impl Dispatch<ZwpTextInputManagerV3, ()> for state::State {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &ZwpTextInputManagerV3,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::GetTextInput { id, seat } = request {
            // Request-created objects must have dispatch data even on rejection.
            let object = init.init(id, ());
            if Seat::<Self>::from_resource(&seat).as_ref() != Some(&state.seat)
                || state.text_input.instances.len() >= MAX_INPUTS
            {
                manager.post_error(0u32, "native text input capacity or seat rejected");
                return;
            }
            let entered = state
                .text_input
                .focus
                .as_ref()
                .filter(|surface| surface.id().same_client_as(&object.id()))
                .cloned();
            if let Some(surface) = &entered {
                object.enter(surface);
            }
            state.text_input.instances.push(Instance {
                object,
                entered,
                serial: 0,
                editor: 0,
                revision: 0,
                enabled: false,
                preedit: false,
                current: Values::default(),
                pending: Pending::default(),
            });
        }
    }
}
impl Dispatch<ZwpTextInputV3, ()> for state::State {
    fn request(
        state: &mut Self,
        _: &Client,
        object: &ZwpTextInputV3,
        request: input::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if state.text_input.request(object, request).is_err() {
            object.post_error(0u32, "native text input exceeds hosted bounds");
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, object: &ZwpTextInputV3, _: &()) {
        state
            .text_input
            .instances
            .retain(|instance| instance.object != *object);
    }
}
