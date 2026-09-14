#!/usr/bin/env python3
"""Compatibility entry point for the existing retained-poster image command.

See examples/README.md for the numbered examples and explicit cleanup behavior.
"""
from pathlib import Path
import runpy

if __name__ == "__main__":
    runpy.run_path(str(Path(__file__).parent / "python" / "vivid_image.py"), run_name="__main__")
