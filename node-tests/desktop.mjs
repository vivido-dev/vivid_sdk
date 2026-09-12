import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const napi = require("../node-bindings/vivid_sdk_node.node");

const presenter = await napi.presenterStart({ endpoint: "tcp:127.0.0.1:0", desktopWidth: 1920, desktopHeight: 1080 });
const endpoint = await presenter.endpoint();
const cap = await presenter.issuePaneCapability(1);
await presenter.updateMetrics(1, 80, 24, 8, 16);
const s = await napi.connect({ endpointControl: endpoint, rootSecret: cap, desktop: true });
console.log("target:", s.info().targetProfile);
const surfaceCfg = { logicalWidth: 1920, logicalHeight: 1080, role: 2,
  semanticProfile: "desktop-content-v1", title: "screen",
  contextId: s.info().rootContextId,
  desktopParameters: {
    capturedOriginX: 0, capturedOriginY: 0,
    topology: [{ outputId: 1, originX: 0, originY: 0, width: 1920, height: 1080,
      scaleNumerator: 1, scaleDenominator: 1, rotation: 0, primary: true }],
    semanticGeneration: 1, inputCapabilities: 0,
  } };
const videoCfg = { kind: "video", slot: 1, codec: "h264", packetization: "h264-annexb-au-v1",
  width: 1920, height: 1080, maximumAccessUnitBytes: 8 * 1024 * 1024,
  maximumRecordBody: 10 * 1024 * 1024, maximumRateMillihertz: 60000,
  maximumEncodedBitsPerSecond: 50_000_000, maximumRecordsPerSecond: 60,
  maximumInflightBodyBytes: 20 * 1024 * 1024, retainedPixelCharge: 1920 * 1080 };
const desktop = await napi.establishDesktop(s, surfaceCfg, videoCfg);
console.log("desktop established");
const vt = await desktop.videoTrack();
console.log("video track:", vt.id, vt.kind);
const seq = await desktop.sendVideo({ data: Buffer.from("keyunit"), ptsUs: 0, key: true });
console.log("key unit seq:", seq);
await desktop.close();
await presenter.close();
console.log("NODE DESKTOP ORCHESTRATOR OK");
