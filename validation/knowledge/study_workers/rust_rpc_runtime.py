"""Gold-free role-local process bridges; never interprets candidate test markers.

The protected driver supplies the exact frozen assertion argv and owns assertion
counting/outcome authentication. The worker driver supplies a pinned worker
binary argv, never a command supplied by a candidate RPC payload. Containers,
mounts, network denial and final process-namespace teardown belong to the trusted
launcher. This module does not make subprocess isolation claims by itself.
"""
from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import threading
import time
from typing import Callable

from validation.knowledge.evaluation_rust_rpc import (
    MAX_FRAME_BYTES, RPC_MODULE, RPC_FUNCTION, SOCKET_PATH, RustRpcError,
    decode_json, encode_json, validate_arguments, validate_decision,
)

MAX_CALLS = 4096
MAX_BYTES = 16 * 1024 * 1024
EXCEPTIONS = frozenset({"RustPanic", "RustRpcError"})


def _request(value, expected_id):
    if type(value) is not dict or value.keys() != {"protocol", "id", "method", "args", "kwargs"}:
        raise RustRpcError("invalid request fields")
    if type(value["protocol"]) is not int or value["protocol"] != 2 or type(value["id"]) is not int or value["id"] != expected_id:
        raise RustRpcError("request identity mismatch")
    args = value["args"]
    if value["method"] != "call" or value["kwargs"] != {} or type(value["kwargs"]) is not dict:
        raise RustRpcError("operation outside closed Rust seam")
    if type(args) is not list or len(args) != 3 or args[:2] != [RPC_MODULE, RPC_FUNCTION]:
        raise RustRpcError("operation outside closed Rust seam")
    validate_arguments(args[2])
    return args


def _result(value):
    if type(value) is not dict: raise RustRpcError("invalid result branch")
    if value.keys() == {"value"}:
        validate_decision(value["value"])
    elif value.keys() == {"exception"}:
        error = value["exception"]
        if type(error) is not dict or error.keys() != {"type", "message"} or type(error["type"]) is not str or error["type"] not in EXCEPTIONS:
            raise RustRpcError("undeclared exception")
        if type(error["message"]) is not str or len(error["message"].encode()) > 4096:
            raise RustRpcError("exception message limit")
    else:
        raise RustRpcError("invalid result branch")
    return value


def _read_fd_line(fd: int, deadline: float) -> bytes:
    # Unbuffered, bytewise reads keep trailing/unsolicited frames detectable.
    out = bytearray()
    while len(out) < MAX_FRAME_BYTES:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise RustRpcError("RPC read timeout")
        part = os.read(fd, 1)
        if not part: raise RustRpcError("RPC stream closed")
        out += part
        if part == b"\n": return bytes(out)
    raise RustRpcError("RPC frame limit")


def _write_fd(fd: int, raw: bytes, deadline: float):
    while raw:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([], [fd], [], remaining)[1]:
            raise RustRpcError("RPC write timeout")
        sent = os.write(fd, raw)
        if sent <= 0: raise RustRpcError("RPC write failed")
        raw = raw[sent:]


def _reap_group(process):
    # Sessions are created by this module, so the PID is the dedicated PGID.
    try: os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError: pass
    process.wait(timeout=5)
    try: os.killpg(process.pid, 0)
    except ProcessLookupError: return
    raise RustRpcError("process group survived cleanup")


class RustCandidateWorker:
    """Persistent binary bridge in the candidate role; result remains untrusted."""
    def __init__(self, argv: tuple[str, ...], *, cwd: str, env: dict[str, str], timeout: float = 30):
        if not argv or not Path(argv[0]).is_absolute(): raise RustRpcError("pinned absolute worker argv required")
        self.process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, bufsize=0, start_new_session=True)
        self.calls = self.bytes = 0
        self.timeout = timeout
        self.lock = threading.Lock()
        for pipe in (self.process.stdin, self.process.stdout): os.set_blocking(pipe.fileno(), False)

    def dispatch(self, method, args, kwargs, context=None):
        del context
        with self.lock:
            if self.calls >= MAX_CALLS or self.bytes >= MAX_BYTES: raise RustRpcError("RPC budget exhausted")
            self.calls += 1
            request = {"protocol": 2, "id": self.calls, "method": method, "args": args, "kwargs": kwargs}
            _request(request, self.calls)
            raw = encode_json(request)
            deadline = time.monotonic() + self.timeout
            _write_fd(self.process.stdin.fileno(), raw, deadline)
            response_raw = _read_fd_line(self.process.stdout.fileno(), deadline)
            self.bytes += len(raw) + len(response_raw)
            if self.bytes > MAX_BYTES: raise RustRpcError("RPC byte budget exhausted")
            response = decode_json(response_raw)
            if type(response) is not dict or response.keys() != {"protocol", "id", "result"}:
                raise RustRpcError("invalid worker envelope")
            if type(response["protocol"]) is not int or response["protocol"] != 2 or type(response["id"]) is not int or response["id"] != self.calls:
                raise RustRpcError("worker response identity mismatch")
            return _result(response["result"])

    def close(self):
        try: _reap_group(self.process)
        finally:
            for pipe in (self.process.stdin, self.process.stdout): pipe.close()


@dataclass(frozen=True)
class AssertionProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    rpc_calls: int
    # No pass/check fields: only the trusted frozen driver can supply those.


def run_exact_assertion_command(argv: tuple[str, ...], rpc: Callable, *, cwd: str,
                                env: dict[str, str], timeout: float = 60) -> AssertionProcessResult:
    """Serve the protected-only UDS while running the caller's exact argv.

    No wrapper is inserted into argv. The supplied environment must already be
    the trusted frozen compile/test environment. A transport error prevents a
    normal result; a child exit is never itself classified as assertion success.
    """
    endpoint = Path(SOCKET_PATH)
    endpoint.parent.mkdir(mode=0o700, parents=True, exist_ok=False)
    os.chmod(endpoint.parent, 0o700)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(endpoint))
    os.chmod(endpoint, 0o600)
    server.listen(1)
    server.settimeout(0.1)
    stop = threading.Event()
    errors = []
    state = {"calls": 0, "bytes": 0}
    deadline = time.monotonic() + timeout

    def serve():
        try:
            while not stop.is_set():
                try: connection, _ = server.accept()
                except socket.timeout: continue
                with connection:
                    connection.setblocking(False)
                    if state["calls"] >= MAX_CALLS: raise RustRpcError("RPC call budget exhausted")
                    raw = _read_fd_line(connection.fileno(), deadline)
                    request = decode_json(raw)
                    args = _request(request, state["calls"] + 1)
                    # This callable is injected into the protected driver by the
                    # trusted hostbroker. No candidate handle is imported here.
                    branch = _result(rpc("call", args, {}))
                    response = encode_json({"protocol": 2, "id": request["id"], "result": branch})
                    state["calls"] += 1
                    state["bytes"] += len(raw) + len(response)
                    if state["bytes"] > MAX_BYTES: raise RustRpcError("RPC byte budget exhausted")
                    _write_fd(connection.fileno(), response, deadline)
        except BaseException as exc:
            errors.append(exc)
            stop.set()

    thread = threading.Thread(target=serve, name="protected-rust-rpc", daemon=True)
    process = None
    thread.start()
    captures = [bytearray(), bytearray()]
    try:
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   bufsize=0, start_new_session=True)
        streams = {process.stdout.fileno(): captures[0], process.stderr.fileno(): captures[1]}
        for fd in streams: os.set_blocking(fd, False)
        while streams:
            if errors: raise RustRpcError("protected Rust RPC bridge failed") from errors[0]
            if time.monotonic() >= deadline: raise RustRpcError("assertion command timeout")
            for fd in select.select(list(streams), [], [], min(0.1, max(0, deadline-time.monotonic())))[0]:
                data = os.read(fd, 65536)
                if not data:
                    del streams[fd]
                else:
                    streams[fd].extend(data)
                    if sum(map(len, captures)) > MAX_BYTES: raise RustRpcError("assertion output limit")
        process.wait(timeout=max(0.1, deadline-time.monotonic()))
        if errors: raise RustRpcError("protected Rust RPC bridge failed") from errors[0]
        return AssertionProcessResult(process.returncode, bytes(captures[0]), bytes(captures[1]), state["calls"])
    finally:
        if process is not None:
            try: _reap_group(process)
            finally:
                process.stdout.close()
                process.stderr.close()
        stop.set()
        thread.join(timeout=max(0.1, deadline-time.monotonic())+0.2)
        server.close()
        endpoint.unlink(missing_ok=True)
        endpoint.parent.rmdir()
        if thread.is_alive(): raise RustRpcError("protected RPC thread survived cleanup")
