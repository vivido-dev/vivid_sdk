"""A table with a fixed header and virtualized rows.

Python twin of `vivid_ui/examples/data_table`. A table is a list whose rows happen to be
columns. The header is outside the list so it does not scroll, and both use the same column
widths, which is what keeps them lined up without a measuring pass.

Five thousand rows exist; a dozen are drawn. That is not an optimization here — a display
list has a command ceiling, and a row costs commands whether or not anyone can see it.
"""
from __future__ import annotations

from typing import Any, Tuple

from _ui import Frame, ListState, Ui, duration
from vivid_sdk.overlay import Rect, WheelEvent

ROWS = 5000
ROW_HEIGHT = 26.0
VIEWPORT = 220.0

#: The columns, and how wide each one is.
COLUMNS: Tuple[Tuple[str, float], ...] = (
    ("symbol", 90.0),
    ("last", 90.0),
    ("change", 90.0),
    ("volume", 110.0),
)


def quote(index: int) -> Tuple[str, float, float, int]:
    """One generated row. Deterministic, so a test and a screenshot agree."""
    seed = index
    symbol = "".join(
        chr(ord("A") + value)
        for value in (seed % 26, (seed // 26) % 26, (seed // 676) % 26)
    )
    last = 10.0 + (seed % 9000) / 100.0
    change = ((seed % 401) - 200.0) / 100.0
    volume = 1000 + seed * 37 % 900_000
    return symbol, last, change, volume


class Table:
    def __init__(self) -> None:
        self.list = ListState(ROWS, ROW_HEIGHT)


def cell(f: Frame, box: Rect, text: str, color: int, align: str) -> None:
    """One cell, padded and clipped to its column — a column that cannot fit its content
    gives up the content, not the alignment."""
    f.label(
        text,
        Rect(box.x + 6, box.y, box.width - 12, box.height),
        size=12,
        color=color,
        align=align,
        max_width=box.width - 12,
    )


def render(state: Table, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.label(
        f"{ROWS} quotes",
        Rect(16, 16, f.width - 32, 14),
        size=12,
        color=0x8080A0FF,
        vcenter=False,
    )

    table_width = sum(width for _, width in COLUMNS)
    header = Rect(16, 36, table_width, 24)
    f.box(header, bg=0x22222EFF, radius=4)
    x = header.x
    for label, width in COLUMNS:
        cell(f, Rect(x, header.y, width, header.height), label, 0x9090B0FF, "start")
        x += width

    box = Rect(16, header.y + 24 + 6, table_width, VIEWPORT)

    def scrolled(event: Any) -> None:
        if isinstance(event, WheelEvent):
            state.list.scroll_by(event.dy, box.height)
            f.notify()

    f.region("table", box, radius=4, on_wheel=scrolled)
    f.canvas.save()
    f.clip(box, radius=4)
    first, last_row = state.list.visible_range(box.height)
    for index in range(first, last_row):
        top = box.y + state.list.row_top(index)
        row = Rect(box.x, top, table_width, ROW_HEIGHT)
        f.box(row, bg=0x16161EFF if index % 2 == 0 else 0x1A1A24FF)
        symbol, last_price, change, volume = quote(index)
        # A gain and a loss are the same number in different colours; nothing else differs.
        tint = 0x70D0A0FF if change >= 0 else 0xE08080FF
        values = (
            (symbol, 0xE6E6F0FF, "start"),
            (f"{last_price:.2f}", 0xD0D0E0FF, "end"),
            (f"{change:+.2f}", tint, "end"),
            (str(volume), 0xA0A0C0FF, "end"),
        )
        x = row.x
        for (value, color, align), (_, width) in zip(values, COLUMNS):
            cell(f, Rect(x, row.y, width, ROW_HEIGHT), value, color, align)
            x += width
    f.canvas.restore()

    track = Rect(box.x + table_width + 4, box.y, 8, box.height)
    f.scrollbar(track, state.list, box.height)


def make_ui() -> Ui:
    return Ui(Table(), render, width=460, height=300)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
