"""Account proven pre-unit prepares without importing outcomes or changing events."""
import hashlib
from pathlib import Path
import re
from validation import python_unit_receipts as r

DESCRIPTOR_SHA256 = '9b6e936d5b28b66fb22e8502b86cab85403802f8b3e834b7b04ad6996828bcc0'


def descriptor():
    path = Path(__file__).with_name('main-source-skew4513.json')
    r.require(path.is_file() and not path.is_symlink(), 'Source4513 proof unavailable')
    raw = path.read_bytes()
    r.require(len(raw) <= r.MAX_BYTES and hashlib.sha256(raw).hexdigest() == DESCRIPTOR_SHA256,
              'Source4513 descriptor changed')
    return r.bounded_json(raw)


def recover(api, scope, prior, jobs):
    if (scope.get('repository'), scope.get('pr'), prior['id']) == (1, 917, 4540):
        return recover_chain4540(api, scope, prior, jobs)
    if (scope.get('repository'), scope.get('pr'), prior['id']) != (1, 917, 4513):
        return recover_prepare_refusal_chain(api, scope, prior, jobs)
    from validation import main_preflight_adoption as adoption
    proof = descriptor()
    r.require(proof['version'] == 1 and proof['scope'] == {'repository': 1, 'pr': 917}
              and proof['run'] == 4513 and proof['job'] == 45673
              and proof['branch'] == 'integration/batch-003-into-main'
              and proof['commit'] == '62672d0865e3d6335065b10dc68f2a58ddb6ce07'
              and proof['event_head'] == '6518779b2cf24fb2a8cf434ffe0e23985e519d43',
              'Source4513 exact proof identity mismatch')
    r.require(all(scope.get(key, value) == value for key, value in {
        'branch': proof['branch'], 'base': 'main', 'workflow': 'main-fast-lane.yml'}.items()),
        'Source4513 current scope mismatch')
    actual = api.get('/actions/runs/4513')
    for run in (prior, actual):
        r.require(run['id'] == proof['run'] and run['commit_sha'] == proof['commit']
                  and run['workflow_id'] == 'main-fast-lane.yml' and run['prettyref'] == '#917'
                  and run['event'] == 'pull_request' and run['repository']['id'] == 1
                  and adoption.terminal_status(run) == 'failure'
                  and hashlib.sha256(run['event_payload'].encode()).hexdigest() == proof['event_sha256'],
                  'Source4513 original run/event mismatch')
    # Do not normalize the original head/source discrepancy or relax bind_event.
    event = r.bounded_json(actual['event_payload'].encode())
    pull = event['pull_request']
    r.require(event['action'] == 'synchronized' and event['number'] == pull['number'] == 917
              and event['repository']['id'] == pull['head']['repo']['id'] == pull['base']['repo']['id'] == 1
              and event['repository']['full_name'] == actual['repository']['full_name']
              and pull['head']['ref'] == proof['branch'] and pull['head']['sha'] == proof['event_head']
              and pull['head']['sha'] != proof['commit'] and pull['base']['ref'] == 'main'
              and pull['base']['sha'] == proof['base'] and pull['state'] == 'open' and pull['draft'] is False,
              'Source4513 exact event discrepancy changed')
    r.require(all(job['run_id'] == 4513 and job['repo_id'] == 1
                  and type(job['attempt']) is int and job['attempt'] == 1 for job in jobs)
              and sorted([[job['id'], job['name'], job['task_id'], job['attempt'],
                           adoption.terminal_status(job)] for job in jobs]) == proof['jobs'],
              'Source4513 complete job inventory mismatch')
    for path, digest in proof['sources'].items():
        r.require(hashlib.sha256(api.bytes('/raw/' + path, {'ref': proof['commit']})).hexdigest() == digest,
                  'Source4513 original producer mismatch')
    raw = api.bytes('/actions/jobs/45673/logs')
    r.require(len(raw) == proof['log_bytes'] and hashlib.sha256(raw).hexdigest() == proof['log_sha256'],
              'Source4513 original complete log mismatch')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line) for line in raw.decode().splitlines()]
    log = '\n'.join(lines)
    refusal = 'Main Python receipt refused: Main receipt must match an open same-repository PR head'
    skipped = ["skipping post step for '" + step + "'; main step was skipped" for step in (
        'Preserve per-ID preflight journal even on failure',
        'Preserve main Python success journal even on unit failure',
        'Publish preflight attempt-start journal', 'Publish main Python attempt-start marker')]
    r.require(raw.endswith(b'\n') and lines[-1] == "Job 'fast policy and contract preflight' failed"
              and proof['commit'] + ':refs/remotes/pull/917/head' in log and lines.count(refusal) == 1
              and all(lines.count(line) == 1 and lines.index(refusal) < lines.index(line) for line in skipped)
              and not any(token in log for token in ('MAIN-UNIT-', 'Main preflight outcome ',
                  'Authenticated candidates=', 'discovered=', 'pending=', 'has been successfully uploaded!',
                  '... ok', '... FAIL', '... ERROR', '... skipped'))
              and not any(re.match(r'^(?:Ran \d+ tests?(?: |$)|(?:not )?ok \d+|# tests )', line)
                          for line in lines), 'Source4513 contradicts zero-unit execution')
    r.require(api.get('/actions/runs/4513/artifacts') == [], 'Source4513 unexpectedly has run artifacts')
    for name in ('main-preflight-v1-r1-pr917', 'main-python-units-v1-r1-pr917'):
        r.require(not any(item['run_id'] == 4513 for item in api.pages('/actions/artifacts', {'name': name}))
                  and not api.pages('/actions/artifacts', {'name': name + '-start-4513'}),
                  'Source4513 unexpectedly has receipt artifacts')
    print('Accounted exact source-skew prepare 4513: original event retained; zero outcomes imported')
    return True


def recover_chain4540(api, scope, prior, jobs):
    """Exact subsequent prepare refusal; retain the earlier skew as negative."""
    from validation import main_preflight_adoption as adoption
    proof = descriptor()
    case = proof['chain']
    r.require(proof['version'] == 1 and proof['scope'] == {'repository': 1, 'pr': 917}
              and case['run'] == 4540 and case['job'] == 45934
              and case['commit'] == 'ffc03221d74de85312f567cf215113adc27416bb'
              and all(scope.get(key, value) == value for key, value in {
                  'branch': proof['branch'], 'base': 'main', 'workflow': 'main-fast-lane.yml'}.items()),
              'Chain4540 exact scope mismatch')
    actual = api.get('/actions/runs/4540')
    for run in (actual, prior):
        r.require(run['id'] == 4540 and run['commit_sha'] == case['commit']
                  and adoption.terminal_status(run) == 'failure'
                  and hashlib.sha256(run['event_payload'].encode()).hexdigest() == case['event_sha256'],
                  'Chain4540 original terminal source/event mismatch')
        event = adoption.bind_event(run, {'repository': 1, 'pr': 917})
        r.require(event['action'] == 'synchronized'
                  and event['pull_request']['head']['ref'] == proof['branch']
                  and event['pull_request']['base']['sha'] == case['base'],
                  'Chain4540 original binding mismatch')
    r.require(all(job['run_id'] == 4540 and job['repo_id'] == 1
                  and type(job['attempt']) is int and job['attempt'] == 1 for job in jobs)
              and sorted([[job['id'], job['name'], job['task_id'], job['attempt'],
                           adoption.terminal_status(job)] for job in jobs]) == case['jobs'],
              'Chain4540 complete job inventory mismatch')
    for path, digest in case['sources'].items():
        r.require(hashlib.sha256(api.bytes('/raw/' + path, {'ref': case['commit']})).hexdigest() == digest,
                  'Chain4540 original producer mismatch')
    raw = api.bytes('/actions/jobs/45934/logs')
    r.require(len(raw) == case['log_bytes'] and hashlib.sha256(raw).hexdigest() == case['log_sha256'],
              'Chain4540 original complete log mismatch')
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line) for line in raw.decode().splitlines()]
    log = '\n'.join(lines)
    refusal = 'Main Python receipt refused: Prior event/source/PR/base/readiness binding mismatch'
    skipped = ["skipping post step for '" + step + "'; main step was skipped" for step in (
        'Preserve per-ID preflight journal even on failure',
        'Preserve main Python success journal even on unit failure',
        'Publish preflight attempt-start journal', 'Publish main Python attempt-start marker')]
    r.require(raw.endswith(b'\n') and lines[-1] == "Job 'fast policy and contract preflight' failed"
              and case['commit'] + ':refs/remotes/pull/917/head' in log and lines.count(refusal) == 1
              and all(lines.count(line) == 1 and lines.index(refusal) < lines.index(line) for line in skipped)
              and not any(token in log for token in ('MAIN-UNIT-', 'Main preflight outcome ',
                  'Authenticated candidates=', 'discovered=', 'pending=', 'has been successfully uploaded!',
                  '... ok', '... FAIL', '... ERROR', '... skipped'))
              and not any(re.match(r'^(?:Ran \d+ tests?(?: |$)|(?:not )?ok \d+|# tests )', line)
                          for line in lines), 'Chain4540 contradicts zero-unit execution')
    r.require(api.get('/actions/runs/4540/artifacts') == [], 'Chain4540 unexpectedly has run artifacts')
    for name in ('main-preflight-v1-r1-pr917', 'main-python-units-v1-r1-pr917'):
        r.require(not any(item['run_id'] == 4540 for item in api.pages('/actions/artifacts', {'name': name}))
                  and not api.pages('/actions/artifacts', {'name': name + '-start-4540'}),
                  'Chain4540 unexpectedly has receipt artifacts')
    print('Accounted exact subsequent prepare 4540: zero outcomes imported')
    return True


PREPARE_PRODUCERS = {'.github/workflows/main-fast-lane.yml': ['c504437ac012ebb70f44042f6c5bfb4f7b9f9716a9fa122f92863e6ec56f8f11'],
 'Makefile': ['45989ea65c32f60f7769a12d742a89ca2827536afc56a2836aa9b2a7c39732ef'],
 'validation/__init__.py': ['347a3ad6f8a7cdabc285fac0bdd906d7c579115edebde81b08fad15426de5b4e'],
 'validation/main_preflight_adoption.py': ['91841c4f105e31ec169f9d497be7c9fca9fc250b1220d6079592fb2cf3c6599c',
                                           '9c195213ebd94f49372e13364d616d0859b218506d294db15add6a34aab2b8db'],
 'validation/main_unit_receipts.py': ['b36c4ee5ea97675034430d86dfc00b67d153e919dea95af52708b3cf5a1f6aa0',
                                      'fecf10b37436ca6307d734c1fec37465df69dc01041af0d7dc5653f17249c748',
                                      'e7842dd386693f9f9ba9bdf3e6095156f2ee81f74bb78d305820d7aa701262bb'],
 'validation/python_unit_receipts.py': ['2cfbfdf8911c83d7c63803c3b447ef332e7d11d3b2f8a909592b222d24cd33bc',
                                        'e5814a8127cd8bd7c7227c168316b1a90ad55511ac8507e8ddb46a908bee978b'],
 'validation/runner.py': ['1f47e242e01ca7e7e33bd2a062d7b7aa1a4790f67f3227ea6a5706c70d8d2daa'],
 'validation/test_markers.py': ['8146dbc8d4ab391f72dccfd3d80f1db69b68f087814fa1ad38974fe8c41c2128']}
PREPARE_HELPERS = ['1188606836585fb133e9331ffcd15fcc3ee6402545d4ce31a0803b6bf4d4b969']


def prepare_ancestor(commit):
    """Bind original source to this immutable current candidate, never HEAD."""
    import os
    import subprocess
    current = r.sha(os.environ['GITHUB_SHA'])
    return subprocess.run(['git', 'merge-base', '--is-ancestor', commit, current],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def recover_prepare_refusal_chain(api, scope, prior, jobs):
    """Prove no units existed before an authenticated reviewed prepare refused."""
    import os
    from validation import main_preflight_adoption as adoption
    if (scope.get('repository'), scope.get('pr')) != (1, 917) or adoption.terminal_status(prior) != 'failure':
        return False
    matches = [job for job in jobs if job['name'] == 'fast policy and contract preflight']
    if len(matches) != 1:
        return False
    raw = api.bytes(f"/actions/jobs/{r.positive(matches[0]['id'])}/logs")
    lines = [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line) for line in raw.decode().splitlines()]
    # Only these reviewed prepare guards can take this zero-outcome route.
    refusals = [line for line in lines if re.fullmatch(
        r'Main Python receipt refused: (?:Main receipt must match an open same-repository PR head|'
        r'Missing final journal for prior attempt [1-9]\d*; preserve possibly passed IDs)', line)]
    if not refusals:
        return False
    run_id = r.positive(prior['id'])
    commit = r.sha(prior['commit_sha'])
    actual = api.get(f'/actions/runs/{run_id}')
    if actual['id'] != run_id:
        return False
    r.require(run_id < r.positive(int(os.environ['GITHUB_RUN_ID']))
              and prepare_ancestor(commit), 'Prepare chain original source/current-run mismatch')
    for run in (actual, prior):
        r.require(run['id'] == run_id and run['commit_sha'] == commit
                  and adoption.terminal_status(run) == 'failure'
                  and run['event_payload'] == actual['event_payload'],
                  'Prepare chain original terminal source/event mismatch')
        event = adoption.bind_event(run, {'repository': 1, 'pr': 917})
        r.require(event['action'] == 'synchronized'
                  and event['pull_request']['head']['ref'] == 'integration/batch-003-into-main'
                  and event['pull_request']['base']['ref'] == 'main',
                  'Prepare chain original event scope mismatch')
    r.require(all(scope.get(field, value) == value for field, value in {
        'branch': 'integration/batch-003-into-main', 'base': 'main',
        'workflow': 'main-fast-lane.yml'}.items()), 'Prepare chain current scope mismatch')
    statuses = {'fast-lane validation scope': 'success', 'fast-lane mobile release version': 'success',
        'fast policy and contract preflight': 'failure', 'Main promotion gate': 'failure',
        'fast Rust gate': 'skipped', 'fast Windows compile': 'skipped',
        'fast web syntax gate': 'skipped', 'fast Apple compile': 'skipped', 'fast Android compile': 'skipped'}
    r.require(len(jobs) == len(statuses) and {job['name'] for job in jobs} == set(statuses)
              and len({job['id'] for job in jobs}) == len(jobs)
              and all(job['run_id'] == run_id and job['repo_id'] == 1
                      and type(job['attempt']) is int and job['attempt'] == 1
                      and adoption.terminal_status(job) == statuses[job['name']] for job in jobs),
              'Prepare chain complete terminal job inventory mismatch')
    sources = {}
    for path, accepted in PREPARE_PRODUCERS.items():
        original = api.bytes('/raw/' + path, {'ref': commit})
        r.require(hashlib.sha256(original).hexdigest() in accepted,
                  'Prepare chain original producer source not reviewed: ' + path)
        sources[path] = original
    if b'validation.main_source_skew4513' in sources['validation/main_preflight_adoption.py']:
        helper = api.bytes('/raw/validation/main_source_skew4513.py', {'ref': commit})
        r.require(hashlib.sha256(helper).hexdigest() in PREPARE_HELPERS
                  or helper == Path(__file__).read_bytes(), 'Prepare chain helper source not reviewed')
    skipped = ["skipping post step for '" + step + "'; main step was skipped" for step in (
        'Preserve per-ID preflight journal even on failure',
        'Preserve main Python success journal even on unit failure',
        'Publish preflight attempt-start journal', 'Publish main Python attempt-start marker')]
    log = '\n'.join(lines)
    r.require(raw.endswith(b'\n') and lines[-1] == "Job 'fast policy and contract preflight' failed"
              and commit + ':refs/remotes/pull/917/head' in log and len(refusals) == 1
              and all(lines.count(line) == 1 and lines.index(refusals[0]) < lines.index(line) for line in skipped)
              and not any(token in log for token in ('MAIN-UNIT-', 'Main preflight outcome ',
                  'Authenticated candidates=', 'discovered=', 'pending=', 'has been successfully uploaded!',
                  '... ok', '... FAIL', '... ERROR', '... skipped'))
              and not any(re.match(r'^(?:Ran \d+ tests?(?: |$)|(?:not )?ok \d+|# tests )', line)
                          for line in lines), 'Prepare chain contradicts zero-unit execution')
    dependency = re.search(r'Missing final journal for prior attempt (\d+)', refusals[0])
    r.require(dependency is None or r.positive(int(dependency[1])) < run_id,
              'Prepare chain dependency is not an earlier attempt')
    r.require(api.get(f'/actions/runs/{run_id}/artifacts') == [],
              'Prepare chain unexpectedly has run artifacts')
    for name in ('main-preflight-v1-r1-pr917', 'main-python-units-v1-r1-pr917'):
        r.require(not any(item['run_id'] == run_id for item in api.pages('/actions/artifacts', {'name': name}))
                  and not api.pages('/actions/artifacts', {'name': name + f'-start-{run_id}'}),
                  'Prepare chain unexpectedly has receipt artifacts')
    print(f'Accounted reviewed prepare refusal {run_id}: zero outcomes imported')
    return True
