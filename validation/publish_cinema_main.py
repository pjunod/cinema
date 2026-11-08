"""Publish the Cinema main release after its authentic split-run checks finish.

This bounded recovery controller retains successful unit execution instead of
claiming that an overall failed historical workflow passed. Each outstanding
runtime lane must independently finish successfully on its reviewed controller.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess

from validation import postmerge_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import qualification

LANDING = 'c470d5aa033e84b4a2de1f8d36aa4180d62cd971'
UNITS = {'sha256': '8d53a0fec705f9e060e5a32c65a833a5c9bd4a02af1eb0a790491732b0b4c466',
         'attachment_id': 149, 'attachment_uuid': '31fc459d-0c43-484b-8696-6a61ae0ece4e'}
# Filled only after reviewing the final package fix and terminal lane records.
REVIEWED_BUILD_TREE = None
CONTROL_PATHS = frozenset({
    '.github/workflows/cinema-main-publish.yml',
    'validation/publish_cinema_main.py',
})
JOBS = None
AMD_IMAGE_ID = None


def git(repository, *args):
    return subprocess.check_output(['git', *args], cwd=repository, text=True).strip()


def build_tree(repository):
    raw = subprocess.check_output(['git', 'ls-tree', '-rz', '--full-tree', 'HEAD'], cwd=repository)
    entries = []
    for entry in raw.split(b'\0'):
        if entry:
            path = entry.split(b'\t', 1)[1].decode('utf-8')
            if path not in CONTROL_PATHS:
                entries.append(entry)
    return hashlib.sha256(b'\0'.join(entries) + b'\0').hexdigest()


def require_main_commit(repository, source, current):
    receipts.require(source in git(repository, 'rev-list', '--first-parent', current).splitlines()
                     and subprocess.run(['git', 'merge-base', '--is-ancestor', LANDING, source],
                                        cwd=repository, capture_output=True).returncode == 0,
                     'Publication source is not a main commit containing the Cinema landing')


def successful_job(api, repository_id, expected):
    """Authenticate one job, without substituting its parent run's conclusion."""
    run_id, job_id, name, workflow, controller = expected
    require = receipts.require
    run = api.get(f'/actions/runs/{run_id}')
    require(run['id'] == run_id and run['repository']['id'] == repository_id
            and run['workflow_id'] == workflow and run['commit_sha'] == controller
            and run['trigger_event'] == 'workflow_dispatch',
            f'Unexpected controller for required lane {name}')
    jobs = api.pages(f'/actions/runs/{run_id}/jobs')
    matches = [job for job in jobs if job['id'] == job_id]
    require(len(matches) == 1, f'Missing or ambiguous required job {name}')
    job = matches[0]
    require(job['name'] == name and job['run_id'] == run_id
            and job['repo_id'] == repository_id and job['attempt'] == 1
            and job['status'] == 'success' and job['task_id'] > 0,
            f'Required job has not succeeded: {name}')
    return {'run_id': run_id, 'job_id': job_id, 'name': name,
            'controller': controller, 'status': 'success'}


def verify(api, env, source, landing):
    """Refuse publication until source, retained outcomes and every lane agree."""
    require = receipts.require
    require(env['GITHUB_EVENT_NAME'] == 'workflow_dispatch'
            and env['GITHUB_REF'] == 'refs/heads/main', 'Publisher must run from main')
    require(REVIEWED_BUILD_TREE is not None and JOBS and AMD_IMAGE_ID,
            'Final reviewed publication binding has not been installed')
    source_sha = receipts.sha(env['CINEMA_SOURCE_SHA'])
    require(git(source, 'rev-parse', 'HEAD') == source_sha
            and build_tree(source) == REVIEWED_BUILD_TREE
            and not git(source, 'status', '--porcelain', '--untracked-files=normal'),
            'Publication source differs from the reviewed clean main tree')
    current = receipts.sha(api.get('/branches/main')['commit']['id'])
    qualification._ensure_task_history_tips(env, source, (source_sha, current))
    require_main_commit(source, source_sha, current)
    repository = api.get('')
    require(repository['full_name'] == env['GITHUB_REPOSITORY'], 'Repository identity differs')
    repo_id = receipts.positive(repository['id'])
    previous = Path.cwd()
    try:
        os.chdir(landing)
        retained = retention.select(api, 'units', UNITS, 1399, LANDING)
    finally:
        os.chdir(previous)
    outcomes = [successful_job(api, repo_id, expected) for expected in JOBS]
    # Python disposition and native nonunit receipts are added here only after
    # their authenticated terminal records have been reviewed.
    require(False, 'Python and nonunit terminal evidence not installed')
    return {'source': source_sha, 'tree': git(source, 'rev-parse', 'HEAD^{tree}'),
            'retained_units': retained, 'jobs': outcomes}


def main():
    api = receipts.API(os.environ['GITHUB_API_URL'], os.environ['GITHUB_REPOSITORY'],
                       os.environ['GITHUB_TOKEN'])
    record = verify(api, os.environ, Path('source').resolve(), Path('landing').resolve())
    receipts.atomic_json(Path(os.environ['RUNNER_TEMP']) / 'cinema-publication.json', record)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        print('image_id=' + AMD_IMAGE_ID, file=output)
    print('Verified actual main source and every required Cinema check', record['source'])


if __name__ == '__main__':
    main()
