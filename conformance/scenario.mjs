// Print this SDK's canonical conformance report.
//
// Runs the same scenarios every binding runs and prints the same report, so the comparison is
// between languages rather than between expectations.

import { PaneSession, connect, constantTable, presenter } from "../dist/index.js";

const PIXELS = Buffer.from([200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255]);
const PANE = 1;

function constants() {
  const values = {};
  for (const entry of constantTable()) {
    values[entry.name] = entry.text ?? entry.number;
  }
  return values;
}

async function raster() {
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    const capability = running.issuePaneCapability(PANE);
    running.updateMetrics(PANE, 80, 24, 8, 16);
    const pane = await PaneSession.connect({
      endpointControl: running.endpoint(),
      rootSecret: capability,
      targetProfile: "terminal-surface-v1",
    });
    try {
      await pane.showRgba(2, 2, PIXELS);
      const retained = await running.waitForMedia(PANE, 5000);
      const capture = await running.capturePane(PANE, 0);
      const layer = capture.layers[0];
      return {
        retained,
        layers: capture.layers.length,
        skipped: capture.skipped.length,
        contentKind: layer?.contentKind ?? "none",
        pixels: layer?.pixels ? [...layer.pixels] : [],
      };
    } finally {
      await pane.close();
    }
  } finally {
    await running.close();
  }
}

const report = { constants: constants(), raster: await raster() };
// Plain stringify: a key allowlist here would apply at every nesting level and serialize the
// constant table as `{}`, which compares equal to anything.
process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
