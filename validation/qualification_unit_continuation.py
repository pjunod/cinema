"""Authenticate complete command coverage without executing unit bodies.

This adapter does not turn a failed historical job into a passing lane. Its
commands come from the current committed repository, never from a receipt.
Completed Python suites have aggregate coverage, not invented per-ID outcomes.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
from functools import lru_cache

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation.main_preflight_adoption import run_commands

JOB_NAMES = {
    'store_contract': 'replicated Store contracts (legacy)',
    'wal': 'replicated WAL recovery contracts',
    'validation': 'fast policy and contract preflight',
    'operations': 'fast policy and contract preflight',
    'android_jvm': 'Android JVM tests and lint',
}
# Literal historical invocation identities; these are not executable receipt data.
COMMANDS = {
    'store_contract': 'make cluster-store-check',
    'wal': 'make cluster-wal-check',
    'validation': "python3 -m unittest discover -s tests/validation -p 'test_*.py'",
    'operations': 'make operations-check',
    'android_jvm': 'BUILDX_CONFIG=target/validation/buildx make android-test',
}
TARGETS = ('store_contract', 'schema_lineage_upgrade')
STORE_FEATURES = 'cluster-read-cost-validation,hiqlite-contract-tests'
IGNORED_STORE_HELPERS = {'hiqlite_contract_node_process', 'hiqlite_activation_node_process',
                         'k05_capture_hiqlite_statements'}


def clean_log(raw):
    text = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', raw, flags=re.M)
    return re.sub(r'\x1b\[[0-9;]*[A-Za-z]', '', text)


def changed_inputs(record, candidate):
    """No protocol-path exemption: the adapter/workflow are tested inputs too."""
    changes = record['changes']
    paths = retention.changed(record['source_commit'], candidate)
    receipts.require(isinstance(changes, dict) and set(changes) == set(paths),
                     'Continuation must classify every changed path')
    affected = set()
    for path, row in changes.items():
        receipts.require(set(row) == {'before', 'after', 'affected_ids', 'reason'}
                         and isinstance(row['reason'], str) and row['reason'].strip(),
                         'Missing reviewed continuation applicability reason')
        old = retention.git_bytes(record['source_commit'], path)
        new = retention.git_bytes(candidate, path)
        receipts.require(row['before'] == (retention.digest(old) if old is not None else None)
                         and row['after'] == (retention.digest(new) if new is not None else None),
                         'Continuation source witness mismatch')
        ids = row['affected_ids']
        receipts.require(isinstance(ids, list) and len(ids) == len(set(ids))
                         and all(isinstance(i, str) and 0 < len(i) <= 1024 for i in ids),
                         'Invalid continuation affected identity')
        affected.update(ids)
    return affected


def job_steps(workflow, job):
    """Retain step boundaries so pipefail in another step is not authority."""
    jobs = list(re.finditer(r"(?m)^  ([A-Za-z0-9_]+):\s*$", workflow))
    matches = [i for i, match in enumerate(jobs) if match[1] == job]
    receipts.require(len(matches) == 1, 'Ambiguous historical workflow job')
    index = matches[0]
    body = workflow[jobs[index].end():jobs[index + 1].start()
                    if index + 1 < len(jobs) else len(workflow)]
    starts = list(re.finditer(r"(?m)^      - ", body))
    return [body[start.start():starts[i + 1].start()
                 if i + 1 < len(starts) else len(body)]
            for i, start in enumerate(starts)]


def historical_command(workflow, kind):
    job = {'store_contract': 'cluster_store_legacy', 'wal': 'cluster_wal',
           'android_jvm': 'android_jvm'}.get(kind, 'preflight')
    expected = {
        'store_contract': ['set -euo pipefail', 'mkdir -p target/validation',
            'make cluster-store-check 2>&1 | tee target/validation/cluster-store.log'],
        'wal': [COMMANDS['wal']],
        'validation': ['make validation-lint', COMMANDS['validation']],
        'operations': [COMMANDS['operations']],
        'android_jvm': ['mkdir -p target/validation/buildx', COMMANDS['android_jvm']],
    }[kind]
    steps = job_steps(workflow, job)
    checkouts = [step for step in steps if re.search(
        r'(?m)^      - uses: https://data\.forgejo\.org/actions/checkout@11d5960a326750d5838078e36cf38b85af677262(?: \# v4)?$', step)]
    receipts.require(len(checkouts) == 1
                     and re.search(r'(?m)^          persist-credentials: false$', checkouts[0])
                     and not re.search(r'(?m)^        if:', checkouts[0])
                     and not re.search(r'(?m)^          ref:', checkouts[0]),
                     'Historical checkout does not bind the actual event source')
    for step in steps:
        if re.search(r'(?m)^        (?:working-directory|env):', step):
            continue
        shell = re.findall(r'(?m)^        shell: (.+)$', step)
        if shell and shell != ['bash']:
            continue
        commands = run_commands('jobs:\n  proof:\n' + step, 'proof')
        if commands == expected:
            return True
    return False


def authenticate_scope(api, run_id, run, workflow, document, prior, repository):
    """A supplied input alone does not prove that manual binding executed."""
    event = receipts.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    base = receipts.sha(inputs['promotion_base_sha'])
    receipts.require(subprocess.run(['git', 'merge-base', '--is-ancestor', base, prior],
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     timeout=10).returncode == 0,
                     'Historical candidate did not contain its declared main base')
    scopes = [j for j in api.pages(f'/actions/runs/{run_id}/jobs')
              if j['name'] == 'validation scope']
    receipts.require(len(scopes) == 1 and scopes[0]['status'] == 'success'
                     and scopes[0]['attempt'] == 1 and scopes[0]['run_id'] == run_id
                     and scopes[0]['repo_id'] == repository,
                     'Historical manual binding scope did not succeed uniquely')
    steps = job_steps(workflow, 'scope')
    receipts.require(not re.search(r'(?m)^defaults:', workflow),
                     'Historical workflow changes the default command executor')
    scope_header = re.search(r'(?ms)^  scope:\n(.*?)(?=^    steps:)', workflow)
    receipts.require(scope_header is not None
                     and not re.search(r'(?m)^    (?:continue-on-error|defaults|env):', scope_header[1]),
                     'Historical binding scope masks errors or overrides its context')
    bind = [step for step in steps if re.search(r'(?m)^        id: promotion$', step)]
    receipts.require(len(bind) == 1, 'Historical promotion binding step unavailable')
    # Closed, source-backed condition: all three nonempty values make it true.
    expected = "github.event_name == 'workflow_dispatch' && (inputs.promotion_pr != '' || inputs.promotion_head_sha != '' || inputs.promotion_base_sha != '')"
    condition = re.search(r'(?ms)^        if: >-\n(.*?)(?=^        [A-Za-z_]+:)', bind[0])
    receipts.require(condition is not None
                     and ' '.join(condition[1].split()) == expected
                     and all(inputs[k] for k in ('promotion_pr', 'promotion_head_sha', 'promotion_base_sha')),
                     'Historical manual binding condition is not proved true')
    receipts.require(not re.search(r'(?m)^        (?:continue-on-error|working-directory|shell):', bind[0]),
                     'Historical binding is masked or changes its execution context')
    environment = re.search(r'(?ms)^        env:\n(.*?)(?=^        [A-Za-z_]+:)', bind[0])
    receipts.require(environment is not None, 'Historical binding environment unavailable')
    rows = re.findall(r'(?m)^          ([A-Z_]+): (.+)$', environment[1])
    expected_environment = {
        'GITHUB_TOKEN': '${{ github.token }}',
        'PLURX_PROMOTION_PR': '${{ inputs.promotion_pr }}',
        'PLURX_PROMOTION_HEAD_SHA': '${{ inputs.promotion_head_sha }}',
        'PLURX_PROMOTION_BASE_SHA': '${{ inputs.promotion_base_sha }}',
    }
    receipts.require(len(rows) == len(expected_environment) and dict(rows) == expected_environment
                     and len(environment[1].splitlines()) == len(rows),
                     'Historical binding environment is not the exact input mapping')
    commands = re.findall(r'(?m)^        run: (.*)$', bind[0])
    receipts.require(commands == ['python3 -m validation.qualification --bind-manual >> "$GITHUB_OUTPUT"']
                     and all(line in bind[0] for line in (
                         'PLURX_PROMOTION_PR: ${{ inputs.promotion_pr }}',
                         'PLURX_PROMOTION_HEAD_SHA: ${{ inputs.promotion_head_sha }}',
                         'PLURX_PROMOTION_BASE_SHA: ${{ inputs.promotion_base_sha }}')),
                     'Historical scope does not execute the actual binding command')
    receipts.require(retention.git_bytes(prior, 'validation/qualification.py') is not None,
                     'Historical manual binding implementation unavailable')
    return base


@lru_cache(maxsize=16)
def frozen_python_ids(prior, kind):
    """Load the authenticated old source's discovery universe without running tests.

    The subprocess receives source only and a minimal credential-free environment.
    Its imports, loader errors, duplicate IDs and timeout are fail-closed.
    """
    receipts.sha(prior)
    receipts.require(kind in {'validation', 'operations'}, 'Not a Python suite')
    with tempfile.TemporaryDirectory(prefix='plurx-unit-discovery-') as temporary:
        root = Path(temporary)
        archive = root / 'source.tar'
        with archive.open('wb') as output:
            result = subprocess.run(['git', 'archive', prior], stdout=output,
                                    stderr=subprocess.PIPE, timeout=60)
        receipts.require(result.returncode == 0 and archive.stat().st_size <= 256 * 1024 * 1024,
                         'Historical source archive unavailable or oversized')
        with tarfile.open(archive) as source:
            members = source.getmembers()
            receipts.require(len(members) <= 20000
                             and sum(member.size for member in members) <= 256 * 1024 * 1024,
                             'Historical extraction bound exhausted')
            source.extractall(root, filter='data')
        archive.unlink()
        program = ("import json; from validation.python_unit_receipts import discover; "
                   "print(json.dumps(sorted(t.id() for t in discover(" + repr(kind) + "))))")
        environment = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': temporary,
                       'PYTHONDONTWRITEBYTECODE': '1', 'GIT_CONFIG_NOSYSTEM': '1',
                       'GIT_CONFIG_GLOBAL': os.devnull}
        completed = subprocess.run([sys.executable, '-c', program], cwd=root,
                                   env=environment, capture_output=True, timeout=120)
        receipts.require(completed.returncode == 0, 'Historical Python inventory discovery failed')
        ids = receipts.bounded_json(completed.stdout)
        receipts.require(isinstance(ids, list) and 0 < len(ids) <= receipts.MAX_TESTS
                         and len(ids) == len(set(ids)) and all(isinstance(i, str) for i in ids),
                         'Historical Python inventory is ambiguous')
        return tuple(ids)



def make_commands(text, target):
    """Literal committed recipes define command identities, never receipt strings."""
    marker = target + ': ##'
    receipts.require(text.count(marker) == 1, 'Ambiguous canonical Make recipe')
    block = text.split(marker, 1)[1].split('\n.PHONY:', 1)[0]
    result, parts = [], []
    for line in block.splitlines()[1:]:
        if not line.startswith('\t') or not line.strip() or line.lstrip().startswith('#'):
            continue
        value = line.strip()
        parts.append(value.removesuffix('\\').strip())
        if not value.endswith('\\'):
            result.append(' '.join(parts))
            parts = []
    receipts.require(result and not parts and len(result) == len(set(result)),
                     'Incomplete or repeated canonical command')
    return result


def rust_identity(command):
    """Bind package, target, feature graph, filter and invocation environment."""
    words = shlex.split(command)
    at = words.index('$(CARGO)')
    receipts.require(words[at + 1] == 'test' and '--locked' in words,
                     'Canonical Rust command is not a locked test')
    environment = dict(word.split('=', 1) for word in words[:at] if '=' in word)
    receipts.require(len(environment) == at, 'Unknown canonical command prefix')
    harness_at = words.index('--') if '--' in words else len(words)
    cargo, harness = words[at + 2:harness_at], words[harness_at + 1:]
    flags, filters, i = {}, [], 0
    valued = {'-p', '--manifest-path', '--test', '--bin', '--features'}
    plain = {'--locked', '--lib', '--no-default-features', '--release'}
    while i < len(cargo):
        word = cargo[i]
        if word in valued:
            receipts.require(word not in flags and i + 1 < len(cargo), 'Ambiguous Rust argument')
            flags[word] = cargo[i + 1]
            i += 2
        elif word in plain:
            receipts.require(word not in flags, 'Duplicate Rust flag')
            flags[word] = True
            i += 1
        else:
            receipts.require(not word.startswith('-'), 'Unknown Rust compilation argument')
            filters.append(word)
            i += 1
    receipts.require(len(filters) <= 1 and ('--lib' in flags or '--test' in flags or '--bin' in flags or flags.get('--manifest-path') == 'vendor/hiqlite-wal/Cargo.toml'),
                     'Rust command lacks one explicit target')
    receipts.require(all(word in {'--exact', '--nocapture', '--ignored', '--test-threads=1'}
                         for word in harness), 'Unknown Rust harness semantics')
    return {'cargo': flags, 'filter': filters[0] if filters else None,
            'harness': harness, 'environment': environment}


def canonical(candidate):
    makefile = retention.git_bytes(candidate, 'Makefile')
    receipts.require(makefile is not None, 'Canonical Makefile unavailable')
    result = {'validation': COMMANDS['validation'], 'operations': COMMANDS['operations'],
              'android_jvm': COMMANDS['android_jvm']}
    for kind, target in [('store_contract', 'cluster-store-check'), ('wal', 'cluster-wal-check')]:
        for index, command in enumerate(make_commands(makefile.decode(), target), 1):
            rust_identity(command)
            result[f'{kind}:{index}'] = command
    return result


def native_inventory(item, identity, candidate):
    receipts.require(set(item) == {'source_commit', 'ids', 'list_log', 'binary_sha256', 'features'},
                     'Unknown native inventory fields')
    receipts.require(item['source_commit'] == candidate
                     and retention.DIGEST.fullmatch(item['binary_sha256']),
                     'Native inventory is not bound to current candidate and binary')
    raw = retention.decoded_log(item['list_log'])
    full = [line.removesuffix(': test') for line in raw.splitlines() if line.endswith(': test')]
    receipts.require(full and len(full) == len(set(full)), 'Native --list inventory ambiguous')
    selector = identity['filter']
    ids = [test_id for test_id in full if selector is None or
           (test_id == selector if '--exact' in identity['harness'] else selector in test_id)]
    receipts.require(item['ids'] == ids, 'Inventory does not implement actual Rust filter')
    expected = identity['environment'].get('PLURX_EXPECT_TEST_COUNT')
    if expected is not None:
        receipts.require(len(ids) == int(expected), 'Native inventory differs from canonical count')
    elif '--exact' in identity['harness']:
        receipts.require(len(ids) == 1, 'Exact command must select exactly one test')
    return ids


def coverage(record, text, kind, discovery=frozen_python_ids):
    """Full-discovery coverage stays typed; no fabricated positive log rows."""
    claim = record['coverage']
    lines = text.splitlines()
    first, last = claim['first_line'], claim['last_line']
    receipts.require(type(first) is int and type(last) is int
                     and 0 < first <= last <= len(lines), 'Invalid actual execution block')
    body = '\n'.join(lines[first - 1:last])
    mode = claim['mode']
    if mode == 'rust':
        receipts.require(set(claim) == {'mode', 'first_line', 'last_line', 'format', 'outcomes'},
                         'Unknown native coverage fields')
        actual = retention.log_outcomes(body, claim['format'])
        receipts.require(claim['format'] in {'rust', 'rust-serial'} and actual == claim['outcomes'],
                         'Actual Rust outcomes contradict command proof')
        return actual
    if mode == 'python-discovery':
        receipts.require(kind in {'validation', 'operations'}
                         and set(claim) == {'mode', 'first_line', 'last_line', 'ids'},
                         'Unknown Python discovery fields')
        ids = list(discovery(record['source_commit'], kind))
        receipts.require(ids == claim['ids'] and ids, 'Python IDs differ from actual frozen discovery')
        summary = re.fullmatch(r'Ran (\d+) tests in [0-9.]+s\n\s*\nOK', body.strip())
        receipts.require(summary is not None and int(summary[1]) == len(ids),
                         'Full Python discovery did not reach its actual outer plain-OK summary')
        summaries = [m for m in re.finditer(r'^Ran (\d+) tests in [0-9.]+s\n\s*\nOK$', text, re.M)
                     if int(m[1]) == len(ids)]
        receipts.require(len(summaries) == 1
                         and text[:summaries[0].start()].count('\n') + 1 == first,
                         'Ambiguous nested or incomplete Python summary')
        return {test_id: 'pass' for test_id in ids}
    if mode == 'python-named':
        receipts.require(kind in {'validation', 'operations'}
                         and set(claim) == {'mode', 'first_line', 'last_line', 'outcomes'},
                         'Unknown focused Python fields')
        rows = re.findall(r'^\w+ \(([^)]+)\) \.\.\. (ok|FAIL|ERROR|skipped)(?:\s|$)', body, re.M)
        actual = {test_id: 'pass' if value == 'ok' else 'ignored' if value == 'skipped' else 'fail'
                  for test_id, value in rows}
        receipts.require(rows and len(rows) == len(actual) and actual == claim['outcomes'],
                         'Focused Python actual names disagree')
        return actual
    receipts.require(mode == 'jvm-whole-command' and kind == 'android_jvm'
                     and set(claim) == {'mode', 'first_line', 'last_line', 'tasks'},
                     'Unsupported typed command coverage')
    tasks = ['testDebugUnitTest', 'lintDebug']
    receipts.require(claim['tasks'] == tasks
                     and all(re.search(r'^> Task :app:' + task + r'[ \t]*$', body, re.M) for task in tasks)
                     and re.search(r'^BUILD SUCCESSFUL in ', body, re.M)
                     and not re.search(r'^BUILD FAILED|^FAILURE: Build failed', body, re.M),
                     'JVM command did not execute genuine test and lint tasks successfully')
    return {'whole-command': 'pass'}



def command_log(proof, record):
    reference = record['log']
    receipts.require(set(reference) == {'sha256_ref'} and retention.DIGEST.fullmatch(reference['sha256_ref']),
                     'Invalid shared command log reference')
    item = proof['logs'][reference['sha256_ref']]
    receipts.require(item['sha256'] == reference['sha256_ref'], 'Shared actual log identity mismatch')
    return retention.decoded_log(item)


def authenticate(api, proof, document, repository):
    """Local records use the authenticated writer; CI records also use live logs."""
    for item in proof['commands']:
        kind = item['name'].split(':')[0]
        for record in item['records']:
            origin = record['origin']
            if origin['kind'] != 'authenticated-ci':
                continue
            match = re.fullmatch(r'run:(\d+)/job:(\d+)', origin['identity'])
            receipts.require(match is not None, 'Malformed CI command origin')
            run_id, job_id = map(receipts.positive, match.groups())
            run = api.get(f'/actions/runs/{run_id}')
            jobs = [job for job in api.pages(f'/actions/runs/{run_id}/jobs') if job['id'] == job_id]
            receipts.require(run['repository']['id'] == repository and run['commit_sha'] == record['source_commit']
                             and run['workflow_id'] == 'ci.yml' and len(jobs) == 1
                             and jobs[0]['name'] == JOB_NAMES[kind] and jobs[0]['attempt'] == 1
                             and jobs[0]['run_id'] == run_id and jobs[0]['repo_id'] == repository
                             and jobs[0]['status'] in {'success','failure'}, 'Historical command job identity mismatch')
            event = receipts.bounded_json(run['event_payload'].encode())
            inputs = event['inputs']
            receipts.require(run.get('trigger_event') == 'workflow_dispatch'
                             and event['ref'] == 'refs/heads/' + document['branch']
                             and inputs['promotion_pr'] == str(document['pr'])
                             and inputs['promotion_head_sha'] == record['source_commit'],
                             'Historical commands did not run on this main-bound effort')
            workflow = retention.git_bytes(record['source_commit'], '.github/workflows/ci.yml')
            receipts.require(workflow is not None and historical_command(workflow.decode(), kind),
                             'Historical command executor differs')
            authenticate_scope(api, run_id, run, workflow.decode(), document, record['source_commit'], repository)
            raw = api.bytes(f'/actions/jobs/{job_id}/logs')
            receipts.require(retention.digest(raw) == record['log']['sha256_ref']
                             and command_log(proof, record).encode() == raw, 'Authenticated command log mismatch')

def validate(proof, candidate, existing=(), commands=canonical, discovery=frozen_python_ids):
    """Only a complete source-applicable union authorizes command adoption."""
    receipts.require(set(proof) == {'version', 'source_commit', 'commands', 'logs'} and proof['version'] == 1
                     and proof['source_commit'] == candidate, 'Unknown or stale command proof')
    receipts.require(isinstance(proof['logs'], dict) and proof['logs'] and len(proof['logs']) <= 1024,
                     'Missing or oversized shared actual logs')
    canonical_commands = commands(candidate)
    claims = proof['commands']
    receipts.require(isinstance(claims, list) and claims and len(claims) <= 512,
                     'Empty or oversized command enrollment')
    seen, by_kind = set(), {}
    for item in claims:
        receipts.require(set(item) == {'name', 'invocation', 'environment', 'inventory', 'records', 'obligations'},
                         'Unknown command unit fields')
        name = item['name'];kind = name.split(':')[0]
        receipts.require(name in canonical_commands and name not in seen
                         and item['invocation'] == canonical_commands[name], 'Unknown or repeated canonical command')
        seen.add(name)
        receipts.require(not (kind == 'android_jvm' and 'android_jvm' in existing),
                         'Multiple unit authorities for one JVM command')
        environment = item['environment']
        receipts.require(isinstance(environment, dict)
                         and {'platform', 'toolchain', 'features', 'runtime'} <= environment.keys()
                         and isinstance(environment['runtime'], dict), 'Incomplete command environment values')
        if kind in {'wal', 'store_contract'}:
            identity = rust_identity(item['invocation'])
            receipts.require(environment['toolchain'] == 'rust1.97.1'
                             and environment['runtime'].get('test_threads') == (1 if '--test-threads=1' in identity['harness'] or '--exact' in identity['harness'] else 'libtest-default')
                             and environment['runtime'].get('rust_min_stack') == (int(identity['environment']['RUST_MIN_STACK']) if 'RUST_MIN_STACK' in identity['environment'] else None),
                             'Rust command runtime differs from exact canonical invocation')
            ids = native_inventory(item['inventory'], identity, candidate)
            requested = identity['cargo'].get('--features', '').split(',')
            requested = [feature for feature in requested if feature]
            receipts.require(set(requested) <= set(item['inventory']['features'])
                             and environment['features'] == item['inventory']['features'],
                             'Rust feature graph mismatch')
        elif kind in {'validation', 'operations'}:
            ids = list(discovery(candidate, kind))
            receipts.require(item['inventory'] == {'ids': ids} and ids, 'Current Python discovery differs')
        else:
            receipts.require(item['inventory'] == {'mode': 'whole-command'}, 'JVM names must remain opaque')
            ids = ['whole-command']
        receipts.require(len(ids) == len(set(ids)), 'Repeated current unit identity')
        records = item['records'];receipts.require(isinstance(records, list) and records, 'No actual command executions')
        result, failed, sequences = {}, set(), set()
        for record in sorted(records, key=lambda row: row['sequence']):
            receipts.require(set(record) == {'sequence', 'source_commit', 'environment', 'origin', 'changes', 'log', 'coverage', 'exit_code'},
                             'Unknown command execution record')
            sequence = record['sequence']
            receipts.require(type(sequence) is int and sequence > 0 and sequence not in sequences,
                             'Ambiguous actual execution chronology');sequences.add(sequence)
            receipts.require(record['environment'] == environment, 'Actual runtime environment differs')
            origin = record['origin'];receipts.require(set(origin) == {'kind', 'identity', 'command'}
                and origin['kind'] in {'attested-local', 'authenticated-ci'} and origin['identity'] and origin['command'],
                'Missing real execution origin')
            invalid = changed_inputs(record, candidate)
            receipts.require(invalid <= set(ids), 'Unknown affected command unit')
            actual = coverage(record, clean_log(command_log(proof, record)), kind, discovery)
            receipts.require(set(actual) <= set(ids), 'Execution belongs to another command inventory')
            if kind == 'android_jvm':
                receipts.require(not invalid and record['exit_code'] == 0,
                                 'Opaque JVM result cannot qualify changed unit inputs or failed command')
            for test_id, outcome in actual.items():
                if outcome == 'fail':failed.add(test_id)
                if test_id not in invalid:result[test_id] = outcome
        receipts.require(set(result) == set(ids) and all(value in {'pass', 'ignored'} for value in result.values())
                         and all(result[test_id] == 'pass' for test_id in failed), 'Incomplete or failed applicable command coverage')
        obligations = item['obligations']
        receipts.require(isinstance(obligations, list), 'Unknown command completion obligations')
        # A failed outer command remains failed. A native-pass union requires an
        # explicit candidate-bound successful wrapper/count validation witness.
        failed_commands = [row['sequence'] for row in records if row['exit_code'] != 0]
        receipts.require(len(obligations) == len(failed_commands), 'Failed outer command obligation not discharged')
        for obligation, sequence in zip(obligations, failed_commands):
            receipts.require(set(obligation) == {'sequence', 'source_commit', 'invocation', 'native_pass_count', 'reason'}
                             and obligation['sequence'] == sequence and obligation['source_commit'] == candidate
                             and obligation['invocation'] == item['invocation'] and obligation['reason'],
                             'Invalid repaired command obligation')
            receipts.require(obligation['native_pass_count'] == sum(value == 'pass' for value in result.values()),
                             'Repaired wrapper/count obligation disagrees with complete native union')
        by_kind.setdefault(kind, []).append(name)
    for kind, names in by_kind.items():
        receipts.require(set(names) == {name for name in canonical_commands if name.split(':')[0] == kind},
                         'Incomplete canonical command cohort')
    return {kind: {'mode': 'retained-complete-command-union', 'commands': sorted(names),
                   'validator': retention.digest(Path(__file__).read_bytes())} for kind, names in by_kind.items()}
