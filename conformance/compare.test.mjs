import { test } from "node:test";
import { strict as assert } from "node:assert";
import { compareReports } from "./compare.mjs";

function valid() {
  return {
    constants: { ...Object.fromEntries(Array.from({ length: 100 }, (_, i) => [`C${i}`, i])), MIC_PACKET_US: 20000, MIC_PACKET_BYTES: 1920 },
    raster: { retained: true, layers: 1, skipped: 0, contentKind: "raster", pixels: [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255] },
    validation: { zeroRasterWidth: true, oversizedRasterWidth: true },
  };
}
const reports = (a, b) => [{ label: "rust", parsed: a }, { label: "binding", parsed: b }];
test("comparison accepts reordered keys but preserves nested values", () => {
  const reordered = valid();
  reordered.constants = Object.fromEntries(Object.entries(reordered.constants).reverse());
  compareReports(reports(valid(), reordered));
  compareReports(reports(valid(), valid()));
  const changed = valid(); changed.constants.C1 = 99;
  assert.throws(() => compareReports(reports(valid(), changed)), /differs/);
});
test("empty reports and a shared wrong microphone constant fail", () => {
  assert.throws(() => compareReports(reports({}, {})));
  const wrong = valid(); wrong.constants.MIC_PACKET_BYTES = 960;
  assert.throws(() => compareReports(reports(wrong, wrong)));
});
test("missing keys, additional keys, wrong pixels, and missing validation fail", () => {
  for (const change of [x => { delete x.constants.C1; }, x => { x.extra = true; }, x => { x.raster.pixels[0] = 0; }, x => { delete x.validation; }]) {
    const wrong = valid(); change(wrong);
    assert.throws(() => compareReports(reports(valid(), wrong)));
  }
});
