// Draw an interactive overlay panel and pump its input lane until it is dismissed.
//
// Unlike examples 01-05 this presents a vector display list rather than a raster: the host
// shapes the text and rasterizes the paths, so moving the window never reuploads anything.
import { OverlaySession, overlay } from "@vivido/vivid-sdk";
import { duration } from "./support.js";

const { Brush, Canvas, Path } = overlay;

const WIDTH = 320;
const HEIGHT = 180;
// Application-chosen hit region ID. Nonzero and unique within one display list; wire hit IDs
// are bigint so they survive values above 2^53.
const PANEL = 1n;

function panel(label: string): overlay.Canvas {
  const face = Path.roundedRectangle({ x: 0, y: 0, width: WIDTH, height: HEIGHT }, 12);
  return new Canvas()
    .fill(face, Brush.solid(0x203050ff))
    .stroke(face, Brush.solid(0x66ccffff), 2)
    // An empty family asks the host for its default; custom font bytes are not supported.
    .text(label, { x: 20, y: 24 }, 18, 0xffffffff)
    // Declaring a region lets the host report which part was pressed; the default hit area
    // is the whole window rectangle.
    .hit(PANEL, face);
}

async function main(): Promise<void> {
  const seconds = duration(process.argv.slice(2));
  const deadline = seconds === undefined ? undefined : Date.now() + seconds * 1000;
  await using session = await OverlaySession.fromEnv();
  await using window = await session.createWindow({
    bounds: { x: 40, y: 40, width: WIDTH, height: HEIGHT },
    mode: "floating",
  });

  await window.present(panel("Click the panel, or press Escape."));
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
    if (event.kind === "pointer" && event.applicationId === PANEL && event.down) break;
    if (event.kind === "dismissed") break;
  }
}

await main();
