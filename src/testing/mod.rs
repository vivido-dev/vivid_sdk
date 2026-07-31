//! Shared test support: one in-process Vivid presenter every producer tests against.
//!
//! Enabled by the `testing` feature so it never reaches a production build. It lives in the SDK
//! rather than in any one producer because three producers with three private fakes is three
//! different notions of correct, and the interoperability bugs that matter are exactly the ones a
//! private fake agrees with.

pub mod presenter;
pub mod script;

pub use presenter::{
    DestroyObservation, Observed, ObservedBinding, ROOT_SECRET_HEX, TargetKind, TestPresenter,
    TrackChannelLog,
};
pub use script::{Fault, Script};
