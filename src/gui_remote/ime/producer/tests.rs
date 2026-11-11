//! Final GUI admission rechecks the producer's watchdog after native queueing.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::gui::{
    AppContext as _, Context, EntityInputHandler, FocusHandle, IntoElement, ParentElement as _,
    Render, Styled as _, TestAppContext, canvas, div,
};
use std::{cell::RefCell, ops::Range, rc::Rc};
use vivid_protocol::{revision::SurfaceGeneration, time::Monotonic};

#[derive(Default)]
struct Reports {
    commits: Vec<String>,
    preedit: String,
}
struct Field {
    focus: FocusHandle,
    reports: Rc<RefCell<Reports>>,
}
impl Render for Field {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::gui::InteractiveElement as _;
        let focus = self.focus.clone();
        let view = cx.entity();
        div().size_full().track_focus(&self.focus).child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    window.handle_input(&focus, gui::ElementInputHandler::new(bounds, view), cx)
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
    ) -> Option<gui::UTF16Selection> {
        Some(gui::UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.reports.borrow_mut().preedit.clear();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.reports.borrow_mut().commits.push(text.into());
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.reports.borrow_mut().preedit = text.into();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: gui::Bounds<gui::Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<gui::Bounds<gui::Pixels>> {
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: gui::Point<gui::Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
fn input() -> Injection {
    let mut input = crate::gui_remote::Shared::new()
        .injection
        .into_inner()
        .unwrap();
    input.lifecycle = Some(Arc::new(crate::SessionLifecycle::new()));
    input.guard.set_preconditions(crate::DesktopPreconditions {
        surface_present: true,
        surface_generation: SurfaceGeneration::new(1),
        capability_mask: crate::INPUT_CLASS_KEYBOARD,
        presented: true,
        lane_live: true,
    });
    input
        .guard
        .enable(
            7,
            SurfaceGeneration::new(1),
            crate::INPUT_CLASS_KEYBOARD,
            2_000_000,
            0,
        )
        .unwrap();
    input
        .guard
        .handle_bound_at(
            &crate::InputBindingStatus {
                producer_epoch: 1,
                grant_generation: 1,
                context_id: 1,
                surface_id: 7,
                surface_generation: 1,
                effective_classes: crate::INPUT_CLASS_KEYBOARD,
                state: 1,
                reason: 0,
                watchdog_timeout_us: 2_000_000,
            },
            Monotonic::ZERO,
        )
        .unwrap();
    input
}
fn producer(input: &Injection) -> Producer {
    let state = Arc::new(State::default());
    state.bound.lock().unwrap().tag = input.guard.current_tag().map(binding);
    Producer {
        state,
        configured: true,
        target: None,
        editor: 0,
        generation: 0,
        reset: 0,
        tag: None,
        marked: false,
    }
}
fn queue(producer: &Producer, edit: Edit, end: Instant) {
    producer.state.bound.lock().unwrap().jobs.push_back(Job {
        operation: Operation {
            editor: producer.editor,
            reset: producer.reset,
            edit,
        },
        end,
    });
}
#[test]
fn queued_native_edits_recheck_revocation_and_expiry_without_cancelling_a_peer() {
    let mut cx = TestAppContext::single();
    let reports = [
        Rc::new(RefCell::new(Reports::default())),
        Rc::new(RefCell::new(Reports::default())),
    ];
    let windows = reports.each_ref().map(|reports| {
        cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Field {
                focus,
                reports: reports.clone(),
            }
        })
    });
    let mut inputs = [input(), input()];
    assert_eq!(inputs[0].guard.current_tag(), inputs[1].guard.current_tag());
    let mut producers = [producer(&inputs[0]), producer(&inputs[1])];
    for i in 0..2 {
        cx.update_window(windows[i].into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            producers[i].pump(&inputs[i], window, cx).unwrap();
        })
        .unwrap();
    }
    for p in &producers {
        queue(
            p,
            Edit {
                preedit: "é😀漢".into(),
                selection: Some((2, 6)),
                ..Default::default()
            },
            Instant::now() + BUDGET,
        );
    }
    for i in 0..2 {
        cx.update_window(windows[i].into(), |_, window, cx| {
            producers[i].pump(&inputs[i], window, cx).unwrap()
        })
        .unwrap();
    }
    assert_eq!(reports[0].borrow().preedit, "é😀漢");
    assert_eq!(reports[1].borrow().preedit, "é😀漢");
    for p in &producers {
        queue(
            p,
            Edit {
                commit: Some("commit".into()),
                ..Default::default()
            },
            Instant::now() + BUDGET,
        );
    }
    inputs[0].revoke();
    for i in 0..2 {
        cx.update_window(windows[i].into(), |_, window, cx| {
            producers[i].pump(&inputs[i], window, cx).unwrap()
        })
        .unwrap();
    }
    assert!(reports[0].borrow().commits.is_empty());
    assert!(reports[0].borrow().preedit.is_empty());
    assert_eq!(reports[1].borrow().commits, ["commit"]);
    queue(
        &producers[1],
        Edit {
            commit: Some("expired".into()),
            ..Default::default()
        },
        Instant::now() - BUDGET,
    );
    cx.update_window(windows[1].into(), |_, window, cx| {
        producers[1].pump(&inputs[1], window, cx).unwrap()
    })
    .unwrap();
    assert_eq!(reports[1].borrow().commits, ["commit"]);
    queue(
        &producers[1],
        Edit {
            preedit: "pending".into(),
            ..Default::default()
        },
        Instant::now() + BUDGET,
    );
    inputs[1].origin = Instant::now() - Duration::from_secs(3);
    cx.update_window(windows[1].into(), |_, window, cx| {
        producers[1].pump(&inputs[1], window, cx).unwrap()
    })
    .unwrap();
    assert!(reports[1].borrow().preedit.is_empty());
}
