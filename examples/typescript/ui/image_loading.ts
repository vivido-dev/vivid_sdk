// A picture that arrives, and one that never will.
//
// TypeScript twin of `vivid_ui/examples/image_loading`. Pixels come from somewhere else — a
// fetch, a file, a side channel — so the bytes are not always what they were promised to be.
// A picture that cannot be decoded leaves the box it would have been drawn in, styled and
// empty, and the frame keeps the rest of its content.
//
// The Rust original generates a PNG and hands it to the toolkit's decoder; the binding takes
// raw pixels, so this port generates the checkerboard directly. And since `uploadRgba` has
// no decoder to refuse them, "bytes that are not a picture" cannot exist here — the port
// draws the styled empty box the Rust view's failed decode leaves behind, which is the
// observable lesson either way.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

// How long the good picture takes to "arrive", so the wait is visible rather than instant.
const ARRIVING = 0.6;

/** A checkerboard as raw RGBA: the same picture the Rust example encodes as a PNG, in the
 *  form the binding actually takes. */
function checkerboard(size: number, square: number): Uint8Array {
  const pixels = new Uint8Array(size * size * 4);
  let at = 0;
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      const dark = (Math.floor(x / square) + Math.floor(y / square)) % 2 === 0;
      pixels.set(dark ? [0x30, 0x38, 0x60, 0xff] : [0x80, 0x90, 0xe0, 0xff], at);
      at += 4;
    }
  }
  return pixels;
}

class Loading {
  arriving = ARRIVING;
  elapsed = 0;

  hasArrived(): boolean {
    return this.elapsed >= this.arriving;
  }
}

async function render(state: Loading, f: Frame): Promise<void> {
  // The clock is the window's, so a test can stop it and watch the wait without waiting.
  state.elapsed = f.elapsed;
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });

  const slot = (x: number): overlay.Rect => rect(x, 36, 120, 96);
  const labelled = async (label: string, x: number): Promise<void> => {
    await f.label(label, rect(x, 20, 120, 12), { size: 10, color: 0x606078ff, vcenter: false });
    f.box(slot(x), { bg: 0x1a1a24ff, radius: 6, border: 1, borderColor: 0x303048ff });
  };

  // A picture that has not arrived is not an error — it is a box with nothing in it yet.
  await labelled("arriving", 20);
  if (state.hasArrived()) {
    await f.image(await f.upload("checkerboard", 96, 96, checkerboard(96, 12)), slot(20));
  } else {
    await f.label("loading…", slot(20), { size: 11, color: 0x606078ff, align: "center" });
    // The window is asked for the frame that will show it, at the moment it arrives. There
    // is no timer anywhere: a wait is a deadline on the window's own clock.
    f.requestFrameAfter(state.arriving - state.elapsed);
  }

  // The bytes here would be an error page. The box is drawn, the picture is not, and the
  // two pictures beside it are unaffected.
  await labelled("not a picture", 156);

  // The undamaged one, unaffected by its neighbour's failure.
  await labelled("undamaged", 292);
  await f.image(await f.upload("checkerboard", 96, 96, checkerboard(96, 12)), slot(292));

  await f.label(
    "a picture that cannot be decoded leaves its box behind",
    rect(20, 144, f.width - 40, 14),
    { size: 11, color: 0x606078ff, vcenter: false },
  );
}

export function makeUi(): Ui<Loading> {
  return new Ui(new Loading(), render, { width: 440, height: 240 });
}

await runMain(makeUi);
