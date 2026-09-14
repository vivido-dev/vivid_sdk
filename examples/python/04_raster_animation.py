"""raster animation: explicit Vivid 1.5 lifecycle."""
import asyncio
import vivid_sdk as vivid
from vivid_sdk import aio
from _support import PIXELS


async def main() -> None:
    session = await aio.connect()
    try:
        surface = await aio.create_surface(
            session, vivid.SurfaceConfig(logical_width=2, logical_height=2,
                                         role=vivid.ROLE_FIGURE, title="SDK raster example"),
        )
        try:
            # Python geometry uses signed 32.32 cells, at the top-left of the terminal.
            await aio.place_terminal_surface(session, surface, node_id=1, width=16 << 32, height=8 << 32)
            track = await aio.create_track(session, surface, vivid.RasterTrackConfig(width=2, height=2))
            channel = await aio.open_track_channel(session, track)
            try:
                await aio.send_raster(channel, PIXELS, frame_id=1)
                await aio.wait_track(
                    session, track, condition=vivid.WAIT_MILESTONE_SET,
                    value=vivid.MILESTONE_OUTPUT_READY, timeout_us=5_000_000,
                )
                await aio.activate_track(session, surface, track)
                # A single awaited sender bounds work even when flow control blocks.
                for frame_id in range(2, 91):
                    await asyncio.sleep(0.034)
                    offset = (frame_id % 4) * 4
                    frame = PIXELS[offset:] + PIXELS[:offset]
                    await aio.send_raster(channel, frame, frame_id=frame_id)
                await aio.channel_eos(channel)
            finally:
                await aio.close_channel(channel)
        finally:
            # Destroying the surface also retires its tracks. Close still runs on failure.
            await asyncio.to_thread(vivid.delete_node, session, surface.context_id, 1)
            await aio.destroy_surface(session, surface)
    finally:
        await aio.close(session)


if __name__ == "__main__":
    asyncio.run(main())
