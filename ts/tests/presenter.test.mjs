import { strict as assert } from "node:assert";
import { test } from "node:test";

import { PaneSession, connect, presenter } from "../../dist/index.js";

const PIXELS = Buffer.from([200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255]);

test("a TypeScript producer reaches a TypeScript presenter and its pixels come back", async () => {
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    const capability = running.issuePaneCapability(1);
    running.updateMetrics(1, 80, 24, 8, 16);

    const pane = await PaneSession.connect({
      endpointControl: running.endpoint(),
      rootSecret: capability,
      targetProfile: "terminal-surface-v1",
    });
    try {
      await pane.showRgba(2, 2, PIXELS);
      assert.equal(await running.waitForMedia(1, 5000), true, "the presenter retained the frame");

      const capture = await running.capturePane(1, 0);
      assert.equal(capture.layers.length, 1);
      const layer = capture.layers[0];
      assert.equal(layer.contentKind, "raster");
      assert.equal(layer.rasterWidth, 2);
      assert.equal(layer.rasterHeight, 2);
      assert.deepEqual([...layer.pixels], [...PIXELS], "the exact pixels sent");

      const summary = await running.paneMediaSummary(1);
      assert.equal(summary.tracks.length, 1);
      assert.equal(summary.tracks[0].capturable, true);
    } finally {
      await pane.close();
    }
  } finally {
    await running.close();
  }
});

test("two owners reusing local ids capture only their own pixels", async () => {
  // The rule the repository states for owner-scoped work: both producers below allocate the same
  // local surface, track, and node numbers, and neither may see the other's media.
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    running.updateMetrics(1, 80, 24, 8, 16);
    running.updateMetrics(2, 80, 24, 8, 16);
    const firstSecret = running.issuePaneCapability(1);
    const secondSecret = running.issuePaneCapability(2);

    const first = await PaneSession.connect({
      endpointControl: running.endpoint(),
      rootSecret: firstSecret,
      targetProfile: "terminal-surface-v1",
    });
    const second = await PaneSession.connect({
      endpointControl: running.endpoint(),
      rootSecret: secondSecret,
      targetProfile: "terminal-surface-v1",
    });
    try {
      await first.showRgba(2, 2, PIXELS);

      assert.equal(await running.waitForMedia(1, 5000), true);
      assert.equal(
        await running.waitForMedia(2, 200),
        false,
        "the second owner holds nothing despite reusing the first owner's local ids",
      );
      assert.equal((await running.capturePane(1, 0)).layers.length, 1);
      assert.equal((await running.capturePane(2, 0)).layers.length, 0);
    } finally {
      await first.close();
      await second.close();
    }
  } finally {
    await running.close();
  }
});

test("media resources name content without letting ids cross owners", async () => {
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    const capability = running.issuePaneCapability(1);
    running.updateMetrics(1, 80, 24, 8, 16);
    const pane = await PaneSession.connect({
      endpointControl: running.endpoint(),
      rootSecret: capability,
      targetProfile: "terminal-surface-v1",
    });
    try {
      await pane.showRgba(2, 2, PIXELS);
      await running.waitForMedia(1, 5000);
      const source = (await running.capturePane(1, 0)).layers[0].source;

      const pinned = running.announceMediaResource(source, true);
      const description = running.describeMediaResource(pinned);
      assert.equal(description.bindingPinned, true);
      assert.equal(description.capturable, true);
      assert.equal(running.releaseMediaResource(pinned), true);
      assert.throws(() => running.describeMediaResource(pinned));
    } finally {
      await pane.close();
    }
  } finally {
    await running.close();
  }
});

test("presenter identities and bounded geometry reject invalid numbers", async () => {
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    for (const producer of [-1, 0.5, NaN, 2 ** 53]) {
      assert.throws(() => running.paneForSource({ producer, context: 1, surface: 1, track: 1 }), /safe integer/);
    }
    assert.throws(() => running.updateMetrics(1, 2 ** 16 + 80, 24, 8, 16), /out of range/);
    running.updateMetrics(1, 80, 24, 8, 16);
    assert.throws(() => running.requestKeyframe({ producer: 1, context: 1, surface: 1, track: 1 }, 1, 2 ** 32), /out of range/);
  } finally { await running.close(); }
});
