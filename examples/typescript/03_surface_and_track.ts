// surface and track: explicit Vivid 1.5 lifecycle.
import { connect, ROLE_FIGURE, WAIT_MILESTONE_SET, MILESTONE_OUTPUT_READY } from "@vivido/vivid-sdk";
import { PIXELS, duration, hold } from "./support.js";

const seconds = duration(process.argv.slice(2));
const session = await connect();
try {
  const surface = await session.createSurface({
    logicalWidth: 2, logicalHeight: 2, role: ROLE_FIGURE, title: "SDK raster example",
  });
  const nodeId = await session.allocateId();
  try {
    // TypeScript takes ordinary cell numbers; the binding converts to 32.32 fixed point.
    await session.placeTerminalSurface(surface, { nodeId, width: 16, height: 8 });
    const track = await session.createTrack(surface, { kind: "raster", width: 2, height: 2 });
    const channel = await session.openTrackChannel(track);
    try {
      await channel.sendRaster(PIXELS, { frameId: 1 });
      await session.waitTrack(track, WAIT_MILESTONE_SET, MILESTONE_OUTPUT_READY, 5000);
      await session.activateTrack(surface, track, MILESTONE_OUTPUT_READY);
      await channel.eos();
      await hold(seconds);
    } finally {
      await channel.close();
    }
  } finally {
    await session.deleteNode(surface.contextId, nodeId);
    await session.destroySurface(surface);
  }
} finally {
  await session.close();
}
