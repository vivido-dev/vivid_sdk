//! Producer-side input binding guard and the bounded input event queue.
use std::io;

use vivid_protocol::input::{
    INPUT_CLASS_KNOWN_MASK, InputBinding, InputEvent, InputTuple, MAX_WATCHDOG_US, MIN_WATCHDOG_US,
};
use vivid_protocol::revision::{InputEpoch, SurfaceGeneration};
use vivid_protocol::time::Monotonic;

use crate::{BoundedQueue, InputBindingStatus, InputGrantTermination, InputLeaseRenewal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPreconditions {
    pub surface_present: bool,
    pub surface_generation: SurfaceGeneration,
    pub capability_mask: u64,
    pub presented: bool,
    pub lane_live: bool,
}
impl DesktopPreconditions {
    pub const fn none() -> Self {
        Self {
            surface_present: false,
            surface_generation: SurfaceGeneration::ZERO,
            capability_mask: 0,
            presented: false,
            lane_live: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveGrant {
    pub grant_generation: u64,
    pub effective_classes: u64,
    pub watchdog_timeout_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardState {
    Fresh,
    Active(ActiveGrant),
    Inactive,
}

/// The producer-side input-binding lifecycle manager.
pub struct InputBindingGuard {
    epoch: u64,
    state: GuardState,
    surface_id: u64,
    surface_generation: SurfaceGeneration,
    watchdog_deadline: Option<Monotonic>,
    preconditions: DesktopPreconditions,
}
impl InputBindingGuard {
    pub fn new() -> Self {
        Self {
            epoch: 0,
            state: GuardState::Fresh,
            surface_id: 0,
            surface_generation: SurfaceGeneration::ZERO,
            watchdog_deadline: None,
            preconditions: DesktopPreconditions::none(),
        }
    }
    pub fn set_preconditions(&mut self, p: DesktopPreconditions) {
        self.preconditions = p;
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn grant(&self) -> Option<ActiveGrant> {
        match self.state {
            GuardState::Active(g) => Some(g),
            _ => None,
        }
    }
    pub fn is_armed(&self, now: Monotonic) -> bool {
        matches!(self.state, GuardState::Active(_)) && !self.is_expired(now)
    }
    pub fn is_expired(&self, now: Monotonic) -> bool {
        self.watchdog_deadline.is_some_and(|d| now >= d)
    }
    pub fn current_tag(&self) -> Option<InputTuple> {
        match self.state {
            GuardState::Active(g) => Some(InputTuple {
                producer_epoch: InputEpoch::new(self.epoch),
                grant_generation: vivid_protocol::revision::GrantGeneration::new(
                    g.grant_generation,
                ),
                context_id: 0,
                surface_id: self.surface_id,
                surface_generation: self.surface_generation,
            }),
            _ => None,
        }
    }
    pub fn enable(
        &mut self,
        surface_id: u64,
        surface_generation: SurfaceGeneration,
        classes: u64,
        watchdog_us: u64,
        reason: u64,
    ) -> io::Result<InputBinding> {
        if classes == 0 || classes & !INPUT_CLASS_KNOWN_MASK != 0 {
            return Err(err("requested classes contain unassigned bits"));
        }
        if !(MIN_WATCHDOG_US..=MAX_WATCHDOG_US).contains(&watchdog_us) {
            return Err(err("requested watchdog is outside the registered range"));
        }
        if reason > 7 {
            return Err(err("transition reason is unregistered"));
        }
        if !self.preconditions.surface_present {
            return Err(err("the named surface is not active"));
        }
        if surface_generation != self.preconditions.surface_generation {
            return Err(err("surface generation mismatch"));
        }
        if !self.preconditions.presented {
            return Err(err("milestone 5 not observed on current video channel"));
        }
        if !self.preconditions.lane_live {
            return Err(err("the interactive lane is not live"));
        }
        if classes & self.preconditions.capability_mask == 0 {
            return Err(err("no requested class within capability mask"));
        }
        let epoch = self.advance_epoch();
        self.surface_id = surface_id;
        self.surface_generation = surface_generation;
        Ok(InputBinding {
            producer_epoch: InputEpoch::new(epoch),
            context_id: 0,
            surface_id,
            surface_generation,
            requested_classes: classes,
            reason,
            requested_watchdog_us: watchdog_us,
        })
    }
    pub fn disable(&mut self, reason: u64) -> InputBinding {
        let epoch = self.advance_epoch();
        self.state = GuardState::Inactive;
        self.watchdog_deadline = None;
        InputBinding {
            producer_epoch: InputEpoch::new(epoch),
            context_id: 0,
            surface_id: 0,
            surface_generation: SurfaceGeneration::ZERO,
            requested_classes: 0,
            reason,
            requested_watchdog_us: 0,
        }
    }
    pub fn handle_bound(&mut self, status: &InputBindingStatus) -> io::Result<()> {
        if status.producer_epoch != self.epoch {
            return Err(err("INPUT_BOUND returned a different epoch"));
        }
        if status.state > 2 {
            return Err(err("INPUT_BOUND returned an unregistered state"));
        }
        if status.effective_classes == 0 && status.state == 1 {
            return Err(err("an enabled grant carries no effective classes"));
        }
        match status.state {
            0 => {
                self.state = GuardState::Inactive;
                self.watchdog_deadline = None;
            }
            1 => {
                if status.effective_classes == 0
                    || status.grant_generation == 0
                    || !(MIN_WATCHDOG_US..=MAX_WATCHDOG_US).contains(&status.watchdog_timeout_us)
                {
                    return Err(err("enabled grant has invalid fields"));
                }
                self.state = GuardState::Active(ActiveGrant {
                    grant_generation: status.grant_generation,
                    effective_classes: status.effective_classes,
                    watchdog_timeout_us: status.watchdog_timeout_us,
                });
            }
            2 => {
                self.state = GuardState::Inactive;
                self.watchdog_deadline = None;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    pub fn handle_renewal(
        &mut self,
        renewal: &InputLeaseRenewal,
        now: Monotonic,
    ) -> io::Result<()> {
        let grant = self
            .grant()
            .ok_or_else(|| err("a renewal arrived without an active grant"))?;
        if renewal.renewal_sequence == 0 {
            return Err(err("a renewal sequence is zero"));
        }
        if renewal.watchdog_timeout_us != grant.watchdog_timeout_us {
            return Err(err("a renewal changed the watchdog"));
        }
        let deadline = now
            .checked_add_micros(renewal.watchdog_timeout_us)
            .ok_or_else(|| err("a renewal overflowed local time"))?;
        self.watchdog_deadline = Some(deadline);
        Ok(())
    }
    pub fn handle_revocation(&mut self, _termination: &InputGrantTermination) -> io::Result<()> {
        self.release();
        Ok(())
    }
    pub fn release(&mut self) {
        self.state = GuardState::Inactive;
        self.watchdog_deadline = None;
    }
    fn advance_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.checked_add(1).expect("input epoch wrapped");
        self.epoch
    }
}
impl Default for InputBindingGuard {
    fn default() -> Self {
        Self::new()
    }
}

pub struct InputQueue {
    inner: BoundedQueue<InputEvent>,
}
impl InputQueue {
    pub fn new(events_per_sec: u64) -> Self {
        Self {
            inner: BoundedQueue::new((events_per_sec.clamp(1, 4096)) as usize),
        }
    }
    pub fn push(&self, event: InputEvent) -> Result<(), InputEvent> {
        self.inner.push_nonblocking(event)
    }
    #[allow(clippy::result_unit_err)]
    pub fn pop(&self) -> Result<InputEvent, ()> {
        self.inner.pop()
    }
    pub fn close(&self) {
        self.inner.close();
    }
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
    pub fn dropped(&self) -> u64 {
        self.inner.dropped()
    }
}

fn err(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, m.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::input::{INPUT_CLASS_KEYBOARD, INPUT_CLASS_POINTER_MOTION};
    use vivid_protocol::revision::GrantGeneration;

    fn pre() -> DesktopPreconditions {
        DesktopPreconditions {
            surface_present: true,
            surface_generation: SurfaceGeneration::new(1),
            capability_mask: INPUT_CLASS_KEYBOARD | INPUT_CLASS_POINTER_MOTION,
            presented: true,
            lane_live: true,
        }
    }

    fn renewal_tuple() -> InputTuple {
        InputTuple {
            producer_epoch: InputEpoch::new(1),
            grant_generation: GrantGeneration::new(5),
            context_id: 1,
            surface_id: 7,
            surface_generation: SurfaceGeneration::new(1),
        }
    }

    #[test]
    fn enable_without_surface_refused() {
        let mut g = InputBindingGuard::new();
        assert!(
            g.enable(
                1,
                SurfaceGeneration::new(1),
                INPUT_CLASS_KEYBOARD,
                2_000_000,
                0
            )
            .is_err()
        );
    }
    #[test]
    fn enable_advances_epoch() {
        let mut g = InputBindingGuard::new();
        g.set_preconditions(pre());
        let b = g
            .enable(
                7,
                SurfaceGeneration::new(1),
                INPUT_CLASS_KEYBOARD,
                2_000_000,
                0,
            )
            .unwrap();
        assert_eq!(b.producer_epoch.get(), 1);
        assert_eq!(b.surface_id, 7);
        assert!(!b.disabled());
        let b2 = g
            .enable(
                7,
                SurfaceGeneration::new(1),
                INPUT_CLASS_KEYBOARD,
                2_000_000,
                0,
            )
            .unwrap();
        assert_eq!(b2.producer_epoch.get(), 2);
    }
    #[test]
    fn disable_clears_grant() {
        let mut g = InputBindingGuard::new();
        g.set_preconditions(pre());
        let _ = g
            .enable(
                7,
                SurfaceGeneration::new(1),
                INPUT_CLASS_KEYBOARD,
                2_000_000,
                0,
            )
            .unwrap();
        g.handle_bound(&InputBindingStatus {
            producer_epoch: 1,
            grant_generation: 5,
            context_id: 1,
            surface_id: 7,
            surface_generation: 1,
            effective_classes: INPUT_CLASS_KEYBOARD,
            state: 1,
            reason: 0,
            watchdog_timeout_us: 2_000_000,
        })
        .unwrap();
        g.handle_renewal(
            &InputLeaseRenewal {
                binding: renewal_tuple(),
                renewal_sequence: 1,
                watchdog_timeout_us: 2_000_000,
            },
            Monotonic::ZERO,
        )
        .unwrap();
        assert!(g.is_armed(Monotonic::ZERO));
        let _ = g.disable(1);
        g.handle_bound(&InputBindingStatus {
            producer_epoch: 2,
            grant_generation: 0,
            context_id: 0,
            surface_id: 0,
            surface_generation: 0,
            effective_classes: 0,
            state: 0,
            reason: 1,
            watchdog_timeout_us: 0,
        })
        .unwrap();
        assert!(!g.is_armed(Monotonic::ZERO));
        assert!(g.current_tag().is_none());
    }
    #[test]
    fn revocation_releases() {
        let mut g = InputBindingGuard::new();
        g.set_preconditions(pre());
        let _ = g
            .enable(
                7,
                SurfaceGeneration::new(1),
                INPUT_CLASS_KEYBOARD,
                2_000_000,
                0,
            )
            .unwrap();
        g.handle_bound(&InputBindingStatus {
            producer_epoch: 1,
            grant_generation: 5,
            context_id: 1,
            surface_id: 7,
            surface_generation: 1,
            effective_classes: INPUT_CLASS_KEYBOARD,
            state: 1,
            reason: 0,
            watchdog_timeout_us: 2_000_000,
        })
        .unwrap();
        assert!(g.grant().is_some());
        g.release();
        assert!(g.grant().is_none());
    }
    #[test]
    fn watchdog_expires() {
        let mut g = InputBindingGuard::new();
        g.set_preconditions(pre());
        let _ = g
            .enable(
                7,
                SurfaceGeneration::new(1),
                INPUT_CLASS_KEYBOARD,
                1_000_000,
                0,
            )
            .unwrap();
        g.handle_bound(&InputBindingStatus {
            producer_epoch: 1,
            grant_generation: 5,
            context_id: 1,
            surface_id: 7,
            surface_generation: 1,
            effective_classes: INPUT_CLASS_KEYBOARD,
            state: 1,
            reason: 0,
            watchdog_timeout_us: 1_000_000,
        })
        .unwrap();
        g.handle_renewal(
            &InputLeaseRenewal {
                binding: renewal_tuple(),
                renewal_sequence: 1,
                watchdog_timeout_us: 1_000_000,
            },
            Monotonic::ZERO,
        )
        .unwrap();
        assert!(!g.is_expired(Monotonic::ZERO));
        assert!(g.is_expired(Monotonic::from_micros(1_000_000)));
    }
    #[test]
    fn overflow_rejected() {
        let q = InputQueue::new(1);
        let e = InputEvent::Key {
            binding: renewal_tuple(),
            usage: 4,
            pressed: true,
        };
        assert!(q.push(e).is_ok());
        assert!(q.push(e).is_err());
        assert_eq!(q.dropped(), 1);
        q.close();
    }
}
