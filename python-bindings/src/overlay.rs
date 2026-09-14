//! Opaque overlay handles. Every socket operation releases the GIL.
use super::*;
use std::sync::{Arc, Weak};
use vivid_sdk::overlay::{Canvas, Color, Command, Point, Scalar, Text};
#[path = "../../bindings/overlay_model.rs"]
mod model;
fn value_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

#[pyclass(name = "OverlayCanvas", module = "vivid_sdk._native")]
struct PyCanvas {
    inner: Mutex<Canvas>,
}
#[pymethods]
impl PyCanvas {
    #[new]
    fn new() -> Self {
        Self {
            inner: Mutex::new(Canvas::new()),
        }
    }
    fn snapshot(&self) -> PyResult<Self> {
        Ok(Self {
            inner: Mutex::new(lock(&self.inner, "canvas")?.clone()),
        })
    }
    #[staticmethod]
    fn shape(kind: &str, bounds: Vec<f64>, radius: f64) -> PyResult<Vec<Vec<f64>>> {
        model::shape(kind, &bounds, radius).map_err(io_error)
    }
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        segments: Vec<Vec<f64>>,
        even_odd: bool,
        kind: &str,
        geometry: Vec<f64>,
        colors: Vec<f64>,
        offsets: Vec<f64>,
        width: Option<f64>,
    ) -> PyResult<()> {
        let path = model::path(&segments, even_odd).map_err(io_error)?;
        let brush = model::brush(kind, &geometry, &colors, &offsets).map_err(io_error)?;
        let command = match width {
            Some(width) => Command::Stroke(path, brush, Scalar::new(width).map_err(value_error)?),
            None => Command::Fill(path, brush),
        };
        lock(&self.inner, "canvas")?
            .push(command)
            .map_err(value_error)?;
        Ok(())
    }
    fn state(&self, kind: &str, values: Vec<f64>) -> PyResult<()> {
        lock(&self.inner, "canvas")?
            .push(model::state(kind, &values).map_err(io_error)?)
            .map_err(value_error)?;
        Ok(())
    }
    fn clip(&self, segments: Vec<Vec<f64>>, even_odd: bool) -> PyResult<()> {
        lock(&self.inner, "canvas")?
            .push(Command::Clip(
                model::path(&segments, even_odd).map_err(io_error)?,
            ))
            .map_err(value_error)?;
        Ok(())
    }
    fn text(
        &self,
        text: String,
        x: f64,
        y: f64,
        size: f64,
        color: u32,
        family: String,
        weight: u16,
        italic: bool,
        max_width: Option<f64>,
    ) -> PyResult<()> {
        if text.len() > vivid_protocol::vector::MAX_TEXT_BYTES {
            return Err(value_error("text limit exceeded"));
        }
        let value = Text {
            text,
            origin: Point::new(x, y).map_err(value_error)?,
            size: Scalar::new(size).map_err(value_error)?,
            color: Color(color),
            family,
            weight,
            italic,
            max_width: max_width
                .map(Scalar::new)
                .transpose()
                .map_err(value_error)?,
        };
        lock(&self.inner, "canvas")?
            .push(Command::Text(value))
            .map_err(value_error)?;
        Ok(())
    }
    fn hit(
        &self,
        segments: Vec<Vec<f64>>,
        even_odd: bool,
        id: u64,
        role: &str,
        edges: f64,
    ) -> PyResult<()> {
        lock(&self.inner, "canvas")?
            .push(Command::Hit {
                id,
                path: model::path(&segments, even_odd).map_err(io_error)?,
                role: model::hit_role(role, edges).map_err(io_error)?,
            })
            .map_err(value_error)?;
        Ok(())
    }
    fn validate(&self) -> PyResult<()> {
        lock(&self.inner, "canvas")?.validate().map_err(value_error)
    }
}

#[pyclass(name = "OverlaySession", module = "vivid_sdk._native")]
struct PyOverlaySession {
    inner: Arc<vivid_sdk::OverlaySession>,
}
#[pymethods]
impl PyOverlaySession {
    #[staticmethod]
    fn adopt(py: Python<'_>, session: &PySession) -> PyResult<Self> {
        let session = session
            .inner
            .lock_py_attached(py)
            .map_err(|_| value_error("session lock poisoned"))?
            .take()
            .ok_or_else(|| ClosedHandleError::new_err("session is closed"))?;
        py.detach(|| vivid_sdk::OverlaySession::from_session(session))
            .map(|inner| Self {
                inner: Arc::new(inner),
            })
            .map_err(io_error)
    }
    fn create_window(
        &self,
        py: Python<'_>,
        bounds: Vec<f64>,
        mode: &str,
        title: String,
        visible: bool,
        parent: Option<&PyOverlayWindow>,
    ) -> PyResult<PyOverlayWindow> {
        let options = model::options(&bounds, mode, title, visible).map_err(io_error)?;
        py.detach(|| match parent {
            Some(parent) => self.inner.create_child(&parent.inner, options),
            None => self.inner.create_window(options),
        })
        .map(|inner| PyOverlayWindow {
            inner: Arc::new(inner),
        })
        .map_err(io_error)
    }
    fn capture_pointer(
        &self,
        py: Python<'_>,
        window: &PyOverlayWindow,
        capture: bool,
    ) -> PyResult<()> {
        py.detach(|| self.inner.capture_pointer(&window.inner, capture))
            .map_err(io_error)
    }
    fn wait_event(&self, py: Python<'_>, timeout: f64) -> PyResult<Option<PyOverlayEvent>> {
        let timeout = model::timeout(timeout).map_err(io_error)?;
        py.detach(|| self.inner.wait_event(timeout))
            .map(|event| {
                event.map(|inner| PyOverlayEvent {
                    owner: Arc::downgrade(&self.inner),
                    inner,
                })
            })
            .map_err(io_error)
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.inner.close()).map_err(io_error)
    }
}
#[pyclass(name = "OverlayWindow", module = "vivid_sdk._native")]
struct PyOverlayWindow {
    inner: Arc<vivid_sdk::OverlayWindow>,
}
#[pymethods]
impl PyOverlayWindow {
    fn present(&self, py: Python<'_>, canvas: &PyCanvas) -> PyResult<()> {
        let canvas = lock(&canvas.inner, "canvas")?.clone();
        py.detach(|| self.inner.present(canvas)).map_err(io_error)
    }
    fn set_bounds(&self, py: Python<'_>, bounds: Vec<f64>) -> PyResult<()> {
        let bounds = model::rect(&bounds).map_err(io_error)?;
        py.detach(|| self.inner.set_bounds(bounds))
            .map_err(io_error)
    }
    fn set_visible(&self, py: Python<'_>, visible: bool) -> PyResult<()> {
        py.detach(|| self.inner.set_visible(visible))
            .map_err(io_error)
    }
    fn action(&self, py: Python<'_>, action: &str) -> PyResult<()> {
        py.detach(|| match action {
            "center" => self.inner.center(),
            "focus" => self.inner.request_focus(),
            "raise" => self.inner.raise(),
            "lower" => self.inner.lower(),
            "close" => self.inner.close(),
            _ => Err(model::invalid("unknown window action")),
        })
        .map_err(io_error)
    }
    fn bounds(&self, py: Python<'_>) -> PyResult<Vec<f64>> {
        py.detach(|| self.inner.bounds())
            .map(model::bounds)
            .map_err(io_error)
    }
    fn viewport(&self, py: Python<'_>) -> PyResult<(f64, f64, u32, u32)> {
        py.detach(|| self.inner.viewport())
            .map(|v| {
                (
                    v.width.get(),
                    v.height.get(),
                    v.scale_numerator,
                    v.scale_denominator,
                )
            })
            .map_err(io_error)
    }
    fn upload_rgba(
        &self,
        py: Python<'_>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> PyResult<PyOverlayImage> {
        py.detach(|| self.inner.upload_rgba(width, height, rgba))
            .map(|inner| PyOverlayImage { inner })
            .map_err(io_error)
    }
    fn draw_image(
        &self,
        py: Python<'_>,
        canvas: &PyCanvas,
        image: &PyOverlayImage,
        bounds: Vec<f64>,
        opacity: f64,
    ) -> PyResult<()> {
        let bounds = model::rect(&bounds).map_err(io_error)?;
        let Command::Opacity(opacity) = model::state("opacity", &[opacity]).map_err(io_error)?
        else {
            unreachable!()
        };
        py.detach(|| {
            let mut command = Canvas::new();
            self.inner
                .draw_image(&mut command, &image.inner, bounds, opacity)?;
            let mut canvas = canvas
                .inner
                .lock()
                .map_err(|_| model::invalid("canvas lock poisoned"))?;
            for command in command.commands() {
                canvas.push(command.clone()).map_err(io::Error::other)?;
            }
            Ok(())
        })
        .map_err(io_error)
    }
}
#[pyclass(name = "OverlayImage", module = "vivid_sdk._native")]
struct PyOverlayImage {
    inner: vivid_sdk::overlay::RetainedImage,
}
#[pyclass(name = "OverlayEvent", module = "vivid_sdk._native")]
struct PyOverlayEvent {
    owner: Weak<vivid_sdk::OverlaySession>,
    inner: vivid_sdk::OverlayLaneEvent,
}
#[pymethods]
impl PyOverlayEvent {
    fn targets(&self, window: &PyOverlayWindow) -> PyResult<bool> {
        match (&self.inner, self.owner.upgrade()) {
            (vivid_sdk::OverlayLaneEvent::Input(event), Some(owner)) => {
                owner.event_targets(event, &window.inner).map_err(io_error)
            }
            _ => Ok(false),
        }
    }
    fn data(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let data = model::event_data(&self.inner);
        let dict = PyDict::new(py);
        dict.set_item("kind", data.kind)?;
        dict.set_item("revision", data.revision)?;
        dict.set_item("region", data.region)?;
        dict.set_item("values", data.values)?;
        dict.set_item("text", data.text)?;
        Ok(dict.unbind())
    }
}
pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyCanvas>()?;
    module.add_class::<PyOverlaySession>()?;
    module.add_class::<PyOverlayWindow>()?;
    module.add_class::<PyOverlayImage>()?;
    module.add_class::<PyOverlayEvent>()?;
    Ok(())
}
