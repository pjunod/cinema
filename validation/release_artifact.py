"""Create and verify the exact two-binary Plurx release artifact."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
from typing import Any


SCHEMA = 1
BINARIES = ("plurxd", "plurx-cluster-check")
GIT_OBJECT = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")
SHA256 = re.compile(r"[0-9a-f]{64}")


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


def create(
    directory: Path,
    *,
    git_tree: str,
    git_commit: str,
    build_ref: str,
    rustc: str,
    target: str,
) -> dict[str, Any]:
    """Write sidecars and a manifest for the exact shipped binary set."""

    git_tree = _require_git_object(git_tree, "git_tree")
    git_commit = _require_git_object(git_commit, "git_commit")
    build_ref = _require_text(build_ref, "build_ref")
    rustc = _require_text(rustc, "rustc")
    target = _require_text(target, "target")

    binaries: dict[str, dict[str, str]] = {}
    for name in BINARIES:
        path = directory / name
        if not path.is_file():
            raise ValueError(f"release artifact is missing binary {name}")
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
) -> dict[str, Any]:
    """Verify identity, sidecars, and bytes before packaging an image."""

    expected_tree = _require_git_object(git_tree, "expected git_tree")
    expected_commit = _require_git_object(git_commit, "expected git_commit")
    expected_ref = _require_text(build_ref, "expected build_ref")
    expected_target = _require_text(target, "expected target")

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
    if not isinstance(binaries, dict) or set(binaries) != set(BINARIES):
        raise ValueError("release artifact manifest must name exactly both binaries")
    for name in BINARIES:
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
        create(args.directory, rustc=args.rustc, **common)
    else:
        verify(args.directory, **common)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
