// Freeform paths: polygons, curves, an even-odd hole, dashes, and freehand drawing.
//
// TypeScript twin of `vivid_ui/examples/painting`. Everything else the toolkit paints is a
// box, a run of text, or a picture; this is the escape hatch, and in a port there is no
// hatch to reach for — the display list *is* the API, so these figures are written the same
// way the toolkit writes them internally.
//
// The ring is the one to watch: two subpaths filled by the even-odd rule, which makes the
// inner circle a hole rather than a second disc. The host hit tests the rule it fills by, so
// that hole is not clickable either.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

const { Brush, Path } = overlay;

// The circle constant, for the cubic approximation of a round shape.
const KAPPA = 0.5522847498307936;
const FIGURE = 104;

/** A circle as four cubics, appended to a path that may already have subpaths. */
function circle(path: overlay.Path, cx: number, cy: number, radius: number): overlay.Path {
  const k = radius * KAPPA;
  return path
    .moveTo(cx, cy - radius)
    .cubicTo(cx + k, cy - radius, cx + radius, cy - k, cx + radius, cy)
    .cubicTo(cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius)
    .cubicTo(cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy)
    .cubicTo(cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius)
    .close();
}

/** Ten corners alternating between two radii, starting upward. */
function star(box: overlay.Rect): overlay.Path {
  const path = new Path();
  const cx = box.x + box.width / 2;
  const cy = box.y + box.height / 2;
  for (let corner = 0; corner < 10; corner += 1) {
    const reach = box.width * (corner % 2 === 0 ? 0.44 : 0.18);
    const angle = -Math.PI / 2 + (corner * Math.PI) / 5;
    const x = cx + reach * Math.cos(angle);
    const y = cy + reach * Math.sin(angle);
    if (corner === 0) path.moveTo(x, y);
    else path.lineTo(x, y);
  }
  return path.close();
}

/** A round-capped stroke, which is what a freehand line wants at both ends. */
function roundStroke(width: number): overlay.StrokeStyle {
  return { width, cap: "round", join: "round" };
}

class Painting {
  // One stroke per press: the points the pointer visited while it was held down.
  readonly strokes: overlay.Point[][] = [];
  drawing = false;

  strokeCount(): number {
    return this.strokes.length;
  }
}

async function render(state: Painting, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  await f.label(
    `filled, stroked, even-odd, dashed — ${state.strokeCount()} strokes on the pad`,
    rect(16, 16, f.width - 32, 14),
    { size: 11, color: 0x8080a0ff, vcenter: false },
  );

  const rowY = 38;
  const wells = [0, 1, 2, 3].map((index) => rect(16 + index * (FIGURE + 10), rowY, FIGURE, FIGURE));
  for (const well of wells) f.box(well, { bg: 0x1a1a24ff, radius: 6 });

  // A filled polygon.
  f.canvas.fill(star(wells[0]!), Brush.solid(0xe0b050ff));

  // One cubic, stroked with round caps and a round join.
  let box = wells[1]!;
  f.canvas.strokeStyled(
    new Path()
      .moveTo(box.x + box.width * 0.12, box.y + box.height * 0.82)
      .cubicTo(
        box.x + box.width * 0.3,
        box.y + box.height * 0.02,
        box.x + box.width * 0.7,
        box.y + box.height * 0.98,
        box.x + box.width * 0.88,
        box.y + box.height * 0.18,
      ),
    Brush.solid(0x8ecbffff),
    roundStroke(5),
  );

  // Two nested circles, filled by the even-odd rule: a shape with a hole in it.
  box = wells[2]!;
  const cx = box.x + box.width / 2;
  const cy = box.y + box.height / 2;
  const ring = circle(new Path(true), cx, cy, box.width * 0.44);
  f.canvas.fill(circle(ring, cx, cy, box.width * 0.2), Brush.solid(0x70d090ff));

  // A dashed stroke: the dashes belong to the style, so the path is one straight line.
  box = wells[3]!;
  f.canvas.strokeStyled(
    new Path()
      .moveTo(box.x + box.width * 0.1, box.y + box.height * 0.5)
      .lineTo(box.x + box.width * 0.9, box.y + box.height * 0.5),
    Brush.solid(0xff8ea0ff),
    { width: 4, cap: "round", join: "round", dashes: [9, 6] },
  );

  // The pad: each stroke is a path through exactly the points the host reported, in the
  // same window coordinates the canvas paints in — nothing in between to get wrong.
  const pad = rect(16, rowY + FIGURE + 10, f.width - 32, f.height - rowY - FIGURE - 26);
  f.region("freehand", pad, {
    radius: 8,
    cursor: "crosshair",
    onMouseDown: (event) => {
      state.strokes.push([event.position]);
      state.drawing = true;
      f.notify();
    },
    onMouseMove: (event) => {
      if (state.drawing && state.strokes.length > 0) {
        state.strokes[state.strokes.length - 1]!.push(event.position);
        f.notify();
      }
    },
    onMouseUp: () => {
      state.drawing = false;
      f.notify();
    },
  });
  f.box(pad, { bg: 0x14141cff, radius: 8 });
  f.canvas.save();
  f.clip(pad, 8);
  for (const points of state.strokes) {
    // A press that has not moved is a dot nobody asked for: the pad reports where the
    // pointer went, and it has not gone anywhere yet.
    if (points.length < 2) continue;
    const path = new Path().moveTo(points[0]!.x, points[0]!.y);
    for (const point of points.slice(1)) path.lineTo(point.x, point.y);
    f.canvas.strokeStyled(path, Brush.solid(0xffd070ff), roundStroke(3));
  }
  f.canvas.restore();
}

export function makeUi(): Ui<Painting> {
  return new Ui(new Painting(), render, { width: 472, height: 300 });
}

await runMain(makeUi, import.meta.url);
