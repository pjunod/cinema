#!/usr/bin/env python3
"""Build/repackage isolated native fixture cohorts, then run actual worker apply/recovery.

Run after unchanged-candidate qualification on the same disposable hosted VM.
Private keys, compiled payloads and local overlay repos stay outside receipt output.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
from common import Receipt, hosted_guard, run, sha256, write_json

HERE = Path(__file__).resolve().parent
BINS = ["plurx-control", "plurx-install-helper", "plurx-update-worker", "plurx-service-launch", "plurx-recovery"]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def repackage_debs(originals, output, binaries, overlay, arch):
    output.mkdir()
    for package in ("plurxd", "plurx-desktop"):
        matches = list(originals.glob(f"{package}_*_{arch}.deb"))
        if len(matches) != 1:
            raise RuntimeError("exact original native package pair required")
        root = output / (package + "-extracted")
        run(["dpkg-deb", "--raw-extract", matches[0], root], timeout=120)
        control = root / "DEBIAN/control"
        text = control.read_text()
        old_version = re.search(r"(?m)^Version: (.+)$", text)[1]
        version = overlay["version"] + "-1"
        text = re.sub(r"(?m)^Version: .+$", "Version: " + version, text)
        text = text.replace(f"plurxd (= {old_version})", f"plurxd (= {version})")
        control.write_text(text)
        if package == "plurxd":
            shutil.copy2(binaries / "plurxd", root / "usr/lib/plurx/plurxd")
            shutil.copy2(binaries / "plurxd.dwp", root / "usr/lib/plurx/plurxd.dwp")
            for name in BINS:
                shutil.copy2(binaries / name, root / "usr/lib/plurx-control" / name)
            write_json(root / "usr/lib/plurx-control/release.json", {
                "schema_version": 1, "repository": "pjunod/cinema", "version": overlay["version"],
                "source_commit": overlay["committed_overlay_sha"], "architecture": arch})
        # Rebuild package integrity metadata after replacing real compiled files.
        checksums = []
        size = 0
        for member in sorted(root.rglob("*")):
            if member.is_file() and not member.is_symlink() and "DEBIAN" not in member.relative_to(root).parts:
                with member.open("rb") as stream:
                    digest = hashlib.file_digest(stream, "md5").hexdigest()
                checksums.append(f"{digest}  {member.relative_to(root).as_posix()}\n")
                size += member.stat().st_size
        (root / "DEBIAN/md5sums").write_text("".join(checksums))
        text = re.sub(r"(?m)^Installed-Size: .+$", f"Installed-Size: {(size + 1023) // 1024}", control.read_text())
        control.write_text(text)
        destination = output / f"{package}_{version}_{arch}.deb"
        run(["dpkg-deb", "--build", "--root-owner-group", root, destination], timeout=180)
        shutil.rmtree(root)


def repackage_macos(original, output, binaries, overlay, source):
    output.mkdir()
    media = output / "media"
    shutil.copytree(original / "runtime", media)
    (media / "plurxd").unlink()
    shutil.rmtree(media / "subtitle-engine")
    app = output / "Plurx Server.app"
    shutil.copytree(original / "Plurx Server.app", app)
    info = app / "Contents/Info.plist"
    with info.open("rb") as stream:
        value = plistlib.load(stream)
    value["CFBundleShortVersionString"] = overlay["version"]
    with info.open("wb") as stream:
        plistlib.dump(value, stream)
    run(["codesign", "--force", "--sign", "-", "--options", "runtime", app])
    symbols = output / "plurxd.dSYM"
    run(["dsymutil", binaries / "plurxd", "-o", symbols], timeout=600)
    name = f"plurxd-{overlay['version']}-macos-universal"
    package = output / name
    run([sys.executable, source / "scripts/package-macos", "--daemon", binaries / "plurxd",
         "--ffmpeg-package", media, "--subtitle-engine-package", original / "runtime/subtitle-engine",
         "--debug-symbols", symbols, "--desktop-app", app, "--control-dir", binaries,
         "--version", overlay["version"], "--source-commit", overlay["committed_overlay_sha"],
         "--output", package], timeout=600)
    archive = output / (name + ".tar.gz")
    run([sys.executable, source / "scripts/package-macos", "--archive", package, "--output", archive], timeout=300)
    # Keep the direct complete root for explicit bootstrap; only the archive enters signed staging.
    return package


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--artifacts", type=Path, required=True)
    p.add_argument("--work", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--openssl", default="openssl")
    a = p.parse_args()
    with Receipt(a.output / "build-and-update.json", a.candidate, "native-local-trust-fixture-build-and-update") as receipt:
        hosted_guard(a.candidate, mutate=True)
        if a.work.exists() or a.work.resolve().is_relative_to(a.output.resolve()):
            raise RuntimeError("fixture work must be fresh and outside uploaded receipt output")
        # A new hosted VM has none of the artifact builders' Cargo cache.
        # Fetch the unchanged committed locks before overlay version resolution,
        # which deliberately remains offline for both manifests.
        for manifest in (a.source / "Cargo.toml", a.source / "tests/desktop/stage-driver/Cargo.toml"):
            run(["cargo", "fetch", "--locked", "--manifest-path", manifest], timeout=600)
        module = load("qualification_overlay", HERE / "create-overlay.py")
        a.work.mkdir(parents=True)
        private = a.work / "private"
        private.mkdir(mode=0o700)
        key = private / "ephemeral.pem"
        run([a.openssl, "genpkey", "-algorithm", "ED25519", "-out", key])
        key.chmod(0o600)
        public = subprocess.run([a.openssl, "pkey", "-in", str(key), "-pubout", "-outform", "DER"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout[-32:].hex()
        text = (a.source / "Cargo.toml").read_text()
        base_version = re.search(r'\[workspace\.package\]\n[^\[]*?version = "([0-9]+\.[0-9]+\.[0-9]+)"', text)[1]
        major, minor, patch = map(int, base_version.split("."))
        versions = [base_version, f"{major}.{minor}.{patch + 1}"]
        os.environ["CARGO_TARGET_DIR"] = str(a.work / "cargo-target")
        os.environ["CARGO_BUILD_JOBS"] = "2"
        overlays = []
        for label, version in zip(("baseline", "target"), versions):
            overlay = module.build_overlay(a.source.resolve(), a.candidate, a.work / label, version, public, "cargo")
            overlays.append(overlay)
            source = Path(overlay["build_source"])
            env = {**os.environ, "PLURX_BUILD_SHA": overlay["committed_overlay_sha"],
                   "PLURX_BUILD_REF": "qualification-local-only", "PLURX_QUALIFICATION_OVERLAY_SHA": overlay["committed_overlay_sha"]}
            if os.uname().sysname == "Darwin":
                binaries = a.work / label / "universal"
                binaries.mkdir()
                for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
                    run(["cargo", "build", "--locked", "--release", "--manifest-path", source / "Cargo.toml",
                         "--target", target, "-p", "plurxd", "-p", "plurx-desktop-control", "--bins"], timeout=7200, env=env)
                for name in ["plurxd", *BINS]:
                    run(["lipo", "-create", *(a.work / f"cargo-target/{target}/release/{name}" for target in
                        ("aarch64-apple-darwin", "x86_64-apple-darwin")), "-output", binaries / name])
                originals = list((a.output.parent / "macos/extracted").glob("plurxd-*-macos-universal"))
                if len(originals) != 1:
                    raise RuntimeError("unchanged-candidate macOS qualification root required")
                package_root = repackage_macos(originals[0], a.work / label / "assets", binaries, overlay, source)
            else:
                run(["cargo", "build", "--locked", "--release", "--manifest-path", source / "Cargo.toml",
                     "-p", "plurxd", "-p", "plurx-desktop-control", "--bins"], timeout=7200, env=env)
                binaries = a.work / "cargo-target/release"
                arch = run(["dpkg", "--print-architecture"]).stdout.strip()
                repackage_debs(a.artifacts, a.work / label / "assets", binaries, overlay, arch)
                package_root = None
            # Keep provenance, public diff and hashes, never fixture binaries/private keys in workflow receipts.
            destination = a.output / label
            destination.mkdir()
            shutil.copyfile(a.work / label / "overlay.json", destination / "overlay.json")
            shutil.copyfile(a.work / label / "overlay.patch", destination / "overlay.patch")
            overlay["package_root"] = str(package_root) if package_root else None
            if label == "target":
                run(["cargo", "build", "--release", "--locked", "--manifest-path",
                     source / "tests/desktop/stage-driver/Cargo.toml"], timeout=600, env=env)
        baseline, target = overlays
        sign = [sys.executable, HERE / "sign-local-cohort.py", "--candidate", a.candidate,
                "--baseline-overlay", a.work / "baseline/overlay.json", "--target-overlay", a.work / "target/overlay.json",
                "--assets", a.work / "target/assets", "--signing-key", key, "--openssl", a.openssl,
                "--platform", "macos" if os.uname().sysname == "Darwin" else "linux",
                "--arch", "universal" if os.uname().sysname == "Darwin" else arch]
        receipt.check("local-selected-cohort-signature", json.loads(run(sign).stdout))
        key.unlink()
        for startup in ("off", "on"):
            for running in (False, True):
                control = Path("/usr/bin/plurx-control") if os.uname().sysname == "Linux" else (
                    Path.home() / "Library/Application Support/plurx/runtime/control/plurx-control")
                authority = ["sudo", "--preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,GITHUB_SERVER_URL,GITHUB_SHA,PLURX_QUALIFICATION_MUTATE"] if os.uname().sysname == "Linux" else []
                run([*authority, control, "stop", "--confirm-interruption"], timeout=180)
                if os.uname().sysname == "Linux":
                    run([*authority, "apt-get", "-qq", "--reinstall", "--allow-downgrades", "install", "-y",
                         *sorted((a.work / "baseline/assets").glob("*.deb"))], timeout=600)
                else:
                    run([Path(baseline["package_root"]) / "control/plurx-control", "bootstrap", "macos", "--package",
                         baseline["package_root"], "--no-start"], timeout=300)
                run([*authority, control, "startup", "set", "server", startup])
                if running:
                    run([*authority, control, "start"], timeout=90)
                run([*authority, sys.executable, HERE / "apply-local-fixture.py", "--candidate", a.candidate,
                     "--baseline-overlay", a.work / "baseline/overlay.json", "--target-overlay", a.work / "target/overlay.json",
                     "--stage-driver", a.work / "cargo-target/release/plurx-qualification-stage",
                     "--assets", a.work / "target/assets", "--openssl", a.openssl,
                     "--output", a.output / f"{startup}-{'running' if running else 'stopped'}"], timeout=1200)
        final_control = Path("/usr/bin/plurx-control") if os.uname().sysname == "Linux" else (
            Path.home() / "Library/Application Support/plurx/runtime/control/plurx-control")
        run([*authority, final_control, "stop", "--confirm-interruption"], timeout=180)
        run([*authority, final_control, "startup", "set", "server", "off"])
        receipt.check("actual-worker-four-state-matrix", {"count": 4, "baseline": baseline["version"], "target": target["version"]})
        receipt.value["limitations"] = ["Ephemeral committed test roots and local staging, not production GitHub download or distribution trust.",
            "Prepared-before-shutdown recovery only; interrupted installation fault injection remains separate."]


if __name__ == "__main__":
    main()
