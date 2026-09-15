"""A panel that describes itself: roles, values, states, actions.

Python twin of `vivid_ui/examples/a11y`. A display list is opaque — it says which rectangles
are filled, never which one is a switch. So the panel describes itself, and assistive
technology reads that description rather than guessing from the drawing.

Nothing is inferred: the switch is announced as a switch because it says so. The description
names the scene revision it belongs to, so it is published only once that scene is on
screen — which is the rule that stops a screen reader announcing a control that has gone.
An offline session refuses the publication outright, which is why this one is only fully
itself in a live pane.
"""
from __future__ import annotations

from typing import Any, Callable, List

from _ui import WHITE, Frame, Ui, duration
from vivid_sdk.overlay import AccessibilityEvent, Rect, SemanticNode

ITEMS = ("Appearance", "Keyboard", "Network")
VOLUME_STEP = 5


class Panel:
    def __init__(self) -> None:
        self.notifications = True
        self.volume = 40
        self.selected = 0
        #: How many times the panel has been applied, so a test can see an action arrive.
        self.applied = 0

    def adjust(self, by: int) -> None:
        self.volume = min(max(self.volume + by, 0), 100)


def render(state: Panel, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    nodes: List[SemanticNode] = []
    width = f.width - 40

    f.label("Settings", Rect(20, 20, width, 24), size=18, color=0xE6E6F0FF, vcenter=False)

    # A switch: a state, not a value.
    def toggle(_event: Any) -> None:
        state.notifications = not state.notifications
        f.notify()

    row = Rect(20, 54, width, 32)
    f.region("notifications", row, radius=6, cursor="pointer", on_click=toggle, on_accessibility=toggle)
    f.box(row, bg=0x1A1A24FF, radius=6)
    f.label("Notifications", Rect(row.x + 10, row.y, 200, row.height), size=13, color=0xD0D0E0FF)
    knob = Rect(row.x + row.width - 46, row.y + 7, 36, 18)
    f.box(knob, bg=0x4A5FD0FF if state.notifications else 0x33334AFF, radius=9)
    nodes.append(
        SemanticNode(
            id=f.ui.region_id("notifications"),
            role="switch",
            bounds=row,
            label="Notifications",
            toggled="on" if state.notifications else "off",
            actions=("default", "click"),
        )
    )

    # A spin button: a value in a range, which increments and decrements.
    def spin(event: AccessibilityEvent) -> None:
        # The action says which way; the step is the application's to choose.
        if event.action == "increment":
            state.adjust(VOLUME_STEP)
        elif event.action == "decrement":
            state.adjust(-VOLUME_STEP)
        f.notify()

    row = Rect(20, 94, width, 32)
    f.region("volume", row, radius=6, on_accessibility=spin)
    f.box(row, bg=0x1A1A24FF, radius=6)
    f.label("Volume", Rect(row.x + 10, row.y, 200, row.height), size=13, color=0xD0D0E0FF)
    f.label(
        str(state.volume),
        Rect(row.x, row.y, row.width - 10, row.height),
        size=13,
        color=0x8ECBFFFF,
        align="end",
    )
    nodes.append(
        SemanticNode(
            id=f.ui.region_id("volume"),
            role="spin-button",
            bounds=row,
            label="Volume",
            numeric=(float(state.volume), 0.0, 100.0),
            actions=("increment", "decrement"),
        )
    )

    # A list, whose items say which of how many they are.
    def choose(index: int) -> Callable[[Any], None]:
        def handler(_event: Any) -> None:
            state.selected = index
            f.notify()

        return handler

    list_top = 134.0
    for index, label in enumerate(ITEMS):
        item = Rect(20, list_top + index * (26 + 4), width, 26)
        chosen = state.selected == index
        element = f.region(
            f"item-{index}",
            item,
            radius=4,
            cursor="pointer",
            on_click=choose(index),
            on_accessibility=choose(index),
        )
        f.box(
            item,
            bg=0x2A2A3CFF if element.hovered else (0x24304AFF if chosen else 0x181824FF),
            radius=4,
        )
        f.label(label, Rect(item.x + 8, item.y, item.width - 16, item.height), size=13, color=0xD0D0E0FF)
        nodes.append(
            SemanticNode(
                id=f.ui.region_id(f"item-{index}"),
                role="list-item",
                bounds=item,
                label=label,
                # "The second of three", which is what a screen reader reads out.
                set=(index + 1, len(ITEMS)),
                actions=("default", "click"),
            )
        )

    def applied(_event: Any) -> None:
        state.applied += 1
        f.notify()

    # Drawn by hand rather than through the button helper, because this one answers an
    # accessibility action as well as a click — and one element is one region.
    apply_box = Rect(20, list_top + len(ITEMS) * 30 + 8, width, 32)
    element = f.region(
        "apply",
        apply_box,
        radius=6,
        cursor="pointer",
        on_click=applied,
        on_accessibility=applied,
    )
    f.box(apply_box, bg=0x5A6FE0FF if element.hovered else 0x4A5FD0FF, radius=6)
    f.label("Apply", apply_box, size=13, color=WHITE, align="center")
    nodes.append(
        SemanticNode(
            id=f.ui.region_id("apply"),
            role="button",
            bounds=apply_box,
            label="Apply settings",
            actions=("default", "click"),
        )
    )

    f.publish_semantics(nodes)


def make_ui() -> Ui:
    return Ui(Panel(), render, width=360, height=340)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
