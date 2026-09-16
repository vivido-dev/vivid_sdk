//! Bounded host-service requests between a terminating presenter and its outer bridge.
use super::BridgeSurfaceKey;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;

// These envelopes also cross bounded JSON control transports (byte arrays expand up to 4x).
pub const MAX_OVERLAY_HOST_BODY: usize = 192 * 1024;
pub(super) const MAX_RETAINED_LAYOUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverlayHostRequest {
    pub id: u64,
    pub surface: BridgeSurfaceKey,
    pub record_type: u16,
    /// Strict encoded control envelope, containing no authentication material.
    pub body: Vec<u8>,
    pub layout_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverlayLayout {
    pub id: u64,
    /// A single-text, retained MeasureBatch envelope in the inner window's namespace.
    pub body: Vec<u8>,
}

#[derive(Default)]
pub(super) struct OverlayRelay {
    pub enabled: bool,
    pub next_id: u64,
    pub next_layout: u64,
    pub queued: VecDeque<OverlayHostRequest>,
    pub pending: HashMap<u64, mpsc::SyncSender<Result<Vec<u8>, String>>>,
    pub reservations: HashMap<u64, (BridgeSurfaceKey, usize, usize)>,
}
