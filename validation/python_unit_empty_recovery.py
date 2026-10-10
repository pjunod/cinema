"""Authenticate PR920's two empty failed attempts; never import outcomes."""

import hashlib
from pathlib import Path
import re

SCOPE = {'repository': 1, 'pr': 920, 'branch': 'codex/dv-m0-harness',
         'base': 'effort/dv-hdr-processing'}
PROOF_SHA256 = 'f1ccca974b71a5258f49fd4868420a3926279ec3ba5ebb2431538009247ae93b'
JOB_FIELDS = ('id', 'run_id', 'repo_id', 'attempt', 'task_id', 'name', 'status')
SOURCE_PATHS = {'.github/workflows/effort-ci.yml', 'validation/python_unit_receipts.py'}


def empty_failure_case():
    from validation.python_unit_receipts import MAX_BYTES, bounded_json, require
    path = Path(__file__).resolve().parent / 'python-unit-empty-failure920.json'
    require(path.is_file() and not path.is_symlink(), 'Empty failure witness unavailable')
    with path.open('rb') as stream:
        raw = stream.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES and hashlib.sha256(raw).hexdigest() == PROOF_SHA256,
            'Unknown or corrupt empty failure witness')
    proof = bounded_json(raw)
    require(proof['version'] == 1 and proof['scope'] == SCOPE
            and [(case['run'], case['job'], case['task']) for case in proof['cases']]
            == [(4501, 45570, 17017), (4502, 45578, 17026)],
            'Empty failure witness identity mismatch')
    return proof


def recover_empty_pr920(api, scope, prior, jobs):
    """Only the reviewed two identities can establish zero unit execution."""
    from validation.python_unit_receipts import artifact_json, bounded_json, key, require, validate_journal
    if scope != SCOPE or prior['id'] not in (4501, 4502):
        return False
    proof = empty_failure_case()
    case = next(item for item in proof['cases'] if item['run'] == prior['id'])
    run = case['run']
    require(prior['commit_sha'] == case['commit'], 'Empty failure prior source mismatch')
    actual = api.get(f'/actions/runs/{run}')
    require(actual['id'] == run and actual['repository']['id'] == scope['repository']
            and actual['commit_sha'] == case['commit'] and actual['prettyref'] == scope['branch']
            and actual['workflow_id'] == 'effort-ci.yml' and actual['status'] == 'cancelled'
            and actual['trigger_event'] == 'workflow_dispatch',
            'Empty failure terminal run mismatch')
    event = bounded_json(actual['event_payload'].encode())
    require(event['repository']['id'] == scope['repository']
            and event['ref'] == 'refs/heads/' + scope['branch']
            and event['workflow'] == '.github/workflows/effort-ci.yml',
            'Empty failure original dispatch mismatch')
    inventory = [{field: job.get(field) for field in JOB_FIELDS} for job in jobs]
    require(sorted(inventory, key=lambda job: job['id']) == case['jobs'],
            'Empty failure terminal job inventory mismatch')
    require(set(case['source_hashes']) == SOURCE_PATHS, 'Empty failure source proof incomplete')
    for path, expected in case['source_hashes'].items():
        raw = api.bytes('/raw/' + path, {'ref': case['commit']})
        require(hashlib.sha256(raw).hexdigest() == expected, 'Empty failure original source mismatch')
    raw = api.bytes(f"/actions/jobs/{case['job']}/logs")
    require(len(raw) == case['log_bytes'] and hashlib.sha256(raw).hexdigest() == case['log_sha256'],
            'Empty failure original log mismatch')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line)
             for line in raw.decode('utf-8').splitlines()]
    log = '\n'.join(lines)
    require(f"received task {case['task']} of job preflight, triggered by event: workflow_dispatch" in log
            and lines.count(case['commit']) == 2
            and lines.count("Job 'Python unit receipts' failed") == 1
            and not any(marker in log for marker in (
                'discovered=', 'historical-passes=', 'pending=', 'Ran ', 'Unit discovery failed',
                'fixture_errors', '... ok', '... FAIL', '... ERROR')),
            'Empty failure log contradicts zero unit execution')
    artifacts = api.get(f'/actions/runs/{run}/artifacts')
    starts = api.pages('/actions/artifacts', {'name': key(scope) + f'-start-{run}'})
    finals = [item for item in api.pages('/actions/artifacts', {'name': key(scope)})
              if item['run_id'] == run]
    if run == 4502:
        refusal = ('Python receipt refusal: ReceiptError: Incomplete receipt attempt 4501; '
                   'preserve artifact and recover individual evidence')
        publications = ["skipping post step for '" + name + "'; main step was skipped" for name in (
            'Publish Python attempt-start marker', 'Preserve Python success journal even on unit failure')]
        require(lines.count(refusal) == 1 and all(lines.count(line) == 1 for line in publications)
                and all(lines.index(refusal) < lines.index(line) for line in publications)
                and not any('has been successfully uploaded!' in line for line in lines),
                'Empty prepare refusal ordering mismatch')
        require(artifacts == starts == finals == [], 'Empty prepare refusal unexpectedly has artifacts')
    else:
        require(len(artifacts) == 2 and len(starts) == len(finals) == 1
                and {item['id'] for item in artifacts} == {1942, 1943},
                'Empty started attempt artifact inventory mismatch')
        journals, uploads = {}, {}
        for label, items in (('start', starts), ('final', finals)):
            item, expected = items[0], case['artifacts'][label]
            name = key(scope) + (f'-start-{run}' if label == 'start' else '')
            require(item['id'] == expected['id'] and item['run_id'] == run and item['name'] == name
                    and item['expired'] is False and item['size_in_bytes'] == expected['size']
                    and sum(entry == item for entry in artifacts) == 1,
                    'Empty attempt artifact metadata mismatch')
            raw = api.bytes(f"/actions/artifacts/{item['id']}/zip")
            require(len(raw) == expected['size'] and hashlib.sha256(raw).hexdigest() == expected['sha256'],
                    'Empty attempt artifact archive mismatch')
            journals[label] = artifact_json(raw)
            validate_journal(journals[label], scope, run, case['commit'], completed=False)
            upload = (f'Artifact {name} has been successfully uploaded! Final size is '
                      f"{expected['size']} bytes. Artifact ID is {expected['id']}")
            require(lines.count(upload) == 1, 'Empty attempt artifact publication mismatch')
            uploads[label] = lines.index(upload)
        expected_journal = {'version': 1, 'scope': scope, 'run': run, 'commit': case['commit'],
                            'applicability_commit': case['commit'], 'complete': False,
                            'passes': {}, 'fixture_errors': []}
        require(journals['start'] == journals['final'] == expected_journal,
                'Empty attempt journals contain outcomes or changed initial state')
        refusal = ('Python receipt refusal: ReceiptError: Dispatch must match exactly one open '
                   'same-repository PR into effort (branch/head/base)')
        require(lines.count(refusal) == 1
                and uploads['start'] < lines.index(refusal) < uploads['final'],
                'Empty started attempt identity refusal ordering mismatch')
    print(f'Accounted run{run}/job{case["job"]}/attempt1: zero Python units, no successes imported')
    return True
