"""Startup cleanup may select only exact prior-attempt devices from this run."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
MODULE = ROOT / "validation" / "apple_simulator_ownership.py"


def device(name: str, udid: str) -> dict[str, str]:
    return {"name": name, "udid": udid}


def startup_run_block() -> str:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text()
    lines = workflow.splitlines()
    marker = "      - name: Create isolated iPhone, iPad, and Apple TV simulators"
    start = lines.index(marker) + 1
    while start < len(lines) and lines[start].strip() != "run: |":
        start += 1
    if start == len(lines):
        raise AssertionError("Apple simulator startup step has no run block")
    body: list[str] = []
    for line in lines[start + 1:]:
        if line and not line.startswith("          "):
            break
        body.append(line[10:] if line.startswith("          ") else "")
    return "\n".join(body) + "\n"


class AppleSimulatorOwnershipTests(unittest.TestCase):
    def run_selector(self, payload: str, run_id: str = "7078", attempt: str = "3"):
        return subprocess.run(
            [sys.executable, str(MODULE), "--run-id", run_id, "--attempt", attempt],
            input=payload, text=True, capture_output=True, check=False,
        )

    def test_selects_only_prior_attempts_for_current_run(self):
        prior_ids = [
            "CCA2E748-980A-4017-8D23-24E58C3AFD29",
            "23AE4C92-DDE5-4EA4-AD32-296BBAE25B88",
        ]
        devices = [
            device("plurx CI iPhone 7078-1", prior_ids[0]),
            device("plurx CI iPad 7078-2", prior_ids[1]),
            device("plurx CI Apple TV 7078-3", "1E922186-F3F8-4430-9CDD-BAF09042B0F6"),
            device("plurx CI iPhone 7079-1", "9CB1A94B-7686-44CE-AC07-D6D25BC56707"),
            device("plurx CI iPhone 7078-4", "9DB1A94B-7686-44CE-AC07-D6D25BC56707"),
            device("plurx CI iPhone old-1", "8DB1A94B-7686-44CE-AC07-D6D25BC56707"),
            device("Plurx CI iPhone 7078-1", "7DB1A94B-7686-44CE-AC07-D6D25BC56707"),
        ]
        payload = json.dumps({"devices": {"runtime": devices}})
        result = self.run_selector(payload)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), prior_ids)

    def test_invalid_owned_uuid_fails_closed(self):
        payload = json.dumps({"devices": {"runtime": [device("plurx CI iPhone 7078-1", "not-a-uuid")]}})
        result = self.run_selector(payload)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_malformed_json_and_invalid_run_metadata_fail_closed(self):
        for payload, run_id, attempt in (
            ("{", "7078", "3"),
            (json.dumps({"devices": []}), "7078", "3"),
            (json.dumps({"devices": {}}), "0", "3"),
            (json.dumps({"devices": {}}), "7078", "0"),
        ):
            with self.subTest(run_id=run_id, attempt=attempt):
                result = self.run_selector(payload, run_id, attempt)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def run_startup(self, payload: str):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as temporary:
            temp = Path(temporary)
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            devices_file = temp / "devices.json"
            devices_file.write_text(payload)
            calls_file = temp / "calls.log"
            create_count = temp / "create-count"
            env_file = temp / "github-env"
            xcrun = bin_dir / "xcrun"
            xcrun.write_text("""#!/bin/bash
set -euo pipefail
printf 'simctl %s\\n' "$*" >> "$SIMCTL_CALLS"
case "$1 $2" in
  'simctl list')
    if [[ "${3:-}" == "devices" && "${4:-}" == "--json" ]]; then
      cat "$SIMCTL_DEVICES"
    else
      exit 2
    fi
    ;;
  'simctl create')
    count=0
    [[ ! -f "$SIMCTL_CREATE_COUNT" ]] || count="$(cat "$SIMCTL_CREATE_COUNT")"
    count=$((count + 1))
    printf '%s' "$count" > "$SIMCTL_CREATE_COUNT"
    printf '00000000-0000-0000-0000-%012d\\n' "$count"
    ;;
  'simctl shutdown'|'simctl delete'|'simctl boot'|'simctl bootstatus') ;;
  *) exit 2 ;;
esac
""")
            xcrun.chmod(0o755)
            env = os.environ.copy()
            env.update({
                "PATH": f"{bin_dir}:{env['PATH']}",
                "SIMCTL_DEVICES": str(devices_file),
                "SIMCTL_CALLS": str(calls_file),
                "SIMCTL_CREATE_COUNT": str(create_count),
                "GITHUB_ENV": str(env_file),
                "GITHUB_RUN_ID": "7078",
                "GITHUB_RUN_ATTEMPT": "3",
            })
            result = subprocess.run(
                ["bash", "-c", startup_run_block()], cwd=ROOT, env=env,
                text=True, capture_output=True, check=False,
            )
            calls = calls_file.read_text().splitlines() if calls_file.exists() else []
            return result, calls

    def test_workflow_startup_replay_only_deletes_this_runs_prior_attempt(self):
        own_prior = "CCA2E748-980A-4017-8D23-24E58C3AFD29"
        foreign_active = "9CB1A94B-7686-44CE-AC07-D6D25BC56707"
        own_current = "23AE4C92-DDE5-4EA4-AD32-296BBAE25B88"
        own_future = "1E922186-F3F8-4430-9CDD-BAF09042B0F6"
        payload = json.dumps({"devices": {"runtime": [
            device("plurx CI iPhone 7078-1", own_prior),
            device("plurx CI iPhone 66006-2", foreign_active),
            device("plurx CI iPad 7078-3", own_current),
            device("plurx CI Apple TV 7078-4", own_future),
        ]}})
        result, calls = self.run_startup(payload)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [call for call in calls if call in (f"simctl shutdown {own_prior}", f"simctl delete {own_prior}")],
            [f"simctl shutdown {own_prior}", f"simctl delete {own_prior}"],
        )
        for foreign_id in (foreign_active, own_current, own_future):
            self.assertNotIn(f"simctl shutdown {foreign_id}", calls)
            self.assertNotIn(f"simctl delete {foreign_id}", calls)
        self.assertEqual(sum(call.startswith("simctl create ") for call in calls), 3)

    def test_workflow_startup_replay_stops_on_malformed_json_before_mutation(self):
        result, calls = self.run_startup("{")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call.startswith(("simctl delete ", "simctl create ")) for call in calls))


if __name__ == "__main__":
    unittest.main()
