//! Native vector composition with explicit host presentation acknowledgement.
//!
//! Prepare immutable frames on the font/render worker, compose their drawings,
//! then acknowledge only the frame actually displayed by the outer GUI. Neither
//! preparation nor GPU submission grants input or emits a Presented outcome.

// Rust guideline compliant 2026-10-07
use super::super::{
    OverlayHostRequest,
    overlay_host::native::{Composition, Stamp},
};
use super::native_text::NativeTextHost;
use super::*;
use crate::SLOT_VECTOR;
use crate::vector_renderer::{self, CompiledScene, Drawing};
use std::collections::BTreeMap;
use std::sync::Weak;
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use vivid_protocol::{
    identity::SurfaceIdentity,
    overlay::wire::{Submission, WindowAddress},
    vector::{Frame, ImageAsset},
};

#[cfg(feature = "vector-images")]
#[path = "native_vector/images.rs"]
mod images;
#[path = "native_vector/input.rs"]
mod input;
#[path = "native_vector/raster.rs"]
mod raster;
#[path = "native_vector/visibility.rs"]
mod visibility;
#[doc(inline)]
pub use input::{
    MAX_VECTOR_TEXT_BYTES, VectorEditorState, VectorImeEdit, VectorInputEvent, VectorInputResult,
    VectorInputTarget, VectorTextTarget,
};
#[doc(inline)]
pub use visibility::NativePaneVisibility;

// Two outstanding GUI/upload frames per pane, and at most 64 across the host.
const MAX_FRAMES: usize = 64;
const MAX_PANES: usize = 64;
const MAX_DIMENSION: u32 = 8192;

// One budget follows immutable CPU/GPU references across all native media workers.
pub(super) fn charge(state: &mut State, bytes: usize) -> io::Result<Arc<()>> {
    state
        .native_charges
        .retain(|(_, token)| token.strong_count() != 0);
    let used = state
        .native_charges
        .iter()
        .try_fold(0u64, |sum, (bytes, _)| sum.checked_add(*bytes as u64))
        .ok_or_else(|| invalid("native resource charge overflow"))?;
    if used
        .checked_add(bytes as u64)
        .is_none_or(|total| total > state.config.media.aggregate_retained_bytes)
    {
        return Err(invalid("native drawing resources exhausted"));
    }
    let token = Arc::new(());
    state.native_charges.push((bytes, Arc::downgrade(&token)));
    Ok(token)
}

pub(super) fn supports_kind(kind: &KindConfiguration) -> bool {
    match kind {
        KindConfiguration::VectorScene(_) => true,
        #[cfg(feature = "vector-audio")]
        KindConfiguration::Audio(_) => true,
        #[cfg(feature = "vector-video")]
        KindConfiguration::Video(config) => super::native_video::output_bytes(config).is_some(),
        KindConfiguration::Raster(config) => {
            config.width <= MAX_DIMENSION
                && config.height <= MAX_DIMENSION
                && u64::from(config.width)
                    .checked_mul(u64::from(config.height))
                    .and_then(|n| n.checked_mul(4))
                    .is_some_and(|n| n != 0 && n <= vector_renderer::MAX_COMPILED_BYTES as u64)
        }
        #[cfg(feature = "vector-images")]
        KindConfiguration::EncodedImage(config) => images::output_bytes(config).is_some(),
        _ => false,
    }
}

/// An immutable pane composition and its private presentation receipt.
#[derive(Clone)]
pub struct VectorFrame(Arc<FrameData>);
struct FrameData {
    presenter: PresenterInstanceId,
    host: Weak<AtomicBool>,
    pane: PaneId,
    serial: u64,
    target: u64,
    visibility: u64,
    dimensions: (u32, u32),
    owners: Vec<u64>,
    resumes: Vec<(u64, u64)>,
    input_context: Vec<(u64, Option<u64>)>,
    stamp: Stamp,
    entries: Vec<Entry>,
    drawings: Vec<Drawing>,
    raster_entries: Vec<raster::Entry>,
    rasters: Vec<vector_renderer::RasterDrawing>,
}
struct Entry {
    window: SurfaceIdentity,
    source: SourceKey,
    resume: u64,
    body: Arc<[u8]>,
    writer: Weak<Writer>,
}
impl VectorFrame {
    /// Returns the back-to-front drawings for the shared GPU compositor.
    pub fn drawings(&self) -> &[Drawing] {
        &self.0.drawings
    }
    /// Returns back-to-front scene-node pixels beneath the vector window plane.
    pub fn rasters(&self) -> &[vector_renderer::RasterDrawing] {
        &self.0.rasters
    }

    /// Compares render content and receipts without granting display or input authority.
    pub fn same_content(&self, other: &Self) -> bool {
        self.same_input_context(other)
            && self.dimensions() == other.dimensions()
            && self.drawings().len() == other.drawings().len()
            && self.drawings().iter().zip(other.drawings()).all(|(a, b)| {
                a.window == b.window
                    && a.window_revision == b.window_revision
                    && a.submission == b.submission
                    && a.bounds == b.bounds
                    && a.scale == b.scale
                    && Arc::ptr_eq(&a.compiled, &b.compiled)
            })
            && self.rasters().len() == other.rasters().len()
            && self
                .rasters()
                .iter()
                .zip(other.rasters())
                .all(|(a, b)| a.same_content(b))
            && self
                .0
                .raster_entries
                .iter()
                .zip(&other.0.raster_entries)
                .all(|(a, b)| a.same(b))
    }
    /// Compares the immutable input context needed to reuse a GUI image delivery.
    ///
    /// Equal pixels can require a new display receipt after lane replacement,
    /// expiry, owner recovery or target changes. This comparison grants no input.
    pub fn same_input_context(&self, other: &Self) -> bool {
        self.0.presenter == other.0.presenter
            && Weak::ptr_eq(&self.0.host, &other.0.host)
            && self.0.pane == other.0.pane
            && self.0.target == other.0.target
            && self.0.visibility == other.0.visibility
            && self.0.owners == other.0.owners
            && self.0.resumes == other.0.resumes
            && self.0.input_context == other.0.input_context
    }
    /// Returns the physical target dimensions for this frame.
    pub fn dimensions(&self) -> (u32, u32) {
        self.0.dimensions
    }
}
impl std::fmt::Debug for VectorFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorFrame")
            .field("dimensions", &self.0.dimensions)
            .field("drawings", &self.0.drawings.len())
            .finish_non_exhaustive()
    }
}
struct Cached {
    submission: Submission,
    body: Weak<[u8]>,
    compiled: Arc<CompiledScene>,
}
struct Image {
    body: Weak<[u8]>,
    data: ImageData,
    token: Arc<()>,
}
struct Candidate {
    window: SurfaceIdentity,
    source: SourceKey,
    resume: u64,
    placement: u64,
    bounds: vivid_protocol::vector::Rect,
    submission: Submission,
    body: Arc<[u8]>,
    assets: Vec<(u16, Arc<[u8]>)>,
    writer: Weak<Writer>,
}
struct Snapshot {
    identity: PresenterInstanceId,
    target: u64,
    visibility: u64,
    dimensions: (u32, u32),
    owners: Vec<u64>,
    resumes: Vec<(u64, u64)>,
    input_context: Vec<(u64, Option<u64>)>,
    stamp: Stamp,
    candidates: Vec<Candidate>,
    rasters: Vec<raster::Candidate>,
}

/// Binds native text, drawing resources and display receipts to one presenter.
pub struct NativeVectorHost {
    presenter: Arc<VirtualVivid>,
    alive: Arc<AtomicBool>,
    text: NativeTextHost,
    next: u64,
    frames: Vec<Weak<FrameData>>,
    presented: HashMap<PaneId, u64>,
    cache: HashMap<SourceKey, Cached>,
    images: HashMap<(SourceKey, u64, u64), Image>,
    raster_cache: HashMap<TrackKey, raster::Cached>,
    #[cfg(feature = "vector-images")]
    decoder: Option<images::Decoder>,
    input: input::InputState,
}
impl NativeVectorHost {
    /// Enables native live and timed audio before any producer authenticates.
    ///
    /// Device and decoder work stays on bounded source workers. This opt-in
    /// integration supports timed audio with exact PLAY placement, prebuffering
    /// and pause/resume. With `vector-video`, linked video follows its audio
    /// master in matching Live or Timed mode. Synchronized starts remain unsupported.
    /// Decoded timestamps stage gaps and overlaps in queue order.
    ///
    /// # Errors
    /// Rejects an already authenticated presenter.
    #[cfg(feature = "vector-audio")]
    pub fn enable_audio(&mut self) -> io::Result<()> {
        let mut state = lock(&self.presenter.state);
        if !state.sessions.is_empty() {
            return Err(invalid(
                "native audio must be enabled before authentication",
            ));
        }
        state.audio_host = true;
        state.native_audio_configured = true;
        Ok(())
    }

    /// Selects a clocked, device-free audio callback before producer authentication.
    ///
    /// Available only with explicit test support. It changes no wire admission,
    /// decoder, scope, flow-control or retirement checks.
    ///
    /// # Errors
    /// Rejects an already authenticated presenter.
    #[cfg(all(feature = "vector-audio", feature = "testing"))]
    pub fn use_test_audio_output(&mut self) -> io::Result<()> {
        let mut state = lock(&self.presenter.state);
        if !state.sessions.is_empty() {
            return Err(invalid("test audio must be selected before authentication"));
        }
        state.audio_test_device = true;
        Ok(())
    }

    /// Enables synchronized native media for producer conformance tests.
    ///
    /// This explicit test-only override exercises the unfinished recovery
    /// integration without changing production attachment's offered profiles.
    /// Call before authenticating producers. Ordinary wire admission, native
    /// readiness and ownership checks remain in force.
    ///
    /// # Errors
    /// Rejects an already authenticated presenter or incompatible profiles.
    #[cfg(all(
        feature = "testing",
        any(feature = "vector-audio", feature = "vector-video")
    ))]
    pub fn use_test_synchronized_media(&mut self) -> io::Result<()> {
        let mut state = lock(&self.presenter.state);
        if !state.sessions.is_empty() {
            return Err(invalid("test synchronization must precede authentication"));
        }
        let mut profiles = state.config.supported_profiles.clone();
        profiles.push(vivid_protocol::registry::TIMED_MEDIA_SYNC.into());
        profiles.sort();
        profiles.dedup();
        vivid_protocol::registry::validate_profile_set(profiles.iter().map(String::as_str))
            .map_err(|_| invalid("native synchronization prerequisites are absent"))?;
        state.config.supported_profiles = profiles;
        Ok(())
    }

    /// Attaches local rendering before producers authenticate.
    ///
    /// Construct and poll this on the native font/render worker. The presenter
    /// must explicitly enable overlays. Native text is attached automatically.
    ///
    /// # Errors
    /// Rejects missing fonts, an active presenter or an existing host attachment.
    pub fn attach(presenter: Arc<VirtualVivid>, family: impl Into<String>) -> io::Result<Self> {
        Self::attach_inner(presenter, family, false)
    }

    /// Replaces a retired native host while retaining authenticated sessions.
    ///
    /// Restores the original audio policy and creates a new attachment identity.
    /// Producers must replace lost tracks and submit fresh scenes/receipts.
    /// Every pane starts hidden; explicitly show it when the frontend is ready.
    ///
    /// # Errors
    /// Rejects a live attachment, missing fonts, absent retirement history, an
    /// exhausted host generation, or an incompatible presenter role.
    pub fn attach_replacement(
        presenter: Arc<VirtualVivid>,
        family: impl Into<String>,
    ) -> io::Result<Self> {
        Self::attach_inner(presenter, family, true)
    }

    fn attach_inner(
        presenter: Arc<VirtualVivid>,
        family: impl Into<String>,
        replacement: bool,
    ) -> io::Result<Self> {
        if lock(&presenter.state).events.is_some() {
            return Err(invalid(
                "native vector host requires an eventless local presenter",
            ));
        }
        let alive = Arc::new(AtomicBool::new(true));
        let text = NativeTextHost::attach_with(presenter.clone(), family, replacement, |state| {
            if replacement
                && (state.native_host_generation == 0
                    || state
                        .native_host_alive
                        .upgrade()
                        .is_some_and(|host| host.load(Ordering::Acquire)))
            {
                return Err(invalid(
                    "native host replacement requires a retired attachment",
                ));
            }
            let generation = state
                .native_host_generation
                .checked_add(1)
                .ok_or_else(|| invalid("native host generation exhausted"))?;
            state.native_host_generation = generation;
            state.native_host_alive = Arc::downgrade(&alive);
            state.overlays.native = Some(Default::default());
            // Production attachments keep synchronization unadvertised while
            // first-party recovery policy and lifecycle acceptance are pending.
            if !replacement {
                state
                    .config
                    .supported_profiles
                    .retain(|profile| profile != vivid_protocol::registry::TIMED_MEDIA_SYNC);
            }
            #[cfg(feature = "vector-video")]
            {
                state.video_host = true;
            }
            #[cfg(feature = "vector-audio")]
            {
                state.audio_host = state.native_audio_configured;
            }
            Ok(())
        })?;
        Ok(Self {
            presenter,
            alive,
            text,
            next: 0,
            frames: Vec::new(),
            presented: HashMap::new(),
            cache: HashMap::new(),
            images: HashMap::new(),
            raster_cache: HashMap::new(),
            #[cfg(feature = "vector-images")]
            decoder: None,
            input: Default::default(),
        })
    }

    /// Advances playback and services cached editor or platform work.
    ///
    /// Call periodically even without a repaint so timed EOS and queued video progress.
    pub fn poll(&mut self) -> Option<OverlayHostRequest> {
        #[cfg(feature = "vector-audio")]
        super::native_audio::prime_requested(
            &self.presenter.state,
            &self.presenter.delivery_changed,
            None,
        );
        #[cfg(feature = "vector-audio")]
        super::native_audio::tick(&mut lock(&self.presenter.state));
        #[cfg(feature = "vector-video")]
        super::native_video::tick(&mut lock(&self.presenter.state));
        self.flush_input();
        let request = self.text.poll()?;
        if request.record_type == messages::SET_OVERLAY_EDITOR {
            let response = Envelope::new(1, Vec::new())
                .encode()
                .map_err(|_| "editor reply encoding failed".into());
            self.presenter
                .complete_overlay_host_request(request.id, response);
            None
        } else {
            Some(request)
        }
    }

    /// Returns the current admitted overlay pointer shape for this pane.
    ///
    /// Apply it only in this pane's GUI viewport. Invalidated or undisplayed
    /// hover geometry returns no shape; this method grants no input authority.
    /// Redispatch the last physical pointer position after a changed composition
    /// to refresh hover geometry even when the pointer has not moved.
    pub fn cursor(&self, pane: PaneId) -> Option<vivid_protocol::vector::CursorShape> {
        lock(&self.presenter.state).overlays.native_cursor(pane)
    }

    /// Sets pane visibility while retaining native media and producer play intent.
    ///
    /// Hidden timed audio and video pause their local clocks. Live media keeps
    /// progressing. Both transitions revoke input and outstanding display receipts;
    /// showing requires a fresh displayed frame. Call before starting a producer
    /// in an initially hidden pane, and keep polling while hidden. This method
    /// never waits for an audio callback or calls a native device API.
    ///
    /// # Errors
    /// Rejects unknown pane metrics, zero pane IDs, or the host's pane limit.
    /// Propagates visibility revision exhaustion after hiding and revoking the pane.
    pub fn set_visible(&mut self, pane: PaneId, visible: bool) -> io::Result<()> {
        if self.visibility_control(pane)?.set_visible(visible)? {
            self.input.invalidate(pane);
            self.presented.insert(pane, self.next);
        }
        Ok(())
    }

    /// Creates a pane-scoped visibility controller independent of the render worker.
    ///
    /// Install metrics first. The cloneable controller can be used on the GUI
    /// thread while this host is compiling, rendering or waiting for its GPU.
    /// It retains no presenter or media resources after this host retires.
    ///
    /// # Errors
    /// Rejects unknown pane metrics, zero pane IDs, or the host's pane limit.
    pub fn visibility_control(&mut self, pane: PaneId) -> io::Result<NativePaneVisibility> {
        let mut state = lock(&self.presenter.state);
        if pane == 0 || !state.metrics.contains_key(&pane) {
            return Err(invalid("native visibility requires pane metrics"));
        }
        if !self.presented.contains_key(&pane) && self.presented.len() >= MAX_PANES {
            return Err(invalid("native pane capacity exhausted"));
        }
        self.presented.entry(pane).or_insert(0);
        if state.native_host_generation > 1 && !state.native_visibility.contains_key(&pane) {
            state.native_hidden.insert(pane);
        }
        state.native_visibility.entry(pane).or_insert(0);
        Ok(NativePaneVisibility::new(
            &self.presenter,
            &self.alive,
            pane,
        ))
    }

    /// Revokes a pane's input and every outstanding pre-invalidation receipt.
    ///
    /// Call when hiding a pane, losing its render target or replacing its GPU
    /// device. Retained drawing resources remain available for recovery. A fresh
    /// displayed receipt is required even when the cached image is reused.
    pub fn invalidate(&mut self, pane: PaneId) {
        let Some(serial) = self.presented.get_mut(&pane) else {
            return;
        };
        *serial = self.next;
        self.input.invalidate(pane);
        let mut state = lock(&self.presenter.state);
        if let Some(native) = &mut state.overlays.native {
            native.compositions.remove(&pane);
        }
        let owners = overlay_sessions_for_pane(&state, pane);
        state.overlays.revoke_native_input(&owners);
        for owner in owners {
            flush_overlay_lane(&mut state, owner);
        }
    }

    /// Compiles one bounded, immutable pane composition without acknowledging it.
    ///
    /// Images and text are resolved in their complete source/channel namespaces.
    /// Unsupported scenes close their own window, leaving peers renderable.
    ///
    /// # Errors
    /// Rejects invalid scale/geometry, exhausted frame capacity or serials, and
    /// panes whose metrics have not been installed. At most two frames per pane
    /// may remain outstanding; drop obsolete upload/presentation receipts.
    pub fn prepare(&mut self, pane: PaneId, scale: f64) -> io::Result<VectorFrame> {
        if !scale.is_finite() || scale <= 0. || scale > 8. {
            return Err(invalid("invalid native vector scale"));
        }
        self.frames.retain(|frame| frame.strong_count() != 0);
        if self.frames.len() >= MAX_FRAMES
            || self
                .frames
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|frame| frame.pane == pane)
                .count()
                >= 2
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native frame capacity exhausted",
            ));
        }
        if !self.presented.contains_key(&pane) && self.presented.len() >= MAX_PANES {
            return Err(invalid("native pane capacity exhausted"));
        }
        let Snapshot {
            identity,
            target,
            visibility,
            dimensions,
            owners,
            resumes,
            input_context,
            mut stamp,
            candidates,
            rasters,
        } = self.snapshot(pane, scale)?;
        // Prune against the entire presenter's live sources, not just this
        // pane. Preparing a neighbor must not evict another pane's image cache.
        {
            let state = lock(&self.presenter.state);
            let track = |source: &SourceKey| {
                let key = SurfaceKey {
                    session: source.producer,
                    context: source.context,
                    surface: source.surface,
                };
                let window = super::super::overlay_host::owner(source.producer)
                    .context(source.context)
                    .ok()?
                    .surface(source.surface)
                    .ok()?;
                if state
                    .sessions
                    .get(&source.producer)
                    .is_none_or(|session| session.closed)
                    || state.overlays.window(window).is_none()
                {
                    return None;
                }
                state.tracks.get(&TrackKey {
                    surface: key,
                    track: source.track,
                })
            };
            self.cache.retain(|source, _| track(source).is_some());
            self.images.retain(|(source, generation, id), _| {
                track(source).is_some_and(|track| {
                    track.state.channel_generation.get() == *generation
                        && track.retained_vector.contains_asset(*id)
                })
            });
        }
        let mut entries = Vec::new();
        let mut drawings = Vec::new();
        for candidate in candidates {
            if lock(&self.presenter.state)
                .overlays
                .window(candidate.window)
                .is_none()
            {
                continue;
            }
            let compiled = match self.compile(&candidate) {
                Ok(compiled) => compiled,
                Err(_) => {
                    self.cache.remove(&candidate.source);
                    self.images.retain(|key, _| key.0 != candidate.source);
                    let mut state = lock(&self.presenter.state);
                    state.overlays.close_native(candidate.window);
                    flush_overlay_lane(&mut state, candidate.source.producer);
                    continue;
                }
            };
            let window = SessionIdentity::new(identity, candidate.source.producer)
                .and_then(|owner| owner.context(candidate.source.context))
                .and_then(|context| context.surface(candidate.source.surface))
                .map_err(io::Error::other)?;
            entries.push(Entry {
                window: candidate.window,
                source: candidate.source,
                resume: candidate.resume,
                body: candidate.body.clone(),
                writer: candidate.writer.clone(),
            });
            drawings.push(Drawing {
                window,
                window_revision: candidate.placement,
                bounds: candidate.bounds,
                scale,
                compiled,
                submission: candidate.submission,
            });
        }
        // A rejected scene may close descendants too; remove only those windows
        // from the original stamp. Concurrent placement changes still invalidate it.
        {
            let state = lock(&self.presenter.state);
            stamp.retain(|(id, generation, _)| {
                state
                    .overlays
                    .window(*id)
                    .is_some_and(|window| window.generation == *generation)
            });
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| invalid("native frame serial exhausted"))?;
        let (raster_entries, rasters) = self.prepare_rasters(rasters, scale)?;
        let frame = Arc::new(FrameData {
            presenter: identity,
            host: Arc::downgrade(&self.alive),
            pane,
            serial: self.next,
            target,
            visibility,
            dimensions,
            owners,
            resumes,
            input_context,
            stamp,
            entries,
            drawings,
            raster_entries,
            rasters,
        });
        self.frames.push(Arc::downgrade(&frame));
        self.presented.entry(pane).or_default();
        Ok(VectorFrame(frame))
    }

    /// Acknowledges the exact composition the outer GUI actually displayed.
    ///
    /// Call after displayed-frame completion, including when the GPU cache was
    /// reused. A stale composition returns `false` and grants no new input.
    ///
    /// # Errors
    /// Rejects a receipt from another presenter or host attachment, or a detached host.
    pub fn presented(&mut self, frame: &VectorFrame) -> io::Result<bool> {
        let frame = &frame.0;
        let mut state = lock(&self.presenter.state);
        if frame.presenter != state.presenter
            || !Weak::ptr_eq(&frame.host, &Arc::downgrade(&self.alive))
            || state.overlays.native.is_none()
        {
            return Err(invalid("foreign native presentation receipt"));
        }
        // Retire revoked holds before presentation resets cancellation markers.
        // Same-owner focus changes may already have delivered their Cancel.
        for owner in self.input.prune(&mut state) {
            flush_overlay_lane(&mut state, owner);
        }
        let mut owners = overlay_sessions_for_pane(&state, frame.pane);
        owners.sort_unstable();
        if state.native_pane_hidden(frame.pane)
            || state
                .native_visibility
                .get(&frame.pane)
                .copied()
                .unwrap_or(0)
                != frame.visibility
            || self
                .presented
                .get(&frame.pane)
                .is_some_and(|serial| *serial >= frame.serial)
            || state
                .metrics
                .get(&frame.pane)
                .is_none_or(|metrics| metrics.generation != frame.target)
            || owners != frame.owners
            || owners
                .iter()
                .map(|owner| (*owner, state.overlays.input_generation(*owner)))
                .collect::<Vec<_>>()
                != frame.input_context
            || frame.resumes.iter().any(|(owner, generation)| {
                state
                    .sessions
                    .get(owner)
                    .is_none_or(|session| session.resume_generation != *generation)
            })
            || state.overlays.native_stamp(&owners) != frame.stamp
            || frame
                .entries
                .iter()
                .zip(&frame.drawings)
                .any(|(entry, drawing)| !current(&state, entry, drawing))
        {
            return Ok(false);
        }
        let changed = raster::presented(&mut state, &frame.raster_entries)
            | frame.entries.iter().any(|entry| {
                state
                    .overlays
                    .native
                    .as_ref()
                    .is_some_and(|native| native.pending.contains_key(&entry.window))
                    || state
                        .tracks
                        .get(&TrackKey {
                            surface: SurfaceKey {
                                session: entry.source.producer,
                                context: entry.source.context,
                                surface: entry.source.surface,
                            },
                            track: entry.source.track,
                        })
                        .is_some_and(|track| !track.outer_presented)
            });
        for (entry, drawing) in frame.entries.iter().zip(&frame.drawings) {
            assert!(
                state.overlays.present_native(
                    entry.window,
                    drawing.window_revision,
                    drawing.submission,
                    drawing.compiled.clone(),
                ),
                "native frame preflight must make presentation atomic"
            );
            if let Some(track) = state.tracks.get_mut(&TrackKey {
                surface: SurfaceKey {
                    session: entry.source.producer,
                    context: entry.source.context,
                    surface: entry.source.surface,
                },
                track: entry.source.track,
            }) {
                track.outer_presented = true;
                track.state.milestones |= MILESTONE_PRESENTED;
            }
        }
        let shown: HashSet<_> = frame.entries.iter().map(|entry| entry.window).collect();
        let native = state
            .overlays
            .native
            .as_mut()
            .expect("attached native host");
        native.displayed.retain(|id, _| {
            !owners.contains(&id.context().session().session_id()) || shown.contains(id)
        });
        native.compositions.insert(
            frame.pane,
            Composition {
                owners: owners.clone(),
                stamp: frame.stamp.clone(),
            },
        );
        for owner in owners {
            flush_overlay_lane(&mut state, owner);
        }
        self.presented.insert(frame.pane, frame.serial);
        if changed {
            state.wake_waits();
            advance_projection(&mut state);
        }
        #[cfg(feature = "vector-video")]
        super::native_video::tick(&mut state);
        Ok(true)
    }

    fn charge(&mut self, bytes: usize) -> io::Result<Arc<()>> {
        charge(&mut lock(&self.presenter.state), bytes)
    }

    fn compile(&mut self, candidate: &Candidate) -> io::Result<Arc<CompiledScene>> {
        if let Some(cached) = self.cache.get(&candidate.source).filter(|cached| {
            cached.submission == candidate.submission
                && cached
                    .body
                    .upgrade()
                    .is_some_and(|body| Arc::ptr_eq(&body, &candidate.body))
        }) {
            return Ok(cached.compiled.clone());
        }
        let frame = Frame::decode(&candidate.body).map_err(io::Error::other)?;
        let referenced: HashSet<_> = frame
            .canvas
            .commands()
            .iter()
            .filter_map(|command| {
                use vivid_protocol::vector::{Brush, Command};
                match command {
                    Command::Image { asset, .. }
                    | Command::Fill(_, Brush::Image { asset, .. })
                    | Command::Stroke(_, Brush::Image { asset, .. }, _)
                    | Command::StyledStroke(_, Brush::Image { asset, .. }, _) => Some(*asset),
                    _ => None,
                }
            })
            .collect();
        let mut images = BTreeMap::new();
        let mut tokens = Vec::new();
        for (kind, body) in &candidate.assets {
            if *kind != messages::VECTOR_ASSET {
                continue;
            }
            let id = asset_id(body).ok_or_else(|| invalid("invalid native image header"))?;
            if !referenced.contains(&id) {
                continue;
            }
            let key = (
                candidate.source,
                candidate.submission.channel_generation,
                id,
            );
            if self.images.get(&key).is_none_or(|image| {
                image
                    .body
                    .upgrade()
                    .is_none_or(|prior| !Arc::ptr_eq(&prior, body))
            }) {
                let token = self.charge(body.len())?;
                let asset = ImageAsset::decode(body).map_err(io::Error::other)?;
                self.images.insert(
                    key,
                    Image {
                        body: Arc::downgrade(body),
                        token,
                        data: ImageData {
                            width: asset.width,
                            height: asset.height,
                            data: Blob::new(Arc::new(asset.rgba)),
                            format: ImageFormat::Rgba8,
                            alpha_type: ImageAlphaType::Alpha,
                        },
                    },
                );
            }
            let image = &self.images[&key];
            images.insert(id, image.data.clone());
            tokens.push(image.token.clone());
        }
        let layouts = self.text.retained_layouts(BridgeSurfaceKey {
            producer: candidate.source.producer,
            context: candidate.source.context,
            surface: candidate.source.surface,
        });
        let mut compiled = vector_renderer::compile_with_layouts(
            &frame.canvas,
            &mut self.text.fonts,
            &images,
            &layouts,
        )
        .map_err(io::Error::other)?;
        let bytes = compiled
            .memory_size()
            .checked_add(candidate.body.len())
            .and_then(|bytes| {
                // Resource insertion can grow the existing token vector. Charge
                // its full possible replacement capacity before retaining it.
                (compiled.retained_resource_count() + tokens.len() + 1)
                    .checked_mul(2 * std::mem::size_of::<Arc<()>>())
                    .and_then(|tokens| bytes.checked_add(tokens))
            })
            .ok_or_else(|| invalid("native scene charge overflow"))?;
        let token = self.charge(bytes)?;
        compiled.retain_resource(token).map_err(io::Error::other)?;
        for token in tokens {
            compiled.retain_resource(token).map_err(io::Error::other)?;
        }
        let compiled = Arc::new(compiled);
        self.cache.insert(
            candidate.source,
            Cached {
                submission: candidate.submission,
                body: Arc::downgrade(&candidate.body),
                compiled: compiled.clone(),
            },
        );
        Ok(compiled)
    }

    fn snapshot(&self, pane: PaneId, scale: f64) -> io::Result<Snapshot> {
        let mut state = lock(&self.presenter.state);
        #[cfg(feature = "vector-video")]
        super::native_video::tick(&mut state);
        state.overlays.prune_native();
        let metrics = state
            .metrics
            .get(&pane)
            .ok_or_else(|| invalid("native pane metrics are absent"))?;
        let width = (f64::from(metrics.viewport_width) * scale).ceil();
        let height = (f64::from(metrics.viewport_height) * scale).ceil();
        if width < 1.
            || height < 1.
            || width > f64::from(MAX_DIMENSION)
            || height > f64::from(MAX_DIMENSION)
        {
            return Err(invalid("native vector target exceeds bounds"));
        }
        let target = metrics.generation;
        let mut owners = overlay_sessions_for_pane(&state, pane);
        owners.sort_unstable();
        let resumes = owners
            .iter()
            .map(|owner| (*owner, state.sessions[owner].resume_generation))
            .collect();
        let input_context = owners
            .iter()
            .map(|owner| (*owner, state.overlays.input_generation(*owner)))
            .collect();
        let stamp = state.overlays.native_stamp(&owners);
        let mut candidates = Vec::new();
        for window in state.overlays.native_windows() {
            let session = window.identity.context().session().session_id();
            if !owners.contains(&session) {
                continue;
            }
            let key = SurfaceKey {
                session,
                context: window.identity.context().context_id(),
                surface: window.identity.surface_id(),
            };
            let Some(surface) = state.surfaces.get(&key) else {
                continue;
            };
            let Some(track_id) = surface.active_slots.get(&SLOT_VECTOR) else {
                continue;
            };
            let Some(track) = state.tracks.get(&TrackKey {
                surface: key,
                track: *track_id,
            }) else {
                continue;
            };
            if track.channel_writer.is_none()
                || track.accepted_surface_generation
                    != Some((surface.state.generation, track.state.channel_generation))
            {
                continue;
            }
            let Some(body) = &track.retained else {
                continue;
            };
            let frame = Frame::decode(body).map_err(io::Error::other)?;
            candidates.push(Candidate {
                window: window.identity,
                source: bridge_track_key(TrackKey {
                    surface: key,
                    track: *track_id,
                }),
                resume: state.sessions[&session].resume_generation,
                placement: window.revision,
                bounds: window.options.bounds,
                submission: Submission {
                    address: WindowAddress {
                        context_id: key.context,
                        surface_id: key.surface,
                        generation: window.generation,
                    },
                    track_id: *track_id,
                    channel_generation: track.state.channel_generation.get(),
                    epoch: frame.epoch,
                    revision: frame.revision,
                },
                body: body.clone(),
                assets: track.retained_vector.snapshot(),
                writer: Arc::downgrade(
                    track
                        .channel_writer
                        .as_ref()
                        .expect("attached native channel"),
                ),
            });
        }
        Ok(Snapshot {
            identity: state.presenter,
            target,
            visibility: state.native_visibility.get(&pane).copied().unwrap_or(0),
            dimensions: (width as u32, height as u32),
            owners,
            resumes,
            input_context,
            stamp,
            candidates,
            rasters: raster::snapshot(&state, pane),
        })
    }
}

fn asset_id(body: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(body.get(..8)?.try_into().ok()?))
}
fn current(state: &State, entry: &Entry, drawing: &Drawing) -> bool {
    let Some(window) = state.overlays.window(entry.window) else {
        return false;
    };
    let Some(native) = &state.overlays.native else {
        return false;
    };
    if window.revision != drawing.window_revision
        || window.generation != drawing.submission.address.generation
        || native.pending.get(&entry.window).map_or_else(
            || native.committed.get(&entry.window) != Some(&drawing.submission),
            |pending| {
                pending.submission != drawing.submission
                    || window.scene_revision >= drawing.submission.revision
            },
        )
    {
        return false;
    }
    let key = SurfaceKey {
        session: entry.source.producer,
        context: entry.source.context,
        surface: entry.source.surface,
    };
    let Some(runtime) = state.sessions.get(&key.session) else {
        return false;
    };
    let Some(surface) = state.surfaces.get(&key) else {
        return false;
    };
    let Some(track) = state.tracks.get(&TrackKey {
        surface: key,
        track: entry.source.track,
    }) else {
        return false;
    };
    !runtime.closed
        && runtime.resume_generation == entry.resume
        && surface.state.generation.get() == drawing.submission.address.generation
        && surface.active_slots.get(&SLOT_VECTOR) == Some(&entry.source.track)
        && track.state.channel_generation.get() == drawing.submission.channel_generation
        && track.channel_writer.is_some()
        && track
            .retained
            .as_ref()
            .filter(|body| Arc::ptr_eq(body, &entry.body))
            .and_then(|body| Frame::decode(body).ok())
            .is_some_and(|frame| {
                frame.epoch == drawing.submission.epoch
                    && frame.revision == drawing.submission.revision
            })
}
impl std::fmt::Debug for NativeVectorHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeVectorHost")
            .field("cached_scenes", &self.cache.len())
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

impl Drop for NativeVectorHost {
    fn drop(&mut self) {
        let mut state = lock(&self.presenter.state);
        #[cfg(any(feature = "vector-audio", feature = "vector-video"))]
        super::native_hold::host_detached(&mut state);
        self.alive.store(false, Ordering::Release);
        state.native_hidden.clear();
        state.native_visibility.clear();
        #[cfg(feature = "vector-audio")]
        {
            state.audio_host = false;
            for track in state
                .tracks
                .values_mut()
                .filter(|t| t.audio_reservation.is_some())
            {
                super::native_audio::clear(track);
                if let Some(writer) = track.channel_writer.take() {
                    writer.close();
                }
                track.audio_reservation = None;
                let _ = track.state.lose();
            }
        }
        #[cfg(feature = "vector-video")]
        {
            state.video_host = false;
        }
        #[cfg(feature = "vector-video")]
        for track in state
            .tracks
            .values_mut()
            .filter(|t| t.video_reservation.is_some())
        {
            if let Some(writer) = track.channel_writer.take() {
                writer.close();
            }
            super::native_video::clear(track);
            track.video_playback = None;
            track.video_reservation = None;
            let _ = track.state.lose();
        }
        let mut windows: HashSet<_> = state
            .overlays
            .native_windows()
            .iter()
            .map(|window| window.identity)
            .collect();
        if let Some(native) = &state.overlays.native {
            windows.extend(
                native
                    .pending
                    .keys()
                    .chain(native.committed.keys())
                    .copied(),
            );
        }
        for window in windows {
            let owner = window.context().session().session_id();
            state.overlays.close_native(window);
            flush_overlay_lane(&mut state, owner);
        }
        // Keep deferred mode installed: a dropped physical host cannot turn
        // subsequent accepted records into synthetic presentation proof.
        if let Some(native) = &mut state.overlays.native {
            *native = Default::default();
        }
    }
}
