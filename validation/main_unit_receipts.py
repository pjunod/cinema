"""Authenticated main-lane Python continuation and bounded legacy bridge."""
from __future__ import annotations

import argparse
import ast
import base64
import hashlib
import json
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
# Reviewed pre-log-snapshot runner implementations, not run-specific waivers.
LEGACY_RUNNER_HASHES = {
    'validation/main_unit_receipts.py': '7b315d5940663e7d26d13bcec889dd16e9c7d4860d5bb708fb7bc4a3c374b6ba',
    'validation/python_unit_receipts.py': '8b0f9cfb68465095d80fb493e0e833812c3084284d3391758ff1e86bcc73b258',
}
# Historical prepare could refuse before it published a start journal. Only
# this reviewed source/workflow combination permits proving that phase from
# its complete runner log; this grants no test successes.
PREPARE_REFUSAL_HASHES = {
    'validation/main_unit_receipts.py': '5e86c38c2b2f97e0eb0341d5e95b50b33f0c2e170a6f0c37c7008400ef8ef3a8',
    'validation/python_unit_receipts.py': '8b0f9cfb68465095d80fb493e0e833812c3084284d3391758ff1e86bcc73b258',
    '.github/workflows/main-fast-lane.yml': 'bf4b542a127ead30fd4239d38d75d3771258b0d7978e55afd8ebdc813aedfc0a',
}
# One reviewed infrastructure failure occurred during the first start upload,
# before discovery or either Python/Node execution step. This is zero execution,
# not a successful run or a replacement final journal.
PREUNIT_UPLOAD4324 = {
    'repository': 1, 'pr': 845, 'run': 4324, 'job': 44110, 'task': 16574,
    'commit': '8d00bf43342cb88b833e2672d48dc0af73558489',
    'base': '96f668128d8b714ed580fa913d3a5a13e6d77c2e',
    'branch': 'codex/architecture-final-closeout-20261007',
    'log_bytes': 494360,
    'log_sha256': '62292b20e42403f2dcc7b5529a4beefee519accef6594fc11033be8b1ee4dca7',
    'sources': {
        'validation/main_unit_receipts.py': '1e5e5c5f56bb8326ce8a15e8154b0e5dd8f987a4fd77ae0505e91aa55f357bf1',
        'validation/python_unit_receipts.py': '8b0f9cfb68465095d80fb493e0e833812c3084284d3391758ff1e86bcc73b258',
        '.github/workflows/main-fast-lane.yml': '414ba7aa7885a52cca54f0b0f80fbf6feaa03e94c17f241846a0696e414979b2',
    },
}

# Both attempts of this one run refused in prepare. The second attempt must
# never hide execution by the first: authenticate both original task logs.
PREUNIT4431 = {
    'repository': 1, 'pr': 888, 'run': 4431, 'job': 44940,
    'commit': 'd05da1109708c4c9574899018c314603b2040f1b',
    'base': 'b3ac74a161a3a0d1b9836ac98d356a9aaea51de6',
    'branch': 'codex/avatar-apple-tv-resume',
    'sources': {
        'validation/__init__.py': '347a3ad6f8a7cdabc285fac0bdd906d7c579115edebde81b08fad15426de5b4e',
        'validation/main_unit_receipts.py': '979b65c62c70d0b51299df9c36e56ba83da6b4c129e13014759b9c2a1a38b7a2',
        'validation/main_preflight_adoption.py': '0816a274932a8e3bd89f27bf24c5a571d63613280529955551753c64e6208b2e',
        'validation/python_unit_receipts.py': '8b0f9cfb68465095d80fb493e0e833812c3084284d3391758ff1e86bcc73b258',
        'validation/main-preflight-inputs.json': '7261bb1597fe9706775353d44c5366638716505f032758a49cbe90d6477131e9',
        '.github/workflows/main-fast-lane.yml': '414ba7aa7885a52cca54f0b0f80fbf6feaa03e94c17f241846a0696e414979b2',
    },
    'attempts': (
        (1, 16781, 142318, '8b8f7ce7b1987a3760c6eed90c1bb62b0d7868c869e922777c5297e0fd53c2b8',
         'Missing final journal for prior attempt 4429; preserve possibly passed IDs'),
        (2, 16793, 142964, '99580c2e2b36b35c0d940ba789f48c9942f604036be06bdfd3b835b92907e7a7',
         'Dispatch fresh runs; never rerun jobs'),
    ),
    'jobs': (
        (44938, 'fast-lane validation scope', 16791, 'success'),
        (44939, 'fast-lane mobile release version', 16792, 'success'),
        (44940, JOB, 16793, 'failure'),
        (44941, 'fast Rust gate', 16794, 'skipped'),
        (44942, 'fast Windows compile', 0, 'skipped'),
        (44943, 'fast web syntax gate', 0, 'skipped'),
        (44944, 'fast Apple compile', 0, 'cancelled'),
        (44945, 'fast Android compile', 0, 'skipped'),
        (44946, 'Main promotion gate', 0, 'cancelled'),
    ),
}


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
            or path in ('.github/workflows/' + WORKFLOW, 'Makefile', *LEGACY_RUNNER_HASHES,
                        *PREUNIT4431['sources']), 'Unsafe historical source path')
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


def emit_snapshot(journal, phase):
    raw = json.dumps(journal, sort_keys=True, separators=(',', ':')).encode()
    require(len(raw) <= receipts.MAX_BYTES, 'Log journal exceeds bound')
    encoded = base64.b64encode(raw).decode()
    chunks = [encoded[i:i + 2048] for i in range(0, len(encoded), 2048)]
    print(f'MAIN-UNIT-JOURNAL {phase} {len(chunks)} {hashlib.sha256(raw).hexdigest()}', flush=True)
    for index, chunk in enumerate(chunks):
        print(f'MAIN-UNIT-CHUNK {phase} {index} {chunk}', flush=True)
    print(f'MAIN-UNIT-END {phase}', flush=True)


def log_lines(raw):
    require(len(raw) <= receipts.MAX_BYTES, 'Historical log exceeds bound')
    return [re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', line) for line in raw.decode().splitlines()]


def read_snapshots(lines):
    snapshots = {}
    index = 0
    while index < len(lines):
        line = lines[index]
        if not line.startswith('MAIN-UNIT-'):
            index += 1
            continue
        marker = re.fullmatch(r'MAIN-UNIT-JOURNAL (start|final) ([1-9][0-9]*) ([0-9a-f]{64})', line)
        require(marker is not None, 'Unknown log journal framing')
        phase, count, digest = marker[1], int(marker[2]), marker[3]
        require((phase == 'start' and not snapshots)
                or (phase == 'final' and set(snapshots) == {'start'}),
                'Reordered log journal phases')
        require(phase not in snapshots and count <= (receipts.MAX_BYTES * 2 // 2048) + 1,
                'Duplicate/oversized log journal')
        chunks = []
        for position in range(count):
            index += 1
            require(index < len(lines), 'Truncated log journal')
            part = re.fullmatch(r'MAIN-UNIT-CHUNK ' + phase + r' ' + str(position) + r' ([A-Za-z0-9+/=]{1,2048})', lines[index])
            require(part is not None, 'Missing/reordered log journal chunk')
            chunks.append(part[1])
        index += 1
        require(index < len(lines) and lines[index] == f'MAIN-UNIT-END {phase}', 'Missing log journal end')
        try:
            raw = base64.b64decode(''.join(chunks), validate=True)
        except ValueError:
            raise receipts.ReceiptError('Invalid log journal encoding') from None
        require(hashlib.sha256(raw).hexdigest() == digest, 'Log journal digest mismatch')
        snapshots[phase] = receipts.bounded_json(raw)
        index += 1
    return snapshots


def verbose_passes(lines, inventories, inherited):
    """Bind every pending ID and terminal count; no success by subtraction."""
    summaries = {}
    for line in lines:
        match = re.fullmatch(r'(validation|operations): discovered=(\d+), historical-passes=(\d+), pending=(\d+)', line)
        if match:
            suite = match[1]
            require(suite not in summaries, 'Duplicate receipt suite summary')
            summaries[suite] = tuple(map(int, match.groups()[1:]))
    require(set(summaries) == set(receipts.SUITES), 'Missing receipt suite summaries')
    pending = {}
    for suite, ids in inventories.items():
        cached = ids & set(inherited)
        pending[suite] = ids - cached
        require(summaries[suite] == (len(ids), len(cached), len(pending[suite])),
                'Historical receipt discovery/pending mismatch')
    expected = set().union(*pending.values())
    observed, passes, outcomes = set(), set(), []
    yield_status = []
    terminal_counts = []
    active = None
    for line in lines:
        header = re.fullmatch(r'(test\w+) \(([A-Za-z0-9_.]+)\)(?: (.*))?', line)
        if header:
            require(active is None and not yield_status, 'Historical method lacks terminal outcome/status')
            candidates = [suite + ':' + header[2] for suite in receipts.SUITES
                          if suite + ':' + header[2] in expected]
            require(len(candidates) == 1 and candidates[0] not in observed,
                    'Unknown/duplicate historical pending method')
            require(header[1] == header[2].rsplit('.', 1)[-1], 'Historical method header mismatch')
            active = candidates[0]
            line = header[3] or ''
        if active is not None:
            outcome = re.search(r'(?:^| )\.\.\. (ok|skipped [\'\"].*[\'\"])$', line)
            if outcome:
                observed.add(active)
                outcomes.append(outcome[1])
                if outcome[1] == 'ok':
                    passes.add(active)
                active = None
        terminal = re.fullmatch(r'Ran (\d+) tests? in [0-9.]+s', line)
        if terminal:
            require(active is None and not yield_status and int(terminal[1]) == len(outcomes), 'Historical unit terminal count mismatch')
            skips = sum(value != 'ok' for value in outcomes)
            terminal_counts.append(int(terminal[1]))
            # The terminal status follows after blank lines, as in TextTestRunner.
            outcomes = []
            expected_status = 'OK' if not skips else f'OK (skipped={skips})'
            yield_status.append(expected_status)
        elif line == 'OK' or line.startswith(('OK (', 'FAILED (')):
            require(yield_status and line == yield_status.pop(0), 'Historical unit terminal status mismatch')
    require(active is None and observed == expected and not outcomes and not yield_status
            and terminal_counts == [len(pending[suite]) for suite in receipts.SUITES if pending[suite]],
            'Incomplete historical pending-method outcomes')
    return passes


def recover_preunit4431(api, scope, prior, jobs):
    """Account for two exact prepare refusals; import no journal or outcomes."""
    from validation import main_preflight_adoption as adoption
    proof = PREUNIT4431
    if (scope.get('repository'), scope.get('pr'), prior['id']) != (proof['repository'], proof['pr'], proof['run']):
        return False
    expected_scope = {'repository': proof['repository'], 'pr': proof['pr'], 'branch': proof['branch'],
                      'base': 'main', 'workflow': WORKFLOW}
    require(all(scope.get(field, value) == value for field, value in expected_scope.items()),
            'Prepare4431 receipt scope mismatch')
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual['id'] == prior['id'] == proof['run']
            and actual['commit_sha'] == prior['commit_sha'] == proof['commit']
            and adoption.terminal_status(actual) == adoption.terminal_status(prior) == 'cancelled',
            'Prepare4431 terminal run/source mismatch')
    event_scope = {'repository': proof['repository'], 'pr': proof['pr']}
    event = adoption.bind_event(actual, event_scope)
    require(event == adoption.bind_event(prior, event_scope)
            and event['action'] == 'synchronized'
            and event['pull_request']['head']['ref'] == proof['branch']
            and event['pull_request']['base']['sha'] == proof['base'],
            'Prepare4431 original event/branch/base mismatch')
    require(len(jobs) == len(proof['jobs'])
            and all(type(job['id']) is int and type(job['task_id']) is int
                    and type(job['attempt']) is int and job['attempt'] == 2
                    and job['run_id'] == proof['run'] and job['repo_id'] == proof['repository'] for job in jobs)
            and tuple(sorted((job['id'], job['name'], job['task_id'], adoption.terminal_status(job))
                             for job in jobs)) == proof['jobs'],
            'Prepare4431 complete terminal job/task/attempt inventory mismatch')
    for path, digest in proof['sources'].items():
        raw = source(proof['commit'], path)
        require(hashlib.sha256(raw).hexdigest() == digest
                and raw == api.bytes('/raw/' + path, {'ref': proof['commit']}),
                'Prepare4431 immutable producer/workflow/runtime mismatch')
    # Forgejo 16's documented job-log endpoint accepts an explicit attempt.
    # The ordinary endpoint defaults to the latest task and cannot prove the
    # first attempt. Exact hashes also bind the runner and task identities.
    skipped = ["skipping post step for '" + step + "'; main step was skipped" for step in (
        'Preserve per-ID preflight journal even on failure',
        'Preserve main Python success journal even on unit failure',
        'Publish preflight attempt-start journal',
        'Publish main Python attempt-start marker',
    )]
    for attempt, task, size, digest, reason in proof['attempts']:
        raw = api.bytes(f"/actions/jobs/{proof['job']}/logs", {'attempt': attempt})
        require(len(raw) == size and hashlib.sha256(raw).hexdigest() == digest,
                f'Prepare4431 attempt {attempt} original complete log mismatch')
        lines = log_lines(raw)
        refusal = 'Main Python receipt refused: ' + reason
        log = '\n'.join(lines)
        require(raw.endswith(b'\n') and lines[-1] == f"Job '{JOB}' failed"
                and lines.count(refusal) == 1 and all(lines.count(line) == 1 for line in skipped)
                and [lines.index(line) for line in skipped] == sorted(lines.index(line) for line in skipped)
                and lines.index(refusal) < lines.index(skipped[0])
                and proof['commit'] + f":refs/remotes/pull/{proof['pr']}/head" in log
                and sum(f'received task {task} of job preflight, triggered by event: pull_request' in line
                        for line in lines[:lines.index(refusal)]) == 1
                and 'node: v22.23.2' in lines[:lines.index(refusal)]
                and not any(marker in log for marker in (
                    'MAIN-UNIT-', 'Main preflight outcome ', 'Authenticated candidates=',
                    'discovered=', 'pending=', 'has been successfully uploaded!',
                    '... ok', '... FAIL', '... ERROR', 'TAP version '))
                and not any(re.fullmatch(r'Ran \d+ tests? in .*', line) for line in lines),
                f'Prepare4431 attempt {attempt} contradicts reviewed zero-unit phase evidence')
    require(api.get(f"/actions/runs/{proof['run']}/artifacts") == [],
            'Prepare4431 unexpectedly has run artifacts')
    for name in (key(expected_scope), f"main-preflight-v1-r{proof['repository']}-pr{proof['pr']}"):
        require(not any(item['run_id'] == proof['run'] for item in api.pages('/actions/artifacts', {'name': name}))
                and not api.pages('/actions/artifacts', {'name': name + f"-start-{proof['run']}"}),
                'Prepare4431 unexpectedly has start/final artifacts')
    print('Accounted run4431/job44940 attempts1+2: zero units; no outcomes or journal imported')
    return True


def recover_preunit_upload4324(api, scope, prior, job):
    """Return the actual incomplete start only; authenticate origins elsewhere."""
    from validation import main_preflight_adoption as adoption
    proof = PREUNIT_UPLOAD4324
    if (scope.get('repository'), scope.get('pr'), prior['id']) != (proof['repository'], proof['pr'], proof['run']):
        return None
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual['id'] == prior['id'] == proof['run']
            and actual['commit_sha'] == prior['commit_sha'] == proof['commit']
            and adoption.terminal_status(actual) == 'failure',
            'Upload4324 terminal run/source mismatch')
    event = adoption.bind_event(actual, {'repository': proof['repository'], 'pr': proof['pr']})
    require(event == adoption.bind_event(prior, {'repository': proof['repository'], 'pr': proof['pr']})
            and event['action'] == 'synchronized'
            and event['pull_request']['head']['ref'] == proof['branch']
            and event['pull_request']['base']['sha'] == proof['base'],
            'Upload4324 original event/branch/base mismatch')
    matching = [item for item in api.pages(f"/actions/runs/{proof['run']}/jobs") if item['name'] == JOB]
    require(len(matching) == 1 and matching[0] == job
            and job['id'] == proof['job'] and job['run_id'] == proof['run']
            and job['repo_id'] == proof['repository'] and type(job['attempt']) is int and job['attempt'] == 1
            and type(job.get('task_id')) is int and job['task_id'] == proof['task']
            and adoption.terminal_status(job) == 'failure',
            'Upload4324 terminal job/task/attempt mismatch')
    for path, digest in proof['sources'].items():
        raw = source(proof['commit'], path)
        require(hashlib.sha256(raw).hexdigest() == digest
                and raw == api.bytes('/raw/' + path, {'ref': proof['commit']}),
                'Upload4324 immutable producer/workflow mismatch')
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(len(raw) == proof['log_bytes'] and hashlib.sha256(raw).hexdigest() == proof['log_sha256'],
            'Upload4324 exact complete log mismatch')
    lines = log_lines(raw)
    snapshots = read_snapshots(lines)
    require(set(snapshots) == {'start'}, 'Upload4324 requires only the original start snapshot')
    expected_scope = {'repository': proof['repository'], 'pr': proof['pr'], 'branch': proof['branch'],
                      'base': 'main', 'workflow': WORKFLOW}
    require(all(scope.get(field, value) == value for field, value in expected_scope.items()),
            'Upload4324 receipt scope mismatch')
    start = snapshots['start']
    receipts.validate_journal(start, expected_scope, proof['run'], proof['commit'], completed=False)
    require(start['complete'] is False and start['fixture_errors'] == []
            and all(type(value['run']) is int and value['run'] < proof['run']
                    and value['commit'] is not None for value in start['passes'].values()),
            'Upload4324 cannot originate unit successes')
    validate_bridge_environment(start, start, proof['commit'])
    ended = lines.index('MAIN-UNIT-END start')
    uploads = [i for i, line in enumerate(lines) if line == 'Beginning upload of artifact content to blob storage']
    failures = [i for i, line in enumerate(lines) if line ==
                '::error::Error runner api getting task: task is not running%0A']
    require(raw.endswith(b'\n') and lines[-1] == f"Job '{JOB}' failed"
            and len(uploads) == len(failures) == 3 and ended < uploads[0]
            and all(upload < failure for upload, failure in zip(uploads, failures))
            and not any('discovered=' in line or 'Main preflight outcome ' in line
                        or re.fullmatch(r'Ran \d+ tests? in .*', line) for line in lines),
            'Upload4324 contradicts reviewed before-discovery failure')
    require(api.get(f"/actions/runs/{proof['run']}/artifacts") == [],
            'Upload4324 unexpectedly has run artifacts')
    for name in (key(expected_scope), f"main-preflight-v1-r{proof['repository']}-pr{proof['pr']}"):
        require(not any(item['run_id'] == proof['run'] for item in api.pages('/actions/artifacts', {'name': name}))
                and not api.pages('/actions/artifacts', {'name': name + f"-start-{proof['run']}"}),
                'Upload4324 unexpectedly has start/final artifacts')
    print('Accounted failed run4324/job44110: zero units; retained start is incomplete and grants no outcomes')
    return start


def recover_log(api, scope, prior, job, older):
    rid, commit = receipts.positive(prior['id']), receipts.sha(prior['commit_sha'])
    start = recover_preunit_upload4324(api, scope, prior, job)
    if start is not None:
        return start
    require(job['status'] in ('success', 'failure', 'cancelled'),
            'Log receipt needs an authenticated terminal preflight')
    lines = log_lines(api.bytes(f"/actions/jobs/{receipts.positive(job['id'])}/logs"))
    require(sum(commit in line for line in lines) >= 2
            and any('triggered by event: pull_request' in line for line in lines),
            'Recovery log lacks exact-source checkout evidence')
    snapshots = read_snapshots(lines)
    if snapshots:
        require(set(snapshots) == {'start', 'final'}, 'Incomplete log journal pair')
        start, final = snapshots['start'], snapshots['final']
        receipts.validate_journal(start, scope, rid, commit, completed=False)
        receipts.validate_journal(final, scope, rid, commit)
        require(start['complete'] is False and all(final['passes'].get(test) == value
                for test, value in start['passes'].items()), 'Log journal inheritance mismatch')
        return final
    # A completed framed journal has the same positive records as its artifact,
    # even when a later step or the job deadline fails. Unframed legacy output
    # still needs whole-job success; a partial log never becomes a receipt.
    require(job['status'] == 'success', 'Missing artifact needs completed successful preflight')
    for path, digest in LEGACY_RUNNER_HASHES.items():
        raw = source(commit, path)
        require(hashlib.sha256(raw).hexdigest() == digest
                and raw == api.bytes('/raw/' + path, {'ref': commit}),
                'Historical runner is not admitted for verbose recovery')
    workflow = source(commit, '.github/workflows/' + WORKFLOW)
    require(workflow == api.bytes('/raw/.github/workflows/' + WORKFLOW, {'ref': commit})
            and workflow.count(b'run: python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations') == 1,
            'Historical receipt workflow mismatch')
    applicable = receipts.SourceApplicability(commit, worktree_reader=lambda path: receipts.read_git_test_source(commit, path))
    inherited = {}
    for test, value in older:
        if value['run'] < rid and applicable(test, value):
            inherited.setdefault(test, value)
    applicable.finish(inherited)
    # Verbose recovery proves each newly executed method positively. Unlike
    # the legacy dot-log migration, no unnamed method becomes a success, so
    # exhaustive module reconstruction (including imported helper classes) is
    # unnecessary. Bind each explicit ID to immutable method/fixture source.
    inventories = {suite: {test for test in inherited if test.startswith(suite + ':')}
                   for suite in receipts.SUITES}
    checker = receipts.SourceApplicability(commit, worktree_reader=lambda path: receipts.read_git_test_source(commit, path))
    for line in lines:
        header = re.fullmatch(r'(test\w+) \(([A-Za-z0-9_.]+)\)(?: (.*))?', line)
        if not header:
            continue
        candidates = []
        for suite in receipts.SUITES:
            test = suite + ':' + header[2]
            try:
                fingerprint = checker.fingerprint(commit, test)
            except receipts.ReceiptError:
                continue
            if fingerprint is not None:
                candidates.append(test)
        require(len(candidates) == 1, 'Historical pending method lacks unique immutable source')
        test = candidates[0]
        inventories[test.split(':', 1)[0]].add(test)
    fresh = verbose_passes(lines, inventories, inherited)
    passes = dict(inherited)
    passes.update({test: {'run': rid, 'commit': commit} for test in fresh})
    return {'version': receipts.VERSION, 'scope': scope, 'run': rid, 'commit': commit,
            'complete': True, 'passes': passes, 'fixture_errors': []}


def unexecuted_preflight(api, scope, prior, job):
    """Prove no unit execution, without treating missing evidence as a pass."""
    rid = receipts.positive(prior['id'])
    # Forgejo assigns task_id only when a runner receives the job. Cancelling
    # a failed scope also cancels its still-unassigned dependent jobs.
    unassigned = job['status'] == 'cancelled' and type(job.get('task_id')) is int and job['task_id'] == 0
    if not unassigned:
        if job['status'] != 'failure':
            return False
        commit = receipts.sha(prior['commit_sha'])
        for path, digest in PREPARE_REFUSAL_HASHES.items():
            raw = source(commit, path)
            if hashlib.sha256(raw).hexdigest() != digest or raw != api.bytes('/raw/' + path, {'ref': commit}):
                return False
        lines = log_lines(api.bytes(f"/actions/jobs/{receipts.positive(job['id'])}/logs"))
        # The admitted workflow runs prepare before publishing the marker and
        # before every test. The failed prepare and skipped marker establish
        # the phase positively; a partial log or a unit-phase refusal does not.
        refused = [i for i, line in enumerate(lines) if line.startswith('Main Python receipt refused: ')]
        skipped = [i for i, line in enumerate(lines) if line ==
                   "skipping post step for 'Publish main Python attempt-start marker'; main step was skipped"]
        ended = [i for i, line in enumerate(lines) if line == f"Job '{JOB}' failed"]
        if not (len(refused) == len(skipped) == len(ended) == 1
                and refused[0] < skipped[0] < ended[0]
                and sum(commit in line for line in lines[:refused[0]]) >= 2
                and any('triggered by event: pull_request' in line for line in lines[:refused[0]])
                and not any(line.startswith('MAIN-UNIT-') for line in lines)):
            return False
    markers = api.pages('/actions/artifacts', {'name': key(scope) + f'-start-{rid}'})
    require(not markers, 'Unexecuted preflight has an attempt-start marker')
    return True


def restore(api, scope, current_run, applicability, bridge=None):
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
    journals, trusted, bootstrap_passes, missing = [], {}, {}, []
    if bridge is not None:
        bootstrap_passes = {test: {'run': value['run'], 'commit': value['commit']}
                            for test, value in bridge[0].items() if not test.startswith('node:')}
    for prior in sorted(runs, key=lambda item: receipts.positive(item['id']), reverse=True):
        if (prior['id'] in (4513, 4540) and prior['id'] != current_run
                and scope['repository'] == 1 and scope['pr'] == 917):
            from validation.main_source_skew4513 import recover as recover_source_skew
            source_jobs = api.pages(f"/actions/runs/{prior['id']}/jobs")
            require(prior['id'] not in indexed and recover_source_skew(api, scope, prior, source_jobs),
                    'Source-skew chain has unexpected final journal or unavailable proof')
            continue
        if (prior['id'] != current_run and prior['id'] not in indexed
                and scope['repository'] == 1 and scope['pr'] == 917):
            from validation.main_source_skew4513 import recover_prepare_refusal_chain
            source_jobs = api.pages(f"/actions/runs/{prior['id']}/jobs")
            if recover_prepare_refusal_chain(api, scope, prior, source_jobs):
                continue
        authenticate_run(scope, prior)
        rid = receipts.positive(prior['id'])
        if rid == current_run:
            require(rid not in indexed, 'Attempt already executed')
            continue
        jobs = api.pages(f'/actions/runs/{rid}/jobs')
        if recover_preunit4431(api, scope, prior, jobs):
            require(rid not in indexed, 'Prepare4431 unexpectedly has indexed final journal')
            continue
        matches = [job for job in jobs if job['name'] == JOB]
        require(len(matches) == 1, 'Ambiguous main preflight job')
        job = matches[0]
        require(job['repo_id'] == scope['repository'] and job['run_id'] == rid
                and type(job['attempt']) is int and job['attempt'] > 0,
                'Historical job repository/run/attempt mismatch')
        commit = receipts.sha(prior['commit_sha'])
        workflow = api.bytes('/raw/.github/workflows/' + WORKFLOW, {'ref': commit})
        receipt_workflow = b'validation.main_unit_receipts' in workflow
        if receipt_workflow or rid in indexed:
            require(job['attempt'] == 1, 'Receipt job retry has ambiguous artifact identity')
        if job['status'] == 'skipped':
            require(rid not in indexed, 'Skipped job has final journal')
            continue
        if job['status'] == 'cancelled' and type(job.get('task_id')) is int and job['task_id'] == 0:
            require(rid not in indexed, 'Unassigned preflight has final journal')
            require(unexecuted_preflight(api, scope, prior, job), 'Unassigned preflight is ambiguous')
            continue
        if rid not in indexed:
            from validation.main_prepare_refusal_recovery import recover
            if recover(api, scope, prior, jobs):
                continue
            if receipt_workflow:
                if unexecuted_preflight(api, scope, prior, job):
                    continue
                missing.append((prior, job))
                continue
            if bridge is not None and rid in bridge[1]:
                continue  # Fully authenticated adapter-era attempt; no invented generic journal.
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
        if bridge is not None:
            validate_bridge_environment(start, journal, commit)
        require(isinstance(journal.get('complete'), bool), 'Invalid completion marker')
        require(all(journal['passes'].get(test) == value for test, value in start['passes'].items()),
                'Final journal discarded or changed inherited evidence')
        for test, value in journal['passes'].items():
            if value == {'run': rid, 'commit': commit}:
                trusted[(test, rid, commit)] = value
        journals.append(journal)
    require(not indexed, 'Final artifact lacks authenticated workflow/job')
    for prior, job in sorted(missing, key=lambda item: item[0]['id']):
        older = list(bootstrap_passes.items()) + [(test, value) for (test, _, _), value in trusted.items()]
        journal = recover_log(api, scope, prior, job, older)
        if bridge is not None:
            validate_bridge_environment(journal, journal, prior['commit_sha'])
        for test, value in journal['passes'].items():
            if value == {'run': prior['id'], 'commit': prior['commit_sha']}:
                trusted[(test, prior['id'], prior['commit_sha'])] = value
        journals.append(journal)
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


def validate_bridge_environment(start, final, commit):
    """Bind generic-origin evidence to its authenticated producer and runtime."""
    from validation import main_preflight_adoption as adoption
    expected_blob = adoption.git('rev-parse', commit + ':validation/main_unit_receipts.py')
    require(start.get('environment') == final.get('environment') == adoption.environment()
            and start.get('producer_blob') == final.get('producer_blob') == expected_blob,
            'Main bridge producer/runtime provenance mismatch')
    workflow = adoption.git('show', commit + ':.github/workflows/' + WORKFLOW)
    require(adoption.adapter_workflow(adoption.run_commands(workflow)),
            'Main bridge workflow producer mismatch')


class DeclaredApplicability:
    """PR845 retains method, fixture, environment and actual production inputs."""

    def __init__(self, commit):
        from validation import main_preflight_adoption as adoption
        self.adoption, self.commit = adoption, commit
        self.source = receipts.SourceApplicability(commit)
        self.families = receipts.bounded_json(adoption.MANIFEST.read_bytes())['families']
        self.paths = set(adoption.git('ls-tree', '-r', '--name-only', commit, 'tests').splitlines())

    def __call__(self, test, value):
        if receipts.test_source_path(test)[0] not in self.paths:
            return False  # Authenticated source tree proves this module was removed.
        row = self.adoption.witness(test, self.families)
        return (self.source(test, value)
                and self.adoption.input_digest(self.commit, row)
                == self.adoption.input_digest(value['commit'], row))

    def finish(self, passes):
        self.source.finish(passes)


def applicability_for(scope, commit):
    return (DeclaredApplicability(commit) if scope['repository'] == 1 and scope['pr'] == 845
            else receipts.SourceApplicability(commit))


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
    if suites is None and journal['scope']['repository'] == 1 and journal['scope']['pr'] == 845:
        from validation import main_preflight_adoption as adoption
        _api, scope, commit, run = adoption.api_context()
        require(scope == {'repository': journal['scope']['repository'], 'pr': journal['scope']['pr']}
                and commit == journal['commit'] and run == journal['run'],
                'Live bridge attempt/source identity mismatch')
        require(not adoption.git('diff', '--name-only', 'HEAD'),
                'Current tracked source differs from prepared commit')
        validate_bridge_environment(journal, journal, journal['commit'])
    if suites is None:
        applicability = applicability_for(journal['scope'], journal['commit'])
        applicable = {test: value for test, value in journal['passes'].items() if applicability(test, value)}
        applicability.finish(applicable)
        require(applicable == journal['passes'], 'Prepared passes are no longer applicable')
    require(not journal['fixture_errors'], 'Unresolved fixture errors')
    inventories = {name: list(suites[name]) if suites is not None else receipts.discover(name)
                   for name in receipts.SUITES}
    if suites is None and journal['scope']['repository'] == 1 and journal['scope']['pr'] == 845:
        from validation import main_preflight_adoption as adoption
        families = receipts.bounded_json(adoption.MANIFEST.read_bytes())['families']
        for name, tests in inventories.items():
            for test in tests:
                adoption.witness(name + ':' + test.id(), families)
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
        from validation import main_preflight_adoption as adoption
        candidates, authenticated_runs = adoption.prepare(output_key='adoption_receipt_key')
        bridge = (candidates, authenticated_runs) if scope['repository'] == 1 and scope['pr'] == 845 else None
        journal = {'version': receipts.VERSION, 'scope': scope, 'run': run, 'commit': commit,
                   'complete': False, 'fixture_errors': [], 'applicability_commit': commit,
                   'passes': restore(api, scope, run, applicability_for(scope, commit), bridge)}
        if bridge is not None:
            journal.update(environment=adoption.environment(),
                           producer_blob=adoption.git('rev-parse', commit + ':validation/main_unit_receipts.py'))
        receipts.atomic_json(PATH, journal)
        emit_snapshot(journal, 'start')
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'receipt_key={key(scope)}\n')
        return 0
    journal = receipts.bounded_json(PATH.read_bytes())
    receipts.validate_journal(journal, scope, run, commit, completed=False)
    require(journal['complete'] is False, 'Attempt already complete')
    try:
        return execute(journal, PATH)
    finally:
        emit_snapshot(receipts.bounded_json(PATH.read_bytes()), 'final')


if __name__ == '__main__':
    try:
        sys.exit(main())
    except receipts.ReceiptError as error:
        print(f'Main Python receipt refused: {error}', file=sys.stderr)
        sys.exit(1)
