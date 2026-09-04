//! Media resources: an opaque, runtime-minted reference to media a presenter is holding.
//!
//! The agent mesh carries envelopes and references, never media. When one agent wants to say "look
//! at what I am showing", the mesh's answer is a reference the receiver resolves by asking the
//! runtime that holds the content. The id is minted **here**, by the presenter, because neither of
//! the other two parties can: a producer does not know how the runtime composed what it sent, and
//! the mesh never sees media at all.
//!
//! Two kinds of reference exist, and the difference is the whole point. "Look at what I was showing
//! when I asked" and "look at what is on that surface now" are different requests, and one
//! reference type would silently answer the wrong one:
//!
//! - A [`Binding::Pinned`] reference records every revision, generation, and media epoch at the
//!   moment it was minted, and resolves only while all of them still match. When the producer
//!   replaced the track, advanced the epoch, or the surface changed generation, it names content
//!   that no longer exists and resolution fails. **It never falls back to whatever is there
//!   instead** — that is the media equivalent of an address silently retargeting after a tab is
//!   reordered, and it is refused for the same reason.
//! - A [`Binding::Live`] reference records the surface only. It follows that surface and is honest
//!   about being a pointer to a place rather than to content, so it goes stale only when the
//!   surface is destroyed or its owner is gone.
//!
//! A resource is scoped to one runtime instance and **does not travel across a gateway**. Vivid
//! invariant 9 lets a hop terminate and re-originate, which creates independent ids, revisions and
//! generations, so the tuple naming content on one side names nothing on the other. The far side
//! must mint its own. Correlating the two would need a content digest, which the mesh's audit
//! design deliberately refuses.

use std::fmt;

/// Which question a reference answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// The content as it was when the reference was minted, or nothing.
    Pinned,
    /// Whatever is on that surface now.
    Live,
}

impl Binding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Live => "live",
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a resource did not resolve.
///
/// These are the mesh's own words, so a receiver can report the reason it was given rather than
/// inventing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceError {
    /// No resource with that id was minted by this runtime instance, or it has been released.
    Unknown,
    /// The reference named content that no longer exists.
    Stale,
}

impl ResourceError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "resource_unknown",
            Self::Stale => "resource_stale",
        }
    }
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::error::Error for ResourceError {}

/// What one track looks like right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackFacts {
    pub track_id: u64,
    pub revision: u64,
    pub channel_generation: u64,
    pub media_epoch: u32,
    /// Whether pixels are in hand this instant, which is a stronger claim than the track being
    /// visual: encoded video never is here, and a raster awaiting recovery is not yet.
    pub capturable: bool,
}

/// A resource resolved against the runtime's current state.
///
/// The complete owner tuple, so a receiver can address the same content through any other interface
/// that runtime offers without guessing which surface a bare id meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaResourceDescription {
    pub binding: Binding,
    pub producer: u64,
    pub context_id: u64,
    pub surface_id: u64,
    pub surface_revision: u64,
    pub surface_generation: u64,
    /// Absent when a live reference points at a surface that currently holds no track. That is a
    /// pointer to an empty place, which is different from a stale one.
    pub track: Option<TrackFacts>,
}

/// What a pinned reference must still match to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PinnedFacts {
    pub(crate) track_id: u64,
    pub(crate) surface_revision: u64,
    pub(crate) surface_generation: u64,
    pub(crate) track_revision: u64,
    pub(crate) channel_generation: u64,
    pub(crate) media_epoch: u32,
}

/// One minted resource, as the presenter remembers it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResourceRecord {
    pub(crate) producer: u64,
    pub(crate) context_id: u64,
    pub(crate) surface_id: u64,
    /// `None` for a live reference, which deliberately remembers nothing it would have to match.
    pub(crate) pinned: Option<PinnedFacts>,
}

impl ResourceRecord {
    pub(crate) fn binding(&self) -> Binding {
        if self.pinned.is_some() {
            Binding::Pinned
        } else {
            Binding::Live
        }
    }
}
