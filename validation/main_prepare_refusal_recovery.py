"""Exact PR917 failed prepares; no replacement journals or success claims."""
import hashlib
from pathlib import Path
import re

from validation import python_unit_receipts as receipts


DESCRIPTOR_SHA256 = '6b532bef3121f311a7a8aaf138d3d1293a50766791585e7e366f105009a1e0fa'


def descriptor():
    path = Path(__file__).resolve().with_name('main-prepare-refusals917.json')
    receipts.require(path.is_file() and not path.is_symlink(), 'Prepare917 proof unavailable')
    with path.open('rb') as stream:
        raw = stream.read(receipts.MAX_BYTES + 1)
    receipts.require(len(raw) <= receipts.MAX_BYTES
                     and hashlib.sha256(raw).hexdigest() == DESCRIPTOR_SHA256,
                     'Prepare917 descriptor changed')
    return receipts.bounded_json(raw)


def recover(api, scope, prior, jobs):
    if (scope.get('repository'), scope.get('pr'), prior['id']) not in (
            (1, 917, 4514), (1, 917, 4516), (1, 917, 4518)):
        return False
    from validation import main_preflight_adoption as adoption
    proof = descriptor()
    expected = {'repository': 1, 'pr': 917}
    receipts.require(proof['version'] == 1 and proof['scope'] == expected
                     and proof['branch'] == 'integration/batch-003-into-main',
                     'Prepare917 descriptor scope mismatch')
    receipts.require(all(scope.get(field, value) == value for field, value in {
        'branch': proof['branch'], 'base': 'main', 'workflow': 'main-fast-lane.yml',
    }.items()), 'Prepare917 current branch/base/workflow mismatch')
    case = next(row for row in proof['cases'] if row['run'] == prior['id'])
    actual = api.get(f"/actions/runs/{case['run']}")
    receipts.require(actual['id'] == prior['id'] == case['run']
                     and actual['commit_sha'] == prior['commit_sha'] == case['commit']
                     and adoption.terminal_status(actual) == adoption.terminal_status(prior) == 'failure',
                     'Prepare917 terminal source mismatch')
    event = adoption.bind_event(actual, expected)
    receipts.require(event == adoption.bind_event(prior, expected)
                     and event['action'] == 'synchronized'
                     and event['pull_request']['head']['ref'] == proof['branch']
                     and event['pull_request']['base']['sha'] == case['base']
                     and hashlib.sha256(actual['event_payload'].encode()).hexdigest() == case['event_sha256'],
                     'Prepare917 event identity mismatch')
    receipts.require(all(job['run_id'] == case['run'] and job['repo_id'] == 1
                         and type(job['attempt']) is int and job['attempt'] == 1 for job in jobs)
                     and sorted([[job['id'], job['name'], job['task_id'], job['attempt'],
                                  adoption.terminal_status(job)] for job in jobs]) == case['jobs'],
                     'Prepare917 complete job inventory mismatch')
    for path, digest in case['sources'].items():
        original = api.bytes('/raw/' + path, {'ref': case['commit']})
        receipts.require(hashlib.sha256(original).hexdigest() == digest,
                         'Prepare917 original producer mismatch')
    raw = api.bytes(f"/actions/jobs/{case['job']}/logs")
    receipts.require(len(raw) == case['log_bytes']
                     and hashlib.sha256(raw).hexdigest() == case['log_sha256'],
                     'Prepare917 complete original log mismatch')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line)
             for line in raw.decode().splitlines()]
    refusal = 'Main Python receipt refused: ' + case['reason']
    skipped = ["skipping post step for '" + step + "'; main step was skipped" for step in (
        'Preserve per-ID preflight journal even on failure',
        'Preserve main Python success journal even on unit failure',
        'Publish preflight attempt-start journal',
        'Publish main Python attempt-start marker',
    )]
    log = '\n'.join(lines)
    receipts.require(raw.endswith(b'\n') and lines[-1] == "Job 'fast policy and contract preflight' failed"
                     and case['commit'] + ':refs/remotes/pull/917/head' in log
                     and lines.count(refusal) == 1
                     and all(lines.count(line) == 1 and lines.index(refusal) < lines.index(line)
                             for line in skipped)
                     and not any(token in log for token in (
                         'MAIN-UNIT-', 'Main preflight outcome ', 'Authenticated candidates=',
                         'discovered=', 'pending=', 'has been successfully uploaded!',
                         '... ok', '... FAIL', '... ERROR', '... skipped'))
                     and not any(re.match(r'^(?:Ran \d+ tests?(?: |$)|(?:not )?ok \d+|# tests )', line)
                                 for line in lines),
                     'Prepare917 contradicts zero-unit evidence')
    receipts.require(api.get(f"/actions/runs/{case['run']}/artifacts") == [],
                     'Prepare917 unexpectedly has run artifacts')
    for name in ('main-preflight-v1-r1-pr917', 'main-python-units-v1-r1-pr917'):
        receipts.require(not any(item['run_id'] == case['run'] for item in
                                 api.pages('/actions/artifacts', {'name': name}))
                         and not api.pages('/actions/artifacts', {'name': name + f"-start-{case['run']}"}),
                         'Prepare917 unexpectedly has receipt artifacts')
    print(f"Accounted exact failed prepare {case['run']}: zero units; no outcomes imported")
    return True
