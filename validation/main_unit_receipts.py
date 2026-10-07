"""Authenticated main-lane Python continuation, with no run-specific exceptions."""
from __future__ import annotations

import argparse
import ast
import os
from pathlib import Path
import re
import subprocess
import sys
import unittest
import urllib.parse

from validation import python_unit_receipts as receipts

WORKFLOW = 'main-fast-lane.yml'
JOB = 'fast policy and contract preflight'
PATH = Path('.main-python-unit-receipts/receipt.json')
require = receipts.require


def identity(api, pr, commit):
    repository = receipts.positive(api.get('')['id'])
    pull = api.get(f'/pulls/{receipts.positive(pr)}')
    require(pull['state'] == 'open' and pull['base']['ref'] == 'main'
            and pull['base']['repo']['id'] == repository
            and pull['head']['repo']['id'] == repository
            and pull['head']['sha'] == commit,
            'Main receipt must match an open same-repository PR head')
    return {'repository': repository, 'pr': pr, 'branch': pull['head']['ref'],
            'base': 'main', 'workflow': WORKFLOW}


def key(scope):
    return f"main-python-units-v1-r{scope['repository']}-pr{scope['pr']}"


def source(commit, path):
    require(re.fullmatch(r'tests/(validation|operations)/test_[a-z0-9_]+\.py', path)
            or path in ('.github/workflows/' + WORKFLOW, 'Makefile'), 'Unsafe historical source path')
    if path.startswith('tests/'):
        return receipts.read_git_test_source(commit, path)
    object_name = f'{receipts.sha(commit)}:{path}'
    size = subprocess.check_output(['git', 'cat-file', '-s', object_name], text=True, timeout=5).strip()
    require(size.isascii() and size.isdigit() and 0 < int(size) <= receipts.MAX_TEST_SOURCE,
            'Historical workflow/Makefile source exceeds bound')
    raw = subprocess.check_output(['git', 'cat-file', 'blob', object_name], timeout=5)
    require(len(raw) == int(size), 'Historical source byte count mismatch')
    return raw


def standard_decorators(decorators, modules):
    for decorator in decorators:
        value = decorator.func if isinstance(decorator, ast.Call) else decorator
        require(isinstance(value, ast.Attribute) and isinstance(value.value, ast.Name)
                and value.value.id in modules
                and value.attr in ('skip', 'skipIf', 'skipUnless', 'expectedFailure'),
                'Unsupported historical test decorator')


def inventory(commit, suite):
    """Reconstruct default unittest discovery without importing historical code."""
    paths = subprocess.check_output(['git', 'ls-tree', '-r', '--name-only',
                                    receipts.sha(commit), receipts.SUITES[suite]], text=True, timeout=5).splitlines()
    require(not any(Path(path).name == '__init__.py' for path in paths),
            'Historical package discovery is unsupported')
    checker = receipts.SourceApplicability(commit, source_reader=source,
                                          worktree_reader=lambda path: source(commit, path))
    ids = set()
    for path in paths:
        if not re.fullmatch(r'tests/' + suite + r'/test_[a-z0-9_]+\.py', path):
            require(not Path(path).name.startswith('test_'), 'Unsupported historical test path')
            continue
        raw = source(commit, path)
        tree = ast.parse(raw, filename=path)
        _, classes, _, modules, cases, _, imported = checker.source(commit, path)
        # Imported classes, module assignment and conditional definitions can
        # alter the loader's inventory. Refuse rather than guess a pass count.
        require(not any(isinstance(node, ast.ImportFrom) and node.module != 'unittest'
                        and any(alias.name.endswith(('Case', 'Test', 'Tests')) for alias in node.names)
                        for node in tree.body), 'Unsupported imported test class')
        require(not any(isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign))
                        and any(isinstance(item, ast.Attribute) and isinstance(item.ctx, ast.Store)
                                for item in ast.walk(node)) for node in tree.body),
                'Unsupported dynamic test mutation')
        active_nodes = [item for node in tree.body
                        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                        for item in ast.walk(node)]
        require(not any(isinstance(item, ast.Call) and (
                    isinstance(item.func, ast.Name)
                    and item.func.id in ('setattr', 'delattr', 'exec', 'eval')
                    or any(isinstance(argument, ast.Name) and argument.id in classes
                           for argument in item.args))
                        for item in active_nodes),
                'Unsupported dynamic historical class mutation')
        require(not any(isinstance(node, (ast.If, ast.For, ast.While, ast.Try, ast.With))
                        and any(isinstance(item, ast.ClassDef) for item in ast.walk(node))
                        for node in tree.body), 'Unsupported conditional historical class')

        def methods(name, seen=frozenset()):
            require(name not in seen and len(seen) < 16, 'Ambiguous discovery inheritance')
            node = classes[name]
            standard_decorators(node.decorator_list, modules)
            for item in node.body:
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)) and item.name.startswith('test'):
                    standard_decorators(item.decorator_list, modules)
            own = {item.name for item in node.body
                   if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                   and item.name.startswith('test')}
            is_case = False
            for base in node.bases:
                if isinstance(base, ast.Name) and base.id in classes:
                    inherited, case = methods(base.id, seen | {name})
                    own |= inherited
                    is_case |= case
                elif isinstance(base, ast.Attribute) and isinstance(base.value, ast.Name) \
                        and base.value.id in modules and base.attr in ('TestCase', 'IsolatedAsyncioTestCase'):
                    is_case = True
                elif isinstance(base, ast.Name) and base.id in cases:
                    is_case = True
                else:
                    require(isinstance(base, ast.Name) and base.id == 'object',
                            'Unsupported external discovery base')
            return own, is_case

        for name in classes:
            names, is_case = methods(name)
            if not is_case:
                continue
            standard_decorators(classes[name].decorator_list, modules)
            for method in names:
                test_key = f'{suite}:{Path(path).stem}.{name}.{method}'
                require(checker.fingerprint(commit, test_key) is not None,
                        'Historical method is not statically attributable')
                require(test_key not in ids, 'Duplicate historical test ID')
                ids.add(test_key)
    require(0 < len(ids) <= receipts.MAX_TESTS, 'Invalid historical inventory')
    return ids


def dot_passes(log, ids, suite):
    """Only an exhaustive terminal default-runner result establishes passes."""
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', line) for line in log.decode().splitlines()]
    summaries = [(i, re.fullmatch(r'Ran (\d+) tests? in [0-9.]+s', line))
                 for i, line in enumerate(lines)]
    summaries = [(i, match) for i, match in summaries if match]
    require(len(summaries) == 1, 'Ambiguous historical unittest terminal result')
    index, summary = summaries[0]
    require(int(summary[1]) == len(ids), 'Historical discovery/count mismatch')
    terminal = [line for line in lines[index + 1:] if line][:1]
    require(len(terminal) == 1, 'Missing historical unittest status')
    failures = re.fullmatch(r'FAILED \(failures=(\d+)\)', terminal[0])
    require(terminal[0] == 'OK' or failures, 'Errors/skips/unknown outcomes cannot establish historical passes')
    failed = set()
    for line in lines[:index]:
        match = re.fullmatch(r'FAIL: (test\w+) \(([a-zA-Z0-9_.]+)\)(?: \(.*\))?', line)
        if match:
            failed.add(suite + ':' + match[2])
        require(not line.startswith(('ERROR:', 'UNEXPECTED SUCCESS:', 'FAIL:')) or match,
                'Unattributable historical outcome')
    require(failed <= ids and (len(failed) == int(failures[1]) if failures else not failed),
            'Historical failed-method identity/count mismatch')
    return ids - failed


def bootstrap(api, scope, prior, job):
    commit, run = receipts.sha(prior['commit_sha']), receipts.positive(prior['id'])
    workflow = api.bytes('/raw/.github/workflows/' + WORKFLOW, {'ref': commit})
    require(b'validation.main_unit_receipts' not in workflow,
            'Receipt-policy run lacks authenticated journals; preserve evidence')
    command = b"python3 -m unittest discover -s tests/validation -p 'test_*.py'"
    require(workflow.count(command) == 1 and b'run: make operations-check' in workflow,
            'Historical main workflow command mismatch')
    require(source(commit, '.github/workflows/' + WORKFLOW) == workflow,
            'Historical checkout workflow differs from authenticated source')
    log = api.bytes(f"/actions/jobs/{receipts.positive(job['id'])}/logs")
    require(log.decode().count(commit) >= 2 and b'triggered by event: pull_request' in log,
            'Historical log lacks exact-source checkout evidence')
    # Each terminal suite binds default discovery IDs and all failed IDs.
    # A one-suite cutover must have failed validation (operations never ran).
    lines = log.splitlines(keepends=True)
    boundaries = []
    offset = 0
    awaiting = False
    for line in lines:
        offset += len(line)
        plain = re.sub(rb'^\d{4}-\d\d-\d\dT[^ ]+ ', b'', line).strip()
        if re.fullmatch(rb'Ran \d+ tests? in [0-9.]+s', plain):
            require(not awaiting, 'Missing terminal suite status')
            awaiting = True
        elif awaiting and plain:
            require(plain == b'OK' or re.fullmatch(rb'FAILED \([^\n]+\)', plain),
                    'Unknown terminal suite status')
            boundaries.append(offset)
            awaiting = False
    require(not awaiting and len(boundaries) in (1, 2), 'Ambiguous historical suite sequence')
    first_end = boundaries[0]
    validation_log = log[:first_end]
    passes = dot_passes(validation_log, inventory(commit, 'validation'), 'validation')
    if len(boundaries) == 1:
        require(b'FAILED (failures=' in validation_log,
                'Historical operations outcome unavailable; preserve exact evidence')
    else:
        require(b'FAILED (' not in validation_log, 'Operations ran after failed validation')
        require(command.replace(b'validation', b'operations') in source(commit, 'Makefile'),
                'Historical operations discovery command mismatch')
        passes |= dot_passes(log[first_end:], inventory(commit, 'operations'), 'operations')
    return {test: {'run': run, 'commit': commit} for test in passes}


def authenticate_run(scope, prior):
    require(prior['workflow_id'] == WORKFLOW and prior['prettyref'] == f"#{scope['pr']}"
            and prior['event'] == 'pull_request', 'Historical workflow/PR/event mismatch')
    event = prior['event_payload']
    if isinstance(event, str):
        event = receipts.bounded_json(event.encode())
    pull = event['pull_request']
    require(event['repository']['id'] == scope['repository'] and event['number'] == scope['pr']
            and pull['number'] == scope['pr'] and pull['head']['ref'] == scope['branch']
            and pull['head']['sha'] == prior['commit_sha'] and pull['base']['ref'] == 'main'
            and pull['head']['repo']['id'] == scope['repository']
            and pull['base']['repo']['id'] == scope['repository'],
            'Historical event repository/PR/head/base mismatch')


def restore(api, scope, current_run, applicability):
    artifacts = api.pages('/actions/artifacts', {'name': key(scope)})
    indexed = {}
    for artifact in artifacts:
        require(artifact['name'] == key(scope) and not artifact['expired']
                and artifact['size_in_bytes'] <= receipts.MAX_BYTES, 'Invalid final receipt artifact')
        rid = receipts.positive(artifact['run_id'])
        require(rid not in indexed, 'Ambiguous final receipt attempt')
        indexed[rid] = artifact
    runs = api.pages('/actions/runs', {'workflow_id': WORKFLOW,
                                     'ref': f"refs/pull/{scope['pr']}/head"}, 'workflow_runs')
    journals, trusted, bootstrap_passes = [], {}, {}
    for prior in sorted(runs, key=lambda item: receipts.positive(item['id']), reverse=True):
        authenticate_run(scope, prior)
        rid = receipts.positive(prior['id'])
        if rid == current_run:
            require(rid not in indexed, 'Attempt already executed')
            continue
        jobs = api.pages(f'/actions/runs/{rid}/jobs')
        matches = [job for job in jobs if job['name'] == JOB]
        require(len(matches) == 1, 'Ambiguous main preflight job')
        job = matches[0]
        require(job['repo_id'] == scope['repository'] and job['run_id'] == rid
                and job['attempt'] == 1, 'Historical job repository/run/attempt mismatch')
        if job['status'] == 'skipped':
            require(rid not in indexed, 'Skipped job has final journal')
            continue
        commit = receipts.sha(prior['commit_sha'])
        if rid not in indexed:
            workflow = api.bytes('/raw/.github/workflows/' + WORKFLOW, {'ref': commit})
            require(b'validation.main_unit_receipts' not in workflow,
                    'Receipt-policy attempt lacks final journal; retain its evidence')
            # One exhaustive legacy baseline is the migration boundary. Older
            # pre-receipt attempts are not imported or declared unexecuted.
            if not bootstrap_passes and job['status'] in ('success', 'failure'):
                log = api.bytes(f"/actions/jobs/{receipts.positive(job['id'])}/logs")
                if re.search(rb'Ran \d+ tests? in [0-9.]+s', log):
                    bootstrap_passes.update(bootstrap(api, scope, prior, job))
            continue
        require(job['status'] in ('success', 'failure', 'cancelled'), 'Historical attempt is still active; retain its evidence')
        markers = api.pages('/actions/artifacts', {'name': key(scope) + f'-start-{rid}'})
        require(len(markers) == 1 and markers[0]['run_id'] == rid and not markers[0]['expired'],
                'Missing/ambiguous attempt-start marker')
        start = receipts.artifact_json(api.bytes(f"/actions/artifacts/{receipts.positive(markers[0]['id'])}/zip"))
        receipts.validate_journal(start, scope, rid, commit, completed=False)
        require(start['complete'] is False, 'Invalid initial journal')
        journal = receipts.artifact_json(api.bytes(f"/actions/artifacts/{receipts.positive(indexed.pop(rid)['id'])}/zip"))
        # A later policy step or an interrupted unit runner can leave a final
        # upload incomplete. Positive atomic success records remain evidence;
        # absence from this map never establishes that a method did not run.
        receipts.validate_journal(journal, scope, rid, commit, completed=False)
        require(isinstance(journal.get('complete'), bool), 'Invalid completion marker')
        require(all(journal['passes'].get(test) == value for test, value in start['passes'].items()),
                'Final journal discarded or changed inherited evidence')
        for test, value in journal['passes'].items():
            if value == {'run': rid, 'commit': commit}:
                trusted[(test, rid, commit)] = value
        journals.append(journal)
    require(not indexed, 'Final artifact lacks authenticated workflow/job')
    candidates = list(bootstrap_passes.items())
    for journal in journals:
        for test, attribution in journal['passes'].items():
            require((test, attribution['run'], attribution['commit']) in trusted
                    or bootstrap_passes.get(test) == attribution,
                    'Inherited success has no authenticated provenance')
            candidates.append((test, attribution))
    passes = {}
    for test, value in candidates:
        if applicability(test, value):
            passes.setdefault(test, value)
    applicability.finish(passes)
    return passes


class MainResult(receipts.RecordingResult):
    """Record ordinary unittest outcomes while caching only real successes."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.observed = set()

    def addSuccess(self, test):
        self.observed.add(test.id())
        super().addSuccess(test)

    def addFailure(self, test, error):
        self.observed.add(test.id())
        super().addFailure(test, error)

    def addError(self, test, error):
        self.observed.add(test.id())
        super().addError(test, error)

    def addSubTest(self, test, subtest, error):
        if error is not None:
            self.observed.add(test.id())
        super().addSubTest(test, subtest, error)

    def addExpectedFailure(self, test, error):
        self.observed.add(test.id())
        super().addExpectedFailure(test, error)

    def addUnexpectedSuccess(self, test):
        self.observed.add(test.id())
        super().addUnexpectedSuccess(test)

    def addSkip(self, test, reason):
        unittest.TextTestResult.addSkip(self, test, reason)
        if test.id() in self.known_ids:
            self.observed.add(test.id())
        else:
            fixture = re.fullmatch(r'setUpClass \(([A-Za-z0-9_.]+)\)', test.id())
            require(fixture is not None, 'Unknown skipped fixture outcome')
            self.observed.update(test_id for test_id in self.known_ids
                                 if test_id.rsplit('.', 1)[0] == fixture[1])
        self.journal.setdefault('skips', {})[self.suite + ':' + test.id()] = str(reason)[:4096]
        receipts.atomic_json(self.path, self.journal)


def execute(journal, path, suites=None):
    """Keep main unittest semantics: a legitimate skip is not a cached pass."""
    require(journal.get('applicability_commit') == journal['commit'], 'Missing current-source prepare')
    if suites is None:
        applicability = receipts.SourceApplicability(journal['commit'])
        applicable = {test: value for test, value in journal['passes'].items() if applicability(test, value)}
        applicability.finish(applicable)
        require(applicable == journal['passes'], 'Prepared passes are no longer applicable')
    require(not journal['fixture_errors'], 'Unresolved fixture errors')
    inventories = {name: list(suites[name]) if suites is not None else receipts.discover(name)
                   for name in receipts.SUITES}
    failed = False
    for name, tests in inventories.items():
        ids = {test.id() for test in tests}
        require(tests and len(ids) == len(tests), 'Empty/duplicate current discovery')
        pending = [test for test in tests if name + ':' + test.id() not in journal['passes']]
        print(f'{name}: discovered={len(tests)}, historical-passes={len(tests)-len(pending)}, pending={len(pending)}')
        if pending:
            result = unittest.TextTestRunner(verbosity=2, resultclass=lambda *a, **kw:
                MainResult(*a, **kw, journal=journal, path=path, suite=name,
                                         known_ids=ids)).run(unittest.TestSuite(pending))
            require({test.id() for test in pending} <= result.observed, 'Unknown pending unit outcome')
            failed |= not result.wasSuccessful() or bool(journal['fixture_errors'])
    journal['complete'] = True
    receipts.atomic_json(path, journal)
    return int(failed)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('prepare', 'run'))
    parser.add_argument('--suite-dir', action='append', default=[])
    args = parser.parse_args(argv)
    require(args.suite_dir == list(receipts.SUITES.values()) if args.command == 'run'
            else not args.suite_dir, 'Declare exactly the two executed suite directories')
    require(not PATH.parent.is_symlink(), 'Receipt directory is a symlink')
    origin = urllib.parse.urlsplit(os.environ['GITHUB_API_URL'])
    server = urllib.parse.urlsplit(os.environ['GITHUB_SERVER_URL'])
    require((origin.scheme, origin.netloc) == (server.scheme, server.netloc), 'Refuse credential forwarding')
    api = receipts.API(os.environ['GITHUB_API_URL'], os.environ['GITHUB_REPOSITORY'], os.environ.get('GITHUB_TOKEN'))
    commit = receipts.sha(os.environ['GITHUB_SHA'])
    require(subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == commit,
            'Checkout is not event source')
    require(os.environ.get('GITHUB_RUN_ATTEMPT', '1') == '1', 'Dispatch fresh runs; never rerun jobs')
    run = receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    scope = identity(api, receipts.positive(int(os.environ['PR_NUMBER'])), commit)
    if args.command == 'prepare':
        journal = {'version': receipts.VERSION, 'scope': scope, 'run': run, 'commit': commit,
                   'complete': False, 'fixture_errors': [], 'applicability_commit': commit,
                   'passes': restore(api, scope, run, receipts.SourceApplicability(commit))}
        receipts.atomic_json(PATH, journal)
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'receipt_key={key(scope)}\n')
        return 0
    journal = receipts.bounded_json(PATH.read_bytes())
    receipts.validate_journal(journal, scope, run, commit, completed=False)
    require(journal['complete'] is False, 'Attempt already complete')
    return execute(journal, PATH)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except receipts.ReceiptError as error:
        print(f'Main Python receipt refused: {error}', file=sys.stderr)
        sys.exit(1)
