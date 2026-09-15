"""Alignment, weight, slant, and decorations.

Python twin of `vivid_ui/examples/text_layout`: everything here is one paragraph per block,
so the alignment is the paragraph's own — a line centered inside a box it fills, not a box
that was moved. Decorations and mixed-weight runs are shaped paragraphs, because the plain
text command carries neither underlines nor several runs.
"""
from __future__ import annotations

from _ui import TEXT, Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect, StyledText, TextRun, TextStyle

BLOCK = 360.0
LABEL = 0x8080A0FF


class TextLayout:
    pass


def render(_state: TextLayout, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    y = 20.0

    def block(label: str) -> Rect:
        """A labelled 360-wide well. Returns the box the paragraph goes in."""
        nonlocal y
        f.label(label, Rect(20, y, BLOCK, 13), size=11, color=LABEL, vcenter=False)
        well = Rect(20, y + 17, BLOCK, 32)
        f.box(well, bg=0x20202CFF, radius=4)
        y = well.y + 32 + 12
        return Rect(well.x + 6, well.y + 6, BLOCK - 12, 20)

    # The three alignments, each one run placed by its own measured width.
    for label, text in (
        ("start", "Aligned to the start edge"),
        ("center", "Aligned to the center"),
        ("end", "Aligned to the end edge"),
    ):
        f.label(text, block(label), size=15, color=TEXT, align=label, vcenter=False)

    # A decoration is a shaped run: the plain command carries neither underlines nor
    # strikethroughs, so each of these goes through a retained layout.
    decorated = block("decorations")
    f.paragraph(
        "decorations",
            StyledText(
                runs=[
                    TextRun("underlined", TextStyle(size=15, color=0x8ECBFFFF, underline=True))
                ]
        ),
        Point(decorated.x, decorated.y),
    )

    # Mixed emphasis is one paragraph of several runs, not several boxes beside each other.
    weights = block("weights")
    f.paragraph(
        "weights",
            StyledText(
                runs=[
                    TextRun("regular ", TextStyle(size=16, color=TEXT)),
                    TextRun("bold ", TextStyle(size=16, color=TEXT, weight=700)),
                    TextRun("italic", TextStyle(size=16, color=TEXT, italic=True)),
                ]
        ),
        Point(weights.x, weights.y),
    )

    struck = block("struck through")
    f.paragraph(
        "struck",
            StyledText(
                runs=[
                    TextRun(
                        "a line that was wrong",
                        TextStyle(size=15, color=TEXT, strikethrough=True),
                    )
                ]
        ),
        Point(struck.x, struck.y),
    )


def make_ui() -> Ui:
    return Ui(TextLayout(), render, width=420, height=460)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
