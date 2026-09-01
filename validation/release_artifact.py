"""Create and verify an exact, version-aware Plurx release artifact."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
from typing import Any


SCHEMA = 1
BINARIES = ("plurxd", "plurx-cluster-check")
SUPPORTED_BINARY_SETS = frozenset((("plurxd",), BINARIES))
GIT_OBJECT = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")
SHA256 = re.compile(r"[0-9a-f]{64}")
ELF64 = 2
ELF_LITTLE_ENDIAN = 1
TARGET_MACHINES = {
    "x86_64-unknown-linux-gnu": 62,
    "aarch64-unknown-linux-gnu": 183,
}


def _digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def _require_text(value: object, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"release artifact {field} must be a non-empty string")
    return value


def _require_git_object(value: object, field: str) -> str:
    value = _require_text(value, field)
    if GIT_OBJECT.fullmatch(value) is None:
        raise ValueError(f"release artifact {field} must be a Git object id")
    return value


def _require_target(value: object, field: str) -> str:
    target = _require_text(value, field)
    if target not in TARGET_MACHINES:
        raise ValueError(f"release artifact {field} is unsupported: {target}")
    return target


def _require_binary_machine(path: Path, target: str) -> None:
    """Prove a candidate is a little-endian ELF64 for its declared target."""

    if path.is_symlink():
        raise ValueError(f"release artifact binary {path.name} must not be a symlink")
    try:
        with path.open("rb") as source:
            header = source.read(20)
    except OSError as error:
        raise ValueError(f"cannot read release artifact binary {path.name}: {error}") from error
    if (
        len(header) < 20
        or header[:4] != b"\x7fELF"
        or header[4] != ELF64
        or header[5] != ELF_LITTLE_ENDIAN
    ):
        raise ValueError(
            f"release artifact binary {path.name} must be a little-endian ELF64"
        )
    machine = int.from_bytes(header[18:20], byteorder="little")
    expected = TARGET_MACHINES[target]
    if machine != expected:
        raise ValueError(
            f"release artifact binary {path.name} machine mismatch: "
            f"target {target} requires {expected}, got {machine}"
        )


def _require_binary_set(binary_names: tuple[str, ...]) -> tuple[str, ...]:
    if binary_names not in SUPPORTED_BINARY_SETS:
        raise ValueError(
            "release artifact binaries must match a supported tagged runtime"
        )
    return binary_names


def create(
    directory: Path,
    *,
    git_tree: str,
    git_commit: str,
    build_ref: str,
    rustc: str,
    target: str,
    binary_names: tuple[str, ...] = BINARIES,
) -> dict[str, Any]:
    """Write sidecars and a manifest for the exact shipped binary set."""

    git_tree = _require_git_object(git_tree, "git_tree")
    git_commit = _require_git_object(git_commit, "git_commit")
    build_ref = _require_text(build_ref, "build_ref")
    rustc = _require_text(rustc, "rustc")
    target = _require_target(target, "target")
    binary_names = _require_binary_set(binary_names)

    binaries: dict[str, dict[str, str]] = {}
    for name in binary_names:
        path = directory / name
        if not path.is_file():
            raise ValueError(f"release artifact is missing binary {name}")
        _require_binary_machine(path, target)
        digest = _digest(path)
        (directory / f"{name}.sha256").write_text(
            f"{digest}  {name}\n", encoding="utf-8"
        )
        binaries[name] = {"sha256": digest}

    manifest: dict[str, Any] = {
        "schema": SCHEMA,
        "git_tree": git_tree,
        "git_commit": git_commit,
        "build_ref": build_ref,
        "rustc": rustc,
        "target": target,
        "binaries": binaries,
    }
    (directory / "build-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return manifest


def verify(
    directory: Path,
    *,
    git_tree: str,
    git_commit: str,
    build_ref: str,
    target: str,
    binary_names: tuple[str, ...] = BINARIES,
) -> dict[str, Any]:
    """Verify identity, sidecars, and bytes before packaging an image."""

    expected_tree = _require_git_object(git_tree, "expected git_tree")
    expected_commit = _require_git_object(git_commit, "expected git_commit")
    expected_ref = _require_text(build_ref, "expected build_ref")
    expected_target = _require_target(target, "expected target")
    binary_names = _require_binary_set(binary_names)

    expected_files = {
        "build-manifest.json",
        *(name for name in binary_names),
        *(f"{name}.sha256" for name in binary_names),
    }
    try:
        actual_files = {path.name for path in directory.iterdir()}
    except OSError as error:
        raise ValueError(f"cannot inspect release artifact directory: {error}") from error
    if actual_files != expected_files:
        missing = sorted(expected_files - actual_files)
        extra = sorted(actual_files - expected_files)
        raise ValueError(
            f"release artifact entry set mismatch: missing={missing}, extra={extra}"
        )

    manifest_path = directory / "build-manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read release artifact manifest: {error}") from error
    if not isinstance(manifest, dict):
        raise ValueError("release artifact manifest must be a JSON object")
    if manifest.get("schema") != SCHEMA:
        raise ValueError("release artifact manifest has an unsupported schema")

    expected_identity = {
        "git_tree": expected_tree,
        "git_commit": expected_commit,
        "build_ref": expected_ref,
        "target": expected_target,
    }
    for field, expected in expected_identity.items():
        if manifest.get(field) != expected:
            raise ValueError(
                f"release artifact {field} mismatch: "
                f"expected {expected}, got {manifest.get(field)}"
            )
    _require_text(manifest.get("rustc"), "rustc")

    binaries = manifest.get("binaries")
    if not isinstance(binaries, dict) or set(binaries) != set(binary_names):
        raise ValueError(
            "release artifact manifest binary set does not match the tagged runtime"
        )
    for name in binary_names:
        record = binaries.get(name)
        if not isinstance(record, dict) or set(record) != {"sha256"}:
            raise ValueError(f"release artifact manifest has invalid record for {name}")
        expected_digest = record.get("sha256")
        if not isinstance(expected_digest, str) or SHA256.fullmatch(expected_digest) is None:
            raise ValueError(f"release artifact manifest has invalid digest for {name}")
        path = directory / name
        if not path.is_file():
            raise ValueError(f"release artifact is missing binary {name}")
        actual_digest = _digest(path)
        if actual_digest != expected_digest:
            raise ValueError(f"release artifact digest mismatch for {name}")
        _require_binary_machine(path, expected_target)
        sidecar = directory / f"{name}.sha256"
        try:
            sidecar_value = sidecar.read_text(encoding="utf-8")
        except OSError as error:
            raise ValueError(f"cannot read release artifact sidecar for {name}") from error
        if sidecar_value != f"{expected_digest}  {name}\n":
            raise ValueError(f"release artifact sidecar mismatch for {name}")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("create", "verify"):
        action = subparsers.add_parser(command)
        action.add_argument("--directory", type=Path, required=True)
        action.add_argument("--git-tree", required=True)
        action.add_argument("--git-commit", required=True)
        action.add_argument("--build-ref", required=True)
        action.add_argument("--target", required=True)
        action.add_argument("--binary", action="append", required=True)
        if command == "create":
            action.add_argument("--rustc", required=True)
    args = parser.parse_args()
    common = {
        "git_tree": args.git_tree,
        "git_commit": args.git_commit,
        "build_ref": args.build_ref,
        "target": args.target,
    }
    if args.command == "create":
        create(
            args.directory,
            rustc=args.rustc,
            binary_names=tuple(args.binary),
            **common,
        )
    else:
        verify(args.directory, binary_names=tuple(args.binary), **common)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
