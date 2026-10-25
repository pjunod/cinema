#!/usr/bin/env python3
"""Commit a never-published test-only public-root/version overlay, then archive its exact source.

The private fixture key stays outside both git trees and source archives. This does
not add a production runtime trust override or alter the candidate checkout.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
from common import git_environment, hosted_guard, run, sha256, write_json


def fixture_root_overlay(raw, public_hex):
    """Validate candidate roots before replacing them only in a disposable tree."""
    if len(raw) > 65536:
        raise ValueError("candidate trust roots exceed the size bound")
    def pairs(items):
        result = {}
        for name, value in items:
            if name in result:
                raise ValueError("duplicate candidate trust-root field")
            result[name] = value
        return result
    original = json.loads(raw, object_pairs_hook=pairs)
    if not isinstance(original, dict) or set(original) != {"schema_version", "repository", "keys"} or type(original["schema_version"]) is not int or original["schema_version"] != 1 or original["repository"] != "pjunod/cinema" or not isinstance(original["keys"], list) or len(original["keys"]) > 16:
        raise ValueError("invalid candidate trust-root document")
    ids, keys = set(), set()
    for key in original["keys"]:
        if not isinstance(key, dict) or set(key) != {"id", "public_key_hex"} or not isinstance(key["id"], str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", key["id"]) or not isinstance(key["public_key_hex"], str) or not re.fullmatch(r"[0-9a-f]{64}", key["public_key_hex"]) or key["public_key_hex"] == "00" * 32 or key["id"] in ids or key["public_key_hex"] in keys:
            raise ValueError("invalid or duplicate candidate trust key")
        ids.add(key["id"])
        keys.add(key["public_key_hex"])
    if not isinstance(public_hex, str) or not re.fullmatch(r"[0-9a-f]{64}", public_hex) or public_hex == "00" * 32 or public_hex in keys:
        raise ValueError("qualification needs a distinct disposable public key")
    fixture = {"schema_version": 1, "repository": "pjunod/cinema",
               "keys": [{"id": "qualification-local-only", "public_key_hex": public_hex}]}
    provenance = {"candidate_trust_roots": original,
                  "candidate_trust_roots_sha256": hashlib.sha256(raw).hexdigest()}
    return fixture, provenance


def build_overlay(source, candidate, destination, version, public_hex, cargo):
    destination.mkdir()
    archive = destination / "candidate.tar"
    with archive.open("wb") as stream:
        subprocess.run(["git", "-C", str(source), "archive", candidate], stdout=stream, check=True, env=git_environment())
    tree = destination / "committed-source"
    tree.mkdir()
    run(["tar", "-xf", archive, "-C", tree], timeout=120)
    archive.unlink()
    run(["git", "init", "-q", tree])
    run(["git", "-C", tree, "config", "user.name", "Plurx disposable qualification"])
    run(["git", "-C", tree, "config", "user.email", "qualification@example.invalid"])
    # The snapshot commit has no new behavior. The subsequent trust/version commit runs the normal tracked hook.
    run(["git", "-C", tree, "add", "--all", "--force"], timeout=120)
    run(["git", "-C", tree, "commit", "-q", "-m", f"chore(desktop): retain exact candidate snapshot {candidate}"], timeout=120)
    snapshot = run(["git", "-C", tree, "rev-parse", "HEAD"]).stdout.strip()
    original_tree = run(["git", "-C", source, "rev-parse", candidate + "^{tree}"]).stdout.strip()
    if run(["git", "-C", tree, "rev-parse", "HEAD^{tree}"]).stdout.strip() != original_tree:
        raise RuntimeError("archived fixture snapshot does not preserve the exact committed candidate tree")
    root_file = tree / "crates/plurx-desktop-control/production-trust-roots.json"
    fixture_roots, root_provenance = fixture_root_overlay(root_file.read_bytes(), public_hex)
    root_file.write_text(json.dumps(fixture_roots, indent=2) + "\n")
    manifest = tree / "Cargo.toml"
    text = manifest.read_text()
    text, count = re.subn(r'(\[workspace\.package\]\n(?:[^\[]*?))version = "[0-9]+\.[0-9]+\.[0-9]+"',
                           lambda m: m[1] + f'version = "{version}"', text, count=1)
    if count != 1:
        raise RuntimeError("could not identify the single workspace package version")
    manifest.write_text(text)
    run([cargo, "update", "--workspace", "--offline", "--manifest-path", manifest], timeout=180)
    driver_manifest = tree / "tests/desktop/stage-driver/Cargo.toml"
    run([cargo, "update", "--workspace", "--offline", "--manifest-path", driver_manifest], timeout=180)
    hooks = tree / ".git/hooks/pre-commit"
    shutil.copy2(tree / "scripts/pre-commit", hooks)
    hooks.chmod(0o755)
    run(["git", "-C", tree, "add", "Cargo.toml", "Cargo.lock", root_file.relative_to(tree), "tests/desktop/stage-driver/Cargo.lock"], timeout=60)
    # Normal hooks are mandatory; no --no-verify, skipped checks or global cfg(test).
    run(["git", "-C", tree, "commit", "-m", "fix(desktop): isolate disposable local qualification root and version",
         "-m", "TEST BUILD ONLY. Never publish or merge this fixture root.",
         "-m", "Regression-Test: tests/operations/test_desktop_release.py::test_production_signer_requires_pinned_identical_candidate_roots"],
        timeout=1800)
    commit = run(["git", "-C", tree, "rev-parse", "HEAD"]).stdout.strip()
    tree_hash = run(["git", "-C", tree, "rev-parse", "HEAD^{tree}"]).stdout.strip()
    diff = destination / "overlay.patch"
    diff.write_text(run(["git", "-C", tree, "diff", "--binary", snapshot, commit]).stdout)
    source_archive = destination / "committed-overlay.tar"
    with source_archive.open("wb") as stream:
        subprocess.run(["git", "-C", str(tree), "archive", commit], stdout=stream, check=True, env=git_environment())
    build = destination / "build-source"
    build.mkdir()
    run(["tar", "-xf", source_archive, "-C", build], timeout=120)
    receipt = {"schema_version": 1, "scope": "isolated-local-trust-overlay", "candidate": candidate,
               "version": version, "committed_overlay_sha": commit, "committed_overlay_tree": tree_hash,
               "diff_sha256": sha256(diff), "source_archive_sha256": sha256(source_archive),
               "public_key_hex": public_hex, "fixture_root_id": "qualification-local-only",
               "never_publish_or_merge": True, "build_source": str(build), **root_provenance}
    write_json(destination / "overlay.json", receipt)
    return receipt


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--public-key-hex", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--cargo", default="cargo")
    a = p.parse_args()
    hosted_guard(a.candidate)
    if not re.fullmatch(r"[0-9a-f]{64}", a.public_key_hex) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", a.version):
        p.error("expected a raw32-byte public key and stable semantic version")
    if run(["git", "-C", a.source, "rev-parse", "HEAD"]).stdout.strip() != a.candidate:
        p.error("source checkout must be the exact dispatched committed candidate")
    if a.output.exists():
        p.error("overlay output must be fresh")
    build_overlay(a.source.resolve(), a.candidate, a.output.resolve(), a.version, a.public_key_hex, a.cargo)


if __name__ == "__main__":
    main()
