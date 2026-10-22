"""Exact-buffer reviewed Python source closure. No imports or I/O at import time.

The trusted operator verifies this bootstrap's bytes before compiling them.
Every project import thereafter is supplied from this closed, hash-pinned map;
normal path loading, cached bytecode and previously loaded project modules are
refused. The import hook stays installed for the entire operator workflow.
"""
from __future__ import annotations
import hashlib
import importlib.abc
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import sys


def read_pinned(path, expected_sha256, *, protected=False, max_bytes=16 * 1024**2):
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts or not re.fullmatch(r"[a-f0-9]{64}", expected_sha256):
        raise ValueError("literal path and SHA-256 required")
    directory = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in path.parts[1:-1]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory);directory = child
        if protected and stat.S_IMODE(os.fstat(directory).st_mode) != 0o700:
            raise ValueError("protected source parent required")
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
        with os.fdopen(fd, "rb") as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size > max_bytes:
                raise ValueError("bounded regular reviewed source required")
            if protected and stat.S_IMODE(before.st_mode) != 0o600:
                raise ValueError("protected source file required")
            raw = stream.read(max_bytes + 1);after = os.fstat(stream.fileno())
        fields = lambda i: (i.st_dev, i.st_ino, i.st_size, i.st_mtime_ns, i.st_ctime_ns)
        if fields(before) != fields(after) or len(raw) != before.st_size or hashlib.sha256(raw).hexdigest() != expected_sha256:
            raise ValueError("reviewed source changed or digest differs")
        return raw
    finally:
        os.close(directory)


def json_pinned(path, digest, *, protected=True):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:raise ValueError("duplicate manifest key")
            result[key] = value
        return result
    return json.loads(read_pinned(path, digest, protected=protected), object_pairs_hook=pairs)


class SourceClosure(importlib.abc.MetaPathFinder, importlib.abc.Loader):
    PREFIXES = ("validation", "_study_operator")

    def __init__(self, manifest_path, manifest_sha256):
        self.path, self.sha256 = Path(manifest_path), manifest_sha256
        self.manifest = json_pinned(self.path, manifest_sha256)
        if set(self.manifest) != {"schema_version", "modules", "assets"} or self.manifest["schema_version"] != 1:
            raise ValueError("closed source manifest required")
        self.modules = self.manifest["modules"];self.assets = self.manifest["assets"]
        if not isinstance(self.modules, dict) or not self.modules or not isinstance(self.assets, dict):
            raise ValueError("source manifest map required")
        self.buffers = {};self.installed = False;self.loaded = set()
        for name, row in self.modules.items():
            if not self.owns(name) or not re.fullmatch(r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*", name):
                raise ValueError("module outside closed project prefixes")
            if set(row) != {"path", "sha256", "package"} or type(row["package"]) is not bool:
                raise ValueError("closed module row required")
            if "." in name:
                parent = name.rpartition(".")[0]
                if parent not in self.modules or not self.modules[parent]["package"]:
                    raise ValueError("reviewed parent package required")
            self.buffers[name] = read_pinned(row["path"], row["sha256"])
        self.asset_buffers = {}
        for name, row in self.assets.items():
            if set(row) != {"path", "sha256"}:
                raise ValueError("closed asset row required")
            self.asset_buffers[name] = read_pinned(row["path"], row["sha256"], max_bytes=64 * 1024**2)

    @classmethod
    def owns(cls, name):
        return any(name == prefix or name.startswith(prefix + ".") for prefix in cls.PREFIXES)

    def verify(self):
        if json_pinned(self.path, self.sha256) != self.manifest:
            raise ValueError("source closure manifest changed")
        for name, row in self.modules.items():
            if read_pinned(row["path"], row["sha256"]) != self.buffers[name]:
                raise ValueError("source buffer identity changed")
        for name, row in self.assets.items():
            if read_pinned(row["path"], row["sha256"], max_bytes=64 * 1024**2) != self.asset_buffers[name]:
                raise ValueError("asset buffer identity changed")
        for name in self.loaded:
            module = sys.modules.get(name)
            if module is None or module.__loader__ is not self or getattr(module, "__reviewed_sha256__", None) != self.modules[name]["sha256"]:
                raise ValueError("loaded source identity changed")
        return True

    def __enter__(self):
        if self.installed or any(self.owns(name) for name in sys.modules):
            raise ValueError("stale project module or reused source loader")
        self.verify();sys.meta_path.insert(0, self);self.installed = True
        return self

    def __exit__(self, *ignored):
        if self in sys.meta_path:sys.meta_path.remove(self)
        for name in tuple(sys.modules):
            if self.owns(name):sys.modules.pop(name, None)
        self.installed = False

    def find_spec(self, fullname, path=None, target=None):
        if not self.owns(fullname):return None
        if not self.installed or fullname not in self.modules:
            raise ImportError("unreviewed project import refused")
        row = self.modules[fullname]
        return importlib.util.spec_from_loader(fullname, self, origin=row["path"], is_package=row["package"])

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        name = module.__name__;row = self.modules[name]
        if name in self.loaded or not self.installed:
            raise ImportError("project module reload refused")
        module.__file__ = row["path"];module.__cached__ = None
        module.__reviewed_sha256__ = row["sha256"]
        # Empty search path ensures the map is the only package source.
        if row["package"]:module.__path__ = []
        try:
            exec(compile(self.buffers[name], row["path"], "exec", dont_inherit=True), module.__dict__)
            read_pinned(row["path"], row["sha256"])
            self.loaded.add(name)
        except BaseException:
            sys.modules.pop(name, None)
            raise


def execute(manifest_path, manifest_sha256, plan_path, plan_sha256):
    """Trusted exact-buffer bootstrap entry; returns Gold-free numeric metadata."""
    import importlib
    with SourceClosure(manifest_path, manifest_sha256) as closure:
        operator = importlib.import_module("validation.knowledge.study_driver.operator_workflow")
        return operator.run_operator(plan_path, plan_sha256, closure=closure)
