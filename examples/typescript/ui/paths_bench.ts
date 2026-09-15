// A thousand star paths in one frame, against the frame's three ceilings.
//
// TypeScript twin of `vivid_ui/examples/paths_bench`. A display list has three ceilings and
// they are not the same size: a path costs one *command* (4096 a frame), eleven *segments*
// (65536 a frame), and about two hundred *bytes* once encoded — and it is the bytes that run
// out first, because the scene a host grants is capped well below the profile's own ceiling.
// A window this size is granted at most 256 KiB, which is where the default comes from: a
// thousand stars fit, twelve hundred do not.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

const { Brush, Path } = overlay;

// A star comes out as eleven segments: a move, nine lines, and a close.
const SEGMENTS_PER_STAR = 11;

// The counts a click steps down through, so the frame can be seen shrinking.
const STEPS = [1000, 500, 250, 125] as const;

const INK = 0xd0a860ff;

class PathsBench {
  step = 0;

  get stars(): number {
    return STEPS[this.step]!;
  }

  segments(): number {
    return this.stars * SEGMENTS_PER_STAR;
  }

  /** Roughly what this frame's drawing weighs once encoded. The view cannot ask: the
   *  granted scene size is negotiated below the toolkit. A star's worth of coordinates
   *  encodes to about 220 bytes, which is close enough to show what the ceiling is doing. */
  kilobytes(): number {
    return Math.floor((this.stars * 220) / 1024);
  }
}

/** Where the `index`th star sits, in a grid that covers the box. */
function place(bounds: overlay.Rect, index: number, stars: number): readonly [number, number, number] {
  const columns = Math.max(Math.ceil(Math.sqrt(stars)), 1);
  const rows = Math.max(Math.ceil(stars / columns), 1);
  const cell = [bounds.width / columns, bounds.height / rows] as const;
  const column = index % columns;
  const row = Math.floor(index / columns);
  return [
    bounds.x + cell[0] * (column + 0.5),
    bounds.y + cell[1] * (row + 0.5),
    Math.min(cell[0], cell[1]) * 0.44,
  ];
}

/** Ten corners alternating between two radii, starting upward. */
function star(cx: number, cy: number, radius: number): overlay.Path {
  const path = new Path();
  for (let corner = 0; corner < 10; corner += 1) {
    const reach = corner % 2 === 0 ? radius : radius * 0.45;
    const angle = -Math.PI / 2 + (corner * Math.PI) / 5;
    const x = cx + reach * Math.cos(angle);
    const y = cy + reach * Math.sin(angle);
    if (corner === 0) path.moveTo(x, y);
    else path.lineTo(x, y);
  }
  return path.close();
}

async function render(state: PathsBench, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x0c0c12ff });
  const field = rect(0, 0, f.width, f.height - 30);

  f.region("bench", field, {
    cursor: "pointer",
    onClick: () => {
      state.step = (state.step + 1) % STEPS.length;
      f.notify();
    },
  });
  const stars = state.stars;
  for (let index = 0; index < stars; index += 1) {
    const [cx, cy, radius] = place(field, index, stars);
    f.canvas.fill(star(cx, cy, radius), Brush.solid(INK));
  }

  await f.label(
    `${stars} star paths · ${state.segments()} of 65536 segments · ${stars + 1} of 4096 ` +
      `commands · ${state.kilobytes()} of 256 KiB · click to halve`,
    rect(8, f.height - 26, f.width - 16, 18),
    { size: 11, color: 0x8080a0ff },
  );
}

export function makeUi(): Ui<PathsBench> {
  return new Ui(new PathsBench(), render, { width: 640, height: 420 });
}

await runMain(makeUi);
