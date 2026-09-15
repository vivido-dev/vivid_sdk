// A frame whose edges ask the host to resize.
//
// TypeScript twin of `vivid_ui/examples/window_shadow`. The edges are hit regions whose role
// is "resize", so the *host* performs the resize: the window grows or shrinks natively as an
// edge is dragged, and the frame redraws at whatever size it ends up. Nothing here
// reimplements window resizing — naming the region is the whole of it.
import { overlay } from "@vivido/vivid-sdk";
import {
  EDGE_BOTTOM,
  EDGE_LEFT,
  EDGE_RIGHT,
  EDGE_TOP,
  rect,
  runMain,
  Ui,
  type Frame,
} from "./ui.js";

const PAD = 12;
const THICK = 6;

// Each edge: the bitmask the host reads, and the shape the pointer takes there.
export const EDGES: readonly (readonly [string, number, overlay.CursorShape])[] = [
  ["left", EDGE_LEFT, "resize-left"],
  ["right", EDGE_RIGHT, "resize-right"],
  ["top", EDGE_TOP, "resize-up"],
  ["bottom", EDGE_BOTTOM, "resize-down"],
];

/** Where one edge's strip sits: a thin band just inside the frame it resizes. */
export function edgeBox(name: string, panel: overlay.Rect): overlay.Rect {
  if (name === "left") return rect(panel.x, panel.y, THICK, panel.height);
  if (name === "right") return rect(panel.x + panel.width - THICK, panel.y, THICK, panel.height);
  if (name === "top") return rect(panel.x, panel.y, panel.width, THICK);
  return rect(panel.x, panel.y + panel.height - THICK, panel.width, THICK);
}

class WindowFrame {}

async function render(_state: WindowFrame, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x0c0c14ff });
  const panel = rect(PAD, PAD, f.width - 2 * PAD, f.height - 2 * PAD);
  f.box(panel, { bg: 0x1a1a28ff, radius: 8, border: 1, borderColor: 0x3a3a52ff });
  await f.label(
    "Drag an edge to resize",
    rect(panel.x + 14, panel.y + 14, panel.width - 28, 18),
    { size: 13, color: 0xc0c0d0ff, vcenter: false },
  );
  // Painted last, so they sit over the frame they belong to.
  for (const [name, edges, cursor] of EDGES) {
    f.resizeRegion(`edge-${name}`, edgeBox(name, panel), edges, cursor);
  }
}

export function makeUi(): Ui<WindowFrame> {
  return new Ui(new WindowFrame(), render, { width: 360, height: 260 });
}

await runMain(makeUi, import.meta.url);
