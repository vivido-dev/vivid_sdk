"""replace track: explicit Vivid 1.5 lifecycle."""
import time
import vivid_sdk as vivid
from _support import PIXELS, arguments, hold


def main() -> None:
    args = arguments(__doc__ or "")
    session = vivid.connect()
    try:
        surface = vivid.create_surface(
            session, vivid.SurfaceConfig(logical_width=2, logical_height=2,
                                         role=vivid.ROLE_FIGURE, title="SDK raster example"),
        )
        try:
            # Python geometry uses signed 32.32 cells, at the top-left of the terminal.
            vivid.place_terminal_surface(session, surface, node_id=1, width=16 << 32, height=8 << 32)
            track = vivid.create_track(session, surface, vivid.RasterTrackConfig(width=2, height=2))
            channel = vivid.open_track_channel(session, track)
            try:
                vivid.send_raster(channel, PIXELS, frame_id=1)
                vivid.wait_track(
                    session, track, condition=vivid.WAIT_MILESTONE_SET,
                    value=vivid.MILESTONE_OUTPUT_READY, timeout_us=5_000_000,
                )
                vivid.activate_track(session, surface, track)
                time.sleep(1)
                # The surface stays 2 x 2 logically; only the encoded raster becomes 4 x 4.
                replacement = vivid.create_track(
                    session, surface, vivid.RasterTrackConfig(width=4, height=4)
                )
                next_channel = vivid.open_track_channel(session, replacement)
                try:
                    vivid.send_raster(next_channel, bytes([255, 180, 0, 255]) * 16)
                    vivid.wait_track(
                        session, replacement, condition=vivid.WAIT_MILESTONE_SET,
                        value=vivid.MILESTONE_OUTPUT_READY, timeout_us=5_000_000,
                    )
                    vivid.activate_track(session, surface, replacement)
                    # Retire the old track only after the replacement occupies its slot.
                    vivid.channel_eos(channel)
                    vivid.close_channel(channel)
                    vivid.destroy_track(session, track)
                    vivid.channel_eos(next_channel)
                    hold(args.duration)
                finally:
                    vivid.close_channel(next_channel)
            finally:
                if not channel.closed:
                    vivid.close_channel(channel)
        finally:
            # Destroying the surface also retires its tracks. Close still runs on failure.
            vivid.delete_node(session, surface.context_id, 1)
            vivid.destroy_surface(session, surface)
    finally:
        vivid.close(session)


if __name__ == "__main__":
    main()
