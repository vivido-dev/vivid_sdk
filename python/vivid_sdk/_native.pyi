from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple, Union

class VividError(OSError): ...
class ClosedHandleError(VividError): ...

class Session:
    @property
    def closed(self) -> bool: ...

class Surface:
    @property
    def context_id(self) -> int: ...
    @property
    def id(self) -> int: ...
    @property
    def revision(self) -> int: ...
    @property
    def generation(self) -> int: ...

class Track:
    @property
    def context_id(self) -> int: ...
    @property
    def surface_id(self) -> int: ...
    @property
    def id(self) -> int: ...
    @property
    def kind(self) -> str: ...
    @property
    def revision(self) -> int: ...
    @property
    def channel_generation(self) -> int: ...

class TrackChannel:
    @property
    def context_id(self) -> int: ...
    @property
    def surface_id(self) -> int: ...
    @property
    def track_id(self) -> int: ...
    @property
    def kind(self) -> str: ...
    @property
    def generation(self) -> int: ...
    @property
    def closed(self) -> bool: ...

def connect(
    *,
    dry_run: bool = ...,
    trace_dir: Optional[Union[str, Path]] = ...,
    endpoint_control: Optional[str] = ...,
    endpoint_interactive: Optional[str] = ...,
    endpoint_realtime: Optional[str] = ...,
    endpoint_bulk: Optional[str] = ...,
    root_secret: Optional[str] = ...,
    producer_name: str = ...,
    producer_version: str = ...,
    target_profile: str = ...,
    required_profiles: Optional[Sequence[str]] = ...,
    optional_profiles: Optional[Sequence[str]] = ...,
) -> Session: ...
def close(session: Session) -> None: ...
def allocate_id(session: Session) -> int: ...
def supports(session: Session, profile: str) -> bool: ...
def session_info(session: Session) -> Dict[str, Any]: ...
def create_surface(session: Session, config: Dict[str, object]) -> Surface: ...
def update_surface(
    session: Session, surface: Surface, config: Dict[str, object]
) -> None: ...
def destroy_surface(session: Session, surface: Surface) -> None: ...
def create_track(session: Session, config: Dict[str, object]) -> Track: ...
def destroy_track(session: Session, track: Track) -> None: ...
def open_track_channel(session: Session, track: Track) -> TrackChannel: ...
def close_channel(channel: TrackChannel) -> None: ...
def send_raster(
    channel: TrackChannel,
    rgba: bytes,
    *,
    epoch: int = ...,
    frame_id: int = ...,
    compress: bool = ...,
) -> int: ...
def send_image(channel: TrackChannel, encoded: bytes) -> int: ...
def send_video(
    channel: TrackChannel,
    data: bytes,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    key: bool,
    epoch: int = ...,
) -> int: ...
def send_audio(
    channel: TrackChannel,
    data: bytes,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    epoch: int = ...,
    trim_start_samples: int = ...,
    trim_end_samples: int = ...,
) -> int: ...
def channel_eos(channel: TrackChannel) -> int: ...
def activate_track(
    session: Session,
    surface: Surface,
    track: Track,
    *,
    required_milestone: int = ...,
) -> int: ...
def wait_track(
    session: Session,
    track: Track,
    *,
    condition: int,
    value: Optional[int] = ...,
    timeout_us: int = ...,
) -> Dict[str, Any]: ...
def place_terminal_surface(
    session: Session,
    surface: Surface,
    *,
    node_id: int,
    x: int = ...,
    y: int = ...,
    width: int,
    height: int,
    text_layer: int = ...,
) -> Tuple[int, int]: ...
def anchor_marker(session: Session, context_id: int, anchor_id: int) -> str: ...
