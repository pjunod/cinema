#!/usr/bin/env python3
"""Developer-only metadata extraction for one authenticated legacy checkpoint.

Production never runs Python or parses pickle. This tool requires the exact
compiled publisher SHA/length before a closed unpickler can inspect metadata.
Only inert storage/tensor descriptors and OrderedDict construction are allowed;
unknown globals, classes, device locations, views, strides and dtypes refuse.
"""
import argparse
from collections import OrderedDict
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import pickle
import struct
import sys

SHA256 = "33ff438ec37160a105f0700819a5b78a07918e1913fc2f249184b1f46a248e4e"
BYTES = 310385901
MAGIC = 119547037146038801333356
MAX_METADATA = 1024 * 1024


@dataclass(frozen=True)
class Storage:
    key: str
    count: int


@dataclass(frozen=True)
class Tensor:
    storage: Storage
    offset: int
    shape: tuple
    strides: tuple


def integer(value):
    if type(value) is not int or value < 0 or value > BYTES:
        raise ValueError("invalid bounded metadata integer")
    return value


def rebuild(storage, offset, shape, strides, *rest):
    if not isinstance(storage, Storage) or type(shape) is not tuple or type(strides) is not tuple:
        raise ValueError("unsupported tensor metadata")
    if len(shape) != len(strides) or not 1 <= len(shape) <= 4:
        raise ValueError("unsupported tensor rank")
    shape = tuple(integer(v) for v in shape)
    strides = tuple(integer(v) for v in strides)
    expected = 1
    for size, stride in zip(reversed(shape), reversed(strides)):
        if size == 0 or stride != expected:
            raise ValueError("noncontiguous tensor")
        expected *= size
    if integer(offset) + expected > storage.count:
        raise ValueError("tensor outside storage")
    return Tensor(storage, offset, shape, strides)


class Metadata(pickle.Unpickler):
    def __init__(self, stream, storages):
        super().__init__(stream)
        self.storages = storages

    def find_class(self, module, name):
        if (module, name) == ("collections", "OrderedDict"):
            return OrderedDict
        if module == "torch._utils" and name in ("_rebuild_tensor", "_rebuild_tensor_v2"):
            return rebuild
        if (module, name) == ("torch", "FloatStorage"):
            return "float32-le"
        raise ValueError(f"unreviewed pickle global: {module}.{name}")

    def persistent_load(self, value):
        if not isinstance(value, tuple) or len(value) != 6:
            raise ValueError("unsupported storage reference")
        kind, dtype, key, location, count, view = value
        if kind != "storage" or dtype != "float32-le" or location != "cpu" or view is not None:
            raise ValueError("unsupported storage type/location/view")
        if type(key) is not str or not key.isascii() or not key.isdigit() or len(key) > 32:
            raise ValueError("unsupported storage key")
        current = Storage(key, integer(count))
        if current.count == 0 or self.storages.setdefault(key, current) != current:
            raise ValueError("inconsistent storage identity")
        return current


def layout(path):
    with path.open("rb") as stream:
        before = path.stat()
        if before.st_size != BYTES:
            raise ValueError("publisher byte length mismatch")
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if digest != SHA256:
            raise ValueError("publisher SHA256 mismatch")
        stream.seek(0)
        storages = {}

        def load():
            value = Metadata(stream, storages).load()
            if stream.tell() > MAX_METADATA:
                raise ValueError("metadata budget exceeded")
            return value

        if load() != MAGIC or load() != 1001:
            raise ValueError("unsupported legacy header")
        info = load()
        if info != {"protocol_version": 1001, "little_endian": True,
                    "type_sizes": {"short": 2, "int": 4, "long": 4}}:
            raise ValueError(f"unreviewed serialization system header: {info!r}")
        tensors = load()
        order = load()
        if type(tensors) not in (dict, OrderedDict) or not 1 <= len(tensors) <= 1024:
            raise ValueError("unsupported state dictionary")
        if type(order) is not list or set(order) != set(storages) or len(order) != len(storages):
            raise ValueError("storage inventory mismatch")
        if len(storages) != 256 or len(tensors) != 258:
            raise ValueError("unreviewed publisher tensor inventory")
        metadata_end = stream.tell()
        raw = []
        offsets = {}
        for key in order:
            count = struct.unpack("<Q", stream.read(8))[0]
            if count != storages[key].count:
                raise ValueError("raw storage count mismatch")
            offset = stream.tell()
            if offset + count * 4 > BYTES:
                raise ValueError("raw storage outside file")
            offsets[key] = len(raw)
            raw.append({"offset": offset, "elements": count, "dtype": "f32le"})
            stream.seek(count * 4, 1)
        if stream.tell() != BYTES:
            raise ValueError("unconsumed model bytes")
        descriptors = []
        for name, tensor in sorted(tensors.items()):
            if type(name) is not str or not name.isascii() or len(name) > 160 or not isinstance(tensor, Tensor):
                raise ValueError("unexpected state dictionary entry")
            descriptors.append({"name": name, "storage": offsets[tensor.storage.key],
                                "offset": tensor.offset, "shape": list(tensor.shape),
                                "strides": list(tensor.strides)})
        after = path.stat()
        held = os.fstat(stream.fileno())
        identity = lambda stat: (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)
        if identity(before) != identity(after) or identity(before) != identity(held):
            raise ValueError("authenticated checkpoint changed during extraction")
        return {"version": 1, "model_sha256": SHA256, "model_bytes": BYTES,
                "format": "pytorch-legacy-1001-f32le", "metadata_end": metadata_end,
                "storages": raw, "tensors": descriptors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--check", action="store_true", help="compare deterministic metadata without writing")
    args = parser.parse_args()
    encoded = (json.dumps(layout(args.checkpoint), indent=2) + "\n").encode()
    if args.check:
        if args.output.read_bytes() != encoded:
            raise ValueError("compiled layout differs from authenticated checkpoint")
    else:
        args.output.write_bytes(encoded)
    print(f"Authenticated legacy layout: {len(encoded)} bytes, SHA256 {hashlib.sha256(encoded).hexdigest()}", file=sys.stderr)


if __name__ == "__main__":
    main()
