// Panels placed against each corner of their parent.
//
// TypeScript twin of `vivid_ui/examples/anchor`. The toolkit spells these as an anchor plus
// an offset and lets layout resolve them; a port has the parent's box in hand, so each
// corner is one line of arithmetic. Painting them last is what puts them over the field
// they sit on — in a display list, paint order is the whole of z-order.
import { WHITE, rect, runMain, Ui, type Frame } from "./ui.js";

// Each corner, as a fraction of the field plus the direction the 8px offset pushes.
const CORNERS: readonly (readonly [string, number, number])[] = [
  ["top left", 0, 0],
  ["top center", 0.5, 0],
  ["top right", 1, 0],
  ["bottom left", 0, 1],
  ["bottom center", 0.5, 1],
  ["bottom right", 1, 1],
];

const OFFSET = 8;
const PAD = 6;

class Anchors {}

async function render(_state: Anchors, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  const field = rect(16, 16, 380, 220);
  f.box(field, { bg: 0x1c1c2cff, radius: 8, border: 1, borderColor: 0x30304aff });
  f.caption("the anchor's parent", { x: field.x + 8, y: field.y + 8 });

  for (const [label, fx, fy] of CORNERS) {
    const measured = await f.measure(label, 11);
    const width = measured.width + PAD * 2;
    const height = measured.height + PAD * 2;
    // The offset pushes inward from whichever edge the anchor names, and a centered panel
    // is centered rather than offset.
    const x = field.x + (field.width - width) * fx + (fx === 0 ? OFFSET : fx === 1 ? -OFFSET : 0);
    const y = field.y + (field.height - height) * fy + (fy === 0 ? OFFSET : -OFFSET);
    const panel = rect(x, y, width, height);
    f.box(panel, { bg: 0x4a5fd0ff, radius: 4 });
    await f.label(label, panel, { size: 11, color: WHITE, align: "center" });
  }
}

export function makeUi(): Ui<Anchors> {
  return new Ui(new Anchors(), render, { width: 420, height: 260 });
}

await runMain(makeUi);
