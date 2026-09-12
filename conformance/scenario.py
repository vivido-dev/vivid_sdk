"""Print this SDK's canonical conformance report.

Runs the same scenarios every binding runs and prints the same report, so the comparison is
between languages rather than between expectations.
"""

from __future__ import annotations

import json
import sys

import vivid_sdk as vivid
from vivid_sdk import presenter as vp

PIXELS = bytes([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255])
PANE = 1


def constants() -> dict[str, object]:
    return {
        name: getattr(vivid, name)
        for name, text, number in vivid._native.constant_table()
    }


def raster() -> dict[str, object]:
    running = vp.start("tcp:127.0.0.1:0")
    try:
        capability = vp.issue_pane_capability(running, PANE)
        vp.update_metrics(running, PANE, columns=80, rows=24, cell_width=8, cell_height=16)
        pane = vivid.PaneSession(
            vivid.connect(
                endpoint_control=vp.endpoint(running),
                root_secret=capability,
                target_profile=vivid.PROFILE_TERMINAL_SURFACE,
            )
        )
        try:
            pane.show_rgba(2, 2, PIXELS)
            retained = vp.wait_for_media(running, PANE, 5000)
            capture = vp.capture_pane(running, PANE)
            layer = capture.layers[0] if capture.layers else None
            # The content union is a raster or an encoded still; the report names which and
            # carries the pixels so the comparison is over bytes, not over a summary of them.
            if layer is None:
                kind, pixels = "none", []
            elif isinstance(layer.content, vp.RasterFrame):
                kind, pixels = "raster", list(layer.content.rgba)
            else:
                kind, pixels = "encodedImage", []
            return {
                "retained": retained,
                "layers": len(capture.layers),
                "skipped": len(capture.skipped),
                "contentKind": kind,
                "pixels": pixels,
            }
        finally:
            pane.close()
    finally:
        vp.close(running)


def validation() -> dict[str, bool]:
    session = vivid.connect(dry_run=True)
    try:
        surface = vivid.create_surface(session, vivid.SurfaceConfig(logical_width=2, logical_height=2))
        result = {}
        for name, width in [("zeroRasterWidth", 0), ("oversizedRasterWidth", 8193)]:
            try:
                vivid.create_track(session, surface, vivid.RasterTrackConfig(width=width, height=2))
                result[name] = False
            except (ValueError, vivid.VividError):
                result[name] = True
        return result
    finally:
        vivid.close(session)


def main() -> None:
    print(json.dumps({"constants": constants(), "raster": raster(), "validation": validation()}, sort_keys=True, indent=2))


if __name__ == "__main__":
    sys.exit(main())
