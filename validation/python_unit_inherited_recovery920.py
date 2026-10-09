"""PR920's exact history refusal retains only run4503's inherited successes."""

import hashlib
import json
from pathlib import Path
import re

SCOPE = {'repository': 1, 'pr': 920, 'branch': 'codex/dv-m0-harness',
         'base': 'effort/dv-hdr-processing'}
PROOF_SHA256 = 'b69a0feeac2f113d22f14295ec283935dd00eee4bbd4a9834dc18d6a875bf0f6'
JOB_FIELDS = ('id', 'run_id', 'repo_id', 'attempt', 'task_id', 'name', 'status')
SOURCE_PATHS = {'.github/workflows/effort-ci.yml', 'validation/python_unit_receipts.py'}


def inherited_failure_case():
    from validation.python_unit_receipts import MAX_BYTES, bounded_json, require
    path = Path(__file__).resolve().parent / 'python-unit-inherited-failure920.json'
    require(path.is_file() and not path.is_symlink(), 'PR920 inherited failure witness unavailable')
    with path.open('rb') as stream:
        raw = stream.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES and hashlib.sha256(raw).hexdigest() == PROOF_SHA256,
            'Unknown or corrupt PR920 inherited failure witness')
    proof = bounded_json(raw)
    require(proof['version'] == 1 and proof['scope'] == SCOPE
            and (proof['run'], proof['job'], proof['task']) == (4506, 45612, 17045)
            and proof['commit'] == '8148ffea9fcded4fef5f605a36fc8ae63e8f25b1'
            and proof['count'] == 1191
            and proof['source'] == {'run': 4503, 'commit': 'c761f2d1478a91c407964583e1b89ea0110457ab'},
            'PR920 inherited failure exact identity mismatch')
    return proof


def recover_inherited_pr920(api, scope, prior, jobs, marker, marker_raw, artifact, final_raw, journal):
    """Return no new origin: ordinary restore must still authenticate run4503."""
    from validation.python_unit_receipts import artifact_json, bounded_json, key, require, validate_journal
    if scope != SCOPE or prior['id'] != 4506:
        return False
    proof = inherited_failure_case()
    run = proof['run']
    require(prior['commit_sha'] == proof['commit'], 'PR920 inherited failure prior source mismatch')
    actual = api.get(f'/actions/runs/{run}')
    require(actual['id'] == run and actual['repository']['id'] == scope['repository']
            and actual['commit_sha'] == proof['commit'] and actual['prettyref'] == scope['branch']
            and actual['workflow_id'] == 'effort-ci.yml' and actual['status'] == 'failure'
            and actual['trigger_event'] == 'workflow_dispatch',
            'PR920 inherited failure terminal run mismatch')
    event = bounded_json(actual['event_payload'].encode())
    require(event['repository']['id'] == scope['repository']
            and event['ref'] == 'refs/heads/' + scope['branch']
            and event['workflow'] == '.github/workflows/effort-ci.yml',
            'PR920 inherited failure original dispatch mismatch')
    inventory = [{field: job.get(field) for field in JOB_FIELDS} for job in jobs]
    require(sorted(inventory, key=lambda job: job['id']) == proof['jobs'],
            'PR920 inherited failure terminal job inventory mismatch')
    require(set(proof['source_hashes']) == SOURCE_PATHS, 'PR920 inherited source proof incomplete')
    for path, expected in proof['source_hashes'].items():
        raw = api.bytes('/raw/' + path, {'ref': proof['commit']})
        require(hashlib.sha256(raw).hexdigest() == expected, 'PR920 inherited original source mismatch')
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(len(raw) == proof['log_bytes'] and hashlib.sha256(raw).hexdigest() == proof['log_sha256'],
            'PR920 inherited original log mismatch')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line)
             for line in raw.decode('utf-8').splitlines()]
    log = '\n'.join(lines)
    require(f"received task {proof['task']} of job preflight, triggered by event: workflow_dispatch" in log
            and lines.count(proof['commit']) == 2
            and lines.count("Job 'Python unit receipts' failed") == 1
            and not any(token in log for token in (
                'discovered=', 'historical-passes=', 'pending=', 'Ran ', 'Unit discovery failed',
                'fixture_errors', '... ok', '... FAIL', '... ERROR')),
            'PR920 inherited failure contradicts zero-unit phase evidence')
    live = api.get(f'/actions/runs/{run}/artifacts')
    require(len(live) == 2 and {item['id'] for item in live} == {1946, 1947},
            'PR920 inherited artifact inventory mismatch')
    uploads = []
    for label, item, raw, name in (
            ('start', marker, marker_raw, key(scope) + f'-start-{run}'),
            ('final', artifact, final_raw, key(scope))):
        expected = proof['artifacts'][label]
        require(item['id'] == expected['id'] and item['run_id'] == run and item['name'] == name
                and item['expired'] is False and item['size_in_bytes'] == len(raw) == expected['size']
                and hashlib.sha256(raw).hexdigest() == expected['sha256']
                and sum(entry == item for entry in live) == 1,
                'PR920 inherited artifact witness mismatch')
        upload = (f'Artifact {name} has been successfully uploaded! Final size is '
                  f"{expected['size']} bytes. Artifact ID is {expected['id']}")
        require(lines.count(upload) == 1, 'PR920 inherited artifact publication mismatch')
        uploads.append(lines.index(upload))
    start = artifact_json(marker_raw)
    require(journal == artifact_json(final_raw), 'PR920 inherited supplied final journal mismatch')
    validate_journal(start, scope, run, proof['commit'], completed=False)
    validate_journal(journal, scope, run, proof['commit'], completed=False)
    expected_journal = {'version': 1, 'scope': scope, 'run': run, 'commit': proof['commit'],
                        'applicability_commit': proof['commit'], 'complete': False,
                        'fixture_errors': [], 'passes': start['passes']}
    passes_raw = json.dumps(start['passes'], sort_keys=True, separators=(',', ':')).encode()
    require(start == journal == expected_journal and len(start['passes']) == proof['count']
            and all(source == proof['source'] for source in start['passes'].values())
            and hashlib.sha256(passes_raw).hexdigest() == proof['passes_sha256'],
            'PR920 inherited failure changed its original map or attribution')
    history = 'historical regression coverage is incomplete:'
    finding = ('  - corrective client commit 8148ffea needs a tests/client-fixes.toml anchor row: '
               'fix(validation): reconcile effort baseline evidence')
    failure = 'make: *** [Makefile:140: history-check] Error 1'
    require(lines.count(history) == lines.count(finding) == lines.count(failure) == 1
            and uploads[0] < lines.index(history) < lines.index(finding)
            < lines.index(failure) < uploads[1],
            'PR920 inherited failure history-before-units ordering mismatch')
    print('Preserved 1191 original run4503 successes through failed run4506/job45612; zero new units or passes')
    return True
