from vivid_sdk import OverlaySession, OverlayWindowOptions
from vivid_sdk.overlay import Brush, Canvas, Path, Rect

with OverlaySession.from_env() as session:
    with session.create_window(OverlayWindowOptions(Rect(40, 40, 320, 180))) as window:
        canvas = Canvas().fill(
            Path.rounded_rectangle(Rect(0, 0, 320, 180), 12),
            Brush.solid(0x203050FF),
        )
        window.present(canvas)
        window.center()
        input("Press Enter to close")
