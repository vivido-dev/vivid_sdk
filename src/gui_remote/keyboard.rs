//! Producer-owned typematic policy for authenticated physical transitions.

// Rust guideline compliant 2026-10-07
use serde::{Deserialize, Serialize};
use std::{io, time::Duration};

pub(super) const ENV: &str = "VIVID_GUI_KEY_REPEAT";

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

    /// Installs this policy in one child's environment without changing the parent.
    pub fn configure(&self, command: &mut std::process::Command) {
        command.env(
            ENV,
            format!(
                "{{\"delay_ms\":{},\"interval_ms\":{}}}",
                self.delay_ms, self.interval_ms
            ),
        );
    }

    pub(super) fn from_env() -> io::Result<Self> {
        match std::env::var(ENV) {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => Err(invalid()),
        }
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
    key: Option<(u16, Duration)>,
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
            if self.key.is_some_and(|(key, _)| key == usage) {
                self.key = None;
            }
        } else if repeatable(usage) {
            self.key = policy
                .delay()
                .and_then(|delay| now.checked_add(delay))
                .map(|deadline| (usage, deadline));
        }
    }

    pub(super) fn take_due(&mut self, now: Duration, policy: KeyRepeat) -> Option<u16> {
        let (usage, deadline) = self.key?;
        if now < deadline {
            return None;
        }
        self.key = now
            .checked_add(policy.interval())
            .map(|deadline| (usage, deadline));
        Some(usage)
    }
}

fn repeatable(usage: u16) -> bool {
    // Printable keys, editing/navigation keys and function keys. Lock keys and
    // modifiers never repeat; a lock-key press must not disarm a held letter.
    matches!(usage, 4..=56 | 58..=69 | 73..=82)
}

#[cfg(test)]
mod tests {
    use super::*;

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
