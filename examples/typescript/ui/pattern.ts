// A tiled background, next to a gradient.
//
// TypeScript twin of `vivid_ui/examples/pattern`. There is no hatch primitive in the
// protocol, so the tile is an image uploaded once and repeated — the same appearance,
// through the path a sprite or a texture atlas would use. The repeat is the brush's, not a
// loop: one fill command covers the whole box however large it is.
import { overlay } from "@vivido/vivid-sdk";
import { WHITE, gradientBrush, rect, runMain, Ui, type Frame } from "./ui.js";

const { Brush, Path } = overlay;
const SIZE = 8;

/** An 8 by 8 diagonal hatch, as RGBA8. */
function tilePixels(): Uint8Array {
  const pixels: number[] = [];
  for (let y = 0; y < SIZE; y += 1) {
    for (let x = 0; x < SIZE; x += 1) {
      // Every other diagonal is lighter, which reads as a slash pattern.
      const lit = (x + y) % 4 < 2;
      pixels.push(...(lit ? [0x50, 0x60, 0xe0, 0xff] : [0x1c, 0x1c, 0x2c, 0xff]));
    }
  }
  return new Uint8Array(pixels);
}

class Patterned {}

async function render(_state: Patterned, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });

  const hatched = rect(20, 20, 360, 120);
  const tile = await f.upload("hatch", SIZE, SIZE, tilePixels());
  // "repeat" is what makes one 8x8 upload cover 360 by 120.
  f.canvas.fill(Path.roundedRectangle(hatched, 6), Brush.image(tile, undefined, "repeat"));

  const graded = rect(20, 152, 360, 120);
  f.canvas.fill(
    Path.roundedRectangle(graded, 6),
    gradientBrush(graded, 135, [
      { offset: 0, color: 0x203050ff },
      { offset: 1, color: 0x50a0d0ff },
    ]),
  );
  await f.label("gradient", rect(graded.x + 10, graded.y + 8, 200, 16), { size: 13, color: WHITE });
}

export function makeUi(): Ui<Patterned> {
  return new Ui(new Patterned(), render, { width: 400, height: 300 });
}

await runMain(makeUi);
