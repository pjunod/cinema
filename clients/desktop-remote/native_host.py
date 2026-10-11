#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Cinema's bounded stdio native-messaging host; run as the logged-in user."""
import argparse
import io
import json
import os
from pathlib import Path
import queue
import re
import struct
import sys
import threading
import time
import uuid

# Isolated Python launch excludes the working directory and user import path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from errors import BackendError

MAX_FRAME = 16 * 1024
MAX_SEQUENCE = (1 << 53) - 1
KEYS = {0x00: "select", 0x01: "up", 0x02: "down", 0x03: "left", 0x04: "right",
        0x09: "home", 0x0d: "back", 0x44: "play", 0x45: "stop", 0x46: "pause", 0x61: "play_pause"}
DIRECTIONS = {"up", "down", "left", "right"}

class ProtocolError(Exception):
    pass

def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ProtocolError("duplicate_field")
        result[key] = value
    return result

def read_exact(stream, count, eof=False):
    parts = bytearray()
    while len(parts) < count:
        block = stream.read(count - len(parts))
        if not block:
            if eof and not parts:
                return None
            raise ProtocolError("truncated_frame")
        parts.extend(block)
    return bytes(parts)

def read_frame(stream):
    header = read_exact(stream, 4, True)
    if header is None:
        return None
    size = struct.unpack("=I", header)[0]
    if not 0 < size <= MAX_FRAME:
        raise ProtocolError("frame_size")
    try:
        value = json.loads(read_exact(stream, size).decode("utf-8"), object_pairs_hook=strict_object,
                           parse_constant=lambda _: (_ for _ in ()).throw(ProtocolError("invalid_number")))
    except (ValueError, UnicodeError) as exc:
        raise ProtocolError("invalid_json") from exc
    if type(value) is not dict:
        raise ProtocolError("invalid_object")
    return value

def write_frame(stream, message):
    payload = json.dumps(message, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode("utf-8")
    if not 0 < len(payload) <= MAX_FRAME:
        raise ProtocolError("frame_size")
    stream.write(struct.pack("=I", len(payload)) + payload)
    stream.flush()

def valid_epoch(value):
    if type(value) is not str:
        return False
    try:
        return str(uuid.UUID(value)) == value
    except ValueError:
        return False

def load_config(path, origin):
    if path.stat().st_size > MAX_FRAME:
        raise ProtocolError("configuration_size")
    config = json.loads(path.read_text(), object_pairs_hook=strict_object)
    if set(config) != {"extension_id", "backend", "device"}:
        raise ProtocolError("configuration_fields")
    if type(config["extension_id"]) is not str or not re.fullmatch("[a-p]{32}", config["extension_id"]):
        raise ProtocolError("extension_id")
    if origin != "chrome-extension://" + config["extension_id"] + "/":
        raise ProtocolError("caller_origin")
    if config["backend"] not in ("kernel", "libcec") or type(config["device"]) is not str or not 0 < len(config["device"]) <= 256 or any(c in config["device"] for c in "\r\n\0"):
        raise ProtocolError("configuration_backend")
    return config

class Engine:
    def __init__(self, factory, send, clock=time.monotonic):
        self.factory, self.send, self.clock = factory, send, clock
        self.events = queue.Queue(maxsize=64)
        self.backend, self.generation = None, 0
        self.epoch, self.sequence, self.last_heartbeat, self.credit = None, 0, 0, None
        self.held, self.last_press, self.next_repeat = None, 0, 0
        self.retry_at, self.retry_delay, self.last_poll = 0, 1, 0
        self.state, self.overloaded = None, False
        self.last_clock = clock()

    def status(self, state):
        if state != self.state:
            self.state = state
            self.send({"type": "status", "state": state, "epoch": self.epoch})

    def release(self):
        self.held, self.last_press, self.next_repeat = None, 0, 0

    def close_backend(self):
        self.generation += 1
        backend, self.backend = self.backend, None
        self.release()
        if backend:
            backend.close()

    def command(self, message):
        kind = message.get("type")
        fields = {"type", "epoch", "credit"} if kind == "heartbeat" else {"type", "epoch"}
        if kind not in ("bind", "heartbeat", "release", "unbind") or set(message) != fields or not valid_epoch(message.get("epoch")) or (kind == "heartbeat" and not valid_epoch(message.get("credit"))):
            raise ProtocolError("command")
        if kind == "bind":
            self.close_backend()
            self.epoch, self.sequence, self.last_heartbeat, self.credit = message["epoch"], 0, self.clock(), None
            self.retry_at, self.retry_delay = 0, 1
            self.status("connecting")
        elif message["epoch"] != self.epoch:
            raise ProtocolError("stale_epoch")
        elif kind == "heartbeat":
            self.last_heartbeat, self.credit = self.clock(), message["credit"]
        elif kind == "release":
            self.release()
        else:
            self.close_backend()
            self.epoch = None
            self.status("unbound")

    def callback(self, generation, kind, key):
        try:
            self.events.put_nowait((generation, kind, key, self.clock(), self.credit))
        except queue.Full:
            self.overloaded = True

    def output_key(self, key, credit):
        if not credit:
            return
        if self.sequence >= MAX_SEQUENCE:
            self.close_backend()
            self.epoch = None
            self.status("unbound")
            return
        self.sequence += 1
        self.send({"type": "input", "key": key, "epoch": self.epoch, "sequence": self.sequence, "credit": credit})

    def eligible(self, now):
        if now < self.last_clock:
            self.close_backend()
            self.epoch = None
            self.status("disconnected")
        self.last_clock = now
        if not self.epoch:
            return False
        if not 0 <= now - self.last_heartbeat < 1:
            self.close_backend()
            self.epoch = None
            self.status("receiver_unavailable")
            return False
        return True

    def tick(self):
        now = self.clock()
        if not self.eligible(now):
            return
        if self.overloaded:
            self.overloaded = False
            self.release()
            self.status("busy")
            while not self.events.empty():
                self.events.get_nowait()
        if self.backend is None and now >= self.retry_at:
            self.generation += 1
            generation = self.generation
            try:
                backend = self.factory(lambda kind, key: self.callback(generation, kind, key))
                self.backend = backend
                backend.open()
                self.retry_delay, self.last_poll = 1, now
                self.status("bound")
            except BackendError as exc:
                self.close_backend()
                self.status(exc.state)
                self.retry_at = now + self.retry_delay
                self.retry_delay = min(30, self.retry_delay * 2)
        now = self.clock()
        if not self.eligible(now):
            return
        if self.backend and now - self.last_poll >= .25:
            self.last_poll = now
            try:
                self.backend.poll()
            except BackendError as exc:
                self.close_backend()
                self.status(exc.state)
                self.retry_at = now + self.retry_delay
                self.retry_delay = min(30, self.retry_delay * 2)
        now = self.clock()
        if not self.eligible(now):
            return
        for _ in range(64):
            try:
                generation, kind, code, when, credit = self.events.get_nowait()
            except queue.Empty:
                break
            if generation != self.generation or not 0 <= now - when < .75 or not self.backend:
                continue
            if kind == "release":
                if code is None or KEYS.get(code) == self.held:
                    self.release()
                continue
            key = KEYS.get(code)
            if not key:
                continue
            if key != self.held:
                self.held, self.last_press, self.next_repeat = key, when, when + .35
                self.output_key(key, credit)
            else:
                self.last_press = when
        if self.held and now - self.last_press >= .75:
            self.release()
        elif self.held in DIRECTIONS and now >= self.next_repeat:
            self.output_key(self.held, self.credit)
            self.next_repeat = now + .125

    def close(self):
        self.close_backend()
        self.epoch = None

def run(config, incoming, outgoing):
    from kernel_cec import KernelCecBackend
    from libcec_backend import LibCecBackend
    backend = KernelCecBackend if config["backend"] == "kernel" else LibCecBackend
    engine = Engine(lambda emit: backend(config["device"], emit), lambda value: write_frame(outgoing, value))
    commands = queue.Queue(maxsize=32)
    stopped = threading.Event()
    def reader():
        try:
            while not stopped.is_set():
                message = read_frame(incoming)
                if message is None:
                    break
                commands.put(message, timeout=.5)
        except (ProtocolError, queue.Full):
            pass
        finally:
            stopped.set()
    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    try:
        while not stopped.is_set():
            try:
                engine.command(commands.get(timeout=.05))
            except queue.Empty:
                pass
            if not stopped.is_set():
                engine.tick()
    finally:
        stopped.set()
        engine.close()

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("origin")
    parser.add_argument("--parent-window")  # Chrome Windows argument; never used for OS input
    args = parser.parse_args()
    if hasattr(os, "geteuid") and os.geteuid() == 0:
        raise ProtocolError("root_not_supported")
    if sys.platform == "win32":
        import msvcrt
        msvcrt.setmode(sys.stdin.fileno(), os.O_BINARY)
        msvcrt.setmode(sys.stdout.fileno(), os.O_BINARY)
    config = load_config(args.config, args.origin)
    run(config, sys.stdin.buffer, sys.stdout.buffer)

if __name__ == "__main__":
    try:
        main()
    except (ProtocolError, OSError, BrokenPipeError, KeyboardInterrupt) as exc:
        print("cinema-remote: " + type(exc).__name__, file=sys.stderr)
        raise SystemExit(1)
