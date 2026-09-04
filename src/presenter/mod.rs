//! Terminating Vivid 1.5 presenter.
//!
//! The other half of this crate. A producer opens sessions, surfaces, tracks, and channels; a
//! presenter accepts them, authenticates each principal, issues flow, and holds the retained scene.
//! The two share `vivid_protocol` and nothing else, which is why they can live together here
//! without either depending on the other.
//!
//! This module is namespaced rather than re-exported into the crate root, unlike every other
//! module: a presenter's `SceneNode` is its own projection of a node and is a different type from
//! the producer's [`crate::SceneNode`]. Flattening the two roles into one namespace would make that
//! collision look like an accident rather than the wire model.
//!
//! Products supply the accepted-connection listener; the module owns only protocol state.

mod config;
mod listener;
mod service;
mod socket;
mod transport;

pub use config::*;
pub use listener::{ConnectionCancel, PresenterListener, Transport};
pub use service::*;
pub use socket::SocketListener;
pub use transport::{Reader, Writer};

/// Why a presenter asked its producer for a keyframe.
///
/// The reason travels in key 5 of `NEED_KEYFRAME`. A transport loss is distinguished from a decoder
/// error because only the former hands the replacement channel a fresh media epoch.
pub const KEYFRAME_REASON_INITIAL: u64 = 1;
pub const KEYFRAME_REASON_DECODER_ERROR: u64 = 2;
pub const KEYFRAME_REASON_TRANSPORT_LOSS: u64 = 5;
