//! Async native overlay transport; workers never call JavaScript.
use super::*;
use std::sync::Weak;
use vivid_sdk::overlay::{Canvas, Color, Command, Point, Scalar, Text};
#[path = "../../bindings/overlay_model.rs"]
mod model;

#[napi]
pub struct OverlayCanvas {
    inner: Arc<Mutex<Canvas>>,
}
#[napi]
impl OverlayCanvas {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Canvas::new())),
        }
    }
    #[napi]
    pub fn snapshot(&self) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(locked(&self.inner, "canvas")?.clone())),
        })
    }
    #[napi]
    pub fn shape(kind: String, bounds: Vec<f64>, radius: f64) -> Result<Vec<Vec<f64>>> {
        model::shape(&kind, &bounds, radius).map_err(io_error)
    }
    #[napi]
    pub fn draw(
        &self,
        segments: Vec<Vec<f64>>,
        even_odd: bool,
        kind: String,
        geometry: Vec<f64>,
        colors: Vec<f64>,
        offsets: Vec<f64>,
        width: Option<f64>,
    ) -> Result<()> {
        let path = model::path(&segments, even_odd).map_err(io_error)?;
        let brush = model::brush(&kind, &geometry, &colors, &offsets).map_err(io_error)?;
        let command = match width {
            Some(width) => Command::Stroke(path, brush, Scalar::new(width).map_err(value_error)?),
            None => Command::Fill(path, brush),
        };
        locked(&self.inner, "canvas")?
            .push(command)
            .map_err(value_error)?;
        Ok(())
    }
    #[napi]
    pub fn state(&self, kind: String, values: Vec<f64>) -> Result<()> {
        locked(&self.inner, "canvas")?
            .push(model::state(&kind, &values).map_err(io_error)?)
            .map_err(value_error)?;
        Ok(())
    }
    #[napi]
    pub fn clip(&self, segments: Vec<Vec<f64>>, even_odd: bool) -> Result<()> {
        locked(&self.inner, "canvas")?
            .push(Command::Clip(
                model::path(&segments, even_odd).map_err(io_error)?,
            ))
            .map_err(value_error)?;
        Ok(())
    }
    #[napi]
    pub fn text(
        &self,
        text: String,
        x: f64,
        y: f64,
        size: f64,
        color: f64,
        family: String,
        weight: f64,
        italic: bool,
        max_width: Option<f64>,
    ) -> Result<()> {
        if text.len() > vivid_protocol::vector::MAX_TEXT_BYTES {
            return Err(value_error("text limit exceeded"));
        }
        let value = Text {
            text,
            origin: Point::new(x, y).map_err(value_error)?,
            size: Scalar::new(size).map_err(value_error)?,
            color: Color(model::number(color, u64::from(u32::MAX)).map_err(io_error)? as u32),
            family,
            weight: model::number(weight, u64::from(u16::MAX)).map_err(io_error)? as u16,
            italic,
            max_width: max_width
                .map(Scalar::new)
                .transpose()
                .map_err(value_error)?,
        };
        locked(&self.inner, "canvas")?
            .push(Command::Text(value))
            .map_err(value_error)?;
        Ok(())
    }
    #[napi]
    pub fn hit(
        &self,
        segments: Vec<Vec<f64>>,
        even_odd: bool,
        id: BigInt,
        role: String,
        edges: f64,
    ) -> Result<()> {
        let (negative, id, lossless) = id.get_u64();
        if negative || !lossless {
            return Err(value_error("hit ID must be an unsigned 64-bit bigint"));
        }
        locked(&self.inner, "canvas")?
            .push(Command::Hit {
                id,
                path: model::path(&segments, even_odd).map_err(io_error)?,
                role: model::hit_role(&role, edges).map_err(io_error)?,
            })
            .map_err(value_error)?;
        Ok(())
    }
    #[napi]
    pub fn validate(&self) -> Result<()> {
        locked(&self.inner, "canvas")?
            .validate()
            .map_err(value_error)
    }
}

#[napi]
pub struct OverlaySession {
    inner: Arc<vivid_sdk::OverlaySession>,
}
#[napi]
impl OverlaySession {
    #[napi(factory)]
    pub async fn adopt(session: &Session) -> Result<Self> {
        let session = locked(&session.inner, "session")?
            .take()
            .ok_or_else(closed_session)?;
        blocking(move || vivid_sdk::OverlaySession::from_session(session).map_err(io_error))
            .await
            .map(|inner| Self {
                inner: Arc::new(inner),
            })
    }
    #[napi]
    pub async fn create_window(
        &self,
        bounds: Vec<f64>,
        mode: String,
        title: String,
        visible: bool,
        min_width: f64,
        min_height: f64,
        parent: Option<&OverlayWindow>,
    ) -> Result<OverlayWindow> {
        let options = model::options(&bounds, &mode, title, visible, min_width, min_height)
            .map_err(io_error)?;
        let owner = self.inner.clone();
        let parent = parent.map(|p| p.inner.clone());
        blocking(move || {
            match parent {
                Some(parent) => owner.create_child(&parent, options),
                None => owner.create_window(options),
            }
            .map_err(io_error)
        })
        .await
        .map(|inner| OverlayWindow {
            inner: Arc::new(inner),
        })
    }
    #[napi]
    pub async fn capture_pointer(&self, window: &OverlayWindow, capture: bool) -> Result<()> {
        let owner = self.inner.clone();
        let window = window.inner.clone();
        blocking(move || owner.capture_pointer(&window, capture).map_err(io_error)).await
    }
    #[napi]
    pub async fn wait_event(&self, timeout: f64) -> Result<Option<OverlayEvent>> {
        let timeout = model::timeout(timeout).map_err(io_error)?;
        let owner = self.inner.clone();
        let weak = Arc::downgrade(&owner);
        blocking(move || owner.wait_event(timeout).map_err(io_error))
            .await
            .map(|event| event.map(|inner| OverlayEvent { owner: weak, inner }))
    }
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let owner = self.inner.clone();
        blocking(move || owner.close().map_err(io_error)).await
    }
}
#[napi]
pub struct OverlayWindow {
    inner: Arc<vivid_sdk::OverlayWindow>,
}
#[napi]
impl OverlayWindow {
    #[napi]
    pub async fn submit(&self, canvas: &OverlayCanvas) -> Result<OverlaySubmission> {
        let canvas = locked(&canvas.inner, "canvas")?.clone();
        let window = self.inner.clone();
        blocking(move || {
            window
                .submit(canvas)
                .map(|inner| OverlaySubmission { inner })
                .map_err(io_error)
        })
        .await
    }
    #[napi]
    pub async fn replace_track(&self, canvas: &OverlayCanvas) -> Result<OverlaySubmission> {
        let canvas = locked(&canvas.inner, "canvas")?.clone();
        let window = self.inner.clone();
        blocking(move || {
            window
                .replace_track(canvas)
                .map(|inner| OverlaySubmission { inner })
                .map_err(io_error)
        })
        .await
    }
    #[napi]
    pub async fn release_image(&self, image: &OverlayImage) -> Result<()> {
        let image = image.inner.clone();
        let window = self.inner.clone();
        blocking(move || window.release_image(&image).map_err(io_error)).await
    }
    #[napi]
    pub async fn reconcile(&self) -> Result<OverlayWindowStatus> {
        let window = self.inner.clone();
        blocking(move || {
            let state = window.reconcile().map_err(io_error)?;
            Ok(OverlayWindowStatus {
                bounds: model::bounds(state.bounds),
                viewport: vec![
                    state.viewport.width.get(),
                    state.viewport.height.get(),
                    f64::from(state.viewport.scale_numerator),
                    f64::from(state.viewport.scale_denominator),
                ],
                viewport_revision: state.viewport_revision.into(),
                window_revision: state.window_revision.into(),
                presented_revision: state.presented_revision.into(),
                accepted_revision: state.accepted_revision.into(),
                active_revision: state.active_revision.map(Into::into),
                focused: state.focused,
            })
        })
        .await
    }
    #[napi]
    pub async fn present(&self, canvas: &OverlayCanvas) -> Result<()> {
        let canvas = locked(&canvas.inner, "canvas")?.clone();
        let window = self.inner.clone();
        blocking(move || window.present(canvas).map_err(io_error)).await
    }
    #[napi]
    pub async fn measure_text(&self, canvas: &OverlayCanvas) -> Result<OverlayTextMeasurement> {
        let text = model::measurement_text(&*locked(&canvas.inner, "canvas")?).map_err(io_error)?;
        let window = self.inner.clone();
        blocking(move || {
            let m = window.measure_text(&text).map_err(io_error)?;
            Ok(OverlayTextMeasurement {
                truncated_at: None,
                width: m.width.get(),
                height: m.height.get(),
                lines: model::text_geometry(&m.lines, &text.text, true).map_err(io_error)?,
                clusters: model::text_geometry(&m.clusters, &text.text, true).map_err(io_error)?,
            })
        })
        .await
    }
    #[napi]
    pub fn text_batch<'env>(
        &self,
        env: &'env Env,
        texts: Vec<ClassInstance<'_, OverlayStyledText>>,
        retain: bool,
    ) -> Result<PromiseRaw<'env, Vec<OverlayTextLayout>>> {
        let texts: Vec<_> = texts.iter().map(|t| t.inner.clone()).collect();
        let window = self.inner.clone();
        env.spawn_future(blocking(move || {
            let sources: Vec<_> = texts.iter().map(|t| t.text()).collect();
            let values: Vec<_> = if retain {
                window
                    .layout_text_batch(&texts)
                    .map_err(io_error)?
                    .into_iter()
                    .map(|layout| (layout.measurement().clone(), Some(layout)))
                    .collect()
            } else {
                window
                    .measure_text_batch(&texts)
                    .map_err(io_error)?
                    .into_iter()
                    .map(|m| (m, None))
                    .collect()
            };
            Ok(values
                .into_iter()
                .zip(sources)
                .map(|((measurement, inner), source)| OverlayTextLayout {
                    measurement,
                    inner,
                    source,
                })
                .collect())
        }))
    }
    #[napi]
    pub async fn draw_text_layout(
        &self,
        canvas: &OverlayCanvas,
        layout: &OverlayTextLayout,
        x: f64,
        y: f64,
    ) -> Result<()> {
        let layout = layout
            .inner
            .clone()
            .ok_or_else(|| value_error("measurement is not retained"))?;
        let origin = Point::new(x, y).map_err(value_error)?;
        let target = canvas.inner.clone();
        let window = self.inner.clone();
        blocking(move || {
            let mut command = Canvas::new();
            window
                .draw_text_layout(&mut command, &layout, origin)
                .map_err(io_error)?;
            let mut target = locked(&target, "canvas")?;
            for command in command.commands() {
                target.push(command.clone()).map_err(value_error)?;
            }
            Ok(())
        })
        .await
    }
    #[napi]
    pub async fn release_text_layout(&self, layout: &OverlayTextLayout) -> Result<()> {
        let layout = layout
            .inner
            .clone()
            .ok_or_else(|| value_error("measurement is not retained"))?;
        let window = self.inner.clone();
        blocking(move || window.release_text_layout(&layout).map_err(io_error)).await
    }
    #[napi]
    pub async fn set_editor_geometry(
        &self,
        scene_revision: BigInt,
        caret: Option<Vec<f64>>,
    ) -> Result<()> {
        let (negative, revision, lossless) = scene_revision.get_u64();
        if negative || !lossless || revision == 0 {
            return Err(value_error("scene revision must be a nonzero u64 bigint"));
        }
        let caret = caret
            .as_deref()
            .map(model::rect)
            .transpose()
            .map_err(io_error)?;
        let window = self.inner.clone();
        blocking(move || {
            window
                .set_editor_geometry(revision, caret)
                .map_err(io_error)
        })
        .await
    }
    #[napi]
    pub async fn set_bounds(&self, bounds: Vec<f64>) -> Result<()> {
        let bounds = model::rect(&bounds).map_err(io_error)?;
        let window = self.inner.clone();
        blocking(move || window.set_bounds(bounds).map_err(io_error)).await
    }
    #[napi]
    pub async fn set_visible(&self, visible: bool) -> Result<()> {
        let window = self.inner.clone();
        blocking(move || window.set_visible(visible).map_err(io_error)).await
    }
    #[napi]
    pub async fn action(&self, action: String) -> Result<()> {
        let window = self.inner.clone();
        blocking(move || {
            match action.as_str() {
                "center" => window.center(),
                "focus" => window.request_focus(),
                "raise" => window.raise(),
                "lower" => window.lower(),
                "close" => window.close(),
                _ => Err(model::invalid("unknown window action")),
            }
            .map_err(io_error)
        })
        .await
    }
    #[napi]
    pub async fn bounds(&self) -> Result<Vec<f64>> {
        let window = self.inner.clone();
        blocking(move || window.bounds().map(model::bounds).map_err(io_error)).await
    }
    #[napi]
    pub async fn viewport(&self) -> Result<Vec<f64>> {
        let window = self.inner.clone();
        blocking(move || {
            window
                .viewport()
                .map(|v| {
                    vec![
                        v.width.get(),
                        v.height.get(),
                        f64::from(v.scale_numerator),
                        f64::from(v.scale_denominator),
                    ]
                })
                .map_err(io_error)
        })
        .await
    }
    #[napi]
    pub async fn upload_rgba(&self, width: f64, height: f64, rgba: Buffer) -> Result<OverlayImage> {
        let width = model::number(width, u64::from(u32::MAX)).map_err(io_error)? as u32;
        let height = model::number(height, u64::from(u32::MAX)).map_err(io_error)? as u32;
        if rgba.len() > vivid_protocol::vector::MAX_ASSET_BYTES {
            return Err(value_error("retained image limit exceeded"));
        }
        let bytes = rgba.to_vec();
        let window = self.inner.clone();
        blocking(move || window.upload_rgba(width, height, &bytes).map_err(io_error))
            .await
            .map(|inner| OverlayImage { inner })
    }
    #[napi]
    pub async fn draw_image(
        &self,
        canvas: &OverlayCanvas,
        image: &OverlayImage,
        bounds: Vec<f64>,
        opacity: f64,
    ) -> Result<()> {
        let bounds = model::rect(&bounds).map_err(io_error)?;
        let Command::Opacity(opacity) = model::state("opacity", &[opacity]).map_err(io_error)?
        else {
            unreachable!()
        };
        let canvas = canvas.inner.clone();
        let image = image.inner.clone();
        let window = self.inner.clone();
        blocking(move || {
            let mut command = Canvas::new();
            window
                .draw_image(&mut command, &image, bounds, opacity)
                .map_err(io_error)?;
            let mut canvas = locked(&canvas, "canvas")?;
            for command in command.commands() {
                canvas.push(command.clone()).map_err(value_error)?;
            }
            Ok(())
        })
        .await
    }
}
#[napi]
pub struct OverlayImage {
    inner: vivid_sdk::overlay::RetainedImage,
}
#[napi(object)]
pub struct OverlayWindowStatus {
    pub bounds: Vec<f64>,
    pub viewport: Vec<f64>,
    pub viewport_revision: BigInt,
    pub window_revision: BigInt,
    pub presented_revision: BigInt,
    pub accepted_revision: BigInt,
    pub active_revision: Option<BigInt>,
    pub focused: bool,
}

#[napi(object)]
pub struct OverlayTextMeasurement {
    pub truncated_at: Option<u32>,
    pub width: f64,
    pub height: f64,
    pub lines: Vec<Vec<f64>>,
    pub clusters: Vec<Vec<f64>>,
}

#[napi]
pub struct OverlayStyledText {
    inner: vivid_sdk::overlay::StyledText,
}
#[napi]
impl OverlayStyledText {
    #[napi]
    pub fn typography(
        &mut self,
        overflow: String,
        letter_spacing: f64,
        word_spacing: f64,
        line_height: Option<f64>,
        ligatures: bool,
        kerning: bool,
    ) -> Result<()> {
        let typography = model::typography(
            &overflow,
            letter_spacing,
            word_spacing,
            line_height,
            ligatures,
            kerning,
        )
        .map_err(io_error)?;
        let mut text = self.inner.clone();
        text.typography = typography;
        text.validate().map_err(value_error)?;
        self.inner = text;
        Ok(())
    }
    #[napi(constructor)]
    pub fn new(
        canvas: &OverlayCanvas,
        decorations: Vec<u32>,
        max_width: Option<f64>,
        alignment: String,
        wrap: bool,
        max_lines: Option<u16>,
    ) -> Result<Self> {
        Ok(Self {
            inner: model::styled_text(
                &*locked(&canvas.inner, "canvas")?,
                &decorations,
                max_width,
                &alignment,
                wrap,
                max_lines,
            )
            .map_err(io_error)?,
        })
    }
}
#[napi]
pub struct OverlayTextLayout {
    inner: Option<vivid_sdk::overlay::RetainedTextLayout>,
    measurement: vivid_sdk::overlay::TextMeasurement,
    source: String,
}
#[napi]
impl OverlayTextLayout {
    #[napi]
    pub fn measurement(&self) -> Result<OverlayTextMeasurement> {
        Ok(OverlayTextMeasurement {
            truncated_at: model::text_offset(self.measurement.truncated_at, &self.source, true)
                .map_err(io_error)?,
            width: self.measurement.width.get(),
            height: self.measurement.height.get(),
            lines: model::text_geometry(&self.measurement.lines, &self.source, true)
                .map_err(io_error)?,
            clusters: model::text_geometry(&self.measurement.clusters, &self.source, true)
                .map_err(io_error)?,
        })
    }
}
#[napi]
pub struct OverlaySubmission {
    inner: vivid_sdk::OverlaySubmission,
}
#[napi]
impl OverlaySubmission {
    #[napi(getter)]
    pub fn revision(&self) -> BigInt {
        self.inner.revision().into()
    }
    #[napi]
    pub async fn wait(&self, timeout: f64) -> Result<Option<String>> {
        let timeout = model::timeout(timeout).map_err(io_error)?;
        let receipt = self.inner.clone();
        blocking(move || {
            receipt
                .wait(timeout)
                .map(|r| r.map(|v| model::outcome(v).into()))
                .map_err(io_error)
        })
        .await
    }
}
#[napi]
pub struct OverlayEvent {
    owner: Weak<vivid_sdk::OverlaySession>,
    inner: vivid_sdk::OverlayLaneEvent,
}
#[napi(object)]
pub struct OverlayEventData {
    pub kind: String,
    pub revision: BigInt,
    pub region: BigInt,
    pub values: Vec<f64>,
    pub text: String,
}
#[napi]
impl OverlayEvent {
    #[napi]
    pub fn targets(&self, window: &OverlayWindow) -> Result<bool> {
        match (&self.inner, self.owner.upgrade()) {
            (vivid_sdk::OverlayLaneEvent::Input(event), Some(owner)) => {
                owner.event_targets(event, &window.inner).map_err(io_error)
            }
            _ => Ok(false),
        }
    }
    #[napi]
    pub fn data(&self) -> OverlayEventData {
        let data = model::event_data(&self.inner);
        OverlayEventData {
            kind: data.kind.into(),
            revision: data.revision.into(),
            region: data.region.into(),
            values: data.values,
            text: data.text,
        }
    }
}
