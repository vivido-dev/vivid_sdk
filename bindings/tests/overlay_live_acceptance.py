"""Bounded manual/native acceptance probe for a live Vivido pane.

This is test tooling, not an SDK example. It intentionally uses the installed Python binding and
the pane capability inherited from Vivido.
"""

from __future__ import annotations

import time

from vivid_sdk import OverlaySession, OverlayWindowOptions
from vivid_sdk.overlay import (
    Brush,
    Canvas,
    FocusEvent,
    ImeEvent,
    KeyEvent,
    Path,
    Point,
    PointerEvent,
    Rect,
    StyledText,
    TextEvent,
    TextRun,
    TextStyle,
    WheelEvent,
)


def main() -> None:
    with OverlaySession.from_env() as session:
        with session.create_window(
            OverlayWindowOptions(Rect(80, 70, 420, 190))
        ) as window:
            label = StyledText(
                (
                    TextRun(
                        "Live overlay input",
                        TextStyle(size=24, weight=650, color=0xFFFFFFFF),
                    ),
                    TextRun(
                        "\nPaste or type; Escape finishes",
                        TextStyle(size=16, color=0xC8D8FFFF),
                    ),
                ),
                max_width=380,
                line_height=30,
            )
            layout = window.layout_text(label)
            canvas = Canvas().fill(
                Path.rounded_rectangle(Rect(0, 0, 420, 190), 16),
                Brush.solid(0x18243CFA),
            )
            window.draw_text_layout(canvas, layout, Point(20, 18))
            canvas = canvas.fill(
                Path.rounded_rectangle(Rect(20, 105, 380, 48), 8),
                Brush.solid(0x0A1020FF),
            ).hit(0xACCE55, Path.rectangle(Rect(20, 105, 380, 48)))
            receipt = window.submit(canvas)
            if receipt.wait(10) != "presented":
                raise RuntimeError("overlay scene was not presented")
            window.request_focus()
            window.set_editor_geometry(receipt.revision, Rect(32, 116, 2, 24))
            print(f"OVERLAY_ACCEPT_READY revision={receipt.revision}", flush=True)

            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                event = session.wait_event(0.25)
                if event is None or not event.targets(window):
                    continue
                if isinstance(event, FocusEvent):
                    print(f"OVERLAY_FOCUS focused={event.focused}", flush=True)
                elif isinstance(event, TextEvent):
                    print(f"OVERLAY_TEXT value={event.text!r}", flush=True)
                elif isinstance(event, ImeEvent):
                    print(
                        f"OVERLAY_IME preedit={event.preedit!r} selection={event.selection!r}",
                        flush=True,
                    )
                elif isinstance(event, KeyEvent):
                    print(
                        "OVERLAY_KEY "
                        f"physical={event.physical} down={event.down} "
                        f"repeat={event.repeat} modifiers={event.modifiers}",
                        flush=True,
                    )
                    if event.physical == 41 and event.down:
                        break
                elif isinstance(event, PointerEvent):
                    print(
                        "OVERLAY_POINTER "
                        f"position={event.position!r} region={event.application_id} "
                        f"button={event.button} down={event.down} modifiers={event.modifiers}",
                        flush=True,
                    )
                elif isinstance(event, WheelEvent):
                    print(
                        "OVERLAY_WHEEL "
                        f"position={event.position!r} dx={event.dx} dy={event.dy} "
                        f"modifiers={event.modifiers}",
                        flush=True,
                    )
            else:
                raise TimeoutError("live overlay input probe timed out")

            window.set_editor_geometry(receipt.revision, None)
            window.release_text_layout(layout)
            print("OVERLAY_ACCEPT_DONE", flush=True)


if __name__ == "__main__":
    main()
