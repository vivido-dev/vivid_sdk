// Show a PNG/JPEG using the native pane API.
import { readFile } from "node:fs/promises";
import { PaneSession } from "@vivido/vivid-sdk";
import { duration, hold } from "./support.js";

const path = process.argv[2];
if (!path || path.startsWith("--")) throw new Error("usage: 01_show_image IMAGE [--duration SECONDS]");
const seconds = duration(process.argv.slice(3));
const encoded = await readFile(path); // Fail for missing files before opening a connection.
const pane = await PaneSession.connect();
try {
  await pane.showEncodedImage(encoded);
  await hold(seconds);
} finally {
  await pane.close(); // Clears the presentation and closes its session.
}
