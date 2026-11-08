"""Select unfinished unit work for a promotion already merged into current main."""

import argparse
import os
import subprocess
from pathlib import Path

from validation import native_python_execute, promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import qualification, workspace_rust_continuation


def landing_binding(api, number, landing, repository=Path('.')):
    """Read the actual merge, including its original base rather than the moving PR base."""
    number, landing = receipts.positive(number), receipts.sha(landing)
    repo = api.get('')
    pull = api.get(f'/pulls/{number}')
    require = receipts.require
    status = subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=normal'],
                                     cwd=repository, text=True)
    require(not status, 'Post-merge selection requires a clean checkout')
    main = receipts.sha(api.get('/branches/main')['commit']['id'])
    if main != landing and subprocess.run(['git', 'cat-file', '-e', main + '^{commit}'],
                                          cwd=repository, capture_output=True).returncode:
        suffix = '/api/v1/repos/' + repo['full_name']
        require(api.root.endswith(suffix), 'Cannot resolve history at this API origin')
        qualification._ensure_task_history_tips(
            {'GITHUB_SERVER_URL': api.root.removesuffix(suffix), 'GITHUB_REPOSITORY': repo['full_name'],
             'GITHUB_TOKEN': api.token}, repository, (landing, main))
    require(qualification.git_object(repository, 'HEAD') == landing
            and subprocess.run(['git', 'merge-base', '--is-ancestor', landing, main], cwd=repository).returncode == 0,
            'Checked-out landing is outside current main history')
    require(pull['number'] == number and pull['state'] == 'closed' and pull['merged'] is True
            and pull['merge_commit_sha'] == landing and pull['base']['ref'] == 'main'
            and qualification.promotion_ref(pull['head']['ref'])
            and pull['head']['repo']['id'] == pull['base']['repo']['id'] == receipts.positive(repo['id']),
            'Closed same-repository promotion does not own this main merge')
    base, head = receipts.sha(pull['merge_base']), receipts.sha(pull['head']['sha'])
    parents = subprocess.check_output(
        ['git', 'show', '-s', '--format=%P', landing], cwd=repository, text=True).split()
    require(parents == [base, head] and qualification.git_object(repository, head + '^{tree}')
            == qualification.git_object(repository, landing + '^{tree}')
            and subprocess.run(['git', 'merge-base', '--is-ancestor', base, head], cwd=repository).returncode == 0,
            'Main merge parents, promotion ancestry or tree differ')
    return {'schema': 1, 'repository': repo['full_name'], 'pull_request': number,
            'head_ref': pull['head']['ref'], 'head_sha': head, 'base_ref': 'main',
            'base_sha': base, 'landing_sha': landing}


def select(api, kind, descriptor, number, landing):
    binding = landing_binding(api, number, landing)
    head = binding['head_sha']
    receipts.require(isinstance(descriptor, dict)
                     and set(descriptor) == {'sha256', 'attachment_id', 'attachment_uuid'},
                     'Expected one enrolled unit evidence attachment')
    if kind == 'python':
        plan = native_python_execute.prepare(api, descriptor, binding, head)
    elif kind == 'rust':
        plan = workspace_rust_continuation.authenticate(api, descriptor, binding, head)
    else:
        receipts.require(kind == 'units', 'Unknown post-merge unit evidence type')
        attachment = receipts.positive(descriptor['attachment_id'])
        uuid, digest = descriptor['attachment_uuid'], descriptor['sha256']
        raw = retention.attachment_bytes(api, number, attachment, uuid, digest)
        plan = retention.verify_enrollment(api, raw, digest, binding, head, attachment, uuid)
    if kind == 'units':
        return {'postmerge_binding': binding, 'historical_retention': plan}
    source_key = 'candidate' if kind == 'rust' else 'source_commit'
    receipts.require(plan[source_key] == head, 'Verified selector source differs from the merge head')
    return {**plan, source_key: landing, 'postmerge_binding': binding}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kind', choices=('python', 'rust', 'units'))
    parser.add_argument('--pull', type=int, required=True)
    parser.add_argument('--landing', required=True)
    parser.add_argument('--descriptor', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    plan = select(retention.runner_api(os.environ), args.kind,
                  receipts.bounded_json(args.descriptor.read_bytes()), args.pull, args.landing)
    receipts.atomic_json(args.out, plan)


if __name__ == '__main__':
    main()
