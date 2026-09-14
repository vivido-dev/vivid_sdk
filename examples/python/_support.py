"""Small CLI/fixture helpers; SDK lifecycle stays in each example."""
import argparse
import math
import time
from typing import Optional

PIXELS = bytes([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255])


def seconds(value: str) -> float:
    result = float(value)
    if not math.isfinite(result) or not 0 <= result <= 3600:
        raise argparse.ArgumentTypeError("duration must be 0..3600 seconds")
    return result


def arguments(description: str, *, image: bool = False) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=description)
    if image:
        parser.add_argument("image")
    parser.add_argument("--duration", type=seconds, help="remove after this many seconds instead of Enter")
    return parser.parse_args()


def hold(duration: Optional[float]) -> None:
    if duration is None:
        input("Press Enter to remove the presentation.")
    else:
        time.sleep(duration)
