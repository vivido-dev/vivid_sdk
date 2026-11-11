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
    /// Samples a focused native-first editor without exposing document contents.
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
                HostedTextState {
                    target: HostedTextTarget { scope, focus },
                    caret,
                }
            })
        });
        self.platform_window.set_input_handler(handler);
        state
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
        // The private process protocol uses the same 4-KiB text bound as Vivid
        // overlay composition. Check borrowed data before any conversion/copy.
        if commit.is_some_and(|text| text.len() > 4096)
            || preedit.len() > 4096
            || selection.as_ref().is_some_and(|r| {
                r.start > r.end
                    || !preedit.is_char_boundary(r.start)
                    || !preedit.is_char_boundary(r.end)
            })
            || !Arc::ptr_eq(&self.hosted_text_scope.borrow(), &target.scope)
            || self.focus != Some(target.focus)
        {
            return false;
        }
        let Some(mut handler) = self.platform_window.take_input_handler() else {
            return false;
        };
        let accepted = handler.focus_id() == Some(target.focus)
            && handler.accepts_text_input(self, cx)
            && self.hosted_text_target_live(target);
        if accepted {
            let selection = selection.map(|r| {
                preedit[..r.start].encode_utf16().count()..preedit[..r.end].encode_utf16().count()
            });
            let flag = self.hosted_text_dispatching.clone();
            let previous = flag.replace(true);
            let _dispatching = Dispatching { flag, previous };
            handler.dispatch_hosted_composition(target, commit, preedit, selection, self, cx);
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
        fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}
        fn replace_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            text: &str,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.reports.borrow_mut().commits.push(text.into());
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
}
