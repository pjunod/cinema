"""Verify the Cinema promotion's remaining Python outcomes at their actual sources."""

import base64
import re
import subprocess

from validation import postmerge_unit_retention as postmerge
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import qualification_unit_continuation as commands

LANDING = 'c470d5aa033e84b4a2de1f8d36aa4180d62cd971'
EFFORT = '0d05ba03455b0ad67990acacb5e6551baaec9326'
REPAIR = 'a96f58b5fe87c20a46d01d0426e341adb7b97072'
CHECK_CONTROLLER = '2aa43d31802a5eec52b012b7a320314958d6ef12'
PYTHON_EVIDENCE = 'e75639ed83941e3be2aeca57c6a6c7e5c919ea5d91533445e149a11a8df887db'
COMPLETION = '2d0deec251a7ff20b5b302607d7615eda0dbf7ac385b6839b39f0377147a1292'
COMPLETION_UUID = '6c9eb14f-6966-48f9-978d-918672f3e8b2'


def native_job(api, run_id, job_id, workflow, event, commit, status, digest):
    run = api.get(f'/actions/runs/{run_id}')
    jobs = [j for j in api.pages(f'/actions/runs/{run_id}/jobs') if j['id'] == job_id]
    receipts.require(run['id'] == run_id and run['repository']['id'] == 1 and run['workflow_id'] == workflow
                     and run['trigger_event'] == event and run['commit_sha'] == commit
                     and len(jobs) == 1 and jobs[0]['run_id'] == run_id and jobs[0]['repo_id'] == 1
                     and jobs[0]['attempt'] == 1 and jobs[0]['status'] == status,
                     'Python completion run or job identity differs')
    raw = api.bytes(f'/actions/jobs/{job_id}/logs')
    receipts.require(retention.digest(raw) == digest, 'Python completion job log differs')
    return raw.decode()


def verify_python_completion(api, landing, scope):
    """Combine authenticated attachment 148's selection with the exact four observed dispositions."""
    require = receipts.require
    binding = postmerge.landing_binding(api, 1399, landing)
    require(landing == LANDING and binding['head_sha'] == EFFORT and scope['postmerge_binding'] == binding
            and scope['evidence_sha256'] == PYTHON_EVIDENCE and scope['source_commit'] == landing,
            'Unexpected Cinema Python selection')
    raw = retention.attachment_bytes(api, 1399, 150, COMPLETION_UUID, COMPLETION)
    envelope, _, _, attestations = retention.verify_identity(api, raw, COMPLETION, binding, landing, 150, COMPLETION_UUID)
    require(envelope['kind'] == 'landed-python-completion-v1' and envelope['repair_source'] == REPAIR,
            'Wrong Python completion envelope')
    part = receipts.bounded_json(base64.b64decode(envelope['partition_b64'], validate=True))
    repair = receipts.bounded_json(base64.b64decode(envelope['repair_binding_b64'], validate=True))
    repair_log = base64.b64decode(envelope['repair_log_b64'], validate=True)
    old = native_job(api, 6810, 64151, 'main-fast-lane.yml', 'pull_request',
                     EFFORT, 'failure', part['job64151_sha256'])
    paper = native_job(api, 6810, 64148, 'main-fast-lane.yml', 'pull_request',
                       EFFORT, 'success', part['job64148_sha256'])
    current = native_job(api, 6824, 64205, 'cinema-landed-main-python.yml', 'workflow_dispatch',
                         CHECK_CONTROLLER, 'success', part['landed_execution']['raw_sha256'])
    observed = set()
    for name, row in part['covered'].items():
        if row['job'] == 64151:
            require('(' + name.split('.', 2)[2] + ') ... ok' in old, 'Prior named Python pass missing')
        else:
            messages = {'test_unlisted_docs_fit_the_budget': 'docs stay under 40 KB, and the shrink-only list only shrinks',
                        'test_no_status_page_calls_a_merged_pull_request_open': 'no status page calls a merged pull request open'}
            require(row['job'] == 64148 and '[ ok ] ' + messages[name.rsplit('.', 1)[1]] in paper,
                    'Prior source-equivalent paperwork module missing')
        observed.add(name)
    passed = set(re.findall(r'(?m)^\S+ test_\w+ \((tests\.(?:validation|operations)\.[\w.]+)\) \.\.\. ok$', current))
    require(passed == set(part['outstanding_on_main']) and len(passed) == 9 and not passed & observed
            and re.search(r'(?m)^\S+ Ran 9 tests in [0-9.]+s$', current)
            and re.search(r'(?m)^\S+ OK$', current), 'Native residual Python completion differs')
    failed, = part['failed_at_landing']
    argv = repair['argv']
    require('(' + failed.split('.', 2)[2] + ') ... FAIL' in old and repair['source_commit'] == REPAIR
            and repair['base_source_commit'] == landing and repair['exit_code'] == 0
            and repair['source_clean_before'] and repair['source_file_stable']
            and argv[1:] == ['-m', 'unittest', '-v', failed]
            and repair['log_sha256'] == retention.digest(repair_log)
            and ('(' + failed + ') ... ok').encode() in repair_log and repair_log.endswith(b'\nOK\n')
            and repair['source_archive_sha256'] == commands.source_archive_digest(REPAIR)
            and subprocess.check_output(['git', 'diff', '--name-only', landing, REPAIR], text=True).splitlines()
            == [repair['changed_path']] == ['tests/operations/test_contracts.py']
            and retention.digest(retention.git_bytes(REPAIR, repair['changed_path'])) == repair['file_sha256'],
            'Original failing assertion or its measured corrected-source pass differs')
    selected = {'tests.' + suite + '.' + name for suite, plan in scope['suites'].items() for name in plan['uncovered']}
    require(failed not in observed | passed and observed | passed | {failed} == selected
            and len(observed) == 10 and len(selected) == 20, 'Incomplete or overlapping Python dispositions')
    return {'landing': landing, 'prior_passing_or_equivalent': len(observed), 'native_residual_passes': len(passed),
            'failed_at_landing': failed, 'passed_on_repair': REPAIR, 'attestations': attestations}
