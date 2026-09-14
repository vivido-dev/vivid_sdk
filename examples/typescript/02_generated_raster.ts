// Show a generated four-color raster without image libraries.
import { PaneSession } from "@vivido/vivid-sdk";
import { PIXELS, duration, hold } from "./support.js";

const seconds = duration(process.argv.slice(2));
const pane = await PaneSession.connect();
try {
  await pane.showRgba(2, 2, PIXELS);
  await hold(seconds);
} finally {
  await pane.close(); // Clears the presentation and closes its session.
}
