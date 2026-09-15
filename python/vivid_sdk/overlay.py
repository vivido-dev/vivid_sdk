"""Typed pane overlays backed by the shared Rust window and Canvas implementation."""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, ClassVar, Iterator, Literal, Optional, Protocol, Sequence, Tuple, cast

from . import _native, connect as _connect
from . import PROFILE_CORE, PROFILE_LIVE_MEDIA, PROFILE_TERMINAL_SURFACE, PROFILE_TERMINAL_OVERLAY, PROFILE_VECTOR_SCENE, PROFILE_OVERLAY_INPUT, PROFILE_OVERLAY_TEXT, PROFILE_OVERLAY_TEXT_LAYOUT, PROFILE_OVERLAY_TYPOGRAPHY

WindowMode = Literal["floating", "popup", "modal"]
HitRole = Literal["input", "drag", "resize", "transparent"]
DismissReason = Literal["escape", "outside-press", "closed", "owner-lost", "parent-closed"]
ScrollPhase = Literal["none", "began", "changed", "ended", "cancelled"]
_SCROLL_PHASES: Tuple[ScrollPhase, ...] = ("none", "began", "changed", "ended", "cancelled")

class Modifiers:
    """Normative overlay modifier bits. A host never forwards its platform bitmask."""
    SHIFT = 1
    CONTROL = 2
    ALT = 4
    SUPER = 8
    CAPS_LOCK = 16
    NUM_LOCK = 32
    KNOWN_MASK = SHIFT | CONTROL | ALT | SUPER | CAPS_LOCK | NUM_LOCK

class MouseButton:
    """Normative overlay pointer buttons, shared with desktop-surface-v1."""
    PRIMARY = 0
    AUXILIARY = 1
    SECONDARY = 2
    BACK = 3
    FORWARD = 4
    MAXIMUM = 31

class Key:
    """Physical keys are USB HID keyboard-page usages; zero is a key the page does not name."""
    UNMAPPED = 0
    FIRST_USAGE = 0x04
    LAST_USAGE = 0xE7

@dataclass(frozen=True)
class Point:
    x: float
    y: float

@dataclass(frozen=True)
class Rect:
    x: float
    y: float
    width: float
    height: float
    def _values(self) -> Tuple[float, float, float, float]:
        return self.x, self.y, self.width, self.height

@dataclass(frozen=True)
class Viewport:
    width: float
    height: float
    scale_numerator: int
    scale_denominator: int

@dataclass(frozen=True)
class OverlayWindowOptions:
    bounds: Rect
    mode: WindowMode = "floating"
    title: str = ""
    visible: bool = True
    min_width: float = 1.0
    min_height: float = 1.0

class Path:
    """Mutable Bézier builder. Canvas commands snapshot paths when added."""
    def __init__(self, *, even_odd: bool = False) -> None:
        self.even_odd = even_odd
        self._segments: list[list[float]] = []
    def _add(self, *values: float) -> Path:
        if len(self._segments) >= 4096:
            raise ValueError("path segment limit exceeded")
        self._segments.append(list(values))
        return self
    def move_to(self, x: float, y: float) -> Path: return self._add(0, x, y)
    def line_to(self, x: float, y: float) -> Path: return self._add(1, x, y)
    def quad_to(self, cx: float, cy: float, x: float, y: float) -> Path: return self._add(2, cx, cy, x, y)
    def cubic_to(self, ax: float, ay: float, bx: float, by: float, x: float, y: float) -> Path: return self._add(3, ax, ay, bx, by, x, y)
    def close(self) -> Path: return self._add(4)
    @staticmethod
    def _shape(kind: str, bounds: Rect, radius: float = 0) -> Path:
        path = Path()
        path._segments = _native.OverlayCanvas.shape(kind, bounds._values(), radius)
        return path
    @staticmethod
    def rectangle(bounds: Rect) -> Path: return Path._shape("rectangle", bounds)
    @staticmethod
    def rounded_rectangle(bounds: Rect, radius: float) -> Path: return Path._shape("rounded", bounds, radius)
    @staticmethod
    def ellipse(bounds: Rect) -> Path: return Path._shape("ellipse", bounds)

@dataclass(frozen=True)
class GradientStop:
    offset: float
    color: int

@dataclass(frozen=True)
class Brush:
    """Colors are straight-alpha sRGB 0xRRGGBBAA; gradient offsets are in [0, 1]."""
    _kind: str
    _geometry: Tuple[float, ...]
    _colors: Tuple[int, ...]
    _offsets: Tuple[float, ...]
    @staticmethod
    def solid(color: int) -> Brush: return Brush("solid", (), (color,), ())
    @staticmethod
    def linear(start: Point, end: Point, stops: Sequence[GradientStop]) -> Brush:
        return Brush("linear", (start.x, start.y, end.x, end.y), tuple(s.color for s in stops), tuple(s.offset for s in stops))
    @staticmethod
    def radial(center: Point, radius: float, stops: Sequence[GradientStop]) -> Brush:
        return Brush("radial", (center.x, center.y, radius), tuple(s.color for s in stops), tuple(s.offset for s in stops))

class Canvas:
    def __init__(self) -> None: self._raw = _native.OverlayCanvas()
    def snapshot(self) -> Canvas:
        result = Canvas()
        result._raw = self._raw.snapshot()
        return result
    def fill(self, path: Path, brush: Brush) -> Canvas:
        self._raw.draw(path._segments, path.even_odd, brush._kind, brush._geometry, brush._colors, brush._offsets, None)
        return self
    def stroke(self, path: Path, brush: Brush, width: float) -> Canvas:
        self._raw.draw(path._segments, path.even_odd, brush._kind, brush._geometry, brush._colors, brush._offsets, width)
        return self
    def save(self) -> Canvas:
        self._raw.state("save", []); return self
    def restore(self) -> Canvas:
        self._raw.state("restore", []); return self
    def opacity(self, value: float) -> Canvas:
        self._raw.state("opacity", [value]); return self
    def transform(self, a: float, b: float, c: float, d: float, e: float, f: float) -> Canvas:
        self._raw.state("transform", [a, b, c, d, e, f]); return self
    def clip(self, path: Path) -> Canvas:
        self._raw.clip(path._segments, path.even_odd); return self
    def text(self, text: str, origin: Point, size: float, color: int, *, family: str = "", weight: int = 400, italic: bool = False, max_width: Optional[float] = None) -> Canvas:
        self._raw.text(text, origin.x, origin.y, size, color, family, weight, italic, max_width)
        return self
    def hit(self, application_id: int, path: Path, role: HitRole = "input", *, edges: int = 0) -> Canvas:
        """Resize edges: left=1, right=2, top=4, bottom=8. IDs are unsigned 64-bit."""
        self._raw.hit(path._segments, path.even_odd, application_id, role, edges); return self
    def validate(self) -> None: self._raw.validate()

class RetainedImage:
    def __init__(self, raw: Any) -> None: self._raw = raw

@dataclass(frozen=True)
class TextGeometry:
    start: int
    end: int
    bounds: Rect
    baseline: float
    rtl: bool

@dataclass(frozen=True)
class TextMeasurement:
    width: float
    height: float
    lines: Tuple[TextGeometry, ...]
    clusters: Tuple[TextGeometry, ...]
    truncated_at: Optional[int] = None

def _text_measurement(raw: Any) -> TextMeasurement:
    def geometry(values: Any) -> Tuple[TextGeometry, ...]:
        return tuple(TextGeometry(int(v[0]), int(v[1]), Rect(*v[2:6]), v[6], bool(v[7])) for v in values)
    return TextMeasurement(raw["width"], raw["height"], geometry(raw["lines"]), geometry(raw["clusters"]), raw.get("truncated_at"))

PresentationOutcome = Literal["presented", "superseded"]

@dataclass(frozen=True)
class TextStyle:
    size: float = 16
    family: str = ""
    weight: int = 400
    italic: bool = False
    color: int = 0xFFFFFFFF
    underline: bool = False
    strikethrough: bool = False

@dataclass(frozen=True)
class TextRun:
    text: str
    style: TextStyle = field(default_factory=TextStyle)

@dataclass(frozen=True)
class StyledText:
    runs: Sequence[TextRun]
    max_width: Optional[float] = None
    alignment: Literal["start", "center", "end", "justify"] = "start"
    wrap: bool = True
    max_lines: Optional[int] = None
    overflow: Literal["clip", "ellipsis"] = "clip"
    letter_spacing: float = 0
    word_spacing: float = 0
    line_height: Optional[float] = None
    ligatures: bool = True
    kerning: bool = True
    def __post_init__(self) -> None: object.__setattr__(self, "runs", tuple(self.runs))
    def _native(self) -> Any:
        canvas = Canvas()
        decorations = []
        for run in self.runs:
            style = run.style
            canvas.text(run.text, Point(0, 0), style.size, style.color, family=style.family, weight=style.weight, italic=style.italic)
            decorations.append(int(style.underline) | (int(style.strikethrough) << 1))
        result = _native.OverlayStyledText(canvas._raw, decorations, self.max_width, self.alignment, self.wrap, self.max_lines)
        result.typography(self.overflow, self.letter_spacing, self.word_spacing, self.line_height, self.ligatures, self.kerning)
        return result

class RetainedTextLayout:
    """Opaque, immutable host layout. Release explicitly through its owning window."""
    def __init__(self, raw: Any) -> None:
        self._raw = raw
        self._measurement = _text_measurement(raw.measurement())
    @property
    def measurement(self) -> TextMeasurement: return self._measurement

class OverlaySubmission:
    """A specific submission. Timeout leaves the receipt usable; lane loss raises OSError."""
    def __init__(self, raw: Any) -> None: self._raw = raw
    @property
    def revision(self) -> int: return int(self._raw.revision)
    def wait(self, timeout: float = 0.25) -> Optional[PresentationOutcome]:
        return cast(Optional[PresentationOutcome], self._raw.wait(timeout))

@dataclass(frozen=True)
class OverlayWindowStatus:
    bounds: Rect
    viewport: Viewport
    viewport_revision: int
    window_revision: int
    presented_revision: int
    accepted_revision: int
    active_revision: Optional[int]
    focused: bool

class _WindowHandle(Protocol):
    @property
    def _raw(self) -> Any: ...

@dataclass(frozen=True)
class OverlayEvent:
    scene_revision: int
    _raw: Any = field(repr=False, compare=False)
    kind: ClassVar[str]
    def targets(self, window: _WindowHandle) -> bool:
        return bool(self._raw.targets(window._raw))

@dataclass(frozen=True)
class PointerEvent(OverlayEvent):
    kind: ClassVar[str] = "pointer"
    position: Point
    application_id: int
    modifiers: int
    button: Optional[int]
    down: Optional[bool]

@dataclass(frozen=True)
class WheelEvent(OverlayEvent):
    kind: ClassVar[str] = "wheel"
    position: Point
    dx: float
    dy: float
    modifiers: int
    precise: bool
    phase: ScrollPhase

@dataclass(frozen=True)
class KeyEvent(OverlayEvent):
    kind: ClassVar[str] = "key"
    physical: int
    down: bool
    repeat: bool
    modifiers: int

@dataclass(frozen=True)
class TextEvent(OverlayEvent):
    kind: ClassVar[str] = "text"
    text: str

@dataclass(frozen=True)
class ImeEvent(OverlayEvent):
    kind: ClassVar[str] = "ime"
    preedit: str
    selection: Optional[Tuple[int, int]]  # Python character offsets, not UTF-8 byte offsets.

@dataclass(frozen=True)
class FocusEvent(OverlayEvent):
    kind: ClassVar[str] = "focus"
    focused: bool

@dataclass(frozen=True)
class GeometryEvent(OverlayEvent):
    kind: ClassVar[str] = "geometry"
    bounds: Rect
    settled: bool

@dataclass(frozen=True)
class DismissedEvent(OverlayEvent):
    kind: ClassVar[str] = "dismissed"
    reason: DismissReason

@dataclass(frozen=True)
class CancelEvent(OverlayEvent):
    kind: ClassVar[str] = "cancel"

@dataclass(frozen=True)
class ConnectionLostEvent(OverlayEvent):
    kind: ClassVar[str] = "connection-lost"
    diagnostic: str

@dataclass(frozen=True)
class ViewportEvent(OverlayEvent):
    kind: ClassVar[str] = "viewport"
    revision: int
    viewport: Viewport

@dataclass(frozen=True)
class SubmissionOutcomeEvent(OverlayEvent):
    kind: ClassVar[str] = "submission-outcome"
    outcome: PresentationOutcome

def _event(raw: Any) -> OverlayEvent:
    data = raw.data()
    kind, revision, values, text = data["kind"], data["revision"], data["values"], data["text"]
    if kind == "viewport": return ViewportEvent(0, raw, revision, Viewport(values[0], values[1], int(values[2]), int(values[3])))
    if kind == "submission-outcome": return SubmissionOutcomeEvent(revision, raw, cast(PresentationOutcome, text))
    if kind == "pointer": return PointerEvent(revision, raw, Point(*values[:2]), data["region"], int(values[2]), int(values[3]) if len(values) > 3 else None, bool(values[4]) if len(values) > 3 else None)
    if kind == "wheel": return WheelEvent(revision, raw, Point(*values[:2]), values[2], values[3], int(values[4]), bool(values[5]), _SCROLL_PHASES[int(values[6])])
    if kind == "key": return KeyEvent(revision, raw, int(values[0]), bool(values[1]), bool(values[2]), int(values[3]))
    if kind == "text": return TextEvent(revision, raw, text)
    if kind == "ime":
        encoded = text.encode("utf-8")
        selection = (len(encoded[:int(values[0])].decode("utf-8")), len(encoded[:int(values[1])].decode("utf-8"))) if values else None
        return ImeEvent(revision, raw, text, selection)
    if kind == "geometry": return GeometryEvent(revision, raw, Rect(*values[:4]), bool(values[4]))
    if kind == "focus": return FocusEvent(revision, raw, bool(values[0]))
    if kind == "dismissed": return DismissedEvent(revision, raw, cast(DismissReason, text))
    if kind == "cancel": return CancelEvent(revision, raw)
    if kind == "connection-lost": return ConnectionLostEvent(revision, raw, text)
    raise ValueError("unknown native overlay event")

class OverlaySession:
    def __init__(self, raw: Any) -> None:
        self._raw = raw
        self.closed = False
    @classmethod
    def connect(cls, **options: Any) -> OverlaySession:
        required = set(options.pop("required_profiles", ()) or ())
        required.update((PROFILE_CORE, PROFILE_LIVE_MEDIA, PROFILE_TERMINAL_SURFACE, PROFILE_TERMINAL_OVERLAY, PROFILE_VECTOR_SCENE, PROFILE_OVERLAY_INPUT))
        options["optional_profiles"] = sorted({p for p in (*(options.get("optional_profiles", ()) or ()), PROFILE_OVERLAY_TEXT, PROFILE_OVERLAY_TEXT_LAYOUT, PROFILE_OVERLAY_TYPOGRAPHY) if p not in required})
        options["target_profile"] = PROFILE_TERMINAL_SURFACE
        session = _connect(required_profiles=sorted(required), **options)
        return cls(_native.OverlaySession.adopt(session))
    @classmethod
    def from_env(cls) -> OverlaySession: return cls.connect()
    def create_window(self, options: OverlayWindowOptions, *, parent: Optional[OverlayWindow] = None) -> OverlayWindow:
        return OverlayWindow(self._raw.create_window(options.bounds._values(), options.mode, options.title, options.visible, options.min_width, options.min_height, parent._raw if parent else None))
    def capture_pointer(self, window: OverlayWindow, capture: bool = True) -> None:
        self._raw.capture_pointer(window._raw, capture)
    def wait_event(self, timeout: float = 0.25) -> Optional[OverlayEvent]:
        """Wait at most timeout seconds (0–60), independently of bulk traffic."""
        if self.closed: return None
        raw = self._raw.wait_event(timeout)
        return _event(raw) if raw is not None else None
    def events(self, timeout: float = 0.25) -> Iterator[OverlayEvent]:
        while not self.closed:
            event = self.wait_event(timeout)
            if event is not None:
                yield event
                if isinstance(event, ConnectionLostEvent): return
    def close(self) -> None:
        if not self.closed:
            self.closed = True
            self._raw.close()
    def __enter__(self) -> OverlaySession: return self
    def __exit__(self, *args: Any) -> None: self.close()

class OverlayWindow:
    def measure_text_batch(self, texts: Sequence[StyledText]) -> Tuple[TextMeasurement, ...]:
        return tuple(_text_measurement(raw.measurement()) for raw in self._raw.text_batch([text._native() for text in texts], False))
    def layout_text_batch(self, texts: Sequence[StyledText]) -> Tuple[RetainedTextLayout, ...]:
        return tuple(RetainedTextLayout(raw) for raw in self._raw.text_batch([text._native() for text in texts], True))
    def layout_text(self, text: StyledText) -> RetainedTextLayout: return self.layout_text_batch((text,))[0]
    def draw_text_layout(self, canvas: Canvas, layout: RetainedTextLayout, origin: Point) -> None:
        self._raw.draw_text_layout(canvas._raw, layout._raw, origin.x, origin.y)
    def release_text_layout(self, layout: RetainedTextLayout) -> None: self._raw.release_text_layout(layout._raw)
    def measure_text(self, text: str, size: float, *, family: str = "", weight: int = 400, italic: bool = False, max_width: Optional[float] = None) -> TextMeasurement:
        canvas = Canvas().text(text, Point(0, 0), size, 0xFFFFFFFF, family=family, weight=weight, italic=italic, max_width=max_width)
        return _text_measurement(self._raw.measure_text(canvas._raw))
    def set_editor_geometry(self, scene_revision: int, caret: Optional[Rect]) -> None:
        self._raw.set_editor_geometry(scene_revision, None if caret is None else [caret.x, caret.y, caret.width, caret.height])
    def __init__(self, raw: Any) -> None:
        self._raw = raw
        self.closed = False
    def present(self, canvas: Canvas) -> None:
        """Submit an atomic snapshot; success does not acknowledge GPU presentation."""
        self._raw.present(canvas._raw)
    def submit(self, canvas: Canvas) -> OverlaySubmission:
        return OverlaySubmission(self._raw.submit(canvas._raw))
    def replace_track(self, canvas: Canvas) -> OverlaySubmission:
        """Prime and activate a fresh track; old retained images cannot appear in canvas."""
        return OverlaySubmission(self._raw.replace_track(canvas._raw))
    def release_image(self, image: RetainedImage) -> None: self._raw.release_image(image._raw)
    def reconcile(self) -> OverlayWindowStatus:
        state = self._raw.reconcile()
        return OverlayWindowStatus(Rect(*state["bounds"]), Viewport(*state["viewport"]), state["viewport_revision"], state["window_revision"], state["presented_revision"], state["accepted_revision"], state["active_revision"], state["focused"])
    def set_bounds(self, bounds: Rect) -> None: self._raw.set_bounds(bounds._values())
    def set_visible(self, visible: bool) -> None: self._raw.set_visible(visible)
    def center(self) -> None: self._raw.action("center")
    def request_focus(self) -> None: self._raw.action("focus")
    def raise_window(self) -> None: self._raw.action("raise")
    def lower(self) -> None: self._raw.action("lower")
    def bounds(self) -> Rect: return Rect(*self._raw.bounds())
    def viewport(self) -> Viewport: return Viewport(*self._raw.viewport())
    def upload_rgba(self, width: int, height: int, rgba: bytes) -> RetainedImage:
        return RetainedImage(self._raw.upload_rgba(width, height, rgba))
    def draw_image(self, canvas: Canvas, image: RetainedImage, bounds: Rect, opacity: float = 1) -> None:
        self._raw.draw_image(canvas._raw, image._raw, bounds._values(), opacity)
    def close(self) -> None:
        if not self.closed:
            self._raw.action("close")
            self.closed = True
    def __enter__(self) -> OverlayWindow: return self
    def __exit__(self, *args: Any) -> None: self.close()
