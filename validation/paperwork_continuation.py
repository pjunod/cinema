"""Authenticated source-gate paperwork reuse; preliminary coverage stays pending."""
from __future__ import annotations

import ast
import json
import os
from pathlib import Path
import re
import subprocess

from validation import main_python_continuation as main
from validation import nonrust_unit_cohort as cohort
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r

PREFIX = 'Paperwork-Continuation: '
RULE_MODULES = ('test_docs_index', 'test_infra_names', 'test_api_doc_routes',
                'test_apple_build_claims', 'test_mobile_build_claims', 'test_status_page_shape',
                'test_status_pr_claims', 'test_doc_status_audit', 'test_web_test_inventory',
                'test_script_syntax', 'test_media_pool_wire', 'test_cluster_facade_ratchet',
                'test_doc_size_budget')
COVERAGE_FIELDS = {'version', 'head', 'base', 'pr', 'run', 'preflight_job', 'evidence_sha256',
                   'inventory_sha256', 'retained_ids', 'current_pass_ids', 'preflight_log_sha256'}


def canonical_digest(value):
    return retention.digest(json.dumps(value, sort_keys=True, separators=(',', ':')).encode())


def enrollment(body):
    rows = [line[len(PREFIX):] for line in (body or '').splitlines() if line.startswith(PREFIX)]
    r.require(len(rows) == 1, 'Explicit unique paperwork continuation enrollment required')
    value = r.bounded_json(rows[0].encode())
    r.require(isinstance(value, dict) and set(value) == {'version', 'pr', 'head', 'base', 'python_unit_evidence'}
              and value['version'] == 1, 'Unknown paperwork enrollment fields')
    r.positive(value['pr']); r.sha(value['head']); r.sha(value['base'])
    descriptor = value['python_unit_evidence']
    r.require(isinstance(descriptor, dict) and set(descriptor) == {'sha256', 'attachment_id', 'attachment_uuid'}
              and retention.DIGEST.fullmatch(descriptor['sha256']) is not None,
              'Unknown paperwork Python evidence descriptor')
    r.positive(descriptor['attachment_id'])
    return value


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def ancestor(root, parent, child):
    return subprocess.run(['git', '-C', str(root), 'merge-base', '--is-ancestor', parent, child],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def binding(api, value, environment, root, mode):
    r.require(mode in {'preliminary', 'publication', 'final'}, 'Unknown paperwork continuation mode')
    repo = api.get(''); pull = api.get('/pulls/' + str(value['pr']))
    candidate, base = value['head'], value['base']
    r.require(pull['number'] == value['pr'] and pull['state'] == 'open' and pull['merged'] is False
              and pull['head']['repo']['id'] == pull['base']['repo']['id'] == repo['id']
              and pull['head']['repo']['full_name'] == pull['base']['repo']['full_name'] == repo['full_name']
              and pull['base']['ref'] == 'main' and pull['base']['sha'] == base
              and api.get('/branches/main')['commit']['id'] == base
              and git(root, 'rev-parse', 'HEAD') == candidate and ancestor(root, base, candidate),
              'Paperwork repository, live PR, current main or checkout moved')
    r.require(enrollment(pull['body']) == value, 'Live PR paperwork enrollment differs')
    branch = pull['head']['ref']
    from validation.qualification import promotion_ref
    r.require(branch != 'main' and not promotion_ref(branch),
              'Paperwork continuation is limited to ordinary PR source gates')
    r.require([row['commit']['id'] for row in api.pages('/branches') if row['name'] == branch]
              == [pull['head']['sha']], 'Published paperwork branch moved')
    if mode == 'publication':
        r.require(not environment.get('GITHUB_RUN_ID') and not environment.get('GITHUB_EVENT_NAME')
                  and pull['draft'] is True and git(root, 'symbolic-ref', '--short', 'HEAD') == branch
                  and not git(root, 'status', '--porcelain')
                  and ancestor(root, r.sha(pull['head']['sha']), candidate),
                  'Local publication requires a clean same-branch draft descendant, outside CI')
    else:
        r.require(environment.get('GITHUB_RUN_ATTEMPT') == '1'
                  and environment.get('GITHUB_SHA') == candidate and pull['head']['sha'] == candidate,
                  'CI paperwork must bind the actual published event head')
        run = api.get('/actions/runs/' + str(r.positive(int(environment['GITHUB_RUN_ID']))))
        r.require(run['repository']['id'] == repo['id'] and run['commit_sha'] == candidate
                  and run['status'] in {'running', 'success'}
                  and run['workflow_id'] == ('main-fast-lane.yml' if mode == 'preliminary' else 'ci.yml')
                  and run['trigger_event'] == ('pull_request' if mode == 'preliminary' else 'workflow_dispatch'),
                  'Paperwork run provenance differs')
        from validation import source_gate_landing as landing
        workflow = retention.git_bytes(candidate, '.github/workflows/' + run['workflow_id'])
        if mode == 'final':
            r.require(retention.digest(workflow) == landing.WORKFLOW_SHA256, 'Unreviewed final paperwork workflow')
        else:
            landing.nextest.projected_main_workflow(workflow)
        event = r.bounded_json(run['event_payload'].encode())
        if mode == 'preliminary':
            supplied = r.bounded_json(Path(environment['GITHUB_EVENT_PATH']).read_bytes())
            for payload in (event, supplied):
                pr = payload['pull_request']
                r.require(pr['number'] == value['pr'] and pr['head']['sha'] == candidate
                          and pr['head']['ref'] == branch and pr['base']['sha'] == base
                          and pr['head']['repo']['id'] == pr['base']['repo']['id'] == repo['id'],
                          'Paperwork PR event identity differs')
        else:
            inputs = event['inputs']
            r.require(event['ref'] == 'refs/heads/' + branch
                      and inputs['promotion_pr'] == str(value['pr'])
                      and inputs['promotion_head_sha'] == candidate and inputs['promotion_base_sha'] == base
                      and r.bounded_json(inputs['python_unit_evidence'].encode()) == value['python_unit_evidence'],
                      'Final paperwork manual inputs differ from enrollment')
    return {'schema': 2, 'repository': repo['full_name'], 'pull_request': value['pr'],
            'head_ref': branch, 'base_ref': 'main', 'head_sha': candidate, 'base_sha': base}, pull


def local_attestation(api, raw, descriptor, bind, candidate, pull):
    """A future local target is explicit; never project the API's actual head."""
    sha = descriptor['sha256']; r.require(retention.digest(raw) == sha, 'Publication evidence digest differs')
    document = r.bounded_json(raw); repository = r.positive(api.get('')['id'])
    r.require(document['repository'] == repository and document['pr'] == pull['number']
              and document['branch'] == bind['head_ref'] and document['base_sha'] == bind['base_sha']
              and document['source_commit'] == candidate, 'Publication evidence source binding differs')
    claim = {'repository': repository, 'pr': pull['number'], 'sha256': sha,
             'attachment_id': descriptor['attachment_id'], 'attachment_uuid': descriptor['attachment_uuid']}
    found = False
    for comment in api.pages(f"/issues/{pull['number']}/comments"):
        lines = [line.removeprefix('Promotion-Unit-Evidence: ') for line in comment['body'].splitlines()
                 if line.startswith('Promotion-Unit-Evidence: ')]
        if not lines:
            continue
        r.require(len(lines) == 1, 'Ambiguous publication evidence attestation')
        if r.bounded_json(lines[0].encode()) == claim:
            r.verify_attestor(api, {'repository': repository}, comment['user'])
            r.positive(comment['id']); found = True
    r.require(found, 'Publication evidence lacks authenticated writer attestation')
    return document, repository


def rule_inventory(plan, root):
    tree = ast.parse((Path(root) / 'scripts/paperwork').read_text())
    rows = [node for node in tree.body if isinstance(node, ast.AnnAssign)
            and isinstance(node.target, ast.Name) and node.target.id == 'TEST_RULES']
    r.require(len(rows) == 1, 'Missing canonical paperwork rules')
    modules = tuple(row[0] for row in ast.literal_eval(rows[0].value))
    r.require(modules == RULE_MODULES, 'Paperwork rule cohort changed beyond reviewed policy')
    names = plan['suites']['operations']['inventory']
    r.require(names == sorted(set(names)), 'Repeated Python inventory')
    inventory = {'operations:' + name for name in names if name.split('.')[0] in modules}
    r.require(inventory and {name.split(':', 1)[1].split('.')[0] for name in inventory} == set(modules),
              'Incomplete current paperwork method discovery')
    retained = set(plan['suites']['operations']['retained_source_records']) & inventory
    due = {'operations:' + name for name in plan['suites']['operations']['uncovered']} & inventory
    r.require(not retained & due and retained | due == inventory, 'Paperwork coverage has a gap or contradiction')
    return inventory, retained, due


def prepare(api, value, environment, root, mode):
    bind, pull = binding(api, value, environment, root, mode)
    d = value['python_unit_evidence']; candidate = value['head']
    raw = retention.attachment_bytes(api, value['pr'], d['attachment_id'], d['attachment_uuid'], d['sha256'])
    if mode == 'publication':
        document, repository = local_attestation(api, raw, d, bind, candidate, pull)
    else:
        document, _, repository, _ = retention.verify_identity(
            api, raw, d['sha256'], bind, candidate, d['attachment_id'], d['attachment_uuid'])
    r.require(document['version'] == main.MAIN_ANCESTOR_POLICY and document['source_commit'] == candidate,
              'Paperwork needs exact-source authenticated main-Python continuation')
    plan = main.prepare(api, document, bind, candidate, repository)
    inventory, retained, due = rule_inventory(plan, root)
    return bind, pull, plan, inventory, retained, due


def current_rule_inventory(root='.'):
    names = sorted(test.id() for test in r.discover('operations'))
    plan = {'suites': {'operations': {'inventory': names, 'uncovered': names,
                                     'retained_source_records': {}}}}
    return rule_inventory(plan, root)[0]


def marker(plan, kind):
    suite = plan['suites'][kind]
    return {'version': 1, 'kind': kind, 'source_commit': plan['source_commit'],
            'evidence_sha256': plan['evidence_sha256'],
            'inventory_sha256': canonical_digest(suite['inventory']),
            'selected_sha256': canonical_digest(suite['uncovered'])}


def current_outcomes(raw, plan):
    text = re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', raw.decode('utf-8', 'strict'), flags=re.M)
    begin = list(re.finditer(r'^SELECTED-PYTHON-BEGIN (.+)$', text, re.M))
    end = list(re.finditer(r'^SELECTED-PYTHON-END (.+)$', text, re.M))
    r.require(sum(line.startswith('SELECTED-PYTHON-') for line in text.splitlines()) == 4
              and len(begin) == len(end) == 2, 'Missing or repeated direct Python execution framing')
    observed = {}
    previous = -1
    for index, kind in enumerate(('validation', 'operations')):
        expected = marker(plan, kind)
        r.require(previous < begin[index].start() < end[index].start()
                  and r.bounded_json(begin[index][1].encode()) == expected
                  and r.bounded_json(end[index][1].encode()) == expected,
                  'Current Python execution source, selection or framing differs')
        selected = plan['suites'][kind]['uncovered']
        block = text[begin[index].end():end[index].start()]
        if not selected:
            r.require(not re.search(r'^\w+ \([^)\n]+\).*?\.\.\.', block, re.M)
                      and not re.search(r'^Ran \d+ tests?', block, re.M),
                      'Unexpected execution in an empty Python selection')
        rows = cohort.python_rows(block, kind, set(selected)) if selected else {}
        r.require(set(rows) == set(selected) and all(value == 'pass' for value in rows.values()),
                  'Current direct Python cases failed, skipped or incomplete')
        observed.update({kind + ':' + name: value for name, value in rows.items()})
        previous = end[index].end()
    return observed


def coverage_shape(value, bind, run, inventory=None):
    r.positive(run)
    r.require(isinstance(value, dict) and set(value) == COVERAGE_FIELDS and value['version'] == 1
              and value['head'] == bind['head_sha'] and value['base'] == bind['base_sha']
              and value['pr'] == bind['pull_request'] and value['run'] == run,
              'Unknown current paperwork coverage scope')
    r.positive(value['pr']); r.positive(value['preflight_job'])
    for field in ('evidence_sha256', 'inventory_sha256', 'preflight_log_sha256'):
        r.require(isinstance(value[field], str) and retention.DIGEST.fullmatch(value[field]) is not None,
                  'Missing paperwork coverage digest')
    retained, current = value['retained_ids'], value['current_pass_ids']
    r.require(isinstance(retained, list) and retained == sorted(set(retained))
              and isinstance(current, list) and current == sorted(set(current))
              and not set(retained) & set(current), 'Repeated or contradictory paperwork coverage')
    union = sorted(set(retained) | set(current))
    r.require(union and all(name.startswith('operations:') and name.split(':', 1)[1].split('.')[0] in RULE_MODULES
                           for name in union)
              and canonical_digest(union) == value['inventory_sha256']
              and (inventory is None or set(union) == set(inventory)), 'Incomplete paperwork rule method census')


def final_coverage(api, value, environment, root):
    bind, pull, plan, inventory, retained, due = prepare(api, value, environment, root, 'final')
    from validation import source_gate_landing as landing
    run_id = r.positive(int(environment['GITHUB_RUN_ID'])); repo = api.get('')
    pointer = [line.removeprefix('Manual-Source-Gate: ') for line in pull['body'].splitlines()
               if line.startswith('Manual-Source-Gate: ')]
    r.require(pointer == [str(run_id)], 'Final paperwork requires its actual manual run pointer')
    jobs = api.pages(f'/actions/runs/{run_id}/jobs')
    job = landing.one_job(jobs, landing.JOB_NAMES['preflight'], run_id, repo['id'], repo['owner']['id'])
    r.require(job['status'] == 'success', 'Current preflight did not succeed')
    raw = api.bytes(f"/actions/jobs/{job['id']}/logs")
    header = landing.nextest.units.TASK_LINE.search(raw.decode()[:4096])
    r.require(header is not None and header[1] == 'preflight' and header[2] == 'workflow_dispatch',
              'Current preflight creator job differs')
    plan['evidence_sha256'] = value['python_unit_evidence']['sha256']
    observed = current_outcomes(raw, plan)
    r.require(due <= set(observed), 'Required current paperwork methods lack direct outcomes')
    coverage = {'version': 1, 'head': value['head'], 'base': value['base'], 'pr': value['pr'],
                'run': run_id, 'preflight_job': job['id'], 'evidence_sha256': plan['evidence_sha256'],
                'inventory_sha256': canonical_digest(sorted(inventory)), 'retained_ids': sorted(retained),
                'current_pass_ids': sorted(due), 'preflight_log_sha256': retention.digest(raw)}
    coverage_shape(coverage, bind, run_id, inventory)
    return coverage


def check(environment, body, base, root, *, hook=False):
    """None preserves the ordinary full-rule path; explicit modes fail closed."""
    mode = environment.get('PLURX_PAPERWORK_CONTINUATION')
    if mode is None or (mode == 'preliminary' and not any(
            line.startswith(PREFIX) for line in (body or '').splitlines())):
        return None
    r.require(mode in {'publication', 'preliminary', 'final'}, 'Unknown explicit paperwork continuation mode')
    api = retention.runner_api(environment)
    if mode == 'publication':
        r.require(hook, 'Local future-head publication is limited to pre-push')
        pr = r.positive(int(environment['PLURX_PAPERWORK_PUBLICATION_PR']))
        pull = api.get('/pulls/' + str(pr)); body = pull['body']
    value = enrollment(body)
    r.require(mode != 'publication' or value['pr'] == pr, 'Publication enrollment names another PR')
    r.require(value['base'] == base, 'Paperwork merge target differs from enrollment')
    if mode == 'final':
        return {'complete': True, 'coverage': final_coverage(api, value, environment, root), 'body': body}
    bind, pull, _, inventory, retained, due = prepare(api, value, environment, root, mode)
    if mode == 'preliminary' and not pull['draft']:
        from validation import source_gate_landing as landing
        pointers = [line.removeprefix('Manual-Source-Gate: ') for line in pull['body'].splitlines()
                    if line.startswith('Manual-Source-Gate: ')]
        r.require(len(pointers) == 1, 'Ready PR requires completed current source gate')
        landing.verify(api, value['pr'], value['head'], value['base'], root, r.positive(int(pointers[0])))
        return {'complete': True, 'body': body, 'retained': len(inventory), 'deferred': 0}
    return {'complete': False, 'body': body, 'retained': len(retained), 'deferred': len(due)}
