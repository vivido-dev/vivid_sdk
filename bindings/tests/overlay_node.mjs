import { strict as assert } from "node:assert";
import { createInterface } from "node:readline";
import { OverlaySession, overlay } from "../../dist/index.js";
const { Canvas, Brush, Path } = overlay;
const bounds = { x: 10, y: 20, width: 100, height: 80 };
const input = createInterface({ input: process.stdin });
const release = new Promise(resolve => input.once("line", resolve));
const session = await OverlaySession.fromEnv();
try {
  const window = await session.createWindow({ bounds });
  await window.center(); await window.setBounds(bounds);
  await window.setVisible(false); await window.setVisible(true);
  await window.raise(); await window.lower(); await window.requestFocus();
  assert.deepEqual(await window.bounds(), bounds);
  assert.equal((await window.viewport()).width, 400);
  const canvas = new Canvas();
  for (const [x, y, color] of [[0, 0, 0xff0000ff], [10, 0, 0x00ff00ff], [0, 10, 0x0000ffff], [10, 10, 0xffff00ff]]) {
    canvas.fill(Path.rectangle({ x, y, width: 10, height: 10 }), Brush.solid(color));
  }
  canvas.hit((1n << 63n) + 17n, Path.rectangle({ x: 0, y: 0, width: 100, height: 80 }));
  const image = await window.uploadRgba(2, 2, Uint8Array.of(255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255));
  await window.drawImage(canvas, image, { x: 50, y: 0, width: 20, height: 20 });
  await window.present(canvas);
  const deadline = Date.now() + 10000, seen = new Set();
  for await (const event of session.events()) {
    if (event.kind === "pointer" || event.kind === "ime") {
      assert(event.targets(window)); assert.equal(event.sceneRevision, 1n);
    }
    if (event.kind === "pointer") { assert.equal(event.applicationId, (1n << 63n) + 17n); seen.add("pointer"); }
    if (event.kind === "ime") { assert.equal(event.preedit, "A😀日"); assert.deepEqual(event.selection, [1, 3]); seen.add("ime"); }
    assert(Date.now() < deadline);
    if (seen.size === 2) break;
  }
  await session.capturePointer(window); await session.capturePointer(window, false);
  assert.equal(await release, "release");
  const popup = await session.createWindow({ bounds: { x: 40, y: 40, width: 20, height: 20 }, mode: "popup" }, window);
  await popup.present(new Canvas()); await popup.close();
  await window.present(canvas); await window.close();
} finally { await session.close(); input.close(); }
