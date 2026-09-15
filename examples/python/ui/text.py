"""A type specimen, and the typography the wire carries.

Python twin of `vivid_ui/examples/text`. The plain text command carries a size, a weight, a
slant, a family and the two decorations — everything on the first three rows. Letter and
word spacing, a line height, and the font's own features ride `overlay-typography-v1`, and a
paragraph that asks for any of them must go through a shaped layout or the request is
silently dropped.

That distinction is invisible on screen and decides what the frame costs: a plain run is one
command, a shaped paragraph is a layout the host holds until it is replaced.
"""
from __future__ import annotations

from _ui import CAPTION, TEXT, Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect, StyledText, TextRun, TextStyle

#: Empty means the host's own default, which is what a terminal-friendly face is.
FAMILIES = ("", "Menlo", "Georgia")
RAMP = (11.0, 14.0, 18.0, 24.0, 32.0)


class Specimen:
    pass


def render(_state: Specimen, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x12121AFF)
    y = 20.0

    def section(label: str, height: float) -> Rect:
        """A labelled band. Returns the box its samples go in."""
        nonlocal y
        f.label(label, Rect(20, y, f.width - 40, 12), size=10, color=0x606078FF, vcenter=False)
        box = Rect(20, y + 15, f.width - 40, height)
        y = box.y + height + 10
        return box

    # The size ramp, sitting on one baseline — which is what makes it a ramp rather than a
    # row of boxes. The baseline comes from the measurement, not from the size.
    box = section("size", 38)
    x = box.x
    baseline = max(f.baseline_of(f"{size:.0f}", size) for size in RAMP)
    for size in RAMP:
        text = f"{size:.0f}"
        f.label(
            text,
            Rect(x, box.y + baseline - f.baseline_of(text, size), 60, size * 1.4),
            size=size,
            color=0xD8D8E8FF,
            vcenter=False,
        )
        x += f.measure(text, size).width + 14

    # Weight, slant, and the two decorations. Decorations need shaping; the rest do not.
    box = section("weight and slant", 20)
    x = box.x
    for label, weight, italic in (("regular", 400, False), ("bold", 700, False), ("italic", 400, True)):
        f.label(label, Rect(x, box.y, 90, 20), size=16, color=0xD8D8E8FF, weight=weight, italic=italic)
        x += f.measure(label, 16, weight=weight, italic=italic).width + 14
    for key, label, underline, strike in (
        ("under", "under", True, False),
        ("struck", "struck", False, True),
    ):
        f.paragraph(
            key,
            StyledText(
                runs=[
                    TextRun(
                        label,
                        TextStyle(size=16, color=0xD8D8E8FF, underline=underline, strikethrough=strike),
                    )
                ]
            ),
            Point(x, box.y),
        )
        x += f.measure(label, 16).width + 14

    # Families. A host without the named family substitutes its own, which is why this
    # names the family rather than promising the glyphs.
    box = section("family", 20)
    x = box.x
    for family in FAMILIES:
        label = family or "default"
        f.label(label, Rect(x, box.y, 100, 20), size=16, color=0xD8D8E8FF, family=family)
        x += f.measure(label, 16, family=family).width + 14

    # The typography the plain command cannot carry. Each of these is shaped.
    box = section("spacing and leading", 90)
    f.paragraph(
        "tracked",
        StyledText(
            runs=[TextRun("loose tracking, wide word spacing", TextStyle(size=15, color=0x9ECBFFFF))],
            letter_spacing=2.5,
            word_spacing=6.0,
        ),
        Point(box.x, box.y),
    )
    # A line height is a measurement as well as a drawing instruction, so this paragraph
    # occupies the taller box the layout was told about, not only the taller box on screen.
    f.paragraph(
        "leading",
        StyledText(
            runs=[
                TextRun(
                    "one line\nset on a taller rhythm\nthan the font would choose",
                    TextStyle(size=14, color=0x9ECBFFFF),
                )
            ],
            line_height=28.0,
        ),
        Point(box.x, box.y + 26),
    )


def make_ui() -> Ui:
    return Ui(Specimen(), render, width=520, height=400)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
