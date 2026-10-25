#!/usr/bin/env python3
"""Sign only a native selected cohort for isolated local staging, never a production feed."""
import argparse
import json
from pathlib import Path
import tarfile
import time
from common import hosted_guard, run, sha256, write_json


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--baseline-overlay", type=Path, required=True)
    p.add_argument("--target-overlay", type=Path, required=True)
    p.add_argument("--assets", type=Path, required=True)
    p.add_argument("--signing-key", type=Path, required=True)
    p.add_argument("--platform", choices=["linux", "macos"], required=True)
    p.add_argument("--arch", choices=["amd64", "arm64", "universal"], required=True)
    p.add_argument("--openssl", default="openssl")
    a = p.parse_args()
    hosted_guard(a.candidate)
    base = json.loads(a.baseline_overlay.read_bytes())
    target = json.loads(a.target_overlay.read_bytes())
    for overlay in (base, target):
        if overlay["candidate"] != a.candidate or overlay["scope"] != "isolated-local-trust-overlay":
            raise RuntimeError("fixture provenance differs from dispatched candidate")
    # DER SubjectPublicKeyInfo ends in the raw32-byte Ed25519 public key.
    import subprocess
    public = subprocess.run([a.openssl, "pkey", "-in", str(a.signing_key), "-pubout", "-outform", "DER"],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout
    if len(public) != 44 or public[:12].hex() != "302a300506032b6570032100":
        raise RuntimeError("fixture signer must be Ed25519")
    if public[12:].hex() != base["public_key_hex"] or base["public_key_hex"] != target["public_key_hex"]:
        raise RuntimeError("fixture signing key does not match both committed overlays")
    if a.signing_key.resolve().is_relative_to(a.assets.resolve()):
        raise RuntimeError("never put a private fixture key among staged artifacts or retained receipts")
    version = target["version"]
    artifacts = []
    if a.platform == "linux":
        if a.arch == "universal":
            raise RuntimeError("Linux requires a concrete native architecture")
        for component, package in (("server", "plurxd"), ("desktop", "plurx-desktop")):
            name = f"{package}_{version}-1_{a.arch}.deb"
            path = a.assets / name
            actual = run(["dpkg-deb", "-f", path, "Package", "Version", "Architecture"]).stdout
            expected = {"Package": package, "Version": f"{version}-1", "Architecture": a.arch}
            fields = dict(line.split(": ", 1) for line in actual.splitlines())
            if fields != expected:
                raise RuntimeError(f"actual fixture package identity differs: {fields}")
            artifacts.append({"name": name, "platform": "linux", "architecture": a.arch, "format": "deb",
                "component": component, "package_name": package, "package_version": f"{version}-1",
                "size": path.stat().st_size, "sha256": sha256(path)})
    else:
        if a.arch != "universal":
            raise RuntimeError("native macOS layout uses the canonical universal package identity")
        name = f"plurxd-{version}-macos-universal.tar.gz"
        path = a.assets / name
        wrapper = name.removesuffix(".tar.gz")
        with tarfile.open(path) as stream:
            release = json.load(stream.extractfile(wrapper + "/release.json"))
            if release != {"schema_version": 1, "repository": "pjunod/cinema", "version": version,
                           "source_commit": target["committed_overlay_sha"], "architecture": "universal"}:
                raise RuntimeError("fixture native archive release identity differs from committed overlay")
        artifacts.append({"name": name, "platform": "macos", "architecture": "universal", "format": "tar_gz",
            "component": "native_runtime", "package_name": "plurx-native", "package_version": version,
            "size": path.stat().st_size, "sha256": sha256(path)})
    now = int(time.time())
    manifest = {"schema_version": 1, "repository": "pjunod/cinema", "channel": "stable", "tag": f"v{version}",
        "version": version, "sequence": now, "source_commit": target["committed_overlay_sha"],
        "source_tree": target["committed_overlay_tree"], "packaging_commit": a.candidate,
        "created_unix": now, "expires_unix": now + 3600, "upgrade_from": [f"={base['version']}"],
        "data_compatibility": "qualification-same-source-data-v1", "cluster_compatibility": "qualification-same-source-cluster-v1",
        "minimum_controller": base["version"], "rollback": "manual_recovery",
        "release_notes_url": f"https://github.com/pjunod/cinema/releases/tag/v{version}", "artifacts": artifacts}
    path = a.assets / "plurx-update-v1.json"
    if path.exists() or path.with_name(path.name + ".sig").exists():
        raise RuntimeError("refusing to replace an existing local signed fixture")
    path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n")
    signature = path.with_name(path.name + ".sig")
    run([a.openssl, "pkeyutl", "-sign", "-rawin", "-inkey", a.signing_key, "-in", path, "-out", signature])
    if signature.stat().st_size != 64:
        raise RuntimeError("fixture signature must be raw64-byte Ed25519")
    print(json.dumps({"scope": "local-selected-cohort-fixture-never-publish", "candidate": a.candidate,
                      "manifest_sha256": sha256(path), "signature_sha256": sha256(signature), "artifacts": artifacts}))


if __name__ == "__main__":
    main()
