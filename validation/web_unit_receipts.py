"""Retain authenticated whole-command Node unit passes for unchanged inputs.

Only unit commands are adopted. The workflow still parses JavaScript and runs
TypeScript. Unknown origin or compatibility proof leaves the real command due.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

from validation.main_preflight_adoption import (
    UNADOPTABLE, api_context, bind_event, commit_available, git, journal_key,
    reason, terminal_status,
)
from validation.python_unit_receipts import IntegrityError, atomic_json, positive, require

WORKFLOW = '.github/workflows/main-fast-lane.yml'
JOB = 'fast web syntax gate'
RUNS_ON = ['self-hosted', 'Linux', 'X64', 'lab', 'general']
COMMANDS = {
    'jsconfig-generated': ['node', 'tests/web/jsconfig-generated.test.js'],
    'player-typedef': ['node', 'tests/web/player-typedef.test.js'],
    'suite': ['make', 'web-unit-check'],
}
LEGACY_JOB_SHA256 = '438b7d51e99d364e7f7ba1c2ea287d1872b9f18d2b0732d1e9eba04a7071f793'
WRAPPED_JOB_SHA256 = 'b2de0a8b789cc74ba31cb8a4a88fe680dbb22827238267bc04128aa8722194f3'
MAKEFILE_SHA256 = 'a8dfee1e29ba93b43cf58ee5233a5aef1aeddd381765a759d47b05c8126b23c2'
MAX_CANDIDATES = 16
MAX_ARTIFACT_PAGES = 4
REPORTS = Path('.main-preflight-receipts/web-unit')
# Audited non-inputs of this exact Node inventory. Keep other documentation,
# all shipped native/server code, JSON fixtures, tests, scripts and tools.
# The workflow and wrapper producer are independently authenticated below.
EXEMPT = {
    WORKFLOW, 'validation/native_receipts.py', 'validation/web_unit_receipts.py',
    'tests/validation/test_native_receipts.py', 'tests/validation/test_web_unit_receipts.py',
    'tests/operations/test_apple_tv_navigation_gate.py',
    'tests/operations/test_apple_tv_navigation_runner.py',
    'tests/operations/test_web_test_inventory.py',
    'docs/features/PROJECT-BACKLOG.md', 'docs/features/SUBTITLE-COMPLETION-STATUS.md',
}
# No Node filesystem reader traverses Apple UI test sources or the Rust clock
# selector. These exact reviewed byte transitions permit only the known runner
# export/resume change and strict filter-header fix plus its Rust regression.
TRANSITIONS = {
    'scripts/apple-tv-navigation-test': ('c758a25eb73a8991508d5ed165e615dd12c49b5e8edbb990f97826ddc1dc0b4e', '8c9f0687923af53d97608cf90ced64c26d75abce3dd4380cef403aacf0269c68'),
    'crates/plurxd/src/subtitle_translation/runtime.rs': ('ceaac16fb23fe9cc643409b53f8feb28736ab487ff9acc5864e9848645f94fcb', '4b7165465a70f4db74329b009e3755190bf923d7c5e87dbaf28f6b03b31f2ba8'),
}


def job_block(workflow, name):
    matches = list(re.finditer(r'(?ms)^  ' + re.escape(name) + r':\n.*?(?=^  [A-Za-z0-9_]+:|\Z)', workflow))
    require(len(matches) == 1, 'Ambiguous workflow job: ' + name)
    return matches[0].group()


def producer(commit, current):
    old = git('show', commit + ':' + WORKFLOW)
    now = git('show', current + ':' + WORKFLOW)
    block = job_block(old, 'web_compile')
    digest = hashlib.sha256(block.encode()).hexdigest()
    require(digest in (LEGACY_JOB_SHA256, WRAPPED_JOB_SHA256), 'Unknown web command producer')
    require(hashlib.sha256(job_block(now, 'web_compile').encode()).hexdigest() == WRAPPED_JOB_SHA256,
            'Current web command environment/control flow changed')
    # Global env, defaults, permissions and the other producer jobs cannot
    # drift unnoticed while the workflow file is outside the unit closure.
    for name in ('web_compile', 'apple_compile'):
        old = old.replace(job_block(old, name), '')
        now = now.replace(job_block(now, name), '')
    require(old == now, 'Shared workflow configuration changed')
    if digest == WRAPPED_JOB_SHA256:
        require(git('rev-parse', commit + ':validation/web_unit_receipts.py') ==
                git('rev-parse', current + ':validation/web_unit_receipts.py'),
                'Unknown historical web wrapper implementation')
    return 'legacy' if digest == LEGACY_JOB_SHA256 else 'wrapped'


def source_inputs(commit):
    files = {}
    for entry in git('ls-tree', '-r', '-z', commit).split('\0'):
        if not entry:
            continue
        header, path = entry.split('\t', 1)
        mode, kind, oid = header.split()
        require(mode in ('100644', '100755') and kind == 'blob',
                'Web source tree contains a symlink/submodule: ' + path)
        if path in EXEMPT or path.startswith('clients/apple/UITests/'):
            continue
        files[path] = (mode, kind, oid)
    return files


def unchanged_inputs(commit, current):
    old, now = source_inputs(commit), source_inputs(current)
    require(old.keys() == now.keys(), 'Web input membership changed')
    transitions = []
    for path in old:
        if old[path] == now[path]:
            continue
        require(old[path][:2] == now[path][:2] and path in TRANSITIONS,
                'Web input changed: ' + path)
        pair = tuple(hashlib.sha256(subprocess.check_output(
            ['git', 'show', source + ':' + path], timeout=10)).hexdigest()
            for source in (commit, current))
        require(pair == TRANSITIONS[path], 'Unreviewed web input transition: ' + path)
        transitions.append(path)
    for source in (commit, current):
        require(hashlib.sha256(subprocess.check_output(
            ['git', 'show', source + ':Makefile'], timeout=10)).hexdigest() == MAKEFILE_SHA256,
            'Unknown web command inventory')
    def digest(files):
        return hashlib.sha256(json.dumps(sorted(files.items())).encode()).hexdigest()
    return {'original_input_digest': digest(old), 'current_input_digest': digest(now),
            'reviewed_transitions': sorted(transitions)}


def candidate_runs(api, scope, current_run):
    """Same-PR final preflight artifacts are discovery hints, never proof."""
    name = journal_key(scope)
    found = set()
    for page in range(1, MAX_ARTIFACT_PAGES + 1):
        batch = api.get('/actions/artifacts', {'name': name, 'page': page, 'limit': 50})
        require(isinstance(batch, list) and len(batch) <= 50, 'Invalid web origin pagination')
        for artifact in batch:
            if (isinstance(artifact, dict) and artifact.get('name') == name
                    and artifact.get('expired') is False and type(artifact.get('run_id')) is int
                    and 0 < artifact['run_id'] < current_run):
                found.add(artifact['run_id'])
        if len(batch) < 50 or len(found) >= MAX_CANDIDATES:
            break
    return sorted(found, reverse=True)[:MAX_CANDIDATES]


def authenticate(api, scope, current, current_run, rid, command, node):
    require(type(rid) is int and 0 < rid < current_run, 'Invalid web origin run')
    run = api.get(f'/actions/runs/{rid}')
    require(run['id'] == rid and run['repository']['id'] == scope['repository'], 'Foreign web origin run')
    bind_event(run, scope)
    commit = run['commit_sha']
    require(commit_available(commit), 'Web origin source unavailable')
    require(subprocess.run(['git', 'merge-base', '--is-ancestor', commit, current],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode == 0,
            'Web origin is not an ancestor')
    kind = producer(commit, current)
    inputs = unchanged_inputs(commit, current)
    jobs = [job for job in api.pages(f'/actions/runs/{rid}/jobs') if job['name'] == JOB]
    require(len(jobs) == 1, 'Ambiguous web origin job')
    job = jobs[0]
    require(job['run_id'] == rid and job['repo_id'] == scope['repository']
            and job['attempt'] == 1 and terminal_status(job) == 'success'
            and job['runs_on'] == RUNS_ON, 'Web origin job did not pass its environment contract')
    raw = api.bytes(f"/actions/jobs/{positive(job['id'])}/logs")
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', line) for line in raw.decode().splitlines()]
    log = '\n'.join(lines)
    versions = re.findall(r'^node: (v22\.[0-9]+\.[0-9]+)$', log, re.MULTILINE)
    require(versions == [node] and commit + f":refs/remotes/pull/{scope['pr']}/head" in log
            and lines[-1:] == ['🏁  Job succeeded']
            and re.search(r'^# tests [1-9][0-9]*$', log, re.MULTILINE)
            and re.search(r'^# fail 0$', log, re.MULTILINE)
            and not re.search(r'^# fail [1-9][0-9]*$', log, re.MULTILINE),
            'Web original checkout/toolchain/executed-success proof unavailable')
    # The exact legacy block executes all three commands unconditionally.
    # A newer wrapper may have adopted a command; only its own post-execution
    # marker can authenticate that command as a new original pass.
    if kind == 'wrapped':
        marker = f'WEB-UNIT-ORIGINAL-PASS {command} {commit}'
        require(lines.count(marker) == 1, 'Adopted web wrapper is not original execution evidence')
    return {'repository': scope['repository'], 'pr': scope['pr'], 'run': rid,
            'job': job['id'], 'attempt': 1, 'commit': commit, 'command': COMMANDS[command],
            'node': node, 'log_sha256': hashlib.sha256(raw).hexdigest(), **inputs}


def write_report(command, report):
    path = REPORTS / (command + '.json')
    require(all(not item.is_symlink() for item in (path, *path.parents)), 'Unsafe web receipt storage')
    atomic_json(path, report)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--command', choices=COMMANDS, default='suite')
    command = parser.parse_args(argv).command
    api, scope, current, run_id = api_context()
    node = subprocess.check_output(['node', '--version'], text=True, timeout=10).strip()
    require(re.fullmatch(r'v22\.[0-9]+\.[0-9]+', node), 'Current web Node toolchain differs from pinned major')
    try:
        candidates = candidate_runs(api, scope, run_id)
    except IntegrityError:
        raise
    except UNADOPTABLE as error:
        print('Web unit history unavailable — ' + reason(error), flush=True)
        candidates = []
    for rid in candidates:
        try:
            original = authenticate(api, scope, current, run_id, rid, command, node)
            write_report(command, {'version': 1, 'scope': scope, 'current_commit': current,
                         'current_run': run_id, 'disposition': 'adopted', 'original': original})
            print(f'WEB-UNIT-ADOPTED {command} original-run={rid} original-job={original["job"]}', flush=True)
            return 0
        except IntegrityError:
            raise
        except UNADOPTABLE as error:
            print(f'Web unit origin {rid} not adopted — {reason(error)}', flush=True)
    result = subprocess.run(COMMANDS[command])
    clean = git('rev-parse', 'HEAD') == current and not git('diff', '--name-only', 'HEAD')
    write_report(command, {'version': 1, 'scope': scope, 'current_commit': current,
                 'current_run': run_id, 'disposition': 'executed', 'command': COMMANDS[command],
                 'returncode': result.returncode, 'source_unchanged': clean, 'node': node})
    require(clean, 'Web source changed during original unit execution')
    if result.returncode == 0:
        print(f'WEB-UNIT-ORIGINAL-PASS {command} {current}', flush=True)
    return result.returncode


if __name__ == '__main__':
    raise SystemExit(main())
