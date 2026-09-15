"""Wrapping, ellipsis, and a line ceiling.

Python twin of `vivid_ui/examples/text_wrapper`. None of this is arithmetic a producer can
do: where a line breaks depends on the font the host chose, so wrapping, truncation, and the
ellipsis that marks it are all the host's, asked for through a shaped paragraph.

That is the whole difference between the plain text command and a retained layout. The
plain one draws a run; this one is a paragraph with a width, a line ceiling, and an opinion
about what to do when it runs out of room.
"""
from __future__ import annotations

from _ui import CAPTION, Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect, StyledText, TextRun, TextStyle

PARAGRAPH = (
    "The quick brown fox jumps over the lazy dog, and then keeps going for long enough "
    "that the line it is on runs out of room and has to be broken somewhere sensible."
)
WIDTH = 150.0


class TextWrapper:
    pass


def render(_state: TextWrapper, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)

    samples = (
        ("wrapped", "wrapped", True, None),
        ("ellipsized", "one line, ellipsized", False, 1),
        ("clamped", "two lines, ellipsized", False, 2),
    )
    for index, (key, label, wrap, max_lines) in enumerate(samples):
        x = 20 + index * (WIDTH + 16)
        f.label(label, Rect(x, 20, WIDTH, 13), size=11, color=CAPTION, vcenter=False)
        box = Rect(x, 38, WIDTH, f.height - 58)
        # Clipped, because a paragraph that overruns its box is the box's business: the
        # ceiling decides how many lines exist, the clip decides how many are seen.
        f.canvas.save()
        f.clip(box)
        f.paragraph(
            key,
            StyledText(
                runs=[TextRun(PARAGRAPH, TextStyle(size=13, color=0xD0D0E0FF))],
                max_width=WIDTH,
                wrap=wrap or max_lines is not None,
                max_lines=max_lines,
                overflow="ellipsis" if max_lines is not None else "clip",
            ),
            Point(box.x, box.y),
        )
        f.canvas.restore()


def make_ui() -> Ui:
    return Ui(TextWrapper(), render, width=560, height=320)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
