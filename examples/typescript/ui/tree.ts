// A box inside a box, 64 levels deep.
//
// TypeScript twin of `vivid_ui/examples/tree`. The Rust original is a lesson about layout
// cost — an auto-sized box asks the engine for its content size at every level, and that
// question compounds — which a port sidesteps entirely: each level's box is two pixels
// inside its parent's, which is arithmetic, not a solver. What survives is the display-list
// half of the lesson: 64 levels is 64 fills, 64 strokes, and 64 runs of text, and that is
// what a frame this deep costs.
import { rect, runMain, Ui, type Frame } from "./ui.js";

const PAD = 2;
const LABEL_HEIGHT = 14;

class DeepTree {
  constructor(readonly depth = 64) {}
}

async function render(state: DeepTree, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x0a0a12ff });
  let box = rect(8, 8, f.width - 16, f.height - 16);

  for (let depth = state.depth; depth >= 0; depth -= 1) {
    if (box.width <= 0 || box.height <= 0) break;
    // The shade cycles every six levels, which is what makes the nesting readable.
    const shade = 0x20 + (depth % 6) * 0x04;
    f.box(box, {
      bg: ((0x101018 + shade * 0x0101) << 8 >>> 0) + 0xff,
      border: 1,
      borderColor: 0x00000040,
    });
    await f.label(String(depth), rect(box.x + PAD, box.y + PAD, box.width - 2 * PAD, LABEL_HEIGHT), {
      size: 11,
      color: 0xc0c0d0ff,
      vcenter: false,
    });
    // Each level sits inside its parent's padding, below the label it drew.
    box = rect(
      box.x + PAD,
      box.y + PAD + LABEL_HEIGHT,
      Math.max(box.width - 2 * PAD, 0),
      Math.max(box.height - 2 * PAD - LABEL_HEIGHT, 0),
    );
  }
}

export function makeUi(): Ui<DeepTree> {
  return new Ui(new DeepTree(), render, { width: 500, height: 500 });
}

await runMain(makeUi);
