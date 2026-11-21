"""Read-only authentication of an explicit manual PR source gate for landing.

No commit status alias is created. Release publishers continue to require
an effort/integration qualification; this receipt only admits a PR merge.
"""
from __future__ import annotations
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import zipfile

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r
from validation import qualification
from validation import nextest_shard_continuation as nextest

WORKFLOW_SHA256 = '33f5e34f9805e81fa788178110f0c06dc5bfafcac8b6d506a6c6bf490591dfae'
JOB_NAMES = {
    'scope': 'validation scope', 'mobile_version': 'mobile release version',
    'preflight': 'fast policy and contract preflight', 'rust': 'Rust unit and SQLite contracts',
    'cluster_store': 'replicated Store contracts', 'cluster_topology': 'replicated topology contracts',
    'cluster_transport_recovery': 'cluster transport recovery campaign',
    'cluster_wal': 'replicated WAL recovery contracts', 'cluster_daemon': 'cluster daemon contracts',
    'web_layout': 'web layout and accessibility', 'vod_web': 'film-addressed VOD browser acceptance',
    'android_jvm': 'Android JVM tests and lint', 'apple': 'Apple iOS + tvOS tests',
    'android_device': 'Android instrumented UI tests',
}



def semantic_pr_title(title):
    """Only the forge's draft-control prefix is excluded from title identity."""
    r.require(isinstance(title, str), 'Missing source-gate PR title')
    return re.sub(r'^\s*(?:WIP:|\[WIP\])\s*', '', title, count=1, flags=re.I)



def expected_paperwork(api, pull, binding, environment, root):
    from validation import paperwork_continuation as pc

    body = pull.get('body') or ''
    record = {'version': 1, 'head': binding['head_sha'], 'base': binding['base_sha'],
              'pr': binding['pull_request'], 'passed': True,
              'body_sha256': retention.digest(body.encode()),
              'title_sha256': retention.digest(semantic_pr_title(pull['title']).encode())}
    if pc.is_enrolled(body):
        record.update(version=2, unit_coverage=pc.final_coverage(
            api, pc.enrollment(body), environment, root))
    return record


WORKER_REQUIRED_PATHS = (
    'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'crates/plurxd/Cargo.toml',
    'crates/plurxd/src/library_work.rs', 'crates/plurxd/src/library_work/env.rs',
    'scripts/check-worker-control', 'validation/worker_control_resume.py',
    'tests/operations/test_worker_control_runner.py',
)


def worker_input_identity(tree):
    governed = set(WORKER_REQUIRED_PATHS)
    governed.update(path for path in tree.entries if path.startswith(
        ('crates/plurx-worker-control/', 'verification/worker-control/')))
    r.require(all(path in tree.entries for path in governed), 'Missing governed worker proof input')
    return {path: tree.entries[path] for path in sorted(governed)}


def require_unchanged_worker_inputs(candidate, base, prior, root):
    trees = [nextest.units.Tree(revision, root) for revision in (candidate, base, prior)]
    identities = [worker_input_identity(tree) for tree in trees]
    r.require(identities[0] == identities[1] == identities[2],
              'Worker controller proof source changed and requires actual current qualification')
    recipes = [nextest.projected_main_workflow(tree.text('.github/workflows/main-fast-lane.yml').encode())
               for tree in trees]
    r.require(recipes[0] == recipes[1] == recipes[2], 'Worker proof invocation recipe changed')
    return retention.digest(json.dumps(identities[0], sort_keys=True).encode())


def one_job(jobs, name, run, repository, owner):
    found = [job for job in jobs if job['name'] == name]
    r.require(len(found) == 1, 'Missing or ambiguous required source-gate surface: ' + name)
    job = found[0]
    r.require(job['run_id'] == run and job['repo_id'] == repository and job['owner_id'] == owner
              and type(job['attempt']) is int and job['attempt'] == 1,
              'Source-gate surface execution identity differs')
    return job


def uploaded_archive(api, artifacts, name, job, task):
    """Bind the exact archive bytes and ID to its actual creator's job log."""
    rows = [row for row in artifacts if row['name'] == name]
    r.require(len(rows) == 1, 'Missing or ambiguous source-gate artifact: ' + name)
    item = rows[0]
    path = f"/actions/artifacts/{r.positive(item['id'])}/zip"
    r.require(item['run_id'] == job['run_id'] and item['expired'] is False
              and 0 < item['size_in_bytes'] <= r.MAX_BYTES and item['archive_download_url'] == api.root + path,
              'Source-gate artifact metadata differs')
    log = api.bytes(f"/actions/jobs/{r.positive(job['id'])}/logs").decode('utf-8', 'strict')
    header = nextest.units.TASK_LINE.search(log[:4096])
    r.require(header is not None and header[1] == task and header[2] == 'workflow_dispatch',
              'Source-gate artifact has no original creator job')
    text = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', log, flags=re.M)
    pattern = (r'^Artifact ' + re.escape(name) + r' has been successfully uploaded! '
               r'Final size is ([0-9]+) bytes\. Artifact ID is ([0-9]+)$')
    uploads = list(re.finditer(pattern, text, re.M))
    r.require(len(uploads) == 1 and int(uploads[0][1]) == item['size_in_bytes']
              and int(uploads[0][2]) == item['id'], 'Artifact was not published by the required job')
    hashes = re.findall(r'^SHA256 hash of uploaded artifact zip is ([0-9a-f]{64})$',
                        text[max(0, uploads[0].start() - 4096):uploads[0].start()], re.M)
    r.require(hashes, 'Artifact creator did not record its archive digest')
    raw = api.bytes(path)
    r.require(len(raw) == item['size_in_bytes'] and retention.digest(raw) == hashes[-1],
              'Artifact archive differs from original creator bytes')
    return raw


def uploaded(api, artifacts, name, job, task, members):
    raw = uploaded_archive(api, artifacts, name, job, task)
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        r.require(len(entries) == len(members) and {entry.filename for entry in entries} == set(members)
                  and all(not entry.is_dir() and 0 < entry.file_size <= r.MAX_BYTES for entry in entries)
                  and sum(entry.file_size for entry in entries) <= r.MAX_BYTES,
                  'Unknown or oversized source-gate archive members')
        values = {entry.filename: r.bounded_json(archive.read(entry)) for entry in entries}
        values['__sha256__'] = {entry.filename: retention.digest(archive.read(entry)) for entry in entries}
        return values


def require_surfaces(jobs, record, run, repository, owner, retained):
    r.require(set(record['jobs']) == qualification.REQUIRED_JOBS, 'Missing required source-gate surfaces')
    for surface, name in JOB_NAMES.items():
        job = one_job(jobs, name, run, repository, owner)
        expected = record['jobs'][surface]
        r.require(job['status'] == expected, 'Recorded source-gate surface status differs from actual job')
        r.require(expected == 'success' or (expected == 'skipped' and retained is not None
                  and surface in retained['lanes'] and surface != 'rust'),
                  'Required source-gate surface lacks applicable successful evidence')
    packages = [job for job in jobs if re.fullmatch(r'package and smoke \((amd64|arm64)\)(?: \([^\n]*\))?', job['name'])]
    r.require(len(packages) == 2 and {re.match(r'package and smoke \(([^)]+)\)', j['name'])[1]
                                    for j in packages} == {'amd64', 'arm64'},
              'Both native package surfaces are required')
    r.require(record['jobs']['package_smoke'] == 'success' and all(
              j['status'] == 'success' and j['run_id'] == run and j['repo_id'] == repository
              and j['owner_id'] == owner and j['attempt'] == 1 for j in packages),
              'Native package surface did not actually succeed')


def completion_census(proof, completion, listing, config, listing_sha256):
    current, due, _ = nextest.current_census(proof, listing, config)
    r.require(set(completion) == {'version', 'kind', 'candidate', 'base', 'source_run',
              'enrollment_digest', 'selection_projection', 'selected', 'retained', 'executed', 'current_listing_sha256',
              'current_universe', 'historical_retired_passes', 'complete'}, 'Unknown source-gate completion fields')
    r.require(completion['version'] == nextest.POLICY and completion['kind'] == 'selected-pr-rust-source-gate'
              and completion['candidate'] == proof['candidate'] and completion['base'] == proof['base']
              and completion['historical_retired_passes'] == proof.get('retired_ids', {})
              and completion['source_run'] == proof['run'] and completion['enrollment_digest'] == proof['digest']
              and completion['selection_projection'] == proof['selection_projection']
              and completion['selected'] == proof['selected'] and completion['retained'] == proof['retained_ids']
              and completion['executed'] == sorted(due) and completion['current_universe'] == sorted(current)
              and completion['current_listing_sha256'] == listing_sha256
              and completion['complete'] is True,
              'Source-gate completion is incomplete, wrong-scope or contradicts authenticated coverage')
    return current, due


def verify(api, number, head, main, root, run_id):
    repo = api.get('')
    repository, owner = r.positive(repo['id']), r.positive(repo['owner']['id'])
    pull = api.get(f'/pulls/{number}')
    run = api.get(f'/actions/runs/{r.positive(run_id)}')
    if run['workflow_id'] == 'source-gate-repair.yml':
        from validation import source_gate_repair
        return source_gate_repair.verify(api, number, head, main, root, run_id)
    event = r.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    branch = pull['head']['ref']
    r.require(run['id'] == run_id and run['workflow_id'] == 'ci.yml' and run['status'] == 'success'
              and run['repository']['id'] == repository and run['trigger_event'] == 'workflow_dispatch'
              and run['commit_sha'] == head and run['prettyref'] == branch
              and event['ref'] == 'refs/heads/' + branch and inputs['promotion_pr'] == str(number)
              and inputs['promotion_head_sha'] == head and inputs['promotion_base_sha'] == main,
              'Actual manual source-gate run does not bind this PR, head and current main')
    r.require(not qualification.promotion_ref(branch), 'This alternate route admits ordinary PR source gates only')
    r.require(api.get('/branches/main')['commit']['id'] == main
              and [row['commit']['id'] for row in api.pages('/branches') if row['name'] == branch] == [head],
              'Source-gate main or branch moved')
    r.require(retention.digest(retention.git_bytes(head, '.github/workflows/ci.yml')) == WORKFLOW_SHA256,
              'Unreviewed manual source-gate workflow')
    environment = dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1',
        GITHUB_REPOSITORY=repo['full_name'], PLURX_PROMOTION_PR=str(number),
        PLURX_PROMOTION_HEAD_SHA=head, PLURX_PROMOTION_BASE_SHA=main,
        GITHUB_SHA=head, GITHUB_REF='refs/heads/' + branch)
    ancestor = subprocess.run(['git', '-C', str(root), 'merge-base', '--is-ancestor', main, head],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
    binding = qualification.manual_binding(environment, pull, head, main, head, ancestor)
    r.require(binding['schema'] == 2, 'Release or full-workspace qualification cannot supply a PR source gate')
    jobs = api.pages(f'/actions/runs/{run_id}/jobs')
    gate = one_job(jobs, 'Main promotion gate', run_id, repository, owner)
    rust = one_job(jobs, JOB_NAMES['rust'], run_id, repository, owner)
    r.require(gate['status'] == rust['status'] == 'success', 'Actual source-gate jobs did not succeed')
    artifacts = api.pages(f'/actions/runs/{run_id}/artifacts')
    record = uploaded(api, artifacts, f'effort-qualification-{number}', gate, 'pr_gate',
                      {'qualification-receipt.json'})['qualification-receipt.json']
    r.require(record['kind'] == 'selected-pr-source-gate' and record['binding_mode'] == 'authenticated-manual-promotion',
              'Wrong qualification scope or forged release evidence')
    descriptor = r.bounded_json(inputs['workspace_rust_evidence'].encode())
    from validation import workspace_rust_continuation as workspace
    proof = workspace.authenticate(api, descriptor, binding, head, current_run=run_id)
    r.require(proof.get('provider') == nextest.POLICY, 'Source-gate Rust evidence uses another provider')
    members = uploaded(api, artifacts, f'selected-pr-rust-source-gate-{run_id}', rust, 'check',
                       {'receipt.json', 'listing.json'})
    completion = members['receipt.json']
    # The JSON member's raw digest is checked from its original archive too.
    config = nextest.units.Config.load(nextest.units.Tree(head, root).text(str(nextest.units.CONFIG)))
    completion_census(proof, completion, members['listing.json'], config, members['__sha256__']['listing.json'])
    retained = record['retained_unit_evidence']
    if retained is not None:
        raw = retention.attachment_bytes(api, number, retained['attachment_id'], retained['attachment_uuid'], retained['sha256'])
        refreshed = retention.verify_enrollment(api, raw, retained['sha256'], binding, head,
                    retained['attachment_id'], retained['attachment_uuid'])
        r.require(refreshed == retained, 'Required surface retention changed or is not authenticated')
        environment['PLURX_RETAINED_UNITS'] = json.dumps(refreshed)
    require_surfaces(jobs, record, run_id, repository, owner, retained)
    # Full ci.yml covers compiled web/native clients and their tests. The one
    # fast-lane-only worker proof requires identical complete governed inputs
    # against both current main and its actual authenticated origin.
    worker_inputs_sha256 = require_unchanged_worker_inputs(head, main, proof['prior'], root)
    previous_jobs = api.pages(f"/actions/runs/{proof['run']}/jobs")
    worker = one_job(previous_jobs, 'production worker controller proof', proof['run'], repository, owner)
    r.require(worker['status'] == 'success', 'Missing applicable production worker controller proof')
    worker_log = api.bytes(f"/actions/jobs/{worker['id']}/logs").decode()
    worker_task = nextest.units.TASK_LINE.search(worker_log[:4096])
    r.require(worker_task is not None and worker_task[1] == 'worker_control_proof'
              and worker_task[2] == 'pull_request', 'Worker proof execution identity differs')
    paperwork = record['source_gate_paperwork']
    paperwork_env = {**environment, 'GITHUB_RUN_ID': str(run_id)}
    r.require(paperwork == expected_paperwork(api, pull, binding, paperwork_env, root),
              'Current PR paperwork/body/title evidence differs')
    environment.update(GITHUB_RUN_ID=str(run_id),
        GITHUB_WORKFLOW_REF=repo['full_name'] + '/.github/workflows/ci.yml@refs/heads/' + branch,
        PLURX_PROMOTION_BINDING=json.dumps(binding), PLURX_SOURCE_GATE_PAPERWORK=json.dumps(paperwork))
    tree = subprocess.check_output(['git', '-C', str(root), 'rev-parse', head + '^{tree}'], text=True).strip()
    r.require(qualification.build_receipt(environment, record['jobs'], head, tree) == record,
              'Source-gate qualification record contradicts actual provenance')
    return {'run': run_id, 'job': gate['id'], 'base': main, 'head': head,
            'worker_inputs_sha256': worker_inputs_sha256,
            'url': api.root.split('/api/v1/repos/')[0] + '/' + repo['full_name'] + '/actions/runs/' + str(run_id)}


def paperwork():
    binding = r.bounded_json(os.environ['PROMOTION_BINDING'].encode())
    if binding['schema'] != 2:
        return
    api = retention.runner_api(os.environ)
    pull = api.get(f"/pulls/{binding['pull_request']}")
    r.require(pull['head']['sha'] == binding['head_sha'] and pull['head']['ref'] == binding['head_ref']
              and pull['base']['sha'] == binding['base_sha'] and pull['base']['ref'] == 'main'
              and pull['state'] == 'open' and not pull['merged'], 'Paperwork PR identity moved')
    directory = Path('target/validation/source-gate-paperwork')
    directory.mkdir(parents=True, exist_ok=False)
    event = directory / 'event.json'
    event.write_text(json.dumps({'pull_request': pull}))
    env = {**os.environ, 'GITHUB_EVENT_PATH': str(event.resolve()), 'PR_HEAD_SHA': binding['head_sha']}
    env.pop('PR_BODY', None)
    env.pop('PR_TITLE', None)
    from validation import paperwork_continuation as pc
    enrolled = pc.is_enrolled(pull.get('body'))
    env.pop('PLURX_PAPERWORK_CONTINUATION', None)
    env.pop('PLURX_PAPERWORK_COVERAGE_OUTPUT', None)
    if enrolled:
        env['PLURX_PAPERWORK_CONTINUATION'] = 'final'
        coverage_path = directory / 'unit-coverage.json'
        env['PLURX_PAPERWORK_COVERAGE_OUTPUT'] = str(coverage_path.resolve())
    subprocess.run([sys.executable, 'scripts/paperwork', '--base', binding['base_sha'],
                    '--merge-target', binding['base_sha'], '--body-env'], env=env, check=True)
    record = {'version': 1, 'head': binding['head_sha'], 'base': binding['base_sha'],
              'pr': binding['pull_request'], 'passed': True,
              'body_sha256': retention.digest((pull.get('body') or '').encode()),
              'title_sha256': retention.digest(semantic_pr_title(pull['title']).encode())}
    if enrolled:
        coverage = r.bounded_json(coverage_path.read_bytes())
        pc.coverage_shape(coverage, binding, int(os.environ['GITHUB_RUN_ID']))
        record.update(version=2, unit_coverage=coverage)
    with open(os.environ['GITHUB_ENV'], 'a') as stream:
        print('PLURX_SOURCE_GATE_PAPERWORK=' + json.dumps(record, separators=(',', ':')), file=stream)


if __name__ == '__main__':
    r.require(sys.argv[1:] == ['paperwork'], 'Unknown source-gate operation')
    paperwork()
