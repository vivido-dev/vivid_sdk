"""Display generated RGBA8 pixels without image libraries."""
import vivid_sdk as vivid
from _support import PIXELS, arguments, hold


def main() -> None:
    args = arguments(__doc__ or "", image=False)
    pane = vivid.PaneSession.from_env()
    try:
        pane.show_rgba(2, 2, PIXELS)
        hold(args.duration)
    finally:
        pane.close()  # Clears the presentation and closes the session.


if __name__ == "__main__":
    main()
