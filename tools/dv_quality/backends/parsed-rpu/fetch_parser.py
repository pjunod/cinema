"""Download only the two pinned renderer sources into a scratch directory.

No Git checkout, repository credentials, installation or host library changes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import urllib.request

MAX_ARCHIVE_BYTES = 16 * 1024 * 1024
MAX_EXPANDED_BYTES = 96 * 1024 * 1024
SOURCES = ({
    "component": "dovi_tool", "revision": "83e1fdad6dcd5995556235946e7c5c0f9010d5a1",
    "filename": "dovi-source.tar.gz",
    "url": "https://codeload.github.com/quietvoid/dovi_tool/tar.gz/83e1fdad6dcd5995556235946e7c5c0f9010d5a1",
    "sha256": "57d13f03d04a7a1b3d7dcb139b976dc3748c745c1720d4b6e44b61e5ef67b69b",
},)


def fetch(root, source):
    archive = root / source["filename"]
    if not archive.exists():
        with urllib.request.urlopen(source["url"], timeout=60) as response:
            payload = response.read(MAX_ARCHIVE_BYTES + 1)
        if len(payload) > MAX_ARCHIVE_BYTES:
            raise ValueError("Source archive exceeds the bounded download size")
        archive.write_bytes(payload)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != source["sha256"]:
        raise ValueError(f"Source hash mismatch: {source['component']}")
    with tarfile.open(archive) as contents:
        members = contents.getmembers()
        if len(members) > 10000 or sum(member.size for member in members) > MAX_EXPANDED_BYTES:
            raise ValueError("Source extraction exceeds bounded limits")
        for member in members:
            path = Path(member.name)
            expected_top = f"{source['component']}-{source['revision']}"
            if (path.is_absolute() or ".." in path.parts or not path.parts or
                    path.parts[0] != expected_top or
                    not (member.isfile() or member.isdir())):
                raise ValueError(f"Unsafe source archive member: {member.name}")
            target = root / path
            if any(parent.is_symlink() for parent in
                   (target, *target.parents) if parent != root.parent):
                raise ValueError("Source extraction destination has a symlink")
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with contents.extractfile(member) as incoming, target.open("wb") as outgoing:
                    shutil.copyfileobj(incoming, outgoing)
    return root / f"{source['component']}-{source['revision']}"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    arguments = parser.parse_args()
    scratch = arguments.root.resolve()
    scratch.mkdir(parents=True, exist_ok=True)
    library = fetch(scratch, SOURCES[0])
    (library / "Cargo.toml").write_text((library / "Cargo.toml").read_text() +
        '\n[workspace]\nmembers = ["dolby_vision"]\n')
    bundle = Path(__file__).resolve().parent
    shutil.copyfile(bundle / "resolved-Cargo.lock", library / "Cargo.lock")
    url = "https://raw.githubusercontent.com/erazortt/DoViBaker/ffba39830b694ddca0bf5f73dcf1b462713bb7f4/include/dovi/rpu_parser.h"
    with urllib.request.urlopen(url, timeout=60) as response:
        header = response.read(256 * 1024 + 1)
    if hashlib.sha256(header).hexdigest() != "77e130d174a285ec4d141a88245f12b068606174a745f39f7aca54a2c1439f82":
        raise ValueError("C API header hash mismatch")
    destination = scratch / "include/libdovi"
    destination.mkdir(parents=True)
    (destination / "rpu_parser.h").write_bytes(header)
