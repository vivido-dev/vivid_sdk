// Smoke test: presenter socket + producer flow through the napi addon.
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const napi = require("../node-bindings/vivid_sdk_node.node");

const presenter = await napi.presenterStart({ endpoint: "tcp:127.0.0.1:0" });
const endpoint = presenter.endpoint();
const cap = presenter.issuePaneCapability(1);
await presenter.updateMetrics(1, 80, 24, 8, 16);
const session = await napi.connect({ endpointControl: endpoint, rootSecret: cap, targetProfile: "terminal-surface-v1" });
console.log("producer connected");

const surface = await session.createSurface({ logicalWidth: 64, logicalHeight: 32, role: 4 });
// The PaneSession ordering: place, then create the track, then send, then wait for the
// presenter's output-ready milestone, then activate with that milestone required.
console.log("place:", JSON.stringify(await session.placeTerminalSurface(surface, 1, 0, 0, 2 << 32, 1 << 32, 1)));
const w = 64, h = 32, body = 72 + w * h * 4;
const track = await session.createTrack(surface, {
  kind: "raster", width: w, height: h, slot: 3,
  maximumRecordBody: body, maximumRateMillihertz: 60000,
  maximumEncodedBitsPerSecond: body * 8 * 60,
  maximumRecordsPerSecond: 60, maximumInflightBodyBytes: 2 * body,
  retainedPixelCharge: w * h,
});
const channel = await session.openTrackChannel(track);
const rgba = Buffer.alloc(w * h * 4);
for (let i = 0; i < w * h; i += 1) rgba[i * 4] = 200;
console.log("send:", await channel.sendRaster(rgba, 0, 1, false));
try {
  console.log("wait OUTPUT_READY:", JSON.stringify(await session.waitTrack(track, 2, 1 << 4, 30000)));
} catch (e) { console.log("wait OUTPUT_READY FAILED:", e.message); }
await session.activateTrack(surface, track, 1 << 4);
// MILESTONE_PRESENTED only arrives through an outer gateway; the in-process presenter proves
// presentation by retaining and compositing, which the capture below observes.
console.log("wait_for_media:", await presenter.waitForMedia(1, 5000));
const media = await presenter.paneMediaSummary(1);
console.log("summary tracks:", media.tracks.length, "kind:", media.tracks[0]?.kind);
const capture = await presenter.capturePane(1, 0);
console.log("capture layers:", capture.layers.length, "skipped:", capture.skipped.length);
const layer = capture.layers[0];
console.log("content:", layer.contentKind, layer.rasterWidth, "x", layer.rasterHeight,
  "bytes", layer.pixels?.length, "red:", layer.pixels?.[0]);

await channel.eos();
await session.close();
await presenter.close();
console.log("END-TO-END OK");
