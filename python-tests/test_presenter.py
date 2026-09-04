"""A Python producer and a Python presenter, over a real socket.

Every other test in this package is a dry run. These are the first that put both halves of the SDK
on a live endpoint, which is the claim Stage 2 makes: Python can now be either end of a Vivid
session, or both at once.

Rules these follow, because a leaked presenter thread hangs the whole run: every wait is bounded,
nothing sleeps on the wall clock, and every presenter is closed in a fixture teardown.
"""

from __future__ import annotations

import asyncio
from typing import Iterator, Tuple

import pytest

import vivid_sdk as vivid
from vivid_sdk import aio, presenter

# Red, green, blue, white. Four distinguishable pixels, so a capture that returned the wrong buffer
# cannot pass by being uniformly coloured.
PIXELS = bytes(
    [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]
)
TIMEOUT = 5.0


@pytest.fixture()  # type: ignore[untyped-decorator]
def running() -> Iterator[presenter.Presenter]:
    handle = presenter.start("tcp:127.0.0.1:0")
    try:
        yield handle
    finally:
        presenter.close(handle)


def producer_for(handle: presenter.Presenter, pane: int) -> vivid.PaneSession:
    presenter.update_metrics(handle, pane=pane, columns=80, rows=24)
    secret = presenter.issue_pane_capability(handle, pane)
    session = vivid.connect(
        endpoint_control=presenter.endpoint(handle), root_secret=secret
    )
    return vivid.PaneSession(session)


def test_a_python_producer_and_presenter_exchange_a_frame(
    running: presenter.Presenter,
) -> None:
    pane = producer_for(running, 1)
    pane.show_rgba(2, 2, PIXELS)

    assert presenter.wait_for_media(running, pane=1, timeout=TIMEOUT)

    capture = presenter.capture_pane(running, pane=1)
    assert capture.skipped == ()
    assert len(capture.layers) == 1

    content = capture.layers[0].content
    assert isinstance(content, presenter.RasterFrame)
    assert (content.width, content.height) == (2, 2)
    assert content.rgba == PIXELS, "the exact bytes the producer sent"


def test_an_endpoint_with_an_ephemeral_port_reports_the_one_it_got(
    running: presenter.Presenter,
) -> None:
    endpoint = presenter.endpoint(running)
    assert endpoint.startswith("tcp:127.0.0.1:")
    assert not endpoint.endswith(":0"), "the resolved port, not the request"


def test_a_pane_capability_never_appears_in_a_repr(
    running: presenter.Presenter,
) -> None:
    secret = presenter.issue_pane_capability(running, 1)
    assert len(secret) == 64, "a 32-byte root secret, hex encoded"
    assert secret not in repr(running)
    assert presenter.endpoint(running) in repr(running), "addressing is not capability material"


def test_a_summary_reports_what_a_capture_would_produce(
    running: presenter.Presenter,
) -> None:
    assert presenter.pane_media_summary(running, pane=1).tracks == ()

    pane = producer_for(running, 1)
    pane.show_rgba(2, 2, PIXELS)
    assert presenter.wait_for_media(running, pane=1, timeout=TIMEOUT)

    summary = presenter.pane_media_summary(running, pane=1)
    assert len(summary.tracks) == 1
    assert summary.tracks[0].kind == "raster"
    assert summary.tracks[0].capturable, "pixels are in hand, not merely promised"


def test_two_owners_reusing_local_ids_capture_only_their_own_pixels(
    running: presenter.Presenter,
) -> None:
    # The rule the repository states for owner-scoped work. Both producers are built by the same
    # helper, so both allocate the same local surface, track, and node numbers.
    first = producer_for(running, 1)
    second = producer_for(running, 2)

    first.show_rgba(2, 2, PIXELS)
    assert presenter.wait_for_media(running, pane=1, timeout=TIMEOUT)
    assert not presenter.wait_for_media(running, pane=2, timeout=0.2), (
        "the second owner holds nothing despite reusing the first owner's local IDs"
    )

    assert len(presenter.capture_pane(running, pane=1).layers) == 1
    assert presenter.capture_pane(running, pane=2).layers == ()

    del second


def test_a_closed_presenter_refuses_further_work() -> None:
    handle = presenter.start("tcp:127.0.0.1:0")
    presenter.close(handle)
    assert handle.closed

    with pytest.raises(vivid.ClosedHandleError):
        presenter.issue_pane_capability(handle, 1)
    with pytest.raises(vivid.ClosedHandleError):
        presenter.capture_pane(handle, pane=1)

    presenter.close(handle)  # idempotent


def test_a_negative_timeout_is_refused_rather_than_waiting_forever(
    running: presenter.Presenter,
) -> None:
    with pytest.raises(ValueError):
        presenter.wait_for_media(running, pane=1, timeout=-1.0)


def test_the_async_facade_drives_the_same_presenter() -> None:
    async def exercise() -> Tuple[int, bytes]:
        handle = await aio.presenter.start("tcp:127.0.0.1:0")
        try:
            await aio.presenter.update_metrics(handle, 1, columns=80, rows=24)
            secret = await aio.presenter.issue_pane_capability(handle, 1)
            session = await aio.connect(
                endpoint_control=aio.presenter.endpoint(handle), root_secret=secret
            )
            pane = vivid.PaneSession(session)
            pane.show_rgba(2, 2, PIXELS)

            assert await aio.presenter.wait_for_media(handle, 1, TIMEOUT)
            capture = await aio.presenter.capture_pane(handle, 1)
            content = capture.layers[0].content
            assert isinstance(content, presenter.RasterFrame)
            return len(capture.layers), content.rgba
        finally:
            await aio.presenter.close(handle)

    layers, rgba = asyncio.run(exercise())
    assert layers == 1
    assert rgba == PIXELS
