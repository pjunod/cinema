"""Run and verify every exact Cargo target in the official release matrix."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

# Cargo launches the runner from each member's directory.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from validation import workspace_rust_execute as workspace


PARTITIONS = ("core", "store", "daemon")
PREREQUISITES = ("fmt-check", "spike-lock-check", "lint", "test-socket-permission-check")
RESTART_CHECK = "vodencode-restart-check"
OUTPUT = workspace.ROOT / "target/validation/official-rust"
PLAN = OUTPUT / "plan.json"


def partition(target: str) -> str:
    if target == "plurxd@src/main.rs":
        return "daemon"
    if target == "store_contract@tests/store_contract.rs":
        return "store"
    return "core"


def identity() -> dict[str, str]:
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    workspace.require(sha == os.environ["GITHUB_SHA"], "Rust partition checkout differs from release SHA")
    return {"sha": sha, "run": os.environ["GITHUB_RUN_ID"]}


def run_target(binary: str) -> None:
    plan = json.loads(PLAN.read_text())
    executable = str(Path(binary).resolve(strict=True))
    target = plan["targets"][executable]
    if partition(target) != plan["partition"]:
        return

    env = dict(os.environ)
    env.pop(workspace.RUNNER_VARIABLE, None)
    subprocess.run([executable], env=env, check=True)
    index = sorted(plan["targets"].values()).index(target)
    with (OUTPUT / f"passed-{index}").open("x") as marker:
        marker.write(target)


def run_partition(name: str) -> None:
    workspace.require(name in PARTITIONS, "Unknown official Rust partition")
    provenance = identity()
    OUTPUT.mkdir(parents=True, exist_ok=False)
    if name == "core":
        subprocess.run(
            ["make", *PREREQUISITES],
            cwd=workspace.ROOT, check=True,
        )
    targets = workspace.compiled_targets()
    PLAN.write_text(json.dumps({"partition": name, "targets": targets}))
    runner = shlex.join([sys.executable, str(Path(__file__).resolve()), "--target"])
    env = {**os.environ, workspace.RUNNER_VARIABLE: runner}
    command = ["cargo", "test", *workspace.WORKSPACE, "--no-fail-fast"]
    subprocess.run([*command, "--lib", "--bins", "--tests"], cwd=workspace.ROOT, env=env, check=True)

    assigned = {key for key in targets.values() if partition(key) == name}
    passed = {marker.read_text() for marker in OUTPUT.glob("passed-*")}
    workspace.require(assigned and passed == assigned, "Cargo did not run the complete Rust partition")
    if name == "core":
        subprocess.run([*command, "--doc"], cwd=workspace.ROOT, check=True)
        subprocess.run(["make", RESTART_CHECK], cwd=workspace.ROOT, check=True)

    receipt = {**provenance, "partition": name, "inventory": sorted(targets.values()), "passed": sorted(passed)}
    (OUTPUT / f"official-rust-{name}.json").write_text(json.dumps(receipt) + "\n")


def verify(directory: Path) -> None:
    provenance = identity()
    receipts = []
    for name in PARTITIONS:
        receipt = json.loads((directory / f"official-rust-{name}.json").read_text())
        workspace.require(receipt["partition"] == name and
                          all(receipt[field] == value for field, value in provenance.items()),
                          "Rust partition receipt belongs to another run, source or partition")
        receipts.append(receipt)

    inventory = receipts[0]["inventory"]
    workspace.require(inventory and inventory == sorted(set(inventory)), "Invalid Cargo target inventory")
    for receipt in receipts:
        name = receipt["partition"]
        expected = [target for target in inventory if partition(target) == name]
        workspace.require(expected and receipt["inventory"] == inventory and receipt["passed"] == expected,
                          f"Missing or inconsistent Rust partition: {name}")
    print(f"Verified full official Rust coverage across {len(inventory)} compiled targets")


if __name__ == "__main__":
    action, value = sys.argv[1:]
    if action == "--target":
        run_target(value)
    elif action == "--run":
        run_partition(value)
    elif action == "--verify":
        verify(Path(value))
    else:
        raise SystemExit("Expected --run, --verify or --target")
