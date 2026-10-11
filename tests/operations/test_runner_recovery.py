"""Stopped VMs recover with headroom; maintenance and busy jobs survive."""

import importlib.machinery
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("runner_watchdog", str(ROOT / "deploy/runner-recovery/plurx-runner-watchdog"))
spec = importlib.util.spec_from_loader(loader.name, loader)
watchdog = importlib.util.module_from_spec(spec)
loader.exec_module(watchdog)


class RunnerRecoveryCase(unittest.TestCase):
    def setUp(self):
        self.name = "gha-lab6-general-01"
        self.unit = f"forgejo-runner-{self.name}.service"
        self.config = {"runners": [{"vm": self.name, "unit": self.unit, "memory_gib": 8}]}
        self.instance = {"name": self.name, "location": "lab6", "status": "Stopped", "expanded_config": {"boot.autostart": "true", "limits.memory": "8GiB"}}
        self.commands = []
        self.state = {}
        self.active = "active"

    def execute(self, *args):
        self.commands.append(args)
        if args[1] == "list":
            return json.dumps([self.instance])
        if args[1] == "query":
            return json.dumps({"memory": {"usage": watchdog.GIB}})
        if "show" in args:
            return self.active
        return ""

    def run_pass(self, available=20, now=10000):
        return watchdog.recover(self.config, self.state, execute=self.execute, memory=lambda: available * watchdog.GIB, host="lab6", now=now)

    def test_stopped_vm_recovers_with_host_headroom(self):
        result = self.run_pass()
        self.assertIn(("incus", "start", "--project", "github-runners", self.name), self.commands)
        self.assertEqual(result["recovered"], [f"{self.name}: VM started"])

    def test_short_host_defers_vm_boot(self):
        result = self.run_pass(available=11)
        self.assertFalse(any(c[1] == "start" for c in self.commands))
        self.assertIn("insufficient host memory", result["deferred"][0])

    def test_intentional_maintenance_is_not_undone(self):
        self.instance["expanded_config"]["boot.autostart"] = "false"
        result = self.run_pass()
        self.assertEqual(len(self.commands), 1)
        self.assertIn("maintenance", result["deferred"][0])

    def test_remote_vm_is_never_started(self):
        self.instance["location"] = "lab4"
        result = self.run_pass()
        self.assertEqual(len(self.commands), 1)
        self.assertTrue(result["errors"])

    def test_larger_inventory_allocation_is_not_admitted_as_small_vm(self):
        self.instance["expanded_config"]["limits.memory"] = "14GiB"
        result = self.run_pass()
        self.assertEqual(len(self.commands), 1)
        self.assertIn("memory allocation differs", result["errors"][0])

    def test_repeated_ooms_have_backoff_and_hourly_ceiling(self):
        for now in (10000, 10001, 10601, 11202, 11803):
            self.run_pass(now=now)
        self.assertEqual(sum(c[1] == "start" for c in self.commands), 3)

    def test_restart_history_is_saved_before_external_start(self):
        saved = []

        def execute(*args):
            if args[1] == "start":
                self.assertEqual(saved, [[10000]])
            return self.execute(*args)

        watchdog.recover(self.config, self.state, execute=execute,
                         memory=lambda: 20 * watchdog.GIB, host="lab6", now=10000,
                         persist=lambda state: saved.append(list(state["attempts"][self.name])))

    def test_failed_daemon_recovers_without_rebooting_vm(self):
        self.instance["status"] = "Running"
        self.active = "failed"
        result = self.run_pass()
        self.assertIn(f"{self.name}: daemon started", result["recovered"])
        self.assertFalse(any(c[1] == "start" for c in self.commands))

    def test_active_and_draining_daemons_are_never_restarted(self):
        self.instance["status"] = "Running"
        for self.active in ("active", "activating", "deactivating"):
            self.commands.clear()
            self.run_pass()
            self.assertFalse(any("start" in c for c in self.commands))

    def test_two_boots_reserve_full_allocations(self):
        second = dict(self.instance, name="gha-lab6-general-02")
        self.config["runners"].append(dict(self.config["runners"][0], vm=second["name"], unit=f"forgejo-runner-{second['name']}.service"))
        def execute(*args):
            if args[1] == "list":
                return json.dumps([self.instance, second])
            return self.execute(*args)
        result = watchdog.recover(self.config, self.state, execute=execute,
                                  memory=lambda: 13 * watchdog.GIB, host="lab6", now=10000)
        self.assertEqual(sum(c[1] == "start" for c in self.commands), 1)
        self.assertIn("insufficient host memory", result["deferred"][0])

    def test_running_guest_unused_allocation_is_reserved_on_later_pass(self):
        running = dict(self.instance, name="gha-lab6-general-02", status="Running")
        def execute(*args):
            if args[1] == "list":
                return json.dumps([self.instance, running])
            return self.execute(*args)
        result = watchdog.recover(self.config, self.state, execute=execute,
                                  memory=lambda: 13 * watchdog.GIB, host="lab6", now=10000)
        self.assertFalse(any(c[1] == "start" for c in self.commands))
        self.assertIn("insufficient host memory", result["deferred"][0])


class RunnerReconcileCase(unittest.TestCase):
    def test_resource_update_preserves_other_environment_and_credentials(self):
        loader = importlib.machinery.SourceFileLoader("runner_reconcile", str(ROOT / "deploy/runner-recovery/plurx-runner-reconcile"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        source = "runner:\n  capacity: 4\n  envs:\n    OTHER_ENV: keep-me\n    CARGO_BUILD_JOBS: 12\n  labels:\n    - general:host\ncache:\n  enabled: true\ncontainer:\n  options: old-options\n  docker_host: unix:///var/run/docker.sock\ncredentials:\n  token: existing-registration-secret\n"
        updated = module.configuration(source, ["self-hosted", "ci-coverage"], 2)
        self.assertIn('    OTHER_ENV: keep-me', updated)
        self.assertIn('  token: existing-registration-secret', updated)
        self.assertIn('  docker_host: unix:///var/run/docker.sock', updated)
        self.assertNotIn('general:host', updated)
        self.assertNotIn('CARGO_BUILD_JOBS: 12', updated)
        self.assertIn('--cpus 2 --memory 6g --memory-swap 8g', updated)
        self.assertEqual(updated, module.configuration(updated, ["self-hosted", "ci-coverage"], 2))
