"""The shared vocabulary of the vivid_ui example ports: tokens, hand-computed geometry,
the interaction state machine, and the repaint loop.

The Rust originals (``vivid_ui/examples``) sit on a toolkit: taffy layout, an element tree,
retained images, an animation clock. Python has none of that here, and adding a framework is
not this directory's job — so every port computes its own geometry and drives the raw overlay
API through this module. What it keeps from the toolkit is the *contract*, not the machinery:

- The host does the hit testing. A frame paints hit regions; events come back naming one.
- A press is active exactly while it is held over the element it started on; a release
  anywhere ends it; a click is a release over the element the press started on.
- Nothing repaints until something asks; anything that moves is a function of the window's
  elapsed time, and a wait is a deadline the loop sleeps to — never a timer.

Colors are straight-alpha sRGB ``0xRRGGBBAA`` ints, as the binding takes them.
"""
from __future__ import annotations

import argparse
import math
import sys
import time
from contextlib import suppress
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Protocol, Sequence, Tuple, cast

from vivid_sdk import OverlaySession, OverlayWindowOptions
from vivid_sdk.overlay import (
    AccessibilityEvent,
    Brush,
    Canvas,
    CursorShape,
    HitRole,
    CancelEvent,
    ConnectionLostEvent,
    DismissedEvent,
    EnvironmentEvent,
    FocusEvent,
    GeometryEvent,
    GradientStop,
    HoverEvent,
    ImeEvent,
    KeyEvent,
    OverlayEvent,
    Path,
    Point,
    PointerEvent,
    Rect,
    RetainedImage,
    RetainedTextLayout,
    Semantics,
    SemanticNode,
    Shadow,
    StyledText,
    SubmissionOutcomeEvent,
    TextEvent,
    TextMeasurement,
    ViewportEvent,
    WheelEvent,
)

# --------------------------------------------------------------------------- tokens

PANEL_BG = 0x101018FF
INSET_BG = 0x1A1A24FF
CAPTION = 0x8080A0FF
TEXT = 0xE6E6F0FF
WHITE = 0xFFFFFFFF
BUTTON = 0x4A5FD0FF
BUTTON_HOVER = 0x5A6FE0FF
BUTTON_ACTIVE = 0x3A4FC0FF
FOCUS_RING = 0x8ECBFFFF

#: Resize edge bits, as the host's gesture handler reads them: left 1, top 2, right 4,
#: bottom 8. (The protocol is the authority: vivid_protocol/src/overlay.rs `fn gesture`.)
EDGE_LEFT = 1
EDGE_TOP = 2
EDGE_RIGHT = 4
EDGE_BOTTOM = 8

#: How long the loop sleeps when nothing is moving and nothing is pending.
IDLE_TICK = 0.2


class Keys:
    """USB HID key usages, which is what a key event carries."""

    ENTER = 0x28
    ESCAPE = 0x29
    BACKSPACE = 0x2A
    TAB = 0x2B
    SPACE = 0x2C
    LEFT = 0x50
    RIGHT = 0x4F
    UP = 0x52
    DOWN = 0x51
    HOME = 0x4A
    END = 0x4D
    DELETE = 0x4C

    @staticmethod
    def letter(ch: str) -> int:
        """The usage for a letter, so `Keys.letter("c")` is what cmd-C presses."""
        return 0x04 + (ord(ch.upper()) - ord("A"))


class Mods:
    SHIFT = 1
    CONTROL = 2
    ALT = 4
    SUPER = 8


def platform() -> int:
    """The platform's command modifier: super on macOS, control elsewhere."""
    return Mods.SUPER if sys.platform == "darwin" else Mods.CONTROL


# ------------------------------------------------------------------------- geometry


def inset(rect: Rect, by: float) -> Rect:
    """Shrink a rect on all four sides, never past nothing."""
    return Rect(
        rect.x + by,
        rect.y + by,
        max(rect.width - 2 * by, 0.0),
        max(rect.height - 2 * by, 0.0),
    )


def fit_rect(width: float, height: float, box: Rect) -> Rect:
    """The largest centered rect of this aspect that fits in `box` ("contain")."""
    if width <= 0 or height <= 0:
        return Rect(box.x, box.y, 0.0, 0.0)
    scale = min(box.width / width, box.height / height)
    placed = (width * scale, height * scale)
    return Rect(
        box.x + (box.width - placed[0]) / 2,
        box.y + (box.height - placed[1]) / 2,
        placed[0],
        placed[1],
    )


def gradient_brush(rect: Rect, angle_deg: float, stops: Sequence[GradientStop]) -> Brush:
    """A linear gradient across `rect` at `angle` degrees — the CSS convention, so 0 is
    upward and 90 is rightward. The endpoint span is the box's extent along the gradient's
    own direction, exactly as the Rust toolkit computes it (`Style::gradient_line`)."""
    radians = math.radians(angle_deg)
    direction = (math.sin(radians), -math.cos(radians))
    extent = abs(direction[0]) * rect.width / 2 + abs(direction[1]) * rect.height / 2
    centre = (rect.x + rect.width / 2, rect.y + rect.height / 2)
    return Brush.linear(
        Point(centre[0] - direction[0] * extent, centre[1] - direction[1] * extent),
        Point(centre[0] + direction[0] * extent, centre[1] + direction[1] * extent),
        list(stops),
    )


#: The shadow token scale, as the Rust toolkit names it: offset, blur, spread.
_SHADOW_TOKENS = {
    "xs": ((0.0, 1.0), 2.0, 0.0),
    "sm": ((0.0, 1.0), 3.0, 0.0),
    "base": ((0.0, 4.0), 6.0, -1.0),
    "md": ((0.0, 6.0), 12.0, -2.0),
    "lg": ((0.0, 10.0), 15.0, -3.0),
    "xl": ((0.0, 20.0), 25.0, -5.0),
}


def shadow_spec(name: str) -> Tuple[Tuple[float, float], float, float]:
    """A shadow token's `(offset, blur, spread)`, as the Rust toolkit names them."""
    return _SHADOW_TOKENS[name]


# ------------------------------------------------------------------------ animation


class Easing:
    """The five curves the Rust toolkit carries, ported exactly."""

    LINEAR = "linear"
    EASE_IN = "ease-in"
    EASE_OUT = "ease-out"
    EASE_IN_OUT = "ease-in-out"
    BACK_OUT = "back-out"

    @staticmethod
    def apply(kind: str, t: float) -> float:
        """The eased value at progress `t` in 0..=1. The ends are exact for every curve."""
        t = min(max(t, 0.0), 1.0)
        if kind == Easing.LINEAR:
            return t
        if kind == Easing.EASE_IN:
            return t * t * t
        if kind == Easing.EASE_OUT:
            return 1.0 - (1.0 - t) ** 3
        if kind == Easing.EASE_IN_OUT:
            return 4.0 * t * t * t if t < 0.5 else 1.0 - ((-2.0 * t + 2.0) ** 3) / 2.0
        if kind == Easing.BACK_OUT:
            c = 1.70158
            return 1.0 + (c + 1.0) * ((t - 1.0) ** 3) + c * ((t - 1.0) ** 2)
        raise ValueError(f"unknown easing: {kind}")


@dataclass
class Animation:
    duration: float
    easing: str = Easing.LINEAR
    repeat: str = "once"  # once | loop | ping-pong


@dataclass
class Running:
    """One animation, started at a moment on the window's clock."""

    animation: Animation
    started: float

    def progress(self, now: float) -> float:
        """Raw progress through the whole run, 0..=1 for `once`, 0..=repeat-span otherwise."""
        duration = max(self.animation.duration, 1e-9)
        elapsed = max(now - self.started, 0.0)
        if self.animation.repeat == "once":
            return min(elapsed / duration, 1.0)
        if self.animation.repeat == "loop":
            return (elapsed / duration) % 1.0
        # ping-pong: there and back, so the second half runs the curve reversed.
        return 1.0 - abs(1.0 - (elapsed / duration) % 2.0)

    def value(self, now: float) -> float:
        return Easing.apply(self.animation.easing, self.progress(now))

    def finished(self, now: float) -> bool:
        if self.animation.repeat != "once":
            return False
        return now - self.started >= self.animation.duration

    def restart(self, now: float) -> None:
        self.started = now


@dataclass
class Spring:
    stiffness: float
    damping: float
    mass: float

    @staticmethod
    def bouncy() -> "Spring":
        return Spring(stiffness=120.0, damping=10.0, mass=1.0)

    @staticmethod
    def stiff() -> "Spring":
        return Spring(stiffness=300.0, damping=40.0, mass=1.0)


#: The physics substep, and the largest step taken at once — as the Rust toolkit paces them.
_SPRING_STEP = 0.008
_SPRING_MAX_STEP = 0.25


@dataclass
class SpringState:
    """Where a spring is and where it is going. A retarget keeps the velocity."""

    value: float = 0.0
    velocity: float = 0.0
    target: float = 0.0

    def retarget(self, target: float) -> None:
        self.target = target

    def advance(self, spring: Spring, dt: float) -> None:
        remaining = min(max(dt, 0.0), _SPRING_MAX_STEP)
        while remaining > 0:
            step = min(remaining, _SPRING_STEP)
            force = -spring.stiffness * (self.value - self.target)
            drag = -spring.damping * self.velocity
            self.velocity += (force + drag) / spring.mass * step
            self.value += self.velocity * step
            remaining -= step

    def at_rest(self) -> bool:
        return abs(self.value - self.target) < 1e-3 and abs(self.velocity) < 1e-3


# ---------------------------------------------------------------------------- lists


@dataclass
class ListState:
    """Where a list is scrolled to, and what it is scrolling through.

    Ported exactly from the Rust toolkit's own arithmetic (`vivid_ui/src/list.rs`), including
    the rule that the visible range holds one row more than the box shows — the one sliding
    in — and that there is no overscroll past the last row.
    """

    count: int
    row_height: float
    offset: float = 0.0

    def content_height(self) -> float:
        return self.count * self.row_height

    def maximum_offset(self, viewport_height: float) -> float:
        return max(self.content_height() - viewport_height, 0.0)

    def set_count(self, count: int, viewport_height: float) -> None:
        self.count = count
        self.scroll_to(self.offset, viewport_height)

    def scroll_to(self, offset: float, viewport_height: float) -> None:
        self.offset = min(max(offset, 0.0), self.maximum_offset(viewport_height))

    def scroll_by(self, dy: float, viewport_height: float) -> None:
        self.scroll_to(self.offset + dy, viewport_height)

    def visible_range(self, viewport_height: float) -> Tuple[int, int]:
        first = int(self.offset // self.row_height)
        shown = int(math.ceil(viewport_height / self.row_height)) + 1
        return (max(first, 0), min(first + shown, self.count))

    def thumb(self, viewport_height: float) -> Optional[Tuple[float, float]]:
        """The scrollbar thumb's `(top_offset_within_content, length)`, or None when the
        content does not overflow. Length is never shorter than 16 logical pixels."""
        content = self.content_height()
        if content <= viewport_height:
            return None
        length = max(viewport_height * viewport_height / content, 16.0)
        travel = viewport_height - length
        return (travel * self.offset / self.maximum_offset(viewport_height), length)

    def row_top(self, row: int) -> float:
        """Where a row sits against the top of the viewport."""
        return row * self.row_height - self.offset


# ---------------------------------------------------------------------- interactions


@dataclass
class ElementState:
    """What an element is right now. Hover, press, and focus are remembered against the
    element's identity between frames, exactly as the toolkit remembers them by region."""

    hovered: bool = False
    pressed: bool = False
    focused: bool = False
    focus_visible: bool = False


#: Handlers are written against the event they expect, so the parameter is the event
#: type the example named, not the union.
Handler = Callable[[Any], None]


@dataclass
class _Region:
    """One painted hit region: its identity, its box, and what it listens for."""

    id: int
    name: str
    bounds: Rect
    handlers: Dict[str, Handler] = field(default_factory=dict)


@dataclass
class Frame:
    """What one paint of a view draws through.

    Regions are registered as they are painted; `bounds_of` reads the *previous* frame's
    table, which is the frame the host hit-tested against and the only one whose geometry
    an event could be referring to.
    """

    ui: "Ui"
    canvas: Canvas
    elapsed: float
    dt: float
    width: float
    height: float
    regions: Dict[int, _Region] = field(default_factory=dict)
    focus_order: List[str] = field(default_factory=list)
    semantics: List[SemanticNode] = field(default_factory=list)
    caret: Optional[Rect] = None

    # -- identity --------------------------------------------------------------

    def region(
        self,
        name: str,
        bounds: Rect,
        *,
        radius: float = 0.0,
        cursor: CursorShape = "",
        role: HitRole = "input",
        edges: int = 0,
        focusable: bool = False,
        on_click: Optional[Handler] = None,
        on_mouse_down: Optional[Handler] = None,
        on_mouse_up: Optional[Handler] = None,
        on_mouse_move: Optional[Handler] = None,
        on_wheel: Optional[Handler] = None,
        on_key: Optional[Handler] = None,
        on_text: Optional[Handler] = None,
        on_ime: Optional[Handler] = None,
        on_pressure: Optional[Handler] = None,
        on_accessibility: Optional[Handler] = None,
    ) -> ElementState:
        """Paint a hit region and register everything about it. Returns the element's live
        state so the caller can restyle around hover, press, and focus."""
        region = _Region(
            id=self.ui.region_id(name),
            name=name,
            bounds=bounds,
            handlers={
                slot: handler
                for slot, handler in (
                    ("click", on_click),
                    ("mouse_down", on_mouse_down),
                    ("mouse_up", on_mouse_up),
                    ("mouse_move", on_mouse_move),
                    ("wheel", on_wheel),
                    ("key", on_key),
                    ("text", on_text),
                    ("ime", on_ime),
                    ("pressure", on_pressure),
                    ("accessibility", on_accessibility),
                )
                if handler is not None
            },
        )
        self.regions[region.id] = region
        if focusable:
            self.focus_order.append(name)
        path = (
            Path.rounded_rectangle(bounds, radius)
            if radius > 0
            else Path.rectangle(bounds)
        )
        self.canvas.hit(region.id, path, role, edges=edges, cursor=cursor)
        state = self.ui.state_of(name)
        state.focused = self.ui.focused() == name
        state.focus_visible = state.focused and self.ui._focus_visible
        return state

    def drag_region(
        self, name: str, bounds: Rect, *, cursor: CursorShape = "grab"
    ) -> None:
        """Name the region the host should move the window by when dragged."""
        self.region(name, bounds, role="drag", cursor=cursor)

    def resize_region(
        self, name: str, bounds: Rect, edges: int, cursor: CursorShape
    ) -> None:
        """Name the region the host should resize the window from, on those edges."""
        self.region(name, bounds, role="resize", edges=edges, cursor=cursor)

    def bounds_of(self, name: str) -> Optional[Rect]:
        """Where an element was in the frame before this one."""
        return self.ui.bounds_of(name)

    def notify(self) -> None:
        self.ui.notify()

    def request_frame_after(self, seconds: float) -> None:
        """Ask to be painted again in `seconds` — how anything that moves is scheduled."""
        self.ui.request_frame_after(seconds)

    # -- drawing ---------------------------------------------------------------

    def box(
        self,
        rect: Rect,
        *,
        bg: Optional[int] = None,
        radius: float = 0.0,
        border: float = 0.0,
        border_color: int = 0x00000000,
    ) -> None:
        """A box: an optional fill, an optional outline that draws inside the edge."""
        path = (
            Path.rounded_rectangle(rect, radius)
            if radius > 0
            else Path.rectangle(rect)
        )
        if bg is not None:
            self.canvas.fill(path, Brush.solid(bg))
        if border > 0:
            self.canvas.stroke(path, Brush.solid(border_color), border)

    def caption(self, text: str, origin: Point) -> None:
        """The 12px grey label an example window opens with."""
        self.canvas.text(text, origin, 12, CAPTION)

    def label(
        self,
        text: str,
        rect: Rect,
        *,
        size: float = 13.0,
        color: int = TEXT,
        align: str = "start",
        vcenter: bool = True,
        weight: int = 400,
        italic: bool = False,
        family: str = "",
        max_width: Optional[float] = None,
    ) -> None:
        """One run of text, placed in a box: horizontally by `align` (measured, for center
        and end), vertically centred on the measured line unless told not to."""
        measured = self.measure(text, size, family=family, weight=weight, italic=italic)
        x = rect.x
        if align == "center":
            x += (rect.width - measured.width) / 2
        elif align == "end":
            x += rect.width - measured.width
        y = rect.y + (rect.height - measured.height) / 2 if vcenter else rect.y
        self.canvas.text(
            text,
            Point(x, y),
            size,
            color,
            family=family,
            weight=weight,
            italic=italic,
            max_width=max_width,
        )

    def button(
        self,
        name: str,
        rect: Rect,
        label: str,
        *,
        on_click: Optional[Handler] = None,
        base: int = BUTTON,
        hover: int = BUTTON_HOVER,
        active: int = BUTTON_ACTIVE,
        text_color: int = WHITE,
        radius: float = 6.0,
        size: float = 13.0,
        focusable: bool = False,
    ) -> ElementState:
        """A button: hover and press restyle it, the host shows the pointer cursor, and a
        focus ring appears only when the keyboard put the focus there."""
        state = self.region(
            name,
            rect,
            radius=radius,
            cursor="pointer",
            focusable=focusable,
            on_click=on_click,
        )
        fill = base
        if state.pressed:
            fill = active
        elif state.hovered:
            fill = hover
        self.box(rect, bg=fill, radius=radius)
        if state.focused and state.focus_visible:
            self.focus_ring(rect, radius=radius)
        self.label(label, rect, size=size, color=text_color, align="center")
        return state

    def focus_ring(self, rect: Rect, *, color: int = FOCUS_RING, radius: float = 0.0, width: float = 2.0) -> None:
        insetted = inset(rect, width / 2)
        path = (
            Path.rounded_rectangle(insetted, max(radius - width / 2, 0.0))
            if radius > 0
            else Path.rectangle(insetted)
        )
        self.canvas.stroke(path, Brush.solid(color), width)

    def shadow_box(self, rect: Rect, token: str = "md", *, radius: float = 0.0) -> None:
        """Cast one of the token shadows behind a box."""
        (offset_x, offset_y), blur, spread = _SHADOW_TOKENS[token]
        self.canvas.shadow(
            Shadow(
                rect=rect,
                radii=(radius, radius, radius, radius),
                color=0x0000000A,
                offset=Point(offset_x, offset_y),
                blur=blur,
                spread=spread,
                inset=False,
            )
        )

    def scrollbar(self, rect: Rect, list_state: ListState, viewport_height: float) -> None:
        """An 8px scrollbar for a list: nothing at all when the content fits."""
        thumb = list_state.thumb(viewport_height)
        if thumb is None:
            return
        top, length = thumb
        self.box(rect, bg=0x16161EFF, radius=4.0)
        self.box(
            Rect(rect.x, rect.y + top, rect.width, length),
            bg=BUTTON,
            radius=4.0,
        )

    def clip(self, rect: Rect, *, radius: float = 0.0) -> None:
        path = (
            Path.rounded_rectangle(rect, radius)
            if radius > 0
            else Path.rectangle(rect)
        )
        self.canvas.clip(path)

    # -- text and media --------------------------------------------------------

    def measure(
        self,
        text: str,
        size: float,
        *,
        family: str = "",
        weight: int = 400,
        italic: bool = False,
        max_width: Optional[float] = None,
    ) -> TextMeasurement:
        """What the host says about a run. Cached: a run that has not changed is not
        measured again, which is the toolkit's rule and the round trip's whole cost."""
        return self.ui.measure(
            text, size, family=family, weight=weight, italic=italic, max_width=max_width
        )

    def baseline_of(self, text: str, size: float) -> float:
        """Where the baseline of a run sits below its top, for baseline alignment."""
        measured = self.measure(text, size)
        for cluster in measured.clusters:
            return cluster.baseline
        return measured.height * 0.8

    def paragraph(self, key: str, styled: StyledText, origin: Point) -> None:
        """Draw a shaped paragraph: several runs, decorations, wrapping, and the typography
        the plain text command cannot carry.

        Shaping is the host's, through a retained layout cached under the caller's own key —
        the layout is host memory, so one per paragraph is the budget. A host that cannot
        shape at all gets the runs drawn plainly instead, which loses the styling that
        needed shaping and keeps the words where they were. That is the toolkit's own rule
        for a frame past the retention ceiling, and it is what makes these examples draw
        against a session with no text service.
        """
        if key not in self.ui._layouts:
            try:
                self.ui._layouts[key] = self.ui._window.layout_text(styled)
            except (ValueError, OSError):
                # Cache the refusal too: one attempt per paragraph, not one per frame.
                self.ui._layouts[key] = None
        layout = self.ui._layouts[key]
        if layout is not None:
            self.ui._window.draw_text_layout(self.canvas, layout, origin)
            return
        x = origin.x
        for run in styled.runs:
            style = run.style
            self.canvas.text(
                run.text,
                Point(x, origin.y),
                style.size,
                style.color,
                family=style.family,
                weight=style.weight,
                italic=style.italic,
            )
            x += self.measure(
                run.text,
                style.size,
                family=style.family,
                weight=style.weight,
                italic=style.italic,
            ).width

    def upload(self, name: str, width: int, height: int, rgba: bytes) -> RetainedImage:
        """Upload pixels once under a name; drawing is what keeps them."""
        image = self.ui._uploads.get(name)
        if image is None:
            image = self.ui._window.upload_rgba(width, height, rgba)
            self.ui._uploads[name] = image
        return image

    def image(self, image: RetainedImage, bounds: Rect, *, opacity: float = 1.0) -> None:
        self.ui._window.draw_image(self.canvas, image, bounds, opacity)

    def publish_semantics(self, nodes: Sequence[SemanticNode]) -> None:
        """What this frame says about itself, published once it is on screen."""
        self.semantics = list(nodes)

    def publish_caret(self, caret: Rect) -> None:
        """Where the focused field's caret is, so the host can place its candidate window."""
        self.caret = caret


class Ui:
    """A window, the host behind it, and the loop that connects them.

    The view is a function of state: `render(state, frame)` draws a whole frame through
    `Frame` and registers what is interactive. Everything else here is the contract —
    press, click, hover, focus, wheel, keys — reproduced from the Rust toolkit's routing.
    """

    def __init__(
        self,
        state: Any,
        render: Callable[[Any, Frame], None],
        *,
        width: float,
        height: float,
        title: str = "",
        on_key: Optional[Handler] = None,
        on_text: Optional[Handler] = None,
        on_ime: Optional[Handler] = None,
        on_wheel: Optional[Handler] = None,
        on_mouse_move: Optional[Handler] = None,
        on_mouse_down: Optional[Handler] = None,
        on_mouse_up: Optional[Handler] = None,
        on_dismissed: Optional[Handler] = None,
    ) -> None:
        self._state = state
        self._render = render
        self.width = width
        self.height = height
        self.title = title
        # Root handlers: whatever no region wanted comes here. They are plain attributes so
        # an example can wire one to the `Ui` it is still building — a key handler usually
        # needs to call `focus` on the very object that will run it.
        self.on_key = on_key
        self.on_text = on_text
        self.on_ime = on_ime
        self.on_wheel = on_wheel
        self.on_mouse_move = on_mouse_move
        # A press or a release that no region wanted still happened somewhere — a drag ends
        # wherever the pointer was let go, which is usually not the element it began on.
        self.on_mouse_down = on_mouse_down
        self.on_mouse_up = on_mouse_up
        self.on_dismissed = on_dismissed
        self._session: Optional[OverlaySession] = None
        self._window: Any = None
        self._ids: Dict[str, int] = {}
        self._next_id = 1
        self._states: Dict[str, ElementState] = {}
        self._prev: Dict[int, _Region] = {}
        self._focus_order: List[str] = []
        self._pressed: Optional[Tuple[int, int]] = None
        self._hovered: Optional[int] = None
        self._focused: Optional[str] = None
        self._focus_visible = False
        self._dirty = True
        self._wake: Optional[float] = None
        self._t0 = time.monotonic()
        self._last_paint = self._t0
        self._measures: Dict[Tuple[Any, ...], TextMeasurement] = {}
        # A None means the host refused to shape this one; it is drawn plainly.
        self._layouts: Dict[str, Optional[RetainedTextLayout]] = {}
        self._uploads: Dict[str, RetainedImage] = {}
        self._quit = False

    # -- identity and state ----------------------------------------------------

    def region_id(self, name: str) -> int:
        """The stable region number a name paints as, so the host's memory of a region
        survives the frame that is rebuilding it."""
        region = self._ids.get(name)
        if region is None:
            region = self._next_id
            self._next_id += 1
            self._ids[name] = region
        return region

    def state_of(self, name: str) -> ElementState:
        state = self._states.get(name)
        if state is None:
            state = ElementState()
            self._states[name] = state
        return state

    def bounds_of(self, name: str) -> Optional[Rect]:
        """Where an element was in the last painted frame."""
        for region in self._prev.values():
            if region.name == name:
                return region.bounds
        return None

    def focused(self) -> Optional[str]:
        return self._focused

    # -- the loop --------------------------------------------------------------

    def run(self, duration: Optional[float] = None) -> None:
        """Open the window against the environment's host and pump until dismissed."""
        with OverlaySession.from_env() as session:
            window = session.create_window(
                OverlayWindowOptions(Rect(60, 60, self.width, self.height), title=self.title)
            )
            with window:
                self.attach(session, window)
                window.center()
                window.request_focus()
                self._loop(duration)

    def attach(self, session: OverlaySession, window: Any) -> None:
        """Connect to a session the caller owns — which is how a test drives the loop."""
        self._session = session
        self._window = window
        self._t0 = time.monotonic()
        self._last_paint = self._t0
        self.paint()

    def quit(self) -> None:
        self._quit = True

    def notify(self) -> None:
        self._dirty = True

    def request_frame_after(self, seconds: float) -> None:
        deadline = time.monotonic() + seconds
        self._wake = deadline if self._wake is None else min(self._wake, deadline)

    def capture(self, on: bool = True) -> None:
        """Keep pointer events arriving after the pointer leaves the region a drag began
        on, which is what a drag is."""
        # Best-effort, as the Rust toolkit's `let _ = cx.capture_pointer(..)` is: a host
        # that will not capture still delivers what happens over its own regions.
        if self._session is not None:
            with suppress(Exception):
                self._session.capture_pointer(self._window, on)

    def focus(self, name: str, *, visible: bool = True) -> None:
        """Move focus to an element, saying whether the keyboard put it there."""
        self._focused = name
        self._focus_visible = visible
        # Focus is a property of the element the moment it moves, not one paint later —
        # hover and press work the same way.
        for state in self._states.values():
            state.focused = False
            state.focus_visible = False
        if name is not None:
            state = self.state_of(name)
            state.focused = True
            state.focus_visible = visible
        if self._window is not None:
            with suppress(Exception):
                self._window.request_focus()
        self.notify()

    def focus_next(self, *, backwards: bool = False, visible: bool = True) -> None:
        """Tab order is paint order; focus moves through the focusable elements."""
        order = [name for name in self._focus_order if name in self._ids]
        if not order:
            return
        current = self._focused if self._focused in order else None
        if current is None:
            index = len(order) - 1 if backwards else 0
        else:
            step = -1 if backwards else 1
            index = (order.index(current) + step) % len(order)
        self.focus(order[index], visible=visible)

    def copy(self, text: str) -> None:
        """Write to the clipboard. Only the host can, and only after a recent gesture."""
        if self._window is not None:
            with suppress(Exception):
                self._window.set_clipboard(text)

    def release_image(self, image: RetainedImage) -> None:
        """Give a picture back. The host's memory is bounded, and an example that caches
        its own uploads is the one that decides what is no longer on screen."""
        if self._window is not None:
            with suppress(Exception):
                self._window.release_image(image)
        for name, held in list(self._uploads.items()):
            if held is image:
                del self._uploads[name]

    def measure(
        self,
        text: str,
        size: float,
        *,
        family: str = "",
        weight: int = 400,
        italic: bool = False,
        max_width: Optional[float] = None,
    ) -> TextMeasurement:
        """What the host says about a run, cached — a run that has not changed is not
        measured again, which is the toolkit's rule and the round trip's whole cost.

        This is on the window rather than the frame because a field measures when a key
        arrives, which is not inside a paint.
        """
        key = (text, size, family, weight, italic, max_width)
        cached = self._measures.get(key)
        if cached is None:
            try:
                cached = self._window.measure_text(
                    text, size, family=family, weight=weight, italic=italic, max_width=max_width
                )
            except (ValueError, OSError, AttributeError):
                # A host with no text service — an offline session, a test — still draws.
                # The estimate keeps boxes laid out and clicks routable, the way the Rust
                # toolkit draws a run nobody measured at its own line height rather than
                # collapsed. A live pane measures for real; nothing else changes.
                cached = TextMeasurement(
                    width=size * 0.6 * len(text),
                    height=size * 1.2,
                    lines=(),
                    clusters=(),
                    truncated_at=None,
                )
            self._measures[key] = cached
        return cached

    def read(self, f: Callable[[Any], Any]) -> Any:
        """Look at the state without handing it out — the test seam."""
        return f(self._state)

    def elapsed(self) -> float:
        return time.monotonic() - self._t0

    def paint(self) -> None:
        """Build one frame and send it, publishing semantics and the caret once the frame
        they describe is the one on screen."""
        assert self._window is not None
        canvas = Canvas()
        now = time.monotonic()
        frame = Frame(
            self,
            canvas,
            elapsed=now - self._t0,
            dt=now - self._last_paint,
            width=self.width,
            height=self.height,
        )
        # A wake is asked for by the frame that wants it; painting re-arms nothing.
        self._wake = None
        self._render(self._state, frame)
        canvas.validate()
        if frame.semantics or frame.caret is not None:
            receipt = self._window.submit(canvas)
            if receipt.wait(1.0) == "presented":
                # Best-effort, like every host call the frame can survive without: an
                # offline session refuses semantics outright, and a host refusing a
                # description of a scene it is not showing is the rule that stops a screen
                # reader announcing a control that has gone — not an error worth a frame.
                with suppress(Exception):
                    if frame.semantics:
                        self._window.set_semantics(
                            Semantics(receipt.revision, tuple(frame.semantics))
                        )
                    if frame.caret is not None:
                        self._window.set_editor_geometry(receipt.revision, frame.caret)
        else:
            self._window.present(canvas)
        self._prev = frame.regions
        self._focus_order = list(frame.focus_order)
        self._last_paint = now
        self._dirty = False

    def _loop(self, duration: Optional[float]) -> None:
        assert self._session is not None
        deadline = None if duration is None else time.monotonic() + duration
        while not self._quit:
            now = time.monotonic()
            if deadline is not None and now >= deadline:
                break
            timeout = IDLE_TICK
            if self._wake is not None:
                timeout = max(0.0, min(timeout, self._wake - now))
            if deadline is not None:
                timeout = max(0.0, min(timeout, deadline - now))
            event = self._session.wait_event(timeout)
            if event is not None:
                self.dispatch(event)
                if isinstance(event, ConnectionLostEvent):
                    break
            now = time.monotonic()
            if self._dirty or (self._wake is not None and now >= self._wake):
                self.paint()

    # -- dispatch ----------------------------------------------------------------
    #
    # Transcribed from the Rust toolkit's routing (vivid_ui/src/window.rs): the host does
    # the hit testing, a click is a release on the element the press started on, leaving a
    # pressed element cancels the press, the wheel goes to whatever is hovered, and keys
    # go to the focused element before the window's own handlers.

    def dispatch(self, event: OverlayEvent) -> None:
        if self._window is not None and not event.targets(self._window):
            return
        if isinstance(event, ConnectionLostEvent):
            self._quit = True
        elif isinstance(event, PointerEvent):
            self._pointer(event)
        elif isinstance(event, HoverEvent):
            self._hover(event)
        elif isinstance(event, WheelEvent):
            self._fire(self._hovered, "wheel", event)
        elif isinstance(event, (KeyEvent, TextEvent, ImeEvent)):
            self._focus_routed(event)
        elif isinstance(event, FocusEvent):
            if not event.focused:
                self._focus_visible = False
                self._cancel_press()
                self.notify()
        elif isinstance(event, GeometryEvent):
            self.width = event.bounds.width
            self.height = event.bounds.height
            self.notify()
        elif isinstance(event, DismissedEvent):
            if self.on_dismissed is not None:
                self.on_dismissed(event)
            else:
                self._quit = True
        elif isinstance(event, CancelEvent):
            self.notify()
        elif isinstance(event, AccessibilityEvent):
            self._fire(event.application_id, "accessibility", event)
        elif isinstance(event, (EnvironmentEvent, ViewportEvent, SubmissionOutcomeEvent)):
            pass

    def _pointer(self, event: PointerEvent) -> None:
        region = event.application_id
        if event.button is None:
            # A move. Pressure is separate and optional: a host with no sensor never
            # says zero, it says nothing.
            self._fire(region, "mouse_move", event)
            if event.pressure is not None:
                self._fire(region, "pressure", event)
            return
        if event.down:
            # Best-effort, as the Rust toolkit's `let _ =` is: a host that cannot take the
            # request still gets the press.
            with suppress(Exception):
                self._window.request_focus()
            self._pressed = (region, event.clicks)
            self._set_pressed(region, True)
            self._fire(region, "mouse_down", event)
        else:
            self._fire(region, "mouse_up", event)
            pressed = self._pressed
            self._pressed = None
            if pressed is not None:
                start, clicks = pressed
                self._set_pressed(start, False)
                if start == region and clicks > 0:
                    self._fire(region, "click", event)

    def _hover(self, event: HoverEvent) -> None:
        region = event.application_id
        name = self._name_of(region)
        state = self._states.get(name) if name is not None else None
        changed = False
        if state is not None and state.hovered != event.entered:
            state.hovered = event.entered
            changed = True
        if event.entered:
            self._hovered = region
        elif self._hovered == region:
            self._hovered = None
        # Leaving a pressed element cancels the press: a release over nothing is not
        # reported at all, so the press must not wait for one.
        if not event.entered and self._pressed is not None and self._pressed[0] == region:
            self._cancel_press()
        if changed:
            self.notify()

    def _focus_routed(self, event: OverlayEvent) -> None:
        slot = (
            "key"
            if isinstance(event, KeyEvent)
            else "text"
            if isinstance(event, TextEvent)
            else "ime"
        )
        focused_id = (
            self.region_id(self._focused) if self._focused is not None else None
        )
        region = self._prev.get(focused_id) if focused_id is not None else None
        if region is not None and slot in region.handlers:
            region.handlers[slot](event)
        else:
            handler = getattr(self, f"on_{slot}", None)
            if handler is not None:
                handler(event)

    def _cancel_press(self) -> None:
        if self._pressed is not None:
            self._set_pressed(self._pressed[0], False)
            self._pressed = None
            self.notify()

    def _set_pressed(self, region_id: int, pressed: bool) -> None:
        name = self._name_of(region_id)
        state = self._states.get(name) if name is not None else None
        if state is not None and state.pressed != pressed:
            state.pressed = pressed
            self.notify()

    def _name_of(self, region_id: int) -> Optional[str]:
        region = self._prev.get(region_id)
        return region.name if region is not None else None

    def _fire(self, region_id: Optional[int], slot: str, event: OverlayEvent) -> None:
        """Deliver to the region the host named, falling to the window's own handler."""
        region = self._prev.get(region_id) if region_id is not None else None
        handler = region.handlers.get(slot) if region is not None else None
        if handler is None:
            handler = getattr(self, f"on_{slot}", None)
        if handler is not None:
            handler(event)


class Measurer(Protocol):
    """Anything that can ask the host how big a run is: a `Frame`, or the `Ui` itself when
    a key arrives between paints."""

    def measure(
        self,
        text: str,
        size: float,
        *,
        family: str = ...,
        weight: int = ...,
        italic: bool = ...,
        max_width: Optional[float] = ...,
    ) -> TextMeasurement: ...


@dataclass
class TextField:
    """A single-line text field: text, a caret, a selection anchor, and the composition an
    input method is still deciding about.

    Offsets are Python character indexes end to end — the binding converts the wire's
    UTF-8 offsets on the way in, and the clusters it reports come back in the same units.
    TypeScript's twin is UTF-16 for the same reason: each language is self-consistent with
    its own binding, and nothing mixes the two.
    """

    text: str = ""
    caret: int = 0
    anchor: int = 0
    preedit: Optional[str] = None
    size: float = 14.0

    def __post_init__(self) -> None:
        self.caret = min(self.caret, len(self.text))
        self.anchor = min(self.anchor, len(self.text))

    @property
    def selected(self) -> bool:
        return self.caret != self.anchor

    def selected_text(self) -> str:
        lo, hi = sorted((self.caret, self.anchor))
        return self.text[lo:hi]

    def _delete_selection(self) -> None:
        lo, hi = sorted((self.caret, self.anchor))
        self.text = self.text[:lo] + self.text[hi:]
        self.caret = self.anchor = lo

    def insert(self, added: str) -> None:
        """Replace the selection with `added`, leaving the caret after it."""
        if self.selected:
            self._delete_selection()
        self.text = self.text[: self.caret] + added + self.text[self.caret :]
        self.caret = self.anchor = self.caret + len(added)

    def boundaries(self, f: Measurer) -> List[int]:
        """Every caret position the text offers: cluster edges, or every index when the
        host could not be asked (an offline session, a test)."""
        measured = f.measure(self.text, self.size)
        edges = {0, len(self.text)}
        for cluster in measured.clusters:
            edges.add(cluster.start)
            edges.add(cluster.end)
        if not measured.clusters:
            edges.update(range(len(self.text) + 1))
        return sorted(edges)

    def handle_key(self, event: KeyEvent, f: Measurer) -> bool:
        """Arrows by cluster, home and end, backspace and delete. Shift moves the caret and
        keeps the anchor, which is what a selection is. Returns whether anything changed."""
        if not event.down or event.repeat:
            return False
        edges = self.boundaries(f)
        changed = False
        if event.physical == Keys.LEFT:
            target = max(e for e in edges if e < self.caret) if self.caret > 0 else self.caret
            if event.modifiers & Mods.SHIFT:
                self.caret = target
            else:
                self.caret = self.anchor = target
            changed = True
        elif event.physical == Keys.RIGHT:
            target = min(e for e in edges if e > self.caret) if self.caret < len(self.text) else self.caret
            if event.modifiers & Mods.SHIFT:
                self.caret = target
            else:
                self.caret = self.anchor = target
            changed = True
        elif event.physical == Keys.HOME:
            self.caret = 0
            if not event.modifiers & Mods.SHIFT:
                self.anchor = 0
            changed = True
        elif event.physical == Keys.END:
            self.caret = len(self.text)
            if not event.modifiers & Mods.SHIFT:
                self.anchor = len(self.text)
            changed = True
        elif event.physical == Keys.BACKSPACE:
            if self.selected:
                self._delete_selection()
            elif self.caret > 0:
                edge = max(e for e in edges if e < self.caret)
                self.text = self.text[:edge] + self.text[self.caret :]
                self.caret = self.anchor = edge
            changed = True
        elif event.physical == Keys.DELETE:
            if self.selected:
                self._delete_selection()
            elif self.caret < len(self.text):
                edge = min(e for e in edges if e > self.caret)
                self.text = self.text[: self.caret] + self.text[edge:]
                self.anchor = self.caret
            changed = True
        return changed

    def handle_text(self, event: TextEvent) -> bool:
        """Committed text — typed characters and paste alike, a terminal host being unable
        to read a clipboard, only write one."""
        if event.text:
            self.insert(event.text)
            return True
        return False

    def handle_ime(self, event: ImeEvent) -> bool:
        """The composition an input method is still deciding about. It is drawn inline and
        the caret sits after it; committing arrives as ordinary text."""
        composing = event.preedit or None
        changed = composing != self.preedit
        self.preedit = composing
        return changed

    def drawn(self) -> str:
        """What the field renders: the text with the composition where the caret is."""
        if not self.preedit:
            return self.text
        return self.text[: self.caret] + self.preedit + self.text[self.caret :]

    def caret_x(self, f: Measurer) -> float:
        return f.measure(self.text[: self.caret], self.size).width

    def caret_rect(self, f: Measurer, box: Rect) -> Rect:
        """Where the caret is drawn, and what the host places its candidate window from.
        A composition in flight is drawn before it, so the caret sits after the preedit."""
        measured = f.measure(self.drawn(), self.size)
        preedit_width = (
            f.measure(self.preedit, self.size).width if self.preedit else 0.0
        )
        return Rect(
            box.x + self.caret_x(f) + preedit_width,
            box.y + (box.height - measured.height) / 2,
            1.5,
            measured.height,
        )

    def selection_rect(self, f: Measurer, box: Rect) -> Optional[Rect]:
        if not self.selected:
            return None
        measured = f.measure(self.text, self.size)
        lo_x = f.measure(self.text[: min(self.caret, self.anchor)], self.size).width
        hi_x = f.measure(self.text[: max(self.caret, self.anchor)], self.size).width
        return Rect(
            box.x + lo_x,
            box.y + (box.height - measured.height) / 2,
            max(hi_x - lo_x, 1.0),
            measured.height,
        )


def duration(argv: Optional[Sequence[str]] = None) -> Optional[float]:
    """The `--duration` every example takes, bounded as the pane helpers bound it."""
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--duration", type=float, default=None)
    known, _ = parser.parse_known_args(
        list(sys.argv[1:] if argv is None else argv)
    )
    seconds = cast(Optional[float], known.duration)
    if seconds is None:
        return None
    if not 0.0 <= seconds <= 3600.0:
        raise SystemExit("--duration takes seconds in 0..3600")
    return seconds
