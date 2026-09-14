"""Display a PNG/JPEG through PaneSession."""
from pathlib import Path
import vivid_sdk as vivid
from _support import arguments, hold


def main() -> None:
    args = arguments(__doc__ or "", image=True)
    encoded = Path(args.image).read_bytes()  # Fail before connecting for a missing file.
    pane = vivid.PaneSession.from_env()
    try:
        pane.show_encoded_image(encoded)
        hold(args.duration)
    finally:
        pane.close()  # Clears the presentation and closes the session.


if __name__ == "__main__":
    main()
