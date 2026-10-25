#!/usr/bin/env python3
"""Apply an already built signed fixture cohort through the actual unchanged control binaries.

Only explicit isolated fixture installations are accepted. Public GitHub preparation
is intentionally not impersonated. Private signing keys are never inputs to apply.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import time
import tempfile
from common import Receipt, hosted_guard, run, sha256, write_json
from lifecycle import Lifecycle


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--baseline-overlay", type=Path, required=True)
    p.add_argument("--target-overlay", type=Path, required=True)
    p.add_argument("--stage-driver", type=Path, required=True)
    p.add_argument("--assets", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--openssl", default="openssl")
    a = p.parse_args()
    with Receipt(a.output / "fixture-apply.json", a.candidate, "local-signed-fixture-not-github-download") as receipt:
        hosted_guard(a.candidate, mutate=True)
        base = json.loads(a.baseline_overlay.read_bytes())
        target = json.loads(a.target_overlay.read_bytes())
        for value in (base, target):
            if value["candidate"] != a.candidate or value["scope"] != "isolated-local-trust-overlay":
                raise RuntimeError("fixture overlays differ from the exact committed candidate")
        if base["public_key_hex"] != target["public_key_hex"]:
            raise RuntimeError("fixture replacement cannot silently change the signing root")
        record = Path("/etc/plurx/installation.json") if os.uname().sysname == "Linux" else (
            Path.home() / "Library/Application Support/plurx/installation.json")
        installed = json.loads(record.read_bytes())
        if installed["trust_key_ids"] != ["qualification-local-only"]:
            raise RuntimeError("refusing production installation: a deliberately installed fixture baseline is required")
        if installed["current_commit"] != base["committed_overlay_sha"] or installed["current_version"] != base["version"]:
            raise RuntimeError("installed fixture source/version differs from committed baseline overlay")
        control = Path("/usr/bin/plurx-control") if os.uname().sysname == "Linux" else Path(installed["paths"]["state"]) / "plurx-control"
        recovery = Path("/usr/lib/plurx-control/plurx-recovery") if os.uname().sysname == "Linux" else Path(installed["paths"]["state"]) / "plurx-recovery"
        roots = json.loads(run([control, "trust-info"]).stdout)
        if roots["keys"] != [{"id": "qualification-local-only", "public_key_hex": target["public_key_hex"]}]:
            raise RuntimeError("actual installed controller does not carry the fixture's exact ephemeral public root")
        lifecycle = Lifecycle(control, receipt)
        manifest = json.loads((a.assets / "plurx-update-v1.json").read_bytes())
        if manifest["source_commit"] != target["committed_overlay_sha"] or manifest["source_tree"] != target["committed_overlay_tree"]:
            raise RuntimeError("signed local cohort differs from the committed target overlay")
        receipt.check("fixture-source-and-assets", {"baseline": base, "target": target,
            "artifacts": {p.name: sha256(p) for p in a.assets.iterdir() if p.is_file()},
            "driver_sha256": sha256(a.stage_driver)})
        initial = lifecycle.call("status")
        configuration = Path(installed["paths"]["config"])
        configuration_before = sha256(configuration) if configuration.is_file() else None
        # An invalid exact-byte signature must be rejected before any runtime/policy mutation.
        with tempfile.TemporaryDirectory(prefix="metadata-tamper-", dir=a.assets.parent) as scratch:
            tampered = Path(scratch)
            for name in ("plurx-update-v1.json", "plurx-update-v1.json.sig"):
                shutil.copyfile(a.assets / name, tampered / name)
            signed = tampered / "plurx-update-v1.json"
            signed.write_bytes(signed.read_bytes() + b" ")
            refused = run([a.stage_driver, tampered.resolve(), a.target_overlay.resolve()], check=False, timeout=60)
            if refused.returncode == 0 or lifecycle.call("status") != initial:
                raise RuntimeError("tampered exact-byte manifest was accepted or changed running/startup policy")
            receipt.check("fixture-tampered-signature-refusal", {"stderr": refused.stderr[-3000:]})

        # A genuinely different fixture key must fail against the unchanged embedded root.
        with tempfile.TemporaryDirectory(prefix="wrong-key-", dir=a.assets.parent) as scratch:
            wrong = Path(scratch)
            shutil.copyfile(a.assets / "plurx-update-v1.json", wrong / "plurx-update-v1.json")
            key = wrong / "private.pem"
            run([a.openssl, "genpkey", "-algorithm", "ED25519", "-out", key])
            run([a.openssl, "pkeyutl", "-sign", "-rawin", "-inkey", key,
                 "-in", wrong / "plurx-update-v1.json", "-out", wrong / "plurx-update-v1.json.sig"])
            key.unlink()
            refused = run([a.stage_driver, wrong.resolve(), a.target_overlay.resolve()], check=False, timeout=60)
            if refused.returncode == 0 or lifecycle.call("status") != initial:
                raise RuntimeError("wrong-key signature was accepted or changed running/startup policy")
            receipt.check("fixture-wrong-key-refusal", {"stderr": refused.stderr[-3000:]})
        # Real staged artifact bytes must match the authenticated manifest before installer execution.
        with tempfile.TemporaryDirectory(prefix="artifact-tamper-", dir=a.assets.parent) as scratch:
            wrong = Path(scratch)
            for name in ("plurx-update-v1.json", "plurx-update-v1.json.sig", *[item["name"] for item in manifest["artifacts"]]):
                shutil.copyfile(a.assets / name, wrong / name)
            payload = wrong / manifest["artifacts"][0]["name"]
            with payload.open("r+b") as stream:
                first = stream.read(1)
                stream.seek(0)
                stream.write(bytes([first[0] ^ 1]))
            record_digest = sha256(record)
            refused = run([a.stage_driver, wrong.resolve(), a.target_overlay.resolve()], check=False, timeout=120)
            after_refusal = lifecycle.call("status")
            if refused.returncode == 0 or sha256(record) != record_digest:
                raise RuntimeError("tampered artifact was installed or changed installation identity")
            for field in ("service_state", "server_startup", "tray_startup"):
                if after_refusal[field] != initial[field]:
                    raise RuntimeError("tampered artifact changed running/startup policy")
            receipt.check("fixture-artifact-hash-refusal-before-install", {"stderr": refused.stderr[-3000:],
                "independent_status": after_refusal, "metadata_effect": "cancelled staging journal only"})

        def stage():
            result = run([a.stage_driver, a.assets.resolve(), a.target_overlay.resolve()], timeout=300)
            value = json.loads(result.stdout)
            receipt.check("actual-api-local-stage", value)
            return value["transaction_id"]

        # Real prepared-before-shutdown recovery should reconcile without changing operator policy.
        transaction = stage()
        recovered = run([recovery, "--transaction", transaction], timeout=180)
        after_recovery = lifecycle.call("status")
        for field in ("service_state", "server_startup", "tray_startup"):
            if after_recovery[field] != initial[field]:
                raise RuntimeError("untouched prepared recovery changed initial operator state")
        receipt.check("actual-native-prepared-recovery", {"transaction": transaction,
            "stdout": recovered.stdout[-3000:], "independent_status": after_recovery})
        transaction = stage()
        lifecycle.call("updates", "apply", "--transaction", transaction, "--confirm-interruption")
        deadline = time.monotonic() + 300
        last = None
        while time.monotonic() < deadline:
            last = lifecycle.call("updates", "status", "--transaction", transaction)
            if last["state"] == "complete":
                break
            if last["state"] in ("failed", "cancelled", "recovery_required"):
                raise RuntimeError(f"actual native update failed: {last}")
            time.sleep(1)
        else:
            raise RuntimeError(f"actual native update timed out: {last}")
        updated = json.loads(record.read_bytes())
        if updated["current_commit"] != target["committed_overlay_sha"] or updated["current_version"] != target["version"]:
            raise RuntimeError("successful phase did not install the exact target fixture identity")
        after = lifecycle.call("status")
        if after["installation_id"] != initial["installation_id"]:
            raise RuntimeError("update changed persistent installation identity")
        if (sha256(configuration) if configuration.is_file() else None) != configuration_before:
            raise RuntimeError("update rewrote operator configuration")
        for field in ("service_state", "server_startup", "tray_startup"):
            if after[field] != initial[field]:
                raise RuntimeError("update failed to preserve initial running/startup state")
        receipt.check("actual-native-apply-complete", {"progress": last, "independent_status": after,
                                                       "installation_record_sha256": sha256(record), "preserved_configuration_sha256": configuration_before})
        receipt.value["limitations"] = ["Ephemeral test-build public root and local selected-cohort staging only.",
            "This does not prove production GitHub discovery/download, owner production trust, Apple notarization, old legacy adoption or crash-after-install recovery."]


if __name__ == "__main__":
    main()
