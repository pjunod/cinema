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
import signal
import selectors
import time
import shlex
import subprocess
import sys
import tarfile
import tempfile
from functools import lru_cache

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation.main_preflight_adoption import run_commands

RUST_KINDS = {'store_contract','wal','topology','transport'}
COUNT_WRAPPERS = {'e85ffaa0382d6261d5749b248fe48b48db745da67c2926a753e0642a91facf07',
                  'b6810e49dae6f5c5b2eacde7d7db15b0c5becc0c72e735fa057a13b8aa0cf1f4'}
JOB_NAMES = {
    'topology': 'replicated topology contracts',
    'transport': 'cluster transport recovery contracts',
    'store_contract': 'replicated Store contracts (legacy)',
    'wal': 'replicated WAL recovery contracts',
    'validation': 'fast policy and contract preflight',
    'operations': 'fast policy and contract preflight',
    'android_jvm': 'Android JVM tests and lint',
}
# Literal historical invocation identities; these are not executable receipt data.
COMMANDS = {
    'topology':'make cluster-harness-check',
    'transport':'make cluster-transport-recovery-contracts',
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
           'android_jvm': 'android_jvm', 'topology':'cluster_topology',
           'transport':'cluster_transport_recovery_contracts'}.get(kind, 'preflight')
    expected = {
        'transport':['set -euo pipefail','mkdir -p target/validation',
                     'make cluster-transport-recovery-contracts \\',
                     '2>&1 | tee target/validation/cluster-transport-recovery-contracts.log'],
        'topology':['set -euo pipefail','mkdir -p target/validation','{',
                    'if [ "$RUN_CLUSTER_AUTH" = true ]; then',
                    'make hiqlite-vendor-clippy','make cluster-harness-check','fi',
                    'if [ "$RUN_HIQLITE_SPIKE" = true ]; then',
                    'cargo clippy --locked --manifest-path spikes/hiqlite-m0/Cargo.toml \\',
                    '--tests --no-deps -- -D warnings','make hiqlite-spike','fi',
                    '} 2>&1 | tee target/validation/cluster-topology.log'],
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
        if re.search(r'(?m)^        working-directory:',step):
            continue
        if re.search(r'(?m)^        env:',step):
            if kind != 'topology':
                continue
            rows=re.findall(r'(?m)^          ([A-Z_]+): (.+)$',step)
            if dict(rows) != {'RUN_CLUSTER_AUTH':'${{ needs.scope.outputs.cluster_auth }}',
                              'RUN_HIQLITE_SPIKE':'${{ needs.scope.outputs.hiqlite_spike }}'} or len(rows)!=2:
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
    prefix=[word for word in words[:at] if word != 'scripts/require-test-count']
    environment = dict(word.split('=',1) for word in prefix if '=' in word)
    receipts.require(len(environment) == len(prefix), 'Unknown canonical command prefix')
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
    receipts.require(len(filters) <= 1 and ('--lib' in flags or '--test' in flags or '--bin' in flags or flags.get('--manifest-path') == 'vendor/hiqlite-wal/Cargo.toml' or flags.get('-p') == 'plurx-cluster-check'),
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
    for kind,target in [('topology','cluster-harness-unit-check'),('transport','cluster-transport-recovery-contracts')]:
        text=makefile.decode()
        if kind == 'topology' and target+': ##' not in text:
            target='cluster-harness-check'
        selected=[]
        for command in make_commands(text,target):
            if '$(CARGO) test ' in command:
                rust_identity(command);selected.append(command)
        receipts.require(len(selected) == (8 if kind=='topology' else 6), 'Changed embedded unit cohort requires review')
        for index,command in enumerate(selected,1):result[f'{kind}:{index}']=command
    return result


def implicit_cluster_target(identity):
    flags=identity['cargo']
    return flags.get('-p') == 'plurx-cluster-check' and not any(flag in flags for flag in ('--lib','--bin','--test'))


def select_ids(full,identity):
    selector=identity['filter']
    return [test_id for test_id in full if selector is None or
            (test_id == selector if '--exact' in identity['harness'] else selector in test_id)]


def target_inventory(item,candidate,identity):
    receipts.require(set(item)=={'source_commit','ids','targets'} and item['source_commit']==candidate
                     and isinstance(item['targets'],list) and item['targets'],'Implicit command target footprint unavailable')
    ids=[];tags=set()
    for target in item['targets']:
        receipts.require(set(target)=={'name','kind','binary_sha256','features','list_log'}
                         and target['kind'] in [['lib'],['bin'],['test']]
                         and retention.DIGEST.fullmatch(target['binary_sha256']), 'Unknown native target footprint')
        tag=(target['name'],tuple(target['kind']))
        receipts.require(tag not in tags,'Duplicate native target footprint');tags.add(tag)
        raw=retention.decoded_log(target['list_log'])
        full=[line.removesuffix(': test') for line in raw.splitlines() if line.endswith(': test')]
        receipts.require(len(full)==len(set(full)),'Ambiguous target-native inventory')
        ids.extend(select_ids(full,identity))
    receipts.require(ids and item['ids']==ids and len(ids)==len(set(ids)), 'Implicit current selected union differs')
    return ids


def native_inventory(item, identity, candidate):
    if implicit_cluster_target(identity):
        return target_inventory(item,candidate,identity)
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


@lru_cache(maxsize=16)
def source_archive_digest(source):
    receipts.sha(source)
    with tempfile.TemporaryDirectory(prefix='plurx-unit-source-binding-') as temporary:
        path = Path(temporary) / 'source.tar'
        with path.open('wb') as output:
            completed = subprocess.run(['git','archive',source],stdout=output,stderr=subprocess.PIPE,timeout=60)
        receipts.require(completed.returncode == 0 and path.stat().st_size <= 256*1024*1024,
                         'Execution source archive unavailable')
        return retention.digest(path.read_bytes())


RUSTC_VERSION='rustc 1.97.1 (8bab26f4f 2026-07-14)'
RUSTC_FINGERPRINT=12019306335353385202
TEST_PROFILE_FINGERPRINTS={0:16018505129029273480,2:6599234302872229273}


@lru_cache(maxsize=32)
def test_profile(source,manifest):
    import tomllib
    path=manifest or 'Cargo.toml'
    raw=retention.git_bytes(source,path)
    receipts.require(raw is not None,'Historical profile manifest unavailable')
    document=tomllib.loads(raw.decode())
    profile=document.get('profile',{}).get('test',{})
    receipts.require(set(profile)<= {'debug'} and not any(key.startswith('package') for key in profile),
                     'Changed Rust test profile requires reviewed compatibility')
    debug=profile.get('debug',2)
    receipts.require(type(debug) is int and debug in {0,2},'Unknown Rust test debuginfo profile')
    return {'opt_level':'0','debuginfo':debug,'debug_assertions':True,'overflow_checks':True,'test':True}


def rust_environment(environment,identity,source):
    receipts.require(set(environment)=={'platform','toolchain','features','runtime'}
                     and environment['platform']=='linux-x64' and environment['toolchain']=='rust1.97.1'
                     and isinstance(environment['features'],list) and environment['features']==sorted(set(environment['features'])),
                     'Unknown actual Rust platform/toolchain/feature contract')
    runtime=environment['runtime']
    receipts.require(set(runtime)=={'test_threads','rust_min_stack','target','profile','rustc_version'}
                     and runtime['target']=='x86_64-unknown-linux-gnu' and runtime['rustc_version']==RUSTC_VERSION
                     and runtime['profile']==test_profile(source,identity['cargo'].get('--manifest-path')),
                     'Rust compiler/target/profile witness differs')


def normalized_cargo(command, *, compile_only=False):
    words = shlex.split(command)
    if compile_only:
        words = [word for word in words if word != '--no-run' and not word.startswith('--message-format=')]
    executors=[index for index,word in enumerate(words) if word == 'cargo' or re.fullmatch(r'/(?:home|opt)/(?:[^/ ]+/)*\.rustup/toolchains/1\.97\.1-x86_64-unknown-linux-gnu/bin/cargo',word)]
    receipts.require(len(executors)==1, 'Actual command has no uniquely pinned Cargo executor')
    at=executors[0]
    prefix = words[:at]
    receipts.require(all('=' in word or word == 'scripts/require-test-count' for word in prefix),
                     'Unknown actual Cargo command prefix')
    prefix = [word for word in prefix if word != 'scripts/require-test-count']
    return rust_identity(shlex.join(prefix + ['$(CARGO)'] + words[at+1:]))


def same_target(left, right):
    # Count-wrapper policy is evaluated separately. Target/filter/harness and
    # actual compilation/runtime semantics cannot change by assertion.
    keys = {'-p','--manifest-path','--test','--bin','--lib','--no-default-features','--release','--locked'}
    receipts.require({k:v for k,v in left['cargo'].items() if k in keys} ==
                     {k:v for k,v in right['cargo'].items() if k in keys}
                     and set(left['cargo'].get('--features','').split(',')) == set(right['cargo'].get('--features','').split(','))
                     and left['filter'] == right['filter'] and left['harness'] == right['harness']
                     and left['environment'].get('RUST_MIN_STACK') == right['environment'].get('RUST_MIN_STACK'),
                     'Historical command target/features/filter/runtime differ')


def logged_command(lines,start):
    parts=[];end=start
    while True:
        line=lines[end];parts.append(line.removesuffix('\\').strip())
        if not line.endswith('\\'):
            return ' '.join(parts),end
        end+=1
        receipts.require(end<len(lines),'Incomplete logged command continuation')


def historical_recipe(record,kind):
    text=retention.git_bytes(record['source_commit'],'Makefile')
    receipts.require(text is not None,'Historical Make recipe unavailable')
    target={'wal':'cluster-wal-check','store_contract':'cluster-store-check',
            'topology':'cluster-harness-check','transport':'cluster-transport-recovery-contracts'}[kind]
    if kind=='topology' and b'cluster-harness-unit-check: ##' in text:
        target='cluster-harness-unit-check'
    return make_commands(text.decode(),target)


def display_tokens(command,kind):
    words=shlex.split(command)
    result=[]
    for word in words:
        if word=='cargo' or re.fullmatch(r'/(?:home|opt)/(?:[^/ ]+/)*\.rustup/toolchains/1\.97\.1-x86_64-unknown-linux-gnu/bin/cargo',word):
            word='$(CARGO)'
        result.append(word)
    # The WAL Make target expands CARGO through its source-bound override.
    if kind=='wal' and 'scripts/require-test-count' in result:
        result.remove('scripts/require-test-count')
    return result


def executor_terminal(record,kind,lines,start):
    if record['origin']['kind']!='authenticated-ci':
        return None
    workflow=retention.git_bytes(record['source_commit'],'.github/workflows/ci.yml')
    receipts.require(workflow is not None,'Historical executor step unavailable')
    job={'store_contract':'cluster_store_legacy','wal':'cluster_wal','topology':'cluster_topology',
         'transport':'cluster_transport_recovery_contracts'}[kind]
    steps=[step for step in job_steps(workflow.decode(),job) if COMMANDS[kind] in step]
    receipts.require(len(steps)==1,'Historical executor step ambiguous')
    name=re.search(r'^      - name: (.+)$',steps[0],re.M)
    receipts.require(name is not None,'Historical executor step has no bound name')
    pattern=r'^\s*(?:✅|❌) (Success|Failure) - Main '+re.escape(name[1])+r'\s*$'
    matches=[(index,re.fullmatch(pattern,line)) for index,line in enumerate(lines) if index>start and re.fullmatch(pattern,line)]
    receipts.require(len(matches)<=1,'Actual executor completion ambiguous')
    return (matches[0][0],matches[0][1][1]=='Success') if matches else None


def require_native_terminals(body,actual,exit_code,command):
    terminals=re.findall(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;',body,re.M)
    receipts.require(terminals and sum(int(row[1]) for row in terminals)==sum(value=='pass' for value in actual.values())
                     and sum(int(row[2]) for row in terminals)==sum(value=='fail' for value in actual.values())
                     and sum(int(row[3]) for row in terminals)==sum(value=='ignored' for value in actual.values()),
                     'Complete native terminal counts contradict named outcomes')
    mismatch=re.search(r'^expected (?:at least )?\d+ matching Rust test\(s\), but \d+ passed$|^expected one passing outcome for ',body,re.M)
    if exit_code==0:
        receipts.require(all(row[0]=='ok' and row[2]=='0' for row in terminals) and mismatch is None
                         and not re.search(r'(?im)^(?:error:|fatal:|make: \*\*\*)',body),
                         'Actual failed Cargo command cannot be labelled successful')
        words=shlex.split(command)
        if 'scripts/require-test-count' in words:
            environment=normalized_cargo(command)['environment']
            count=sum(value=='pass' for value in actual.values())
            expected=environment.get('PLURX_EXPECT_TEST_COUNT')
            if expected is None and '--exact' in words:expected='1'
            receipts.require(expected is None or count==int(expected),'Actual historical exact wrapper predicate failed')
            minimum=environment.get('PLURX_EXPECT_TEST_COUNT_AT_LEAST')
            receipts.require(minimum is None or count>=int(minimum),'Actual historical minimum wrapper predicate failed')
            receipts.require(all(actual.get(name)=='pass' for name in environment.get('PLURX_EXPECT_TEST_NAMES','').split(',') if name),
                             'Actual historical required-name wrapper predicate failed')


def native_target_paths(identity,source):
    flags=identity['cargo'];package=flags.get('-p')
    if package is None:
        import tomllib
        manifest=retention.git_bytes(source,flags['--manifest-path'])
        receipts.require(manifest is not None,'Native target manifest unavailable')
        package=tomllib.loads(manifest.decode())['package']['name']
    if '--test' in flags:
        name=flags['--test'];kind='integration-test'
    elif '--bin' in flags:
        name=flags['--bin'];kind='bin'
    else:
        receipts.require('--lib' in flags,'Implicit missing-only target needs explicit target authority')
        name=package.replace('-','_');kind='lib'
    return package,name.replace('-','_'),'test-'+kind+'-'+name+'.json'


def bound_execution(record, item, raw, source_commands=canonical):
    """No receipt-selected slice can remove another outcome from one execution."""
    kind = item['name'].split(':')[0]
    origin = record['origin']
    receipts.require(set(origin) == {'kind','identity','command','execution'}, 'Missing execution binding')
    witness = origin['execution']
    lines = raw.splitlines()
    first = witness['first_line']
    receipts.require(type(first) is int and 0 < first <= len(lines), 'Invalid command anchor')
    if witness['mode'] == 'local-binary':
        receipts.require(origin['kind'] == 'attested-local' and kind in RUST_KINDS
                         and set(witness) == {'mode','first_line','build_invocation','archive_sha256','binary_sha256_before','binary_sha256_after','features','fingerprint','binary_path','fingerprint_path'},
                         'Unknown local binary witness')
        receipts.require(lines[first-1].startswith('COMMAND '), 'Local command anchor unavailable')
        argv = receipts.bounded_json(lines[first-1].removeprefix('COMMAND ').encode())
        receipts.require(isinstance(argv,list) and argv and all(isinstance(arg,str) for arg in argv)
                         and origin['command'] == shlex.join(argv), 'Local command contradicts actual argv')
        receipts.require(retention.DIGEST.fullmatch(witness['archive_sha256'])
                         and retention.DIGEST.fullmatch(witness['binary_sha256_before'])
                         and witness['binary_sha256_after'] == witness['binary_sha256_before']
                         and witness['features'] == record['environment']['features']
                         and witness['archive_sha256'] == source_archive_digest(record['source_commit'])
                         and isinstance(witness['fingerprint'],dict)
                         and json.loads(witness['fingerprint'].get('features','null')) == witness['features']
                         and witness['fingerprint'].get('profile')==TEST_PROFILE_FINGERPRINTS[record['environment']['runtime']['profile']['debuginfo']]
                         and witness['fingerprint'].get('rustc')==RUSTC_FINGERPRINT
                         and witness['fingerprint'].get('compile_kind')==0 and witness['fingerprint'].get('rustflags')==[], 'Local build/binary witness differs')
        build = normalized_cargo(witness['build_invocation'],compile_only=True);target = rust_identity(item['invocation'])
        keys = {'-p','--manifest-path','--test','--bin','--lib','--no-default-features','--release'}
        receipts.require({k:v for k,v in build['cargo'].items() if k in keys} == {k:v for k,v in target['cargo'].items() if k in keys}
                         and {feature for feature in target['cargo'].get('--features','').split(',') if feature} <= set(witness['features']), 'Local binary belongs to another target or graph')
        ends = [index for index in range(first,len(lines)) if lines[index].startswith('NATIVE_EXIT ') or lines[index].startswith('COMMAND ')]
        receipts.require(ends and lines[ends[0]].startswith('NATIVE_EXIT '), 'Local execution exit boundary missing')
        end = ends[0]
        receipts.require(lines[end] == 'NATIVE_EXIT ' + str(record['exit_code']), 'Actual local exit differs')
        body = '\n'.join(lines[first:end])
        # Native argv may select a missing-only subset of the canonical cohort.
        receipts.require(witness['binary_path']==argv[0], 'Native argv is not the preserved binary witness')
        suffix=Path(argv[0]).name.rsplit('-',1)[-1]
        package,stem,fingerprint_name=native_target_paths(target,record['source_commit'])
        fingerprint_path=Path(witness['fingerprint_path'])
        receipts.require(re.fullmatch(r'[0-9a-f]{8,64}',suffix) is not None
                         and Path(argv[0]).name==stem+'-'+suffix
                         and fingerprint_path.name==fingerprint_name
                         and fingerprint_path.parent.name==package+'-'+suffix
                         and fingerprint_path.parent.parent.name=='.fingerprint',
                         'Native binary/fingerprint target linkage differs')
        args = argv[1:]
        receipts.require('--list' not in args and '--exact' in args and '--test-threads=1' in args
                         and len([arg for arg in args if not arg.startswith('--')]) == 1,
                         'Unknown local missing-only harness')
        selected = next(arg for arg in args if not arg.startswith('--'))
        actual = retention.log_outcomes(body, record['coverage']['format'])
        receipts.require(set(actual) == {selected}, 'Actual local selector/outcomes differ')
    else:
        receipts.require(witness['mode'] == 'cargo-command' and kind in RUST_KINDS
                         and set(witness) == ({'mode','first_line','historical_name','job_status'} if origin['kind']=='authenticated-ci' else {'mode','first_line','historical_name'}), 'Unknown Cargo execution witness')
        historical = source_commands(record['source_commit'])
        name = witness['historical_name']
        receipts.require(name in historical and name.split(':')[0] == kind, 'Unknown historical command ordinal')
        expected = rust_identity(historical[name]);current = rust_identity(item['invocation']);same_target(expected,current)
        start = first-1;end_command = start;parts = []
        while True:
            line = lines[end_command];parts.append(line.removesuffix('\\').strip())
            if not line.endswith('\\'):break
            end_command += 1
            receipts.require(end_command < len(lines), 'Incomplete logged command continuation')
        command = ' '.join(parts)
        receipts.require(origin['command'] == command, 'Origin command contradicts exact logged invocation')
        same_target(normalized_cargo(command),expected)
        actual_env = normalized_cargo(command)['environment']
        receipts.require(actual_env == expected['environment'], 'Historical wrapper environment differs')
        recipes=historical_recipe(record,kind)
        command_matches=[index for index,value in enumerate(recipes) if value==historical[name]]
        receipts.require(len(command_matches)==1,'Historical command ordinal lacks unique recipe authority')
        ordinal=command_matches[0]
        next_command=recipes[ordinal+1] if ordinal+1<len(recipes) else None
        terminal=executor_terminal(record,kind,lines,end_command)
        end=terminal[0] if terminal is not None else len(lines)
        progressed=False
        for index in range(end_command+1,end):
            if re.match(r'^(?:(?:[A-Z_]+=[^ ]+|scripts/require-test-count) +)*(?:cargo|/[^ ]+/cargo) (?:test|run|clippy|check)(?: |$)',lines[index]):
                value,_=logged_command(lines,index)
                receipts.require(next_command is not None and display_tokens(value,kind)==display_tokens(next_command,kind),
                                 'Ambiguous Cargo-looking output is not the next historical recipe command')
                end=index;progressed=True;break
        if origin['kind']=='authenticated-ci' and record['exit_code']==0:
            receipts.require(progressed or (terminal is not None and terminal[1]) or witness.get('job_status')=='success',
                             'Historical success is not proved by actual Make progression or executor completion')
        body = '\n'.join(lines[end_command+1:end])
        flags = expected['cargo']
        target = flags.get('--test') or flags.get('--bin') or flags.get('-p')
        if target is None:
            import tomllib
            manifest = retention.git_bytes(record['source_commit'],flags['--manifest-path'])
            receipts.require(manifest is not None,'Historical manifest unavailable')
            target = tomllib.loads(manifest.decode())['package']['name']
        target = target.replace('-','_')
        running = list(re.finditer(r'^\s*Running ((?:unittests|tests/).*?)\(([^)]+)\)$',body,re.M))
        if implicit_cluster_target(expected):
            tags=set()
            for index,match in enumerate(running):
                label,path=match.groups()
                target_kind=['lib'] if label.startswith('unittests src/lib.rs') else ['bin'] if label.startswith('unittests src/main.rs') else ['test']
                stem=Path(path).name.rsplit('-',1)[0]
                eligible=[target for target in item['inventory']['targets'] if target['name'].replace('-','_')==stem and target['kind']==target_kind]
                receipts.require(len(eligible)==1,'Logged implicit target differs from current footprint')
                tags.add((eligible[0]['name'],tuple(target_kind)))
                segment=body[match.end():running[index+1].start() if index+1<len(running) else len(body)]
                if re.search(r'^running 0 tests$',segment,re.M):
                    receipts.require(re.search(r'^test result: ok\. 0 passed; 0 failed; 0 ignored;',segment,re.M), 'Zero target lacks its actual terminal')
            receipts.require(tags=={(target['name'],tuple(target['kind'])) for target in item['inventory']['targets']}, 'Logged implicit target footprint incomplete')
        else:
            receipts.require(len(running)==1 and re.search(r'(?:^|/)' + re.escape(target) + r'-[0-9a-f]+$',running[0][2]),
                             'Actual Cargo target differs or is ambiguous')
        actual = retention.log_outcomes(body, record['coverage']['format'])
        require_native_terminals(body,actual,record['exit_code'],command)
    receipts.require(actual == record['coverage']['outcomes'], 'Cropped or contradictory complete execution outcomes')
    return body


def completion_obligations(item, records, result, bodies):
    """Closed failure categories; current wrapper predicates are derived from source."""
    identity = rust_identity(item['invocation'])
    environment = identity['environment']
    passed = sum(value == 'pass' for value in result.values())
    expected = environment.get('PLURX_EXPECT_TEST_COUNT')
    if expected is None and '--exact' in identity['harness']:
        expected = '1'
    if expected is not None:
        receipts.require(passed == int(expected), 'Current exact count predicate fails')
    minimum = environment.get('PLURX_EXPECT_TEST_COUNT_AT_LEAST')
    if minimum is not None:
        receipts.require(passed >= int(minimum), 'Current minimum count predicate fails')
    names = environment.get('PLURX_EXPECT_TEST_NAMES','')
    receipts.require(all(result.get(name) == 'pass' for name in names.split(',') if name), 'Current required-name predicate fails')
    failed = [record for record in records if record['exit_code'] != 0]
    receipts.require(len(item['obligations']) == len(failed), 'Original failed execution obligation missing')
    for obligation, record in zip(item['obligations'],failed):
        receipts.require(set(obligation) == {'sequence','category'} and obligation['sequence'] == record['sequence'], 'Unknown completion obligation')
        body = bodies[record['sequence']]
        outcomes = record['coverage']['outcomes']
        if obligation['category'] == 'native-failure':
            receipts.require(record['exit_code'] in {101,2} and 'fail' in outcomes.values()
                             and re.search(r'^test result: FAILED\.',body,re.M), 'Unknown native failure category')
        else:
            receipts.require(obligation['category'] == 'stale-count-wrapper', 'Unexplained failed command')
            diagnostics = re.findall(r'^expected (\d+) matching Rust test\(s\), but (\d+) passed$',body,re.M)
            terminals = re.findall(r'^test result: ok\. (\d+) passed; 0 failed; 0 ignored;',body,re.M)
            old_env = normalized_cargo(record['origin']['command'])['environment']
            old_expected = old_env.get('PLURX_EXPECT_TEST_COUNT')
            receipts.require(record['exit_code'] in {1,2} and old_expected is not None and len(diagnostics) == 1 and terminals
                             and diagnostics[0] == (old_expected,str(sum(map(int,terminals))))
                             and int(diagnostics[0][1]) == sum(value == 'pass' for value in outcomes.values())
                             and set(outcomes.values()) == {'pass'} and old_expected != diagnostics[0][1]
                             and not re.search(r'(?im)^(?!make: \*\*\*|##\[error\]Process completed with exit code [12]\.?$).*?(?:fatal|panicked|error:)',body), 'Ambiguous or unproved stale count failure')
            wrapper = retention.git_bytes(record['source_commit'],'scripts/require-test-count')
            receipts.require(wrapper is not None and retention.digest(wrapper) in COUNT_WRAPPERS, 'Historical count wrapper unavailable')


def historical_rust_environment_contract(source,workflow,kind):
    # Source/action authority only. Historical compiler binaries and host-global
    # configuration were not captured, and are deliberately not retroactively hashed.
    for path,digest in {
        '.github/actions/cargo-cache/action.yml':'b22fa9dcb68f0cb063ae9de4f80a420631d19f61af60b32e1c6b47e2d9aadb43',
        'rust-toolchain.toml':'03cbfb27fcdc8253524b3e4c71c832457a504f7e9b1da5e6220178404b77f4e4',
    }.items():
        raw=retention.git_bytes(source,path)
        receipts.require(raw is not None and retention.digest(raw)==digest,
                         'Historical Rust cache/toolchain contract requires review')
    for root in ('','.cargo/','vendor/hiqlite/.cargo/'):
        paths=('.cargo/config','.cargo/config.toml') if not root else (root+'config',root+'config.toml')
        for path in paths:
            receipts.require(retention.git_bytes(source,path) is None,'Historical Cargo source config requires review')
    job={'store_contract':'cluster_store_legacy','wal':'cluster_wal','topology':'cluster_topology',
         'transport':'cluster_transport_recovery_contracts'}[kind]
    text='\n'.join(job_steps(workflow,job))
    receipts.require(text.count('uses: https://github.com/dtolnay/rust-toolchain@4716b85f2fac3e324e64fa2810f6b5c3905760a5')==1
                     and not re.search(r'RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_TARGET|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_PROFILE_',text),
                     'Historical Rust setup or semantic overrides differ')


def authenticate(api, proof, document, repository):
    """Local records use the authenticated writer; CI records also use live logs."""
    origins, runs, jobs_by_run, scopes = {}, {}, {}, set()
    for item in proof['commands']:
        kind = item['name'].split(':')[0]
        for record in item['records']:
            origin = record['origin']
            if origin['kind'] != 'authenticated-ci':
                continue
            match = re.fullmatch(r'run:(\d+)/job:(\d+)', origin['identity'])
            receipts.require(match is not None, 'Malformed CI command origin')
            run_id, job_id = map(receipts.positive, match.groups())
            if run_id not in runs:
                runs[run_id]=api.get(f'/actions/runs/{run_id}')
                jobs_by_run[run_id]=api.pages(f'/actions/runs/{run_id}/jobs')
            run=runs[run_id]
            jobs=[job for job in jobs_by_run[run_id] if job['id']==job_id]
            receipts.require(run['repository']['id'] == repository and run['commit_sha'] == record['source_commit']
                             and run['workflow_id'] == 'ci.yml' and len(jobs) == 1
                             and jobs[0]['name'] == JOB_NAMES[kind] and jobs[0]['attempt'] == 1
                             and jobs[0]['run_id'] == run_id and jobs[0]['repo_id'] == repository
                             and jobs[0]['status'] in {'success','failure'}, 'Historical command job identity mismatch')
            execution=origin['execution']
            receipts.require(execution.get('mode')=='cargo-command' and execution.get('job_status')==jobs[0]['status'],
                             'Declared historical job disposition differs from live authority')
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
            key=(run_id,record['source_commit'])
            if key not in scopes:
                authenticate_scope(api,run_id,run,workflow.decode(),document,record['source_commit'],repository)
                scopes.add(key)
            if job_id not in origins:
                origins[job_id] = api.bytes(f'/actions/jobs/{job_id}/logs')
            raw = origins[job_id]
            if kind in RUST_KINDS:
                historical_rust_environment_contract(record['source_commit'],workflow.decode(),kind)
                text=clean_log(raw.decode())
                receipts.require(re.search(r'^'+re.escape(RUSTC_VERSION)+r'$',text,re.M), 'Actual historical pinned compiler version unavailable')
                job_header=workflow.decode().split('  '+{'store_contract':'cluster_store_legacy','wal':'cluster_wal','topology':'cluster_topology','transport':'cluster_transport_recovery_contracts'}[kind]+':',1)[1].split('    steps:',1)[0]
                receipts.require(re.search(r'^    runs-on: \[self-hosted, Linux, X64,',job_header,re.M), 'Historical compiler platform authority differs')
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
    seen, by_kind, native_inventories = set(), {}, {}
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
        if kind in RUST_KINDS:
            identity = rust_identity(item['invocation'])
            rust_environment(environment,identity,candidate)
            receipts.require(environment['toolchain'] == 'rust1.97.1'
                             and environment['runtime'].get('test_threads') == (1 if '--test-threads=1' in identity['harness'] else 'libtest-default')
                             and environment['runtime'].get('rust_min_stack') == (int(identity['environment']['RUST_MIN_STACK']) if 'RUST_MIN_STACK' in identity['environment'] else None),
                             'Rust command runtime differs from exact canonical invocation')
            ids = native_inventory(item['inventory'], identity, candidate)
            native_inventories[name] = {'ids_sha256': retention.digest(json.dumps(ids, separators=(',', ':')).encode()), 'features': environment['features']}
            if implicit_cluster_target(identity):
                native_inventories[name]['targets']=[{'name':target['name'],'kind':target['kind'],'features':target['features']} for target in item['inventory']['targets']]
            requested = identity['cargo'].get('--features', '').split(',')
            requested = [feature for feature in requested if feature]
            graphs=[target['features'] for target in item['inventory']['targets']] if implicit_cluster_target(identity) else [item['inventory']['features']]
            receipts.require(all(set(requested)<=set(graph) and environment['features']==graph for graph in graphs),
                             'Rust feature graph mismatch')
        elif kind in {'validation', 'operations'}:
            ids = list(discovery(candidate, kind))
            receipts.require(item['inventory'] == {'ids': ids} and ids, 'Current Python discovery differs')
        else:
            receipts.require(item['inventory'] == {'mode': 'whole-command'}, 'JVM names must remain opaque')
            ids = ['whole-command']
        receipts.require(len(ids) == len(set(ids)), 'Repeated current unit identity')
        records = item['records'];receipts.require(isinstance(records, list) and records, 'No actual command executions')
        result, last_failure, last_pass, sequences, bodies = {}, {}, {}, set(), {}
        for record in sorted(records, key=lambda row: row['sequence']):
            receipts.require(set(record) == {'sequence', 'source_commit', 'environment', 'origin', 'changes', 'log', 'coverage', 'exit_code'},
                             'Unknown command execution record')
            sequence = record['sequence']
            receipts.require(type(sequence) is int and sequence > 0 and sequence not in sequences,
                             'Ambiguous actual execution chronology');sequences.add(sequence)
            receipts.require(record['environment'] == environment, 'Actual runtime environment differs')
            if kind in RUST_KINDS:
                rust_environment(record['environment'],identity,record['source_commit'])
            origin = record['origin'];receipts.require(set(origin) == {'kind', 'identity', 'command', 'execution'}
                and origin['kind'] in {'attested-local', 'authenticated-ci'} and origin['identity'] and origin['command'],
                'Missing real execution origin')
            invalid = changed_inputs(record, candidate)
            receipts.require(invalid <= set(ids), 'Unknown affected command unit')
            raw = clean_log(command_log(proof, record))
            if kind in RUST_KINDS:
                bodies[sequence] = bound_execution(record,item,raw)
            actual = coverage(record, raw, kind, discovery)
            receipts.require(set(actual) <= set(ids), 'Execution belongs to another command inventory')
            if kind == 'android_jvm':
                receipts.require(not invalid and record['exit_code'] == 0,
                                 'Opaque JVM result cannot qualify changed unit inputs or failed command')
            for test_id, outcome in actual.items():
                if outcome == 'ignored':
                    receipts.require(kind == 'store_contract' and test_id in IGNORED_STORE_HELPERS,
                                     'Ignored result is outside the closed canonical helper policy')
                if outcome == 'fail':
                    last_failure[test_id] = sequence
                if test_id not in invalid:
                    result[test_id] = outcome
                    if outcome == 'pass':
                        last_pass[test_id] = sequence
        receipts.require(set(result) == set(ids) and all(value in {'pass', 'ignored'} for value in result.values())
                         and all(last_pass.get(test_id, 0) > sequence for test_id, sequence in last_failure.items()), 'Incomplete or failed applicable command coverage')
        if kind in RUST_KINDS:
            completion_obligations(item,records,result,bodies)
        else:
            receipts.require(item['obligations'] == [] and all(row['exit_code'] == 0 for row in records),
                             'Non-Rust failed commands cannot qualify by assertion')
        by_kind.setdefault(kind, []).append(name)
    for kind, names in by_kind.items():
        receipts.require(set(names) == {name for name in canonical_commands if name.split(':')[0] == kind},
                         'Incomplete canonical command cohort')
    return {kind: {'mode': 'retained-complete-command-union', 'commands': sorted(names),
                   'native_inventories': {name: native_inventories[name] for name in names if name in native_inventories},
                   'validator': retention.digest(Path(__file__).read_bytes())} for kind, names in by_kind.items()}


def compile_commands(kind, candidate):
    """Compile distinct package/target/feature combinations; execute no tests."""
    actions = {}
    for name, invocation in canonical(candidate).items():
        if name.split(':')[0] != kind:
            continue
        identity = rust_identity(invocation)
        flags = identity['cargo']
        arguments = ['cargo', 'test', '--no-run', '--message-format=json']
        for flag, value in flags.items():
            arguments.append(flag)
            if value is not True:
                arguments.append(value)
        actions.setdefault(tuple(arguments), []).append((name, identity))
    receipts.require(actions, 'No canonical Rust cohort to compile')
    return actions


def owned_output(command, *, timeout, limit):
    """Queue cancellation through birth and retirement; bound all live output."""
    pending=[]
    def cancelled(signum, _frame):
        # A handler never raises across Popen registration or child retirement.
        pending.append(signum)
    def check_cancel():
        receipts.require(not pending, f'Owned command cancelled by signal {pending[0] if pending else "none"}')
    previous={}
    process=None
    successful=False
    try:
        for signum in (signal.SIGTERM,signal.SIGINT):
            previous[signum]=signal.signal(signum,cancelled)
        process=subprocess.Popen(command,stdout=subprocess.PIPE,start_new_session=True)
        check_cancel()
        deadline=time.monotonic()+timeout
        output=bytearray()
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout,selectors.EVENT_READ)
            while True:
                check_cancel()
                remaining=deadline-time.monotonic()
                receipts.require(remaining>0,'Owned command timeout')
                if not selector.select(min(remaining,1)):
                    continue
                chunk=os.read(process.stdout.fileno(),65536)
                if not chunk:
                    break
                receipts.require(len(output)+len(chunk)<=limit,'Owned command output limit exceeded')
                output.extend(chunk)
        while process.poll() is None:
            check_cancel()
            receipts.require(time.monotonic()<deadline,'Owned command timeout')
            time.sleep(0.05)
        check_cancel()
        receipts.require(process.returncode==0,'Owned command failed')
        decoded=output.decode('utf-8',errors='strict')
        successful=True
        return decoded
    finally:
        try:
            if process is not None:
                try:
                    os.killpg(process.pid,signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=10)
                finally:
                    if process.stdout is not None:
                        process.stdout.close()
        finally:
            for signum,handler in previous.items():
                signal.signal(signum,handler)
        if successful:
            check_cancel()


def owned_compile(command):
    return owned_output(command, timeout=1800, limit=64 * 1024 * 1024)


def require_store_mode(adopted, mode):
    receipts.require('store_contract' not in adopted or mode == 'legacy',
                     'Retained Store units require legacy mode; shards would replay them')


def require_unmodified_cargo_environment(environment):
    semantic={'RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CARGO_BUILD_TARGET','RUSTC',
              'RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','CARGO_BUILD_RUSTC',
              'CARGO_BUILD_RUSTC_WRAPPER','CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'}
    values=dict(environment)
    if re.fullmatch(r'/(?:home|opt)/[^\s]+/\.rustup/toolchains/1\.97\.1-x86_64-unknown-linux-gnu/bin/rustc',values.get('RUSTC','')):
        values.pop('RUSTC')
    overrides=[key for key,value in values.items() if value and
               (key in semantic or key.startswith('CARGO_PROFILE_') or
                (key.startswith('CARGO_TARGET_') and key.endswith('_RUSTFLAGS')))]
    receipts.require(not overrides,'Unbound semantic Cargo overrides: '+','.join(sorted(overrides)))


def require_current_fingerprint(artifact,profile):
    binary=Path(artifact['executable'])
    suffix=binary.name.rsplit('-',1)[-1]
    receipts.require(binary.parent.name=='deps' and re.fullmatch(r'[0-9a-f]{8,64}',suffix) is not None,
                     'Unknown current artifact layout')
    root=binary.parent.parent/'.fingerprint'
    matches=list(root.glob('*-'+suffix+'/test-*.json'))
    receipts.require(len(matches)==1,'Current test artifact fingerprint unavailable or ambiguous')
    fingerprint=receipts.bounded_json(matches[0].read_bytes())
    receipts.require(fingerprint.get('rustc')==RUSTC_FINGERPRINT
                     and fingerprint.get('profile')==TEST_PROFILE_FINGERPRINTS[profile['debuginfo']]
                     and fingerprint.get('compile_kind')==0 and fingerprint.get('rustflags')==[]
                     and json.loads(fingerprint.get('features','null'))==sorted(artifact['features']),
                     'Actual current Cargo compiler/target/flags/profile differs')


def compile_adopted(kind, proof, candidate):
    """The current compiler independently verifies each adopted native inventory."""
    receipts.require(proof['mode'] == 'retained-complete-command-union'
                     and proof['validator'] == retention.digest(Path(__file__).read_bytes()),
                     'Command adoption validator changed after scope')
    require_unmodified_cargo_environment(os.environ)
    version=owned_output(['rustc','-vV'],timeout=10,limit=8192)
    receipts.require(version.splitlines()[0]==RUSTC_VERSION and 'host: x86_64-unknown-linux-gnu' in version.splitlines(),
                     'Current command compiler release/host mismatch')
    expected = proof['native_inventories']
    actions = compile_commands(kind, candidate)
    receipts.require(set(expected) == {name for group in actions.values() for name, _ in group},
                     'Adopted command/compiler footprint incomplete')
    for command, group in actions.items():
        print('COMPILE_ONLY ' + shlex.join(command), flush=True)
        output = owned_compile(command)
        artifacts = []
        for line in output.splitlines():
            try:
                row = json.loads(line)
            except (ValueError, TypeError) as error:
                raise receipts.ReceiptError('Malformed current Cargo JSON evidence') from error
            receipts.require(isinstance(row, dict), 'Malformed current Cargo artifact')
            if row.get('reason') == 'compiler-artifact' and row.get('profile', {}).get('test') and row.get('executable'):
                artifacts.append(row)
        for name, identity in group:
            flags = identity['cargo']
            profile=test_profile(candidate,flags.get('--manifest-path'))
            if implicit_cluster_target(identity):
                footprint=expected[name]['targets']
                matching=[row for row in artifacts if row['package_id'].split('#')[-1].startswith('plurx-cluster-check@') or '/plurx-cluster-check#' in row['package_id']]
                tags=[(row['target']['name'],tuple(row['target']['kind'])) for row in matching]
                receipts.require(len(tags)==len(set(tags)) and set(tags)=={(target['name'],tuple(target['kind'])) for target in footprint},'Compiled implicit target footprint differs')
                all_ids=[]
                for target in footprint:
                    rows=[row for row in matching if row['target']['name']==target['name'] and row['target']['kind']==target['kind']]
                    receipts.require(len(rows)==1 and sorted(rows[0]['features'])==sorted(target['features']) and rows[0]['profile']==profile,'Implicit target feature graph differs')
                    require_current_fingerprint(rows[0],profile)
                    output=owned_output([rows[0]['executable'],'--list','--format=terse'],timeout=30,limit=8*1024*1024)
                    full=[line.removesuffix(': test') for line in output.splitlines() if line.endswith(': test')]
                    all_ids.extend(select_ids(full,identity))
                receipts.require(retention.digest(json.dumps(all_ids,separators=(',',':')).encode())==expected[name]['ids_sha256'],'Compiled implicit selected universe differs')
                print('ADOPTED_COMPLETE_UNIT_COMMAND '+name,flush=True)
                continue
            target_name = flags.get('--test') or flags.get('--bin')
            if target_name is None:
                package = flags.get('-p')
                if package is None:
                    import tomllib
                    package = tomllib.loads(Path(flags['--manifest-path']).read_text())['package']['name']
                target_name = package.replace('-', '_')
            matching = [row for row in artifacts if row['target']['name'] == target_name]
            receipts.require(len(matching) == 1, 'Current target artifact missing or ambiguous')
            artifact = matching[0]
            receipts.require(sorted(artifact['features']) == sorted(expected[name]['features']) and artifact['profile']==profile,
                             'Current compiled feature graph differs from adopted target')
            require_current_fingerprint(artifact,profile)
            output = owned_output([artifact['executable'], '--list', '--format=terse'], timeout=30, limit=8 * 1024 * 1024)
            ids = [line.removesuffix(': test') for line in output.splitlines() if line.endswith(': test')]
            selector = identity['filter']
            selected = [test_id for test_id in ids if selector is None or
                        (test_id == selector if '--exact' in identity['harness'] else selector in test_id)]
            actual = retention.digest(json.dumps(selected, separators=(',', ':')).encode())
            receipts.require(actual == expected[name]['ids_sha256'],
                             'Current compiler discovered another unit universe')
            print('ADOPTED_COMPLETE_UNIT_COMMAND ' + name, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compile-units', choices=sorted(RUST_KINDS), required=True)
    args = parser.parse_args()
    candidate = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    receipts.require(candidate == os.environ['GITHUB_SHA'], 'Checked-out adoption source differs')
    verified = receipts.bounded_json(os.environ['PLURX_RETAINED_UNITS'].encode())
    receipts.require(verified['candidate_sha'] == candidate, 'Scope adoption bound to another source')
    compile_adopted(args.compile_units, verified['command_units'][args.compile_units], candidate)


if __name__ == '__main__':
    main()
