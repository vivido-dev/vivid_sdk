// A grid of pictures against a bounded upload budget.
//
// TypeScript twin of `vivid_ui/examples/image_gallery`. The host holds a bounded number of
// uploads, and a gallery is the case where that matters: scroll far enough and the pictures
// on screen are not the pictures uploaded first.
//
// The Rust toolkit has a cache that releases whatever was drawn least recently, so the view
// never thinks about it. A port has no toolkit, so the example owns that policy itself —
// which is the honest version of the lesson: someone has to decide what is no longer on
// screen, and here it is visibly the application. Pictures are generated rather than
// decoded, there being no decoder in the standard library.
import { overlay } from "@vivido/vivid-sdk";
import { ListState, rect, runMain, Ui, type Frame } from "./ui.js";

const TILES = 60;
const COLUMNS = 4;
const ROW_HEIGHT = 86;
const VIEWPORT = 200;
const TILE = 48;

// How many uploads the gallery keeps. Smaller than the host's own ceiling, so the eviction
// is this example's decision rather than the host's refusal.
const BUDGET = 24;

/** A small gradient, generated so the gallery carries its own pictures. */
function gradient(tint: readonly [number, number, number]): Uint8Array {
  const pixels: number[] = [];
  for (let y = 0; y < TILE; y += 1) {
    for (let x = 0; x < TILE; x += 1) {
      const across = Math.floor((x * 255) / TILE);
      const down = Math.floor((y * 255) / TILE);
      pixels.push(
        Math.floor((across * tint[0]) / 255),
        Math.floor((down * tint[1]) / 255),
        Math.floor((Math.floor((across + down) / 2) * tint[2]) / 255),
        0xff,
      );
    }
  }
  return new Uint8Array(pixels);
}

function tintOf(index: number): readonly [number, number, number] {
  return [(60 + index * 3) % 256, (120 + index * 5) % 256, ((200 - index * 2) % 256 + 256) % 256];
}

class Gallery {
  readonly list = new ListState(Math.ceil(TILES / COLUMNS), ROW_HEIGHT);
  // The uploads this gallery is holding, oldest first.
  readonly held = new Map<number, overlay.RetainedImage>();
  order: number[] = [];

  /** The picture for a tile, uploading it if this gallery is not already holding it, and
   *  letting the oldest one go when the budget is spent. */
  async picture(f: Frame, index: number): Promise<overlay.RetainedImage> {
    const existing = this.held.get(index);
    if (existing) {
      // Drawing is what keeps a picture: touching it moves it off the chopping block.
      this.order = this.order.filter((held) => held !== index);
      this.order.push(index);
      return existing;
    }
    const image = await f.upload(`tile-${index}`, TILE, TILE, gradient(tintOf(index)));
    this.held.set(index, image);
    this.order.push(index);
    while (this.order.length > BUDGET) {
      const oldest = this.order.shift()!;
      const released = this.held.get(oldest);
      if (released) {
        f.ui.releaseImage(released);
        this.held.delete(oldest);
      }
    }
    return image;
  }
}

async function render(state: Gallery, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  await f.label(
    `${TILES} pictures, a row at a time · ${state.order.length} of ${BUDGET} uploads held`,
    rect(16, 16, f.width - 32, 14),
    { size: 12, color: 0x8080a0ff, vcenter: false },
  );

  const box = rect(16, 38, f.width - 32, VIEWPORT);
  f.region("gallery", box, {
    radius: 6,
    onWheel: (event) => {
      state.list.scrollBy(event.dy, box.height);
      f.notify();
    },
  });
  f.canvas.save();
  f.clip(box, 6);
  const [first, last] = state.list.visibleRange(box.height);
  for (let row = first; row < last; row += 1) {
    const top = box.y + state.list.rowTop(row);
    for (let column = 0; column < COLUMNS; column += 1) {
      const index = row * COLUMNS + column;
      if (index >= TILES) break;
      await f.image(await state.picture(f, index), rect(box.x + 8 + column * 80, top + 7, 72, 72));
    }
  }
  f.canvas.restore();
}

export function makeUi(): Ui<Gallery> {
  return new Ui(new Gallery(), render, { width: 400, height: 260 });
}

await runMain(makeUi, import.meta.url);
