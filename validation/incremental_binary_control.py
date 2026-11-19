"""Bound an incremental local test binary to a same-source canonical compile control.

The control only admits the measured debug=0 incremental fingerprint. It does
not change the canonical Cargo profile, source applicability, or current CI
compilation and discovery requirements.
"""
from __future__ import annotations

import json
from pathlib import Path
import re
import shlex

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts

INCREMENTAL_TEST_PROFILE_0 = 5718297822855796632
CONTROL_ENV_KEYS = {
    'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR',
    'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_PROFILE_TEST_DEBUG',
    'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
}
CONTROL_FIELDS = {
    'version', 'source_commit', 'archive_sha256', 'source_manifest_sha256',
    'compiler', 'cwd', 'environment', 'build_argv', 'build_exit',
    'build_stdout', 'build_stdout_sha256', 'build_stdout_bytes',
    'build_stderr_sha256', 'artifact', 'binary_path',
    'binary_sha256_before', 'binary_sha256_after', 'fingerprint_path',
    'fingerprint_sha256', 'fingerprint_raw', 'fingerprint', 'list_argv',
    'list_exit', 'list_stdout', 'list_stdout_sha256',
    'source_inventory_sha256',
}


def shared_text(proof, reference):
    receipts.require(isinstance(proof, dict) and isinstance(proof.get('logs'), dict)
                     and isinstance(reference, dict) and set(reference) == {'sha256_ref'}
                     and retention.DIGEST.fullmatch(reference['sha256_ref'])
                     and reference['sha256_ref'] in proof['logs'],
                     'Incremental control reference unavailable')
    item = proof['logs'][reference['sha256_ref']]
    raw = retention.decoded_log(item)
    receipts.require(item['sha256'] == reference['sha256_ref']
                     and retention.digest(raw.encode()) == reference['sha256_ref'],
                     'Incremental control shared digest differs')
    return raw


def verify(record, item, witness, proof):
    """Require both original execution evidence and independent compile control."""
    from validation import qualification_unit_continuation as unit

    profile = record['environment']['runtime']['profile']
    receipts.require(profile['debuginfo'] == 0
                     and witness['fingerprint']['profile'] == INCREMENTAL_TEST_PROFILE_0
                     and set(witness) >= {'source_artifact', 'source_list_log', 'incremental_control'},
                     'Unknown incremental source profile or witness')
    target_identity = unit.rust_identity(item['invocation'])
    package, stem, fingerprint_name = unit.native_target_paths(target_identity, record['source_commit'])
    artifact = witness['source_artifact']
    receipts.require(isinstance(artifact, dict)
                     and artifact.get('reason') == 'compiler-artifact'
                     and artifact.get('target', {}).get('name') == stem
                     and artifact['target'].get('kind') == ['test']
                     and artifact.get('executable') == witness['binary_path']
                     and artifact.get('features') == witness['features']
                     and artifact.get('profile') == profile,
                     'Original incremental Cargo artifact differs')
    source_list = shared_text(proof, witness['source_list_log'])
    source_lines = source_list.splitlines()
    source_ids = [line.removesuffix(': test') for line in source_lines if line.endswith(': test')]
    actual_argv = shlex.split(record['origin']['command'])
    selectors = [word for word in actual_argv[1:] if not word.startswith('--')]
    actual_outcomes = record['coverage']['outcomes']
    receipts.require(source_lines and len(source_ids) == len(source_lines)
                     and len(source_ids) == len(set(source_ids))
                     and len(selectors) == 1 and len(actual_outcomes) == 1
                     and selectors[0] in source_ids
                     and set(actual_outcomes) == {selectors[0]},
                     'Executed case absent from original incremental inventory')

    control = receipts.bounded_json(shared_text(proof, witness['incremental_control']).encode())
    receipts.require(isinstance(control, dict) and set(control) == CONTROL_FIELDS
                     and control['version'] == 1
                     and isinstance(control['source_commit'], str)
                     and isinstance(control['archive_sha256'], str)
                     and isinstance(control['source_manifest_sha256'], str)
                     and isinstance(control['compiler'], str)
                     and isinstance(control['cwd'], str)
                     and control['source_commit'] == record['source_commit']
                     and control['archive_sha256'] == witness['archive_sha256']
                     and control['archive_sha256'] == unit.source_archive_digest(record['source_commit'])
                     and retention.DIGEST.fullmatch(control['source_manifest_sha256'])
                     and control['compiler'].splitlines()[0] == unit.RUSTC_VERSION
                     and 'host: x86_64-unknown-linux-gnu' in control['compiler'].splitlines()
                     and Path(control['cwd']).is_absolute(),
                     'Incremental control source/compiler differs')
    environment = control['environment']
    receipts.require(isinstance(environment, dict) and set(environment) == CONTROL_ENV_KEYS
                     and environment['CARGO_INCREMENTAL'] == '0'
                     and environment['CARGO_BUILD_JOBS'] == '1'
                     and all(environment[key] is None for key in CONTROL_ENV_KEYS - {
                         'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR'})
                     and isinstance(environment['CARGO_TARGET_DIR'], str)
                     and Path(environment['CARGO_TARGET_DIR']).is_absolute(),
                     'Incremental control Cargo environment differs')
    build_argv = control['build_argv']
    receipts.require(isinstance(build_argv, list)
                     and all(isinstance(word, str) for word in build_argv)
                     and shlex.join(build_argv) == witness['build_invocation']
                     and control['build_exit'] == 0,
                     'Incremental control did not run the original single-target Cargo command')
    actual_build = unit.normalized_cargo(shlex.join(build_argv), compile_only=True)
    original_build = unit.normalized_cargo(witness['build_invocation'], compile_only=True)
    receipts.require(actual_build == original_build
                     and actual_build['cargo'].get('--test') == stem,
                     'Incremental control target or flags differ')
    stdout = control['build_stdout']
    receipts.require(isinstance(stdout, str)
                     and retention.digest(stdout.encode()) == control['build_stdout_sha256']
                     and len(stdout.encode()) == control['build_stdout_bytes']
                     and retention.DIGEST.fullmatch(control['build_stderr_sha256']),
                     'Incremental control Cargo output differs')
    frames = [receipts.bounded_json(line.encode()) for line in stdout.splitlines()]
    finished = [frame for frame in frames if frame.get('reason') == 'build-finished']
    artifacts = [frame for frame in frames if frame.get('reason') == 'compiler-artifact'
                 and frame.get('executable') == control['binary_path']
                 and frame.get('target', {}).get('name') == stem
                 and frame['target'].get('kind') == ['test']]
    receipts.require(len(finished) == len(artifacts) == 1
                     and finished[0].get('success') is True
                     and artifacts[0] == control['artifact']
                     and artifacts[0]['profile'] == artifact['profile'] == profile
                     and artifacts[0]['features'] == artifact['features'] == witness['features']
                     and all(artifacts[0].get(key) == artifact.get(key)
                             for key in ('package_id', 'manifest_path'))
                     and artifacts[0]['target'].get('src_path') == artifact['target'].get('src_path'),
                     'Incremental control artifact/feature/profile differs')
    receipts.require(isinstance(control['binary_path'], str)
                     and isinstance(control['fingerprint_path'], str),
                     'Incremental control paths are not strings')
    binary = Path(control['binary_path'])
    suffix = binary.name.rsplit('-', 1)[-1]
    fingerprint_path = Path(control['fingerprint_path'])
    receipts.require(binary.is_absolute() and binary.parent.name == 'deps'
                     and binary.parent.parent == Path(environment['CARGO_TARGET_DIR']) / 'debug'
                     and re.fullmatch(r'[0-9a-f]{8,64}', suffix) is not None
                     and binary.name == stem + '-' + suffix
                     and fingerprint_path.name == fingerprint_name
                     and fingerprint_path.parent.name == package + '-' + suffix
                     and fingerprint_path.parent.parent.name == '.fingerprint'
                     and fingerprint_path.parent.parent.parent == binary.parent.parent
                     and retention.DIGEST.fullmatch(control['binary_sha256_before'])
                     and control['binary_sha256_after'] == control['binary_sha256_before'],
                     'Incremental control binary/fingerprint custody differs')
    fingerprint_raw = control['fingerprint_raw']
    receipts.require(isinstance(fingerprint_raw, str)
                     and retention.digest(fingerprint_raw.encode()) == control['fingerprint_sha256']
                     and receipts.bounded_json(fingerprint_raw.encode()) == control['fingerprint'],
                     'Incremental control fingerprint bytes differ')
    fingerprint = control['fingerprint']
    receipts.require(fingerprint.get('rustc') == unit.RUSTC_FINGERPRINT
                     and fingerprint.get('compile_kind') == 0
                     and fingerprint.get('rustflags') == []
                     and fingerprint.get('profile') == unit.TEST_PROFILE_FINGERPRINTS[0]
                     and json.loads(fingerprint.get('features', 'null')) == witness['features'],
                     'Incremental control canonical fingerprint differs')
    listing = control['list_stdout']
    receipts.require(control['list_argv'] == [control['binary_path'], '--list', '--format=terse']
                     and control['list_exit'] == 0
                     and isinstance(listing, str)
                     and retention.digest(listing.encode()) == control['list_stdout_sha256']
                     and control['source_inventory_sha256'] == retention.digest(source_list.encode())
                     and listing == source_list,
                     'Incremental source/control native inventory differs')
