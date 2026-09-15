// A log pinned to its newest line, with a scrollbar.
//
// TypeScript twin of `vivid_ui/examples/list_example`. Two things make a log a log rather
// than a list: it follows its own tail until the reader scrolls away from it, and it takes
// that up again when the reader comes back. Both are one comparison against the maximum
// offset.
//
// The scrollbar is drawing, not a special case: a track, and a thumb whose length is the
// fraction of the content on screen and whose position is how far through it that is.
import { Keys, ListState, rect, runMain, Ui, type Frame } from "./ui.js";

const ROW_HEIGHT = 22;
const VIEWPORT = 200;

class Log {
  entries = Array.from(
    { length: 200 },
    (_, index) => `[${String(index).padStart(4, "0")}] the log said something worth keeping`,
  );
  readonly list = new ListState(200, ROW_HEIGHT);
  // Stay at the bottom as entries arrive, until the reader scrolls away.
  follow = true;

  constructor() {
    this.list.scrollTo(this.list.maximumOffset(VIEWPORT), VIEWPORT);
  }

  /** Append, following the tail if the reader has not scrolled away from it. */
  push(entry: string, viewport: number): void {
    this.entries.push(entry);
    this.list.setCount(this.entries.length, viewport);
    if (this.follow) this.list.scrollTo(this.list.maximumOffset(viewport), viewport);
  }

  /** Whether the list is at its end, which is what "following" means. */
  atTail(viewport: number): boolean {
    return Math.abs(this.list.offset - this.list.maximumOffset(viewport)) < 1;
  }
}

async function render(state: Log, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  await f.label(state.follow ? "following the tail" : "scrolled back", rect(16, 16, f.width - 32, 14), {
    size: 12,
    color: 0x8080a0ff,
    vcenter: false,
  });

  const box = rect(16, 38, f.width - 44, VIEWPORT);

  f.region("log", box, {
    radius: 6,
    onWheel: (event) => {
      state.list.scrollBy(event.dy, box.height);
      // Letting go of the tail is a scroll away from it; coming back takes it up again.
      state.follow = state.atTail(box.height);
      f.notify();
    },
  });
  f.box(box, { bg: 0x16161eff, radius: 6 });

  // Only the rows the box can show exist. The one past the bottom is the one sliding in.
  f.canvas.save();
  f.clip(box, 6);
  const [first, last] = state.list.visibleRange(box.height);
  for (let index = first; index < last; index += 1) {
    const top = box.y + state.list.rowTop(index);
    await f.label(state.entries[index]!, rect(box.x + 8, top, box.width - 16, ROW_HEIGHT), {
      size: 12,
      color: 0xc0c0d0ff,
    });
  }
  f.canvas.restore();

  f.scrollbar(rect(box.x + box.width + 4, box.y, 8, box.height), state.list, box.height);
}

export function makeUi(): Ui<Log> {
  const state = new Log();
  const ui = new Ui(state, render, { width: 420, height: 260 });

  ui.onKey = (event) => {
    if (!(event.down && !event.repeat && event.physical === Keys.enter)) return;
    const viewport = ui.boundsOf("log")?.height ?? VIEWPORT;
    state.push(`[${String(state.entries.length).padStart(4, "0")}] appended by hand`, viewport);
    ui.notify();
  };

  return ui;
}

await runMain(makeUi, import.meta.url);
