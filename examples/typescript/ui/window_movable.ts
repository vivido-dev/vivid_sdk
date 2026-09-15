// A title bar that moves the window, beside the edges that resize it.
//
// TypeScript twin of `vivid_ui/examples/window_movable`. Both are the same mechanism: a
// region's *role* tells the host what a drag on it means — move the window, or resize it
// from that edge — and the host does the work natively, without a frame in between.
//
// An overlay window has no titlebar of its own; the terminal behind it does. So what is
// movable here is the part of the pane the application nominates.
import { rect, runMain, Ui, type Frame } from "./ui.js";
// The edges are the same four this window's sibling declares, so they are shared rather
// than written twice — the Rust views duplicate the table between them.
import { EDGES, edgeBox } from "./window_shadow.js";

const PAD = 12;
const TITLE_BAR = 30;

class Movable {
  // How many drags ended here — what a view does with the notification, the host having
  // already done the moving.
  drags = 0;
}

async function render(state: Movable, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x0c0c14ff });
  const panel = rect(PAD, PAD, f.width - 2 * PAD, f.height - 2 * PAD);
  f.box(panel, { bg: 0x1a1a28ff, radius: 8, border: 1, borderColor: 0x3a3a52ff });

  // The bar is a drag handle first and an element second. It carries no click handler for
  // the same reason: the host is already moving the window, and a region that both moved it
  // and reported a click would do both.
  const bar = rect(panel.x, panel.y, panel.width, TITLE_BAR);
  f.region("title-bar", bar, {
    role: "drag",
    cursor: "pointer",
    onMouseUp: () => {
      // A drag ends with a release the view can hear about, which is what "remember where
      // the window was put" would hang off.
      state.drags += 1;
      f.notify();
    },
  });
  f.box(bar, { bg: 0x26263cff, radius: 8 });
  // Square off the bar's own bottom corners, so only the window's top ones are round.
  f.box(rect(bar.x, bar.y + TITLE_BAR / 2, bar.width, TITLE_BAR / 2), { bg: 0x26263cff });
  await f.label("drag this bar to move the window", bar, {
    size: 12,
    color: 0xc0c0d0ff,
    align: "center",
  });

  await f.label(
    "the four edges resize, the bar moves",
    rect(panel.x + 14, bar.y + TITLE_BAR + 14, panel.width - 28, 16),
    { size: 12, color: 0x8080a0ff, vcenter: false },
  );
  await f.label(
    `${state.drags} drags so far`,
    rect(panel.x + 14, bar.y + TITLE_BAR + 36, panel.width - 28, 16),
    { size: 12, color: 0x606078ff, vcenter: false },
  );

  for (const [name, edges, cursor] of EDGES) {
    f.resizeRegion(`edge-${name}`, edgeBox(name, panel), edges, cursor);
  }
}

export function makeUi(): Ui<Movable> {
  return new Ui(new Movable(), render, { width: 384, height: 264 });
}

await runMain(makeUi);
