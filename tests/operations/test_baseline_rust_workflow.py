"""Contracts for the opt-in, frozen-source Rust continuation; no Rust suite runs here."""
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / '.github/workflows/baseline-rust.yml').read_text()


def heredoc(marker):
    start = SOURCE.index("<<'" + marker + "'\n") + len("<<'" + marker + "'\n")
    return textwrap.dedent(SOURCE[start:SOURCE.index('\n          ' + marker, start)])


def step(name, source=SOURCE):
    tail = source[source.index('      - name: ' + name + '\n'):]
    end = re.search(r'\n      - (?:name|uses|id):', tail)
    return tail[:end.start() + 1] if end else tail


def controller(cell='rust'):
    namespace = {'__name__': 'rust_continuation_contract'}
    with patch.dict(os.environ, {'CONTINUATION_CELL': cell}):
        exec(compile(heredoc('PYPHASE'), '<rust phase observer>', 'exec'), namespace)
    return namespace


def outcome(code=0, cleanup=True):
    return dict(success=code == 0 and cleanup, errors=[], blocked_cells=[],
                cells=[dict(returncode=code, success=code == 0, timed_out=False,
                            cleanup_complete=cleanup, **({} if cleanup else {'cleanup_error': 'group remains live'}))])



def cargo_log(binary, rows):
    passed = sum(status == 'ok' for _, status in rows)
    failed = sum(status == 'FAILED' for _, status in rows)
    ignored = sum(status == 'ignored' for _, status in rows)
    return ('   \x1b[1m\x1b[32mRunning\x1b[0m unittests src/main.rs (target/debug/deps/' + binary + ')\n\n'
            + 'running ' + str(len(rows)) + ' tests\n'
            + ''.join('test ' + name + ' ... ' + status + '\n' for name, status in rows)
            + '\ntest result: ' + ('FAILED' if failed else 'ok') + '. '
            + f'{passed} passed; {failed} failed; {ignored} ignored; 0 measured; 117 filtered out; finished in 0.18s\n')

class RustContinuationWorkflowTests(unittest.TestCase):
    def test_only_opt_in_exact_clean_source_and_immutable_environment(self):
        self.assertIn('  workflow_dispatch:', SOURCE)
        self.assertNotRegex(SOURCE, r'(?m)^  (push|pull_request|schedule):')
        self.assertIn('runs-on: [self-hosted, Linux, X64, lab, general, high-cpu, baremetal]', SOURCE)
        self.assertIn('default: "86f1385005b27122e8a8d3a3d5bd9501eb2fdb16"', SOURCE)
        self.assertIn('ref: ${{ inputs.expected_sha }}', SOURCE)
        self.assertEqual(SOURCE.count('persist-credentials: false'), 2)
        self.assertEqual(SOURCE.count('clean: true'), 2)
        self.assertIn("re.fullmatch('[0-9a-f]{40}', value)", SOURCE)
        self.assertIn("assert actual == expected and not git('status', '--porcelain', '--untracked-files=all')", SOURCE)
        self.assertIn('plurx-ci@sha256:3d84b711936233da699308d0301ac1e91f12e76c6b928967c41817e841f5e855', SOURCE)
        self.assertIn('rust-toolchain@4716b85f2fac3e324e64fa2810f6b5c3905760a5 # 1.97.1', SOURCE)
        for recipe in ['desktop: "true"', 'major: "8"', 'binary: /usr/lib/jellyfin-ffmpeg/ffmpeg', 'lane: rust-gate']:
            self.assertIn(recipe, SOURCE)
        self.assertNotIn('--privileged', SOURCE)
        self.assertNotIn('docker.sock', SOURCE)

    def test_phase_bounds_leave_explicit_job_reserve_and_upload_after_each_phase(self):
        api = controller()
        self.assertEqual(list(api['BUDGETS'].values()), [9000, 600, 1200, 5400, 600, 600])
        bounds = [int(value) for value in re.findall(r'(?m)^        timeout-minutes: (\d+)$', SOURCE)]
        self.assertLessEqual(sum(bounds), 347)
        self.assertIn('    timeout-minutes: 360', SOURCE)
        for phase in api['PHASES']:
            execution = step('Execute ' + phase + ' phase retaining original exit')
            self.assertIn('continue-on-error: true', execution)
            bound = int(re.search(r'timeout-minutes: (\d+)', execution)[1])
            self.assertGreaterEqual(bound * 60, api['BUDGETS'][phase] + 60)
            tail = SOURCE[SOURCE.index(execution) + len(execution):]
            self.assertTrue(tail.startswith('      - name: Preserve ' + phase + ' evidence immediately'))
            upload = step('Preserve ' + phase + ' evidence immediately')
            self.assertIn("if: always() && steps.bind.outcome == 'success'", upload)
            self.assertIn('if-no-files-found: error', upload)
            self.assertIn('path: ${{ runner.temp }}/rust-continuation-${{ inputs.cell }}', upload)
        self.assertLess(SOURCE.index('Execute compile phase'), SOURCE.index('Execute inventory phase'))
        self.assertLess(SOURCE.index('Execute inventory phase'), SOURCE.index('Execute units phase'))

    def test_observer_is_blob_bound_stdlib_and_outside_frozen_checkout(self):
        copied = step('Preserve exact committed safe observer outside tested checkout')
        for check in ["spec = transport + ':' + source", "['git', 'show', spec]", "['git', 'hash-object', '--stdin']",
                      'sys.stdlib_module_names', 'not temporary.is_relative_to(checkout)', 'blob_sha256=digest']:
            self.assertIn(check, copied)
        self.assertLess(SOURCE.index(copied), SOURCE.index('ref: ${{ inputs.expected_sha }}'))
        bound = heredoc('PYBIND')
        for key in ['candidate_sha=actual', 'candidate_tree=tree', 'workflow_transport_sha=transport',
                    'primary_run_id=5620', "receipt['blob_sha256'] == hashlib.sha256(observer.read_bytes()).hexdigest()"]:
            self.assertIn(key, bound)
        self.assertIn('No promotion, publishing or result reuse', bound)
        self.assertIn("actual = api['source_identity']", heredoc('PYPHASE'))
        self.assertIn("context[key]", heredoc('PYPHASE'))

    def test_owner_capture_and_restore_match_existing_container_lane(self):
        browser = (ROOT / '.github/workflows/baseline-browser.yml').read_text()
        for name in ['Capture mounted runner workspace owner', 'Restore persistent runner workspace ownership']:
            observed = re.sub(r'^        timeout-minutes: \d+\n', '', step(name), flags=re.M)
            self.assertEqual(observed.strip(), step(name, browser).strip())

    def test_native_commands_and_ingest_never_recompile(self):
        api = controller()
        output = Path('/tmp/native-artifacts')
        prior = {'compile': {'cells': [{'returncode': 7}]}}
        compile_command = api['command']('compile', output, {})
        self.assertEqual(compile_command[:5], ['bash', '-e', '-o', 'pipefail', '-c'])
        self.assertEqual(compile_command[5], 'cargo test --workspace --locked --exclude plurx-cluster-check --no-fail-fast --no-run --message-format=json > "$1/compiler.jsonl" 2> "$1/compiler.log"')
        wrapped_listing = api['command']('inventory', output, prior)
        self.assertEqual(wrapped_listing[:5], ['bash', '-e', '-o', 'pipefail', '-c'])
        self.assertEqual(wrapped_listing[5], 'cargo metadata --format-version=1 --locked --offline --no-deps > "$1/workspace-metadata.json"; shift; exec "$@"')
        listing = wrapped_listing[8:]
        self.assertEqual(listing[:3], ['python3', '-m', 'validation.baseline_rust_inventory'])
        self.assertEqual(listing[listing.index('--compiler-exit-code') + 1], '7')
        self.assertEqual(listing[listing.index('--list-timeout') + 1], '10')
        self.assertIn('--artifacts', listing)
        self.assertNotIn('--no-run', listing)
        self.assertEqual(api['command']('units', output, {}), api['NATIVE'])
        self.assertEqual(api['command']('static', output, {}), ['make', 'fmt-check', 'spike-lock-check', 'lint', 'test-socket-permission-check'])
        for phase, case in api['CASES'].items():
            self.assertEqual(api['command'](phase, output, {})[-5:], [case, '--', '--exact', '--ignored', '--test-threads=1'])

    def test_compile_redirection_preserves_original_nonzero_exit_and_separate_streams(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            cargo = output / 'cargo'
            cargo.write_text('#!/bin/sh\nprintf "compiler-json\\n"\nprintf "compiler-error\\n" >&2\nexit 7\n')
            cargo.chmod(0o700)
            result = subprocess.run(controller()['command']('compile', output, {}),
                                    env={**os.environ, 'PATH': str(output) + os.pathsep + os.environ['PATH']}, check=False)
            self.assertEqual(result.returncode, 7)
            self.assertEqual((output / 'compiler.jsonl').read_text(), 'compiler-json\n')
            self.assertEqual((output / 'compiler.log').read_text(), 'compiler-error\n')

    def exercise(self, prior, phase, result=None, inventory=True, changed=False, cell='rust', log_kind=None, inventory_kind=None, doctest=None):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            observer = output / 'observer.py'
            observer.write_text('# immutable observer fixture\n')
            (output / 'observer-provenance.json').write_text(json.dumps({'blob_sha256': hashlib.sha256(observer.read_bytes()).hexdigest()}))
            source = dict(candidate_sha='a' * 40, candidate_tree='b' * 40)
            (output / 'source-context.json').write_text(json.dumps(dict(source, cell=cell)))
            (output / 'toolchain-ready').touch()
            (output / 'compiler.jsonl').write_text('{}\n')
            package_id = 'path+file:///frozen/crates/plurxd#plurxd@1.0.0'
            excluded_id = 'path+file:///frozen/crates/plurx-cluster-check#plurx-cluster-check@1.0.0'
            dependency_id = 'registry+https://github.com/rust-lang/crates.io-index#cfg-if@1.0.4'
            library = dict(name='workspace_lib', kind=['lib'], doctest=True, src_path='/frozen/crates/plurxd/src/lib.rs')
            targets = [dict(name='plurxd', kind=['bin'], doctest=True, src_path='/frozen/crates/plurxd/src/main.rs')]
            if doctest in ['workspace', 'both']:
                targets.append(library)
            metadata = dict(version=1, workspace_members=[package_id, excluded_id], packages=[
                dict(id=package_id, name='plurxd', targets=targets),
                dict(id=excluded_id, name='plurx-cluster-check', targets=[dict(name='plurx_cluster_check', kind=['lib'], doctest=True, src_path='/frozen/excluded.rs')])])
            (output / 'workspace-metadata.json').write_text(json.dumps(metadata))
            for key, value in prior.items():
                (output / key).mkdir()
                (output / key / 'results.json').write_text(json.dumps(value))
            api = controller(cell)
            name = 'schema_lineage_upgrade' if cell == 'store-lineage' else 'plurxd'
            binary = output / (name + '-0123456789abcdef')
            binary.write_bytes(b'original binary')
            lineage = 'narrow_schema_lineage_bridge_preserves_and_refuses'
            cases = ([dict(name=lineage, ignored=False)] if cell == 'store-lineage' else
                     [dict(name='ordinary_case', ignored=False), dict(name='deliberately_ignored_case', ignored=True),
                      *[dict(name=case, ignored=True) for case in api['CASES'].values()]])
            if inventory:
                (output / 'inventory').mkdir(exist_ok=True)
                relative = 'crates/plurx-core/tests/schema_lineage_upgrade.rs' if cell == 'store-lineage' else 'crates/plurxd/src/main.rs'
                item = dict(executable=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), cases=cases,
                            target=dict(name=name, kind=['test' if cell == 'store-lineage' else 'bin'], src_path=str(ROOT / relative)))
                candidates = [dict(package_id=excluded_id, target='plurx_cluster_check')]
                if doctest in ['dependency', 'both']:
                    candidates.append(dict(package_id=dependency_id, target='cfg_if'))
                if doctest in ['workspace', 'both']:
                    candidates.append(dict(package_id=package_id, target='workspace_lib'))
                receipt = dict(discovery_complete=True, binaries=[item], doctests=dict(targets=candidates))
                if inventory_kind == 'serial-not-ignored':
                    next(case for case in item['cases'] if case['name'] == api['CASES']['serial-vfr'])['ignored'] = False
                elif inventory_kind == 'wrong-target':
                    item['target']['name'] = 'some_other_binary'
                (output / 'inventory' / 'inventory.json').write_text(json.dumps(receipt))
                if changed:
                    binary.write_bytes(b'changed binary')
            original_run_path = runpy.run_path
            def execute(manifest, destination):
                report = json.loads(json.dumps(result or outcome()))
                selected = cases if phase == 'units' else [dict(name=api['CASES'].get(phase, lineage), ignored=False)]
                rows = [(case['name'], 'ignored' if case['ignored'] else 'ok') for case in selected]
                if report['cells'][0]['returncode'] != 0:
                    rows[0] = (rows[0][0], 'FAILED')
                if log_kind == 'zero-match':
                    rows = []
                elif log_kind == 'missing-case':
                    rows = rows[1:]
                elif log_kind == 'ignored-body':
                    rows = [(name, 'ignored') for name, _ in rows]
                log = cargo_log('other_binary-0123456789abcdef' if log_kind == 'wrong-binary' else binary.name, rows)
                if doctest in ['workspace', 'both'] and log_kind != 'missing-doctest' and cell == 'rust':
                    log += '\n   Doc-tests workspace_lib\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
                (destination / (phase + '.log')).write_text(log)
                report['cells'][0]['log'] = phase + '.log'
                return report
            def load(path):
                return {'source_identity': lambda _: source, 'run_manifest': execute} if Path(path).resolve() == observer.resolve() else original_run_path(path)
            with patch.dict(os.environ, {'PLURX_BASELINE_DIR': directory, 'PLURX_BASELINE_COMMANDS': str(observer)}), patch('runpy.run_path', side_effect=load):
                code = api['main'](phase)
            report = json.loads((output / phase / 'results.json').read_text())
            evidence = output / phase / 'native-evidence.json'
            self.last_native_evidence = json.loads(evidence.read_text()) if evidence.is_file() else None
            return code, report

    def test_regular_native_failure_allows_independent_static_but_blocks_dependent_units(self):
        failure = dict(outcome(7), safe_to_continue=True)
        code, report = self.exercise({'compile': failure, 'inventory': dict(outcome(1), safe_to_continue=True)}, 'static')
        self.assertEqual(code, 0)
        self.assertTrue(report['safe_to_continue'])
        code, report = self.exercise({'compile': failure, 'inventory': dict(outcome(1), safe_to_continue=True),
                                     'static': dict(outcome(), safe_to_continue=True)}, 'units')
        self.assertEqual(code, 1)
        self.assertEqual(report['blocked_cells'], ['units'])
        self.assertIn('compilation did not succeed', report['errors'][0])

    def test_unsafe_or_missing_previous_cleanup_blocks_even_independent_static(self):
        for previous in [{}, dict(outcome(7, False), safe_to_continue=False)]:
            code, report = self.exercise({'compile': previous, 'inventory': dict(outcome(), safe_to_continue=True)}, 'static')
            self.assertEqual(code, 1)
            self.assertFalse(report['safe_to_continue'])
            self.assertEqual(report['blocked_cells'], ['static'])
            self.assertIn('cleanup unqualified', report['errors'][0])

    def test_regular_unit_failure_keeps_serial_phase_independent_and_native_exit(self):
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory', 'static']}
        code, report = self.exercise(prior, 'units', outcome(7))
        self.assertEqual(code, 1)
        self.assertEqual(report['cells'][0]['returncode'], 7)
        self.assertTrue(report['safe_to_continue'])
        self.assertTrue(report['qualification_complete'])
        self.assertFalse(report['cells'][0]['success'])
        self.assertIn('FAILED', [item['status'] for suite in self.last_native_evidence['observed_suites'] for item in suite['outcomes']])
        prior['units'] = report
        code, report = self.exercise(prior, 'serial-vfr')
        self.assertEqual(code, 0)
        self.assertTrue(report['success'])

    def test_missing_inventory_or_changed_binary_blocks_before_bodies(self):
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory', 'static']}
        for settings in [{'inventory': False}, {'changed': True}]:
            with self.subTest(settings=settings):
                code, report = self.exercise(prior, 'units', **settings)
                self.assertEqual(code, 1)
                self.assertEqual(report['cells'], [])
                self.assertEqual(report['blocked_cells'], ['units'])

    def test_zero_exit_missing_binary_or_case_cannot_qualify_ordinary_units(self):
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory', 'static']}
        for log_kind in ['zero-match', 'wrong-binary', 'missing-case']:
            with self.subTest(log_kind=log_kind):
                code, report = self.exercise(prior, 'units', log_kind=log_kind)
                self.assertEqual(code, 1)
                self.assertEqual(report['cells'][0]['returncode'], 0)
                self.assertTrue(report['cells'][0]['success'])
                self.assertTrue(report['safe_to_continue'])
                self.assertFalse(report['qualification_complete'])
                self.assertTrue(report['qualification_gaps'])

    def test_serial_requires_intended_ignored_case_and_one_observed_body(self):
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory', 'static', 'units']}
        for settings in [{'log_kind': 'zero-match'}, {'log_kind': 'wrong-binary'}, {'log_kind': 'ignored-body'},
                         {'inventory_kind': 'serial-not-ignored'}, {'inventory_kind': 'wrong-target'}]:
            with self.subTest(settings=settings):
                code, report = self.exercise(prior, 'serial-vfr', **settings)
                self.assertEqual(code, 1)
                self.assertFalse(report['success'])
                self.assertTrue(report['safe_to_continue'])
        prior['serial-vfr'] = dict(outcome(), safe_to_continue=True)
        code, report = self.exercise(prior, 'serial-bitmap')
        self.assertEqual(code, 0)
        self.assertTrue(report['qualification_complete'])

    def test_explicit_ignored_unit_outcome_is_preserved_as_ignored(self):
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory', 'static']}
        code, report = self.exercise(prior, 'units')
        self.assertEqual(code, 0)
        self.assertTrue(report['qualification_complete'])
        # Native receipts are retained; qualification never rewrites native outcomes.
        self.assertEqual(report['cells'][0]['returncode'], 0)
        ignored = [item for suite in self.last_native_evidence['observed_suites'] for item in suite['outcomes'] if item['name'] == 'deliberately_ignored_case']
        self.assertEqual([item['status'] for item in ignored], ['ignored'])

    def test_fixed_lineage_mode_has_exact_recipe_and_independent_600_second_body(self):
        api = controller('store-lineage')
        self.assertEqual(api['PHASES'], ['compile', 'inventory', 'units'])
        self.assertEqual(api['BUDGETS']['units'], 600)
        expected = ['env', 'RUST_MIN_STACK=8388608', 'cargo', 'test', '--locked', '-p', 'plurx-core',
                    '--features', 'cluster-read-cost-validation,hiqlite-contract-tests', '--test', 'schema_lineage_upgrade', '--', '--test-threads=1']
        self.assertEqual(api['command']('units', Path('/tmp/results'), {}), expected)
        compile_command = api['command']('compile', Path('/tmp/results'), {})
        self.assertIn('env RUST_MIN_STACK=8388608 cargo test --locked -p plurx-core', compile_command[5])
        self.assertIn('--test schema_lineage_upgrade --no-run --message-format=json', compile_command[5])
        self.assertNotIn('--workspace', compile_command[5])
        self.assertNotIn('store_contract', compile_command[5])
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory']}
        code, report = self.exercise(prior, 'units', cell='store-lineage')
        self.assertEqual(code, 0)
        self.assertTrue(report['qualification_complete'])
        code, report = self.exercise(prior, 'units', cell='store-lineage', log_kind='zero-match')
        self.assertEqual(code, 1)
        self.assertFalse(report['qualification_complete'])
        code, report = self.exercise(prior, 'static', cell='store-lineage')
        self.assertEqual(code, 0)
        self.assertEqual(report['status'], 'not_selected')
        self.assertFalse(report['success'])
        self.assertIn("assert cell in ['rust', 'store-lineage']", heredoc('PYBIND'))
        with self.assertRaises(AssertionError):
            controller('arbitrary-shell')

    def test_dependency_doctest_candidates_do_not_require_native_dependency_doctests(self):
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory', 'static']}
        code, report = self.exercise(prior, 'units', doctest='dependency')
        self.assertEqual(code, 0)
        self.assertTrue(report['qualification_complete'])
        # The selected workspace library must still produce a native summary.
        code, report = self.exercise(prior, 'units', doctest='both', log_kind='missing-doctest')
        self.assertEqual(code, 1)
        self.assertFalse(report['qualification_complete'])
        self.assertTrue(any(gap.get('binary') == 'doctest:workspace_lib' for gap in report['qualification_gaps']))
        code, report = self.exercise(prior, 'units', doctest='both')
        self.assertEqual(code, 0)
        self.assertTrue(report['qualification_complete'])

    def test_doctest_scope_uses_metadata_workspace_ids_and_records_unselected_candidates(self):
        workspace = 'path+file:///frozen/core#plurx-core@1.0.0'
        excluded = 'path+file:///frozen/cluster#plurx-cluster-check@1.0.0'
        dependency = 'registry+https://github.com/rust-lang/crates.io-index#cfg-if@1.0.4'
        metadata = dict(version=1, workspace_members=[workspace, excluded], packages=[
            dict(id=workspace, name='plurx-core', targets=[
                dict(name='plurx_core', kind=['lib'], doctest=True, src_path='/frozen/core/src/lib.rs'),
                dict(name='bin_target', kind=['bin'], doctest=True, src_path='/frozen/core/src/main.rs'),
                dict(name='disabled_doc', kind=['lib'], doctest=False, src_path='/frozen/core/disabled.rs')]),
            dict(id=excluded, name='plurx-cluster-check', targets=[
                dict(name='plurx_cluster_check', kind=['lib'], doctest=True, src_path='/frozen/cluster/src/lib.rs')])])
        inventory = dict(doctests=dict(targets=[dict(package_id=workspace, target='plurx_core'),
            dict(package_id=workspace, target='bin_target'), dict(package_id=excluded, target='plurx_cluster_check'),
            dict(package_id=dependency, target='cfg_if')]))
        scope = controller()['doctest_selection'](metadata, inventory)
        self.assertEqual([(item['package_id'], item['target']) for item in scope['required']], [(workspace, 'plurx_core')])
        self.assertEqual({item['target']: item['reason'] for item in scope['not_selected']},
                         dict(bin_target='target not selected for doctests', plurx_cluster_check='excluded workspace package', cfg_if='dependency outside workspace'))
        lineage = controller('store-lineage')['doctest_selection'](metadata, inventory)
        self.assertEqual(lineage['required'], [])
        self.assertEqual(len(lineage['not_selected']), 4)
        self.assertTrue(all(item['reason'] == 'explicit integration --test selection' for item in lineage['not_selected']))
        prior = {key: dict(outcome(), safe_to_continue=True) for key in ['compile', 'inventory']}
        code, report = self.exercise(prior, 'units', cell='store-lineage', doctest='both')
        self.assertEqual(code, 0)
        self.assertTrue(report['qualification_complete'])

    def test_owned_inventory_recipe_preserves_listing_exit_after_bounded_metadata_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / 'cargo').write_text("""#!/bin/sh
[ "$1" = metadata ] || exit 99
printf '%s\\n' '{"version":1,"workspace_members":["fixture"],"packages":[]}'
""")
            (output / 'python3').write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$FIXTURE_ARGS"\nexit 7\n')
            (output / 'cargo').chmod(0o700)
            (output / 'python3').chmod(0o700)
            command = controller()['command']('inventory', output, {'compile': {'cells': [{'returncode': 17}]}})
            environment = dict(os.environ, PATH=str(output) + os.pathsep + os.environ['PATH'], FIXTURE_ARGS=str(output / 'listing-args'))
            result = subprocess.run(command, env=environment, capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(json.loads((output / 'workspace-metadata.json').read_text())['version'], 1)
            args = (output / 'listing-args').read_text().splitlines()
            self.assertEqual(args[args.index('--compiler-exit-code') + 1], '17')
            self.assertEqual(args[args.index('--list-timeout') + 1], '10')
            self.assertIn('--artifacts', args)

    def test_embedded_python_and_all_shell_recipes_parse(self):
        for marker in ['PYCOPY', 'PYBIND', 'PYPHASE']:
            compile(heredoc(marker), '<' + marker + '>', 'exec')
        # Every run block, including Python and shell heredocs, must parse as bash.
        for block in re.findall(r'(?ms)^        run: \|\n(.*?)(?=^      - |\Z)', SOURCE):
            result = subprocess.run(['bash', '-n'], input=textwrap.dedent(block), text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
