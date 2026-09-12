// Compare language reports and assert independent fixture expectations.

import { spawnSync } from "node:child_process";
import { strict as assert } from "node:assert";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));

/** Run one producer command and return its parsed report. */
function report(label, command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    env: process.env,
    timeout: 120_000,
  });
  if (result.status !== 0) {
    process.stderr.write(`${label} failed (${result.status}): ${result.error?.message ?? ""}\n${result.stderr}\n`);
    process.exit(1);
  }
  const parsed = JSON.parse(result.stdout.slice(result.stdout.indexOf("{")));
  return { label, parsed };
}

/** Agreement alone cannot detect a bug shared by all three implementations. */
export function validateReport(report) {
  assert.equal(report.raster.retained, true);
  assert.equal(report.raster.layers, 1);
  assert.equal(report.raster.skipped, 0);
  assert.equal(report.raster.contentKind, "raster");
  assert.deepEqual(report.raster.pixels, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
  assert.ok(Object.keys(report.constants).length > 90, "constant table is incomplete");
  // 48 kHz * 20 ms * one channel * two bytes per sample (audio-input-v1).
  assert.equal(report.constants.MIC_PACKET_US, 20_000);
  assert.equal(report.constants.MIC_PACKET_BYTES, 1920);
  assert.deepEqual(report.validation, { zeroRasterWidth: true, oversizedRasterWidth: true });
}

export function compareReports(reports) {
  assert.ok(reports.length >= 2, "need at least two language reports");
  for (const report of reports) validateReport(report.parsed);
  for (const other of reports.slice(1)) {
    assert.deepEqual(other.parsed, reports[0].parsed,
      `${other.label} differs from ${reports[0].label}`);
  }
}

function main() {
  const reports = [
    report("rust", "cargo", ["run", "--quiet", "--example", "conformance", "--features", "presenter"]),
    report("python", process.env.VIVID_CONFORMANCE_PYTHON ?? "uv",
      process.env.VIVID_CONFORMANCE_PYTHON ? ["conformance/scenario.py"] : ["run", "python", "conformance/scenario.py"]),
    report("typescript", process.execPath, ["conformance/scenario.mjs"]),
  ];
  compareReports(reports);
  for (const other of reports.slice(1)) console.log(`ok    ${other.label} matches rust`);
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) main();
