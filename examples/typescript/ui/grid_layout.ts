// A page laid out on grid tracks.
//
// TypeScript twin of `vivid_ui/examples/grid_layout`. Two fixed columns and three rows is
// the kind of layout a grid engine is for, and without one it is the arithmetic a grid
// engine does: a fixed 140px sidebar, the rest to the main column, the header and footer at
// their content heights, and the body taking what is left. The window's own width decides
// which arrangement applies — a geometry event repaints, so a resize re-runs the branch.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

// Past this, the sidebar sits beside the content; below it, above the content.
const WIDE = 600;
const SIDEBAR = 140;
const GAP = 10;
const PAD = 16;
const HEADER = 44;
const FOOTER = 44;

class Page {}

async function cell(f: Frame, box: overlay.Rect, label: string, color: number): Promise<void> {
  f.box(box, { bg: color, radius: 6 });
  await f.label(label, box, { size: 13, color: 0xf0f0f8ff, align: "center" });
}

async function render(_state: Page, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  const width = f.width - 2 * PAD;
  const bodyTop = PAD + HEADER + GAP;
  const bodyBottom = f.height - PAD - FOOTER - GAP;

  await cell(f, rect(PAD, PAD, width, HEADER), "header", 0x30364cff);

  if (f.width >= WIDE) {
    // Side by side: the sidebar takes its fixed track, the main column the rest.
    await cell(f, rect(PAD, bodyTop, SIDEBAR, bodyBottom - bodyTop), "sidebar", 0x2a4a3aff);
    await cell(
      f,
      rect(PAD + SIDEBAR + GAP, bodyTop, width - SIDEBAR - GAP, bodyBottom - bodyTop),
      "main",
      0x1e2a44ff,
    );
  } else {
    // Narrow: the sidebar collapses into a strip above the content.
    const strip = 48;
    await cell(f, rect(PAD, bodyTop, width, strip), "side", 0x2a4a3aff);
    await cell(
      f,
      rect(PAD, bodyTop + strip + GAP, width, bodyBottom - bodyTop - strip - GAP),
      "main",
      0x1e2a44ff,
    );
  }

  await cell(f, rect(PAD, bodyBottom + GAP, width, FOOTER), "footer", 0x3a2a3aff);
}

export function makeUi(): Ui<Page> {
  return new Ui(new Page(), render, { width: 640, height: 360 });
}

await runMain(makeUi, import.meta.url);
