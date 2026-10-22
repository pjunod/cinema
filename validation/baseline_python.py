"""Run native unittest discovery and exact outcome accounting in a controlled cell.

Discovery imports test modules, so invoke this only in the intended CI/toolchain
cell, never merely to count tests on a shared host. The JSON artifact binds the
candidate source, cell configuration, inventory, outcomes and per-test durations.
It is diagnostic provenance, not authenticated evidence for historical adoption.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
import re
import runpy
from pathlib import Path
import subprocess
import sys
import time
import unittest

from validation.test_cell_manifest import ManifestError, assess_cell, canonical_context, require


def verify_source(context, root, environment, source_receipt=None):
    """Refuse a different candidate before importing any test modules.

    Source-only toolchain archives have no Git metadata. For those, the caller
    supplies its transfer receipt; the archive digest is retained, not silently
    treated as verified against an archive that no longer exists on this host.
    """
    for field, variable in [('candidate_sha', 'PLURX_TEST_CANDIDATE_SHA'),
                            ('candidate_tree', 'PLURX_TEST_CANDIDATE_TREE')]:
        require(environment.get(variable) == context[field], f'{variable} differs from context')
    provenance = {'mode': 'git'}
    if (root / '.git').exists():
        def git(*args):
            return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
        require(git('rev-parse', 'HEAD') == context['candidate_sha'], 'Checkout candidate differs')
        require(git('rev-parse', 'HEAD^{tree}') == context['candidate_tree'], 'Checkout tree differs')
        require(not git('status', '--porcelain', '--untracked-files=all'),
                'Checkout must be clean before runtime discovery')
    else:
        require(source_receipt is not None, 'Source-only checkout requires a transfer receipt')
        receipt = json.loads(Path(source_receipt).read_text())
        require(isinstance(receipt, dict), 'Source receipt must be an object')
        for field in ('candidate_sha', 'candidate_tree'):
            require(receipt.get(field) == context[field], f'Source receipt {field} differs')
        digest = receipt.get('archive_sha256')
        require(isinstance(digest, str) and len(digest) == 64
                and all(char in '0123456789abcdef' for char in digest), 'Invalid archive digest')
        provenance = {'mode': 'source-transfer-receipt', 'receipt': receipt,
                      'receipt_sha256': hashlib.sha256(Path(source_receipt).read_bytes()).hexdigest()}
    return provenance


def flatten(suite):
    for item in suite:
        if isinstance(item, unittest.TestSuite):
            yield from flatten(item)
        else:
            yield item


class RecordingResult(unittest.TextTestResult):
    """One row per discovered method, with subtest failures bound to the parent."""
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.outcomes = []
        self.observations = []
        self.durations_seconds = {}
        self._status = {}
        self._started = {}

    @staticmethod
    def _parent(test):
        return getattr(test, 'test_case', test)

    def _record(self, test, status, detail=None):
        name = self._parent(test).id()
        # A later successful subtest must not erase another subtest's failure.
        rank = {'passed': 0, 'skipped': 1, 'failed': 2, 'error': 3}
        if rank[status] > rank.get(self._status.get(name), -1):
            self._status[name] = status
        if detail:
            self.observations.append({'id': name, 'status': status, 'detail': str(detail)})

    def startTest(self, test):
        super().startTest(test)
        self._started[test.id()] = time.monotonic()

    def stopTest(self, test):
        name = test.id()
        self.durations_seconds[name] = time.monotonic() - self._started[name]
        self.outcomes.append({'id': name, 'status': self._status.get(name, 'error')})
        super().stopTest(test)

    def addSuccess(self, test):
        super().addSuccess(test)
        self._record(test, 'passed')

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self._record(test, 'failed', f'{err[0].__name__}: {err[1]}')

    def addError(self, test, err):
        super().addError(test, err)
        self._record(test, 'error', f'{err[0].__name__}: {err[1]}')

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self._record(test, 'skipped', reason)

    def addExpectedFailure(self, test, err):
        super().addExpectedFailure(test, err)
        self._record(test, 'skipped', 'unittest expected failure')

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        self._record(test, 'failed', 'unittest unexpected success')

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            self._record(test, 'failed' if issubclass(err[0], test.failureException) else 'error')


def collect_entrypoint(context, directory, root, provenance, *, environment, source_receipt,
                       exceptions=(), stream=None):
    """Preserve a supported script's __main__ initialization and unittest.main.

    Only its runner is injected. Native argument parsing, suite construction,
    outcomes and SystemExit behavior remain intact. Inventory is captured at
    runner entry, before any test body runs. A script without a literal native
    unittest.main call is refused before execution (notably decoder campaigns).
    """
    filename = context.get('entrypoint')
    require(isinstance(filename, str) and re.fullmatch(r'test_[A-Za-z0-9_]+\.py', filename),
            'Entrypoint must name an exact local test script')
    require(context.get('test_files') == [filename], 'Entrypoint requires a one-file allowlist')
    script = directory / filename
    require(script.is_file(), 'Missing unittest entrypoint script')
    cwd_name = context.get('cwd')
    require(isinstance(cwd_name, str), 'Entrypoint context must bind cwd')
    cwd = (root / cwd_name).resolve()
    require(cwd.is_relative_to(root) and cwd.is_dir(), 'Entrypoint cwd must be inside source root')
    arguments = context.get('discovery_argv')
    require(isinstance(arguments, list) and all(isinstance(value, str)
            and not any(ord(char) < 32 for char in value) for value in arguments),
            'Entrypoint discovery_argv must be an explicit string list')
    syntax = ast.parse(script.read_text(), filename=str(script))
    has_native_main = any(isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                          and node.func.attr == 'main' and isinstance(node.func.value, ast.Name)
                          and node.func.value.id == 'unittest' for node in ast.walk(syntax))
    require(has_native_main, 'Entrypoint has no literal unittest.main; refusing non-unit campaign')
    framework_calls, runs = [], []
    started = time.monotonic()
    discovery_seconds, execution_seconds = 0.0, 0.0
    original_main = unittest.main

    class EntrypointRunner(unittest.TextTestRunner):
        def __init__(self, *args, **kwargs):
            kwargs['resultclass'] = RecordingResult
            kwargs['stream'] = sys.stderr if stream is None else stream
            super().__init__(*args, **kwargs)

        def run(self, suite):
            nonlocal discovery_seconds, execution_seconds
            inventory = [test.id() for test in flatten(suite)]
            discovery_seconds = time.monotonic() - started
            execution_started = time.monotonic()
            result = super().run(suite)
            execution_seconds += time.monotonic() - execution_started
            runs.append((inventory, result))
            return result

    def native_main(*args, **kwargs):
        framework_calls.append(True)
        require(kwargs.get('testRunner') is None, 'Custom unittest runner requires an explicit adapter')
        kwargs['testRunner'] = EntrypointRunner
        return original_main(*args, **kwargs)

    original_argv, original_path, original_cwd = sys.argv, list(sys.path), Path.cwd()
    exit_code, entrypoint_error = 0, None
    unittest.main = native_main
    sys.argv = [str(script), *arguments]
    sys.path.insert(0, str(directory))
    try:
        os.chdir(cwd)
        try:
            runpy.run_path(str(script), run_name='__main__')
        except SystemExit as error:
            exit_code = int(error.code) if isinstance(error.code, int) else (0 if error.code is None else 1)
        except Exception as error:
            entrypoint_error = f'{type(error).__name__}: {error}'
            exit_code = 1
    finally:
        unittest.main = original_main
        sys.argv = original_argv
        sys.path[:] = original_path
        os.chdir(original_cwd)
    scope_errors = []
    if len(framework_calls) != 1 or len(runs) != 1:
        scope_errors.append('Entrypoint must invoke native unittest.main and its runner exactly once')
    try:
        require(verify_source(context, root, environment, source_receipt) == provenance,
                'Source transfer receipt changed during execution')
    except ManifestError as error:
        scope_errors.append(f'Source changed during entrypoint execution: {error}')
    inventory = [name for discovered, _ in runs for name in discovered]
    outcomes = [row for _, result in runs for row in result.outcomes]
    if any(not result.wasSuccessful() for _, result in runs):
        exit_code = exit_code or 1
    report = assess_cell(context, inventory, outcomes, discovery_returncode=0 if runs else 1,
                         execution_returncode=exit_code, exceptions=exceptions, scope_errors=scope_errors)
    report.update({'source_provenance': provenance, 'mode': 'entrypoint',
                   'discovery_seconds': discovery_seconds, 'execution_seconds': execution_seconds,
                   'framework_calls': len(framework_calls), 'entrypoint_error': entrypoint_error,
                   'durations_seconds': {name: duration for _, result in runs
                                         for name, duration in result.durations_seconds.items()},
                   'observations': [row for _, result in runs for row in result.observations]})
    return report


def collect(context, suite_dir, root, *, environment=None, source_receipt=None,
            exceptions=(), loader=None, stream=None):
    context, _ = canonical_context(context)
    require(context['framework'] == 'unittest', 'Python cell framework must be unittest')
    require(context.get('suite_dir') == suite_dir, 'Context suite_dir differs from requested discovery')
    root = Path(root).resolve()
    provenance = verify_source(context, root, os.environ if environment is None else environment,
                               source_receipt)
    directory = (root / suite_dir).resolve()
    require(directory.is_relative_to(root) and directory.is_dir(),
            'Suite directory must exist inside the source root')
    if context.get('mode') == 'entrypoint':
        return collect_entrypoint(context, directory, root, provenance,
                                  environment=os.environ if environment is None else environment,
                                  source_receipt=source_receipt, exceptions=exceptions, stream=stream)
    require(context.get('mode', 'discover') == 'discover', 'Unknown Python baseline mode')
    selected_files = context.get('test_files')
    discovery_argv = context.get('discovery_argv', [])
    require(isinstance(discovery_argv, list) and all(isinstance(value, str)
            and not any(ord(char) < 32 for char in value) for value in discovery_argv),
            'discovery_argv must be an explicit string list')
    if selected_files is None:
        require(not discovery_argv, 'discovery_argv requires an exact test_files allowlist')
        patterns = ['test_*.py']
        sources = sorted(directory.glob('test_*.py'))
    else:
        require(isinstance(selected_files, list) and selected_files
                and all(isinstance(name, str) and re.fullmatch(r'test_[A-Za-z0-9_]+\.py', name)
                        for name in selected_files), 'test_files must name exact local test modules')
        require(len(selected_files) == len(set(selected_files)), 'Duplicate selected test file')
        require(all((directory / name).is_file() for name in selected_files),
                'Selected test file does not exist')
        patterns = selected_files
        sources = [directory / name for name in selected_files]
    function_controls = []
    source_inspection_errors = []
    for source in sources:
        try:
            tree = ast.parse(source.read_text(), filename=str(source))
        except SyntaxError as error:
            source_inspection_errors.append({'path': source.relative_to(root).as_posix(),
                                             'error': str(error)})
            continue
        names = [node.name for node in tree.body
                 if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                 and node.name.startswith('test_')]
        if names:
            function_controls.append({'path': source.relative_to(root).as_posix(),
                                      'functions': names,
                                      'classification': 'not-native-unittest-methods'})
    loader = unittest.TestLoader() if loader is None else loader
    started = time.monotonic()
    suite = unittest.TestSuite()
    for pattern in patterns:
        original_argv = sys.argv
        # These source-bound arguments are reset for each module: some older
        # evidence checkers consume their fixture argument during import.
        sys.argv = [str(directory / pattern), *discovery_argv]
        try:
            suite.addTests(loader.discover(str(directory), pattern=pattern))
        finally:
            sys.argv = original_argv
    inventory = [test.id() for test in flatten(suite)]
    discovery_seconds = time.monotonic() - started
    errors = list(loader.errors)
    outcomes, observations, durations = [], [], {}
    execution_returncode = 1
    execution_seconds = 0.0
    if not errors:
        started = time.monotonic()
        result = unittest.TextTestRunner(stream=sys.stderr if stream is None else stream,
                                         verbosity=1, resultclass=RecordingResult).run(suite)
        execution_seconds = time.monotonic() - started
        outcomes, observations, durations = (result.outcomes, result.observations,
                                            result.durations_seconds)
        execution_returncode = 0 if result.wasSuccessful() else 1
    scope_errors = [f"Unregistered function control: {row['path']}::{name}"
                    for row in function_controls for name in row['functions']]
    try:
        require(verify_source(context, root, os.environ if environment is None else environment,
                              source_receipt) == provenance,
                'Source transfer receipt changed during execution')
    except ManifestError as error:
        scope_errors.append(f'Source changed during unittest execution: {error}')
    report = assess_cell(context, inventory, outcomes, discovery_returncode=1 if errors else 0,
                         execution_returncode=execution_returncode, exceptions=exceptions,
                         scope_errors=scope_errors)
    report.update({'source_provenance': provenance, 'discovery_errors': errors,
                   'unregistered_function_controls': function_controls,
                   'source_inspection_errors': source_inspection_errors,
                   'discovery_seconds': discovery_seconds, 'execution_seconds': execution_seconds,
                   'durations_seconds': durations, 'observations': observations})
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', type=Path, required=True)
    parser.add_argument('--suite-dir', required=True)
    parser.add_argument('--source-root', type=Path, default=Path.cwd())
    parser.add_argument('--source-receipt', type=Path)
    parser.add_argument('--exceptions', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    context = json.loads(args.context.read_text())
    exceptions = json.loads(args.exceptions.read_text()) if args.exceptions else []
    report = collect(context, args.suite_dir, args.source_root,
                     source_receipt=args.source_receipt, exceptions=exceptions)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, sort_keys=True, indent=2) + '\n')
    print(f"{report['context']['cell_id']}: discovered={report['counts']['discovered']} "
          f"complete={report['complete']} passed={report['passed']}")
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ManifestError, OSError, json.JSONDecodeError) as error:
        raise SystemExit(f'baseline-python: {error}') from error
