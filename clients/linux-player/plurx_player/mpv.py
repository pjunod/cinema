"""mpv over its JSON IPC socket.

The socket lives in a private 0700 directory, and the handoff token never
appears on mpv's command line (which `ps` shows to every user): when a
route needs the bearer, it is set over this socket as an HTTP header field.
"""

import json
import os
import queue
import shutil
import socket
import subprocess
import threading
import time

DEFAULT_BINARY = "/usr/lib/plurx-player/bin/mpv"
# Plan section 4.2: vo=gpu-next on Vulkan, which receives the compositor's
# preferred colour description through Wayland colour management and
# presents PQ, with libplacebo mapping.
VIDEO_ARGS = ["--vo=gpu-next", "--gpu-api=vulkan", "--hwdec=auto-safe",
              "--target-colorspace-hint=yes"]
CLOSED = "__closed__"


class MpvError(Exception):
    pass


def resolve_binary(explicit=None):
    """The private build when installed, otherwise mpv on PATH."""
    for candidate in (explicit, os.environ.get("PLURX_PLAYER_MPV")):
        if candidate:
            return candidate
    if os.access(DEFAULT_BINARY, os.X_OK):
        return DEFAULT_BINARY
    return shutil.which("mpv")


class Mpv:
    def __init__(self, binary, socket_path, title="plurx Player"):
        self.binary = binary
        self.socket_path = socket_path
        self.title = title
        self.proc = None
        self._sock = None
        self._send_lock = threading.Lock()
        self._replies = {}
        self._reply_ready = threading.Condition()
        self._next_id = 0
        self._events = queue.Queue()
        self.closed = False

    def args(self):
        # idle=once: wait for our loadfile, quit when that playback ends.
        return [self.binary, "--idle=once", "--force-window=yes", "--no-terminal",
                f"--input-ipc-server={self.socket_path}", f"--title={self.title}",
                *VIDEO_ARGS]

    def start(self, connect_timeout=15.0):
        self.proc = subprocess.Popen(self.args(), stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + connect_timeout
        while True:
            if self.proc.poll() is not None:
                raise MpvError(f"mpv exited with status {self.proc.returncode} before it was ready")
            try:
                sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sock.connect(self.socket_path)
                break
            except OSError:
                sock.close()
                if time.monotonic() > deadline:
                    self.kill()
                    raise MpvError("mpv did not open its IPC socket") from None
                time.sleep(0.02)
        self._sock = sock
        threading.Thread(target=self._read, name="mpv-ipc", daemon=True).start()

    def _read(self):
        buffer = b""
        try:
            while True:
                chunk = self._sock.recv(65536)
                if not chunk:
                    break
                buffer += chunk
                while b"\n" in buffer:
                    line, buffer = buffer.split(b"\n", 1)
                    if line.strip():
                        self._dispatch(line)
        except OSError:
            pass
        finally:
            with self._reply_ready:
                self.closed = True
                self._reply_ready.notify_all()
            self._events.put({"event": CLOSED})

    def _dispatch(self, line):
        try:
            message = json.loads(line)
        except ValueError:
            return
        if not isinstance(message, dict):
            return
        if "event" in message:
            self._events.put(message)
        elif "request_id" in message:
            with self._reply_ready:
                self._replies[message["request_id"]] = message
                self._reply_ready.notify_all()

    def command(self, *args, timeout=5.0):
        if self.closed or self._sock is None:
            raise MpvError("mpv is not running")
        with self._send_lock:
            self._next_id += 1
            request_id = self._next_id
            payload = json.dumps({"command": list(args), "request_id": request_id}) + "\n"
            try:
                self._sock.sendall(payload.encode("utf-8"))
            except OSError as error:
                raise MpvError(f"mpv IPC write failed: {error}") from None
        deadline = time.monotonic() + timeout
        with self._reply_ready:
            while request_id not in self._replies:
                remaining = deadline - time.monotonic()
                if self.closed:
                    raise MpvError("mpv closed")
                if remaining <= 0:
                    raise MpvError(f"mpv did not answer {args[0]}")
                self._reply_ready.wait(remaining)
            reply = self._replies.pop(request_id)
        if reply.get("error") != "success":
            raise MpvError(f"{args[0]}: {reply.get('error')}")
        return reply.get("data")

    def get(self, name, default=None):
        try:
            value = self.command("get_property", name)
        except MpvError:
            return default
        return default if value is None else value

    def set(self, name, value):
        return self.command("set_property", name, value)

    def events(self):
        drained = []
        while True:
            try:
                drained.append(self._events.get_nowait())
            except queue.Empty:
                return drained

    def osd(self, text, duration_ms):
        try:
            self.command("show-text", text, int(duration_ms))
        except MpvError:
            pass

    def quit(self, timeout=5.0):
        if self.proc is None:
            return
        if self.proc.poll() is None and not self.closed:
            try:
                self.command("quit", timeout=2.0)
            except MpvError:
                pass
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.kill()
        self._close_socket()

    def kill(self):
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self._close_socket()

    def _close_socket(self):
        if self._sock is not None:
            try:
                # shutdown first: close() alone does not wake the reader's recv().
                self._sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                self._sock.close()
            except OSError:
                pass
