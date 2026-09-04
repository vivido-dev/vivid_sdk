"""The automation client, against servers that speak the real wire.

These are in-process fake servers, but what they speak is the protocol as the runtimes frame it:
newline-delimited JSON with the version and id correlation for vivido, the VVMX preface and
sequence-numbered records for vvmux. A client that only ever talked to a mock of its own design
would prove nothing about the runtimes; these frames are the ones `vivido` and `vvmux` write,
and the discovery tests run against real registry files in a real (temporary) runtime directory.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import socket
import stat
import tempfile
import threading
from pathlib import Path
from typing import Any, Dict, Iterator, List, Optional, Tuple

import pytest

from vivid_sdk import automation as auto

TIMEOUT = 5.0


# -------------------------------------------------------------------------------------------
# Fake servers
# -------------------------------------------------------------------------------------------


def serve_unix(
    path: Path, handler: "ServerHandler"
) -> Iterator[socket.socket]:
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(os.fspath(path))
    server.settimeout(TIMEOUT)
    server.listen(4)
    thread = threading.Thread(target=_accept_loop, args=(server, handler), daemon=True)
    thread.start()
    yield server
    server.close()


# A handler receives one accepted connection and drives it however the protocol requires.
ServerHandler = Any


def _accept_loop(server: socket.socket, handler: Any) -> None:
    while True:
        try:
            stream, _ = server.accept()
        except OSError:
            return
        thread = threading.Thread(target=handler, args=(stream,), daemon=True)
        thread.start()


def recv_exact(stream: socket.socket, count: int) -> bytes:
    parts: List[bytes] = []
    while count > 0:
        chunk = stream.recv(count)
        if not chunk:
            # Raised as an OSError, so a client that hangs up ends the handler quietly through
            # the same except OSError the real event paths take.
            raise ConnectionResetError("client closed mid-record")
        parts.append(chunk)
        count -= len(chunk)
    return b"".join(parts)


class NdjsonServer:
    """A vivido-shaped server: hello, then whatever the script answers.

    `script` maps a request method to a result, or an (code, message) error tuple. Setting
    `interleave_after_hello` makes it slip one id-less event frame in before the next response,
    which is what a subscription on another connection would look like to a client that must
    skip it.
    """

    def __init__(
        self,
        capabilities: Dict[str, Any],
        script: Dict[str, Any],
        *,
        interleave_after_hello: bool = False,
    ) -> None:
        self.capabilities = capabilities
        self.script = script
        self.interleave_after_hello = interleave_after_hello

    def __call__(self, stream: socket.socket) -> None:
        stream.settimeout(TIMEOUT)
        reader = stream.makefile("rb")
        while True:
            line = reader.readline()
            if not line:
                return
            request = json.loads(line)
            if request["method"] == "hello":
                self._reply(stream, request["id"], True, self.capabilities)
                continue
            if self.interleave_after_hello:
                stream.sendall(
                    json.dumps(
                        {
                            "version": auto.PROTOCOL_VERSION,
                            "subscription_id": 7,
                            "event_sequence": 1,
                            "event": {"type": "bell", "data": {}},
                        }
                    ).encode()
                    + b"\n"
                )
                self.interleave_after_hello = False
            answer = self.script.get(request["method"])
            if isinstance(answer, tuple):
                code, message = answer
                self._reply(
                    stream,
                    request["id"],
                    False,
                    None,
                    {"code": code, "message": message},
                )
            else:
                self._reply(stream, request["id"], True, answer)

    @staticmethod
    def _reply(
        stream: socket.socket,
        request_id: int,
        ok: bool,
        result: Any,
        error: Optional[Dict[str, Any]] = None,
    ) -> None:
        envelope: Dict[str, Any] = {
            "version": auto.PROTOCOL_VERSION,
            "id": request_id,
            "ok": ok,
        }
        if ok:
            envelope["result"] = result
        else:
            envelope["error"] = error
        stream.sendall(json.dumps(envelope).encode() + b"\n")


class VvmxServer:
    """A vvmux-shaped server: preface first, then structured records.

    Speaks `version` (defaulting to the client's own) so the retry test can raise it. The record
    bodies and the 16-byte `!QHHI` header are the server's real framing.
    """

    def __init__(
        self,
        version: int = auto.VVMX_VERSION,
        script: Optional[Dict[str, Any]] = None,
    ) -> None:
        self.version = version
        self.script = script or {}
        self.received: List[Dict[str, Any]] = []

    def __call__(self, stream: socket.socket) -> None:
        try:
            self._serve(stream)
        except OSError:
            return  # A client that hangs up is a closed connection, not a crashed server.

    def _serve(self, stream: socket.socket) -> None:
        stream.settimeout(TIMEOUT)
        peer_maximum = 1 << 20
        stream.sendall(
            auto.VVMX_MAGIC
            + self.version.to_bytes(2, "big")
            + bytes([1, 0])
            + peer_maximum.to_bytes(4, "big")
        )
        offered = recv_exact(stream, 12)
        if offered[:4] != auto.VVMX_MAGIC or int.from_bytes(offered[4:6], "big") != self.version:
            return
        # Sequence numbers are per direction: incoming starts at 0 and so does outgoing.
        incoming = 0
        outgoing = 0
        while True:
            header = recv_exact(stream, 16)
            record_sequence, record_type, flags, length = auto._VVMX_HEADER.unpack(header)
            if record_sequence != incoming:
                return
            incoming = (incoming + 1) & auto._U64
            body = recv_exact(stream, length)
            request = json.loads(body)
            self.received.append(request)
            automation = request.get("automation", {})
            if automation.get("method") == "hello":
                continue
            answer = self.script.get(automation.get("method"))
            reply = {"id": automation.get("id"), "ok": True, "result": answer}
            self._send(stream, outgoing, {"Automation": reply})
            outgoing = (outgoing + 1) & auto._U64

    @staticmethod
    def _send(stream: socket.socket, sequence: int, message: Dict[str, Any]) -> None:
        body = json.dumps(message).encode()
        stream.sendall(auto._VVMX_HEADER.pack(sequence, 1, 0, len(body)) + body)


# -------------------------------------------------------------------------------------------
# Fixtures
# -------------------------------------------------------------------------------------------


@pytest.fixture()  # type: ignore[untyped-decorator]
def runtime_root(monkeypatch: pytest.MonkeyPatch) -> Iterator[Path]:
    """The XDG_RUNTIME_DIR the tests resolve against.

    A short root of necessity: a registry socket name carries a 32-hex digest and AF_UNIX caps
    paths at 108 bytes, longer than pytest's own tmp_path. Held to the standard the client
    demands — owner-only, never a symlink — because a runtime directory that fails that check is
    a refused runtime directory, and these tests are about resolution, not about this one.
    """

    root = Path(tempfile.mkdtemp(prefix="vvsdk-"))
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(root))
    monkeypatch.delenv("VIVIDO_SESSION", raising=False)
    monkeypatch.delenv("VIVIDO_SOCKET", raising=False)
    try:
        yield root
    finally:
        shutil.rmtree(root, ignore_errors=True)


@pytest.fixture()  # type: ignore[untyped-decorator]
def runtime_dir(runtime_root: Path) -> Path:
    # The client demands owner-only, and mkdir() leaves the umask's mode: make it explicit.
    root = runtime_root / "vivido"
    root.mkdir()
    root.chmod(0o700)
    return root


@pytest.fixture()  # type: ignore[untyped-decorator]
def vvmux_dir(runtime_root: Path) -> Path:
    root = runtime_root / "vvmux"
    root.mkdir()
    root.chmod(0o700)
    return root


def write_registry(runtime: Path, name: str, socket_path: Path, *, live: bool = True) -> None:
    """Write a registry the way the server writes one, including the process-birth record.

    `live` writes *this* test process's birth, so the pid is real and the birth matches. A
    wrong-birth registry is exactly what a recycled pid leaves behind.
    """

    digest = hashlib.sha256(name.encode()).hexdigest()[:32]
    registry = {
        "schema": 1,
        "name": name,
        "pid": os.getpid(),
        "instance_nonce": "ab" * 32,
        "vivido_version": "test",
        "protocol_version": auto.PROTOCOL_VERSION,
        "endpoint_id": "cd" * 32,
        "process_birth": {"platform": "linux", "start_ticks": 1} if not live else _this_birth(),
        "socket": str(socket_path),
        "headless": True,
        "columns": 80,
        "lines": 24,
    }
    (runtime / f"session-{digest}.json").write_text(json.dumps(registry), encoding="utf-8")


def _this_birth() -> Dict[str, Any]:
    birth = auto._linux_birth(os.getpid())
    assert isinstance(birth, dict)
    return birth


def session_socket(runtime: Path, name: str) -> Path:
    digest = hashlib.sha256(name.encode()).hexdigest()[:32]
    return runtime / f"session-{digest}.sock"


# -------------------------------------------------------------------------------------------
# vivido / vivida
# -------------------------------------------------------------------------------------------


def test_hello_and_a_request_round_trip(runtime_dir: Path) -> None:
    server = NdjsonServer({"methods": ["typing"]}, {"inspect": {"window": {"id": 42}}})
    path = runtime_dir / "direct.sock"
    for _ in serve_unix(path, server):
        session = auto.vivido_connect(socket=path, timeout=TIMEOUT)
        try:
            assert session.capabilities == {"methods": ["typing"]}
            assert session.request("inspect", {"window_id": 42}) == {"window": {"id": 42}}
        finally:
            session.close()


def test_an_interleaved_event_frame_does_not_answer_a_request(runtime_dir: Path) -> None:
    server = NdjsonServer(
        {"methods": []}, {"ping": "pong"}, interleave_after_hello=True
    )
    path = runtime_dir / "events.sock"
    for _ in serve_unix(path, server):
        session = auto.vivido_connect(socket=path, timeout=TIMEOUT)
        try:
            # The event frame carries no id; if it were mistaken for a reply this request would
            # hang or misresolve.
            assert session.request("ping") == "pong"
        finally:
            session.close()


def test_a_refused_request_raises_a_typed_error(runtime_dir: Path) -> None:
    server = NdjsonServer(
        {"methods": []}, {"inspect": ("window_not_found", "no window 99 on this instance")}
    )
    path = runtime_dir / "error.sock"
    for _ in serve_unix(path, server):
        session = auto.vivido_connect(socket=path, timeout=TIMEOUT)
        try:
            with pytest.raises(auto.AutomationError) as raised:
                session.request("inspect", {"window_id": 99})
            assert raised.value.code == "window_not_found"
            assert raised.value.message == "no window 99 on this instance"
            assert "window_not_found" in str(raised.value)
        finally:
            session.close()


def test_a_bad_method_never_reaches_the_wire(runtime_dir: Path) -> None:
    server = NdjsonServer({"methods": []}, {})
    path = runtime_dir / "methods.sock"
    for _ in serve_unix(path, server):
        session = auto.vivido_connect(socket=path, timeout=TIMEOUT)
        try:
            for method in ("", "with-dash", "with space", "x" * 129):
                with pytest.raises(auto.AutomationError) as raised:
                    session.request(method)
                assert raised.value.code == "invalid_request"
        finally:
            session.close()


def test_a_named_target_resolves_through_its_registry(
    runtime_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    server = NdjsonServer({"methods": ["vivida_layout"]}, {})
    path = session_socket(runtime_dir, "scratch")
    write_registry(runtime_dir, "scratch", path)
    for _ in serve_unix(path, server):
        with auto.vivido_connect(target="scratch", timeout=TIMEOUT) as session:
            assert session.capabilities == {"methods": ["vivida_layout"]}


def test_a_registry_cannot_point_a_name_at_another_socket(
    runtime_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The identity check is what stops one edited JSON file from redirecting a session name to
    # whatever socket the editor likes.
    write_registry(runtime_dir, "scratch", runtime_dir / "elsewhere.sock")
    with pytest.raises(auto.AutomationError) as raised:
        auto.vivido_connect(target="scratch", timeout=TIMEOUT)
    assert raised.value.code == "endpoint_unsafe"


def test_a_recycled_pid_is_a_stale_registry(
    runtime_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Live pid, wrong birth: exactly what a registry looks like after the real owner exited and
    # the pid was handed to someone else.
    write_registry(runtime_dir, "scratch", session_socket(runtime_dir, "scratch"), live=False)
    with pytest.raises(auto.AutomationError) as raised:
        auto.vivido_connect(target="scratch", timeout=TIMEOUT)
    assert raised.value.code == "endpoint_not_found"
    assert "no longer running" in raised.value.message


def test_a_named_target_that_is_gone_is_an_error_never_a_fall_through(
    runtime_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A live instance exists, but the *named* one does not. Naming an instance that is not
    # running must be an error, not a silent connection to whichever instance happens to exist.
    server = NdjsonServer({"methods": []}, {})
    path = runtime_dir / "other.sock"
    write_registry(runtime_dir, "other", path)
    for _ in serve_unix(path, server):
        with pytest.raises(auto.AutomationError) as raised:
            auto.vivido_connect(target="missing", timeout=TIMEOUT)
        assert raised.value.code == "endpoint_not_found"
        monkeypatch.setenv("VIVIDO_SESSION", "missing")
        with pytest.raises(auto.AutomationError):
            auto.vivido_connect(timeout=TIMEOUT)


def test_an_unnamed_request_reaches_the_sole_live_instance(
    runtime_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    server = NdjsonServer({"methods": []}, {})
    path = session_socket(runtime_dir, "only")
    write_registry(runtime_dir, "only", path)
    for _ in serve_unix(path, server):
        with auto.vivido_connect(timeout=TIMEOUT) as session:
            assert session.capabilities == {"methods": []}


def test_instances_lists_live_registries_only(runtime_dir: Path) -> None:
    server = NdjsonServer({"methods": []}, {})
    path = session_socket(runtime_dir, "alive")
    write_registry(runtime_dir, "alive", path)
    write_registry(runtime_dir, "gone", runtime_dir / "unbound.sock", live=False)
    # A registry whose socket does not match its name is not an instance either, however alive
    # its pid.
    write_registry(runtime_dir, "liar", runtime_dir / "other-place.sock")
    for _ in serve_unix(path, server):
        found = auto.vivido_instances()
    assert [instance["name"] for instance in found] == ["alive"]


def test_an_unsafe_runtime_directory_is_declined(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    readable = tmp_path / "readable" / "vivido"
    readable.mkdir(parents=True)
    readable.chmod(0o755)
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path / "readable"))
    monkeypatch.delenv("VIVIDO_SESSION", raising=False)
    with pytest.raises(auto.AutomationError) as raised:
        auto.vivido_connect(timeout=TIMEOUT)
    assert raised.value.code == "endpoint_unsafe"


def test_a_symlinked_socket_is_declined(runtime_dir: Path) -> None:
    server = NdjsonServer({"methods": []}, {})
    real = runtime_dir / "real.sock"
    link = runtime_dir / "link.sock"
    for _ in serve_unix(real, server):
        link.symlink_to(real)
        with pytest.raises(auto.AutomationError) as raised:
            auto.vivido_connect(socket=link, timeout=TIMEOUT)
        assert raised.value.code == "endpoint_unsafe"


# -------------------------------------------------------------------------------------------
# vvmux
# -------------------------------------------------------------------------------------------


def test_a_vvmux_request_round_trip(vvmux_dir: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    server = VvmxServer(script={"list_panes": [{"pane_id": 1, "command": "vim"}]})
    path = vvmux_dir / f"session-{hashlib.sha256(b'default').hexdigest()[:32]}.sock"
    for _ in serve_unix(path, server):
        with auto.vvmux_connect("default", timeout=TIMEOUT) as session:
            panes = session.request({"method": "list_panes"})
        assert panes == [{"pane_id": 1, "command": "vim"}]
        assert server.received[0]["automation"]["method"] == "list_panes"
        assert server.received[0]["automation"]["id"] == 1


def test_request_fields_are_envelope_not_method(
    vvmux_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    server = VvmxServer(script={"get_text": "hello"})
    path = vvmux_dir / f"session-{hashlib.sha256(b'work').hexdigest()[:32]}.sock"
    for _ in serve_unix(path, server):
        with auto.vvmux_connect("work", timeout=TIMEOUT) as session:
            session.request(
                {"method": "get_text", "max_bytes": 4096}, pane_id=3, allow_focused=True
            )
    sent = server.received[0]["automation"]
    # The verb and its own parameters stay together; the envelope fields ride beside them.
    assert sent["method"] == "get_text"
    assert sent["max_bytes"] == 4096
    assert sent["pane_id"] == 3
    assert sent["allow_focused"] is True


def test_a_vvmux_version_mismatch_is_discovered_and_retried(
    vvmux_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A server rebuilt across a preface bump: the first connection learns v21 and the retry
    # speaks it, so the client connects without this module being edited.
    server = VvmxServer(version=auto.VVMX_VERSION + 1, script={"capabilities": {"version": 21}})
    path = vvmux_dir / f"session-{hashlib.sha256(b'bumped').hexdigest()[:32]}.sock"
    for _ in serve_unix(path, server):
        with auto.vvmux_connect("bumped", timeout=TIMEOUT) as session:
            assert session.request({"method": "capabilities"}) == {"version": 21}


def test_a_vvmux_refusal_raises_a_typed_error(
    vvmux_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    server = RefusingVvmxServer()
    path = vvmux_dir / f"session-{hashlib.sha256(b'no').hexdigest()[:32]}.sock"
    for _ in serve_unix(path, server):
        with auto.vvmux_connect("no", timeout=TIMEOUT) as session:
            with pytest.raises(auto.AutomationError) as raised:
                session.request({"method": "capture"}, pane_id=9)
            assert raised.value.code == "pane_not_found"


class RefusingVvmxServer(VvmxServer):
    """Answers every method with a typed refusal, the way a real server refuses."""

    def __call__(self, stream: socket.socket) -> None:
        try:
            self._serve(stream)
        except OSError:
            return  # A client that hangs up is a closed connection, not a crashed server.

    def _serve(self, stream: socket.socket) -> None:
        stream.settimeout(TIMEOUT)
        stream.sendall(
            auto.VVMX_MAGIC
            + self.version.to_bytes(2, "big")
            + bytes([1, 0])
            + (1 << 20).to_bytes(4, "big")
        )
        recv_exact(stream, 12)
        while True:
            header = recv_exact(stream, 16)
            _sequence, _record_type, _flags, length = auto._VVMX_HEADER.unpack(header)
            body = recv_exact(stream, length)
            automation = json.loads(body).get("automation", {})
            reply = {
                "Automation": {
                    "id": automation.get("id"),
                    "ok": False,
                    "error": {"code": "pane_not_found", "message": "no pane 9 here"},
                }
            }
            encoded = json.dumps(reply).encode()
            stream.sendall(auto._VVMX_HEADER.pack(0, 1, 0, len(encoded)) + encoded)


def test_a_method_without_a_verb_is_refused_before_the_wire(
    vvmux_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    server = VvmxServer()
    path = vvmux_dir / f"session-{hashlib.sha256(b'q').hexdigest()[:32]}.sock"
    for _ in serve_unix(path, server):
        with auto.vvmux_connect("q", timeout=TIMEOUT) as session:
            with pytest.raises(auto.AutomationError) as raised:
                session.request({"pane_id": 1})
            assert raised.value.code == "invalid_request"


# -------------------------------------------------------------------------------------------
# Shared rules
# -------------------------------------------------------------------------------------------


def test_session_names_follow_the_runtime_rule() -> None:
    for name in ("", "." + "a", "a" * 65, "sp ace", "sl/ash"):
        with pytest.raises(auto.AutomationError) as raised:
            auto._validate_session_name(name)
        assert raised.value.code == "invalid_session_name"
    assert auto._validate_session_name("dev-1.2_a") == "dev-1.2_a"


def test_every_fake_server_socket_was_owner_only(runtime_dir: Path) -> None:
    # The client refuses sockets it does not own before writing to them; these tests only pass
    # because the fixtures create sockets the test user owns, which is the same property the
    # client checks. Pinned here so nobody weakens _connect_socket and watches the suite stay
    # green.
    path = runtime_dir / "probe.sock"
    for _ in serve_unix(path, NdjsonServer({}, {})):
        meta = path.lstat()
    assert stat.S_ISSOCK(meta.st_mode)
    assert meta.st_uid == os.geteuid()
