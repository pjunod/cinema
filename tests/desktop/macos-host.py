#!/usr/bin/env python3
"""Install candidate artifacts and observe real Aqua UI only on a fresh hosted Mac."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tarfile
import time
from common import Receipt, hosted_guard, run, sha256
from lifecycle import Lifecycle

ROOT = Path(__file__).resolve().parents[2]


def smoke(app, action, output, lifecycle, console):
    lifecycle.call("start")
    before = lifecycle.state("running")
    with (app / "Contents/Info.plist").open("rb") as stream:
        executable = app / "Contents/MacOS" / plistlib.load(stream)["CFBundleExecutable"]
    with output.with_suffix(".log").open("w") as log:
        process = subprocess.Popen([str(executable), "--smoke-receipt", str(output), "--smoke-action", action,
                                    "--smoke-timeout", "90"], stdout=log, stderr=subprocess.STDOUT)
        observed = None
        deadline = time.monotonic() + 100
        while process.poll() is None and time.monotonic() < deadline:
            result = run(["/bin/ps", "-p", str(process.pid), "-o", "uid=,comm="], check=False)
            fields = result.stdout.strip().split(maxsplit=1)
            if len(fields) == 2 and fields[0] == str(os.getuid()) and fields[1] == str(executable):
                observed = {"pid": process.pid, "uid": int(fields[0]), "executable": fields[1]}
            time.sleep(0.05)
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
            raise RuntimeError("real Aqua tray smoke timed out")
        if process.returncode:
            raise RuntimeError(f"real Aqua tray {action} exited {process.returncode}; see {log.name}")
    value = json.loads(output.read_bytes())
    if not observed or value["ui_pid"] != observed["pid"] or value["uid"] != observed["uid"]:
        raise RuntimeError("actual UI process identity differs from independent observation")
    if not value["session"]["gui_session"] or value["session"]["console_user_uid"] != console["uid"]:
        raise RuntimeError("UI is not in the proved console GUI session")
    if value["session"]["session_id"] != console["session_id"]:
        raise RuntimeError("tray and independently observed CG console session differ")
    if not value["tray"]["status_item_present"] or not value["tray"]["button_image_template"]:
        raise RuntimeError("actual native template status item was not created")
    embedded = app / "Contents/MacOS/plurx-control"
    if Path(value["controller"]["path"]).resolve() != embedded.resolve() or sha256(embedded) != sha256(lifecycle.control):
        raise RuntimeError("UI did not use the exact installed bundled controller")
    if not value["controller"]["first_status"]["ok"]:
        raise RuntimeError("native menu never observed successful actual controller status")
    if not value["menu"]["titles"] or value["quit"]["controller_calls_during_quit"] != 0:
        raise RuntimeError("normal tray menu/Quit path was not exercised")
    after = lifecycle.state("stopped" if action == "stop" else "running")
    if after["installation_id"] != before["installation_id"]:
        raise RuntimeError("UI action changed installation identity")
    if action == "stop" and not value["action"]["ui_alive_after"]:
        raise RuntimeError("Stop closed the tray")
    return {"ui": value, "observed_process": observed, "independent_before": before, "independent_after": after}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--candidate", required=True)
    p.add_argument("--artifacts", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    a.output = a.output.resolve()
    with Receipt(a.output / "host.json", a.candidate, "native-macos-aqua-hosted-vm") as receipt:
        hosted_guard(a.candidate, mutate=True)
        uid = os.getuid()
        run(["/bin/launchctl", "print", f"gui/{uid}"])
        if run(["/usr/bin/stat", "-f", "%u", "/dev/console"]).stdout.strip() != str(uid):
            raise RuntimeError("hosted runner is not the active console owner")
        observer = a.output / "console-session"
        run(["xcrun", "swiftc", ROOT / "tests/desktop/console-session.swift", "-o", observer], timeout=120)
        console = json.loads(run([observer]).stdout)
        if console["uid"] != uid or not console["on_console"] or not console["login_done"]:
            raise RuntimeError("real console graphics login is absent")
        receipt.check("independent-aqua-console", console)
        home = Path.home()
        for path in (home / "Library/Application Support/plurx", home / "Applications/Plurx Server.app",
                     home / "Library/LaunchAgents/com.plurx.plurxd.plist", Path("/usr/local/bin/plurxd")):
            if path.exists():
                raise RuntimeError(f"refusing pre-existing operator installation: {path}")
        for domain in ("system", f"gui/{uid}"):
            if run(["/bin/launchctl", "print", f"{domain}/com.plurx.plurxd"], check=False).returncode == 0:
                raise RuntimeError("conflicting existing launchd job; disposable fresh install required")
        archives = list(a.artifacts.glob("plurxd-*-macos-universal.tar.gz"))
        if len(archives) != 1:
            raise RuntimeError("exactly one candidate universal runtime archive required")
        archive = archives[0]
        archive_digest = sha256(archive)
        receipt.check("untouched-candidate-archive", {"name": archive.name, "sha256": archive_digest})
        extracted = a.output / "extracted"
        extracted.mkdir()
        with tarfile.open(archive) as stream:
            # Python's data filter refuses path traversal and device entries. Links are also refused.
            members = stream.getmembers()
            if any(not (m.isfile() or m.isdir()) for m in members):
                raise RuntimeError("native release archive contains links or special files")
            stream.extractall(extracted, members=members, filter="data")
        packages = list(extracted.iterdir())
        if len(packages) != 1 or not (packages[0] / "runtime-closure-v1.json").is_file():
            raise RuntimeError("native archive must contain exactly one complete canonical package root")
        package = packages[0]
        release = json.loads((package / "release.json").read_bytes())
        if release["source_commit"] != a.candidate:
            raise RuntimeError("downloaded archive source differs from exact dispatched candidate")
        distribution = run(["python3", ROOT / "scripts/package-macos", "--verify-distribution", package,
                            "--team-id", "AAAAAAAAAA"], timeout=180, check=False)
        if distribution.returncode == 0:
            raise RuntimeError("candidate unexpectedly passed the unprovisioned distribution identity gate")
        if sha256(archive) != archive_digest:
            raise RuntimeError("distribution refusal changed the untouched archive")
        receipt.check("untouched-candidate-distribution-gate-refusal", {"stderr": distribution.stderr[-4000:]})
        control = package / "control/plurx-control"
        roots = json.loads(run([control, "trust-info"]).stdout)
        receipt.check("untouched-candidate-production-roots", roots)
        bootstrap = run([control, "bootstrap", "macos", "--package", package, "--no-start"], timeout=180)
        value = json.loads(bootstrap.stdout)
        if not value.get("ok") or value["result"]["service_state"] != "stopped":
            raise RuntimeError(f"native --no-start bootstrap failed: {value}")
        receipt.check("native-bootstrap-no-start", value)
        record = home / "Library/Application Support/plurx/installation.json"
        installation = json.loads(record.read_bytes())
        installed_control = Path(installation["paths"]["state"]) / "plurx-control"
        lifecycle = Lifecycle(installed_control, receipt)
        if not roots["keys"]:
            before = lifecycle.call("status")
            digest = sha256(record)
            negative = run([installed_control, "updates", "prepare", "--version", "999.0.0"], timeout=30, check=False)
            refusal = json.loads(negative.stdout)
            if negative.returncode == 0 or refusal.get("error", {}).get("code") != "untrusted_update":
                raise RuntimeError("empty production roots did not fail closed")
            if lifecycle.call("status") != before or sha256(record) != digest:
                raise RuntimeError("empty-root refusal changed installation or running/startup policy")
            receipt.check("production-empty-root-refusal-before-runtime-mutation", refusal)
        lifecycle.four_states()
        app = home / "Applications/Plurx Server.app"
        for action in ("quit", "stop", "restart"):
            receipt.check("native-ui-" + action, smoke(app, action, a.output / f"ui-{action}.json", lifecycle, console))
        lifecycle.stop()
        lifecycle.call("startup", "set", "server", "off")
        receipt.value["limitations"] = ["This runs the native architecture slice; universal compatibility requires both native runners.",
            "SMAppService tray login registration/approval is controlled only by the app; no controller tray-login override was attempted. Actual approval, logout/relogin, reboot, hardware transcode and old legacy adoption are separate.",
            "Local signed fixture staging is not production GitHub discovery/download or Developer ID/notary acceptance."]


if __name__ == "__main__":
    main()
