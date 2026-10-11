"""Safely unpack the checksum-bound retained prerequisite into a NEW directory."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile

MAX_ARCHIVE = 128 * 1024 * 1024
MAX_EXPANDED = 160 * 1024 * 1024


def unpack(archive, destination, record):
    archive, destination = Path(archive), Path(destination)
    if destination.exists() or destination.is_symlink():
        raise ValueError("new prerequisite directory required")
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size > MAX_ARCHIVE:
        raise ValueError("bounded regular prerequisite archive required")
    if archive.stat().st_size != record["archive_bytes"] or hashlib.sha256(archive.read_bytes()).hexdigest() != record["archive_sha256"]:
        raise ValueError("prerequisite archive identity mismatch")
    with tarfile.open(archive) as source:
        members = source.getmembers()
        if len(members) > 1000 or sum(member.size for member in members) > MAX_EXPANDED:
            raise ValueError("prerequisite extraction cap")
        names = [member.name for member in members]
        if len(set(names)) != len(names):
            raise ValueError("duplicate prerequisite member")
        for member in members:
            path = Path(member.name)
            if (path.is_absolute() or ".." in path.parts or not path.parts or
                    path.parts[0] not in ("inputs", "registry") or not member.isfile()):
                raise ValueError("unsafe prerequisite member")
        if set(names) != set(record["files"]):
            raise ValueError("prerequisite member coverage differs")
        destination.mkdir()
        for member in members:
            target = destination / member.name
            target.parent.mkdir(parents=True, exist_ok=True)
            with source.extractfile(member) as incoming, target.open("xb") as output:
                shutil.copyfileobj(incoming, output)
            if hashlib.sha256(target.read_bytes()).hexdigest() != record["files"][member.name]:
                raise ValueError("extracted prerequisite identity mismatch")
    return {"archive_sha256": record["archive_sha256"], "verified_files": len(names),
            "image_id": record["image_id"], "bootstrap": "retained prerequisite unpack, not source-only bootstrap"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("new_directory", type=Path)
    args = parser.parse_args()
    metadata = json.loads((Path(__file__).resolve().parent / "prerequisites.json").read_text())
    print(json.dumps(unpack(args.archive, args.new_directory, metadata), indent=2, allow_nan=False))
