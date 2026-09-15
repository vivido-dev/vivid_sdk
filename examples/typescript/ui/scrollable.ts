// Content taller than its box, scrolled by the wheel.
//
// TypeScript twin of `vivid_ui/examples/scrollable`: a fixed box clips a column of rows, and
// the wheel moves the content under the clip. The rows past the edge are simply not drawn —
// the same arithmetic a virtualized list is, written out once where it can be watched.
import { rect, runMain, Ui, type Frame } from "./ui.js";

const ROW_HEIGHT = 28;
const VISIBLE_ROWS = 6;
const ROWS = 40;

class Scrollable {
  offset = 0;

  /** The largest offset that still leaves content on screen. */
  maximumOffset(): number {
    return Math.max(ROWS * ROW_HEIGHT - VISIBLE_ROWS * ROW_HEIGHT, 0);
  }

  /** Move the content by a wheel delta. A positive `dy` is a scroll down, so the content
   *  moves up: the offset from the top of the content grows. */
  scrollBy(dy: number): void {
    this.offset = Math.min(Math.max(this.offset + dy, 0), this.maximumOffset());
  }
}

async function render(state: Scrollable, f: Frame): Promise<void> {
  const viewport = rect(0, 0, 300, VISIBLE_ROWS * ROW_HEIGHT);
  f.box(viewport, { bg: 0x101018ff });

  // The content is clipped: a row scrolled past the edge is neither drawn nor reachable.
  f.canvas.save();
  f.clip(viewport);
  for (let index = 0; index < ROWS; index += 1) {
    const top = index * ROW_HEIGHT - state.offset;
    if (top > viewport.height || top + ROW_HEIGHT < 0) continue;
    const row = rect(0, top, 300, ROW_HEIGHT);
    f.box(row, { bg: index % 2 === 0 ? 0x1a1a24ff : 0x14141cff });
    await f.label(`Row ${index}`, row, { size: 14, color: 0xd0d0e0ff });
  }
  f.canvas.restore();

  f.region("viewport", viewport, {
    onWheel: (event) => {
      state.scrollBy(event.dy);
      f.notify();
    },
  });
}

export function makeUi(): Ui<Scrollable> {
  return new Ui(new Scrollable(), render, { width: 300, height: VISIBLE_ROWS * ROW_HEIGHT });
}

await runMain(makeUi, import.meta.url);
