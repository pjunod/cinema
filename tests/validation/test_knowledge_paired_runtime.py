"""Synthetic stage/identity contracts; run only after independent review.

No private cases, actual graders, providers, Docker, or candidate execution.
"""
from dataclasses import replace
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from validation.knowledge import evaluation_isolation as isolation
from validation.knowledge.study_driver import paired_runtime as paired
from validation.knowledge.study_driver import runtime_bindings as bindings


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


class PairedRuntimeContracts(unittest.TestCase):
    def setUp(self):
        base = (Path(__file__).resolve().parents[2] /
                'target/agent-fixtures/.hermes/hdr-collaboration/workspaces/paired-runtime-tests')
        base.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=base, prefix='owned-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.lock = b'# synthetic dependency lock\n'
        self.baseline = SimpleNamespace(commit='a' * 40, expected='b' * 64,
                                        read_raw=lambda path: self.lock)
        self.image = 'sha256:' + 'c' * 64
        self.receipt = {
            'schema_version': 1, 'kind': 'locked-dependency-image',
            'image_id': self.image, 'base_image_id': paired.DependencyImageSeed.BASE_IMAGE,
            'base_rootfs_diff_ids': ['sha256:' + 'd' * 64],
            'image_rootfs_diff_ids': ['sha256:' + 'd' * 64, 'sha256:' + 'e' * 64],
            'source_commit': self.baseline.commit, 'source_archive_sha256': self.baseline.expected,
            'cargo_lock_sha256': digest(self.lock),
            'dependency_fetch_receipt_sha256': paired.DependencyImageSeed.FETCH_RECEIPT_SHA256,
            'vendor_manifest_sha256': '1' * 64, 'dockerfile_sha256': '2' * 64,
            'context_manifest_sha256': '3' * 64, 'vendor_directory': '/opt/locked-crates',
        }
        self.path = self.root / 'receipt.json'

    def seed(self, body=None, *, reviewed=True):
        raw = json.dumps(self.receipt if body is None else body).encode()
        self.path.write_bytes(raw)
        return paired.DependencyImageSeed(self.path, digest(raw), image=self.image,
            baseline=self.baseline, admission=SimpleNamespace(assert_current=lambda: None),
            review_gate=lambda: reviewed)

    def test_dependency_seed_only_writes_exact_offline_config(self):
        runtime = self.root / 'runtime'
        runtime.mkdir(mode=0o700)
        (runtime / 'cargo').mkdir(mode=0o700)
        (runtime / 'unrelated').write_bytes(b'preserve')
        seed = self.seed()
        with patch.object(paired, 'runtime_layout', side_effect=AssertionError('extra runtime writes')):
            result = seed(runtime)
        raw = (runtime / 'cargo/config.toml').read_bytes()
        expected = ('# dependency image acquisition sha256:' + seed.receipt_sha256 + '\n'
            '[source.crates-io]\nreplace-with = "locked-pilot"\n'
            '[source.locked-pilot]\ndirectory = "/opt/locked-crates"\n'
            '[net]\noffline = true\n').encode()
        self.assertEqual(raw, expected)
        self.assertEqual(result, digest(raw))
        self.assertEqual(seed(runtime), result)
        self.assertEqual((runtime / 'unrelated').read_bytes(), b'preserve')
        self.assertEqual((runtime / 'cargo').stat().st_mode & 0o777, 0o700)
        self.assertEqual((runtime / 'cargo/config.toml').stat().st_mode & 0o777, 0o644)
        self.assertEqual({p.relative_to(runtime).as_posix() for p in runtime.rglob('*')},
                         {'cargo', 'cargo/config.toml', 'unrelated'})

    def test_dependency_receipt_rejects_rehashed_wrong_inputs(self):
        changes = (
            {'schema_version': True}, {'extra': 'unapproved'},
            {'base_image_id': 'sha256:' + 'f' * 64},
            {'dependency_fetch_receipt_sha256': 'f' * 64},
            {'source_archive_sha256': 'f' * 64}, {'source_commit': 'f' * 40},
            {'cargo_lock_sha256': 'f' * 64}, {'vendor_directory': '/registry'},
            {'image_id': paired.DependencyImageSeed.BASE_IMAGE},
            {'image_rootfs_diff_ids': self.receipt['base_rootfs_diff_ids']},
            {'image_rootfs_diff_ids': ['sha256:' + 'f' * 64, 'sha256:' + 'e' * 64]},
            {'vendor_manifest_sha256': 'invalid'},
        )
        for change in changes:
            with self.subTest(change=change), self.assertRaises(bindings.BindingRefusal):
                self.seed(dict(self.receipt, **change))._verified()
        duplicate = json.dumps(self.receipt).removesuffix('}') + ',"kind":"locked-dependency-image"}'
        seed = self.seed()
        self.path.write_text(duplicate)
        seed.receipt_sha256 = digest(duplicate.encode())
        with self.assertRaises(bindings.BindingRefusal): seed._verified()

    def test_dependency_seed_requires_review_and_rejects_config_conflict(self):
        runtime = self.root / 'runtime'
        runtime.mkdir()
        (runtime / 'cargo').mkdir()
        with self.assertRaises(bindings.BindingRefusal): self.seed(reviewed=False)(runtime)
        (runtime / 'cargo/config.toml').write_text('[net]\noffline = false\n')
        with self.assertRaises(bindings.BindingRefusal): self.seed()(runtime)
        (runtime / 'cargo/config.toml').unlink()
        seed = self.seed()
        self.path.write_bytes(b'changed')
        with self.assertRaises(bindings.BindingRefusal): seed(runtime)
        self.assertFalse((runtime / 'cargo/config.toml').exists())

    def worker_plan(self, suffix):
        config = isolation.IsolationConfig(engine=Path('/usr/bin/false'), endpoint='unix:///synthetic',
            image=self.image, owned_root=self.root / suffix, candidate_root=self.root / suffix / 'candidate',
            grader_root=self.root / suffix / 'grader', runtime_root=self.root / suffix / 'runtime',
            acceptance_argv=(('/usr/bin/python3', '/grader/worker.py'),), uid=1000, gid=1000,
            candidate_readonly=True)
        source = isolation.TrustedSourceFile('worker.py', '1' * 64)
        return isolation.ProtectedWorkerPlan(config, source, config.acceptance_argv[0], ('health',),
                                             '2' * 64, '3' * 64)

    def test_family_identity_rebinds_paths_but_pins_worker_policy(self):
        step = isolation.ExecutionStep('check', ('/usr/bin/python3', '/grader/check.py', '/work'))
        plan = isolation.FamilyPlanV2('synthetic', (replace(step, kind='prerequisite'),), (step,),
            isolation.TrustedSourceFile('protected.py', '4' * 64), worker=self.worker_plan('a'))
        rebound = replace(plan, worker=self.worker_plan('b'))
        self.assertEqual(paired.family_identity(plan), paired.family_identity(rebound))
        for changes in ({'uid': 1001}, {'image': 'sha256:' + 'f' * 64}, {'memory_bytes': 2**31}):
            altered = replace(rebound, worker=replace(rebound.worker,
                              config=replace(rebound.worker.config, **changes)))
            self.assertNotEqual(paired.family_identity(plan), paired.family_identity(altered))
        self.assertNotEqual(paired.family_identity(plan), paired.family_identity(
            replace(plan, acceptance_steps=(replace(step, environment=(('SYNTHETIC', '1'),)),))))

    def test_identical_command_selects_exact_setup_or_acceptance_mount_policy(self):
        owner = object.__new__(bindings.SourceStager)
        owner.api = isolation
        owner.admission = SimpleNamespace(assert_current=lambda: None)
        owner.engine, owner.endpoint, owner.image = Path('/usr/bin/false'), 'unix:///synthetic', self.image
        owner.python, owner.rustc, owner.cargo = '/usr/bin/python3', '/usr/bin/rustc', '/usr/bin/cargo'
        owner.execution_options = {}
        owner.baseline = SimpleNamespace(commit='a' * 40, expected='b' * 64, exceptions={}, permits={})
        argv = ('/usr/bin/python3', '/grader/check.py', '/work')
        setup = isolation.ExecutionStep('setup', argv, writes=frozenset({'candidate'}),
                                        kind='setup', protected_readonly=False)
        check = isolation.ExecutionStep('check', argv)
        stage = bindings.Stage(self.root, self.root / 'candidate', self.root / 'grader', self.root / 'runtime')
        stage.family_v2 = isolation.FamilyPlanV2('synthetic', (setup,), (check,),
                                                isolation.TrustedSourceFile('protected.py', '4' * 64))
        command = owner.step_command(check)
        self.assertTrue(owner.configuration(stage, command).candidate_readonly)
        self.assertFalse(owner.configuration(stage, command, _step=setup).candidate_readonly)
        with self.assertRaises(bindings.BindingRefusal):
            owner.configuration(stage, command, _step=replace(setup))

    def test_copy_final_runtime_preserves_exact_files_and_rejects_wrong_lineage(self):
        source, target = self.root / 'source', self.root / 'target'
        source.mkdir(); target.mkdir()
        (source / 'installed').write_bytes(b'synthetic non-idempotent setup result')
        (source / 'installed').chmod(0o644)
        expected = paired.measure_tree(source)
        paired.copy_qualified_runtime(source, target, expected)
        self.assertEqual(paired.measure_tree(target), expected)
        (source / 'installed').write_bytes(b'changed after qualification')
        with self.assertRaises(bindings.BindingRefusal):
            paired.copy_qualified_runtime(source, target, expected)
        self.assertEqual(paired.measure_tree(target), expected)

    def test_nonidempotent_setup_adopts_runtimes_and_keeps_initial_protected_source(self):
        import uuid
        owner = object.__new__(bindings.SourceStager)
        owner.root = self.root
        owner.api = isolation
        owner.admission = SimpleNamespace(assert_current=lambda: None)
        stages = []
        for name in ('qualified', 'current'):
            root = self.root / uuid.uuid4().hex
            root.mkdir(mode=0o700)
            stage = bindings.Stage(root, root / 'candidate', root / 'grader', root / 'runtime')
            for path in (stage.candidate, stage.grader, stage.runtime): path.mkdir()
            (stage.candidate / 'synthetic.py').write_bytes(b'initial protected source')
            (stage.candidate / '.runtime').mkdir()
            stage.worker_runtime = root / 'worker-runtime'; stage.worker_runtime.mkdir()
            stage.worker_candidate = root / 'worker-source'; stage.worker_candidate.mkdir()
            stage.paired_role = object()
            owner._register_stage(stage)
            stages.append(stage)
        baseline, current = stages
        owner._stage_record(baseline)["pinned"] = True
        current.paired_role = baseline.paired_role
        initial = owner.source_manifest(current.candidate)
        (baseline.candidate / 'synthetic.py').write_bytes(b'setup rewrote its private helper')
        (baseline.runtime / 'installed').write_bytes(b'protected final runtime')
        (baseline.runtime / 'quiet-logs').mkdir()
        (baseline.runtime / 'quiet-logs/check.log').write_bytes(b'protected wrapper log')
        (baseline.worker_runtime / 'compiled').write_bytes(b'worker final runtime')
        body = {'schema_version': 2, 'status': 'qualified', 'initial_protected_source': initial,
                'after': {'runtime': paired.measure_tree(baseline.runtime),
                          'worker_runtime': paired.measure_tree(baseline.worker_runtime)}}
        private = baseline.root / '.isolation'; private.mkdir(mode=0o700)
        receipt = private / ('prerequisite-' + uuid.uuid4().hex + '.json')
        receipt_digest = isolation._write_private(receipt, body)
        prerequisite = isolation.PrerequisiteReceipt('qualified', receipt_digest, receipt, 'synthetic', 'f' * 64)
        owner.paired = object.__new__(paired.PairedRoleStager); owner.paired.owner = owner
        owner.paired.bind_plan = lambda stage, plan: plan
        with patch.dict(isolation._QUALIFIED, {id(prerequisite): (prerequisite, body)}):
            owner.adopt_qualified_runtime(current, baseline, prerequisite)
        self.assertEqual((current.candidate / 'synthetic.py').read_bytes(), b'initial protected source')
        self.assertEqual(paired.measure_tree(current.runtime), body['after']['runtime'])
        self.assertEqual(paired.measure_tree(current.worker_runtime), body['after']['worker_runtime'])
        self.assertEqual((current.runtime / 'quiet-logs/check.log').read_bytes(), b'protected wrapper log')
        self.assertFalse((current.worker_runtime / 'quiet-logs').exists())


if __name__ == '__main__':
    unittest.main()
