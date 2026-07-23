#!/usr/bin/env python3
"""Display a PNG or JPEG in Vivido through vivid-sdk."""

import argparse

import vivid_sdk

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("image")
parser.add_argument("--scale", type=float, default=1.0)
args = parser.parse_args()
vivid_sdk.display_image(args.image, args.scale)
