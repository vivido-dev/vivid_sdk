//! Window-scoped handles for host-shaped text, reusable across vector track replacement.
use super::*;
use vivid_protocol::overlay::wire::text::styled::{BatchMeasured, MeasureBatch, ReleaseLayouts};

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
