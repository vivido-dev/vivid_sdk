// A real loopback transport and exact retained-pixel assertions, without a terminal.
import assert from "node:assert/strict";
import { PaneSession, PROFILE_TERMINAL_SURFACE, presenter } from "@vivido/vivid-sdk";
import { PIXELS } from "./support.js";

const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
try {
  running.updateMetrics(1, 80, 24, 8, 16);
  // Pass capability material in memory, never through argv or diagnostics.
  const capability = running.issuePaneCapability(1);
  const pane = await PaneSession.connect({
    endpointControl: running.endpoint(), rootSecret: capability,
    targetProfile: PROFILE_TERMINAL_SURFACE,
  });
  try {
    await pane.showRgba(2, 2, PIXELS);
    assert.ok(await running.waitForMedia(1, 5000), "timed out waiting for retained pixels");
    const capture = await running.capturePane(1);
    assert.equal(capture.layers.length, 1);
    assert.equal(capture.skipped.length, 0);
    const layer = capture.layers[0];
    assert.ok(layer);
    assert.equal(layer.contentKind, "raster");
    assert.equal(layer.rasterWidth, 2);
    assert.equal(layer.rasterHeight, 2);
    assert.ok(layer.pixels);
    assert.deepEqual([...layer.pixels], [...PIXELS]);
    console.log("Verified one 2 x 2 raster with exact red, green, blue, white pixels.");
  } finally {
    await pane.close();
  }
} finally {
  await running.close();
}
