//! Focus-pinned composition access for independently hosted offscreen windows.

// Rust guideline compliant 2026-10-07
use super::*;

/// An immutable composition destination in one window and focus incarnation.
#[derive(Clone)]
pub struct HostedTextTarget {
    scope: Arc<()>,
    focus: FocusId,
}
impl Debug for HostedTextTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedTextTarget").finish_non_exhaustive()
    }
}
impl HostedTextTarget {
    /// Compares the owning window and exact focus grant across repaint.
    pub fn same_editor(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.scope, &other.scope) && self.focus == other.focus
    }
}

/// Locally sampled composition eligibility and window-local candidate placement.
#[derive(Clone, Debug)]
pub struct HostedTextState {
    /// The exact registered editor; cannot be redirected after focus changes.
    pub target: HostedTextTarget,
    /// Candidate geometry in logical window coordinates, when available.
    pub caret: Option<Bounds<Pixels>>,
    /// Explicitly exported bounded surrounding text; absent by default.
    pub document: Option<HostedTextContext>,
}

/// An opt-in excerpt excluding preedit, with the complete current selection.
///
/// Text is limited to 4096 UTF-8 bytes. Cursor and anchor are excerpt byte
/// boundaries. Offset is the excerpt's UTF-16 position in the producer document.
/// Sensitive fields must return empty text and zero offsets; they accept only
/// relative composition. Never include passwords or other protected contents.
#[derive(Clone, PartialEq, Eq)]
pub struct HostedTextContext {
    /// Bounded committed surrounding text, excluding preedit.
    pub text: String,
    /// Active selection endpoint in UTF-8 bytes.
    pub cursor: u32,
    /// Fixed selection endpoint in UTF-8 bytes.
    pub anchor: u32,
    /// Excerpt start in producer document UTF-16 units; never sent to the host.
    pub offset_utf16: u32,
    /// Prevents document queries and absolute editing.
    pub sensitive: bool,
}
impl Debug for HostedTextContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedTextContext")
            .field("bytes", &self.text.len())
            .field("sensitive", &self.sensitive)
            .finish_non_exhaustive()
    }
}
impl HostedTextContext {
    /// Checks the excerpt bound, selection boundaries and coordinate arithmetic.
    pub fn is_valid(&self) -> bool {
        self.text.len() <= 4096
            && self.text.is_char_boundary(self.cursor as usize)
            && self.text.is_char_boundary(self.anchor as usize)
            && self
                .offset_utf16
                .checked_add(self.text.encode_utf16().count() as u32)
                .is_some()
            && (!self.sensitive
                || (self.text.is_empty()
                    && self.cursor == 0
                    && self.anchor == 0
                    && self.offset_utf16 == 0))
    }
    fn range(&self, range: &Range<usize>) -> Option<Range<usize>> {
        (range.start <= range.end
            && self.text.is_char_boundary(range.start)
            && self.text.is_char_boundary(range.end))
        .then_some(())?;
        let offset = self.offset_utf16 as usize;
        Some(
            offset.checked_add(self.text[..range.start].encode_utf16().count())?
                ..offset.checked_add(self.text[..range.end].encode_utf16().count())?,
        )
    }
}

/// Bounded composition or document mutation in excerpt UTF-8 coordinates.
#[derive(Default)]
pub struct HostedTextEdit<'a> {
    /// Committed text, if any.
    pub commit: Option<&'a str>,
    /// New preedit, or empty to clear it.
    pub preedit: &'a str,
    /// Ordered byte range within preedit.
    pub selection: Option<Range<usize>>,
    /// Explicit committed-document replacement covering the current selection.
    pub replacement: Option<Range<usize>>,
    /// Standalone ordered document selection, mutually exclusive with text edits.
    pub document_selection: Option<Range<usize>>,
}
impl Debug for HostedTextEdit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedTextEdit").finish_non_exhaustive()
    }
}

struct Dispatching {
    flag: Rc<Cell<bool>>,
    previous: bool,
}
impl Drop for Dispatching {
    fn drop(&mut self) {
        self.flag.set(self.previous);
    }
}

impl Window {
    pub(crate) fn hosted_text_target_live(&self, target: &HostedTextTarget) -> bool {
        Arc::ptr_eq(&self.hosted_text_scope.borrow(), &target.scope)
            && self.focus == Some(target.focus)
    }
    /// Samples a focused native-first editor and its explicitly exported context.
    ///
    /// Raw-key handlers and pending key chords retain their ordinary keybinding
    /// path. Only handlers preferring IME character input opt into this service.
    pub fn hosted_text_state(&mut self, cx: &mut App) -> Option<HostedTextState> {
        let mut handler = self.platform_window.take_input_handler()?;
        let state = self.focus.and_then(|focus| {
            (handler.focus_id() == Some(focus)
                && handler.accepts_text_input(self, cx)
                && handler.prefers_hosted_composition(self, cx))
            .then(|| {
                let scope = self.hosted_text_scope.borrow().clone();
                let caret = handler.selected_bounds(self, cx);
                let document = handler
                    .hosted_text_context(self, cx)
                    .filter(HostedTextContext::is_valid);
                HostedTextState {
                    target: HostedTextTarget { scope, focus },
                    caret,
                    document,
                }
            })
        });
        self.platform_window.set_input_handler(handler);
        state.filter(|s| self.hosted_text_target_live(&s.target))
    }

    /// Delivers bounded composition to its original registered focused editor.
    ///
    /// Selection uses UTF-8 byte offsets into preedit. Repaint preserves the
    /// target; focus round trips, external text/selection notifications and foreign
    /// windows reject it. Notifications raised during this call retain its target.
    /// Callers remain
    /// responsible for authenticating external input before invoking this API.
    /// Returns false for stale targets or malformed/oversized input.
    pub fn dispatch_hosted_composition(
        &mut self,
        target: &HostedTextTarget,
        commit: Option<&str>,
        preedit: &str,
        selection: Option<Range<usize>>,
        cx: &mut App,
    ) -> bool {
        self.dispatch_hosted_edit(
            target,
            None,
            &HostedTextEdit {
                commit,
                preedit,
                selection,
                ..Default::default()
            },
            cx,
        )
    }

    /// Applies an edit only against the exact current opt-in document excerpt.
    ///
    /// Absolute ranges use excerpt UTF-8 offsets; conversion to producer UTF-16
    /// happens after rechecking the application context. Stale contexts, sensitive
    /// fields, split characters and replacements missing the selection are rejected.
    /// Relative composition should use [`Self::dispatch_hosted_composition`].
    pub fn dispatch_hosted_document(
        &mut self,
        target: &HostedTextTarget,
        context: &HostedTextContext,
        edit: &HostedTextEdit<'_>,
        cx: &mut App,
    ) -> bool {
        self.dispatch_hosted_edit(target, Some(context), edit, cx)
    }

    fn dispatch_hosted_edit(
        &mut self,
        target: &HostedTextTarget,
        context: Option<&HostedTextContext>,
        edit: &HostedTextEdit<'_>,
        cx: &mut App,
    ) -> bool {
        if context.is_some_and(|c| !c.is_valid())
            || edit.commit.is_some_and(|text| text.len() > 4096)
            || edit.preedit.len() > 4096
            || edit.selection.as_ref().is_some_and(|r| {
                r.start > r.end
                    || !edit.preedit.is_char_boundary(r.start)
                    || !edit.preedit.is_char_boundary(r.end)
            })
            || (edit.document_selection.is_some()
                && (edit.commit.is_some()
                    || !edit.preedit.is_empty()
                    || edit.replacement.is_some()
                    || edit.selection.is_some()))
            || (edit.replacement.is_some() && edit.commit.is_none() && edit.preedit.is_empty())
            || !self.hosted_text_target_live(target)
        {
            return false;
        }
        let absolute = edit.replacement.is_some() || edit.document_selection.is_some();
        let mut replacement = None;
        let mut document_selection = None;
        if absolute {
            let Some(context) = context.filter(|c| c.is_valid() && !c.sensitive) else {
                return false;
            };
            if let Some(range) = &edit.replacement {
                if range.start > context.cursor.min(context.anchor) as usize
                    || range.end < context.cursor.max(context.anchor) as usize
                {
                    return false;
                }
                let Some(range) = context.range(range) else {
                    return false;
                };
                replacement = Some(range);
            }
            if let Some(range) = &edit.document_selection {
                let Some(range) = context.range(range) else {
                    return false;
                };
                document_selection = Some(range);
            }
        }
        let Some(mut handler) = self.platform_window.take_input_handler() else {
            return false;
        };
        let accepted = handler.focus_id() == Some(target.focus)
            && handler.accepts_text_input(self, cx)
            && (context.is_none() || handler.hosted_text_context(self, cx).as_ref() == context)
            && self.hosted_text_target_live(target);
        if accepted {
            let selection = edit.selection.clone().map(|r| {
                edit.preedit[..r.start].encode_utf16().count()
                    ..edit.preedit[..r.end].encode_utf16().count()
            });
            let flag = self.hosted_text_dispatching.clone();
            let previous = flag.replace(true);
            let _dispatching = Dispatching { flag, previous };
            handler.dispatch_hosted_edit(
                target,
                &HostedTextEdit {
                    commit: edit.commit,
                    preedit: edit.preedit,
                    selection,
                    replacement,
                    document_selection,
                },
                self,
                cx,
            );
        }
        self.platform_window.set_input_handler(handler);
        accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ElementInputHandler, EntityInputHandler, TestAppContext, UTF16Selection, canvas, div,
    };
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct Reports {
        commits: Vec<String>,
        ranges: Vec<Option<Range<usize>>>,
        selected: Vec<Range<usize>>,
        unmarks: usize,
        document: Option<HostedTextContext>,
        preedits: Vec<(String, Option<Range<usize>>)>,
    }
    struct Field {
        focus: FocusHandle,
        reports: Rc<RefCell<Reports>>,
        redirect: Option<FocusHandle>,
    }
    impl Render for Field {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let focus = self.focus.clone();
            let view = cx.entity();
            div().size_full().track_focus(&self.focus).child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx)
                    },
                )
                .size_full(),
            )
        }
    }
    impl EntityInputHandler for Field {
        fn hosted_text_context(
            &mut self,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<HostedTextContext> {
            self.reports.borrow().document.clone()
        }
        fn set_selected_text_range(
            &mut self,
            range: Range<usize>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) {
            self.reports.borrow_mut().selected.push(range);
        }
        fn text_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<String> {
            None
        }
        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<UTF16Selection> {
            Some(UTF16Selection {
                range: 0..0,
                reversed: false,
            })
        }
        fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
            None
        }
        fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
            self.reports.borrow_mut().unmarks += 1;
        }
        fn replace_text_in_range(
            &mut self,
            range: Option<Range<usize>>,
            text: &str,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.reports.borrow_mut().commits.push(text.into());
            self.reports.borrow_mut().ranges.push(range);
            window.notify_text_input_changed(&self.focus);
            if let Some(focus) = self.redirect.take() {
                focus.focus(window, cx);
            }
        }
        fn replace_and_mark_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            text: &str,
            selection: Option<Range<usize>>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) {
            self.reports
                .borrow_mut()
                .preedits
                .push((text.into(), selection));
        }
        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            bounds: Bounds<Pixels>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<Bounds<Pixels>> {
            Some(bounds)
        }
        fn character_index_for_point(
            &mut self,
            _: Point<Pixels>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<usize> {
            None
        }
    }
    #[gpui::test]
    fn composition_targets_are_window_scoped_and_cannot_revive_after_focus_round_trips(
        a: &mut TestAppContext,
        b: &mut TestAppContext,
    ) {
        let first = Rc::new(RefCell::new(Reports::default()));
        let peer = Rc::new(RefCell::new(Reports::default()));
        let one = a.add_window(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Field {
                focus,
                reports: first.clone(),
                redirect: None,
            }
        });
        let two = b.add_window(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Field {
                focus,
                reports: peer.clone(),
                redirect: None,
            }
        });
        let target = a
            .update_window(one.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.hosted_text_state(cx).unwrap().target
            })
            .unwrap();
        let other = b
            .update_window(two.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.hosted_text_state(cx).unwrap().target
            })
            .unwrap();
        assert_eq!(
            target.focus, other.focus,
            "independent owners reuse local focus IDs"
        );
        b.update_window(two.into(), |_, window, cx| {
            assert!(!window.dispatch_hosted_composition(&target, Some("foreign"), "", None, cx));
            assert!(window.dispatch_hosted_composition(&other, Some("peer"), "", None, cx));
        })
        .unwrap();
        a.update_window(one.into(), |view, window, cx| {
            assert!(window.dispatch_hosted_composition(&target, None, "é😀漢", Some(2..6), cx));
            assert!(!window.dispatch_hosted_composition(&target, None, "😀", Some(1..4), cx));
            assert!(!window.dispatch_hosted_composition(
                &target,
                Some(&"x".repeat(4097)),
                "",
                None,
                cx
            ));
            let focus = view.downcast::<Field>().unwrap().read(cx).focus.clone();
            let temporary = cx.focus_handle();
            temporary.focus(window, cx);
            focus.focus(window, cx);
            assert!(!window.dispatch_hosted_composition(&target, Some("stale"), "", None, cx));
            let current = window.hosted_text_state(cx).unwrap().target;
            assert!(!target.same_editor(&current));
            assert!(window.dispatch_hosted_composition(&current, Some("current"), "", None, cx));
            assert!(current.same_editor(&window.hosted_text_state(cx).unwrap().target));
            window.notify_text_selection_changed(&focus);
            assert!(!window.dispatch_hosted_composition(
                &current,
                Some("late selection"),
                "",
                None,
                cx
            ));
        })
        .unwrap();
        assert_eq!(first.borrow().preedits, [("é😀漢".into(), Some(1..3))]);
        assert_eq!(first.borrow().commits, ["current"]);
        assert_eq!(peer.borrow().commits, ["peer"]);
    }
    #[gpui::test]
    fn document_edits_check_excerpt_identity_and_translate_unicode_offsets(
        cx: &mut TestAppContext,
    ) {
        let context = HostedTextContext {
            text: "a😀éz".into(),
            cursor: 7,
            anchor: 5,
            offset_utf16: 100,
            sensitive: false,
        };
        let reports = Rc::new(RefCell::new(Reports {
            document: Some(context.clone()),
            ..Default::default()
        }));
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Field {
                focus,
                reports: reports.clone(),
                redirect: None,
            }
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            let target = window.hosted_text_state(cx).unwrap().target;
            let mut edit = HostedTextEdit {
                commit: Some("文"),
                replacement: Some(1..7),
                ..Default::default()
            };
            assert!(window.dispatch_hosted_document(&target, &context, &edit, cx));
            assert_eq!(reports.borrow().ranges, [Some(101..104)]);
            edit.replacement = Some(2..7);
            assert!(!window.dispatch_hosted_document(&target, &context, &edit, cx));
            edit.replacement = Some(0..5);
            assert!(!window.dispatch_hosted_document(&target, &context, &edit, cx));
            let select = HostedTextEdit {
                document_selection: Some(1..5),
                ..Default::default()
            };
            assert!(window.dispatch_hosted_document(&target, &context, &select, cx));
            assert_eq!(reports.borrow().selected, [101..103]);
            assert_eq!(
                reports.borrow().unmarks,
                1,
                "absolute selection must not interpose a document-mutating unmark callback"
            );
            reports.borrow_mut().document.as_mut().unwrap().offset_utf16 = 200;
            assert!(
                !window.dispatch_hosted_document(&target, &context, &select, cx),
                "offset changed without notification"
            );
            let masked = HostedTextContext {
                text: String::new(),
                cursor: 0,
                anchor: 0,
                offset_utf16: 0,
                sensitive: true,
            };
            reports.borrow_mut().document = Some(masked.clone());
            assert!(!window.dispatch_hosted_document(
                &target,
                &masked,
                &HostedTextEdit {
                    document_selection: Some(0..0),
                    ..Default::default()
                },
                cx
            ));
            assert!(window.dispatch_hosted_composition(&target, Some("private"), "", None, cx));
        })
        .unwrap();
        assert!(
            !HostedTextContext {
                offset_utf16: u32::MAX,
                ..context.clone()
            }
            .is_valid()
        );
        assert!(
            !HostedTextContext {
                sensitive: true,
                ..context.clone()
            }
            .is_valid()
        );
        assert!(!format!("{context:?}").contains("a😀éz"));
    }
}
