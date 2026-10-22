"""Durable private checkpoints. Importing this module performs no I/O.

A new run always creates a new exclusive directory. Reopening and continuing an
uncertain provider request is deliberately unsupported: an operator must inspect
the preserved reservation instead of silently replaying it.
"""
from __future__ import annotations
import hashlib
import json
import os
import secrets
import stat
from pathlib import Path


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


def _directory(path):
    path = Path(path)
    if not path.is_absolute() or path.resolve() != path or ".." in path.parts:
        raise ValueError("literal absolute protected directory required")
    if stat.S_IMODE(path.stat().st_mode) != 0o700 or not path.is_dir():
        raise ValueError("protected directory required")
    return path


def atomic_private(root, name, value, *, replace=True):
    """Atomic 0600 checkpoint, fsync contents and containing directory."""
    root = _directory(root)
    if Path(name).name != name or name in ("", ".", ".."):
        raise ValueError("literal checkpoint filename required")
    raw = canonical(value) + b"\n"
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    temporary = ".pending-" + secrets.token_hex(16)
    try:
        try:
            info = os.stat(name, dir_fd=directory, follow_symlinks=False)
        except FileNotFoundError:
            info = None
        if info is not None and (not replace or not stat.S_ISREG(info.st_mode)
                                 or stat.S_IMODE(info.st_mode) != 0o600):
            raise ValueError("checkpoint collision or unsafe checkpoint")
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=directory)
        with os.fdopen(fd, "wb") as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        if replace:
            os.replace(temporary, name, src_dir_fd=directory, dst_dir_fd=directory)
        else:
            # link is exclusive even if another process creates the name after
            # the precheck; no existing journal event can be replaced.
            os.link(temporary, name, src_dir_fd=directory, dst_dir_fd=directory,
                    follow_symlinks=False)
            os.unlink(temporary, dir_fd=directory)
        os.fsync(directory)
    finally:
        try:
            os.unlink(temporary, dir_fd=directory)
        except FileNotFoundError:
            pass
        os.close(directory)
    return hashlib.sha256(raw).hexdigest()


class PrivateRequestJournal:
    """Synchronous hash-chained events persisted before each spending step."""
    def __init__(self, root, manifest):
        parent = _directory(Path(root).parent)
        root = parent / Path(root).name
        if not isinstance(manifest, str) or not manifest:
            raise ValueError("frozen manifest identity required")
        root.mkdir(mode=0o700)  # Exclusive: never reuse a prior execution.
        parent_fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(parent_fd)
        finally:
            os.close(parent_fd)
        self.root = root
        self.manifest = manifest
        self.sequence = 0
        self.previous = "0" * 64
        self.failed = False
        self.terminal = False
        self.append({"event": "journal_created"})

    def append(self, event):
        if self.failed or self.terminal or not isinstance(event, dict):
            raise ValueError("journal unavailable or terminal")
        if event.get("manifest_sha256", self.manifest) != self.manifest:
            raise ValueError("journal manifest mismatch")
        value = {"manifest_sha256": self.manifest, "sequence": self.sequence,
                 "previous_sha256": self.previous, "payload": event}
        try:
            digest = atomic_private(self.root, f"{self.sequence:08d}.private.json", value, replace=False)
        except BaseException:
            self.failed = True
            raise
        self.sequence += 1
        self.previous = digest
        return digest

    def __call__(self, event):
        return self.append(event)

    def finish(self, status, result_sha256):
        self.append({"event": "journal_finished", "status": status,
                     "result_sha256": result_sha256})
        self.terminal = True
        return self.previous
