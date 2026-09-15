// Linear fills at several angles and stop sets.
//
// TypeScript twin of `vivid_ui/examples/gradient`. The angle convention is CSS's — 0 is
// upward, 90 rightward — and the helper turns it into the two endpoints the wire wants,
// spanning the box along the gradient's own direction. A hard stop is two stops at the same
// offset.
import { overlay } from "@vivido/vivid-sdk";
import { WHITE, gradientBrush, rect, runMain, Ui, type Frame } from "./ui.js";

const { Path } = overlay;

const PANELS: readonly (readonly [string, number, readonly (readonly [number, number])[]])[] = [
  ["to right", 90, [[0, 0xff5060ff], [1, 0x5060ffff]]],
  ["to bottom", 180, [[0, 0x50d0a0ff], [1, 0x104060ff]]],
  ["diagonal", 45, [[0, 0xffe080ff], [0.5, 0xff8060ff], [1, 0x603060ff]]],
  // Two stops at the same offset is a hard edge rather than a blend.
  ["hard stop", 90, [[0, 0x202030ff], [0.5, 0x202030ff], [0.5, 0x8080ffff], [1, 0x8080ffff]]],
];

class Gradients {}

async function render(_state: Gradients, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  for (const [index, [label, angle, stops]] of PANELS.entries()) {
    const panel = rect(20 + (index % 2) * (180 + 12), 20 + Math.floor(index / 2) * (110 + 12), 180, 110);
    f.canvas.fill(
      Path.roundedRectangle(panel, 8),
      gradientBrush(panel, angle, stops.map(([offset, color]) => ({ offset, color }))),
    );
    await f.label(label, rect(panel.x + 8, panel.y + panel.height - 22, panel.width - 16, 14), {
      size: 12,
      color: WHITE,
    });
  }
}

export function makeUi(): Ui<Gradients> {
  return new Ui(new Gradients(), render, { width: 420, height: 300 });
}

await runMain(makeUi);
