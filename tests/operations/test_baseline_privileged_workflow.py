"""Confinement and frozen-source contracts for the diagnostic mount workflow."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

from validation.baseline_commands import normalize

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / '.github/workflows/baseline-privileged.yml').read_text()


def step(name, source=SOURCE):
    start = source.index('      - name: ' + name + '\n')
    tail = source[start:]
    match = re.search(r'\n      - (?:name|uses|id):', tail)
    return tail[:match.start() + 1] if match else tail


def heredoc(marker, source=SOURCE):
    start = source.index("<<'" + marker + "'\n") + len("<<'" + marker + "'\n")
    return textwrap.dedent(source[start:source.index('\n          ' + marker, start)])


class BaselinePrivilegedWorkflowTests(unittest.TestCase):
    def test_only_manual_exact_candidate_checkout_and_minimal_capability(self):
        self.assertIn('  workflow_dispatch:', SOURCE)
        self.assertNotRegex(SOURCE, r'(?m)^  (push|pull_request|schedule):')
        self.assertIn('ref: ${{ inputs.expected_sha }}', SOURCE)
        self.assertIn('clean: true', SOURCE)
        self.assertIn('persist-credentials: false', SOURCE)
        self.assertIn('--cap-add SYS_ADMIN --security-opt seccomp=unconfined', SOURCE)
        self.assertIn('--security-opt apparmor=unconfined', SOURCE)
        self.assertIn('runs-on: [self-hosted, Linux, X64, lab, baremetal, general, high-cpu]', SOURCE)
        self.assertNotIn('--privileged', SOURCE)
        self.assertNotIn('--device', SOURCE)
        self.assertNotRegex(SOURCE, r'(?m)^\s+volumes:')
        container_options = re.search(r'(?m)^      options: (.+)$', SOURCE)[1]
        self.assertNotRegex(container_options, r'--(?:volume|mount)\b')
        self.assertIn('test -z "$(git status --porcelain --untracked-files=normal)"', SOURCE)
        self.assertIn('"$(git rev-parse HEAD)" = "$EXPECTED_SHA"', SOURCE)

    def test_owner_capture_restore_and_immutable_build_environment_match_check_job(self):
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        check = ci.split('\n  check:\n', 1)[1].split('\n  cluster_store_legacy:\n', 1)[0]
        for name in ('Capture mounted runner workspace owner', 'Restore persistent runner workspace ownership'):
            self.assertEqual(step(name).strip(), step(name, check).strip())
        self.assertIn('plurx-ci@sha256:3d84b711936233da699308d0301ac1e91f12e76c6b928967c41817e841f5e855', SOURCE)
        self.assertIn('rust-toolchain@4716b85f2fac3e324e64fa2810f6b5c3905760a5 # 1.97.1', SOURCE)
        self.assertIn('binary: /usr/lib/jellyfin-ffmpeg/ffmpeg', SOURCE)
        self.assertIn('lane: rust-gate', SOURCE)

    def test_docker_socket_endpoint_invalid_sha_and_nonroot_are_unqualified(self):
        code = heredoc('PYCONFINEMENT')
        cases = [({'EXPECTED_SHA': 'bad'}, False, 0), ({}, True, 0),
                 ({'DOCKER_HOST': 'private-value-never-print'}, False, 0), ({}, False, 1000)]
        for override, socket, uid in cases:
            with self.subTest(override=override, socket=socket, uid=uid), tempfile.TemporaryDirectory() as directory:
                (Path(directory) / 'privileged-baseline').mkdir()
                environment = {'RUNNER_TEMP': directory, 'EXPECTED_SHA': 'a' * 40, **override}
                with patch.dict(os.environ, environment, clear=True), patch.object(os, 'geteuid', return_value=uid), patch.object(Path, 'exists', return_value=socket):
                    with self.assertRaises(SystemExit):
                        exec(compile(code, '<confinement>', 'exec'), {})
                receipt = json.loads((Path(directory) / 'privileged-baseline/confinement.json').read_text())
                self.assertFalse(receipt['qualified'])
                self.assertNotIn('private-value', json.dumps(receipt))

    def test_independent_manifest_is_candidate_bound_and_caps_each_original_make_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            context = {'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40}
            (root / 'source-context.json').write_text(json.dumps(context))
            with patch.dict(os.environ, {'PLURX_PRIVILEGED_DIR': directory}):
                exec(compile(heredoc('PYMANIFEST'), '<manifest>', 'exec'), {})
            manifest = json.loads((root / 'manifest.json').read_text())
            cells = normalize(manifest, ROOT)
            self.assertEqual(manifest['candidate_sha'], context['candidate_sha'])
            self.assertEqual(manifest['candidate_tree'], context['candidate_tree'])
            self.assertEqual([cell['argv'][-2:] for cell in cells], [['make', 'bind-mount-check'], ['make', 'storage-pressure-check']])
            self.assertTrue(all(cell['timeout_seconds'] == 1200 for cell in cells))
            self.assertTrue(all(cell['argv'][:4] == ['unshare', '--mount', '--propagation', 'private'] for cell in cells))

    def test_missing_sys_admin_fails_before_setup_with_kernel_qualification_receipt(self):
        self.check_kernel_qualification('0000000000000000', False)

    def test_effective_sys_admin_and_lsm_are_recorded_without_assuming_probe_passed(self):
        self.check_kernel_qualification('0000000000200000', True)

    def check_kernel_qualification(self, effective, expected_capability):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory) / 'privileged-baseline'
            evidence.mkdir()
            original_read = Path.read_text
            def read(path, *args, **kwargs):
                if str(path) == '/proc/self/status':
                    return f'CapEff:\t{effective}\nCapBnd:\t0000000000200000\nSeccomp:\t0\nNoNewPrivs:\t0\n'
                if str(path) == '/proc/self/attr/current':
                    return 'unconfined\n'
                return original_read(path, *args, **kwargs)
            environment = {'RUNNER_TEMP': directory, 'EXPECTED_SHA': 'a' * 40}
            with patch.dict(os.environ, environment, clear=True), patch.object(os, 'geteuid', return_value=0), patch.object(Path, 'exists', return_value=False), patch.object(Path, 'read_text', autospec=True, side_effect=read):
                if expected_capability:
                    exec(compile(heredoc('PYCONFINEMENT'), '<confinement>', 'exec'), {})
                else:
                    with self.assertRaisesRegex(SystemExit, 'CAP_SYS_ADMIN'):
                        exec(compile(heredoc('PYCONFINEMENT'), '<confinement>', 'exec'), {})
            confinement = json.loads((evidence / 'confinement.json').read_text())
            qualification = json.loads((evidence / 'capability.json').read_text())
            self.assertEqual(confinement['cap_sys_admin_effective'], expected_capability)
            self.assertEqual(confinement['qualified'], expected_capability)
            self.assertFalse(qualification['qualified'])
            self.assertEqual(qualification['kernel'], {'CapEff': effective, 'CapBnd': '0000000000200000', 'Seccomp': '0', 'NoNewPrivs': '0', 'lsm_current': 'unconfined'})
            self.assertLess(SOURCE.index('PYCONFINEMENT'), SOURCE.index('rust-toolchain@'))

    def test_namespace_wrapper_is_valid_shell_and_uses_only_private_temp_probe(self):
        code = heredoc('PRIVATECELL')
        with tempfile.NamedTemporaryFile(mode='w', suffix='.sh') as source:
            source.write(code)
            source.flush()
            subprocess.run(['bash', '-n', source.name], check=True)
        self.assertIn('export TMPDIR="$private_tmp"', code)
        self.assertIn('mktemp -d "$RUNNER_TEMP/plurx-private-cell.XXXXXX"', code)
        self.assertIn('mount --make-rprivate /', code)
        self.assertIn('mount -t tmpfs -o size=64k,nodev,nosuid tmpfs "$probe"', code)
        self.assertLess(code.rindex('umount "$probe"'), code.rindex('"$@"'))
        self.assertIn('trap cleanup EXIT', code)

    def test_failure_receipts_and_artifacts_are_always_retained(self):
        self.assertIn('unqualified; privileged test cells not executed', SOURCE)
        self.assertIn('if [ "$capability_result" -ne 0 ]', SOURCE)
        self.assertIn('validation.baseline_commands', step('Execute independent privileged test cells'))
        for name in ('Finish resource recording', 'Preserve privileged diagnostic evidence', 'Restore persistent runner workspace ownership'):
            self.assertIn('if: always()', step(name))
        self.assertIn('path: ${{ runner.temp }}/privileged-baseline', SOURCE)
        self.assertIn('source-context.json', SOURCE)


if __name__ == '__main__':
    unittest.main()
