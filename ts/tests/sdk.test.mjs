import { strict as assert } from "node:assert";
import { test } from "node:test";

import {
  ClosedHandleError,
  FIT_CONTAIN,
  MILESTONE_OUTPUT_READY,
  PROFILE_CORE,
  ROLE_FIGURE,
  SLOT_RASTER,
  VividError,
  connect,
  constantNames,
  constantTable,
  probeEncodedImage,
} from "../../dist/index.js";

const PNG = Buffer.from(
  "89504e470d0a1a0a0000000d4948445200000080000000400806000000",
  "hex",
);

test("constants come from the SDK table", () => {
  // A value here and in Rust is one value; a copy here and in Rust is two.
  assert.equal(PROFILE_CORE, "vivid-core-control-v1");
  assert.equal(MILESTONE_OUTPUT_READY, 16);
  assert.equal(FIT_CONTAIN, 2);
  assert.equal(SLOT_RASTER, 3);
  assert.ok(constantTable().length > 90);
});

test("every table name is reachable from the package", async () => {
  const published = await import("../../dist/index.js");
  // The table is the SDK's; a name it carries that this package does not re-export is a
  // constant TypeScript callers cannot use, which is the drift this guards against.
  const missing = constantNames().filter(
    (name) => !(name in published) && name !== "MAX_TRACK_WAIT_TIMEOUT_US",
  );
  assert.deepEqual(missing, []);
});

test("image probing reads header metadata and rejects missing headers", () => {
  const info = probeEncodedImage(PNG);
  assert.equal(info.encoding, 1);
  assert.equal(info.width, 128);
  assert.equal(info.height, 64);
  assert.equal(info.encodedLength, PNG.length);
  assert.throws(() => probeEncodedImage(PNG.subarray(0, 12)), VividError);
  assert.throws(() => probeEncodedImage(Buffer.from("GIF89a")), VividError);
});

test("an offline session drives surfaces, tracks, and channels", async () => {
  const session = await connect({ offline: true });
  try {
    assert.equal(session.closed, false);
    const surface = await session.createSurface({
      logicalWidth: 2,
      logicalHeight: 2,
      role: ROLE_FIGURE,
    });
    const track = await session.createTrack(surface, {
      kind: "raster",
      width: 2,
      height: 2,
    });
    assert.equal(track.kind, "raster");
    const channel = await session.openTrackChannel(track);
    const sequence = await channel.sendRaster(Buffer.alloc(2 * 2 * 4), {
      epoch: 0,
      frameId: 1,
    });
    assert.ok(sequence > 0);
    assert.equal(channel.mediaCreditAvailable(16), true);
    const pressure = channel.takeSendPressure();
    assert.equal(pressure.records, 1);
    assert.ok(await channel.eos());
    await channel.close();
    assert.equal(channel.closed, true);
  } finally {
    await session.close();
  }
});

test("a closed handle is a distinct error class", async () => {
  const session = await connect({ offline: true });
  await session.close();
  let failure;
  try {
    session.info();
    assert.fail("a closed session must refuse a handle read");
  } catch (error) {
    failure = error;
  }
  assert.ok(failure instanceof ClosedHandleError);
  assert.ok(failure instanceof VividError);
  assert.equal(failure.closed, true);
  assert.equal(failure.message, "session is closed");
});

test("waitTrack reports a satisfied condition", async () => {
  const session = await connect({ offline: true });
  try {
    const surface = await session.createSurface({ logicalWidth: 2, logicalHeight: 2 });
    const track = await session.createTrack(surface, {
      kind: "raster",
      width: 2,
      height: 2,
    });
    const waited = await session.waitTrack(track, 7, undefined, 1000); // ChannelAccepted
    assert.equal(waited.trackId, track.id);
    assert.equal(waited.condition, 7);
  } finally {
    await session.close();
  }
});

test("events iterate and end when the session closes", async () => {
  const session = await connect({ offline: true });
  const seen = [];
  const pump = (async () => {
    for await (const event of session.events({ timeoutMs: 50 })) {
      seen.push(event.kind);
    }
  })();
  await session.close();
  await pump;
  assert.ok(Array.isArray(seen));
});

test("a channel stamps its own frame ids", async () => {
  // The SDK requires media IDs to be nonzero and strictly increasing, so a wrapper that made
  // every call without an explicit id send frame 1 would work once and then fail confusingly.
  const session = await connect({ offline: true });
  try {
    const surface = await session.createSurface({ logicalWidth: 2, logicalHeight: 2 });
    const track = await session.createTrack(surface, { kind: "raster", width: 2, height: 2 });
    const channel = await session.openTrackChannel(track);
    const frame = Buffer.alloc(2 * 2 * 4);
    const first = await channel.sendRaster(frame);
    const second = await channel.sendRaster(frame);
    const third = await channel.sendRasterAdaptive(frame);
    assert.ok(second > first, `${second} must follow ${first}`);
    assert.ok(third > second, `${third} must follow ${second}`);
    // An explicit id still wins, and the sequence continues past it.
    const explicit = await channel.sendRaster(frame, { frameId: 100 });
    assert.ok(explicit > third);
  } finally {
    await session.close();
  }
});

test("invalid numbers are rejected before IDs or geometry can be changed", async () => {
  const session = await connect({ offline: true });
  try {
    for (const contextId of [-1, -0.5, NaN, Infinity, 2 ** 53]) {
      await assert.rejects(session.createSurface({ logicalWidth: 2, logicalHeight: 2, contextId }), VividError);
    }
    await assert.rejects(session.createSurface({ logicalWidth: 2, logicalHeight: 2, scaleDenominator: 0 }), VividError);
    const surface = await session.createSurface({ logicalWidth: 2, logicalHeight: 2 });
    for (const config of [
      { width: 2 ** 32 + 2 },
      { maximumDeltaOperations: 257 },
      { maximumRateMillihertz: NaN },
      { maximumEncodedBitsPerSecond: -1 },
    ]) {
      await assert.rejects(session.createTrack(surface, { kind: "raster", width: 2, height: 2, ...config }), VividError);
    }
    await assert.rejects(session.createTrack(surface, { kind: "audio", sampleRate: 48000, channels: 257 }), VividError);
    await assert.rejects(session.waitEvent(Number.MAX_VALUE), VividError);
    const track = await session.createTrack(surface, { kind: "raster", width: 2, height: 2 });
    const channel = await session.openTrackChannel(track);
    try {
      await assert.rejects(channel.sendRaster(Buffer.alloc(16), { epoch: 2 ** 32 }), VividError);
      await channel.sendRaster(Buffer.alloc(16), { frameId: 1 });
    } finally { await channel.close(); }
  } finally { await session.close(); }
});

test("submodule constants agree with the root and the microphone wire shape", async () => {
  const sdk = await import("../../dist/index.js");
  assert.equal(sdk.pipeline.MIC_PACKET_BYTES, 1920);
  for (const module of [sdk.pipeline, sdk.lease, sdk.fileDrop]) {
    for (const [name, value] of Object.entries(module)) {
      if (/^[A-Z_]+$/.test(name) && name in sdk) assert.equal(value, sdk[name], name);
    }
  }
});
