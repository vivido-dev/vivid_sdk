//! Real, presenter-bound text service with retained native drawing resources.

// Rust guideline compliant 2026-10-07
use super::super::{
    OverlayHostRequest,
    overlay_relay::{MAX_OVERLAY_HOST_BODY, MAX_RETAINED_LAYOUT_BYTES},
};
use super::*;
use crate::vector_renderer::{
    text::{FontSystem, measure_styled},
    text_layout::MeasuredLayout,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Weak,
};
use vivid_protocol::overlay::wire::{
    WindowAddress,
    text::{
        MeasureText,
        styled::{BatchMeasured, MAX_RETAINED_LAYOUTS, MeasureBatch, ReleaseLayouts},
    },
};

struct LayoutEntry {
    reservation: u64,
    generation: u64,
    resume: u64,
    value: Arc<MeasuredLayout>,
}
struct Charge {
    owner: u64,
    bytes: usize,
    token: Weak<()>,
}

/// Serves real font measurement for one terminating presenter's overlay windows.
///
/// Attach before authenticating producers, then poll on a host worker. This owns
/// no GUI loop and creates no terminal. Native measurements and retained painting
/// share the same shaped layout. Other platform requests are returned to the host.
/// Dropping this service cancels pending requests and never restores synthetic metrics.
pub struct NativeTextHost {
    presenter: Arc<VirtualVivid>,
    pub(super) fonts: FontSystem,
    queued: VecDeque<OverlayHostRequest>,
    layouts: HashMap<(BridgeSurfaceKey, u64), LayoutEntry>,
    charges: Vec<Charge>,
}

impl NativeTextHost {
    /// Attaches native font measurement to an unused, overlay-enabled presenter.
    ///
    /// An empty default family selects the platform's sans-serif family.
    ///
    /// # Errors
    /// Rejects missing fonts, active producers, an existing relay or disabled overlays.
    pub fn attach(presenter: Arc<VirtualVivid>, family: impl Into<String>) -> io::Result<Self> {
        Self::attach_with(presenter, family, false, |_| Ok(()))
    }

    pub(super) fn attach_with(
        presenter: Arc<VirtualVivid>,
        family: impl Into<String>,
        replacement: bool,
        initialize: impl FnOnce(&mut State) -> io::Result<()>,
    ) -> io::Result<Self> {
        let fonts = FontSystem::new(family).map_err(|error| invalid(error.reason()))?;
        let mut state = lock(&presenter.state);
        if !state.config.overlay
            || (if replacement {
                !state.overlay_relay.enabled
                    || !state.overlay_relay.local_host_claimed
                    || state.overlay_relay.local_host_available
            } else {
                !state.sessions.is_empty()
                    || state.overlay_relay.enabled
                    || state.overlay_relay.local_host_claimed
            })
        {
            return Err(invalid(
                "native text host requires an unused overlay presenter",
            ));
        }
        let mut profiles = state.config.supported_profiles.clone();
        profiles.push(registry::OVERLAY_TYPOGRAPHY.into());
        profiles.sort();
        profiles.dedup();
        registry::validate_profile_set(profiles.iter().map(String::as_str))
            .map_err(|_| invalid("native typography prerequisites are absent"))?;
        let previous = std::mem::replace(&mut state.config.supported_profiles, profiles);
        if let Err(error) = initialize(&mut state) {
            state.config.supported_profiles = previous;
            return Err(error);
        }
        state.overlay_relay.enabled = true;
        state.overlay_relay.local_host_claimed = true;
        state.overlay_relay.local_host_available = true;
        drop(state);
        Ok(Self {
            presenter: presenter.clone(),
            fonts,
            queued: VecDeque::new(),
            layouts: HashMap::new(),
            charges: Vec::new(),
        })
    }

    /// Processes one request and returns non-text platform work to the caller.
    ///
    /// Callers complete returned requests through the bound presenter's ordinary
    /// host-service API. `None` means idle or one text request completed. Work,
    /// retained layouts and source bytes all have independent bounds.
    pub fn poll(&mut self) -> Option<OverlayHostRequest> {
        self.prune();
        if self.queued.is_empty() {
            self.queued
                .extend(self.presenter.take_overlay_host_requests());
        }
        let request = self.queued.pop_front()?;
        if !lock(&self.presenter.state)
            .overlay_relay
            .pending
            .contains_key(&request.id)
        {
            return None;
        }
        if !matches!(
            request.record_type,
            messages::MEASURE_OVERLAY_TEXT
                | messages::MEASURE_OVERLAY_TEXT_BATCH
                | messages::RELEASE_OVERLAY_TEXT_LAYOUTS
        ) {
            return Some(request);
        }
        let result = self.process(&request).map_err(|message| message.to_owned());
        self.presenter
            .complete_overlay_host_request(request.id, result);
        None
    }

    /// Clones live retained layouts for compilation in this exact presenter.
    ///
    /// Released layouts remain available only while the presenter's retained
    /// scenes reference them. Producer admission still rejects new released references.
    pub fn retained_layouts(
        &mut self,
        surface: BridgeSurfaceKey,
    ) -> BTreeMap<u64, Arc<MeasuredLayout>> {
        self.prune();
        let state = lock(&self.presenter.state);
        if state
            .sessions
            .get(&surface.producer)
            .is_none_or(|session| session.closed)
        {
            return BTreeMap::new();
        }
        let key = SurfaceKey {
            session: surface.producer,
            context: surface.context,
            surface: surface.surface,
        };
        let known: HashSet<_> = state
            .overlay_layouts
            .get(&key)
            .into_iter()
            .flatten()
            .map(|layout| layout.id)
            .collect();
        self.layouts
            .iter()
            .filter_map(|((owner, id), entry)| {
                (*owner == surface && known.contains(id)).then_some((*id, entry.value.clone()))
            })
            .collect()
    }

    fn prune(&mut self) {
        let state = lock(&self.presenter.state);
        self.layouts.retain(|(surface, id), entry| {
            let key = SurfaceKey {
                session: surface.producer,
                context: surface.context,
                surface: surface.surface,
            };
            state.sessions.contains_key(&surface.producer)
                && state
                    .surfaces
                    .get(&key)
                    .is_some_and(|value| value.state.generation.get() == entry.generation)
                && (state
                    .overlay_layouts
                    .get(&key)
                    .is_some_and(|values| values.iter().any(|layout| layout.id == *id))
                    || (state
                        .sessions
                        .get(&surface.producer)
                        .is_some_and(|session| {
                            !session.closed && session.resume_generation == entry.resume
                        })
                        && state.overlay_relay.pending.contains_key(&entry.reservation)))
        });
        self.queued
            .retain(|request| state.overlay_relay.pending.contains_key(&request.id));
        self.charges
            .retain(|charge| charge.token.strong_count() != 0);
    }

    fn process(&mut self, request: &OverlayHostRequest) -> Result<Vec<u8>, &'static str> {
        let envelope =
            messages::decode_control(&request.body).map_err(|_| "invalid native text request")?;
        let value = Value::Map(envelope.payload);
        let address = match request.record_type {
            messages::MEASURE_OVERLAY_TEXT => {
                MeasureText::decode(request.surface.surface, &value)
                    .map_err(|_| "invalid text request")?
                    .address
            }
            messages::MEASURE_OVERLAY_TEXT_BATCH => {
                MeasureBatch::decode(request.surface.surface, &value)
                    .map_err(|_| "invalid text batch")?
                    .address
            }
            _ => {
                ReleaseLayouts::decode(request.surface.surface, &value)
                    .map_err(|_| "invalid text release")?
                    .address
            }
        };
        let resume = self.authorize(request, address)?;
        if request.record_type == messages::MEASURE_OVERLAY_TEXT {
            let query = MeasureText::decode(request.surface.surface, &value)
                .map_err(|_| "invalid text request")?;
            let measured = self
                .fonts
                .measure_plain(&query.text)
                .map_err(|error| error.reason())?;
            let body = Envelope::new(
                envelope.request_id,
                measured
                    .payload(address)
                    .map_err(|_| "invalid text geometry")?,
            )
            .encode()
            .map_err(|_| "text reply encoding failed")?;
            self.authorize(request, address)?;
            return bounded(body);
        }
        if request.record_type == messages::RELEASE_OVERLAY_TEXT_LAYOUTS {
            // The presenter prunes only after its live scenes release their references.
            return Envelope::new(envelope.request_id, Vec::new())
                .encode()
                .map_err(|_| "text release encoding failed");
        }
        let query = MeasureBatch::decode(request.surface.surface, &value)
            .map_err(|_| "invalid text batch")?;
        if request.layout_ids.len() != if query.retain { query.texts.len() } else { 0 } {
            return Err("native layout reservation count mismatch");
        }
        let bytes = query
            .texts
            .iter()
            .map(|text| {
                let single = MeasureBatch {
                    address,
                    texts: vec![text.clone()],
                    retain: true,
                };
                Envelope::new(1, single.payload().map_err(|_| "invalid retained text")?)
                    .encode()
                    .map(|body| body.len())
                    .map_err(|_| "retained text encoding failed")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if query.retain {
            let existing = self
                .charges
                .iter()
                .filter(|charge| charge.owner == request.surface.producer)
                .count();
            let extra = bytes
                .iter()
                .try_fold(0usize, |sum, bytes| sum.checked_add(*bytes))
                .ok_or("native text byte count overflow")?;
            let used = self
                .charges
                .iter()
                .try_fold(0usize, |sum, charge| sum.checked_add(charge.bytes))
                .ok_or("native text charge overflow")?;
            if existing
                .checked_add(query.texts.len())
                .is_none_or(|count| count > MAX_RETAINED_LAYOUTS)
                || self
                    .charges
                    .len()
                    .checked_add(query.texts.len())
                    .is_none_or(|count| {
                        count > MAX_RETAINED_LAYOUTS * vivid_protocol::overlay::MAX_OWNERS
                    })
                || used
                    .checked_add(extra)
                    .is_none_or(|size| size > MAX_RETAINED_LAYOUT_BYTES)
            {
                return Err("native retained text capacity exhausted");
            }
        }
        let mut metrics = Vec::with_capacity(query.texts.len());
        let mut painted = Vec::with_capacity(query.texts.len());
        for (index, text) in query.texts.iter().enumerate() {
            let (measurement, layout) = measure_styled(&mut self.fonts, text, query.retain)
                .map_err(|error| error.reason())?;
            metrics.push((
                request.layout_ids.get(index).copied().unwrap_or(0),
                measurement,
            ));
            painted.push(layout);
        }
        let body = bounded(
            Envelope::new(
                envelope.request_id,
                BatchMeasured {
                    address,
                    layouts: metrics,
                }
                .payload()
                .map_err(|_| "native text batch exceeds geometry limits")?,
            )
            .encode()
            .map_err(|_| "native text batch encoding failed")?,
        )?;
        self.authorize(request, address)?;
        for (index, layout) in painted.into_iter().enumerate() {
            if let Some(layout) = layout {
                let id = request.layout_ids[index];
                self.charges.push(Charge {
                    owner: request.surface.producer,
                    bytes: bytes[index],
                    token: Arc::downgrade(&layout.token),
                });
                self.layouts.insert(
                    (request.surface, id),
                    LayoutEntry {
                        reservation: request.id,
                        generation: address.generation,
                        resume,
                        value: Arc::new(layout),
                    },
                );
            }
        }
        Ok(body)
    }

    fn authorize(
        &self,
        request: &OverlayHostRequest,
        address: WindowAddress,
    ) -> Result<u64, &'static str> {
        let state = lock(&self.presenter.state);
        let session = state
            .sessions
            .get(&request.surface.producer)
            .ok_or("native text owner is gone")?;
        let key = SurfaceKey {
            session: request.surface.producer,
            context: request.surface.context,
            surface: request.surface.surface,
        };
        if session.closed
            || request.surface.context != address.context_id
            || request.surface.surface != address.surface_id
            || !state.overlay_relay.pending.contains_key(&request.id)
            || state
                .surfaces
                .get(&key)
                .is_none_or(|value| value.state.generation.get() != address.generation)
        {
            return Err("native text window is stale");
        }
        let identity = super::super::overlay_host::owner(key.session)
            .context(key.context)
            .map_err(|_| "invalid text context")?
            .surface(key.surface)
            .map_err(|_| "invalid text window")?;
        if state
            .overlays
            .window(identity)
            .is_none_or(|window| window.generation != address.generation)
        {
            return Err("native text overlay window is absent");
        }
        Ok(session.resume_generation)
    }
}

fn bounded(body: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    if body.len() > MAX_OVERLAY_HOST_BODY {
        Err("native text reply exceeds transport limit")
    } else {
        Ok(body)
    }
}

impl Drop for NativeTextHost {
    fn drop(&mut self) {
        let mut state = lock(&self.presenter.state);
        state.overlay_relay.local_host_available = false;
        state.overlay_relay.pending.clear();
        state.overlay_relay.clipboard.clear();
        state.overlay_relay.reservations.clear();
        state.overlay_relay.queued.clear();
    }
}
impl std::fmt::Debug for NativeTextHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTextHost")
            .field("retained_layouts", &self.layouts.len())
            .field("live_charges", &self.charges.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presenter::SocketListener;
    use crate::{
        ProducerAuthentication, ProducerConfig, RequestMetadata, Session, SurfaceBuilder,
        SurfaceRole,
    };
    use vivid_protocol::messages::PayloadMap;
    use vivid_protocol::overlay::wire::{
        SetWindow,
        text::styled::{StyledText, TextStyle},
    };
    use vivid_protocol::overlay::{WindowMode, WindowOptions};

    fn fixture() -> (Arc<VirtualVivid>, NativeTextHost) {
        let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
        let presenter = Arc::new(
            VirtualVivid::start_configured_eventless(
                listener,
                PresenterConfig::terminal_with_overlay(MediaConfig::default()),
            )
            .unwrap(),
        );
        let host = NativeTextHost::attach(presenter.clone(), "").unwrap();
        (presenter, host)
    }

    fn client(presenter: &VirtualVivid, pane: u64) -> (Session, WindowAddress, BridgeSurfaceKey) {
        presenter.update_metrics(pane, 80, 24, (8, 16));
        let secret = presenter.issue_pane_capability(pane).unwrap();
        let mut config = ProducerConfig {
            endpoint_control: Some(presenter.endpoint()),
            authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
            required_profiles: [
                registry::CORE_CONTROL,
                registry::LIVE_MEDIA,
                registry::TERMINAL_SURFACE,
                registry::TERMINAL_OVERLAY,
                registry::VECTOR_SCENE,
                registry::OVERLAY_INPUT,
                registry::OVERLAY_TEXT,
                registry::OVERLAY_TEXT_LAYOUT,
                registry::OVERLAY_TYPOGRAPHY,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..ProducerConfig::default()
        };
        config.required_profiles.sort();
        config
            .optional_profiles
            .retain(|profile| !config.required_profiles.contains(profile));
        window(Session::connect(config).unwrap())
    }

    fn window(mut session: Session) -> (Session, WindowAddress, BridgeSurfaceKey) {
        let definition = SurfaceBuilder::new(&session, 300, 100)
            .unwrap()
            .titled(SurfaceRole::Figure, "native text test")
            .build()
            .unwrap();
        let surface = session
            .create_surface(definition, &RequestMetadata::default())
            .unwrap();
        let address = WindowAddress {
            context_id: surface.context_id(),
            surface_id: surface.id(),
            generation: surface.generation().get(),
        };
        session
            .set_overlay_window(&SetWindow {
                address,
                expected_revision: 0,
                options: WindowOptions::new(
                    vivid_protocol::vector::Rect::new(0., 0., 300., 100.).unwrap(),
                    WindowMode::Floating,
                ),
            })
            .unwrap();
        let key = BridgeSurfaceKey {
            producer: session.info().session_id,
            context: address.context_id,
            surface: address.surface_id,
        };
        (session, address, key)
    }

    fn pump<T>(host: &mut NativeTextHost, result: &mpsc::Receiver<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(host.poll().is_none(), "unexpected platform service");
            if let Ok(value) = result.try_recv() {
                return value;
            }
            assert!(Instant::now() < deadline, "native text service stalled");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_owner_retirement(presenter: &VirtualVivid, owners: &[u64]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if owners.iter().all(|owner| {
                lock(&presenter.state)
                    .sessions
                    .get(owner)
                    .is_none_or(|session| session.closed)
            }) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "native text owner did not retire"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn measure(
        session: &Session,
        address: WindowAddress,
        texts: Vec<StyledText>,
        retain: bool,
    ) -> mpsc::Receiver<io::Result<BatchMeasured>> {
        let handle = session.overlay_host_handle().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = handle(
                messages::MEASURE_OVERLAY_TEXT_BATCH,
                address.surface_id,
                MeasureBatch {
                    address,
                    texts,
                    retain,
                }
                .payload()
                .unwrap(),
            )
            .and_then(|payload| {
                BatchMeasured::decode(address, &Value::Map(payload)).map_err(io::Error::other)
            });
            tx.send(result).unwrap();
        });
        rx
    }

    fn call(
        session: &Session,
        kind: u16,
        object: u64,
        payload: PayloadMap,
    ) -> mpsc::Receiver<io::Result<PayloadMap>> {
        let handle = session.overlay_host_handle().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            tx.send(handle(kind, object, payload)).unwrap();
        });
        rx
    }

    #[test]
    fn authenticated_native_layouts_keep_each_owners_resources_until_compiled_scenes_drop() {
        let (presenter, mut host) = fixture();
        let (first, a, owner_a) = client(&presenter, 7);
        let (second, b, owner_b) = client(&presenter, 8);
        assert_eq!((a.context_id, a.surface_id), (b.context_id, b.surface_id));
        assert_ne!(owner_a.producer, owner_b.producer);
        let paragraphs = vec![
            StyledText::new("iiii", TextStyle::default()),
            StyledText::new("WWWW", TextStyle::default()),
        ];
        let reply_a = pump(&mut host, &measure(&first, a, paragraphs.clone(), true)).unwrap();
        let reply_b = pump(&mut host, &measure(&second, b, paragraphs, true)).unwrap();
        assert!(reply_a.layouts[1].1.width.get() > reply_a.layouts[0].1.width.get() * 1.5);
        let mut canvas = vivid_protocol::vector::Canvas::new();
        canvas
            .push(vivid_protocol::vector::Command::TextLayout {
                layout: reply_a.layouts[0].0,
                origin: vivid_protocol::vector::Point::new(0., 0.).unwrap(),
            })
            .unwrap();
        let layouts = host.retained_layouts(owner_a);
        let compiled = crate::vector_renderer::compile_with_layouts(
            &canvas,
            &mut host.fonts,
            &BTreeMap::new(),
            &layouts,
        )
        .unwrap();
        drop(layouts);
        assert_eq!(compiled.retained_resource_count(), 1);
        assert_eq!(host.retained_layouts(owner_b).len(), reply_b.layouts.len());
        let release = ReleaseLayouts {
            address: a,
            ids: reply_a.layouts.iter().map(|(id, _)| *id).collect(),
        };
        pump(
            &mut host,
            &call(
                &first,
                messages::RELEASE_OVERLAY_TEXT_LAYOUTS,
                a.surface_id,
                release.payload().unwrap(),
            ),
        )
        .unwrap();
        assert!(host.retained_layouts(owner_a).is_empty());
        first.close().unwrap();
        host.prune();
        assert!(host.retained_layouts(owner_a).is_empty());
        assert_eq!(host.retained_layouts(owner_b).len(), 2);
        assert_eq!(
            host.charges
                .iter()
                .filter(|charge| charge.owner == owner_a.producer)
                .count(),
            1
        );
        drop(compiled);
        host.prune();
        assert!(
            host.charges
                .iter()
                .all(|charge| charge.owner == owner_b.producer)
        );
        second.close().unwrap();
        wait_for_owner_retirement(&presenter, &[owner_a.producer, owner_b.producer]);
        host.prune();
        assert!(host.layouts.is_empty() && host.charges.is_empty());
    }

    #[test]
    fn pending_host_work_does_not_block_control_or_another_owner_and_is_cancelled_on_close() {
        let (presenter, mut host) = fixture();
        let (first, address, owner) = client(&presenter, 7);
        let (second, peer_address, peer) = client(&presenter, 8);
        let texts = vec![StyledText::new("pending native text", TextStyle::default())];
        let pending: Vec<_> = (0..8)
            .map(|_| measure(&first, address, texts.clone(), true))
            .collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if lock(&presenter.state).overlay_relay.queued.len() == 8 {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        // The native host intentionally leaves this request unanswered. A query
        // must return before that request's ten-second host deadline.
        let start = Instant::now();
        first.query_session().unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(
            measure(&first, address, texts.clone(), true)
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        let held = presenter.take_overlay_host_requests();
        assert_eq!(held.len(), 8);
        let peer_reply = pump(&mut host, &measure(&second, peer_address, texts, true)).unwrap();
        assert_eq!(peer_reply.layouts.len(), 1);
        first.close().unwrap();
        for result in pending {
            assert!(
                result
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .is_err()
            );
        }
        // GOODBYE is acknowledged before the connection worker retires its
        // owner. Local request cancellation alone does not prove host cleanup.
        wait_for_owner_retirement(&presenter, &[owner.producer]);
        host.prune();
        assert!(host.retained_layouts(owner).is_empty());
        assert_eq!(host.retained_layouts(peer).len(), 1);
        let state = lock(&presenter.state);
        assert!(
            state
                .overlay_relay
                .reservations
                .values()
                .all(|(surface, _, _)| surface.producer != owner.producer)
        );
        drop(state);
        second.close().unwrap();
    }

    #[test]
    fn plain_measurements_use_native_fonts_and_a_dropped_host_never_falls_back() {
        let (presenter, mut host) = fixture();
        assert!(NativeTextHost::attach(presenter.clone(), "").is_err());
        let (session, address, _) = client(&presenter, 7);
        let mut widths = Vec::new();
        for source in ["iiii", "WWWW"] {
            let request = MeasureText {
                address,
                text: vivid_protocol::vector::Text {
                    text: source.into(),
                    origin: vivid_protocol::vector::Point::new(0., 0.).unwrap(),
                    size: vivid_protocol::vector::Scalar::new(16.).unwrap(),
                    family: String::new(),
                    weight: 400,
                    italic: false,
                    color: vivid_protocol::vector::Color(0xffffffff),
                    max_width: None,
                },
            };
            let payload = pump(
                &mut host,
                &call(
                    &session,
                    messages::MEASURE_OVERLAY_TEXT,
                    address.surface_id,
                    request.payload().unwrap(),
                ),
            )
            .unwrap();
            let measurement = vivid_protocol::overlay::wire::text::TextMeasurement::decode(
                address,
                &Value::Map(payload),
            )
            .unwrap();
            measurement.validate_text(source).unwrap();
            widths.push(measurement.width.get());
        }
        assert!(widths[1] > widths[0] * 1.5);
        let waiting = measure(
            &session,
            address,
            vec![StyledText::new("host is gone", TextStyle::default())],
            true,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&presenter.state).overlay_relay.queued.is_empty() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        drop(host);
        assert!(
            waiting
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        assert!(
            measure(
                &session,
                address,
                vec![StyledText::new("still gone", TextStyle::default())],
                false
            )
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err()
        );
        session.query_session().unwrap();
        session.close().unwrap();
    }

    #[test]
    fn released_layouts_held_by_compiled_scenes_still_consume_native_owner_capacity() {
        let (presenter, mut host) = fixture();
        let (first, address, owner) = client(&presenter, 7);
        let (peer, peer_address, peer_owner) = client(&presenter, 8);
        let text = StyledText::new("x", TextStyle::default());
        let mut ids = Vec::new();
        for _ in 0..MAX_RETAINED_LAYOUTS / 32 {
            let reply = pump(
                &mut host,
                &measure(&first, address, vec![text.clone(); 32], true),
            )
            .unwrap();
            ids.extend(reply.layouts.into_iter().map(|(id, _)| id));
        }
        assert_eq!(ids.len(), MAX_RETAINED_LAYOUTS);
        let mut canvas = vivid_protocol::vector::Canvas::new();
        for id in &ids {
            canvas
                .push(vivid_protocol::vector::Command::TextLayout {
                    layout: *id,
                    origin: vivid_protocol::vector::Point::new(0., 0.).unwrap(),
                })
                .unwrap();
        }
        let layouts = host.retained_layouts(owner);
        let compiled = crate::vector_renderer::compile_with_layouts(
            &canvas,
            &mut host.fonts,
            &BTreeMap::new(),
            &layouts,
        )
        .unwrap();
        drop(layouts);
        assert_eq!(compiled.retained_resource_count(), MAX_RETAINED_LAYOUTS);
        for chunk in ids.chunks(32) {
            let release = ReleaseLayouts {
                address,
                ids: chunk.to_vec(),
            };
            pump(
                &mut host,
                &call(
                    &first,
                    messages::RELEASE_OVERLAY_TEXT_LAYOUTS,
                    address.surface_id,
                    release.payload().unwrap(),
                ),
            )
            .unwrap();
        }
        assert!(host.retained_layouts(owner).is_empty());
        assert!(
            pump(
                &mut host,
                &measure(&first, address, vec![text.clone()], true)
            )
            .is_err()
        );
        pump(
            &mut host,
            &measure(&peer, peer_address, vec![text.clone()], true),
        )
        .unwrap();
        assert_eq!(host.retained_layouts(peer_owner).len(), 1);
        drop(compiled);
        pump(&mut host, &measure(&first, address, vec![text], true)).unwrap();
        assert_eq!(host.retained_layouts(owner).len(), 1);
        assert_eq!(host.retained_layouts(peer_owner).len(), 1);
        first.close().unwrap();
        peer.close().unwrap();
    }

    #[test]
    fn leased_text_resources_resume_without_accepting_the_retired_control_writer() {
        use crate::SessionLeaseBuilder;
        use vivid_protocol::lease::CleanupPolicy;
        let (presenter, mut host) = fixture();
        let profiles = lock(&presenter.state).config.supported_profiles.clone();
        let (definition, mut activation) = SessionLeaseBuilder::new(41, 9)
            .permitted_profiles(profiles.clone())
            .contract(ResourceContract::new(
                [u64::MAX / 4; vivid_protocol::resource::RESOURCE_COUNT],
            ))
            .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
            .disconnect_grace_us(5_000_000)
            .build()
            .unwrap();
        presenter.update_metrics(41, 80, 24, (8, 16));
        presenter.issue_lease(41, definition).unwrap();
        let config = |authentication| ProducerConfig {
            endpoint_control: Some(presenter.endpoint()),
            authentication,
            required_profiles: profiles.clone(),
            optional_profiles: Vec::new(),
            ..ProducerConfig::default()
        };
        let (first, address, owner) = window(
            Session::connect(config(
                ProducerAuthentication::lease_activation_bytes(41, 9, activation.take().unwrap())
                    .unwrap(),
            ))
            .unwrap(),
        );
        let (peer, peer_address, peer_owner) = client(&presenter, 7);
        let text = StyledText::new("retained across recovery", TextStyle::default());
        let initial = pump(
            &mut host,
            &measure(&first, address, vec![text.clone()], true),
        )
        .unwrap();
        pump(
            &mut host,
            &measure(&peer, peer_address, vec![text.clone()], true),
        )
        .unwrap();
        let resume = first.resume_authentication().unwrap();
        let writer = lock(&presenter.state).sessions[&owner.producer]
            .writer
            .clone();
        let pending = measure(&first, address, vec![text.clone()], true);
        let deadline = Instant::now() + Duration::from_secs(5);
        let held = loop {
            if let Some(request) = presenter.take_overlay_host_requests().pop() {
                break request;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        };
        (first.cancel_handle())();
        drop(first);
        loop {
            if lock(&presenter.state).leases[&(41, 9)].machine.state() == LeaseState::Suspended {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            pending
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        assert!(host.retained_layouts(owner).is_empty());
        assert!(host.layouts.contains_key(&(owner, initial.layouts[0].0)));
        assert_eq!(host.retained_layouts(peer_owner).len(), 1);
        let resumed = Session::connect(config(resume)).unwrap();
        assert_eq!(resumed.info().session_id, owner.producer);
        assert_eq!(resumed.info().resume_generation, 1);
        assert!(
            host.retained_layouts(owner)
                .contains_key(&initial.layouts[0].0)
        );
        let stale = Record {
            record_type: held.record_type,
            flags: 0,
            object_id: address.surface_id,
            sequence: 1,
            body: held.body,
        };
        assert!(
            dispatch_control_for_connection(&presenter.state, owner.producer, &stale, &writer)
                .is_err()
        );
        pump(&mut host, &measure(&resumed, address, vec![text], true)).unwrap();
        assert_eq!(host.retained_layouts(owner).len(), 2);
        assert_eq!(host.retained_layouts(peer_owner).len(), 1);
        resumed.close().unwrap();
        peer.close().unwrap();
        wait_for_owner_retirement(&presenter, &[owner.producer, peer_owner.producer]);
        host.prune();
        assert!(host.layouts.is_empty() && host.charges.is_empty());
    }
}
