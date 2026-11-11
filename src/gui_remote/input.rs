//! Bounded physical input translation into one process-owned GUI window.

// Rust guideline compliant 2026-10-07
use super::*;
use gui::{App, Keystroke, Modifiers, MouseButton, PlatformInput, point, px};

/// Maps GPUI's ASCII key names to USB HID usages for the initial US layout.
///
/// Unsupported names return `None`; callers must not invent physical usages.
pub fn hid_usage(key: &str) -> Option<u16> {
    if key.len() == 1 {
        let byte = key.as_bytes()[0].to_ascii_lowercase();
        if byte.is_ascii_lowercase() {
            return Some(u16::from(byte - b'a') + 4);
        }
        if (b'1'..=b'9').contains(&byte) {
            return Some(u16::from(byte - b'1') + 30);
        }
    }
    Some(match key {
        "0" => 39,
        "enter" => 40,
        "escape" => 41,
        "backspace" => 42,
        "tab" => 43,
        "space" | " " => 44,
        ")" => 39,
        "!" => 30,
        "@" => 31,
        "#" => 32,
        "$" => 33,
        "%" => 34,
        "^" => 35,
        "&" => 36,
        "*" => 37,
        "(" => 38,
        "-" | "_" => 45,
        "=" | "+" => 46,
        "[" | "{" => 47,
        "]" | "}" => 48,
        "\\" | "|" => 49,
        ";" | ":" => 51,
        "'" | "\"" => 52,
        "`" | "~" => 53,
        "," | "<" => 54,
        "." | ">" => 55,
        "/" | "?" => 56,
        "capslock" => 57,
        "insert" => 73,
        "home" => 74,
        "pageup" => 75,
        "delete" => 76,
        "end" => 77,
        "pagedown" => 78,
        "right" => 79,
        "left" => 80,
        "down" => 81,
        "up" => 82,
        _ => {
            return key
                .strip_prefix('f')
                .and_then(|number| number.parse::<u16>().ok())
                .filter(|number| (1..=12).contains(number))
                .map(|number| number + 57);
        }
    })
}

fn shifted(key: &str) -> String {
    // USB usages are interpreted as the US layout until layout negotiation exists.
    match key {
        "1" => "!",
        "2" => "@",
        "3" => "#",
        "4" => "$",
        "5" => "%",
        "6" => "^",
        "7" => "&",
        "8" => "*",
        "9" => "(",
        "0" => ")",
        "-" => "_",
        "=" => "+",
        "[" => "{",
        "]" => "}",
        "\\" => "|",
        ";" => ":",
        "'" => "\"",
        "`" => "~",
        "," => "<",
        "." => ">",
        "/" => "?",
        _ => return key.to_ascii_uppercase(),
    }
    .to_owned()
}

fn key_name(usage: u16) -> Option<String> {
    if (4..=29).contains(&usage) {
        return Some(char::from(b'a' + (usage - 4) as u8).to_string());
    }
    if (30..=38).contains(&usage) {
        return Some(char::from(b'1' + (usage - 30) as u8).to_string());
    }
    if (58..=69).contains(&usage) {
        return Some(format!("f{}", usage - 57));
    }
    [
        "0",
        "enter",
        "escape",
        "backspace",
        "tab",
        "space",
        "-",
        "=",
        "[",
        "]",
        "\\",
        "",
        ";",
        "'",
        "`",
        ",",
        ".",
        "/",
        "capslock",
    ]
    .get(usize::from(usage.checked_sub(39)?))
    .filter(|name| !name.is_empty())
    .map(|name| (*name).to_owned())
    .or_else(|| {
        Some(
            match usage {
                73 => "insert",
                74 => "home",
                75 => "pageup",
                76 => "delete",
                77 => "end",
                78 => "pagedown",
                79 => "right",
                80 => "left",
                81 => "down",
                82 => "up",
                _ => return None,
            }
            .to_owned(),
        )
    })
}

fn button(index: usize) -> MouseButton {
    match index {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::Navigate(gui::NavigationDirection::Back),
        _ => MouseButton::Navigate(gui::NavigationDirection::Forward),
    }
}

pub(super) struct Translator {
    held: [bool; 232],
    buttons: [bool; 5],
    position: gui::Point<gui::Pixels>,
    reset: u64,
    binding: Option<crate::InputTuple>,
    repeat: keyboard::Repeating,
    policy: KeyRepeat,
}
impl Default for Translator {
    fn default() -> Self {
        Self {
            held: [false; 232],
            buttons: [false; 5],
            position: point(px(0.), px(0.)),
            reset: 0,
            binding: None,
            repeat: Default::default(),
            policy: KeyRepeat::default(),
        }
    }
}

impl Translator {
    pub(super) fn new(policy: KeyRepeat) -> Self {
        Self {
            policy,
            ..Default::default()
        }
    }
    fn modifiers(&self) -> Modifiers {
        Modifiers {
            control: self.held[224] || self.held[228],
            shift: self.held[225] || self.held[229],
            alt: self.held[226] || self.held[230],
            platform: self.held[227] || self.held[231],
            function: false,
        }
    }
    fn key(&self, usage: u16) -> Option<Keystroke> {
        let key = key_name(usage)?;
        let modifiers = self.modifiers();
        let key_char = if modifiers.control || modifiers.platform || modifiers.alt {
            None
        } else if key == "space" {
            Some(" ".into())
        } else if key.len() == 1 {
            Some(if modifiers.shift {
                shifted(&key)
            } else {
                key.clone()
            })
        } else {
            None
        };
        Some(Keystroke {
            modifiers,
            key,
            key_char,
        })
    }
    fn release(&mut self, window: &mut Window, cx: &mut App) {
        for usage in 4..224 {
            if self.held[usage]
                && let Some(keystroke) = self.key(usage as u16)
            {
                window.dispatch_cancelled_input(
                    PlatformInput::KeyUp(gui::KeyUpEvent { keystroke }),
                    cx,
                );
            }
        }
        for index in 0..5 {
            if self.buttons[index] {
                window.dispatch_cancelled_input(
                    PlatformInput::MouseUp(gui::MouseUpEvent {
                        button: button(index),
                        position: self.position,
                        modifiers: self.modifiers(),
                        click_count: 1,
                    }),
                    cx,
                );
            }
        }
        self.held.fill(false);
        self.buttons.fill(false);
        self.repeat = Default::default();
        window.dispatch_cancelled_input(
            PlatformInput::ModifiersChanged(gui::ModifiersChangedEvent::default()),
            cx,
        );
    }
    pub(super) fn apply(&mut self, injection: &mut Injection, window: &mut Window, cx: &mut App) {
        if injection.guard.is_expired(injection.now())
            || (injection.guard.grant().is_some()
                && injection
                    .lifecycle
                    .as_ref()
                    .is_none_or(|state| state.closed.load(Ordering::Acquire)))
        {
            injection.revoke();
        }
        if self.reset != injection.reset || self.binding != injection.guard.current_tag() {
            self.release(window, cx);
            self.reset = injection.reset;
            self.binding = injection.guard.current_tag();
        }
        injection.clipboard.refresh(injection);
        for _ in 0..INPUT_CAPACITY {
            if injection
                .lifecycle
                .as_ref()
                .is_none_or(|state| state.closed.load(Ordering::Acquire))
            {
                if injection.guard.grant().is_some() {
                    injection.revoke();
                    self.release(window, cx);
                    self.reset = injection.reset;
                    self.binding = None;
                }
                break;
            }
            let Some(event) = injection.events.pop_front() else {
                break;
            };
            if !injection.guard.is_armed(injection.now())
                || injection.guard.current_tag() != Some(event.binding())
                || injection
                    .guard
                    .grant()
                    .is_none_or(|grant| grant.effective_classes & event.class() == 0)
            {
                continue;
            }
            match event {
                InputEvent::Key { usage, pressed, .. } => {
                    let held = &mut self.held[usize::from(usage)];
                    if *held == pressed {
                        continue;
                    }
                    *held = pressed;
                    self.repeat
                        .transition(usage, pressed, injection.origin.elapsed(), self.policy);
                    if usage >= 224 {
                        window.dispatch_event(
                            PlatformInput::ModifiersChanged(gui::ModifiersChangedEvent {
                                modifiers: self.modifiers(),
                                ..Default::default()
                            }),
                            cx,
                        );
                    } else if let Some(keystroke) = self.key(usage) {
                        if pressed {
                            window.dispatch_keystroke(keystroke, cx);
                        } else {
                            window.dispatch_event(
                                PlatformInput::KeyUp(gui::KeyUpEvent { keystroke }),
                                cx,
                            );
                        }
                    }
                }
                InputEvent::PointerMotion { x, y, .. } => {
                    self.position = point(
                        px((x as f64 / 4_294_967_296.) as f32),
                        px((y as f64 / 4_294_967_296.) as f32),
                    );
                    window.dispatch_event(
                        PlatformInput::MouseMove(gui::MouseMoveEvent {
                            position: self.position,
                            pressed_button: self.buttons.iter().position(|held| *held).map(button),
                            modifiers: self.modifiers(),
                        }),
                        cx,
                    );
                }
                InputEvent::PointerButton {
                    button: index,
                    pressed,
                    x,
                    y,
                    ..
                } => {
                    self.position = point(
                        px((x as f64 / 4_294_967_296.) as f32),
                        px((y as f64 / 4_294_967_296.) as f32),
                    );
                    if self.buttons[usize::from(index)] == pressed {
                        continue;
                    }
                    self.buttons[usize::from(index)] = pressed;
                    let event = if pressed {
                        PlatformInput::MouseDown(gui::MouseDownEvent {
                            button: button(usize::from(index)),
                            position: self.position,
                            modifiers: self.modifiers(),
                            click_count: 1,
                            first_mouse: false,
                        })
                    } else {
                        PlatformInput::MouseUp(gui::MouseUpEvent {
                            button: button(usize::from(index)),
                            position: self.position,
                            modifiers: self.modifiers(),
                            click_count: 1,
                        })
                    };
                    window.dispatch_event(event, cx);
                }
                InputEvent::PointerAxis {
                    horizontal,
                    vertical,
                    x,
                    y,
                    ..
                } => {
                    self.position = point(
                        px((x as f64 / 4_294_967_296.) as f32),
                        px((y as f64 / 4_294_967_296.) as f32),
                    );
                    window.dispatch_event(
                        PlatformInput::Scroll(gui::ScrollEvent {
                            position: self.position,
                            delta: gui::ScrollDelta::Lines(gui::point(
                                horizontal as f32 / 120.,
                                vertical as f32 / 120.,
                            )),
                            modifiers: self.modifiers(),
                            ..Default::default()
                        }),
                        cx,
                    );
                }
                _ => injection.revoke(),
            }
        }
        if self.reset != injection.reset {
            self.release(window, cx);
            self.reset = injection.reset;
            self.binding = None;
        }
        // Repeat is new input, so validate its authority at the actual dispatch
        // boundary even in idle turns without incoming physical reports.
        if injection.guard.is_armed(injection.now())
            && injection
                .guard
                .grant()
                .is_some_and(|grant| grant.effective_classes & crate::INPUT_CLASS_KEYBOARD != 0)
            && injection
                .lifecycle
                .as_ref()
                .is_some_and(|state| !state.closed.load(Ordering::Acquire))
            && let Some(usage) = self
                .repeat
                .take_due(injection.origin.elapsed(), self.policy)
            && self.held[usize::from(usage)]
            && let Some(key) = self.key(usage)
        {
            window.dispatch_keystroke_with_repeat(key, true, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_physical_mapping_preserves_shifted_punctuation_and_letters() {
        for usage in 4..=56 {
            if let Some(name) = key_name(usage) {
                assert_eq!(hid_usage(&name), Some(usage));
                if name.len() == 1 {
                    assert_eq!(hid_usage(&shifted(&name)), Some(usage));
                }
            }
        }
        let mut translator = Translator::default();
        translator.held[225] = true;
        assert_eq!(translator.key(30).unwrap().key_char.as_deref(), Some("!"));
        assert_eq!(translator.key(4).unwrap().key_char.as_deref(), Some("A"));
        translator.held[224] = true;
        assert_eq!(translator.key(4).unwrap().key_char, None);
        for unsupported in ["é", "😀", "f13", "f0", "unknown"] {
            assert_eq!(hid_usage(unsupported), None);
        }
    }

    #[cfg(feature = "gui-testing")]
    #[test]
    fn repeat_dispatch_checks_watchdog_and_owner_lifecycle_with_reused_ids() {
        use gui::{
            AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement, Render,
            Styled as _, TestAppContext,
        };
        use std::{cell::RefCell, rc::Rc};
        use vivid_protocol::{revision::SurfaceGeneration, time::Monotonic};

        #[derive(Default)]
        struct Reports {
            down: Vec<(String, bool)>,
            up: Vec<bool>,
        }
        struct View {
            focus: FocusHandle,
            reports: Rc<RefCell<Reports>>,
        }
        impl Render for View {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                gui::div()
                    .size_full()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(|view, event: &gui::KeyDownEvent, _, _| {
                        view.reports
                            .borrow_mut()
                            .down
                            .push((event.keystroke.key.clone(), event.is_held));
                    }))
                    .on_key_up(cx.listener(|view, _: &gui::KeyUpEvent, window, _| {
                        view.reports
                            .borrow_mut()
                            .up
                            .push(window.is_input_cancelled());
                    }))
            }
        }

        fn injection() -> Injection {
            let mut input = Shared::new().injection.into_inner().unwrap();
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
            input.events.push_back(InputEvent::Key {
                binding: input.guard.current_tag().unwrap(),
                usage: 4,
                pressed: true,
            });
            input
        }

        let mut cx = TestAppContext::single();
        let reports = [
            Rc::new(RefCell::new(Reports::default())),
            Rc::new(RefCell::new(Reports::default())),
        ];
        let windows = reports.each_ref().map(|reports| {
            cx.add_window(|window, cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                View {
                    focus,
                    reports: reports.clone(),
                }
            })
        });
        let mut inputs = [injection(), injection()];
        assert_eq!(inputs[0].guard.current_tag(), inputs[1].guard.current_tag());
        let mut translators = [Translator::default(), Translator::default()];
        for index in 0..2 {
            cx.update_window(windows[index].into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                translators[index].apply(&mut inputs[index], window, cx);
                inputs[index].origin = Instant::now() - Duration::from_millis(600);
                translators[index].apply(&mut inputs[index], window, cx);
            })
            .unwrap();
            assert_eq!(
                reports[index].borrow().down,
                [("a".into(), false), ("a".into(), true)]
            );
        }

        // Disconnect without a worker-side reset: final GUI admission must still
        // release the original owner's held state and disarm its local timer.
        inputs[0]
            .lifecycle
            .as_ref()
            .unwrap()
            .closed
            .store(true, Ordering::Release);
        inputs[0].origin = Instant::now() - Duration::from_secs(1);
        inputs[1].origin = Instant::now() - Duration::from_secs(1);
        for index in 0..2 {
            cx.update_window(windows[index].into(), |_, window, cx| {
                translators[index].apply(&mut inputs[index], window, cx);
            })
            .unwrap();
        }
        assert_eq!(reports[0].borrow().down.len(), 2);
        assert_eq!(reports[0].borrow().up, [true]);
        assert_eq!(reports[1].borrow().down.len(), 3);
        assert!(reports[1].borrow().up.is_empty());

        // Expiration is checked on an idle GUI turn, without any new lane event.
        inputs[1].origin = Instant::now() - Duration::from_secs(3);
        cx.update_window(windows[1].into(), |_, window, cx| {
            translators[1].apply(&mut inputs[1], window, cx);
        })
        .unwrap();
        assert_eq!(reports[1].borrow().down.len(), 3);
        assert_eq!(reports[1].borrow().up, [true]);
    }
}
