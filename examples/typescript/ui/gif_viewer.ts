// An animation playing at its own frame rate.
//
// TypeScript twin of `vivid_ui/examples/gif_viewer`. Nothing here drives the animation: the
// frame being shown is a function of how long the window has been up, and the view asks the
// loop to wake it exactly when the next frame is due — so a held window sleeps, and a
// playing one wakes as often as the animation asks for and no more.
//
// The Rust original carries a generated GIF through a real encoder and decoder; the binding
// takes raw pixels, so this port generates the same eight frames directly and uploads each
// one. The lesson — a picture advancing on the window's clock — is unchanged.
import { Keys, rect, runMain, Ui, type Frame } from "./ui.js";

// How many frames the generated animation has, and how long each one lasts.
const FRAMES = 8;
const FRAME_MS = 80;
const SIZE = 96;

/** One frame of a dot walking around a ring: the same picture the Rust example encodes into
 *  a GIF, as the pixels the binding actually takes. */
function walkerFrame(step: number): Uint8Array {
  const angle = (step / FRAMES) * Math.PI * 2;
  const centre = SIZE / 2;
  const radius = SIZE * 0.3;
  const cx = centre + Math.cos(angle) * radius;
  const cy = centre + Math.sin(angle) * radius;
  const pixels = new Uint8Array(SIZE * SIZE * 4);
  let at = 0;
  for (let y = 0; y < SIZE; y += 1) {
    for (let x = 0; x < SIZE; x += 1) {
      const near = Math.hypot(x - cx, y - cy) < SIZE * 0.16;
      pixels.set(near ? [0x8e, 0xcb, 0xff, 0xff] : [0x14, 0x14, 0x1c, 0xff], at);
      at += 4;
    }
  }
  return pixels;
}

class Gif {
  playing = true;
}

async function render(state: Gif, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  f.caption(state.playing ? "playing — space to hold" : "held — space to play", { x: 20, y: 20 });

  // Held means the first frame, exactly as a paused source in the Rust toolkit reads the
  // clock at zero. Playing means the frame the window's age says is on.
  let frame = 0;
  if (state.playing) {
    const moment = f.elapsed;
    frame = Math.floor((moment * 1000) / FRAME_MS) % FRAMES;
    const step = FRAME_MS / 1000;
    f.requestFrameAfter(step - (moment % step));
  }
  const image = await f.upload(`walker-${frame}`, SIZE, SIZE, walkerFrame(frame));
  await f.image(image, rect(20, 46, 160, 160));

  await f.label(`${FRAMES} frames at ${FRAME_MS}ms`, rect(20, 218, f.width - 40, 14), {
    size: 11,
    color: 0x606080ff,
    vcenter: false,
  });
}

export function makeUi(): Ui<Gif> {
  const state = new Gif();
  const ui = new Ui(state, render, { width: 320, height: 260 });

  ui.onKey = (event) => {
    if (event.down && !event.repeat && event.physical === Keys.space) {
      state.playing = !state.playing;
      ui.notify();
    }
  };

  return ui;
}

await runMain(makeUi);
