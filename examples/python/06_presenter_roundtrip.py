"""Verify retained pixels through a real loopback presenter; no terminal required."""
import vivid_sdk as vivid
from vivid_sdk import presenter
from _support import PIXELS


def main() -> None:
    running = presenter.start("tcp:127.0.0.1:0")
    try:
        presenter.update_metrics(running, 1, columns=80, rows=24, cell_width=8, cell_height=16)
        # Hand the capability directly to the producer; never log it or put it in argv.
        capability = presenter.issue_pane_capability(running, 1)
        pane = vivid.PaneSession.from_env(
            endpoint_control=presenter.endpoint(running), root_secret=capability,
            target_profile=vivid.PROFILE_TERMINAL_SURFACE,
        )
        try:
            pane.show_rgba(2, 2, PIXELS)
            if not presenter.wait_for_media(running, 1, 5.0):
                raise RuntimeError("timed out waiting for retained pixels")
            capture = presenter.capture_pane(running, 1)
            if len(capture.layers) != 1 or capture.skipped:
                raise RuntimeError("expected exactly one retained layer")
            frame = capture.layers[0].content
            if not isinstance(frame, presenter.RasterFrame):
                raise RuntimeError("expected raster content")
            if (frame.width, frame.height, frame.rgba) != (2, 2, PIXELS):
                raise RuntimeError("captured dimensions or RGBA pixels differ")
            print("Verified one 2 x 2 raster with exact red, green, blue, white pixels.")
        finally:
            pane.close()
    finally:
        presenter.close(running)


if __name__ == "__main__":
    main()
