//! Producer-owned typematic policy for authenticated physical transitions.

// Rust guideline compliant 2026-10-07
use serde::{Deserialize, Serialize};
use std::{io, time::Duration};

pub(super) const ENV: &str = "VIVID_GUI_KEY_REPEAT";

const FOLLOW_SYSTEM: &str = "VIVID_GUI_FOLLOW_SYSTEM_REPEAT";
const LAYOUT_ENV: &str = "VIVID_GUI_CHARACTER_LAYOUT";
pub(super) const MAX_LAYOUT_BYTES: usize = 32 * 1024;

/// One host layout sampled for physical printable keys at raster process launch.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterLayout {
    id: String,
    keys: Vec<crate::gui::KeyboardCharacterKey>,
}

impl CharacterLayout {
    /// Captures a bounded layout if the platform exports physical character mappings.
    pub fn sample(layout: &dyn crate::gui::PlatformKeyboardLayout) -> Option<Self> {
        let map = Self {
            id: layout.id().to_owned(),
            keys: layout.character_keys(),
        };
        map.encode().ok()?;
        Some(map)
    }
    fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 256
            && !self.id.chars().any(char::is_control)
            && (1..=128).contains(&self.keys.len())
            && self
                .keys
                .windows(2)
                .all(|pair| pair[0].usage < pair[1].usage)
            && self.keys.iter().all(|key| {
                matches!(key.usage, 4..=39 | 45..=49 | 51..=56 | 84..=87 | 89..=100 | 103 | 133 | 135 | 137)
                    && key
                        .layers
                        .iter().chain(key.caps_lock_layers.iter().flatten())
                        .all(|text| text.len() <= 32 && !text.chars().any(char::is_control))
            })
    }
    /// Installs this layout in the child's launch environment.
    ///
    /// # Errors
    /// Rejects excessive, malformed or ambiguous mappings.
    pub fn configure(&self, command: &mut std::process::Command) -> io::Result<()> {
        command.env(LAYOUT_ENV, self.encode()?);
        Ok(())
    }
    pub(super) fn encode(&self) -> io::Result<String> {
        if !self.valid() {
            return Err(invalid_layout());
        }
        let value = serde_json::to_string(self)?;
        if value.len() > MAX_LAYOUT_BYTES {
            return Err(invalid_layout());
        }
        Ok(value)
    }
    pub(super) fn from_env() -> io::Result<Option<Self>> {
        match std::env::var(LAYOUT_ENV) {
            Ok(value) => Self::parse(&value).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(invalid_layout()),
        }
    }
    fn parse(value: &str) -> io::Result<Self> {
        if value.len() > MAX_LAYOUT_BYTES {
            return Err(invalid_layout());
        }
        let map: Self = serde_json::from_str(value).map_err(|_| invalid_layout())?;
        if !map.valid() {
            return Err(invalid_layout());
        }
        Ok(map)
    }
    pub(super) fn keystroke(
        &self,
        usage: u16,
        mut modifiers: crate::gui::Modifiers,
        caps_lock: bool,
    ) -> Option<crate::gui::Keystroke> {
        let index = self
            .keys
            .binary_search_by_key(&usage, |key| key.usage)
            .ok()?;
        let [base, shift, _, _, cmd, cmd_shift] = &self.keys[index].layers;
        let command_layout = modifiers.platform
            || self.keys.first().is_some_and(|key| {
                key.usage == 4 && !key.layers[0].is_ascii() && key.layers[4].is_ascii()
            });
        let key_char = if modifiers.control || modifiers.platform || modifiers.function {
            None
        } else {
            let layer = usize::from(modifiers.alt) * 2 + usize::from(modifiers.shift);
            let text = if caps_lock && let Some(locked) = &self.keys[index].caps_lock_layers {
                &locked[layer]
            } else {
                &self.keys[index].layers[layer]
            };
            (!text.is_empty()).then(|| text.clone())
        };
        let logical = if command_layout { cmd } else { base };
        let key = if modifiers.shift && !logical.chars().all(|c| c.is_ascii_lowercase()) {
            modifiers.shift = false;
            if command_layout && cmd_shift != cmd {
                cmd_shift.clone()
            } else {
                shift.clone()
            }
        } else {
            logical.clone()
        };
        Some(crate::gui::Keystroke {
            key,
            key_char,
            modifiers,
        })
    }
}
fn invalid_layout() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid remote character layout",
    )
}

/// Bounded repeat policy for one remote application's keyboard.
///
/// The presenter sends physical transitions only. The producer synthesizes at
/// most one repeat per GUI turn, never catching up a stalled event loop. Timing
/// is consequently limited by the remote GUI's frame cadence. Zero for both
/// values disables repeat. This policy does not select a keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Parameters")]
pub struct KeyRepeat {
    delay_ms: u32,
    interval_ms: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameters {
    delay_ms: u32,
    interval_ms: u32,
}

impl TryFrom<Parameters> for KeyRepeat {
    type Error = io::Error;

    fn try_from(value: Parameters) -> Result<Self, Self::Error> {
        Self::new(value.delay_ms, value.interval_ms)
    }
}

impl Default for KeyRepeat {
    fn default() -> Self {
        // A conservative desktop policy, independent of the producer's hidden
        // native window and the presenter's suppressed native repeat events.
        Self {
            delay_ms: 500,
            interval_ms: 50,
        }
    }
}

impl KeyRepeat {
    /// Creates a repeat policy using milliseconds.
    ///
    /// # Errors
    /// Rejects delays outside 100–5,000 ms and intervals outside 33–1,000 ms,
    /// unless both values are zero to disable repeat.
    pub fn new(delay_ms: u32, interval_ms: u32) -> io::Result<Self> {
        if (delay_ms == 0 && interval_ms == 0)
            || ((100..=5_000).contains(&delay_ms) && (33..=1_000).contains(&interval_ms))
        {
            Ok(Self {
                delay_ms,
                interval_ms,
            })
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid remote keyboard repeat policy",
            ))
        }
    }

    /// Adapts system timings to the producer's bounded repeat clock. A zero or
    /// excessively long timer disables repeat. Short timers use the supported
    /// minimum, and fractional milliseconds round up.
    pub fn from_system_timing(delay: Duration, interval: Duration) -> Self {
        if delay.is_zero()
            || interval.is_zero()
            || delay > Duration::from_secs(5)
            || interval > Duration::from_secs(1)
        {
            return Self {
                delay_ms: 0,
                interval_ms: 0,
            };
        }
        let millis = |value: Duration| value.as_nanos().div_ceil(1_000_000) as u32;
        Self {
            delay_ms: millis(delay).max(100),
            interval_ms: millis(interval).max(33),
        }
    }

    /// Installs this policy in one child's environment without changing the parent.
    pub fn configure(&self, command: &mut std::process::Command) {
        command.env_remove(FOLLOW_SYSTEM);
        command.env(
            ENV,
            format!(
                "{{\"delay_ms\":{},\"interval_ms\":{}}}",
                self.delay_ms, self.interval_ms
            ),
        );
    }

    /// Installs the initial timing and asks a local GUI child to follow system updates.
    pub fn configure_system(&self, command: &mut std::process::Command) {
        self.configure(command);
        command.env(FOLLOW_SYSTEM, "1");
    }

    pub(super) fn from_env() -> io::Result<(Self, bool)> {
        let follow = match std::env::var(FOLLOW_SYSTEM) {
            Ok(value) if value == "1" => true,
            Err(std::env::VarError::NotPresent) => false,
            _ => return Err(invalid()),
        };
        let policy = match std::env::var(ENV) {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => Err(invalid()),
        }?;
        Ok((policy, follow))
    }

    fn parse(value: &str) -> io::Result<Self> {
        // The two-number envelope needs fewer than 64 bytes. Bound source input
        // before Serde allocation, allowing modest launch-format whitespace.
        if value.len() > 128 {
            return Err(invalid());
        }
        serde_json::from_str(value).map_err(|_| invalid())
    }

    pub(super) fn delay(self) -> Option<Duration> {
        (self.delay_ms != 0).then(|| Duration::from_millis(u64::from(self.delay_ms)))
    }

    pub(super) fn interval(self) -> Duration {
        Duration::from_millis(u64::from(self.interval_ms))
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid remote keyboard repeat policy",
    )
}

#[derive(Default)]
pub(super) struct Repeating {
    key: Option<u16>,
    deadline: Option<Duration>,
}

impl Repeating {
    pub(super) fn transition(
        &mut self,
        usage: u16,
        pressed: bool,
        now: Duration,
        policy: KeyRepeat,
    ) {
        if !pressed {
            if self.key == Some(usage) {
                *self = Self::default();
            }
        } else if repeatable(usage) {
            self.key = Some(usage);
            self.arm(now, policy);
        }
    }

    pub(super) fn rearm(&mut self, now: Duration, policy: KeyRepeat) {
        if self.key.is_some() {
            self.arm(now, policy);
        }
    }

    fn arm(&mut self, now: Duration, policy: KeyRepeat) {
        self.deadline = policy.delay().and_then(|delay| now.checked_add(delay));
    }

    pub(super) fn refresh(
        &mut self,
        now: Duration,
        next_sample: &mut Option<Duration>,
        policy: &mut KeyRepeat,
        sample: impl FnOnce() -> Option<(Duration, Duration)>,
    ) {
        if self.key.is_some() && next_sample.is_some_and(|deadline| now >= deadline) {
            *next_sample = now.checked_add(Duration::from_millis(250));
            if let Some((delay, interval)) = sample() {
                let observed = KeyRepeat::from_system_timing(delay, interval);
                if observed != *policy {
                    *policy = observed;
                    self.arm(now, observed);
                }
            }
        }
    }

    pub(super) fn take_due(&mut self, now: Duration, policy: KeyRepeat) -> Option<u16> {
        let usage = self.key?;
        if now < self.deadline? {
            return None;
        }
        self.deadline = now.checked_add(policy.interval());
        Some(usage)
    }
}

fn repeatable(usage: u16) -> bool {
    // Printable keys, editing/navigation keys and function keys. Lock keys and
    // modifiers never repeat; a lock-key press must not disarm a held letter.
    matches!(usage, 4..=56 | 58..=69 | 73..=82 | 84..=87 | 89..=100 | 103 | 133 | 135 | 137)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_shortcuts_text_and_physical_modifiers_follow_separate_layers() {
        use crate::gui::Modifiers;
        let map = CharacterLayout::parse(r#"{"id":"fixture","keys":[{"usage":4,"layers":["ф","Ф","œ","Œ","a","A"]},{"usage":30,"layers":["&","1","¡","⁄","&","1"]}]}"#).unwrap();
        let plain = map.keystroke(4, Modifiers::default(), false).unwrap();
        assert_eq!(plain.key, "a");
        assert_eq!(plain.key_char.as_deref(), Some("ф"));
        let option = map
            .keystroke(
                4,
                Modifiers {
                    alt: true,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert_eq!(option.key_char.as_deref(), Some("œ"));
        let chord = map
            .keystroke(
                4,
                Modifiers {
                    platform: true,
                    shift: true,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert_eq!(chord.key, "a");
        assert!(chord.modifiers.shift && chord.key_char.is_none());
        let shifted = map
            .keystroke(
                30,
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert_eq!(shifted.key_char.as_deref(), Some("1"));
        assert_eq!(shifted.key, "1");
        assert!(!shifted.modifiers.shift);
        assert!(map.keystroke(232, Modifiers::default(), false).is_none());
    }

    #[test]
    fn caps_lock_uses_native_text_layers_and_preserves_shortcuts() {
        use crate::gui::Modifiers;
        let map = CharacterLayout::parse(r#"{"id":"fixture","keys":[{"usage":30,"layers":["&","1","¡","⁄","&","1"],"caps_lock_layers":["1","&","π","Ω"]}]}"#).unwrap();
        for (shift, alt, expected) in [
            (false, false, "1"),
            (true, false, "&"),
            (false, true, "π"),
            (true, true, "Ω"),
        ] {
            let mods = Modifiers {
                shift,
                alt,
                ..Default::default()
            };
            let plain = map.keystroke(30, mods, false).unwrap();
            let locked = map.keystroke(30, mods, true).unwrap();
            assert_eq!(locked.key_char.as_deref(), Some(expected));
            assert_eq!(locked.key, plain.key);
            assert_eq!(locked.modifiers, plain.modifiers);
        }
        assert!(
            map.keystroke(
                30,
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                true
            )
            .unwrap()
            .key_char
            .is_none()
        );
    }

    #[test]
    fn invalid_and_ambiguous_character_maps_are_rejected() {
        let key = |usage| serde_json::json!({"usage":usage, "layers":["a","A","å","Å","a","A"]});
        for keys in [
            vec![],
            vec![key(224)],
            vec![key(5), key(4)],
            vec![key(4), key(4)],
            vec![
                serde_json::json!({"usage":4,"layers":["a","A","å","Å","a","A"],"caps_lock_layers":["A","A","\n","Å"]}),
            ],
        ] {
            assert!(
                CharacterLayout::parse(&serde_json::json!({"id":"test", "keys":keys}).to_string())
                    .is_err()
            );
        }
        assert!(CharacterLayout::parse(&" ".repeat(MAX_LAYOUT_BYTES + 1)).is_err());
    }

    #[test]
    fn policy_rejects_unbounded_or_ambiguous_launch_values() {
        for value in [
            r#"{"delay_ms":0,"interval_ms":50}"#,
            r#"{"delay_ms":500,"interval_ms":0}"#,
            r#"{"delay_ms":99,"interval_ms":50}"#,
            r#"{"delay_ms":5001,"interval_ms":50}"#,
            r#"{"delay_ms":500,"interval_ms":32}"#,
            r#"{"delay_ms":500,"interval_ms":1001}"#,
            r#"{"delay_ms":500,"interval_ms":50,"unknown":true}"#,
            r#"{"delay_ms":500,"delay_ms":600,"interval_ms":50}"#,
            r#"{"delay_ms":4294967296,"interval_ms":50}"#,
        ] {
            assert!(KeyRepeat::parse(value).is_err(), "{value}");
        }
        assert!(KeyRepeat::parse(&" ".repeat(129)).is_err());
        for policy in [KeyRepeat::default(), KeyRepeat::new(0, 0).unwrap()] {
            assert_eq!(
                KeyRepeat::parse(&serde_json::to_string(&policy).unwrap()).unwrap(),
                policy
            );
        }
    }

    #[test]
    fn system_repeat_preserves_slow_settings_and_bounds_fast_or_disabled_timers() {
        let ms = Duration::from_millis;
        for (delay, interval, expected) in [
            (ms(500), Duration::from_nanos(83_333_333), (500, 84)),
            (ms(40), ms(15), (100, 33)),
            (ms(5_000), ms(1_000), (5_000, 1_000)),
            (Duration::MAX, ms(50), (0, 0)),
            (ms(500), Duration::ZERO, (0, 0)),
            (ms(500), ms(1_001), (0, 0)),
        ] {
            assert_eq!(
                KeyRepeat::from_system_timing(delay, interval),
                KeyRepeat::new(expected.0, expected.1).unwrap()
            );
        }
    }

    #[test]
    fn live_system_timing_rearms_only_on_change_and_only_for_held_keys() {
        let ms = Duration::from_millis;
        let mut repeat = Repeating::default();
        let mut next = Some(Duration::ZERO);
        let mut policy = KeyRepeat::default();
        repeat.refresh(ms(0), &mut next, &mut policy, || panic!("idle"));
        repeat.transition(4, true, ms(0), policy);
        repeat.refresh(ms(0), &mut None, &mut policy, || {
            panic!("explicit override")
        });
        repeat.refresh(ms(0), &mut next, &mut policy, || Some((ms(500), ms(50))));
        repeat.refresh(ms(249), &mut next, &mut policy, || panic!("too soon"));
        repeat.refresh(ms(250), &mut next, &mut policy, || Some((ms(500), ms(50))));
        assert_eq!(repeat.take_due(ms(500), policy), Some(4));
        repeat.refresh(ms(500), &mut next, &mut policy, || Some((ms(0), ms(0))));
        assert_eq!(repeat.take_due(ms(750), policy), None);
        repeat.refresh(ms(750), &mut next, &mut policy, || Some((ms(100), ms(40))));
        assert_eq!(repeat.take_due(ms(849), policy), None);
        assert_eq!(repeat.take_due(ms(850), policy), Some(4));
        assert_eq!(repeat.take_due(ms(889), policy), None);
        assert_eq!(repeat.take_due(ms(890), policy), Some(4));
        repeat.transition(4, false, ms(900), policy);
        repeat.refresh(ms(1000), &mut next, &mut policy, || panic!("released"));
        assert_eq!(repeat.take_due(ms(1000), policy), None);
    }

    #[test]
    fn repeats_are_bounded_and_do_not_catch_up_after_a_stall() {
        let policy = KeyRepeat::default();
        let mut repeat = Repeating::default();
        repeat.transition(4, true, Duration::ZERO, policy);
        assert_eq!(repeat.take_due(Duration::from_millis(499), policy), None);
        assert_eq!(repeat.take_due(Duration::from_millis(500), policy), Some(4));
        assert_eq!(repeat.take_due(Duration::from_secs(10), policy), Some(4));
        assert_eq!(repeat.take_due(Duration::from_secs(10), policy), None);
        repeat.transition(4, false, Duration::from_secs(10), policy);
        assert_eq!(repeat.take_due(Duration::from_secs(11), policy), None);
    }

    #[test]
    fn owners_modifiers_and_replacement_keys_keep_independent_timers() {
        let policy = KeyRepeat::default();
        let mut owner = Repeating::default();
        let mut peer = Repeating::default();
        for repeat in [&mut owner, &mut peer] {
            repeat.transition(4, true, Duration::ZERO, policy);
        }
        owner.transition(5, true, Duration::from_millis(100), policy);
        owner.transition(4, false, Duration::from_millis(200), policy);
        owner.transition(225, true, Duration::from_millis(300), policy);
        owner.transition(57, true, Duration::from_millis(300), policy);
        assert_eq!(owner.take_due(Duration::from_millis(500), policy), None);
        assert_eq!(peer.take_due(Duration::from_millis(500), policy), Some(4));
        assert_eq!(owner.take_due(Duration::from_millis(600), policy), Some(5));
        owner = Repeating::default();
        assert_eq!(owner.take_due(Duration::from_secs(1), policy), None);
        assert_eq!(peer.take_due(Duration::from_secs(1), policy), Some(4));
        owner.transition(4, true, Duration::ZERO, KeyRepeat::new(0, 0).unwrap());
        assert_eq!(owner.take_due(Duration::from_secs(1), policy), None);
    }
}
