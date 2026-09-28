#!/usr/bin/env python3
"""Fake automation servers for `test_automation.lua`, which cannot open a socket from Lua.

What they speak is the wire as the runtimes frame it — newline-delimited JSON with version and id
correlation for vivido, the VVMX preface and sequence-numbered records for vvmux — so the Lua client
is tested against the protocol rather than against a mock of its own design. The same shapes as the
Python client's fixtures in `python-tests/test_automation.py`.

Usage: fake_automation_server.py CONFIG_JSON_PATH

The config names the mode (`ndjson` or `vvmx`), the socket — literally, or as the session a
runtime derives it from, since Lua has no SHA-256 to derive it with — the scripted answers, how
many connections to serve, and any registries to write. Registries carry this process's own pid,
so they are live for exactly as long as the server is. Prints `ready` once listening, then serves
until it has served every connection, the `stop` file appears, or ten seconds pass. Standard
library only.
"""

from __future__ import annotations

import hashlib
import json
import os
import socket
import struct
import sys
import time
from pathlib import Path
from typing import Any, Dict, List

TIMEOUT = 5.0
PROTOCOL_VERSION = 2
VVMX_MAGIC = b"VVMX"
VVMX_HEADER = struct.Struct("!QHHI")


def recv_exact(stream: socket.socket, count: int) -> bytes:
    parts: List[bytes] = []
    while count > 0:
        chunk = stream.recv(count)
        if not chunk:
            raise ConnectionResetError("client closed mid-record")
        parts.append(chunk)
        count -= len(chunk)
    return b"".join(parts)


def linux_birth(pid: int) -> Any:
    """This process's `ProcessBirth::Linux` record, as the server writes it."""
    try:
        text = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
    except OSError:
        return None
    fields = text[text.rfind(") ") + 2 :].split()
    return {"platform": "linux", "start_ticks": int(fields[19])}


def dead_pid() -> int:
    """A pid that belonged to a process which has exited: what a stale registry names."""
    child = os.fork()
    if child == 0:
        os._exit(0)
    os.waitpid(child, 0)
    return child


def session_path(root: str, product: str, name: str, suffix: str) -> str:
    """Where a runtime keeps a named session's socket or registry: derived from the name."""
    digest = hashlib.sha256(name.encode()).hexdigest()[:32]
    return str(Path(root, product, f"session-{digest}.{suffix}"))


def write_registry(spec: Dict[str, Any]) -> None:
    name = spec["name"]
    live = spec.get("live", True)
    birth = linux_birth(os.getpid())
    if spec.get("wrong_birth"):
        # A recycled pid: alive, but not the process that wrote the registry.
        birth = {"platform": "linux", "start_ticks": 1}
    registry = {
        "schema": 1,
        "name": name,
        "pid": os.getpid() if live else dead_pid(),
        "instance_nonce": "ab" * 32,
        "vivido_version": "test",
        "protocol_version": PROTOCOL_VERSION,
        "endpoint_id": "cd" * 32,
        "process_birth": birth or {"platform": "macos", "start": 0},
        "socket": spec.get("socket") or session_path(spec["root"], "vivido", name, "sock"),
        "headless": True,
        "columns": 80,
        "lines": 24,
    }
    Path(session_path(spec["root"], "vivido", name, "json")).write_text(
        json.dumps(registry), encoding="utf-8"
    )


def serve_ndjson(stream: socket.socket, config: Dict[str, Any]) -> None:
    reader = stream.makefile("rb")
    interleave = bool(config.get("interleave"))

    def reply(request_id: Any, ok: bool, result: Any = None, error: Any = None) -> None:
        envelope: Dict[str, Any] = {"version": PROTOCOL_VERSION, "id": request_id, "ok": ok}
        if ok:
            envelope["result"] = result
        else:
            envelope["error"] = error
        stream.sendall(json.dumps(envelope).encode() + b"\n")

    while True:
        line = reader.readline()
        if not line:
            return
        request = json.loads(line)
        method = request["method"]
        if method == "hello":
            reply(request["id"], True, config.get("capabilities", {}))
            continue
        if interleave:
            # An id-less subscription event slipped in before the answer, which the client must
            # skip rather than take as the answer.
            event = {"version": PROTOCOL_VERSION, "subscription_id": 7, "event_sequence": 1,
                     "event": {"type": "bell", "data": {}}}
            stream.sendall(json.dumps(event).encode() + b"\n")
            interleave = False
        errors = config.get("errors", {})
        if method in errors:
            reply(request["id"], False, error=errors[method])
        elif method == "echo":
            reply(request["id"], True, request)
        else:
            reply(request["id"], True, config.get("methods", {}).get(method))


def serve_vvmx(stream: socket.socket, config: Dict[str, Any]) -> None:
    version = int(config.get("version", 20))
    stream.sendall(VVMX_MAGIC + version.to_bytes(2, "big") + bytes([1, 0]) + (1 << 20).to_bytes(4, "big"))
    offered = recv_exact(stream, 12)
    if offered[:4] != VVMX_MAGIC or int.from_bytes(offered[4:6], "big") != version:
        return  # The client learns our version from our preface and reconnects speaking it.
    incoming = outgoing = 0
    while True:
        sequence, _record_type, _flags, length = VVMX_HEADER.unpack(recv_exact(stream, 16))
        if sequence != incoming:
            return
        incoming += 1
        request = json.loads(recv_exact(stream, length))
        automation = request.get("automation", {})
        method = automation.get("method")
        if config.get("refuse"):
            answer = {"id": automation.get("id"), "ok": False,
                      "error": {"code": "pane_not_found", "message": "no pane here"}}
        elif method == "echo":
            answer = {"id": automation.get("id"), "ok": True, "result": request}
        else:
            answer = {"id": automation.get("id"), "ok": True,
                      "result": config.get("methods", {}).get(method)}
        # A record addressed to no request, which the client must skip.
        for message in ({"Pong": {}}, {"Automation": answer}):
            body = json.dumps(message).encode()
            stream.sendall(VVMX_HEADER.pack(outgoing, 1, 0, len(body)) + body)
            outgoing += 1


def main() -> None:
    config = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    for spec in config.get("registries", []):
        write_registry(spec)
    path = config.get("socket")
    if path is None:
        session = config["session"]
        path = session_path(session["root"], session["product"], session["name"], "sock")
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    server.listen(4)
    server.settimeout(0.05)
    print("ready", flush=True)
    serve = serve_vvmx if config["mode"] == "vvmx" else serve_ndjson
    stop = config.get("stop")
    deadline = time.monotonic() + 10
    served = 0
    try:
        while served < int(config.get("connections", 1)) and time.monotonic() < deadline:
            if stop and os.path.exists(stop):
                break
            try:
                stream, _ = server.accept()
            except socket.timeout:
                continue
            served += 1
            stream.settimeout(TIMEOUT)
            try:
                serve(stream, config)
            except OSError:
                pass  # A client that hangs up is a closed connection, not a crashed server.
            finally:
                stream.close()
    finally:
        server.close()


if __name__ == "__main__":
    main()
