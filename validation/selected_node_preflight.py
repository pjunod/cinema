"""Retain exact direct Node preflight results from an authenticated merged-main origin."""
from __future__ import annotations

import json
import re
import subprocess

from validation import main_preflight_adoption as journal
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r
from validation.retained_lane_commands import WEB_SCRIPTS

POLICY = 'main-preflight-node-continuation-v1'
ENVIRONMENT = {'platform': 'linux', 'machine': 'x86_64', 'python': [3, 12], 'node': 'v22.23.4'}
BEGIN = 'SELECTED-NODE-BEGIN '
PASS = 'SELECTED-NODE-PASS '
END = 'SELECTED-NODE-END '


def plan_for_records(descriptor, records, candidate):
    source = descriptor['source_commit']
    source_families = r.bounded_json(retention.git_bytes(source, str(journal.MANIFEST)))['families']
    current_families = r.bounded_json(retention.git_bytes(candidate, str(journal.MANIFEST)))['families']
    retained, uncovered = {}, {}
    for script in WEB_SCRIPTS:
        identity = 'node:' + script
        record = records.get(identity)
        origin_inputs = journal.witness(identity, source_families)
        current_inputs = journal.witness(identity, current_families)
        r.require(isinstance(record, dict) and record == {
            'run': descriptor['run'], 'job': descriptor['job'], 'attempt': 1,
            'commit': source, 'environment': ENVIRONMENT, 'outcome': 'success',
            'inputs': journal.input_digest(source, origin_inputs),
        }, 'Missing actual direct Node success at the authenticated origin')
        current = journal.input_digest(candidate, current_inputs)
        if current_inputs == origin_inputs and current == record['inputs']:
            retained[script] = record
        else:
            uncovered[script] = current
    r.require(retained, 'No exact Node input match in selected preflight origin')
    return {'provider': POLICY, 'origin': descriptor, 'retained': retained, 'uncovered': uncovered}


def prepare(api, descriptor, supplemental, candidate, repository):
    """The Python provider has already authenticated this merged-main origin."""
    r.require(isinstance(descriptor, dict) and set(descriptor) == {
        'version', 'pr', 'run', 'job', 'source_commit', 'artifact'
    } and descriptor['version'] == POLICY, 'Unknown Node preflight descriptor')
    run = r.positive(descriptor['run'])
    pr = r.positive(descriptor['pr'])
    job = r.positive(descriptor['job'])
    source = r.sha(descriptor['source_commit'])
    r.require(sum((row['run'], row['pr'], row['source_commit']) == (run, pr, source)
                  for row in supplemental) == 1,
              'Node preflight origin is outside the authenticated merged-main sources')
    artifact_id = r.positive(descriptor['artifact'])
    scope = {'repository': repository, 'pr': pr}
    name = journal.journal_key(scope)
    rows = api.pages(f'/actions/runs/{run}/artifacts')
    finals = [row for row in rows if row['id'] == artifact_id]
    r.require(len(finals) == 1 and finals[0]['run_id'] == run and finals[0]['name'] == name,
              'Node preflight artifact is outside the authenticated origin')
    from validation import main_unit_receipts as main

    preflight = main.preflight_job(api, scope, run)
    r.require(preflight['id'] == job and preflight['status'] == 'success',
              'Node preflight creator did not complete successfully')
    records, skips, _ = journal.authenticate_journal(api, scope, finals[0], ENVIRONMENT, {}, {})
    r.require(not {'node:' + script for script in WEB_SCRIPTS} & set(skips),
              'A skipped Node command is not reusable')
    return plan_for_records(descriptor, records, candidate)


def marker(plan):
    from validation.paperwork_continuation import canonical_digest
    return {'head': plan['source_commit'], 'evidence_sha256': plan['evidence_sha256'],
            'selection_sha256': canonical_digest(plan['node_preflight']),
            'environment': ENVIRONMENT}


def emit(prefix, fields):
    print(prefix + json.dumps(fields, sort_keys=True, separators=(',', ':')), flush=True)


def require_runtime():
    r.require(journal.environment() == ENVIRONMENT, 'Current Node preflight runtime differs')


def execute(plan):
    selection = plan['node_preflight']
    require_runtime()
    identity = marker(plan)
    emit(BEGIN, identity)
    for script in WEB_SCRIPTS:
        if script in selection['uncovered']:
            subprocess.run(['node', script], check=True)
            emit(PASS, {'script': script, 'input_sha256': selection['uncovered'][script]})
    emit(END, identity)


def coverage(raw, plan):
    selection = plan.get('node_preflight')
    text = re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', raw.decode('utf-8', 'strict'), flags=re.M)
    events = re.findall(r'^SELECTED-NODE-(BEGIN|PASS|END) (.+)$', text, re.M)
    if selection is None:
        r.require(not events, 'Unexpected selected Node execution')
        return None
    identity = marker(plan)
    expected = [('BEGIN', identity),
                *(('PASS', {'script': script, 'input_sha256': selection['uncovered'][script]})
                  for script in WEB_SCRIPTS if script in selection['uncovered']),
                ('END', identity)]
    r.require([(kind, r.bounded_json(value.encode())) for kind, value in events] == expected,
              'Current selected Node commands failed, skipped or disagree with their inputs')
    return {'provider': POLICY, 'origin': selection['origin'],
            'retained': selection['retained'], 'current_passes': selection['uncovered']}


def coverage_shape(value):
    r.require(isinstance(value, dict) and set(value) == {'provider', 'origin', 'retained', 'current_passes'}
              and value['provider'] == POLICY, 'Unknown selected Node coverage')
    retained, current = value['retained'], value['current_passes']
    r.require(isinstance(retained, dict) and isinstance(current, dict) and retained
              and set(retained) | set(current) == set(WEB_SCRIPTS) and not set(retained) & set(current),
              'Incomplete or contradictory selected Node coverage')
