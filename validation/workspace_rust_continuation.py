"""Authenticate a partial Cargo run, leaving the current Rust CI job required."""
from __future__ import annotations

import ast
import os
from pathlib import Path
import re
import subprocess

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import qualification_unit_continuation as commands
from validation import workspace_rust_outcomes as native

# Reviewed native job includes the unmodified checkout, toolchain, image, root
# ownership prerequisite, Cargo flags and runtime limits. A changed historical
# job needs explicit review before any earlier pass is usable.
ORIGINAL_JOB = '1a68af55093f2b325f2187fe79082a03f1c6bf8b13ea75545e99f3155783f304'
# Reviewed four-line HDR inverse check uses the already pinned FFmpeg 8
# action. Checkout/toolchain/image/Cargo commands/flags/limits are unchanged;
# ORIGINAL_JOB and protected-input refusal remain exact and unchanged.
CURRENT_JOB = '8672bfc684e8900df352a108a13fe12946ca6bde0f451b8895482379f3072da8'
ROOT_SOURCES = {'tests/operations/test_source_custody_recovery.py', 'scripts/source-custody-recovery'}
PROTECTED = {'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'Makefile'}
NATIVE_ACTIONS = ('.cargo/', '.github/actions/rust-native-deps/', '.github/actions/ffmpeg/',
                  '.github/actions/cargo-cache/', '.github/actions/cargo-cache-finalize/')
POLICY = 'workspace-rust-continuation-v1'


def check_changes(record, candidate, targets):
    prior = receipts.sha(record['prior'])
    changes = record['changes']
    receipts.require(isinstance(changes, dict) and set(changes) == retention.changed(prior, candidate),
                     'Rust applicability must classify every changed path')
    known = {target + '::' + name for target, row in targets.items() for name in row['outcomes']}
    known.update(target+'::'+row['interrupted_test'] for target, row in targets.items() if row.get('interrupted_test'))
    invalidated = set()
    for path, witness in changes.items():
        receipts.require(path not in PROTECTED and not path.startswith(NATIVE_ACTIONS)
                         and not path.endswith('/Cargo.toml'),
                         'Historical Rust compile or execution environment changed')
        receipts.require(isinstance(witness, dict) and set(witness) == {'before', 'after', 'affected', 'reason'}
                         and isinstance(witness['reason'], str) and 0 < len(witness['reason'].strip()) <= 2048,
                         'Rust applicability reason is missing')
        old, new = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        receipts.require(witness['before'] == (retention.digest(old) if old is not None else None)
                         and witness['after'] == (retention.digest(new) if new is not None else None),
                         'Rust applicability path bytes contradict source')
        ids = witness['affected']
        receipts.require(isinstance(ids, list) and len(ids) == len(set(ids)) and set(ids) <= known,
                         'Rust applicability references an unknown historical body')
        invalidated.update(ids)
    return invalidated


def check_workflow_environment(workflow, expected):
    job = re.search(r'(?s)\n  check:\n.*?(?=\n  cluster_store_legacy:)', workflow)
    global_env = re.findall(r'(?m)^env:\n((?:  [^\n]+\n)+)',workflow)
    roots = re.findall(r'(?m)^[^\s#][^\n]*$', workflow)
    accepted = expected if isinstance(expected, tuple) else (expected,)
    receipts.require(job and retention.digest(job[0].encode()) in accepted
                     and roots == ['name: ci', 'on:', 'env:', 'concurrency:', 'jobs:']
                     and global_env == ['  CARGO_TERM_COLOR: always\n']
                     and not re.search(r'(?m)^defaults:',workflow),
                     'Unrecognized workflow or native Rust job execution environment')


def require_native_inputs(inputs):
    receipts.require(all(not inputs.get(key) for key in (
        'retained_units_sha256', 'retained_units_attachment_id',
        'retained_units_attachment_uuid', 'workspace_rust_evidence')),
        'Historical Rust job may not adopt earlier unit outcomes')


def authenticate(api, descriptor, binding, candidate):
    receipts.require(set(descriptor) == {'sha256', 'attachment_id', 'attachment_uuid'},
                     'Unknown workspace continuation descriptor')
    attachment = receipts.positive(descriptor['attachment_id'])
    uuid, digest = descriptor['attachment_uuid'], descriptor['sha256']
    raw = retention.attachment_bytes(api, binding['pull_request'], attachment, uuid, digest)
    record, _, repository, _ = retention.verify_identity(api, raw, digest, binding, candidate, attachment, uuid)
    receipts.require(set(record) == {'version','repository','pr','branch','base_sha','source_commit',
                                    'prior','run','job','log_sha256','changes'}
                     and record['version'] == POLICY and record['source_commit'] == candidate,
                     'Rust continuation must bind this exact promotion')
    prior = receipts.sha(record['prior'])
    run_id, job_id = receipts.positive(record['run']), receipts.positive(record['job'])
    run = api.get(f'/actions/runs/{run_id}')
    payload = receipts.bounded_json(run['event_payload'].encode())
    inputs = payload['inputs']
    receipts.require(run['repository']['id'] == repository and run['workflow_id'] == 'ci.yml'
                     and run['trigger_event'] == 'workflow_dispatch' and run['commit_sha'] == prior
                     and run['prettyref'] == record['branch'] and inputs['promotion_head_sha'] == prior
                     and str(record['pr']) == str(inputs['promotion_pr']),
                     'Historical Rust promotion identity differs')
    require_native_inputs(inputs)
    jobs = [j for j in api.pages(f'/actions/runs/{run_id}/jobs')
            if j['name'] == 'Rust unit and SQLite contracts']
    receipts.require(len(jobs) == 1 and jobs[0]['id'] == job_id and jobs[0]['run_id'] == run_id
                     and jobs[0]['repo_id'] == repository and jobs[0]['attempt'] == 1
                     and jobs[0]['status'] in ('success','failure'), 'Original native Rust job differs')
    workflow = retention.git_bytes(prior, '.github/workflows/ci.yml').decode()
    check_workflow_environment(workflow,(ORIGINAL_JOB,CURRENT_JOB))
    check_workflow_environment(retention.git_bytes(candidate,'.github/workflows/ci.yml').decode(),CURRENT_JOB)
    commands.authenticate_scope(api, run_id, run, workflow, record, prior, repository)
    source = api.bytes(f'/actions/jobs/{job_id}/logs')
    receipts.require(retention.digest(source) == record['log_sha256'], 'Original native Rust log changed')
    lines = native.clean_lines(source)
    for value in ('commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452',
                  'host: x86_64-unknown-linux-gnu', 'release: 1.97.1'):
        receipts.require(value in lines, 'Historical Rust compiler identity is missing')
    targets = native.workspace_targets(source)
    invalidated = check_changes(record, candidate, targets)
    retained = {target: [name for name, status in row['outcomes'].items()
                         if status == 'pass' and target + '::' + name not in invalidated]
                for target, row in targets.items()}
    root_ok = not (ROOT_SOURCES & set(record['changes'])) and root_prerequisite(source, retention.git_bytes(prior,
        'tests/operations/test_source_custody_recovery.py'))
    observed = {target: sorted(row['outcomes']) for target, row in targets.items()}
    must_pass = {target: [name for name, status in row['outcomes'].items()
                          if status == 'fail' or (status == 'pass' and name not in retained[target])]
                 for target, row in targets.items()}
    for target, row in targets.items():
        if row.get('interrupted_test'):
            observed[target].append(row['interrupted_test'])
            must_pass[target].append(row['interrupted_test'])
    return {'candidate':candidate,'prior':prior,'digest':digest,'retained':retained,
            'observed':observed,'must_pass':must_pass,'root_ownership_passed':root_ok}


def root_prerequisite(raw, test_source):
    classes = [n for n in ast.parse(test_source).body if isinstance(n, ast.ClassDef)]
    if len(classes) != 1 or classes[0].name != 'RecoveryHelperTests':
        return False
    expected = {n.name for n in classes[0].body if isinstance(n,ast.FunctionDef) and n.name.startswith('test_')}
    lines = native.clean_lines(raw)
    section = lines[:lines.index('cargo fmt --all --check')]
    rows = re.compile(r'(test_\w+) \(test_source_custody_recovery\.RecoveryHelperTests\.(test_\w+)\) \.\.\. ok')
    matches = [m.groups() for line in section if (m := rows.fullmatch(line))]
    return bool(matches and all(a == b for a, b in matches) and len(matches) == len(expected)
                and {a for a, _ in matches} == expected
                and any(re.fullmatch(r'Ran ' + str(len(matches)) + r' tests in [0-9.]+s', line) for line in section)
                and section.count('OK') == 1)


def main():
    candidate = receipts.sha(os.environ['GITHUB_SHA'])
    receipts.require(subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip() == candidate,
                     'Rust continuation checkout differs')
    proof = authenticate(retention.runner_api(os.environ),
                         receipts.bounded_json(os.environ['WORKSPACE_RUST_EVIDENCE'].encode()),
                         receipts.bounded_json(os.environ['PROMOTION_BINDING'].encode()), candidate)
    destination = Path('target/validation/workspace-rust-selection.json')
    receipts.atomic_json(destination,proof)
    print('Verified prior Rust passes for', sum(map(len, proof['retained'].values())), 'named target bodies')
    with open(os.environ['GITHUB_OUTPUT'],'a') as output:
        print('root_retained=' + str(proof['root_ownership_passed']).lower(),file=output)


if __name__ == '__main__':
    main()
