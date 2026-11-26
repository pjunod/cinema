"""Carry an authenticated nextest census across the reviewed native FEL source merge."""

import base64
import re

from validation import main_python_continuation as main_python
from validation import nextest_shard_continuation as nextest
from validation import paperwork_continuation as paperwork
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts


FORMAT = 'workspace-nextest-fel-base-bridge-v1'
PROFILE = 'validation/nextest-fel-base-projection.json'
PROFILE_SHA256 = '36ef3b189bd091f2832d921ea5b010cfecfab653eaa6747226ff1b18341da8be'
RUST_INPUTS = ('crates', 'vendor', 'Cargo.toml', 'Cargo.lock', 'build.rs', '.cargo', '.config',
               'rust-toolchain.toml', 'rustfmt.toml')

# The candidate Rust sources are fixed below. None of these named changes is
# consumed by that Rust execution. Everything else outside the Cargo trees,
# including bundled DV tools, other test fixtures and scripts, stays exact.
CONTROL_PATHS = frozenset({
    '.github/workflows/ci.yml', '.github/workflows/publish-release.yml', 'CHANGELOG.md',
    'scripts/release-package-candidate', 'tests/playback/rolling-producer-owners.toml',
    'tests/operations/test_contracts.py', 'tests/operations/test_release_publication.py',
    'tests/validation/test_nextest_base_bridge.py', 'tests/validation/test_selected_pr_jvm_discovery.py',
    'tests/validation/test_source_gate_repair.py',
    'validation/jvm_unit_discovery.py', 'validation/main_python_continuation.py',
    'validation/nextest_base_bridge.py', PROFILE, 'validation/paperwork_continuation.py',
    'validation/qualification.py', 'validation/source_gate_landing.py',
    'validation/source_gate_repair.py', 'validation/workspace_rust_continuation.py',
})



def file_digest(revision, path):
    raw = retention.git_bytes(revision, path)
    return retention.digest(raw) if raw is not None else None


def source_range(candidate, before):
    import subprocess
    raw = subprocess.check_output(['git', 'diff', '--name-only', '-z', before, candidate, '--', *RUST_INPUTS])
    return {filename.decode() for filename in raw.split(b'\x00') if filename}


def require_external_inputs(prior, candidate, reviewed_rust):
    external = retention.changed(prior, candidate) - set(reviewed_rust)
    receipts.require(external <= CONTROL_PATHS,
                     'Historical Rust external inputs changed: ' + ', '.join(sorted(external - CONTROL_PATHS)))


def current_projection(profile, candidate, base):
    prior = profile['source_before']
    receipts.require(base == profile['ci_current_main'] and paperwork.ancestor('.', base, candidate)
                     and paperwork.ancestor('.', prior, candidate)
                     and source_range(candidate, prior) == set(profile['changes']),
                     'Current Rust source differs beyond the exact reviewed native FEL merge')
    for path, record in profile['changes'].items():
        receipts.require(file_digest(prior, path) == record['before']
                         and file_digest(candidate, path) == record['after'],
                         'Reviewed Rust source input differs: ' + path)
    return nextest.source_selection(candidate, base)


def original_document(api, descriptor, old):
    receipts.require(set(descriptor) == {'sha256', 'attachment_id', 'attachment_uuid'},
                     'Invalid historical nextest descriptor')
    raw = retention.attachment_bytes(api, old['pull_request'], descriptor['attachment_id'],
                                     descriptor['attachment_uuid'], descriptor['sha256'])
    pull = api.get(f"/pulls/{old['pull_request']}")
    return paperwork.local_attestation(api, raw, descriptor, old, old['head_sha'], pull)


def upstream_linux_cases(profile, encoded, candidate):
    raw = base64.b64decode(encoded, validate=True)
    receipts.require(retention.digest(raw) == profile['upstream_linux_log_sha256'],
                     'Upstream native FEL execution log differs from the reviewed original')
    lines = raw.decode('utf-8', 'strict').splitlines()
    receipts.require(lines[0].startswith('ADMITTED ')
                     and receipts.bounded_json(lines[0].removeprefix('ADMITTED ').encode())['head'] ==
                         profile['upstream_linux_execution_commit']
                     and lines[1].startswith('Exact archive verified ' + profile['upstream_linux_execution_commit'] + ' ')
                     and 'rustc 1.97.1 (8bab26f4f 2026-07-14)' in lines,
                     'Upstream execution does not identify its exact compiler and archived source')
    receipts.require(main_python.main_ancestor(profile['upstream_linux_execution_commit'], profile['ci_current_main'])
                     and paperwork.ancestor('.', profile['ci_current_main'], candidate),
                     'Upstream Linux execution is outside the actual merged source ancestry')
    origin = profile['upstream_linux_execution_commit']
    for path, digest in profile['unchanged_upstream_case_sources'].items():
        receipts.require(file_digest(origin, path) == digest == file_digest(candidate, path),
                         'A directly exercised upstream Linux test input changed: ' + path)
    expected = set(profile['upstream_observed_former_ae_due_ids']) | set(profile['new_linux_only_ids'])
    actual = {'plurxd::bin/plurxd ' + name: outcome
              for name, outcome in retention.log_outcomes(raw.decode('utf-8', 'strict'), 'rust').items()}
    terminals = [line.split(' passed;', 1)[0] for line in lines if line.startswith('test result:')]
    receipts.require(actual == dict.fromkeys(expected, 'pass') and terminals == ['test result: ok. 7', 'test result: ok. 1'],
                     'Upstream native FEL receipt does not contain the expected eight real outcomes')
    return sorted(actual)


def authenticate(api, record, binding, candidate, repository, *, current_run=None):
    receipts.require(set(record) == {'version', 'repository', 'pr', 'branch', 'source_commit', 'base_sha',
                                     'prior_evidence', 'upstream_linux_log'}
                     and record['version'] == FORMAT and record['source_commit'] == candidate
                     and record['base_sha'] == binding['base_sha'] and record['repository'] == repository
                     and record['pr'] == binding['pull_request'] and record['branch'] == binding['head_ref']
                     and binding['schema'] == 2, 'Unknown nextest source bridge')
    from validation import workspace_rust_continuation as workspace
    workflow = retention.git_bytes(candidate, '.github/workflows/ci.yml')
    receipts.require(workflow is not None, 'Missing current native Rust workflow')
    workspace.check_workflow_environment(workflow.decode(), workspace.CURRENT_JOB)
    raw = retention.git_bytes(candidate, PROFILE)
    receipts.require(raw is not None and retention.digest(raw) == PROFILE_SHA256,
                     'Reviewed Rust source bridge projection bytes changed')
    profile = receipts.bounded_json(raw)
    selection = current_projection(profile, candidate, binding['base_sha'])
    prior = profile['source_before']
    require_external_inputs(prior, candidate, profile['changes'])
    old = {**binding, 'head_sha': prior, 'base_sha': profile['source_before_base']}
    original, repository = original_document(api, record['prior_evidence'], old)
    proof = nextest.authenticate(api, original, old, prior, repository, current_run=current_run)
    selected = sorted(name for name, row in selection['crates'].items() if row['due'])
    receipts.require(selected == proof['selected'], 'Nextest required crate scope changed in source bridge')
    newly_invalid = set(profile['additional_retained_ids_due'])
    previously_retained = set(proof['retained_ids'])
    receipts.require(newly_invalid <= previously_retained, 'Source bridge invalidates another historical census')
    supplemental = set(upstream_linux_cases(profile, record['upstream_linux_log'], candidate))
    newly_discoverable = set(profile['new_portable_ids']) | set(profile['new_linux_only_ids'])
    retained = (previously_retained - newly_invalid) | supplemental
    universe = set(proof['universe']) | supplemental
    proof.update(candidate=candidate, base=binding['base_sha'], selection_projection=selection['projection'],
                 retained_ids=sorted(retained), universe=sorted(universe),
                 must_pass_ids=sorted((set(proof['must_pass_ids']) | newly_invalid) - supplemental),
                 mandatory_new_ids=sorted((set(proof['mandatory_new_ids']) | newly_discoverable) - supplemental),
                 outcomes={**proof['outcomes'], **dict.fromkeys(supplemental, 'PASS')},
                 supplemental_origin={'source_commit': profile['upstream_linux_execution_commit'],
                                      'log_sha256': profile['upstream_linux_log_sha256'],
                                      'retained_ids': sorted(supplemental)})
    return proof
