#!/usr/bin/env python3
"""Display a PNG or JPEG through Vivid 1.5, then exit leaving it on screen."""

import argparse

import vivid_sdk

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("image")
args = parser.parse_args()
# The image is anchored to the cursor cell it is written at, and the text cursor is left below
# it, so the shell prompt that returns after this exits lands under the image, not behind it.
presentation = vivid_sdk.display_image(args.image)
try:
    # Wait until the presenter has actually presented the frame: the poster it retains across
    # the clean GOODBYE below is a copy of the last presented output, so one has to exist. A
    # presenter with no downstream presentation path may never report the milestone, so bound
    # the wait instead.
    vivid_sdk.wait_track(
        presentation.session,
        presentation.track,
        condition=vivid_sdk.WAIT_MILESTONE_SET,
        value=vivid_sdk.MILESTONE_PRESENTED,
        timeout_us=5_000_000,
    )
except vivid_sdk.VividError:
    pass
# A clean GOODBYE hands the anchored image to the presenter, which keeps it as a retained
# poster. `presentation.close()` would instead destroy the scene, removing the image.
vivid_sdk.close(presentation.session)
