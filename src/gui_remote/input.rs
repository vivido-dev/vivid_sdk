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
}
impl Default for Translator {
    fn default() -> Self {
        Self {
            held: [false; 232],
            buttons: [false; 5],
            position: point(px(0.), px(0.)),
            reset: 0,
        }
    }
}

impl Translator {
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
        if self.reset != injection.reset {
            self.release(window, cx);
            self.reset = injection.reset;
        }
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
}
