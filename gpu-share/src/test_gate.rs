//! Test-only control of one export on a privately owned producer queue.

// Rust guideline compliant 2026-10-07
use super::*;
use objc2::rc::Retained;
use objc2_metal::MTLSharedEvent;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// Releases a withheld Metal signal on drop, including during fixture teardown.
#[derive(Debug)]
pub struct WithheldSignal(Retained<ProtocolObject<dyn MTLSharedEvent>>);

impl Drop for WithheldSignal {
    fn drop(&mut self) {
        self.0.setSignaledValue(1);
    }
}

#[derive(Debug, Default)]
pub(super) struct Gate(Mutex<Option<SyncSender<WithheldSignal>>>);

impl Pool {
    /// Withholds the next export on this pool until the receiver drops its signal.
    /// Use only on a fixture's privately owned producer queue. The receiver gets
    /// the signal after submission. Other pools on separate queues remain live.
    ///
    /// # Errors
    /// Returns `WouldBlock` when a previous request is still armed.
    pub fn withhold_next_copy_for_test(&self) -> io::Result<Receiver<WithheldSignal>> {
        let mut gate = self.gate.0.lock().expect("test signal mutex");
        if gate.is_some() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let (sender, receiver) = sync_channel(1);
        *gate = Some(sender);
        Ok(receiver)
    }
}

impl Gate {
    pub(super) fn stage(
        &self,
        queue: &wgpu::Queue,
    ) -> io::Result<Option<(SyncSender<WithheldSignal>, WithheldSignal)>> {
        let Some(sender) = self.0.lock().expect("test signal mutex").take() else {
            return Ok(None);
        };
        // SAFETY: Borrow the live Metal queue without destroying it. Only the
        // fixture worker submits to this queue while arming and issuing its copy.
        let queue = unsafe { queue.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or_else(|| invalid("expected a Metal test queue"))?;
        queue.enable_strict_event_sync().map_err(io::Error::other)?;
        let event = queue
            .as_raw()
            .device()
            .newSharedEvent()
            .ok_or_else(|| io::Error::other("Metal shared event unavailable"))?;
        queue.add_wait_event(event.clone(), 1);
        Ok(Some((sender, WithheldSignal(event))))
    }
}
