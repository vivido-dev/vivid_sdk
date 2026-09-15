// A table with a fixed header and virtualized rows.
//
// TypeScript twin of `vivid_ui/examples/data_table`. A table is a list whose rows happen to
// be columns. The header is outside the list so it does not scroll, and both use the same
// column widths, which is what keeps them lined up without a measuring pass.
//
// Five thousand rows exist; a dozen are drawn. That is not an optimization here — a display
// list has a command ceiling, and a row costs commands whether or not anyone can see it.
import { overlay } from "@vivido/vivid-sdk";
import { ListState, rect, runMain, Ui, type Frame } from "./ui.js";

const ROWS = 5000;
const ROW_HEIGHT = 26;
const VIEWPORT = 220;

// The columns, and how wide each one is.
const COLUMNS: readonly (readonly [string, number])[] = [
  ["symbol", 90],
  ["last", 90],
  ["change", 90],
  ["volume", 110],
];

/** One generated row. Deterministic, so a test and a screenshot agree. */
export function quote(index: number): readonly [string, number, number, number] {
  const seed = index;
  const symbol = [seed % 26, Math.floor(seed / 26) % 26, Math.floor(seed / 676) % 26]
    .map((value) => String.fromCharCode(65 + value))
    .join("");
  const last = 10 + (seed % 9000) / 100;
  const change = ((seed % 401) - 200) / 100;
  const volume = 1000 + ((seed * 37) % 900_000);
  return [symbol, last, change, volume];
}

class Table {
  readonly list = new ListState(ROWS, ROW_HEIGHT);
}

/** One cell, padded and clipped to its column — a column that cannot fit its content gives
 *  up the content, not the alignment. */
async function cell(
  f: Frame,
  box: overlay.Rect,
  text: string,
  color: number,
  align: "start" | "end",
): Promise<void> {
  await f.label(text, rect(box.x + 6, box.y, box.width - 12, box.height), {
    size: 12,
    color,
    align,
    maxWidth: box.width - 12,
  });
}

async function render(state: Table, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  await f.label(`${ROWS} quotes`, rect(16, 16, f.width - 32, 14), {
    size: 12,
    color: 0x8080a0ff,
    vcenter: false,
  });

  const tableWidth = COLUMNS.reduce((total, [, width]) => total + width, 0);
  const header = rect(16, 36, tableWidth, 24);
  f.box(header, { bg: 0x22222eff, radius: 4 });
  let x = header.x;
  for (const [label, width] of COLUMNS) {
    await cell(f, rect(x, header.y, width, header.height), label, 0x9090b0ff, "start");
    x += width;
  }

  const box = rect(16, header.y + 30, tableWidth, VIEWPORT);
  f.region("table", box, {
    radius: 4,
    onWheel: (event) => {
      state.list.scrollBy(event.dy, box.height);
      f.notify();
    },
  });
  f.canvas.save();
  f.clip(box, 4);
  const [first, lastRow] = state.list.visibleRange(box.height);
  for (let index = first; index < lastRow; index += 1) {
    const row = rect(box.x, box.y + state.list.rowTop(index), tableWidth, ROW_HEIGHT);
    f.box(row, { bg: index % 2 === 0 ? 0x16161eff : 0x1a1a24ff });
    const [symbol, lastPrice, change, volume] = quote(index);
    // A gain and a loss are the same number in different colours; nothing else differs.
    const tint = change >= 0 ? 0x70d0a0ff : 0xe08080ff;
    const values: readonly (readonly [string, number, "start" | "end"])[] = [
      [symbol, 0xe6e6f0ff, "start"],
      [lastPrice.toFixed(2), 0xd0d0e0ff, "end"],
      [`${change >= 0 ? "+" : ""}${change.toFixed(2)}`, tint, "end"],
      [String(volume), 0xa0a0c0ff, "end"],
    ];
    let cellX = row.x;
    for (const [index2, [value, color, align]] of values.entries()) {
      const width = COLUMNS[index2]![1];
      await cell(f, rect(cellX, row.y, width, ROW_HEIGHT), value, color, align);
      cellX += width;
    }
  }
  f.canvas.restore();

  f.scrollbar(rect(box.x + tableWidth + 4, box.y, 8, box.height), state.list, box.height);
}

export function makeUi(): Ui<Table> {
  return new Ui(new Table(), render, { width: 460, height: 300 });
}

await runMain(makeUi, import.meta.url);
