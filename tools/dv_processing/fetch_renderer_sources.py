"""Download the two pinned sources of the production DV helper's renderer.

The retained tools/dv_quality evidence keeps its own historical fetcher.

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
SOURCES = (
    {
        # The release the native player's reconstruction is built from
        # (clients/linux-player/packaging/upstream.lock): one renderer, one
        # sampling and NLQ contract on both sides.
        "component": "libplacebo",
        "revision": "v7.387.0",
        "filename": "libplacebo-source.tar.gz",
        "url": "https://code.videolan.org/videolan/libplacebo/-/archive/v7.387.0/libplacebo-v7.387.0.tar.gz",
        "sha256": "3bf5d72786682c194387c33d563872f6bfe202a9816f9dde6535a31a9b2e0d74",
    },
    {
        "component": "Vulkan-Headers",
        "revision": "74d8a6cb930c68ef617b202c3ff3c59d919e086b",
        "filename": "vulkan-headers-source.tar.gz",
        "url": "https://codeload.github.com/KhronosGroup/Vulkan-Headers/tar.gz/74d8a6cb930c68ef617b202c3ff3c59d919e086b",
        "sha256": "75967a8288ff16044a86d1af462b96d2e75c435937efb75f2bb4102ab0382f29",
    },
)


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
    headers = fetch(scratch, SOURCES[1])
    shutil.copytree(headers, library / "3rdparty/Vulkan-Headers", dirs_exist_ok=True)
    (scratch / "source-lock.json").write_text(json.dumps(list(SOURCES), indent=2, allow_nan=False) + "\n")
