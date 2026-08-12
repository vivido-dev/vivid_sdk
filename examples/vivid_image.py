#!/usr/bin/env python3
"""Display a PNG or JPEG through Vivid 1.5 until Enter is pressed."""

import argparse

import vivid_sdk

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("image")
args = parser.parse_args()
presentation = vivid_sdk.display_image(args.image)
try:
    input("press Enter to remove the image")
finally:
    presentation.close()
