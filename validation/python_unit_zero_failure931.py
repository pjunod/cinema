"""Authenticate two exact PR931 pre-unit failures without adding outcomes."""
import hashlib
from pathlib import Path
import re

from validation import python_unit_receipts as receipts


def descriptor():
    path = Path(__file__).resolve().with_name('python-unit-zero-failure931.json')
    receipts.require(path.is_file() and not path.is_symlink(), 'PR931 zero-unit proof unavailable')
    with path.open('rb') as stream:
        raw = stream.read(receipts.MAX_BYTES + 1)
    receipts.require(len(raw) <= receipts.MAX_BYTES and hashlib.sha256(raw).hexdigest() ==
                     '30dbdcdecf975434bfeca1bec319d561970c147ae097e20c59056ddef47f09b2',
                     'PR931 zero-unit descriptor mismatch')
    return receipts.bounded_json(raw)


def recover(api, scope, prior, jobs, marker=None, marker_raw=None,
            artifact=None, final_raw=None, journal=None):
    expected_scope = {'repository': 1, 'pr': 931, 'branch': 'codex/candidate-history-repair',
                      'base': 'effort/batch003-repairs'}
    if scope != expected_scope or prior['id'] not in (4522, 4523):
        return False
    proof = descriptor()
    receipts.require(proof['version'] == 1 and proof['scope'] == expected_scope,
                     'PR931 proof scope mismatch')
    case = next(row for row in proof['cases'] if row['run'] == prior['id'])
    actual = api.get(f"/actions/runs/{case['run']}")
    receipts.require(actual['id'] == prior['id'] == case['run']
                     and actual['repository']['id'] == 1
                     and actual['commit_sha'] == prior['commit_sha'] == case['commit']
                     and actual['prettyref'] == scope['branch']
                     and actual['workflow_id'] == 'effort-ci.yml' and actual['status'] == 'failure'
                     and hashlib.sha256(actual['event_payload'].encode()).hexdigest() == case['event_sha256'],
                     'PR931 original run/event/source mismatch')
    receipts.require(all(job['run_id'] == case['run'] and job['repo_id'] == 1
                         and type(job['attempt']) is int and job['attempt'] == 1 for job in jobs)
                     and sorted([[job['id'], job['name'], job['task_id'], job['attempt'], job['status']]
                                 for job in jobs]) == case['jobs'],
                     'PR931 complete job inventory mismatch')
    for path, digest in case['sources'].items():
        receipts.require(hashlib.sha256(api.bytes('/raw/' + path, {'ref': case['commit']})).hexdigest() == digest,
                         'PR931 original producer mismatch')
    raw = api.bytes(f"/actions/jobs/{case['job']}/logs")
    receipts.require(len(raw) == case['log_bytes'] and hashlib.sha256(raw).hexdigest() == case['log_sha256'],
                     'PR931 original complete log mismatch')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line) for line in raw.decode().splitlines()]
    log = '\n'.join(lines)
    receipts.require(raw.endswith(b'\n') and lines[-1] == "Job 'Python unit receipts' failed"
                     and case['commit'] in log
                     and not any(token in log for token in (
                         'discovered=', 'pending=', 'historical-passes=', '... ok', '... FAIL', '... ERROR'))
                     and not any(re.match(r'^(?:Ran \d+ tests?(?: |$)|(?:not )?ok \d+|# tests )', line)
                                 for line in lines), 'PR931 log contradicts zero units')
    if case['run'] == 4523:
        receipts.require(all(value is None for value in (marker, marker_raw, artifact, final_raw, journal)),
                         'PR931 prepare refusal unexpectedly supplied journals')
        refusal = ('Python receipt refusal: ReceiptError: Incomplete receipt attempt 4522; '
                   'preserve artifact and recover individual evidence')
        skipped = ["skipping post step for '" + step + "'; main step was skipped" for step in (
            'Preserve Python success journal even on unit failure', 'Publish Python attempt-start marker')]
        receipts.require(lines.count(refusal) == 1
                         and all(lines.count(line) == 1 and lines.index(refusal) < lines.index(line)
                                 for line in skipped), 'PR931 prepare refusal phase mismatch')
        receipts.recovery_absence(api, scope, 4523)
        return True
    receipts.require(all(value is not None for value in (marker, marker_raw, artifact, final_raw, journal)),
                     'PR931 requires original live start/final journals')
    positions = []
    for label, item, zipped, name in (
            ('start', marker, marker_raw, receipts.key(scope) + '-start-4522'),
            ('final', artifact, final_raw, receipts.key(scope))):
        expected = case['artifacts'][label]
        receipts.require(item['id'] == expected['id'] and item['run_id'] == 4522
                         and item['name'] == name and not item['expired']
                         and item['size_in_bytes'] == len(zipped) == expected['size']
                         and hashlib.sha256(zipped).hexdigest() == expected['sha256'],
                         'PR931 original live artifact mismatch')
        uploaded = (f"Artifact {name} has been successfully uploaded! Final size is "
                    f"{expected['size']} bytes. Artifact ID is {expected['id']}")
        receipts.require(lines.count(uploaded) == 1, 'PR931 original artifact upload mismatch')
        positions.append(lines.index(uploaded))
    start = receipts.artifact_json(marker_raw)
    receipts.validate_journal(start, scope, 4522, case['commit'], completed=False)
    receipts.validate_journal(journal, scope, 4522, case['commit'], completed=False)
    receipts.require(start == journal and start['complete'] is False and start['passes'] == {}
                     and start.get('applicability_commit') == case['commit'],
                     'PR931 empty incomplete original journal changed')
    history = 'historical regression coverage is incomplete:'
    failed = 'make: *** [Makefile:144: history-check] Error 1'
    receipts.require(lines.count(history) == lines.count(failed) == 1
                     and positions[0] < lines.index(history) < lines.index(failed) < positions[1],
                     'PR931 history-before-units ordering mismatch')
    return True
