"""Authenticate a completed promotion before publishing the identical merged tree."""
from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import re
import zipfile

from validation import python_unit_receipts as receipts
from validation import qualification


def native_receipt(api, run_id, pr, expected_digest):
    require = receipts.require
    artifacts = api.pages(f'/actions/runs/{run_id}/artifacts')
    matches = [row for row in artifacts if row['name'] == f'effort-qualification-{pr}']
    require(len(matches) == 1, 'Missing or ambiguous native qualification artifact')
    artifact = matches[0]
    artifact_id = receipts.positive(artifact['id'])
    path = f'/actions/artifacts/{artifact_id}/zip'
    require(artifact['run_id'] == run_id and artifact['expired'] is False
            and artifact['archive_download_url'] == api.root + path
            and 0 < artifact['size_in_bytes'] <= receipts.MAX_BYTES,
            'Invalid native qualification archive identity or size')
    archive_bytes = api.bytes(path)
    require(len(archive_bytes) == artifact['size_in_bytes'], 'Truncated qualification archive')
    if artifact.get('digest'):
        require(artifact['digest'] == 'sha256:' + hashlib.sha256(archive_bytes).hexdigest(),
                'Native qualification archive digest differs')
    with zipfile.ZipFile(io.BytesIO(archive_bytes)) as archive:
        entries = archive.infolist()
        require(len(entries) == 1 and entries[0].filename == 'qualification-receipt.json'
                and 0 < entries[0].file_size <= receipts.MAX_BYTES,
                'Unknown qualification archive contents')
        raw = archive.read(entries[0])
    require(hashlib.sha256(raw).hexdigest() == expected_digest,
            'Qualification receipt differs from the reviewed promotion digest')
    return receipts.bounded_json(raw)


def verify(api, env, repository=Path('.')):
    require = receipts.require
    require(env['GITHUB_EVENT_NAME'] == 'workflow_dispatch' and env['GITHUB_REF'] == 'refs/heads/main',
            'Publication requires manual dispatch on main')
    pr = receipts.positive(int(env['QUALIFICATION_PR']))
    run_id = receipts.positive(int(env['QUALIFICATION_RUN']))
    digest = env['QUALIFICATION_RECEIPT_SHA256']
    require(re.fullmatch('[0-9a-f]{64}', digest), 'Invalid qualification receipt digest')
    landing = receipts.sha(env['GITHUB_SHA'])
    repo = api.get('')
    repo_id = receipts.positive(repo['id'])
    slug = env['GITHUB_REPOSITORY']
    require(repo['full_name'] == slug and qualification.git_object(repository, 'HEAD') == landing
            and api.get('/branches/main')['commit']['id'] == landing,
            'Publication checkout must be the current main tip')
    pull = api.get(f'/pulls/{pr}')
    require(pull['number'] == pr and pull['state'] == 'closed' and pull['merged'] is True
            and pull['merge_commit_sha'] == landing and pull['base']['ref'] == 'main'
            and pull['base']['repo']['id'] == pull['head']['repo']['id'] == repo_id,
            'Merged promotion PR does not own this main commit')
    head = qualification.git_object(repository, 'HEAD^2')
    base = qualification.git_object(repository, 'HEAD^1')
    tree = qualification.git_object(repository, 'HEAD^{tree}')
    branch = pull['head']['ref']
    require(qualification.promotion_ref(branch) and pull['head']['sha'] == head
            and qualification.git_object(repository, 'HEAD^2^{tree}') == tree,
            'Merged main tree differs from the promotion head')
    run = api.get(f'/actions/runs/{run_id}')
    require(run['id'] == run_id and run['status'] == 'success' and run['workflow_id'] == 'ci.yml'
            and run['trigger_event'] == 'workflow_dispatch' and run['repository']['id'] == repo_id
            and run['commit_sha'] == head and run['prettyref'] == branch,
            'Qualification workflow did not succeed on the merged source')
    event = receipts.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    require(event['ref'] == 'refs/heads/' + branch and inputs['promotion_pr'] == str(pr)
            and inputs['promotion_head_sha'] == head and inputs['promotion_base_sha'] == base,
            'Qualification dispatch does not bind this PR and both merge parents')
    gates = [job for job in api.pages(f'/actions/runs/{run_id}/jobs') if job['name'] == 'Main promotion gate']
    require(len(gates) == 1 and gates[0]['status'] == 'success' and gates[0]['attempt'] == 1
            and gates[0]['run_id'] == run_id and gates[0]['repo_id'] == repo_id,
            'Authenticated first-attempt Main promotion gate success is missing')
    record = native_receipt(api, run_id, pr, digest)
    binding = dict(schema=1, repository=slug, pull_request=pr, base_sha=base, head_sha=head,
                   base_ref='main', head_ref=branch)
    qualification_env = dict(GITHUB_REPOSITORY=slug, GITHUB_EVENT_NAME='workflow_dispatch',
                             GITHUB_RUN_ID=str(run_id), GITHUB_RUN_ATTEMPT='1', GITHUB_SHA=head,
                             GITHUB_WORKFLOW_REF=record['workflow_ref'], PLURX_PROMOTION_PR=str(pr),
                             PLURX_PROMOTION_HEAD_SHA=head, PLURX_PROMOTION_BASE_SHA=base,
                             PLURX_PROMOTION_BINDING=json.dumps(binding))
    if record['retained_unit_evidence'] is not None:
        qualification_env['PLURX_RETAINED_UNITS'] = json.dumps(record['retained_unit_evidence'])
    expected = qualification.build_receipt(qualification_env, record['jobs'], head, tree)
    require(record == expected, 'Native qualification receipt does not describe this merge')
    require(api.get('/branches/main')['commit']['id'] == landing, 'Main moved during qualification verification')
    return landing


if __name__ == '__main__':
    client = receipts.API(os.environ['GITHUB_API_URL'], os.environ['GITHUB_REPOSITORY'], os.environ['GITHUB_TOKEN'])
    print('Verified qualified main commit', verify(client, os.environ))
