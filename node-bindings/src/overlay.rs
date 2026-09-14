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
        parent: Option<&OverlayWindow>,
    ) -> Result<OverlayWindow> {
        let options = model::options(&bounds, &mode, title, visible).map_err(io_error)?;
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
    pub async fn present(&self, canvas: &OverlayCanvas) -> Result<()> {
        let canvas = locked(&canvas.inner, "canvas")?.clone();
        let window = self.inner.clone();
        blocking(move || window.present(canvas).map_err(io_error)).await
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
