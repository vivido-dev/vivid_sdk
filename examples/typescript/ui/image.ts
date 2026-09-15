// One picture, fitted into its box four ways.
//
// TypeScript twin of `vivid_ui/examples/image`. The Rust original encodes a PNG and decodes
// it again to prove the round trip; the protocol carries raw pixels either way, and neither
// stdlib Python nor Node has a decoder, so this port generates the checkerboard directly as
// RGBA and uploads that. The lesson is unchanged: a picture reaches the host as an upload
// and a rectangle, and the fit is arithmetic the producer does.
import { overlay } from "@vivido/vivid-sdk";
import { CAPTION, fitRect, rect, runMain, Ui, type Frame } from "./ui.js";

const WIDTH = 64;
const HEIGHT = 32;
const SQUARE = 8;
const SLOT = [96, 72] as const;

/** A checkerboard, because how a picture is fitted into a box is obvious in one and
 *  invisible in a photograph. */
function checkerboard(): Uint8Array {
  const pixels: number[] = [];
  for (let y = 0; y < HEIGHT; y += 1) {
    for (let x = 0; x < WIDTH; x += 1) {
      const dark = (Math.floor(x / SQUARE) + Math.floor(y / SQUARE)) % 2 === 0;
      pixels.push(...(dark ? [0x20, 0x20, 0x30, 0xff] : [0x70, 0x80, 0xd0, 0xff]));
    }
  }
  return new Uint8Array(pixels);
}

class Images {}

async function render(_state: Images, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  const picture = await f.upload("checkerboard", WIDTH, HEIGHT, checkerboard());

  /** A labelled well, so the only difference between the variants is the fit. */
  const slot = async (index: number, row: number, label: string): Promise<overlay.Rect> => {
    const x = 20 + index * (SLOT[0] + 12);
    const y = 20 + row * (SLOT[1] + 32);
    await f.label(label, rect(x, y, SLOT[0], 13), { size: 11, color: CAPTION, vcenter: false });
    const box = rect(x, y + 17, SLOT[0], SLOT[1]);
    f.box(box, { bg: 0x1a1a24ff, radius: 4 });
    return box;
  };

  /** Cover: the box filled, proportions kept, the overflow clipped away. */
  const cover = async (box: overlay.Rect, radius: number): Promise<void> => {
    f.canvas.save();
    f.clip(box, radius);
    const scale = Math.max(box.width / WIDTH, box.height / HEIGHT);
    await f.image(
      picture,
      rect(
        box.x + (box.width - WIDTH * scale) / 2,
        box.y + (box.height - HEIGHT * scale) / 2,
        WIDTH * scale,
        HEIGHT * scale,
      ),
    );
    f.canvas.restore();
  };

  // Natural size: the picture's own pixels, centred in the well rather than scaled.
  let box = await slot(0, 0, "natural size");
  await f.image(
    picture,
    rect(box.x + (box.width - WIDTH) / 2, box.y + (box.height - HEIGHT) / 2, WIDTH, HEIGHT),
  );

  // Contain: the whole picture, as large as fits, proportions kept.
  await f.image(picture, fitRect(WIDTH, HEIGHT, await slot(1, 0, "contain")));

  await cover(await slot(2, 0, "cover"), 4);

  // Fill: the box exactly, proportions abandoned.
  await f.image(picture, await slot(0, 1, "fill"));

  // The same picture at half opacity, which is the draw's own parameter.
  await f.image(picture, fitRect(WIDTH, HEIGHT, await slot(1, 1, "half opacity")), 0.5);

  // Rounded: a clip in the shape of the well, so the corners come off the picture.
  await cover(await slot(2, 1, "rounded"), 12);
}

export function makeUi(): Ui<Images> {
  return new Ui(new Images(), render, { width: 460, height: 280 });
}

await runMain(makeUi);
