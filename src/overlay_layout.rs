//! Window-scoped handles for host-shaped text, reusable across vector track replacement.
use super::*;
use vivid_protocol::overlay::wire::text::styled::{BatchMeasured, MeasureBatch, ReleaseLayouts};

/// An authenticated, bounded host-service handle usable away from an event-loop thread.
pub type OverlayHostHandle =
    Arc<dyn Fn(u16, u64, messages::PayloadMap) -> io::Result<messages::PayloadMap> + Send + Sync>;

impl Session {
    pub fn query_overlay_window(&self, query: Query) -> io::Result<Status> {
        let reply = self
            .request(
                messages::QUERY_OVERLAY,
                query.surface_id,
                query.payload()?,
                &RequestMetadata::default(),
                None,
                None,
            )?
            .ok_or_else(|| invalid_input("offline overlay host"))?;
        expect_record(&reply, messages::OVERLAY_STATUS, query.surface_id)?;
        Status::decode(
            owner(self),
            query.surface_id,
            &Value::Map(decoded_payload(&reply)?),
        )
        .map_err(io::Error::other)
    }
    pub fn overlay_host_handle(&self) -> Option<OverlayHostHandle> {
        let crate::session::ControlPlane::Live { writer, pending } = &self.control else {
            return None;
        };
        let control = crate::session::ControlPlane::Live {
            writer: writer.clone(),
            pending: pending.clone(),
        };
        let ids = self.next_request_id.clone();
        let profiles = self.info.accepted_profiles.clone();
        Some(Arc::new(move |kind, object, payload| {
            let (profile, reply_kind) = match kind {
                messages::MEASURE_OVERLAY_TEXT_BATCH => (
                    vivid_protocol::registry::OVERLAY_TEXT_LAYOUT,
                    messages::OVERLAY_TEXT_BATCH_MEASURED,
                ),
                messages::RELEASE_OVERLAY_TEXT_LAYOUTS => {
                    (vivid_protocol::registry::OVERLAY_TEXT_LAYOUT, messages::OK)
                }
                messages::SET_OVERLAY_EDITOR => {
                    (vivid_protocol::registry::OVERLAY_TEXT, messages::OK)
                }
                messages::SET_OVERLAY_CLIPBOARD => {
                    (vivid_protocol::registry::OVERLAY_CLIPBOARD, messages::OK)
                }
                messages::SET_OVERLAY_SEMANTICS => {
                    (vivid_protocol::registry::OVERLAY_A11Y, messages::OK)
                }
                _ => return Err(invalid_input("unsupported overlay host call")),
            };
            if !profiles.iter().any(|accepted| accepted == profile) {
                return Err(invalid_input("outer overlay host profile is unavailable"));
            }
            let id = ids
                .try_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |id| id.checked_add(1),
                )
                .map_err(|_| invalid_data("request ID exhausted"))?;
            let body = messages::Envelope::new(id, payload).encode()?;
            let reply = control
                .request(id, kind, object, &body)?
                .ok_or_else(|| invalid_data("missing host reply"))?;
            if reply.record_type == messages::ERROR {
                return Err(presenter_error(&reply.body)?);
            }
            expect_record(&reply, reply_kind, object)?;
            decoded_payload(&reply)
        }))
    }
    /// Ask the host to shape text for an existing overlay surface.
    pub fn measure_overlay_text(&self, request: &MeasureBatch) -> io::Result<BatchMeasured> {
        if !self.supports(vivid_protocol::registry::OVERLAY_TEXT_LAYOUT) {
            return Err(invalid_input(
                "presenter does not support overlay-text-layout-v1",
            ));
        }
        let reply = self
            .request(
                messages::MEASURE_OVERLAY_TEXT_BATCH,
                request.address.surface_id,
                request.payload().map_err(io::Error::other)?,
                &RequestMetadata::default(),
                None,
                None,
            )?
            .ok_or_else(|| invalid_input("offline presenter has no text service"))?;
        expect_record(
            &reply,
            messages::OVERLAY_TEXT_BATCH_MEASURED,
            request.address.surface_id,
        )?;
        BatchMeasured::decode(request.address, &Value::Map(decoded_payload(&reply)?))
            .map_err(io::Error::other)
    }

    pub fn release_overlay_layouts(&self, request: &ReleaseLayouts) -> io::Result<()> {
        if !self.supports(vivid_protocol::registry::OVERLAY_TEXT_LAYOUT) {
            return Err(invalid_input(
                "presenter does not support overlay-text-layout-v1",
            ));
        }
        let reply = self
            .request(
                messages::RELEASE_OVERLAY_TEXT_LAYOUTS,
                request.address.surface_id,
                request.payload().map_err(io::Error::other)?,
                &RequestMetadata::default(),
                None,
                None,
            )?
            .ok_or_else(|| invalid_input("offline presenter has no text service"))?;
        expect_record(&reply, messages::OK, request.address.surface_id)
    }
}

#[derive(Debug, Clone)]
pub struct RetainedTextLayout {
    session: Weak<Mutex<Option<Session>>>,
    window: WindowAddress,
    id: u64,
    measurement: TextMeasurement,
}
impl RetainedTextLayout {
    pub fn measurement(&self) -> &TextMeasurement {
        &self.measurement
    }

    /// The identity a scene draws through. A layout outlives the frame that shaped it, so a
    /// caller that keeps one needs to be able to name it.
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// Paint commands exist only under `overlay-paint-v1`. Failing here, before anything is sent,
/// gives the producer a local diagnosis instead of a channel failure on the host.
pub(super) fn validate_paint(canvas: &Canvas, session: &Session) -> io::Result<()> {
    if canvas
        .commands()
        .iter()
        .any(|command| command.requires_paint())
        && !session.supports(vivid_protocol::registry::OVERLAY_PAINT)
    {
        return Err(invalid_input("presenter does not support overlay-paint-v1"));
    }
    Ok(())
}

/// Cursors exist only under `overlay-pointer-v1`.
pub(super) fn validate_pointer(canvas: &Canvas, session: &Session) -> io::Result<()> {
    if canvas
        .commands()
        .iter()
        .any(|command| command.requires_pointer())
        && !session.supports(vivid_protocol::registry::OVERLAY_POINTER)
    {
        return Err(invalid_input(
            "presenter does not support overlay-pointer-v1",
        ));
    }
    Ok(())
}

pub(super) fn validate_references(canvas: &Canvas, layouts: &BTreeSet<u64>) -> io::Result<()> {
    for command in canvas.commands() {
        if let Command::TextLayout { layout, .. } = command
            && !layouts.contains(layout)
        {
            return Err(invalid_input("text layout is absent or released"));
        }
    }
    Ok(())
}

impl OverlayWindow {
    /// Shape a bounded batch in one host request without retaining host resources.
    pub fn measure_text_batch(&self, texts: &[StyledText]) -> io::Result<Vec<TextMeasurement>> {
        Ok(self
            .text_batch(texts, false)?
            .into_iter()
            .map(|(_, m)| m)
            .collect())
    }
    /// Shape and atomically retain a batch. Handles preserve the exact measured glyph scenes.
    pub fn layout_text_batch(&self, texts: &[StyledText]) -> io::Result<Vec<RetainedTextLayout>> {
        Ok(self
            .text_batch(texts, true)?
            .into_iter()
            .map(|(id, measurement)| RetainedTextLayout {
                session: self.session.clone(),
                window: self.address,
                id,
                measurement,
            })
            .collect())
    }
    pub fn layout_text(&self, text: &StyledText) -> io::Result<RetainedTextLayout> {
        self.layout_text_batch(std::slice::from_ref(text))?
            .pop()
            .ok_or_else(|| invalid_data("missing layout result"))
    }
    fn text_batch(
        &self,
        texts: &[StyledText],
        retain: bool,
    ) -> io::Result<Vec<(u64, TextMeasurement)>> {
        self.with_state(|session, state| {
            if texts.iter().any(|t| t.typography != Default::default())
                && !session.supports(vivid_protocol::registry::OVERLAY_TYPOGRAPHY)
            {
                return Err(invalid_input(
                    "presenter does not support overlay-typography-v1",
                ));
            }
            if !session.supports(vivid_protocol::registry::OVERLAY_TEXT_LAYOUT) {
                return Err(invalid_input(
                    "presenter does not support overlay-text-layout-v1",
                ));
            }
            let request = MeasureBatch {
                address: self.address,
                texts: texts.to_vec(),
                retain,
            };
            let reply = session
                .request(
                    messages::MEASURE_OVERLAY_TEXT_BATCH,
                    self.address.surface_id,
                    request.payload().map_err(io::Error::other)?,
                    &RequestMetadata::default(),
                    None,
                    None,
                )?
                .ok_or_else(|| invalid_input("offline presenter has no text service"))?;
            expect_record(
                &reply,
                messages::OVERLAY_TEXT_BATCH_MEASURED,
                self.address.surface_id,
            )?;
            let measured =
                BatchMeasured::decode(self.address, &Value::Map(decoded_payload(&reply)?))
                    .map_err(io::Error::other)?;
            if measured.layouts.len() != texts.len() {
                return Err(invalid_data("text batch result count mismatch"));
            }
            let mut ids = BTreeSet::new();
            for ((id, m), text) in measured.layouts.iter().zip(texts) {
                if m.truncated_at.is_some() && text.typography.overflow != TextOverflow::Ellipsis {
                    return Err(invalid_data("unexpected ellipsis geometry"));
                }
                if retain && (*id == 0 || !ids.insert(*id) || state.layouts.contains(id))
                    || !retain && *id != 0
                {
                    return Err(invalid_data("invalid retained layout identity"));
                }
                m.validate_text(&text.text()).map_err(io::Error::other)?;
            }
            state.layouts.extend(ids);
            Ok(measured.layouts)
        })
    }
    /// Append the measured glyph scene at a window-local origin. Handles cannot cross windows.
    pub fn draw_text_layout(
        &self,
        canvas: &mut Canvas,
        layout: &RetainedTextLayout,
        origin: Point,
    ) -> io::Result<()> {
        self.with_state(|_, state| {
            self.check_text_layout(state, layout)?;
            canvas
                .push(Command::TextLayout {
                    layout: layout.id,
                    origin,
                })
                .map_err(io::Error::other)?;
            Ok(())
        })
    }
    /// Remove this layout from future lookup. Already accepted scenes keep their references.
    pub fn release_text_layout(&self, layout: &RetainedTextLayout) -> io::Result<()> {
        self.with_state(|session, state| {
            self.check_text_layout(state, layout)?;
            let request = ReleaseLayouts {
                address: self.address,
                ids: vec![layout.id],
            };
            let reply = session
                .request(
                    messages::RELEASE_OVERLAY_TEXT_LAYOUTS,
                    self.address.surface_id,
                    request.payload().map_err(io::Error::other)?,
                    &RequestMetadata::default(),
                    None,
                    None,
                )?
                .ok_or_else(|| invalid_input("offline presenter has no text service"))?;
            expect_record(&reply, messages::OK, self.address.surface_id)?;
            state.layouts.remove(&layout.id);
            Ok(())
        })
    }
    fn check_text_layout(
        &self,
        state: &WindowState,
        layout: &RetainedTextLayout,
    ) -> io::Result<()> {
        if !Weak::ptr_eq(&self.session, &layout.session)
            || layout.window != self.address
            || !state.layouts.contains(&layout.id)
        {
            return Err(invalid_input(
                "text layout belongs to another window or was released",
            ));
        }
        Ok(())
    }
}
