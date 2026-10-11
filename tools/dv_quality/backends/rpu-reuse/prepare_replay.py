"""Prepare new offline scratch from verified public source archives and fixture inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import tomllib


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def files(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("dependency symlink refused")
        if path.is_file():
            result[str(path.relative_to(root))] = sha(path)
    return result


def copy_checked(source, destination, checksum):
    if source.is_symlink() or not source.is_file() or sha(source) != checksum:
        raise ValueError("source/input identity mismatch")
    shutil.copyfile(source, destination)
    if sha(destination) != checksum:
        raise ValueError("copied identity mismatch")


def extract(archive, root, top, limit):
    with tarfile.open(archive) as source:
        members = source.getmembers()
        if len(members) > 12000 or sum(member.size for member in members) > limit:
            raise ValueError("source extraction cap")
        for member in members:
            relative = Path(member.name)
            if (relative.is_absolute() or ".." in relative.parts or not relative.parts or
                    relative.parts[0] != top or not (member.isfile() or member.isdir())):
                raise ValueError("unsafe pinned source member")
            target = root / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as incoming, target.open("xb") as outgoing:
                    shutil.copyfileobj(incoming, outgoing)
                target.chmod(member.mode & 0o755)


def prepare(scratch, inputs, cargo_cache):
    bundle = Path(__file__).resolve().parent
    lock = json.loads((bundle / "input-lock.json").read_text())
    for name, checksum in lock.items():
        copy_checked(inputs / name, scratch / name, checksum)
    shutil.copyfile(bundle / "input-lock.json", scratch / "input-lock.json")
    expected = json.loads((bundle / "cargo-cache-lock.json").read_text())
    actual = files(cargo_cache)
    if actual != expected:
        raise ValueError("offline Cargo cache identity mismatch")
    # Check archived crates against the repository-pinned resolved lock, not cache claims.
    packages = tomllib.loads((bundle / "resolved-Cargo.lock").read_text())["package"]
    checksums = {f"{p['name']}-{p['version']}.crate": p["checksum"]
                 for p in packages if "checksum" in p}
    for name, checksum in actual.items():
        if name.endswith(".crate") and checksums.get(Path(name).name) != checksum:
            raise ValueError("crate archive does not match pinned Cargo checksum")
    shutil.copytree(cargo_cache, scratch / "cargo-home/registry")
    if files(scratch / "cargo-home/registry") != expected:
        raise ValueError("copied offline cache identity mismatch")
    ffmpeg = "FFmpeg-bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa"
    libdovi = "dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1"
    extract(scratch / "ffmpeg-source.tar.gz", scratch, ffmpeg, 100 * 1024 * 1024)
    extract(scratch / "dovi-source.tar.gz", scratch, libdovi, 96 * 1024 * 1024)
    manifest = scratch / libdovi / "Cargo.toml"
    manifest.write_text(manifest.read_text() + '\n[workspace]\nmembers = ["dolby_vision"]\n')
    shutil.copyfile(bundle / "resolved-Cargo.lock", scratch / libdovi / "Cargo.lock")
    for source, target in (("generate_reuse.rs", "m0_reuse.rs"), ("inspect_reuse.rs", "m0_inspect_reuse.rs")):
        shutil.copyfile(bundle / source, scratch / libdovi / "dolby_vision/examples" / target)
    for name in ("build_ffmpeg.sh", "generate.sh", "mux_reuse.c", "decode_reuse.c", "cache_probe.c",
                 "run_controls.sh", "decode_cases.sh", "record_provenance.py", "check_reuse.py", "test_driver.py", "execute_stage.py"):
        shutil.copyfile(bundle / name, scratch / name)
    return {"schema": 1, "inputs": lock, "cargo_cache_manifest_sha256": sha(bundle / "cargo-cache-lock.json"),
            "libdovi_workspace_patch": "append workspace containing dolby_vision",
            "cargo_lock_sha256": sha(bundle / "resolved-Cargo.lock"), "ffmpeg_patch": "none"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scratch", type=Path)
    parser.add_argument("inputs", type=Path)
    parser.add_argument("cargo_cache", type=Path)
    args = parser.parse_args()
    print(json.dumps(prepare(args.scratch, args.inputs, args.cargo_cache), indent=2, allow_nan=False))
