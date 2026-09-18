//! Headless overlay state for [`TestPresenter`](super::presenter::TestPresenter).
//!
//! The actual state lives in [`crate::presenter::overlay_host`], shared with the real terminating
//! presenter so a producer-side regression against this presenter and against a live host agree on
//! window, focus, revision, and validation behavior. This module only re-exports that surface under
//! its established test-facing path.

pub(crate) use crate::presenter::overlay_host::{OverlayReply, Overlays};
pub use crate::presenter::overlay_host::{PresentedScene, RetainedAsset};
