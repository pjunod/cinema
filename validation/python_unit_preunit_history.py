"""Recover only proven empty history-stage journals from reviewed producers."""
import hashlib
import re
from pathlib import Path
from validation import python_unit_receipts as r

# Reviewed producer versions, not run/PR exemptions. Any producer change must
# acquire a new source review before its failures can use this proof route.
PRODUCERS = {'.github/workflows/effort-ci.yml': {'7a7d6774a15b8c5f3b5edfd8a4b72204a6cc823413c16182316b91e45d45743f'},
 'Makefile': {'45989ea65c32f60f7769a12d742a89ca2827536afc56a2836aa9b2a7c39732ef'},
 'validation/__init__.py': {'347a3ad6f8a7cdabc285fac0bdd906d7c579115edebde81b08fad15426de5b4e'},
 'validation/history.py': {'c82f603543090e8bbadf528ad0d12d09123d477aef69dd9aa036d03c761a34c5'},
 'validation/python_unit_receipts.py': {'b3200a1b0fc9a8fba310358ef126ef39c79a84a4dce20cb042d451e3620e777e',
                                        'e5814a8127cd8bd7c7227c168316b1a90ad55511ac8507e8ddb46a908bee978b'},
 'validation/qualification.py': {'b4bce59995c7497315ec144179ea239ab72986d68e13c540d35862a785690dc4'},
 'validation/runner.py': {'1f47e242e01ca7e7e33bd2a062d7b7aa1a4790f67f3227ea6a5706c70d8d2daa'},
 'validation/test_markers.py': {'8146dbc8d4ab391f72dccfd3d80f1db69b68f087814fa1ad38974fe8c41c2128'}}
JOB_NAMES = {'effort validation scope', 'Python unit receipts', 'effort Rust compile',
             'effort Windows compile', 'effort web static contracts', 'effort Apple compile',
             'effort Android compile', 'Effort development gate'}


def recover(api, scope, prior, jobs, marker, marker_raw, artifact, final_raw, journal):
    if journal.get('complete') is not False:
        return False
    matching = [job for job in jobs if job['name'] == r.JOB_PREFIX]
    if len(matching) != 1:
        return False
    log_raw = api.bytes(f"/actions/jobs/{r.positive(matching[0]['id'])}/logs")
    refusal = 'history audit error: authenticated promotion history binding refused'
    if refusal.encode() not in log_raw:
        return False
    commit = r.sha(prior['commit_sha'])
    run = r.positive(prior['id'])
    actual = api.get(f'/actions/runs/{run}')
    r.require(actual['id'] == run and actual['repository']['id'] == scope['repository']
              and actual['commit_sha'] == commit and actual['prettyref'] == scope['branch']
              and actual['workflow_id'] == 'effort-ci.yml' and actual['status'] == 'failure'
              and actual['trigger_event'] == 'workflow_dispatch', 'Pre-unit run identity mismatch')
    event = r.bounded_json(actual['event_payload'].encode())
    r.require(event['repository']['id'] == scope['repository']
              and event['ref'] == 'refs/heads/' + scope['branch']
              and event['workflow'] == '.github/workflows/effort-ci.yml',
              'Pre-unit event identity mismatch')
    pull = api.get('/pulls/' + str(r.positive(scope['pr'])))
    r.require(pull['number'] == scope['pr'] and pull['head']['repo']['id'] == scope['repository']
              and pull['base']['repo']['id'] == scope['repository']
              and pull['head']['ref'] == scope['branch'] and pull['base']['ref'] == scope['base']
              and scope['base'].startswith('effort/'), 'Pre-unit PR scope mismatch')
    r.require(len(jobs) == len(JOB_NAMES) and {job['name'] for job in jobs} == JOB_NAMES
              and len({job['id'] for job in jobs}) == len(jobs)
              and all(job['repo_id'] == scope['repository'] and job['run_id'] == run
                      and type(job['attempt']) is int and job['attempt'] == 1
                      and job['status'] in {'success', 'failure', 'skipped'} for job in jobs),
              'Pre-unit complete job inventory mismatch')
    unit_job = next(job for job in jobs if job['name'] == r.JOB_PREFIX)
    r.require(unit_job['status'] == 'failure', 'Pre-unit job was not failed')
    for path, digests in PRODUCERS.items():
        raw = api.bytes('/raw/' + path, {'ref': commit})
        r.require(hashlib.sha256(raw).hexdigest() in digests,
                  'Pre-unit producer source not reviewed: ' + path)
        if path == 'validation/python_unit_receipts.py' and b'recover_preunit_history' in raw:
            helper = api.bytes('/raw/validation/python_unit_preunit_history.py', {'ref': commit})
            r.require(helper == Path(__file__).read_bytes(), 'Pre-unit recovery helper source changed')
    r.require(log_raw.endswith(b'\n'), 'Pre-unit log incomplete')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line) for line in log_raw.decode().splitlines()]
    log = '\n'.join(lines)
    r.require(lines[-1] == "Job 'Python unit receipts' failed" and commit in lines
              and not any(token in log for token in ('discovered=', 'historical-passes=', 'pending=',
                  '... ok', '... FAIL', '... ERROR', 'Unit discovery failed'))
              and not any(re.match(r'^(?:Ran \d+ tests?(?: |$)|(?:not )?ok \d+|# tests )', line)
                          for line in lines), 'Pre-unit log contradicts nonexecution')
    refusal = 'history audit error: authenticated promotion history binding refused'
    failure = 'make: *** [Makefile:144: history-check] Error 2'
    r.require(lines.count(refusal) == lines.count(failure) == 1,
              'Pre-unit exact history refusal missing')
    positions = []
    for item, zipped, name in ((marker, marker_raw, r.key(scope) + f'-start-{run}'),
                               (artifact, final_raw, r.key(scope))):
        r.require(item['run_id'] == run and item['name'] == name and not item['expired']
                  and item['size_in_bytes'] == len(zipped) <= r.MAX_BYTES,
                  'Pre-unit artifact identity mismatch')
        uploaded = (f'Artifact {name} has been successfully uploaded! Final size is '
                    f"{len(zipped)} bytes. Artifact ID is {r.positive(item['id'])}")
        digest_line = 'SHA256 hash of uploaded artifact zip is ' + hashlib.sha256(zipped).hexdigest()
        r.require(lines.count(uploaded) == lines.count(digest_line) == 1,
                  'Pre-unit artifact authenticated upload/digest mismatch')
        position = lines.index(uploaded)
        r.require(lines.index(digest_line) < position, 'Pre-unit artifact digest ordering mismatch')
        positions.append(position)
    start = r.artifact_json(marker_raw)
    r.validate_journal(start, scope, run, commit, completed=False)
    r.validate_journal(journal, scope, run, commit, completed=False)
    r.require(start == journal and start['complete'] is False and start['passes'] == {}
              and start.get('applicability_commit') == commit
              and positions[0] < lines.index(refusal) < lines.index(failure) < positions[1],
              'Pre-unit journal changed or history ordering mismatch')
    return True
