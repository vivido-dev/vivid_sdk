// Draw freeform paths: curves, an even-odd hole, and a stroke with caps, joins and dashes.
//
// Example 07 used the shape constructors - rectangles, rounded rectangles, ellipses - and those
// cover a user interface. Everything else is a path you build: a chart, a map, a signature, a
// gauge. The four segment kinds compose into all of it, and the even-odd rule is what puts a hole
// in a shape rather than a second layer over it.
//
// The same builder exists in all three languages, with the same segment kinds and the same
// options, so the geometry below is written the same way in each.
import { OverlaySession, overlay } from "@vivido/vivid-sdk";
import { duration } from "./support.js";

const { Brush, Canvas, Path } = overlay;

const WIDTH = 400;
const HEIGHT = 220;
// Application-chosen hit region ID. Nonzero and unique within one display list; wire hit IDs
// are bigint so they survive values above 2^53.
const CANVAS = 1n;
// The circle constant, for the cubic approximation of a round shape.
const KAPPA = 0.5522847498307936;

// A circle as four cubics, appended to a path that may already have subpaths.
//
// A ring needs two of these in one builder, and a finished Path cannot be extended - which is
// what a builder is for. Path.ellipse does the same arithmetic for a whole ellipse.
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

// A star, filled: ten corners alternating between two radii, walked with lineTo.
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

function scene(label: string): overlay.Canvas {
  const face = Path.roundedRectangle({ x: 0, y: 0, width: WIDTH, height: HEIGHT }, 10);
  // A filled star: ten corners, alternating radii, one lineTo each.
  const badge = star(58, 72, 34);
  // One cubic through two control points, stroked with round caps and a round join. The caps are
  // why the ends are not cut off square.
  const curve = new Path().moveTo(108, 96).cubicTo(150, 20, 220, 130, 262, 46);
  // A ring: two circles in one path, with the even-odd rule. The inner one is a hole rather than
  // a second disc, so the background shows through it - and the host hit tests the rule it fills
  // by, so the hole is not part of the region either.
  const ring = circle(circle(new Path(true), 316, 72, 34), 316, 72, 16);
  // A dashed line: the dashes belong to the stroke, not to the path, so the path is two points.
  const rule = new Path().moveTo(24, 158).lineTo(WIDTH - 24, 158);
  return new Canvas()
    .fill(face, Brush.solid(0x181828ff))
    .fill(badge, Brush.solid(0xe0b050ff))
    .strokeStyled(curve, Brush.solid(0x8ecbffff), { width: 5, cap: "round", join: "round" })
    .fill(ring, Brush.solid(0x70d090ff))
    .strokeStyled(rule, Brush.solid(0xff8ea0ff), { width: 3, dashes: [10, 6] })
    // An empty family asks the host for its default; custom font bytes are not supported.
    .text(label, { x: 24, y: 180 }, 15, 0xc0c0d0ff)
    // Declaring a region lets the host report which part was pressed; the default hit area is
    // the whole window rectangle.
    .hit(CANVAS, face);
}

async function main(): Promise<void> {
  const seconds = duration(process.argv.slice(2));
  const deadline = seconds === undefined ? undefined : Date.now() + seconds * 1000;
  await using session = await OverlaySession.fromEnv();
  await using window = await session.createWindow({
    bounds: { x: 40, y: 40, width: WIDTH, height: HEIGHT },
    mode: "floating",
  });

  await window.present(scene("star, curve, ring, dashes - click or press Escape"));
  await window.center();
  await window.requestFocus();

  while (deadline === undefined || Date.now() < deadline) {
    const event = await session.waitEvent(0.2);
    if (event === undefined) continue;
    if (event.kind === "connection-lost") {
      console.error(`overlay connection lost: ${event.diagnostic}`);
      break;
    }
    // One session can own many windows, so every event names the one it belongs to.
    if (!event.targets(window)) continue;
    if (event.kind === "pointer" && event.applicationId === CANVAS && event.down) break;
    if (event.kind === "dismissed") break;
  }
}

await main();
