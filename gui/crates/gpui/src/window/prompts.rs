use std::ops::Deref;

use futures::channel::oneshot;

use crate::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, PromptButton, PromptLevel, Render,
    StatefulInteractiveElement, Styled, div, opaque_grey, white,
};

use super::Window;

/// The event emitted when a prompt's option is selected.
/// The usize is the index of the selected option, from the actions
/// passed to the prompt.
pub struct PromptResponse(pub usize);

/// A prompt that can be rendered in the window.
pub trait Prompt: EventEmitter<PromptResponse> + Focusable {}

impl<V: EventEmitter<PromptResponse> + Focusable> Prompt for V {}

/// A handle to a prompt that can be used to interact with it.
pub struct PromptHandle {
    sender: oneshot::Sender<usize>,
}

impl PromptHandle {
    pub(crate) fn new(sender: oneshot::Sender<usize>) -> Self {
        Self { sender }
    }

    /// Construct a new prompt handle from a view of the appropriate types
    pub fn with_view<V: Prompt + Render>(
        self,
        view: Entity<V>,
        window: &mut Window,
        cx: &mut App,
    ) -> RenderablePromptHandle {
        let previous_focus = match &window.prompt {
            Some(current) => current.previous_focus.clone(),
            None => window.focused(cx),
        };
        let window_handle = window.window_handle();
        let subscription = cx.subscribe(&view, move |_: Entity<V>, e: &PromptResponse, cx| {
            window_handle
                .update(cx, |_, window, cx| {
                    if let Some(prompt) = window.prompt.take() {
                        window.a11y.clear_action_targets();
                        let _ = prompt.sender.send(e.0);
                        match prompt.previous_focus {
                            Some(focus) => window.focus(&focus, cx),
                            None => window.blur(cx),
                        }
                    }
                })
                .ok();
        });

        window.focus(&view.focus_handle(cx), cx);

        RenderablePromptHandle {
            view: Box::new(view),
            sender: self.sender,
            previous_focus,
            _subscription: subscription,
        }
    }
}

/// A prompt handle capable of being rendered in a window.
pub struct RenderablePromptHandle {
    pub(crate) view: Box<dyn PromptViewHandle>,
    sender: oneshot::Sender<usize>,
    previous_focus: Option<FocusHandle>,
    _subscription: crate::Subscription,
}

/// Use this function in conjunction with [App::set_prompt_builder] to force
/// GPUI to always use the fallback prompt renderer.
pub fn fallback_prompt_renderer(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let renderer = cx.new(|cx| FallbackPromptRenderer {
        level,
        message: message.to_string(),
        detail: detail.map(ToString::to_string),
        actions: actions
            .iter()
            .map(|button| (button.clone(), cx.focus_handle()))
            .collect(),
        focus: cx.focus_handle(),
    });

    handle.with_view(renderer, window, cx)
}

/// The default GPUI fallback for rendering prompts, when the platform doesn't support it.
pub struct FallbackPromptRenderer {
    level: PromptLevel,
    message: String,
    detail: Option<String>,
    actions: Vec<(PromptButton, FocusHandle)>,
    focus: FocusHandle,
}

impl Render for FallbackPromptRenderer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompt = div()
            .id("window-prompt")
            .role(match self.level {
                PromptLevel::Info => accesskit::Role::Dialog,
                PromptLevel::Warning | PromptLevel::Critical => accesskit::Role::AlertDialog,
            })
            .aria_modal(true)
            .aria_label(self.message.clone());
        let prompt = match &self.detail {
            Some(detail) => prompt.aria_description(detail.clone()),
            None => prompt,
        };
        let prompt = prompt
            .cursor_default()
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|prompt, event: &crate::KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    let modifiers = event.keystroke.modifiers;
                    let focused = prompt
                        .actions
                        .iter()
                        .position(|(_, focus)| focus.is_focused(window));
                    if key == "tab" && modifiers.number_of_modifiers() == u8::from(modifiers.shift)
                    {
                        let len = prompt.actions.len();
                        if len > 0 {
                            let next = match (focused, modifiers.shift) {
                                (None | Some(0), true) => len - 1,
                                (Some(index), true) => index - 1,
                                (Some(index), false) => (index + 1) % len,
                                (None, false) => 0,
                            };
                            window.focus(&prompt.actions[next].1, cx);
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                        return;
                    }
                    if event.is_held || modifiers.modified() {
                        return;
                    }
                    let answer = match key {
                        "enter" => focused.or_else(|| {
                            prompt
                                .actions
                                .iter()
                                .position(|(button, _)| matches!(button, PromptButton::Ok(_)))
                        }),
                        "space" => focused,
                        "escape" => prompt
                            .actions
                            .iter()
                            .position(|(button, _)| button.is_cancel()),
                        _ => None,
                    };
                    if let Some(index) = answer {
                        cx.emit(PromptResponse(index));
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
            .w_72()
            .bg(white())
            .rounded_lg()
            .overflow_hidden()
            .p_3()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .justify_around()
                    .child(div().overflow_hidden().child(self.message.clone())),
            )
            .children(self.detail.clone().map(|detail| {
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .justify_around()
                    .text_sm()
                    .mb_2()
                    .child(div().child(detail))
            }))
            .children(
                self.actions
                    .iter()
                    .enumerate()
                    .map(|(ix, (action, focus))| {
                        div()
                            .flex()
                            .flex_row()
                            .justify_around()
                            .border_1()
                            .border_color(opaque_grey(0.2, 0.5))
                            .mt_1()
                            .rounded_xs()
                            .cursor_pointer()
                            .text_sm()
                            .child(action.label().clone())
                            .id(ix)
                            .track_focus(focus)
                            .focus_visible(|style| {
                                style
                                    .border_color(crate::rgb(0x205cb0))
                                    .bg(crate::rgb(0xe5efff))
                            })
                            .role(accesskit::Role::Button)
                            .aria_label(action.label().clone())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PromptResponse(ix));
                                cx.stop_propagation();
                            }))
                    }),
            );

        div()
            .size_full()
            .occlude()
            .child(
                div()
                    .size_full()
                    .bg(opaque_grey(0.5, 0.6))
                    .absolute()
                    .top_0()
                    .left_0(),
            )
            .child(
                div()
                    .size_full()
                    .absolute()
                    .top_0()
                    .left_0()
                    .flex()
                    .flex_col()
                    .justify_around()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .justify_around()
                            .child(prompt),
                    ),
            )
    }
}

impl EventEmitter<PromptResponse> for FallbackPromptRenderer {}

impl Focusable for FallbackPromptRenderer {
    fn focus_handle(&self, _: &crate::App) -> FocusHandle {
        self.focus.clone()
    }
}

pub(crate) trait PromptViewHandle {
    fn any_view(&self) -> AnyView;
}

impl<V: Prompt + Render> PromptViewHandle for Entity<V> {
    fn any_view(&self) -> AnyView {
        self.clone().into()
    }
}

pub(crate) enum PromptBuilder {
    Default,
    Custom(
        Box<
            dyn Fn(
                PromptLevel,
                &str,
                Option<&str>,
                &[PromptButton],
                PromptHandle,
                &mut Window,
                &mut App,
            ) -> RenderablePromptHandle,
        >,
    ),
}

impl Deref for PromptBuilder {
    type Target = dyn Fn(
        PromptLevel,
        &str,
        Option<&str>,
        &[PromptButton],
        PromptHandle,
        &mut Window,
        &mut App,
    ) -> RenderablePromptHandle;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Default => &fallback_prompt_renderer,
            Self::Custom(f) => f.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Empty, TestAppContext};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn embedding_presenter_observes_only_its_windows_live_tree() {
        struct Button(&'static str);
        impl Render for Button {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .id("button")
                    .role(accesskit::Role::Button)
                    .aria_label(self.0)
            }
        }
        let mut test = TestAppContext::single();
        let host: crate::AnyWindowHandle = test.add_window(|_, _| Button("Exported button")).into();
        let peer: crate::AnyWindowHandle = test.add_window(|_, _| Button("Private button")).into();
        let seen = Rc::new(RefCell::new(Vec::new()));
        host.update(&mut test, |_, window, cx| {
            let seen = seen.clone();
            window.observe_hosted_accessibility(move |tree, _| {
                let buttons: Vec<_> = tree
                    .nodes
                    .iter()
                    .filter(|(_, node)| node.role() == accesskit::Role::Button)
                    .filter_map(|(_, node)| node.label().map(str::to_owned))
                    .collect();
                seen.borrow_mut().push(buttons);
            });
            window.draw(cx).clear(cx);
        })
        .unwrap();
        assert_eq!(seen.borrow().last().unwrap(), &["Exported button"]);
        peer.update(&mut test, |_, window, cx| {
            window.draw(cx).clear(cx);
            assert!(!window.is_a11y_active());
        })
        .unwrap();
        let count = seen.borrow().len();
        host.update(&mut test, |_, window, cx| {
            window.stop_observing_hosted_accessibility();
            window.draw(cx).clear(cx);
            assert!(!window.is_a11y_active());
        })
        .unwrap();
        assert!(test.simulate_accessibility_activation(host, true));
        host.update(&mut test, |_, window, cx| {
            window.draw(cx).clear(cx);
            assert!(window.is_a11y_active());
        })
        .unwrap();
        assert_eq!(seen.borrow().len(), count);
    }

    #[test]
    fn accessible_dialog_is_modal_and_only_live_answers_can_be_activated() {
        use accesskit::{Action, NodeId, Role};
        struct Background;
        impl Render for Background {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .id("underlay")
                    .role(Role::Button)
                    .aria_label("Underlying action")
            }
        }
        let mut test = TestAppContext::single();
        let handle: crate::AnyWindowHandle = test.add_window(|_, _| Background).into();
        let peer = test.add_window(|_, _| Background).into();
        test.update(|cx| cx.set_prompt_builder(fallback_prompt_renderer));
        assert!(test.simulate_accessibility_activation(handle, true));
        assert!(test.simulate_accessibility_activation(peer, true));
        test.run_until_parked();
        let tree =
            |handle: crate::AnyWindowHandle, test: &mut TestAppContext| -> serde_json::Value {
                handle
                    .update(test, |_, window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                        serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap()
                    })
                    .unwrap()
            };
        let node = |tree: &serde_json::Value, label: &str, role: &str| {
            tree["nodes"]
                .as_object()
                .unwrap()
                .values()
                .find(|node| node["aria"]["label"] == label && node["aria"]["role"] == role)
                .map(|node| NodeId(node["accesskit_id"].as_str().unwrap().parse().unwrap()))
        };
        assert!(node(&tree(handle, &mut test), "Underlying action", "Button").is_some());
        let open = |test: &mut TestAppContext, message| {
            handle
                .update(test, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        message,
                        Some("Review your changes"),
                        &["Cancel", "Save a copy", "OK"],
                        cx,
                    )
                })
                .unwrap()
        };
        let peer_id = node(&tree(peer, &mut test), "Underlying action", "Button").unwrap();
        let mut old = open(&mut test, "First question");
        let first = tree(handle, &mut test);
        let dialog = node(&first, "First question", "AlertDialog").expect("missing warning dialog");
        let focused = &first["nodes"][first["gpui_focus"].as_str().unwrap()];
        assert_eq!(focused["accesskit_id"], dialog.0.to_string());
        assert_eq!(focused["aria"]["description"], "Review your changes");
        assert_eq!(focused["aria"]["modal"], true);
        assert!(node(&first, "Underlying action", "Button").is_none());
        assert_eq!(
            node(&tree(peer, &mut test), "Underlying action", "Button"),
            Some(peer_id)
        );
        let retired = node(&first, "Save a copy", "Button").unwrap();
        let mut fresh = open(&mut test, "Second question");
        assert!(old.try_recv().is_err());
        let second = tree(handle, &mut test);
        let live = node(&second, "Save a copy", "Button").unwrap();
        assert_ne!(live, retired);
        assert!(test.simulate_accessibility_action(handle, dialog, Action::Blur));
        test.run_until_parked();
        let surviving_focus = tree(handle, &mut test);
        let focus = &surviving_focus["nodes"][surviving_focus["gpui_focus"].as_str().unwrap()];
        assert_eq!(focus["aria"]["label"], "Second question");
        assert!(test.simulate_accessibility_action(handle, retired, Action::Click));
        test.run_until_parked();
        assert_eq!(fresh.try_recv().unwrap(), None);
        assert!(test.simulate_accessibility_action(handle, live, Action::Click));
        test.run_until_parked();
        assert_eq!(fresh.try_recv().unwrap(), Some(1));
        let mut keyboard = open(&mut test, "Keyboard question");
        tree(handle, &mut test);
        for key in ["shift-tab", "tab", "tab"] {
            test.simulate_keystrokes(handle, key);
            tree(handle, &mut test);
        }
        test.simulate_keystrokes(handle, "space");
        assert_eq!(keyboard.try_recv().unwrap(), Some(1));
        let restored = tree(handle, &mut test);
        assert!(node(&restored, "Underlying action", "Button").is_some());
        assert!(node(&restored, "Second question", "AlertDialog").is_none());
    }

    #[test]
    fn replaced_dialog_cancels_its_answer_and_restores_the_original_focus() {
        let mut test = TestAppContext::single();
        let owner = test.add_window(|_, _| Empty);
        let peer = test.add_window(|_, _| Empty);
        let views = Rc::new(RefCell::new(Vec::new()));
        test.update(|cx| {
            let views = views.clone();
            cx.set_prompt_builder(move |level, message, _, buttons, handle, window, cx| {
                let view = cx.new(|cx| FallbackPromptRenderer {
                    level,
                    message: message.to_owned(),
                    detail: None,
                    actions: buttons
                        .iter()
                        .map(|button| (button.clone(), cx.focus_handle()))
                        .collect(),
                    focus: cx.focus_handle(),
                });
                views.borrow_mut().push(view.clone());
                handle.with_view(view, window, cx)
            });
        });
        let (initial_focus, mut old) = owner
            .update(&mut test, |_, window, cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                (
                    focus,
                    window.prompt(PromptLevel::Info, "old", None, &["OK", "Cancel"], cx),
                )
            })
            .unwrap();
        let mut other = peer
            .update(&mut test, |_, window, cx| {
                window.prompt(PromptLevel::Info, "peer", None, &["OK", "Cancel"], cx)
            })
            .unwrap();
        let mut fresh = owner
            .update(&mut test, |_, window, cx| {
                window.prompt(PromptLevel::Info, "fresh", None, &["OK", "Cancel"], cx)
            })
            .unwrap();

        assert!(old.try_recv().is_err(), "replaced prompt still waiting");
        let views = views.borrow();
        views[0].update(&mut test, |_, cx| cx.emit(PromptResponse(1)));
        assert_eq!(fresh.try_recv().unwrap(), None);
        assert_eq!(other.try_recv().unwrap(), None);
        owner
            .update(&mut test, |_, window, cx| {
                assert!(window.has_active_prompt());
                assert!(views[2].focus_handle(cx).is_focused(window));
            })
            .unwrap();
        views[2].update(&mut test, |_, cx| cx.emit(PromptResponse(0)));
        assert_eq!(fresh.try_recv().unwrap(), Some(0));
        owner
            .update(&mut test, |_, window, _| {
                assert!(!window.has_active_prompt());
                assert!(initial_focus.is_focused(window));
            })
            .unwrap();
        assert_eq!(other.try_recv().unwrap(), None);
        views[1].update(&mut test, |_, cx| cx.emit(PromptResponse(1)));
        assert_eq!(other.try_recv().unwrap(), Some(1));
    }
}
