// Ten thousand rows, of which a handful exist.
//
// TypeScript twin of `vivid_ui/examples/uniform_list`: a display list has a fixed command
// ceiling, so a row nobody can see must not be built. Scrolling changes *which* rows exist,
// not how many. The arithmetic is the helper's `ListState`, ported from the toolkit's own.
import { ListState, rect, runMain, Ui, type Frame } from "./ui.js";

const ROWS = 10_000;
const ROW_HEIGHT = 24;
// p(16), a caption line, gap(8): what remains of a 320x260 window.
const LIST_BOX = rect(16, 38, 288, 204);

class Rows {
  readonly list = new ListState(ROWS, ROW_HEIGHT);
}

/** The height the list was actually given: `boundsOf` is last frame's box, and on the first
 *  frame the declared height is the best estimate there is. */
function viewportOf(f: Frame): number {
  return f.boundsOf("list")?.height ?? LIST_BOX.height;
}

async function render(state: Rows, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  f.caption(`${ROWS} rows, a dozen of them real`, { x: 16, y: 16 });

  f.box(LIST_BOX, { bg: 0x14141cff, radius: 6 });
  f.canvas.save();
  f.clip(LIST_BOX, 6);
  const [first, end] = state.list.visibleRange(viewportOf(f));
  for (let index = first; index < end; index += 1) {
    const top = LIST_BOX.y + state.list.rowTop(index);
    const row = rect(LIST_BOX.x, top, LIST_BOX.width, ROW_HEIGHT);
    f.box(row, { bg: index % 2 === 0 ? 0x1a1a24ff : 0x14141cff });
    // px(8): the label sits in the row, inset from its left edge.
    await f.label(`Row ${index}`, rect(row.x + 8, row.y, row.width - 16, row.height), {
      size: 13,
      color: 0xd0d0e0ff,
    });
  }
  f.canvas.restore();

  f.region("list", LIST_BOX, {
    radius: 6,
    onWheel: (event) => {
      state.list.scrollBy(event.dy, viewportOf(f));
      f.notify();
    },
  });
}

export function makeUi(): Ui<Rows> {
  return new Ui(new Rows(), render, { width: 320, height: 260 });
}

await runMain(makeUi, import.meta.url);
