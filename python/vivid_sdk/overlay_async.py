"""Async overlay transport. Creation cancellation closes the newly created resource."""
from __future__ import annotations
import asyncio
import functools
from typing import Any, AsyncIterator, Callable, Optional, TypeVar
from . import overlay

_Owned = TypeVar("_Owned", overlay.OverlaySession, overlay.OverlayWindow)
_Result = TypeVar("_Result")

async def _run(function: Callable[..., _Result], *args: Any, **kwargs: Any) -> _Result:
    # aio re-exports these handles; defer this import so either module can be imported first.
    from .aio import _run as run
    return await run(function, *args, **kwargs)

async def _create(function: Callable[..., _Owned], *args: Any, **kwargs: Any) -> _Owned:
    future = asyncio.get_running_loop().run_in_executor(None, functools.partial(function, *args, **kwargs))
    try:
        return await asyncio.shield(future)
    except asyncio.CancelledError:
        # Repeated cancellation must not abandon native resource creation or its cleanup.
        while not future.done():
            try:
                await asyncio.shield(future)
            except asyncio.CancelledError:
                pass
        resource = future.result()
        cleanup = asyncio.get_running_loop().run_in_executor(None, resource.close)
        while not cleanup.done():
            try:
                await asyncio.shield(cleanup)
            except asyncio.CancelledError:
                pass
        cleanup.result()
        raise

class OverlaySession:
    def __init__(self, inner: overlay.OverlaySession) -> None: self._inner = inner
    @classmethod
    async def connect(cls, **options: Any) -> OverlaySession:
        return cls(await _create(overlay.OverlaySession.connect, **options))
    @classmethod
    async def from_env(cls) -> OverlaySession: return await cls.connect()
    @property
    def closed(self) -> bool: return self._inner.closed
    async def create_window(self, options: overlay.OverlayWindowOptions, *, parent: Optional[OverlayWindow] = None) -> OverlayWindow:
        return OverlayWindow(await _create(self._inner.create_window, options, parent=parent._inner if parent else None))
    async def capture_pointer(self, window: OverlayWindow, capture: bool = True) -> None:
        await _run(self._inner.capture_pointer, window._inner, capture)
    async def wait_event(self, timeout: float = 0.25) -> Optional[overlay.OverlayEvent]:
        return await _run(self._inner.wait_event, timeout)
    async def events(self, timeout: float = 0.25) -> AsyncIterator[overlay.OverlayEvent]:
        while not self.closed:
            event = await self.wait_event(timeout)
            if event is not None:
                yield event
                if isinstance(event, overlay.ConnectionLostEvent): return
    async def close(self) -> None: await _run(self._inner.close)
    async def __aenter__(self) -> OverlaySession: return self
    async def __aexit__(self, *args: Any) -> None: await self.close()

class OverlayWindow:
    def __init__(self, inner: overlay.OverlayWindow) -> None: self._inner = inner
    @property
    def closed(self) -> bool: return self._inner.closed
    @property
    def _raw(self) -> Any: return self._inner._raw
    async def present(self, canvas: overlay.Canvas) -> None:
        await _run(self._inner.present, canvas.snapshot())
    async def set_bounds(self, bounds: overlay.Rect) -> None: await _run(self._inner.set_bounds, bounds)
    async def set_visible(self, visible: bool) -> None: await _run(self._inner.set_visible, visible)
    async def center(self) -> None: await _run(self._inner.center)
    async def request_focus(self) -> None: await _run(self._inner.request_focus)
    async def raise_window(self) -> None: await _run(self._inner.raise_window)
    async def lower(self) -> None: await _run(self._inner.lower)
    async def bounds(self) -> overlay.Rect: return await _run(self._inner.bounds)
    async def viewport(self) -> overlay.Viewport: return await _run(self._inner.viewport)
    async def upload_rgba(self, width: int, height: int, rgba: bytes) -> overlay.RetainedImage:
        return await _run(self._inner.upload_rgba, width, height, rgba)
    async def draw_image(self, canvas: overlay.Canvas, image: overlay.RetainedImage, bounds: overlay.Rect, opacity: float = 1) -> None:
        await _run(self._inner.draw_image, canvas, image, bounds, opacity)
    async def close(self) -> None: await _run(self._inner.close)
    async def __aenter__(self) -> OverlayWindow: return self
    async def __aexit__(self, *args: Any) -> None: await self.close()
