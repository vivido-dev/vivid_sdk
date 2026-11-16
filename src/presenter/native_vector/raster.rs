//! Retained scene-node pixels and owner-scoped physical display receipts.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::vector_renderer::{RasterDrawing, RasterImage};

#[derive(Clone)]
enum Body {
    Bytes(Weak<[u8]>),
    #[cfg(feature = "vector-video")]
    Video(Weak<RasterImage>),
}
impl Body {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bytes(a), Self::Bytes(b)) => Weak::ptr_eq(a, b),
            #[cfg(feature = "vector-video")]
            (Self::Video(a), Self::Video(b)) => Weak::ptr_eq(a, b),
            #[cfg(feature = "vector-video")]
            _ => false,
        }
    }
    #[cfg(feature = "vector-images")]
    fn bytes(&self) -> Option<Arc<[u8]>> {
        match self {
            Self::Bytes(bytes) => bytes.upgrade(),
            #[cfg(feature = "vector-video")]
            _ => None,
        }
    }
}
enum Pixels {
    Bytes(Arc<[u8]>),
    #[cfg(feature = "vector-video")]
    Video(Arc<RasterImage>),
}
impl Pixels {
    fn identity(&self) -> Body {
        match self {
            Self::Bytes(body) => Body::Bytes(Arc::downgrade(body)),
            #[cfg(feature = "vector-video")]
            Self::Video(image) => Body::Video(Arc::downgrade(image)),
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
enum Content {
    #[cfg(feature = "vector-video")]
    Video {
        epoch: u32,
        frame: u64,
        pts: i64,
        reset: u64,
        play: u64,
        width: u32,
        height: u32,
    },
    Raster {
        premultiplied: bool,
        epoch: u32,
        frame: u64,
        width: u32,
        height: u32,
    },
    #[cfg(feature = "vector-images")]
    Encoded(ImageConfiguration),
}
impl Content {
    fn same_image(&self, other: &Self) -> bool {
        if self.dimensions() != other.dimensions() {
            return false;
        }
        match (self, other) {
            (
                Self::Raster {
                    premultiplied: a, ..
                },
                Self::Raster {
                    premultiplied: b, ..
                },
            ) => a == b,
            #[cfg(feature = "vector-video")]
            (Self::Video { .. }, Self::Video { .. }) => true,
            #[cfg(feature = "vector-images")]
            (Self::Encoded(a), Self::Encoded(b)) => a == b,
            #[cfg(any(feature = "vector-images", feature = "vector-video"))]
            _ => false,
        }
    }
    fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Raster { width, height, .. } => (*width, *height),
            #[cfg(feature = "vector-video")]
            Self::Video { width, height, .. } => (*width, *height),
            #[cfg(feature = "vector-images")]
            Self::Encoded(c) => (c.width, c.height),
        }
    }
    fn presentation(&self) -> (u32, u64) {
        match self {
            Self::Raster { epoch, frame, .. } => (*epoch, *frame),
            #[cfg(feature = "vector-video")]
            Self::Video { epoch, frame, .. } => (*epoch, *frame),
            #[cfg(feature = "vector-images")]
            Self::Encoded(_) => (0, 1),
        }
    }
}
#[derive(Clone)]
struct Scope {
    #[cfg(feature = "vector-video")]
    incarnation: Option<Weak<()>>,
    source: TrackKey,
    generation: SurfaceGeneration,
    channel: ChannelGeneration,
    resume: u64,
    writer: Option<Weak<Writer>>,
    body: Body,
    content: Content,
}
impl Scope {
    fn same_body(&self, other: &Self) -> bool {
        self.content.same_image(&other.content) && self.body.same(&other.body)
    }
    fn same(&self, other: &Self) -> bool {
        self.source == other.source
            && self.generation == other.generation
            && self.channel == other.channel
            && self.resume == other.resume
            && same_writer(&self.writer, &other.writer)
            && self.same_body(other)
            && self.content == other.content
            && {
                #[cfg(feature = "vector-video")]
                {
                    match (&self.incarnation, &other.incarnation) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Weak::ptr_eq(a, b),
                        _ => false,
                    }
                }
                #[cfg(not(feature = "vector-video"))]
                {
                    true
                }
            }
    }
    fn image_current(&self, state: &State) -> bool {
        let Some(track) = state
            .tracks
            .get(&self.source)
            .filter(|t| !t.state.lost && !t.recovery_pending)
        else {
            return false;
        };
        match &self.content {
            #[cfg(feature = "vector-video")]
            Content::Video { .. } => track
                .native_video
                .as_ref()
                .is_some_and(|f| self.body.same(&Body::Video(Arc::downgrade(&f.image)))),
            Content::Raster {
                width,
                height,
                premultiplied,
                ..
            } => {
                matches!(&track.configuration.kind, KindConfiguration::Raster(c) if (c.alpha_mode == 2) == *premultiplied)
                    && track.retained_raster.as_ref().is_some_and(|r| {
                        r.width == *width
                            && r.height == *height
                            && self.body.same(&Body::Bytes(Arc::downgrade(&r.pixels)))
                    })
            }
            #[cfg(feature = "vector-images")]
            Content::Encoded(config) => {
                matches!(&track.configuration.kind, KindConfiguration::EncodedImage(c) if c == config)
                    && track
                        .retained
                        .as_ref()
                        .is_some_and(|b| self.body.same(&Body::Bytes(Arc::downgrade(b))))
            }
        }
    }
    fn content_current(&self, state: &State) -> bool {
        self.image_current(state)
            && match self.content {
                #[cfg(feature = "vector-video")]
                Content::Video {
                    epoch,
                    frame,
                    reset,
                    ..
                } => state.tracks[&self.source]
                    .native_video
                    .as_ref()
                    .is_some_and(|f| f.epoch == epoch && f.id == frame && f.reset == reset),
                Content::Raster { epoch, frame, .. } => state.tracks[&self.source]
                    .retained_raster
                    .as_ref()
                    .is_some_and(|r| r.epoch == epoch && r.frame_id == frame),
                #[cfg(feature = "vector-images")]
                Content::Encoded(_) => true,
            }
    }
    fn decode_current(&self, state: &State) -> bool {
        self.content_current(state) && self.binding_current(state)
    }
    fn binding_current(&self, state: &State) -> bool {
        state
            .sessions
            .get(&self.source.surface.session)
            .is_some_and(|s| !s.closed && s.resume_generation == self.resume)
            && state
                .surfaces
                .get(&self.source.surface)
                .is_some_and(|s| s.state.generation == self.generation)
            && state.tracks.get(&self.source).is_some_and(|t| {
                !t.state.lost
                    && !t.recovery_pending
                    && {
                        #[cfg(feature = "vector-video")]
                        {
                            self.incarnation
                                .as_ref()
                                .is_none_or(|id| super::super::native_video::owns(t, id))
                        }
                        #[cfg(not(feature = "vector-video"))]
                        {
                            true
                        }
                    }
                    && t.accepted_surface_generation == Some((self.generation, self.channel))
                    && t.state.channel_generation == self.channel
                    && same_writer(&self.writer, &t.channel_writer.as_ref().map(Arc::downgrade))
            })
    }
    fn current(&self, state: &State) -> bool {
        let live = {
            #[cfg(feature = "vector-video")]
            if let Content::Video {
                epoch,
                frame,
                reset,
                play,
                ..
            } = self.content
            {
                self.binding_current(state)
                    && state.tracks.get(&self.source).is_some_and(|t| {
                        t.state.media_epoch == epoch
                            && t.decoder_reset_serial == reset
                            && t.native_frame_serial >= frame
                            && t.video_playback.as_ref().is_none_or(|p| p.serial == play)
                    })
            } else {
                self.decode_current(state)
            }
            #[cfg(not(feature = "vector-video"))]
            {
                self.decode_current(state)
            }
        };
        live && state
            .surfaces
            .get(&self.source.surface)
            .is_some_and(|s| s.active_slots.values().any(|id| *id == self.source.track))
    }
}
pub(super) struct Entry {
    node: NodeKey,
    config: ProtocolSceneNode,
    scope: Scope,
}
impl Entry {
    pub(super) fn same(&self, other: &Self) -> bool {
        self.node == other.node && self.config == other.config && self.scope.same(&other.scope)
    }
    fn current(&self, state: &State) -> bool {
        state
            .nodes
            .get(&self.node)
            .is_some_and(|node| node.node == self.config)
            && self.scope.current(state)
    }
}
fn same_writer(a: &Option<Weak<Writer>>, b: &Option<Weak<Writer>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Weak::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
enum CachedImage {
    Ready(Arc<RasterImage>),
    #[cfg(feature = "vector-images")]
    Waiting,
    #[cfg(feature = "vector-images")]
    Pending(u64),
    #[cfg(feature = "vector-images")]
    Failed,
}
pub(super) struct Cached {
    scope: Scope,
    image: CachedImage,
}
pub(super) struct Candidate {
    entry: Entry,
    body: Pixels,
    bounds: vivid_protocol::vector::Rect,
    clip: Option<vivid_protocol::vector::Rect>,
}
fn supported(kind: &KindConfiguration) -> bool {
    match kind {
        KindConfiguration::Raster(_) => true,
        #[cfg(feature = "vector-video")]
        KindConfiguration::Video(_) => true,
        #[cfg(feature = "vector-images")]
        KindConfiguration::EncodedImage(_) => true,
        _ => false,
    }
}
pub(super) fn snapshot(state: &State, pane: PaneId) -> Vec<Candidate> {
    let Some(metrics) = state.metrics.get(&pane) else {
        return Vec::new();
    };
    let desktop = state.config.target.profile_name() == registry::DESKTOP_SURFACE;
    // Terminal scene-node geometry is signed 32.32 cells; overlays use logical pixels.
    let unit = if desktop {
        (1., 1.)
    } else {
        (
            f64::from(metrics.cell_width) / 4_294_967_296.,
            f64::from(metrics.cell_height) / 4_294_967_296.,
        )
    };
    let rect = |x: i64, y: i64, w: i64, h: i64| {
        vivid_protocol::vector::Rect::new(
            x as f64 * unit.0,
            y as f64 * unit.1,
            w as f64 * unit.0,
            h as f64 * unit.1,
        )
        .ok()
    };
    let mut nodes: Vec<_> = state
        .nodes
        .iter()
        .filter(|(_, n)| n.pane == pane && n.node.visible && n.node.opacity != 0)
        .collect();
    nodes.sort_by_key(|(key, n)| (n.node.z_index, key.session, key.context, key.node));
    let mut result = Vec::new();
    for (node, config) in nodes {
        let source = SurfaceKey {
            session: node.session,
            context: config.node.surface_context_id,
            surface: config.node.surface_id,
        };
        let Some(surface) = state.surfaces.get(&source) else {
            continue;
        };
        let Some(session) = state.sessions.get(&node.session).filter(|s| !s.closed) else {
            continue;
        };
        let Some(key) = surface
            .active_slots
            .iter()
            .filter_map(|(slot, id)| {
                let key = TrackKey {
                    surface: source,
                    track: *id,
                };
                state
                    .tracks
                    .get(&key)
                    .filter(|t| supported(&t.configuration.kind))
                    .map(|_| (*slot, key))
            })
            .min_by_key(|(slot, _)| *slot)
            .map(|(_, key)| key)
        else {
            continue;
        };
        let Some(track) = state.tracks.get(&key).filter(|t| {
            !t.state.lost
                && !t.recovery_pending
                && t.accepted_surface_generation
                    == Some((surface.state.generation, t.state.channel_generation))
        }) else {
            continue;
        };
        let (body, content) = match &track.configuration.kind {
            #[cfg(feature = "vector-video")]
            KindConfiguration::Video(_) => {
                let Some(frame) = track.native_video.as_ref().filter(|f| {
                    f.reset == track.decoder_reset_serial && f.surface == surface.state.generation
                }) else {
                    continue;
                };
                let (width, height) = frame.image.dimensions();
                (
                    Pixels::Video(frame.image.clone()),
                    Content::Video {
                        epoch: frame.epoch,
                        frame: frame.id,
                        pts: frame.pts,
                        reset: frame.reset,
                        play: frame.play,
                        width,
                        height,
                    },
                )
            }
            KindConfiguration::Raster(config) => {
                let Some(r) = &track.retained_raster else {
                    continue;
                };
                (
                    Pixels::Bytes(r.pixels.clone()),
                    Content::Raster {
                        premultiplied: config.alpha_mode == 2,
                        epoch: r.epoch,
                        frame: r.frame_id,
                        width: r.width,
                        height: r.height,
                    },
                )
            }
            #[cfg(feature = "vector-images")]
            KindConfiguration::EncodedImage(c) => {
                let Some(body) = &track.retained else {
                    continue;
                };
                (Pixels::Bytes(body.clone()), Content::Encoded(c.clone()))
            }
            _ => continue,
        };
        let Some(projected) = projected_node_config(
            &config.node,
            bridge_track_key(key),
            session,
            0,
            state.config.target.profile_name(),
            state.config.target.extent(),
        ) else {
            continue;
        };
        let n = projected.node;
        let Some(bounds) = rect(n.x, n.y, n.width, n.height) else {
            continue;
        };
        let clip = match projected.clip {
            Some(c) => {
                let Some(rect) = rect(c.x, c.y, c.width, c.height) else {
                    continue;
                };
                Some(rect)
            }
            None => None,
        };
        if result.len() == vivid_protocol::overlay::MAX_WINDOWS {
            break;
        }
        let scope = Scope {
            #[cfg(feature = "vector-video")]
            incarnation: track
                .native_video
                .as_ref()
                .map(|frame| frame.incarnation.clone()),
            source: key,
            generation: surface.state.generation,
            channel: track.state.channel_generation,
            resume: session.resume_generation,
            writer: track.channel_writer.as_ref().map(Arc::downgrade),
            body: body.identity(),
            content,
        };
        result.push(Candidate {
            entry: Entry {
                node: *node,
                config: config.node.clone(),
                scope,
            },
            body,
            bounds,
            clip,
        });
    }
    result
}
impl NativeVectorHost {
    pub(super) fn prepare_rasters(
        &mut self,
        candidates: Vec<Candidate>,
        scale: f64,
    ) -> io::Result<(Vec<Entry>, Vec<RasterDrawing>)> {
        #[cfg(feature = "vector-images")]
        self.prepare_images()?;
        {
            let state = lock(&self.presenter.state);
            self.raster_cache.retain(|key, cache| {
                #[cfg(feature = "vector-images")]
                if matches!(cache.scope.content, Content::Encoded(_)) {
                    // Encoded tracks may prime before activation or placement.
                    return true;
                }
                let live = if matches!(cache.image, CachedImage::Ready(_)) {
                    cache.scope.image_current(&state)
                } else {
                    cache.scope.current(&state)
                };
                live && state
                    .surfaces
                    .get(&key.surface)
                    .is_some_and(|surface| surface.active_slots.values().any(|id| *id == key.track))
                    && state.nodes.iter().any(|(node, entry)| {
                        node.session == key.surface.session
                            && entry.node.visible
                            && entry.node.opacity != 0
                            && entry.node.surface_context_id == key.surface.context
                            && entry.node.surface_id == key.surface.surface
                    })
            });
        }
        for candidate in &candidates {
            let scope = &candidate.entry.scope;
            if let Some(cache) = self.raster_cache.get_mut(&scope.source)
                && cache.scope.same_body(scope)
                && (cache.scope.same(scope) || matches!(cache.image, CachedImage::Ready(_)))
            {
                cache.scope = scope.clone();
                continue;
            }
            self.raster_cache.remove(&scope.source);
            let image = match &scope.content {
                #[cfg(feature = "vector-video")]
                Content::Video { .. } => {
                    let Pixels::Video(image) = &candidate.body else {
                        continue;
                    };
                    CachedImage::Ready(image.clone())
                }
                Content::Raster {
                    width,
                    height,
                    premultiplied,
                    ..
                } => {
                    #[cfg(not(feature = "vector-video"))]
                    let Pixels::Bytes(body) = &candidate.body;
                    #[cfg(feature = "vector-video")]
                    let Pixels::Bytes(body) = &candidate.body else {
                        continue;
                    };
                    let Ok(token) = self.charge(body.len()) else {
                        continue;
                    };
                    let image = if *premultiplied {
                        RasterImage::new_premultiplied(*width, *height, body.clone())
                    } else {
                        RasterImage::new(*width, *height, body.clone())
                    };
                    let Ok(mut image) = image else {
                        continue;
                    };
                    image
                        .retain_resource(token)
                        .expect("new uncharged raster image");
                    CachedImage::Ready(Arc::new(image))
                }
                #[cfg(feature = "vector-images")]
                Content::Encoded(_) => CachedImage::Waiting,
            };
            self.raster_cache.insert(
                scope.source,
                Cached {
                    scope: scope.clone(),
                    image,
                },
            );
        }
        #[cfg(feature = "vector-images")]
        self.start_images()?;
        let mut entries = Vec::new();
        let mut drawings = Vec::new();
        for candidate in candidates {
            let e = candidate.entry;
            let Some(Cached {
                image: CachedImage::Ready(image),
                ..
            }) = self.raster_cache.get(&e.scope.source)
            else {
                continue;
            };
            let Ok(surface) = SessionIdentity::new(
                lock(&self.presenter.state).presenter,
                e.scope.source.surface.session,
            )
            .and_then(|s| s.context(e.scope.source.surface.context))
            .and_then(|c| c.surface(e.scope.source.surface.surface)) else {
                continue;
            };
            let drawing = RasterDrawing {
                surface,
                node_context: e.node.context,
                node_id: e.node.node,
                bounds: candidate.bounds,
                clip: candidate.clip,
                fit: e.config.fit,
                linear_sampling: e.config.linear_sampling,
                opacity: e.config.opacity,
                scale,
                image: image.clone(),
            };
            if drawing.validate().is_err() {
                self.raster_cache.remove(&e.scope.source);
                continue;
            }
            drawings.push(drawing);
            entries.push(e);
        }
        Ok((entries, drawings))
    }
    #[cfg(feature = "vector-images")]
    fn prepare_images(&mut self) -> io::Result<()> {
        let scopes: Vec<_> = {
            let state = lock(&self.presenter.state);
            self.raster_cache.retain(|_, cache| {
                !matches!(cache.scope.content, Content::Encoded(_))
                    || if matches!(cache.image, CachedImage::Ready(_)) {
                        cache.scope.image_current(&state)
                    } else {
                        cache.scope.decode_current(&state)
                    }
            });
            state
                .tracks
                .iter()
                .filter_map(|(source, track)| {
                    let KindConfiguration::EncodedImage(config) = &track.configuration.kind else {
                        return None;
                    };
                    let body = track.retained.as_ref()?;
                    let surface = state.surfaces.get(&source.surface)?;
                    let session = state.sessions.get(&source.surface.session)?;
                    let scope = Scope {
                        #[cfg(feature = "vector-video")]
                        incarnation: None,
                        source: *source,
                        generation: surface.state.generation,
                        channel: track.state.channel_generation,
                        resume: session.resume_generation,
                        writer: track.channel_writer.as_ref().map(Arc::downgrade),
                        body: Body::Bytes(Arc::downgrade(body)),
                        content: Content::Encoded(config.clone()),
                    };
                    scope.decode_current(&state).then_some(scope)
                })
                .collect()
        };
        for scope in scopes {
            if let Some(cache) = self.raster_cache.get_mut(&scope.source)
                && cache.scope.same_body(&scope)
                && (cache.scope.same(&scope) || matches!(cache.image, CachedImage::Ready(_)))
            {
                cache.scope = scope;
                continue;
            }
            self.raster_cache.insert(
                scope.source,
                Cached {
                    scope,
                    image: CachedImage::Waiting,
                },
            );
        }
        self.finish_images()?;
        self.start_images()
    }
    #[cfg(feature = "vector-images")]
    fn finish_images(&mut self) -> io::Result<()> {
        while let Some(done) = self
            .decoder
            .as_mut()
            .map(super::images::Decoder::poll)
            .transpose()?
            .flatten()
        {
            if let Some(cache) = self.raster_cache.get_mut(&done.source)
                && matches!(cache.image, CachedImage::Pending(id) if id == done.id)
            {
                // Cleanup can race the earlier cache-pruning pass. Keep the
                // final source check and installation under the same state lock.
                let mut state = lock(&self.presenter.state);
                if !cache.scope.decode_current(&state) {
                    self.raster_cache.remove(&done.source);
                    continue;
                }
                match done.image {
                    Ok(image) => {
                        cache.image = CachedImage::Ready(image);
                        let track = state
                            .tracks
                            .get_mut(&done.source)
                            .expect("validated decoded source");
                        track.state.milestones |=
                            MILESTONE_DECODER_INITIALIZED | MILESTONE_OUTPUT_READY;
                        state.wake_waits();
                        advance_projection(&mut state);
                    }
                    Err(()) => {
                        cache.image = CachedImage::Failed;
                        drop(state);
                        reject_image(
                            &self.presenter,
                            &cache.scope,
                            messages::ERROR_DECODER,
                            "native image decoding failed",
                        );
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(feature = "vector-images")]
    fn start_images(&mut self) -> io::Result<()> {
        let mut sources: Vec<_> = self
            .raster_cache
            .iter()
            .filter(|(_, c)| matches!(c.image, CachedImage::Waiting))
            .map(|(key, _)| *key)
            .collect();
        if sources.is_empty() {
            return Ok(());
        }
        if self.decoder.is_none() {
            self.decoder = Some(super::images::Decoder::new()?);
        }
        sources.sort_by_key(|source| super::images::order(*source));
        if let Some(last) = self.decoder.as_ref().and_then(|d| d.last) {
            let pivot = sources.partition_point(|source| super::images::order(*source) <= last);
            sources.rotate_left(pivot);
        }
        for source in sources {
            if !self
                .decoder
                .as_ref()
                .expect("created image decoder")
                .available()
            {
                break;
            }
            let scope = self.raster_cache[&source].scope.clone();
            let Content::Encoded(config) = &scope.content else {
                continue;
            };
            let Some(bytes) = super::images::output_bytes(config) else {
                self.raster_cache
                    .get_mut(&source)
                    .expect("waiting source")
                    .image = CachedImage::Failed;
                reject_image(
                    &self.presenter,
                    &scope,
                    messages::ERROR_LIMIT_EXCEEDED,
                    "native image dimensions exceed decode limits",
                );
                continue;
            };
            let Some(body) = scope.body.bytes() else {
                self.raster_cache.remove(&source);
                continue;
            };
            let Ok(token) = self.charge(bytes) else {
                continue;
            };
            if let Some(id) = self
                .decoder
                .as_mut()
                .expect("created image decoder")
                .submit(source, body, config.clone(), token)?
            {
                self.raster_cache
                    .get_mut(&source)
                    .expect("waiting source")
                    .image = CachedImage::Pending(id);
            }
        }
        Ok(())
    }
}
#[cfg(feature = "vector-images")]
fn reject_image(presenter: &VirtualVivid, scope: &Scope, code: u64, diagnostic: &'static str) {
    let mut state = lock(&presenter.state);
    if !scope.decode_current(&state) {
        return;
    }
    let track = state
        .tracks
        .get_mut(&scope.source)
        .expect("validated image source");
    if track.state.lose().is_err() {
        return;
    }
    if let Some(writer) = track.channel_writer.take() {
        writer.close();
    }
    track.retained = None;
    let revision = track.state.revision.get();
    if let Ok(body) = Envelope::new(
        0,
        vec![
            (0, Value::Unsigned(scope.source.surface.context)),
            (1, Value::Unsigned(scope.source.surface.surface)),
            (2, Value::Unsigned(scope.source.track)),
            (3, Value::Unsigned(code)),
            (4, Value::Unsigned(revision)),
            (5, Value::Map(vec![])),
            (6, Value::Text(diagnostic.into())),
        ],
    )
    .encode()
        && let Some(session) = state.sessions.get(&scope.source.surface.session)
    {
        let _ = session
            .writer
            .write_record(messages::TRACK_LOST, scope.source.track, &body);
    }
    retire_track_deliveries(&mut state, scope.source);
    advance_projection(&mut state);
    state.wake_waits();
}
pub(super) fn presented(state: &mut State, entries: &[Entry]) -> bool {
    let mut changed = false;
    for entry in entries {
        if !entry.current(state) {
            continue;
        }
        let track = state
            .tracks
            .get_mut(&entry.scope.source)
            .expect("validated visual track");
        let (epoch, id) = entry.scope.content.presentation();
        let presentation = (entry.scope.channel, epoch, id);
        #[cfg(feature = "vector-video")]
        if let Content::Video { pts, reset, .. } = entry.scope.content {
            if track
                .native_visual_presented
                .is_some_and(|(c, e, previous)| {
                    c == entry.scope.channel && e == epoch && previous >= id
                })
            {
                continue;
            }
            if let Some(p) = track.video_playback.as_mut() {
                p.presented = pts;
                p.presentation_id = id;
                track.outer_position =
                    super::super::native_video::playback_position(track, Instant::now());
            } else {
                let decoded = track.outer_position.map_or(pts, |p| p.decoded_pts_us);
                track.outer_position = Some(crate::presenter::BridgePositionSnapshot {
                    decoder_reset_serial: reset,
                    playing: true,
                    start_pts_us: 0,
                    state: 2,
                    clock_pts_us: None,
                    decoded_pts_us: decoded,
                    presented_pts_us: pts,
                    presentation_id: id,
                });
            }
        }
        if track.native_visual_presented != Some(presentation) {
            track.native_visual_presented = Some(presentation);
            changed = true;
        }
        #[cfg(feature = "vector-video")]
        if matches!(entry.scope.content, Content::Video { .. })
            && super::super::native_video::ended(track)
        {
            track.state.milestones |= MILESTONE_BUFFERED_ENDED;
        }
        if !track.outer_presented || track.state.milestones & MILESTONE_PRESENTED == 0 {
            track.outer_presented = true;
            track.state.milestones |= MILESTONE_PRESENTED;
            changed = true;
        }
    }
    changed
}
