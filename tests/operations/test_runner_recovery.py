"""Stopped VMs recover with headroom; maintenance and busy jobs survive."""

import importlib.machinery
import importlib.util
import json
import hashlib
import os
from pathlib import Path
import unittest
import tempfile
import threading
import subprocess
import sys
from unittest.mock import patch
from types import SimpleNamespace

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
        self.assertIn('--cpus 2 --memory 6g --memory-swap 10g', updated)
        self.assertEqual(updated, module.configuration(updated, ["self-hosted", "ci-coverage"], 2))

    def test_general_compiler_budget_keeps_coverage_separate(self):
        module = self.swap_module()
        source = "runner:\n  envs: {}\ncontainer:\n  options: old\n"
        general = module.configuration(source, ["self-hosted", "general", "high-cpu"], 3)
        coverage = module.configuration(source, ["self-hosted", "ci-coverage"], 2)
        self.assertIn('--cpus 3 --memory 7g --memory-swap 14g', general)
        self.assertIn('--cpus 2 --memory 6g --memory-swap 10g', coverage)
        self.assertIn('CARGO_BUILD_JOBS: "1"', general)
        self.assertIn('CARGO_BUILD_JOBS: "1"', coverage)

    def swap_module(self):
        loader = importlib.machinery.SourceFileLoader("runner_swap", str(ROOT / "deploy/runner-recovery/plurx-runner-reconcile"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        return module

    def test_swap_refuses_to_consume_disk_reserve(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = []
            with self.assertRaisesRegex(RuntimeError, "disk reserve"):
                self.swap_module().ensure_swap(root / "runner.swap", execute=lambda *args: commands.append(args), disk=lambda path: SimpleNamespace(free=28 * 1024 ** 3))
            self.assertEqual(commands, [])
            self.assertFalse((root / "runner.swap").exists())

    def test_swap_is_persistent_and_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            swap, fstab, swaps = root / "runner.swap", root / "fstab", root / "swaps"
            fstab.write_text("# original mount table\n/dev/root / ext4 defaults 0 1\n")
            swaps.write_text("Filename Type Size Used Priority\n")
            commands = []
            def execute(*args):
                commands.append(args)
                if args[0] == "fallocate":
                    with Path(args[-1]).open("r+b") as file:
                        file.truncate(int(args[2]))
                if args[0] == "swapon":
                    swaps.write_text(f"Filename Type Size Used Priority\n{swap} file 4194304 0 -2\n")
            module = self.swap_module()
            for _ in range(2):
                module.ensure_swap(swap, fstab, swaps, execute=execute, disk=lambda path: SimpleNamespace(free=30 * 1024 ** 3))
            self.assertEqual([command[0] for command in commands], ["fallocate", "mkswap", "swapon"])
            self.assertEqual(fstab.read_text().count(f"{swap} none swap sw 0 0"), 1)
            self.assertIn("/dev/root / ext4", fstab.read_text())
            self.assertEqual(swap.stat().st_mode & 0o777, 0o600)

    def test_busy_job_defers_restart_until_next_idle_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            pending = Path(directory) / "restart-pending"
            module = self.swap_module()
            commands = []
            def execute(*args):
                commands.append(args)
                return "123\n" if args[0] == "systemctl" and args[1] == "show" else ""
            self.assertFalse(module.restart_when_idle("runner.service", pending, True,
                                                     ["ACTIONS-TASK-1"], execute))
            self.assertTrue(pending.exists())
            self.assertEqual(commands, [])
            self.assertTrue(module.restart_when_idle("runner.service", pending, False, [], execute))
            self.assertFalse(pending.exists())
            self.assertIn(("systemctl", "restart", "--no-block", "runner.service"), commands)

    def test_host_job_children_defer_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            pending = Path(directory) / "restart-pending"
            commands = []
            def execute(*args):
                commands.append(args)
                return "123\n" if args[0] == "systemctl" else "123\n456\n789\n"
            self.assertFalse(self.swap_module().restart_when_idle("runner.service", pending, True, [], execute))
            self.assertTrue(pending.exists())
            self.assertFalse(any(command[1] == "restart" for command in commands))

    def test_supplemental_swap_preserves_active_original(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original, supplemental = root / "runner.swap", root / "compiler.swap"
            fstab, swaps = root / "fstab", root / "swaps"
            with original.open("wb") as file:
                file.truncate(4 * 1024 ** 3)
            fstab.write_text(f"{original} none swap sw 0 0\n")
            swaps.write_text(f"Filename Type Size Used Priority\n{original} file 4194304 1024 -2\n")
            commands = []
            def execute(*args):
                commands.append(args)
                if args[0] == "fallocate":
                    with Path(args[-1]).open("r+b") as file:
                        file.truncate(int(args[2]))
                if args[0] == "swapon":
                    with swaps.open("a") as file:
                        file.write(f"{supplemental} file 4194304 0 -3\n")
            module = self.swap_module()
            for _ in range(2):
                module.ensure_swap(original, fstab, swaps, execute=execute)
                module.ensure_swap(supplemental, fstab, swaps, execute=execute,
                                   disk=lambda path: SimpleNamespace(free=30 * 1024 ** 3))
            self.assertEqual([command[0] for command in commands], ["fallocate", "mkswap", "swapon"])
            self.assertEqual(original.stat().st_size, 4 * 1024 ** 3)
            self.assertEqual(fstab.read_text().count(f"{original} none swap"), 1)
            self.assertEqual(fstab.read_text().count(f"{supplemental} none swap"), 1)
            self.assertEqual(supplemental.stat().st_mode & 0o777, 0o600)

    def test_swap_does_not_overwrite_existing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            swap = Path(directory) / "runner.swap"
            swap.write_text("existing data")
            with self.assertRaisesRegex(RuntimeError, "refusing to replace"):
                self.swap_module().ensure_swap(swap)
            self.assertEqual(swap.read_text(), "existing data")


class OperatorPauseCase(unittest.TestCase):
    """ci-fleet's Drain must survive watchdog passes; Start VM must target one VM."""

    execute = RunnerRecoveryCase.execute

    def setUp(self):
        RunnerRecoveryCase.setUp(self)
        self.paused = tempfile.TemporaryDirectory()
        self.addCleanup(self.paused.cleanup)

    def pause(self):
        (Path(self.paused.name) / self.name).write_text(json.dumps({"actor": "operator", "reason": "measuring"}))

    def pass_with(self, **kwargs):
        return watchdog.recover(self.config, self.state, execute=self.execute, memory=lambda: 20 * watchdog.GIB,
                                host="lab6", now=10000, paused_dir=self.paused.name, **kwargs)

    def test_a_drained_runner_stays_stopped_across_two_passes(self):
        self.instance["status"] = "Running"
        self.active = "inactive"
        self.pause()
        for _ in range(2):
            receipt = self.pass_with()
            self.assertEqual(receipt["deferred"], [f"{self.name}: paused by operator: measuring"])
        self.assertFalse([c for c in self.commands if "start" in c], self.commands)

    def test_a_paused_stopped_vm_is_not_booted(self):
        self.pause()
        self.pass_with()
        self.assertNotIn(("incus", "start", "--project", "github-runners", self.name), self.commands)

    def test_resume_lets_the_next_pass_start_the_daemon(self):
        self.instance["status"] = "Running"
        self.active = "inactive"
        self.pause()
        self.pass_with()
        (Path(self.paused.name) / self.name).unlink()
        receipt = self.pass_with()
        self.assertIn(f"{self.name}: daemon started", receipt["recovered"])

    def test_vm_option_recovers_only_that_vm(self):
        other = "gha-lab6-general-02"
        self.config["runners"].append({"vm": other, "unit": f"forgejo-runner-{other}.service", "memory_gib": 8})
        receipt = self.pass_with(only=self.name)
        self.assertIn(f"{self.name}: VM started", receipt["recovered"])
        self.assertFalse(any(other in entry for entry in receipt["errors"] + receipt["deferred"]))

    def test_vm_option_refuses_an_unmanaged_vm(self):
        receipt = self.pass_with(only="gha-lab6-general-09")
        self.assertEqual(receipt["errors"], ["gha-lab6-general-09: not managed by this host's watchdog"])
        self.assertFalse([c for c in self.commands if "start" in c])


class GuardedAdmissionCase(unittest.TestCase):
    execute_legacy = RunnerRecoveryCase.execute

    def setUp(self):
        RunnerRecoveryCase.setUp(self)
        self.folder = tempfile.TemporaryDirectory(); self.addCleanup(self.folder.cleanup)
        self.uuid = "00000000-0000-4000-8000-000000000001"
        self.generation = "00000000-0000-4000-8000-000000000002"
        self.instance["expanded_config"].update({"volatile.uuid":self.uuid,"volatile.uuid.generation":self.generation})
        self.maintenance = Path(self.folder.name)/"maintenance.json"
        self.paused = Path(self.folder.name)/"paused"; self.paused.mkdir()

    def execute(self,*args):
        if args[1] == "query" and "/state?" not in args[2]:
            self.commands.append(args); return json.dumps(self.instance)
        return self.execute_legacy(*args)

    def guarded(self,**kwargs):
        args = dict(execute=self.execute,memory=lambda:20*watchdog.GIB,host="lab6",now=10000,
            only=self.name,expected_instance_uuid=self.uuid,expected_instance_generation=self.generation,
            authorization_deadline=15,monotonic=lambda:10,paused_dir=self.paused,maintenance=self.maintenance,
            command_id='a'*32,command_hash='b'*64,owner_code_hash='c'*64)
        args.update(kwargs)
        return watchdog.recover(self.config,self.state,**args)

    def test_exact_identity_starts_only_vm_and_returns_structured_receipt(self):
        receipt = self.guarded()
        self.assertEqual(receipt["owner_contract"],2); self.assertEqual(receipt["outcome"],"started")
        self.assertEqual(receipt["observed_resource"],receipt["expected_resource"])
        self.assertEqual(sum(c[1] == "start" for c in self.commands),1)
        self.assertFalse(any(c[1] == "exec" for c in self.commands))

    def test_same_name_replacement_changed_generation_and_missing_identity_refuse(self):
        for field in ("volatile.uuid","volatile.uuid.generation"):
            original = self.instance["expanded_config"][field]
            for value in ("00000000-0000-4000-8000-000000000009",None):
                self.instance["expanded_config"][field] = value; self.commands = []
                receipt = self.guarded()
                self.assertEqual(receipt["outcome"],"rejected"); self.assertTrue(receipt["errors"])
                if value is None:self.assertIsNone(receipt['observed_resource'])
                self.assertFalse(receipt['external_submission_started'])
                self.assertFalse(any(c[1] in ("start","exec") for c in self.commands))
            self.instance["expanded_config"][field] = original

    def test_running_vm_never_repairs_an_inactive_daemon(self):
        self.instance["status"] = "Running"; self.active = "inactive"
        receipt = self.guarded()
        self.assertEqual(receipt["outcome"],"already_running")
        self.assertFalse(any(c[1] in ("start","exec") for c in self.commands))

    def test_pause_arriving_during_headroom_check_blocks_final_admission(self):
        def memory():
            (self.paused/self.name).write_text('{"actor":"operator"}')
            return 20*watchdog.GIB
        receipt = self.guarded(memory=memory)
        self.assertEqual(receipt["outcome"],"deferred")
        self.assertFalse(any(c[1] == "start" for c in self.commands)); self.assertFalse(self.state['attempts'].get(self.name))

    def test_pause_or_maintenance_arriving_during_persistence_blocks_start(self):
        for marker in (self.paused/self.name,self.maintenance):
            self.commands = []; self.state = {}
            receipt = self.guarded(persist=lambda state:marker.write_text('{"owner":"operator"}'))
            self.assertEqual(receipt['outcome'],'deferred')
            self.assertFalse(any(c[1] == 'start' for c in self.commands))
            self.assertEqual(self.state['attempts'][self.name],[10000])
            marker.unlink()

    def test_maintenance_survives_process_restart_and_blocks_timer_and_guarded(self):
        self.maintenance.write_text('{"owner":"rolling-maintenance"}')
        self.assertEqual(self.guarded()["outcome"],"deferred")
        self.commands = []
        receipt = watchdog.recover(self.config,{},execute=self.execute,host="lab6",maintenance=self.maintenance)
        self.assertTrue(receipt["deferred"]); self.assertFalse(any(c[1] == "start" for c in self.commands))

    def test_expiry_before_admission_or_during_persistence_never_starts(self):
        for times in ([16],[10,16]):
            self.commands = []; self.state = {}
            with patch.object(watchdog.time,'monotonic',side_effect=times) as clock:
                receipt = self.guarded(monotonic=clock)
            self.assertEqual(receipt["outcome"],"deferred")
            self.assertFalse(any(c[1] == "start" for c in self.commands))

    def test_start_timeout_is_unknown_with_persisted_attempt_not_success(self):
        order = []
        def execute(*args):
            if args[1] == "start":
                self.assertEqual(order,["persisted","persisted"])
                raise subprocess.TimeoutExpired(args,120)
            return self.execute(*args)
        receipt = self.guarded(execute=execute,persist=lambda state:order.append("persisted"))
        self.assertEqual(receipt["outcome"],"unknown"); self.assertTrue(receipt["errors"])
        self.assertEqual(self.state['attempts'][self.name],[10000])

    def test_unknown_start_retains_owned_inhibition_for_timer_and_managed_mutation(self):
        def execute(*args):
            if args[1]=='start':raise subprocess.TimeoutExpired(args,120)
            return self.execute(*args)
        self.assertEqual(self.guarded(execute=execute)['outcome'],'unknown')
        marker = json.loads(self.maintenance.read_text())
        self.assertEqual(marker['owner'],'a'*32)
        self.assertEqual(self.state['guarded'][self.name]['phase'],'unknown')
        self.commands=[]
        receipt = self.guarded()
        self.assertEqual(receipt['outcome'],'deferred')
        self.assertFalse(any(c[1]=='start' for c in self.commands))
        self.assertTrue(self.maintenance.exists())
        self.assertEqual(self.state['guarded'][self.name]['phase'],'unknown')

    def test_terminal_record_is_persisted_before_owned_inhibition_is_cleared(self):
        saved = []
        def persist(value):
            saved.append(json.loads(json.dumps(value)))
            if value.get('guarded',{}).get(self.name,{}).get('phase')=='terminal':
                self.assertTrue(self.maintenance.exists())
        receipt = self.guarded(persist=persist)
        self.assertEqual(receipt['outcome'],'started'); self.assertFalse(self.maintenance.exists())
        self.assertEqual(saved[-1]['guarded'][self.name]['receipt']['command_hash'],'b'*64)

    def test_kill_after_inhibition_before_external_call_never_retries(self):
        def persist(value):
            if value.get('guarded',{}).get(self.name,{}).get('phase')=='submitting':raise OSError('injected disk failure')
        with self.assertRaises(OSError):self.guarded(persist=persist)
        self.assertTrue(self.maintenance.exists()); self.assertFalse(any(c[1]=='start' for c in self.commands))
        self.assertFalse(watchdog.recover_terminal_maintenance(self.state,self.maintenance))

    def test_lost_terminal_reply_can_release_only_the_exact_durable_owner_record(self):
        with patch.object(watchdog,'clear_maintenance',side_effect=RuntimeError('injected interruption')):
            with self.assertRaises(RuntimeError):self.guarded()
        self.assertTrue(self.maintenance.exists())
        self.assertEqual(self.state['guarded'][self.name]['phase'],'terminal')
        self.assertTrue(watchdog.recover_terminal_maintenance(self.state,self.maintenance))
        self.assertFalse(self.maintenance.exists())
        self.maintenance.write_text('{"owner":"another-maintenance"}')
        self.assertFalse(watchdog.recover_terminal_maintenance(self.state,self.maintenance))
        self.assertTrue(self.maintenance.exists())

    def test_partial_identity_contract_cannot_fall_back_to_legacy(self):
        for kwargs in ({'expected_instance_generation':None},{'authorization_deadline':None},{'only':None},
                       {'expected_instance_uuid':'not-a-uuid'},{'authorization_deadline':float('nan')}):
            with self.subTest(kwargs=kwargs),self.assertRaises(ValueError):self.guarded(**kwargs)
        self.assertEqual(self.commands,[])

    def test_managed_mutator_cannot_replace_identity_between_read_and_start(self):
        path = Path(self.folder.name)/'admission.lock'; attempted = threading.Event(); mutated = threading.Event(); workers = []
        def mutate():
            attempted.set()
            with watchdog.admission_lock():self.instance['expanded_config']['volatile.uuid']='00000000-0000-4000-8000-000000000009'
            mutated.set()
        def execute(*args):
            result = self.execute(*args)
            if args[1] == 'query' and '/state?' not in args[2]:
                worker = threading.Thread(target=mutate); workers.append(worker); worker.start()
                self.assertTrue(attempted.wait(1)); self.assertFalse(mutated.wait(0.1))
            if args[1] == 'start':self.assertFalse(mutated.is_set())
            return result
        with patch.object(watchdog,'ADMISSION_LOCK',path):
            with watchdog.admission_lock():receipt=self.guarded(execute=execute)
            for worker in workers:worker.join(2)
        self.assertEqual(receipt['outcome'],'started'); self.assertTrue(mutated.is_set())

    def test_attempt_state_is_durably_replaced_before_execution(self):
        path = Path(self.folder.name)/'state.json'
        self.guarded(persist=lambda value:watchdog.save_state(path,value))
        self.assertEqual(json.loads(path.read_text())['attempts'][self.name],[10000])
        self.assertFalse(path.with_suffix('.tmp').exists())

    def test_source_hash_reads_pinned_inode_across_atomic_installation(self):
        path = Path(self.folder.name)/'watchdog'; path.write_bytes(b'approved executing bytes')
        fd = os.open(path,os.O_RDONLY)
        try:
            replacement = path.with_suffix('.new'); replacement.write_bytes(b'new deployment bytes'); os.replace(replacement,path)
            fd_path = ('/proc/self/fd/' if sys.platform.startswith('linux') else '/dev/fd/')+str(fd)
            with patch.object(watchdog,'__file__',fd_path):
                self.assertEqual(watchdog.owner_source_hash(),hashlib.sha256(b'approved executing bytes').hexdigest())
            self.assertNotEqual(watchdog.owner_source_hash(),hashlib.sha256(b'new deployment bytes').hexdigest())
        finally:os.close(fd)

    def test_contract_description_does_not_touch_state_or_resources(self):
        process = subprocess.run([sys.executable,str(ROOT/'deploy/runner-recovery/plurx-runner-watchdog'),
                                  '--describe-owner-contract'],capture_output=True,text=True,timeout=3)
        self.assertEqual(process.returncode,0); value=json.loads(process.stdout)
        self.assertEqual(value['version'],2); self.assertTrue(value['monotonic_authorization'])



class FirecrackerSliceChargeCase(unittest.TestCase):
    """A reserved Firecracker slice keeps its unrealized cap out of VM admission."""

    def setUp(self):
        self.name = "gha-lab6-general-01"
        self.config = {"runners": [{"vm": self.name, "unit": f"forgejo-runner-{self.name}.service",
                                    "memory_gib": 8}],
                       "firecracker_slices": ["cifleetfirecracker.slice"]}
        self.instance = {"name": self.name, "location": "lab6", "status": "Stopped",
                         "expanded_config": {"boot.autostart": "true", "limits.memory": "8GiB"}}
        self.commands = []
        self.slice_show = f"LoadState=loaded\nMemoryMax={3 * watchdog.GIB}\n"
        cgroups = tempfile.TemporaryDirectory()
        self.addCleanup(cgroups.cleanup)
        self.cgroups = Path(cgroups.name)
        for name, value in (("CGROUP_ROOT", self.cgroups), ("total_memory", lambda: 64 * watchdog.GIB)):
            replacing = patch.object(watchdog, name, value)
            replacing.start()
            self.addCleanup(replacing.stop)

    def execute(self, *args):
        self.commands.append(args)
        if args[:2] == ("systemctl", "show") and args[2].endswith(".slice"):
            return self.slice_show
        if args[1] == "list":
            return json.dumps([self.instance])
        if "show" in args:
            return "active"
        return ""

    def run_pass(self, available):
        self.commands.clear()
        return watchdog.recover(self.config, {}, execute=self.execute,
                                memory=lambda: available * watchdog.GIB, host="lab6", now=10000)

    def started(self):
        return any(command[:2] == ("incus", "start") for command in self.commands)

    def test_inactive_slice_is_charged_its_whole_cap(self):
        # 8 GiB guest + 4 GiB reserve: 14 GiB free minus the 3 GiB slice is short.
        result = self.run_pass(14)
        self.assertFalse(self.started())
        self.assertIn("insufficient host memory", result["deferred"][0])
        self.run_pass(15)
        self.assertTrue(self.started())

    def test_used_anonymous_memory_is_credited_but_page_cache_is_not(self):
        (self.cgroups / "cifleetfirecracker.slice").mkdir()
        (self.cgroups / "cifleetfirecracker.slice" / "memory.stat").write_text(
            f"anon {2 * watchdog.GIB}\nfile {8 * watchdog.GIB}\n")
        # Only 1 GiB of the slice is unrealized now.
        self.run_pass(13)
        self.assertTrue(self.started())
        self.run_pass(12)
        self.assertFalse(self.started())

    def test_slice_without_an_enforced_cap_blocks_the_start(self):
        for show in ("LoadState=loaded\nMemoryMax=infinity\n", "LoadState=not-found\nMemoryMax=0\n"):
            self.slice_show = show
            result = self.run_pass(40)
            self.assertFalse(self.started())
            self.assertEqual(result["errors"],
                             [f"{self.name}: cannot account for the reserved Firecracker slice"])

    def test_shared_slices_are_not_reservations(self):
        self.config["firecracker_slices"] = ["system.slice"]
        result = self.run_pass(40)
        self.assertFalse(self.started())
        self.assertEqual(result["errors"], [f"{self.name}: invalid reserved Firecracker slice"])

    def test_hosts_without_firecracker_are_unchanged(self):
        del self.config["firecracker_slices"]
        self.run_pass(12)
        self.assertTrue(self.started())
        self.assertFalse(any(command[0] == "systemctl" and command[2].endswith(".slice")
                             for command in self.commands if len(command) > 2))

    def blocked(self, message, available=40):
        result = self.run_pass(available)
        self.assertFalse(self.started())
        self.assertEqual(result["errors"], [f"{self.name}: {message}"])

    def test_unset_limit_sentinels_and_zero_caps_are_not_reservations(self):
        for cap in (2**64 - 1, 2**63, 64 * watchdog.GIB, 0):
            self.slice_show = f"LoadState=loaded\nMemoryMax={cap}\n"
            self.blocked("cannot account for the reserved Firecracker slice")

    def test_usage_above_the_cap_floors_at_zero(self):
        (self.cgroups / "cifleetfirecracker.slice").mkdir()
        (self.cgroups / "cifleetfirecracker.slice" / "memory.stat").write_text(f"anon {5 * watchdog.GIB}\n")
        self.run_pass(12)
        self.assertTrue(self.started())

    def test_malformed_or_unreadable_usage_blocks_the_start(self):
        group = self.cgroups / "cifleetfirecracker.slice"
        group.mkdir()
        (group / "memory.stat").write_text("file 1\n")
        self.blocked("cannot account for Firecracker slice memory usage")
        (group / "memory.stat").unlink()
        (group / "memory.stat").mkdir()  # reading a directory raises an OSError
        self.blocked("cannot account for Firecracker slice memory usage")

    def test_a_slice_emptied_after_systemctl_show_is_charged_its_whole_cap(self):
        group = self.cgroups / "cifleetfirecracker.slice"
        group.mkdir()
        (group / "memory.stat").write_text(f"anon {2 * watchdog.GIB}\n")
        show = self.slice_show

        def vanish(*args):
            if args[:2] == ("systemctl", "show") and args[2].endswith(".slice"):
                (group / "memory.stat").unlink()
                group.rmdir()
                self.commands.append(args)
                return show
            return FirecrackerSliceChargeCase.execute(self, *args)

        self.execute = vanish
        result = self.run_pass(14)
        self.assertFalse(self.started())
        self.assertIn("insufficient host memory", result["deferred"][0])

    def test_duplicate_nested_and_incus_slices_are_refused(self):
        self.config["firecracker_slices"] = ["cifleetfirecracker.slice"] * 2
        self.blocked("firecracker_slices must list distinct slice names")
        self.config["firecracker_slices"] = ["cifleet-firecracker.slice"]
        self.blocked("reserved Firecracker slice must be top-level; '-' names a nested slice")
        for shared in ("incus.slice", "lxc.slice"):
            self.config["firecracker_slices"] = [shared]
            self.blocked("invalid reserved Firecracker slice")
