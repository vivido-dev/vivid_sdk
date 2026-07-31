//! Scripted faults the test presenter injects.
//!
//! Every recovery flow a producer implements exists because something went wrong on the wire, so
//! the only honest way to test one is to make that thing go wrong. A script is a set of one-shot
//! or standing instructions the presenter consults as it serves; nothing here is timing-dependent,
//! so a test that arms a fault gets it deterministically.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A fault the presenter applies while serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Swallow the next `times` replies of this record type, leaving the request unanswered.
    ///
    /// The request is still processed and still recorded: a dropped reply is a lost *reply*, and a
    /// producer that retries must find the presenter already holding the outcome.
    DropReply { record_type: u16, times: u32 },
    /// Close a track's transport after it has accepted `media_records` records.
    CloseTrackTransportAfter { track_id: u64, media_records: u64 },
    /// Hold this record type back for a while before writing it.
    Delay {
        record_type: u16,
        duration: Duration,
    },
    /// Never raise a track's flow window, so a producer's sender blocks against the initial grant.
    StallFlow { track_id: u64 },
    /// Narrow every input grant to exclude these classes.
    DenyInputClasses { classes: u64 },
    /// Refuse to advance the target beyond this generation.
    FreezeTarget,
}

/// The mutable fault state one presenter consults.
#[derive(Debug, Default)]
pub(crate) struct ScriptState {
    /// Remaining drops per record type.
    drops: HashMap<u16, u32>,
    delays: HashMap<u16, Duration>,
    close_after: HashMap<u64, u64>,
    stalled: Vec<u64>,
    denied_classes: u64,
    frozen_target: bool,
}

impl ScriptState {
    /// Whether this reply should be swallowed, consuming one armed drop.
    pub(crate) fn take_drop(&mut self, record_type: u16) -> bool {
        match self.drops.get_mut(&record_type) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn delay_for(&self, record_type: u16) -> Option<Duration> {
        self.delays.get(&record_type).copied()
    }

    /// The record count after which this track's transport closes, if armed.
    pub(crate) fn close_after(&self, track_id: u64) -> Option<u64> {
        self.close_after.get(&track_id).copied()
    }

    pub(crate) fn flow_stalled(&self, track_id: u64) -> bool {
        self.stalled.contains(&track_id)
    }

    pub(crate) fn denied_classes(&self) -> u64 {
        self.denied_classes
    }

    pub(crate) fn target_frozen(&self) -> bool {
        self.frozen_target
    }

    pub(crate) fn arm(&mut self, fault: Fault) {
        match fault {
            Fault::DropReply { record_type, times } => {
                *self.drops.entry(record_type).or_default() += times;
            }
            Fault::CloseTrackTransportAfter {
                track_id,
                media_records,
            } => {
                self.close_after.insert(track_id, media_records);
            }
            Fault::Delay {
                record_type,
                duration,
            } => {
                self.delays.insert(record_type, duration);
            }
            Fault::StallFlow { track_id } => {
                if !self.stalled.contains(&track_id) {
                    self.stalled.push(track_id);
                }
            }
            Fault::DenyInputClasses { classes } => self.denied_classes |= classes,
            Fault::FreezeTarget => self.frozen_target = true,
        }
    }
}

/// A handle a test uses to arm faults, shared with the serving threads.
#[derive(Clone, Debug, Default)]
pub struct Script {
    pub(crate) state: Arc<Mutex<ScriptState>>,
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm a fault. Faults compose: arming two drops of the same record type drops twice.
    pub fn arm(&self, fault: Fault) -> &Self {
        self.state.lock().expect("script").arm(fault);
        self
    }

    /// Swallow the next reply of this type.
    pub fn drop_reply(&self, record_type: u16, times: u32) -> &Self {
        self.arm(Fault::DropReply { record_type, times })
    }

    /// Close a track's transport after it accepts this many media records.
    pub fn close_track_transport_after(&self, track_id: u64, media_records: u64) -> &Self {
        self.arm(Fault::CloseTrackTransportAfter {
            track_id,
            media_records,
        })
    }

    /// Delay a record type before it is written.
    pub fn delay(&self, record_type: u16, duration: Duration) -> &Self {
        self.arm(Fault::Delay {
            record_type,
            duration,
        })
    }

    /// Never raise this track's flow window.
    pub fn stall_flow(&self, track_id: u64) -> &Self {
        self.arm(Fault::StallFlow { track_id })
    }

    /// Refuse these input classes in every grant.
    pub fn deny_input_classes(&self, classes: u64) -> &Self {
        self.arm(Fault::DenyInputClasses { classes })
    }

    /// Refuse to advance the target generation.
    pub fn freeze_target(&self) -> &Self {
        self.arm(Fault::FreezeTarget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_reply_is_consumed_exactly_as_many_times_as_armed() {
        let script = Script::new();
        script.drop_reply(0x0101, 2);
        let mut state = script.state.lock().unwrap();
        assert!(state.take_drop(0x0101));
        assert!(state.take_drop(0x0101));
        assert!(!state.take_drop(0x0101), "the third reply is delivered");
        assert!(!state.take_drop(0x0102), "an unarmed type is never dropped");
    }

    #[test]
    fn arming_the_same_drop_twice_accumulates() {
        let script = Script::new();
        script.drop_reply(0x0101, 1).drop_reply(0x0101, 1);
        let mut state = script.state.lock().unwrap();
        assert!(state.take_drop(0x0101));
        assert!(state.take_drop(0x0101));
        assert!(!state.take_drop(0x0101));
    }

    #[test]
    fn standing_faults_are_scoped_to_their_object() {
        let script = Script::new();
        script
            .stall_flow(7)
            .close_track_transport_after(7, 3)
            .deny_input_classes(0b10);
        let state = script.state.lock().unwrap();
        assert!(state.flow_stalled(7));
        assert!(!state.flow_stalled(8), "another track is unaffected");
        assert_eq!(state.close_after(7), Some(3));
        assert_eq!(state.close_after(8), None);
        assert_eq!(state.denied_classes(), 0b10);
    }
}
