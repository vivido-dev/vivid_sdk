"""Drive the terminal runtimes over their automation endpoints.

Three runtimes take automation requests: `vivido` controls terminal windows (keys, mouse, grid
reads, sequence-based waits, Vivid presenter inspection), `vivida` adds workspace layout, and
`vvmux` adds panes, tabs and agents. Until now the only client in any language was each CLI, so
every script and agent shelled out and parsed printed JSON.

Two wire protocols, one module:

* **vivido / vivida** — newline-delimited JSON over an owner-checked local socket. A `hello`
  handshake answers with the capability document naming what this instance claims; everything
  else is `{version, id, method, params}` in, `{version, id, ok, result | error}` out.
* **vvmux** — VVMX: a 12-byte preface, then sequence-numbered length-prefixed records whose
  structured bodies are JSON. The automation record is the one `vvmux api schema --json`
  publishes.

This module is pure standard library: it speaks the wire, it does not spawn a CLI.

    import vivid_sdk.automation as auto

    v = auto.vivido_connect(target="scratch")     # or socket=..., or no argument to discover
    before = v.request("inspect", {"window_id": 42})["window"]["sequences"]["screen"]
    v.request("typing", {"text": "cargo test", "window_id": 42})
    v.request("key", {"key": "Enter", "mods": [], "repeat": 1, "route": "application",
                      "target": {"window_id": 42}})
    v.request("wait_text", {"text": "test result", "regex": False, "after_screen": before,
                            "common": {"timeout": 5000, "target": {"window_id": 42}}})

    m = auto.vvmux_connect("default")
    panes = m.request({"method": "list_panes"})

Params mirror the *serde* shape of each runtime's request struct, not its CLI flags: clap's
`flatten` and defaults do not apply to serde, so structs flattened on the command line arrive as
nested objects (`target`, `common`), and fields the CLI would default (`mods`, `repeat`, `route`,
`regex`) are still required on the wire when the struct lacks `#[serde(default)]`. The capability
document's method list names the methods; the runtime's CLI source remains the reference for each
param shape.

Boundaries, stated plainly:

* This drives *products*; it is not a Vivid session role. Producing and presenting media is the
  rest of this package. Messaging other agents is the mesh — `agent_mesh`, a separate
  distribution, because the mesh deliberately is not Vivid.
* Unix only. The runtimes also serve Windows named pipes, which the standard library cannot open;
  use the CLI there.
* Trust is one operating-system account, the same trust the runtimes themselves grant: every
  socket is checked to belong to this user before a byte is written to it, and the peer's
  credential is checked after connect. Registries are only read from a runtime directory that is
  a plain, owner-only directory, so a registry is only ever read from somewhere this user
  arranged.
* A named target never silently falls through to a different instance. Discovery without a name
  may: an inherited `VIVIDO_SOCKET`, the only live instance, then the newest window on this
  display — the same order the CLI resolves, so a script and a human reach the same endpoint.
"""

from __future__ import annotations

import hashlib
import json
import os
import socket
import stat
import struct
from pathlib import Path
from typing import Any, Dict, Mapping, Optional, Tuple

__all__ = [
    "AutomationError",
    "VividoSession",
    "VvmuxSession",
    "vivido_connect",
    "vivido_instances",
    "vvmux_connect",
]

# The newline-delimited protocol vivido serves and vivida embeds (vivido/src/polling/ipc.rs).
PROTOCOL_VERSION = 2
_MAX_REQUEST_FRAME_BYTES = 1024 * 1024
_MAX_REPLY_FRAME_BYTES = 16 * 1024 * 1024

# VVMX framing (vvmux/src/ipc.rs). `VVMX_VERSION` is what this client offers first; the preface
# exchange discovers the server's own version, and a mismatch retries once speaking that, so a
# vvmux rebuilt across a preface-version bump costs one extra connection, not a failed session.
VVMX_VERSION = 20
VVMX_MAGIC = b"VVMX"
_VVMX_CONTROL_CHANNEL = 1
_VVMX_CONTROL_MAX_BODY = 1024 * 1024
_VVMX_HEADER = struct.Struct("!QHHI")
_VVMX_STRUCTURED_RECORD = 1
_U64 = (1 << 64) - 1

_SESSION_ENV = "VIVIDO_SESSION"
_SOCKET_ENV = "VIVIDO_SOCKET"


class AutomationError(OSError):
    """A request the runtime refused, or an endpoint that could not be resolved.

    ``code`` is the contract the runtimes document — ``window_not_found``,
    ``method_not_supported``, ``limit_exceeded``, and the rest — while ``message`` is for humans.
    Endpoint resolution failures use client-side codes: ``endpoint_not_found``,
    ``endpoint_unsafe``.
    """

    def __init__(self, code: str, message: str, data: Any = None) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.data = data


# -------------------------------------------------------------------------------------------
# Shared endpoint plumbing
# -------------------------------------------------------------------------------------------


def _validate_session_name(name: str) -> str:
    """The rule both runtimes enforce (`validate_session_name` in each), mirrored before a name
    becomes part of a socket path. A name rejected here is one the runtime would reject too."""

    if (
        not name
        or len(name) > 64
        or name.startswith(".")
        or not all(byte.isascii() and (byte.isalnum() or byte in "-_.") for byte in name)
    ):
        raise AutomationError(
            "invalid_session_name",
            "session name must be 1-64 ASCII letters, digits, '.', '-' or '_' and not start '.'",
        )
    return name


def _runtime_dir(product: str) -> Path:
    """The per-user runtime root where a product keeps its sockets and registries.

    Held to the standard the servers hold it to, for the reason they do: a registry read from a
    directory another user can write to is a socket path chosen by that user. One that fails the
    check is declined, never repaired."""

    base = os.environ.get("XDG_RUNTIME_DIR")
    root = (Path(base) if base else Path(f"/tmp/{product}-{os.geteuid()}")) / product
    try:
        meta = root.lstat()
    except OSError as err:
        raise AutomationError(
            "endpoint_not_found", f"no {product} runtime directory at {root}"
        ) from err
    if (
        stat.S_ISLNK(meta.st_mode)
        or not stat.S_ISDIR(meta.st_mode)
        or meta.st_uid != os.geteuid()
        or meta.st_mode & 0o077
    ):
        raise AutomationError(
            "endpoint_unsafe", f"{product} runtime directory {root} is not owner-only"
        )
    return root


def _connect_socket(path: Path) -> socket.socket:
    """Connect to a local automation socket.

    The socket file must belong to this user before a byte is written — the same pre-connect
    check the CLI makes, because a socket another user planted where discovery looks should be
    declined, not talked to — and the peer's credential is checked after connect."""

    try:
        meta = path.lstat()
    except OSError as err:
        raise AutomationError("endpoint_not_found", f"no endpoint socket at {path}") from err
    if stat.S_ISLNK(meta.st_mode) or meta.st_uid != os.geteuid():
        raise AutomationError(
            "endpoint_unsafe", f"endpoint socket {path} is not owned by this user"
        )
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        stream.connect(os.fspath(path))
    except OSError:
        stream.close()
        raise
    peer = _peer_uid(stream)
    if peer is not None and peer != os.geteuid():
        stream.close()
        raise AutomationError(
            "endpoint_unsafe", f"endpoint {path} is served by uid {peer}, not this user"
        )
    return stream


def _peer_uid(stream: socket.socket) -> Optional[int]:
    """The connected peer's uid, where the platform can tell us.

    Linux answers from `SO_PEERCRED`; macOS from `getpeereid`, reached through ctypes because
    nothing in the standard library wraps it. Where neither exists the check is skipped and
    ``None`` says so — the pre-connect owner check above already ran."""

    if hasattr(socket, "SO_PEERCRED"):
        credential = stream.getsockopt(
            socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")
        )
        uid = struct.unpack("3i", credential)[1]
        return int(uid)
    try:
        import ctypes

        # ctypes has no c_uid_t on every release this package supports; uid and gid are
        # unsigned ints on the platforms this branch exists for.
        uid = ctypes.c_uint32()
        gid = ctypes.c_uint32()
        library = ctypes.CDLL(None, use_errno=True)
        if library.getpeereid(stream.fileno(), ctypes.byref(uid), ctypes.byref(gid)) != 0:
            return None
        return int(uid.value)
    except (OSError, AttributeError):
        return None


def _birth_check_supported() -> bool:
    """Whether this platform can recompute the process-birth record a registry carries."""

    return os.uname().sysname == "Linux"


def _linux_birth(pid: int) -> Any:
    """The `ProcessBirth::Linux` record the server wrote, recomputed from the same field of the
    same file: the process's start time in clock ticks. This is what makes a recycled pid a
    stale registry rather than someone else's session."""

    try:
        stat_text = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
    except (OSError, ValueError):
        return None
    end = stat_text.rfind(") ")
    if end < 0:
        return None
    fields = stat_text[end + 2 :].split()
    if len(fields) < 20:
        return None
    return {"platform": "linux", "start_ticks": int(fields[19])}


def _process_matches(registry: Mapping[str, Any]) -> bool:
    """Whether the registry's pid is still the process that wrote it.

    On Linux the birth record must match, exactly as the CLI demands. Elsewhere that record has
    a shape this module cannot recompute, so liveness alone decides — a documented weaker check,
    not a silent one."""

    pid = registry.get("pid")
    if not isinstance(pid, int) or pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    if _birth_check_supported():
        return bool(_linux_birth(pid) == registry.get("process_birth"))
    return True


def _instance_identity_ok(root: Path, registry: Mapping[str, Any]) -> bool:
    """Whether a registry is the one this name and socket layout produce.

    The socket path is *derived from the name*, never taken from the file: a registry that names
    another path fails here, so editing one JSON file cannot point a session name at an
    arbitrary socket."""

    name = registry.get("name")
    if not isinstance(name, str):
        return False
    digest = hashlib.sha256(name.encode()).hexdigest()[:32]
    if not isinstance(registry.get("socket"), str):
        return False
    return Path(registry["socket"]) == root / f"session-{digest}.sock"


# -------------------------------------------------------------------------------------------
# vivido / vivida
# -------------------------------------------------------------------------------------------


def vivido_instances() -> Tuple[Dict[str, Any], ...]:
    """Every live Vivido instance this user can reach, windowed or headless.

    The same registries `vivido list --all --json` prints, with the same validation: a registry
    whose identity does not check out, or whose process is gone, is not an instance."""

    root = _runtime_dir("vivido")
    found = []
    for entry in sorted(root.glob("session-*.json")):
        try:
            loaded = json.loads(entry.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if not isinstance(loaded, dict) or loaded.get("schema") != 1:
            continue
        if not _instance_identity_ok(root, loaded):
            continue
        if _process_matches(loaded):
            found.append(loaded)
    return tuple(found)


class VividoSession:
    """One connection to a vivido or vivida instance.

    vivida embeds vivido's host and serves the same endpoint — its extra `vivida_*` methods ride
    the same hello and the same envelopes — so one session drives either, and
    :attr:`capabilities` is the authority on which methods this instance claims. Reach for it
    rather than assuming: standalone Vivido claims `list_windows` and `create_window` where
    Vivida claims `vivida_layout` and `vivida_resolve_pane`, and a method the other product
    serves is not a method this one does.
    """

    def __init__(self, stream: socket.socket) -> None:
        self._stream = stream
        self._file = stream.makefile("rwb")
        self._next_id = 1
        self._capabilities: Dict[str, Any] = self._round_trip("hello", {})

    @property
    def capabilities(self) -> Mapping[str, Any]:
        """The hello document: methods, event kinds, error codes, limits."""

        return self._capabilities

    def request(self, method: str, params: Optional[Mapping[str, Any]] = None) -> Any:
        """Issue one automation request and return its result.

        The method name is validated before it is sent and the frame bounded before it is
        written — the same 1 MiB the server refuses — so an oversized request fails here with a
        clear error instead of a protocol error there. Read the sequences you are about to wait
        against *before* acting, or the wait can be satisfied by state you already saw.
        """

        if (
            not method
            or len(method) > 128
            or not method.isascii()
            or not method.replace("_", "a").isalnum()
        ):
            raise AutomationError(
                "invalid_request",
                "method must contain 1-128 ASCII letters, digits, or underscores",
            )
        return self._round_trip(method, dict(params or {}))

    def close(self) -> None:
        """End this connection. The runtime keeps running; only the connection goes."""

        self._file.close()
        self._stream.close()

    def __enter__(self) -> VividoSession:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def _round_trip(self, method: str, params: Dict[str, Any]) -> Any:
        self._next_id += 1
        request_id = self._next_id
        frame = json.dumps(
            {
                "version": PROTOCOL_VERSION,
                "id": request_id,
                "method": method,
                "params": params,
            }
        ).encode()
        if len(frame) + 1 > _MAX_REQUEST_FRAME_BYTES:
            raise AutomationError("limit_exceeded", "request exceeds the 1 MiB frame limit")
        self._file.write(frame + b"\n")
        self._file.flush()
        while True:
            line = self._file.readline(_MAX_REPLY_FRAME_BYTES + 1)
            if not line:
                raise AutomationError("endpoint_not_found", "the runtime closed the connection")
            if len(line) > _MAX_REPLY_FRAME_BYTES or not line.endswith(b"\n"):
                raise AutomationError("limit_exceeded", "reply exceeds the 16 MiB frame limit")
            value = json.loads(line)
            if not isinstance(value, dict) or "id" not in value:
                continue  # A subscription event, interleaved: no id, not this conversation.
            if value.get("version") != PROTOCOL_VERSION or value.get("id") != request_id:
                continue
            if value.get("ok") is True:
                return value.get("result")
            error = value.get("error") or {}
            raise AutomationError(
                str(error.get("code", "invalid_response")),
                str(error.get("message", "the runtime sent no error payload")),
                error.get("data"),
            )


def vivido_connect(
    *,
    socket: Optional[os.PathLike[str] | str] = None,
    target: Optional[str] = None,
    timeout: Optional[float] = None,
) -> VividoSession:
    """Connect to a vivido or vivida instance, resolving the endpoint the way the CLI does.

    An explicit ``socket`` wins. Then ``target``, else an inherited ``VIVIDO_SESSION`` — and a
    named instance that is not running is an error, never a silent fall-through to a different
    one. Without a name: ``VIVIDO_SOCKET`` if it still connects, the only live instance when
    there is exactly one, and finally the newest windowed instance on this display.
    """

    root: Optional[Path] = None
    try:
        root = _runtime_dir("vivido")
    except AutomationError:
        if socket is None and target is None and _SESSION_ENV not in os.environ:
            raise

    if socket is not None:
        stream = _connect_socket(Path(os.fspath(socket)))
    elif target is not None or _SESSION_ENV in os.environ:
        if root is None:
            raise AutomationError("endpoint_not_found", "no vivido runtime directory")
        name = target if target is not None else os.environ[_SESSION_ENV]
        registry = _named_registry(root, name)
        stream = _connect_socket(Path(str(registry["socket"])))
    else:
        stream = _discover(root)

    if timeout is not None:
        stream.settimeout(timeout)
    return VividoSession(stream)


def _named_registry(root: Path, name: str) -> Dict[str, Any]:
    """Read one named instance's registry, holding it to every check the CLI holds."""

    _validate_session_name(name)
    digest = hashlib.sha256(name.encode()).hexdigest()[:32]
    try:
        loaded = json.loads((root / f"session-{digest}.json").read_text(encoding="utf-8"))
    except OSError as err:
        raise AutomationError(
            "endpoint_not_found", f"no running Vivido instance named {name!r}"
        ) from err
    except ValueError as err:
        raise AutomationError(
            "endpoint_unsafe", f"registry for {name!r} is not valid JSON"
        ) from err
    if not isinstance(loaded, dict):
        raise AutomationError("endpoint_unsafe", f"registry for {name!r} is not an object")
    if loaded.get("schema") != 1 or loaded.get("protocol_version") != PROTOCOL_VERSION:
        raise AutomationError(
            "endpoint_unsafe", f"registry for {name!r} is not a schema this client reads"
        )
    if loaded.get("name") != name or not _instance_identity_ok(root, loaded):
        raise AutomationError(
            "endpoint_unsafe", f"registry for {name!r} does not match its endpoint identity"
        )
    if not _process_matches(loaded):
        raise AutomationError(
            "endpoint_not_found", f"Vivido instance {name!r} is no longer running"
        )
    return loaded


def _discover(root: Optional[Path]) -> socket.socket:
    """The unqualified order: inherited socket, sole live instance, newest windowed.

    Every step may decline; only running out of steps is an error, which is what makes the
    inherited-socket step a preference rather than a commitment."""

    inherited = os.environ.get(_SOCKET_ENV)
    if inherited:
        try:
            return _connect_socket(Path(inherited))
        except (AutomationError, OSError):
            pass
    if root is not None:
        instances = vivido_instances()
        if len(instances) == 1:
            return _connect_socket(Path(str(instances[0]["socket"])))
        return _newest_windowed(root)
    raise AutomationError(
        "endpoint_not_found", "no vivido endpoint: pass socket= or target=, or start an instance"
    )


def _newest_windowed(root: Path) -> socket.socket:
    """Windowed instances advertise on the display they render to:
    `Vivido-<display>-<pid>.sock`. Sorted newest-first by name — the pid is in the name, the same
    ordering the CLI uses — and stale sockets are skipped, not adopted."""

    display = os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY") or ""
    prefix = f"Vivido-{display.replace('/', '-')}-"
    for path in sorted(root.glob(f"{prefix}*.sock"), reverse=True):
        try:
            return _connect_socket(path)
        except (AutomationError, OSError):
            continue
    raise AutomationError("endpoint_not_found", "no windowed Vivido instance on this display")


# -------------------------------------------------------------------------------------------
# vvmux
# -------------------------------------------------------------------------------------------


class VvmuxSession:
    """One connection to a vvmux session server.

    Requests are automation records — the dicts `vvmux api schema --json` describes, with the
    verb as the ``method`` key — and the per-request fields that are properties of the request
    rather than of the verb (``pane_id``, ``agent``, ``expect``, and the rest) are keyword
    arguments, so the method dict stays exactly what the schema publishes.
    """

    def __init__(self, stream: socket.socket, maximum_body: int) -> None:
        self._stream = stream
        self._maximum = maximum_body
        self._send_sequence = 0
        self._recv_sequence = 0
        self._next_id = 0

    def request(
        self,
        method: Mapping[str, Any],
        *,
        pane_id: Optional[int] = None,
        agent: Optional[str] = None,
        pane_name: Optional[str] = None,
        lease: Optional[str] = None,
        allow_focused: bool = False,
        expect: Optional[Mapping[str, Any]] = None,
        idempotency_key: Optional[str] = None,
    ) -> Any:
        """Issue one automation request and return its result.

        ``method`` must carry a ``method`` key naming the verb. The keyword arguments are the
        envelope fields every request may carry: which pane it addresses, which agent it names,
        what it assumes about the session, and how a retry is recognised.
        """

        verb = method.get("method")
        if not isinstance(verb, str) or not verb:
            raise AutomationError("invalid_request", "an automation method needs a `method` verb")
        envelope = ("id", "pane_id", "agent", "pane_name", "lease", "allow_focused", "expect",
                    "idempotency_key")
        clash = sorted(set(method) & set(envelope))
        if clash:
            raise AutomationError(
                "invalid_request",
                f"{', '.join(clash)} belong on the request, not inside the method",
            )
        self._next_id += 1
        # The method record rides whole — the verb and its own parameters, exactly as the schema
        # publishes them — and the envelope fields go beside it.
        request: Dict[str, Any] = {"id": self._next_id, **method}
        for key, value in (
            ("pane_id", pane_id),
            ("agent", agent),
            ("pane_name", pane_name),
            ("lease", lease),
            ("expect", expect),
            ("idempotency_key", idempotency_key),
        ):
            if value is not None:
                request[key] = value
        if allow_focused:
            request["allow_focused"] = True
        self._send({"automation": request})
        while True:
            reply = self._recv()
            response = reply.get("Automation")
            if not isinstance(response, dict) or response.get("id") != self._next_id:
                continue  # Pong, Title and friends: addressed to no request of ours.
            if response.get("ok") is True:
                return response.get("result")
            error = response.get("error") or {}
            raise AutomationError(
                str(error.get("code", "invalid_response")),
                str(error.get("message", "the session server sent no error payload")),
            )

    def close(self) -> None:
        """End this connection. The session keeps running; only the connection goes."""

        self._stream.close()

    def __enter__(self) -> VvmuxSession:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def _send(self, message: Mapping[str, Any]) -> None:
        body = json.dumps(message).encode()
        if len(body) > self._maximum:
            raise AutomationError("limit_exceeded", "request exceeds the negotiated body limit")
        header = _VVMX_HEADER.pack(self._send_sequence, _VVMX_STRUCTURED_RECORD, 0, len(body))
        self._send_sequence = (self._send_sequence + 1) & _U64
        self._stream.sendall(header + body)

    def _recv(self) -> Dict[str, Any]:
        header = _recv_exact(self._stream, _VVMX_HEADER.size)
        sequence, record_type, flags, length = _VVMX_HEADER.unpack(header)
        if sequence != self._recv_sequence:
            raise AutomationError(
                "invalid_response", f"VVMX record sequence gap at {self._recv_sequence}"
            )
        self._recv_sequence = (self._recv_sequence + 1) & _U64
        if flags & ~0x0001 or record_type != _VVMX_STRUCTURED_RECORD:
            raise AutomationError("invalid_response", "unexpected VVMX control record")
        if length > self._maximum:
            raise AutomationError(
                "invalid_response", "VVMX record body exceeds the negotiated limit"
            )
        value = json.loads(_recv_exact(self._stream, length))
        if not isinstance(value, dict):
            raise AutomationError("invalid_response", "VVMX record body is not an object")
        return value


def vvmux_connect(target: str = "default", *, timeout: Optional[float] = None) -> VvmuxSession:
    """Connect to a vvmux session server by name.

    The socket path is derived from the name the same way the server derives its own. The
    preface exchange doubles as version discovery: this client offers ``VVMX_VERSION`` and, if
    the server answers with a different preface version, reconnects once speaking that — so a
    vvmux rebuilt across a version bump still connects, without this module being edited.
    """

    _validate_session_name(target)
    root = _runtime_dir("vvmux")
    digest = hashlib.sha256(target.encode()).hexdigest()[:32]
    path = root / f"session-{digest}.sock"
    stream = _connect_socket(path)
    if timeout is not None:
        stream.settimeout(timeout)
    try:
        stream, maximum = _vvmux_preface(stream, VVMX_VERSION, path)
    except OSError:
        stream.close()
        raise
    return VvmuxSession(stream, maximum)


def _vvmux_preface(
    stream: socket.socket, version: int, path: Path
) -> Tuple[socket.socket, int]:
    """Exchange prefaces, offering ``version`` and honouring what comes back.

    The server writes its preface before it reads ours, so a mismatch is still answered: the
    client learns the version it should have offered and reconnects once. Bounded at one retry —
    a server that disagrees twice is not going to agree a third time."""

    for attempt in (1, 2):
        offered = (
            VVMX_MAGIC
            + version.to_bytes(2, "big")
            + bytes([_VVMX_CONTROL_CHANNEL, 0])
            + _VVMX_CONTROL_MAX_BODY.to_bytes(4, "big")
        )
        stream.sendall(offered)
        peer = _recv_exact(stream, 12)
        if peer[:4] != VVMX_MAGIC:
            stream.close()
            raise AutomationError("invalid_response", "bad VVMX magic")
        peer_version = int.from_bytes(peer[4:6], "big")
        peer_maximum = int.from_bytes(peer[8:12], "big")
        if peer_version == version:
            if peer[6] != _VVMX_CONTROL_CHANNEL or peer[7] != 0:
                stream.close()
                raise AutomationError("invalid_response", "VVMX channel mismatch")
            if peer_maximum == 0 or peer_maximum > _VVMX_CONTROL_MAX_BODY:
                stream.close()
                raise AutomationError("invalid_response", "invalid VVMX maximum body")
            return stream, min(_VVMX_CONTROL_MAX_BODY, peer_maximum)
        stream.close()
        if attempt == 2:
            raise AutomationError(
                "invalid_response",
                f"vvmux speaks VVMX v{peer_version}, not v{version}",
            )
        version = peer_version
        stream = _connect_socket(path)
    raise AssertionError("unreachable")  # pragma: no cover


def _recv_exact(stream: socket.socket, count: int) -> bytes:
    parts = []
    while count > 0:
        chunk = stream.recv(count)
        if not chunk:
            raise AutomationError("endpoint_not_found", "the endpoint closed the connection")
        parts.append(chunk)
        count -= len(chunk)
    return b"".join(parts)
