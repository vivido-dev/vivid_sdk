"""The shared helper behind the `vivid_ui` example ports, and the contracts it reproduces.

None of this needs a pane. The arithmetic — list ranges, scrollbar thumbs, easings, springs,
gradient endpoints, fitted boxes — is checked directly, and the interaction contract is
driven with synthetic events against a dry-run session, which is the same code path a live
host drives. What cannot be checked here is what the host does with a frame: that is the
live acceptance the examples' own docstrings point at.
"""
from __future__ import annotations

import math
import sys
from pathlib import Path as FilePath
from typing import Any, List, Optional, Tuple

import pytest

sys.path.insert(0, str(FilePath(__file__).resolve().parent.parent / "examples" / "python" / "ui"))

from _ui import (  # noqa: E402 - the path above is what makes this importable
    Animation,
    Easing,
    ListState,
    Running,
    Spring,
    SpringState,
    TextField,
    Ui,
    fit_rect,
    gradient_brush,
    inset,
)
from vivid_sdk import OverlaySession, OverlayWindowOptions  # noqa: E402
from vivid_sdk.overlay import (  # noqa: E402
    Brush,
    GradientStop,
    HoverEvent,
    KeyEvent,
    Point,
    PointerEvent,
    Rect,
    TextEvent,
    WheelEvent,
)


class _Raw:
    """Stands in for the native event handle. Ours always targets the window under test."""

    def targets(self, _window: Any) -> bool:
        return True


def pointer(
    region: int,
    x: float = 0.0,
    y: float = 0.0,
    button: Optional[int] = None,
    down: Optional[bool] = None,
    clicks: int = 0,
    pressure: Optional[float] = None,
) -> PointerEvent:
    return PointerEvent(0, _Raw(), Point(x, y), region, 0, button, down, clicks, pressure)


def hover(region: int, entered: bool) -> HoverEvent:
    return HoverEvent(0, _Raw(), region, entered)


def wheel(dy: float) -> WheelEvent:
    return WheelEvent(0, _Raw(), Point(0, 0), 0.0, dy, 0, True, "changed")


def key(usage: int, modifiers: int = 0, down: bool = True, repeat: bool = False) -> KeyEvent:
    return KeyEvent(0, _Raw(), usage, down, repeat, modifiers)


# --------------------------------------------------------------------------- arithmetic


def test_a_list_shows_the_rows_its_box_can_hold_and_one_more() -> None:
    state = ListState(1000, 20.0)
    # A 100 pixel box holds five whole rows; the sixth is the one sliding in.
    assert state.visible_range(100.0) == (0, 6)

    state.scroll_to(200.0, 100.0)
    assert state.visible_range(100.0) == (10, 16)

    # Part way through a row: the row half off the top is still the first one.
    state.scroll_to(205.0, 100.0)
    assert state.visible_range(100.0) == (10, 16)


def test_a_list_does_not_scroll_past_its_own_end() -> None:
    state = ListState(10, 20.0)
    assert state.maximum_offset(100.0) == 100.0
    state.scroll_by(1000.0, 100.0)
    assert state.offset == 100.0
    state.scroll_by(-1000.0, 100.0)
    assert state.offset == 0.0

    # Content shorter than the box does not scroll at all.
    short = ListState(2, 20.0)
    short.scroll_by(50.0, 100.0)
    assert short.offset == 0.0
    assert short.visible_range(100.0) == (0, 2)


def test_a_list_that_shrinks_keeps_an_offset_it_can_still_honour() -> None:
    state = ListState(100, 20.0)
    state.scroll_to(state.maximum_offset(100.0), 100.0)
    state.set_count(10, 100.0)
    assert state.offset == state.maximum_offset(100.0) == 100.0


def test_a_scrollbar_thumb_is_the_fraction_on_screen_and_never_a_sliver() -> None:
    state = ListState(100, 20.0)
    thumb = state.thumb(100.0)
    assert thumb is not None
    top, length = thumb
    assert top == 0.0
    # A twentieth of the content is on screen, but a thumb is never shorter than 16.
    assert length == pytest.approx(16.0)

    state.scroll_to(state.maximum_offset(100.0), 100.0)
    bottom = state.thumb(100.0)
    assert bottom is not None
    assert bottom[0] == pytest.approx(100.0 - bottom[1])

    # Content that fits has no thumb at all, rather than a full-length one.
    assert ListState(2, 20.0).thumb(100.0) is None


def test_every_easing_starts_at_nothing_and_ends_at_everything() -> None:
    for kind in (
        Easing.LINEAR,
        Easing.EASE_IN,
        Easing.EASE_OUT,
        Easing.EASE_IN_OUT,
        Easing.BACK_OUT,
    ):
        assert Easing.apply(kind, 0.0) == pytest.approx(0.0)
        assert Easing.apply(kind, 1.0) == pytest.approx(1.0)
        # Past the ends is clamped, not extrapolated.
        assert Easing.apply(kind, -1.0) == pytest.approx(0.0)
        assert Easing.apply(kind, 2.0) == pytest.approx(1.0)

    assert Easing.apply(Easing.LINEAR, 0.5) == pytest.approx(0.5)
    assert Easing.apply(Easing.EASE_IN_OUT, 0.5) == pytest.approx(0.5)
    assert Easing.apply(Easing.EASE_OUT, 0.5) == pytest.approx(0.875)
    assert Easing.apply(Easing.EASE_IN, 0.5) == pytest.approx(0.125)
    # Back-out overshoots before it settles, which is the whole point of it.
    assert Easing.apply(Easing.BACK_OUT, 0.7) > 1.0


def test_a_ping_pong_animation_goes_there_and_comes_back() -> None:
    running = Running(Animation(2.0, repeat="ping-pong"), 0.0)
    assert running.progress(0.0) == pytest.approx(0.0)
    assert running.progress(2.0) == pytest.approx(1.0)
    assert running.progress(4.0) == pytest.approx(0.0)
    # It never finishes, which is what keeps it asking for frames.
    assert not running.finished(100.0)

    once = Running(Animation(1.0), 0.0)
    assert once.progress(2.0) == pytest.approx(1.0)
    assert once.finished(1.0)


def test_a_spring_settles_and_keeps_its_speed_when_it_is_sent_somewhere_else() -> None:
    spring = Spring.bouncy()
    state = SpringState()
    state.retarget(1.0)
    for _ in range(250):
        state.advance(spring, 1 / 60)
    assert state.at_rest()
    assert state.value == pytest.approx(1.0, abs=0.01)

    # Retargeted in flight, it carries its velocity into the new target rather than
    # restarting — which is the whole reason this is a spring and not a duration.
    moving = SpringState()
    moving.retarget(1.0)
    for _ in range(6):
        moving.advance(spring, 1 / 60)
    speed = moving.velocity
    assert speed > 0
    moving.retarget(0.5)
    assert moving.velocity == speed


def test_a_gradient_spans_its_box_along_its_own_direction() -> None:
    box = Rect(0, 0, 100, 80)
    stops = [GradientStop(0.0, 0x000000FF), GradientStop(1.0, 0xFFFFFFFF)]

    def endpoints(angle: float) -> Tuple[Tuple[float, float], Tuple[float, float]]:
        brush = gradient_brush(box, angle, stops)
        assert isinstance(brush, Brush)
        values = brush._geometry  # noqa: SLF001 - the geometry is what is under test
        return (values[0], values[1]), (values[2], values[3])

    # 0 is upward, so the gradient runs from the bottom edge to the top one.
    start, end = endpoints(0.0)
    assert start == pytest.approx((50.0, 80.0))
    assert end == pytest.approx((50.0, 0.0))

    # 90 is rightward.
    start, end = endpoints(90.0)
    assert start == pytest.approx((0.0, 40.0))
    assert end == pytest.approx((100.0, 40.0))

    # 180 is downward: the reverse of 0.
    start, end = endpoints(180.0)
    assert start == pytest.approx((50.0, 0.0))
    assert end == pytest.approx((50.0, 80.0))

    # A diagonal spans the box's extent along the diagonal, not its corners.
    start, end = endpoints(45.0)
    extent = abs(math.sin(math.radians(45.0))) * 50 + abs(math.cos(math.radians(45.0))) * 40
    assert start == pytest.approx((50.0 - extent * math.sin(math.radians(45.0)),
                                  40.0 + extent * math.cos(math.radians(45.0))))


def test_a_fitted_picture_keeps_its_proportions_and_its_centre() -> None:
    # Wider than its box: the width binds and the result is centred vertically.
    assert fit_rect(96, 48, Rect(0, 0, 72, 72)) == Rect(0.0, 18.0, 72.0, 36.0)
    # Taller than its box: the height binds.
    assert fit_rect(48, 96, Rect(0, 0, 72, 72)) == Rect(18.0, 0.0, 36.0, 72.0)
    # A picture with no pixels fits nothing rather than dividing by zero.
    assert fit_rect(0, 10, Rect(0, 0, 72, 72)).width == 0.0


def test_an_inset_never_shrinks_past_nothing() -> None:
    assert inset(Rect(0, 0, 100, 100), 10) == Rect(10.0, 10.0, 80.0, 80.0)
    assert inset(Rect(0, 0, 10, 10), 40).width == 0.0


# ------------------------------------------------------------------- the text field


def _measuring_ui() -> Ui:
    """A `Ui` with no window: `TextField` measures through it, and with no host behind it
    the estimate stands in — which is exactly what a field on a text-less host gets."""
    return Ui(object(), lambda _state, _frame: None, width=100, height=100)


def test_a_field_types_selects_and_deletes_by_cluster() -> None:
    ui = _measuring_ui()
    field = TextField("hello")
    field.caret = field.anchor = 5

    field.insert(" world")
    assert field.text == "hello world"
    assert field.caret == field.anchor == 11

    # Shift-left extends the selection; the anchor stays where it was.
    from _ui import Keys, Mods

    field.handle_key(key(Keys.LEFT, Mods.SHIFT), ui)
    field.handle_key(key(Keys.LEFT, Mods.SHIFT), ui)
    assert field.selected_text() == "ld"
    assert field.anchor == 11

    # Backspace takes the selection rather than one character.
    field.handle_key(key(Keys.BACKSPACE), ui)
    assert field.text == "hello wor"
    assert not field.selected

    # Home and end collapse the selection to an edge.
    field.handle_key(key(Keys.HOME), ui)
    assert field.caret == field.anchor == 0
    field.handle_key(key(Keys.END), ui)
    assert field.caret == len("hello wor")


def test_a_composition_draws_inline_and_commits_as_ordinary_text() -> None:
    from vivid_sdk.overlay import ImeEvent

    field = TextField("ab")
    field.caret = field.anchor = 1
    field.handle_ime(ImeEvent(0, _Raw(), "み", None))
    # The composition is where the caret is, and the text underneath has not changed yet.
    assert field.drawn() == "aみb"
    assert field.text == "ab"

    field.handle_ime(ImeEvent(0, _Raw(), "", None))
    field.handle_text(TextEvent(0, _Raw(), "見"))
    assert field.text == "a見b"


# --------------------------------------------------------- the interaction contract


def _started(module_name: str) -> Tuple[Ui, Any, OverlaySession]:
    """One example, attached to a dry-run session and painted once."""
    import importlib

    module = importlib.import_module(module_name)
    session = OverlaySession.connect(dry_run=True)
    ui = module.make_ui()
    window = session.create_window(OverlayWindowOptions(Rect(60, 60, ui.width, ui.height)))
    ui.attach(session, window)
    return ui, module, session


def test_a_click_is_a_release_over_the_element_the_press_began_on() -> None:
    ui, _module, session = _started("hello_world")
    with session:
        region = ui.region_id("increment")
        ui.dispatch(pointer(region, button=0, down=True, clicks=1))
        ui.dispatch(pointer(region, button=0, down=False))
        assert ui.read(lambda state: state.count) == 1

        # A release over nothing is not a click, however it started.
        ui.dispatch(pointer(region, button=0, down=True, clicks=1))
        ui.dispatch(pointer(0, button=0, down=False))
        assert ui.read(lambda state: state.count) == 1

        # Nor is a press with no click count behind it — a drag's first move, say.
        ui.dispatch(pointer(region, button=0, down=True, clicks=0))
        ui.dispatch(pointer(region, button=0, down=False))
        assert ui.read(lambda state: state.count) == 1


def test_a_release_over_a_different_element_clicks_neither_of_them() -> None:
    # Two elements that both answer clicks. Pressing one and releasing over the other must
    # click neither: the press's own element never got the release, and the other one never
    # had a press. A frame that fired the release's element would act on a button the
    # pointer merely passed over.
    ui, _module, session = _started("focus_visible")
    with session:
        ui.dispatch(pointer(ui.region_id("button-1"), button=0, down=True, clicks=1))
        ui.dispatch(pointer(ui.region_id("button-2"), button=0, down=False))
        assert ui.read(lambda state: state.focused) == 0
        assert ui.focused() is None


def test_leaving_a_pressed_element_cancels_the_press() -> None:
    # The whole lesson of `active_state_bug`: a release over nothing is never reported, so
    # a press that waited for one would stay stuck for good.
    ui, _module, session = _started("active_state_bug")
    with session:
        region = ui.region_id("press")
        ui.dispatch(pointer(region, button=0, down=True, clicks=1))
        assert ui.state_of("press").pressed

        ui.dispatch(hover(region, False))
        assert not ui.state_of("press").pressed

        ui.dispatch(pointer(0, button=0, down=False))
        assert ui.read(lambda state: state.presses) == 0


def test_a_ring_is_shown_by_the_keyboard_and_not_by_a_click() -> None:
    ui, _module, session = _started("focus_visible")
    with session:
        from _ui import Keys

        ui.dispatch(key(Keys.TAB))
        assert ui.focused() == "button-1"
        assert ui.state_of("button-1").focus_visible

        region = ui.region_id("button-1")
        ui.dispatch(pointer(region, button=0, down=True, clicks=1))
        ui.dispatch(pointer(region, button=0, down=False))
        assert ui.focused() == "button-1"
        assert not ui.state_of("button-1").focus_visible


def test_the_wheel_goes_to_whatever_is_hovered() -> None:
    ui, _module, session = _started("uniform_list")
    with session:
        # Nothing is hovered, so the wheel reaches no region and the list stays put.
        ui.dispatch(wheel(120.0))
        assert ui.read(lambda state: state.list.offset) == 0.0

        ui.dispatch(hover(ui.region_id("list"), True))
        ui.dispatch(wheel(120.0))
        assert ui.read(lambda state: state.list.offset) == 120.0


def test_a_drag_drops_where_it_was_let_go() -> None:
    ui, _module, session = _started("drag_drop")
    with session:
        tray = ui.bounds_of("tray")
        assert tray is not None

        ui.dispatch(pointer(ui.region_id("tile-0"), 40, 66, button=0, down=True, clicks=1))
        assert ui.read(lambda state: state.dragging) is not None
        ui.dispatch(pointer(0, 200, tray.y + 20))
        ui.dispatch(pointer(0, 200, tray.y + 20, button=0, down=False))
        assert ui.read(lambda state: state.dropped) == [0]

        # Released off the tray, a tile goes back rather than being dropped.
        ui.paint()
        ui.dispatch(pointer(ui.region_id("tile-1"), 110, 66, button=0, down=True, clicks=1))
        ui.dispatch(pointer(0, 300, 8))
        ui.dispatch(pointer(0, 300, 8, button=0, down=False))
        assert ui.read(lambda state: state.dropped) == [0]


def test_an_accessibility_action_arrives_by_the_same_identity_a_click_uses() -> None:
    from vivid_sdk.overlay import AccessibilityEvent

    ui, _module, session = _started("a11y")
    with session:
        ui.dispatch(AccessibilityEvent(0, _Raw(), ui.region_id("volume"), "increment"))
        assert ui.read(lambda state: state.volume) == 45
        ui.dispatch(AccessibilityEvent(0, _Raw(), ui.region_id("volume"), "decrement"))
        assert ui.read(lambda state: state.volume) == 40

        ui.dispatch(AccessibilityEvent(0, _Raw(), ui.region_id("notifications"), "default"))
        assert ui.read(lambda state: state.notifications) is False

        ui.dispatch(AccessibilityEvent(0, _Raw(), ui.region_id("apply"), "click"))
        assert ui.read(lambda state: state.applied) == 1


def test_escape_and_q_end_an_example() -> None:
    from _ui import Keys, Mods

    # Escape ends it.
    ui, _module, session = _started("hello_world")
    with session:
        assert not ui.quitting()
        ui.dispatch(key(Keys.ESCAPE))
        assert ui.quitting()

    # So does `q`, which is what a hand on the keyboard reaches for first.
    ui, _module, session = _started("hello_world")
    with session:
        ui.dispatch(key(Keys.letter("q")))
        assert ui.quitting()

    # A chord is somebody else's shortcut: control-q is not this.
    ui, _module, session = _started("hello_world")
    with session:
        ui.dispatch(key(Keys.letter("q"), Mods.CONTROL))
        assert not ui.quitting()

    # Neither is a release, or a key repeat.
    ui, _module, session = _started("hello_world")
    with session:
        ui.dispatch(key(Keys.ESCAPE, down=False))
        ui.dispatch(key(Keys.letter("q"), repeat=True))
        assert not ui.quitting()


def test_a_window_with_a_field_in_it_spends_q_on_typing() -> None:
    from _ui import Keys

    # An example that accepts typed text cannot spend a letter on quitting: `q` goes into
    # the field, and escape is the way out.
    ui, _module, session = _started("input")
    with session:
        ui.dispatch(pointer(ui.region_id("field"), button=0, down=True, clicks=1))
        ui.dispatch(pointer(ui.region_id("field"), button=0, down=False))
        ui.dispatch(TextEvent(0, _Raw(), "q"))
        assert not ui.quitting()
        assert ui.read(lambda state: state.field.text) == "q"

        ui.dispatch(key(Keys.letter("q")))
        assert not ui.quitting()

        ui.dispatch(key(Keys.ESCAPE))
        assert ui.quitting()


def test_a_gallery_holds_no_more_pictures_than_it_budgeted_for() -> None:
    ui, module, session = _started("image_gallery")
    with session:
        ui.dispatch(hover(ui.region_id("gallery"), True))
        for _ in range(40):
            ui.dispatch(wheel(60.0))
            ui.paint()
        held = ui.read(lambda state: len(state.order))
        assert held <= module.BUDGET
        # And it really did scroll past more pictures than it kept.
        assert held < module.TILES


def test_every_example_builds_a_frame_the_protocol_accepts() -> None:
    """Each example, painted twice: once cold, once with the caches warm. A frame that the
    wire would refuse fails inside `paint`, so reaching the end is the assertion."""
    import importlib

    names = sorted(
        path.stem
        for path in (FilePath(__file__).resolve().parent.parent / "examples" / "python" / "ui").glob("*.py")
        if not path.stem.startswith("_")
    )
    assert len(names) == 32, names
    with OverlaySession.connect(dry_run=True) as session:
        for name in names:
            ui = importlib.import_module(name).make_ui()
            window = session.create_window(
                OverlayWindowOptions(Rect(60, 60, ui.width, ui.height))
            )
            ui.attach(session, window)
            ui.paint()
