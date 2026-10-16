"""Typed non-Rust source cohorts; opaque successes never become runtime rows.

The authenticated promotion writer supplies source-impact reviews. Every changed
path remains classified. Current compilers and independent discovery still run.
"""
from __future__ import annotations

import base64
import io
import json
import tarfile
import re
import xml.etree.ElementTree as ET
from pathlib import Path

from validation.nonrust_original_captures import LEGACY_JVM
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts

KINDS = {'validation', 'operations', 'android_jvm'}
BASE_SOURCE = '59440187d0ad16ceec7632f2461493112f3dfc60'
BASE_PYTHON_LOG = 'f5b438be3ec8c0b4e2fc0d496081503c44b69c6bb2afc0c1b2d96fda487ab45e'
BASE_JVM_LOG = 'e1b031c6072281733118e3d31a486e60bb48709d479338c4fdc234ff9ebef4bb'
BASE_JVM_METADATA = '70d509cfa3b3193c8622c0b8f58b1da789a313c7337d9261962bd19cedb5dcc6'
MODERN_PYTHON_BINARY = '87d4df53fd91304be5bac391fb204643c36b7df2023c04a0953bcbc7d4fdf634'
MODERN_NODE_BINARY = '902b6a6984d5d825829ea9064ab73b734548df37bc0683990dca31c8dc2a9253'
NODELESS_MODERN_CAPTURES = {
    '8751a0c9fc6228958645019a6b0a2fea20c041b141a19595189bb72d7ba4676d': '2d0d5864808c99631eaef4098a6af36a8d4e3130',
    '7293165eaf69133db2a6a3036ff5ece2f1f1e2e6ad063e3b859fac34cb1b0aa9': '2d0d5864808c99631eaef4098a6af36a8d4e3130',
}
MODERN_JVM_BINARY = 'b6adf75c6ce0d3666da6eced72b5ca9359e175e2aa958812919bb7b547e4a201'
MODERN_JVM_ORIGINAL_BINDING = '127ce14386b206d0c140d67d6d876c0b6e2a44a27b07951434a736cb6a8cb6f8'
MODERN_JVM_ORIGINAL_ENVIRONMENT = '7e937df7e09182c234d801fc1b4a9d37149a8d678f8a28f0ffe3475a5979ded7'
ORIGINAL40_JVM_BUILD_INPUTS = {
    'clients/android/gradle/wrapper/gradle-wrapper.properties': 'd01b41913834fb3cb14e71f69a08898e6a47e663b08f5c9b8c7e0772c38889d2',
    'clients/android/gradle/libs.versions.toml': 'ced36ebe0658327618e6b959982f31acc1baae352a46ee2b8cc23b15250f00c2',
    'clients/android/app/build.gradle.kts': '7780da0bd369665d9dd71d73efdf9246efdeaee21fe03cf7cc98688314a885c8',
    'clients/android/build.gradle.kts': '0aec8d2e01e88e3abdd6b4690201ddd107b10c05b6e86a4014315cc4ab64fd46',
    'clients/android/settings.gradle.kts': '5891ed8c25e4412da6dd0fff31d21626fa2bb3dde4cc6326e62d39eb9db79055',
}
CURRENT_JVM_BUILD_INPUTS = {**ORIGINAL40_JVM_BUILD_INPUTS,
    'clients/android/app/build.gradle.kts': '7fa382078cde36ee16165749cf757be44c10271332a90694855aae4d750f0fe0',
}
# One immutable capture whose native command passed but whose enclosing wrapper
# observed transient dirtiness. Independent tracked-input custody does not rewrite
# that observation or authorize another dirty capture.
RECONCILED_CUSTODY = {
    'c040dd312606e7062acb25205425fee7b7dcc5d5ca52f1c822c2d31392540193': {
        'supplement': '3e69b6bcfd470c8eefe46d61502fb5e75b5358f79364f2c6302735e528d12ab3',
        'source': 'da8d67f18a910ae0acdc51cbd153ac6ef04d75bf',
        'log': '7c3fbeaf644fb04123b61df55b5e76549e02ea1056ccb0de5e187694e7e1f2d6',
    },
}
CONTRACTS = {'validation': 'python-source-cohort-v1',
             'operations': 'python-source-cohort-v1',
             'android_jvm': 'junit4-gradle9-java21-or25-v1'}

# Finite original local captures. Unknown historical patch/binary measurements
# remain unknown. These exact log/source/tool identities authorize no new run.
LEGACY_PYTHON = {
    # Incoming named captures are retained with their source representatives;
    # their actual execution commit and interpreter measurements remain unknown.
    'c2ee570075789bc59018007c2d9caead7669596e7f10484643753d3760720e85': {
        'source': 'e08a0680ca1fe0d405c8b3421e5f1f4912ace980',
        'tool': 'exec-15f40524-4701-41fd-9130-d5973040a947',
        'count': 2, 'mode': 'python-named', 'selectors': None, 'exit_code': 0,
        'actual_execution_source_commit': None,
        'python_patch': None, 'interpreter_sha256': None},
    '239966f35d143b735aba9c622e74fe2b725fa1f94a9869a6ab43a61e47600fdd': {
        'source': 'a3983d8a1d8c04e294c84327b6f73cbee5defcc7',
        'tool': 'exec-7c70d17d-6eb2-4dad-bd49-ba1545a10e67',
        'count': 1, 'mode': 'python-named', 'selectors': None, 'exit_code': 0,
        'actual_execution_source_commit': None,
        'python_patch': None, 'interpreter_sha256': None},
    '7222aea9ea9fa174d94c877d61a5162719f4589528ce053a09caaa20bed99797': {
        'source': '584d72a3453ae2350cde760d01122db70520da29',
        'tool': 'exec-a003587b-d684-4167-a978-4d82381ad0aa',
        'count': 157, 'mode': 'python-selected-partial', 'exit_code': 1,
        'actual_execution_source_commit': None,
        'selectors': ['test_contracts', 'test_player_input_fence', 'test_doc_status_audit',
                      'test_docs_index', 'test_remote_diagnostics_status', 'test_apple_build_claims',
                      'test_mobile_build_claims', 'test_mobile_versions'],
        'failed_ids': ['test_contracts.OperationsContractCase.test_ui_baseline_reserves_distinct_http_raft_and_api_ports'],
        'python_patch': None, 'interpreter_sha256': None},
    '26a2157691aecf52871e13521306b611618cafe5bfc90d41154d52b360a51897': {
        'source': '584d72a3453ae2350cde760d01122db70520da29',
        'tool': 'exec-fca8a650-0139-46fe-84f7-2629a26f08d2',
        'count': 1, 'mode': 'python-selected', 'exit_code': 0,
        'actual_execution_source_commit': None,
        'selectors': ['test_contracts.OperationsContractCase.test_ui_baseline_reserves_distinct_http_raft_and_api_ports'],
        'exact_selected_repair': ['test_contracts.OperationsContractCase.test_ui_baseline_reserves_distinct_http_raft_and_api_ports'],
        'python_patch': None, 'interpreter_sha256': None},
    '92b4fe6722bf85c03fd7dadaf6cb631402c0d926664a9235d10904bcaca0f77b': {
        'source': 'a7fa0053e6089d77bb2ad31539ea3c7be723ec5c',
        'tool': 'exec-a7f4a0f5-4eaf-4514-9b61-f1b54db2c609',
        'count': 22, 'mode': 'python-named', 'selectors': None,
        'python_patch': None, 'interpreter_sha256': None},
    'c5bfcb16e1b9700611c77352545eb04d2a1e8a3cd2ac372c05121c2331bdc8bd': {
        'source': '8672b07d5ebc80ef542122d5088ab75b2d8fda05',
        'tool': 'exec-a48bfc9c-c9e4-4097-a33b-af4583d607b4',
        'count': 37, 'mode': 'python-named', 'selectors': None,
        'python_patch': None, 'interpreter_sha256': None},
    'bbf220d889d409913082932298af56e5c6199e198725dc0cb2584e097ddc68d6': {
        'source': 'f587385b22df50c6bcb049801048ed90c19ea4eb',
        'tool': 'exec-3133748c-ec2f-4f5c-aaae-af9ffaa80351',
        'count': 2, 'mode': 'python-selected', 'selectors': [
            'test_bounded_write_capture.BoundedCaptureCase.test_rolling_deployment_excludes_only_the_mixed_tick_and_retains_both_sides',
            'test_bounded_write_capture.BoundedCaptureCase.test_tick_gap_splits_even_when_request_offsets_hide_the_observed_gap'],
        'python_patch': None, 'interpreter_sha256': None},
    '9ec8801e314f78994e3a409b28fd13b0d38129fe15ab7fb5aad3bdf5bbabc36e': {
        'source': 'f587385b22df50c6bcb049801048ed90c19ea4eb',
        'tool': 'exec-f9cf3039-c8f5-444e-9fca-0b75fcc58888',
        'count': 5, 'mode': 'python-selected', 'selectors': [
            'test_bounded_write_capture.BoundedCaptureCase.test_boundaries_preserve_segments_without_stitching_or_new_source_attestation',
            'test_bounded_write_capture.BoundedCaptureCase.test_partial_and_invalid_inputs_cannot_be_zero_or_extend_the_campaign',
            'test_bounded_write_capture.BoundedCaptureCase.test_actual_cli_reports_bounded_cost_without_a_twelve_hour_gate',
            'test_bounded_write_capture.BoundedCaptureCase.test_actual_bounded_sampler_keeps_one_deadline_and_closes_owned_io',
            'test_replicated_write_capture.GateCase.test_strict_after_requires_fresh_bound_evidence_and_exact_elapsed_rates'],
        'python_patch': None, 'interpreter_sha256': None},
    '884b440fa5b321eaac740d8b580c9dd34582c2ffe9611973a3e1ad4e5df90048': {
        'source': '3c9bf8f575e71a20fadb995fc50ac4085cd0a56c',
        'tool': 'exec-621b98bc-ee32-4c82-9858-b1f6ec1cdd66',
        'count': 25, 'mode': 'python-selected', 'selectors': [
            'test_bounded_write_capture', 'test_player_input_fence',
            'test_rolling_producer_ownership_inventory'],
        'python_patch': None, 'interpreter_sha256': None},
}


ORIGINAL_PYTHON_REPAIR_EDGES = {
    '7222aea9ea9fa174d94c877d61a5162719f4589528ce053a09caaa20bed99797': {
        'source': '584d72a3453ae2350cde760d01122db70520da29',
        'id': 'test_contracts.OperationsContractCase.test_ui_baseline_reserves_distinct_http_raft_and_api_ports',
        'repair': '26a2157691aecf52871e13521306b611618cafe5bfc90d41154d52b360a51897',
    },
}


INTERLEAVED_PYTHON_CAPTURES = {
    # One complete original effort capture: stdout from its mocked compiler
    # precedes unittest's terminal. This authority never matches another run.
    'ed108d64a0792ce0d54f724c31f7888ec0beaaa4e232f6b85a87f5b5294b32fe': {
        'source': 'bf1eee9c49524d00d1fa13ba01f50153e91f1183',
        'binding': 'cd626ce71d1216702948b9db385a38046e7f49e78b929565ae9fa8e1799ee025',
        'id': 'tests.validation.test_unit_openraft_package.OpenraftPackageCase.'
              'test_current_compile_binds_owned_package_profile_features_and_discovery_without_bodies',
    },
}


def named_python_capture_text(raw, source, kind, binding_raw):
    """Parsing projection only; proof custody retains the entire original raw."""
    text = raw.decode()
    capture = INTERLEAVED_PYTHON_CAPTURES.get(retention.digest(raw))
    if capture is None:
        return text
    receipts.require(source == capture['source'] and kind == 'validation'
                     and retention.digest(binding_raw) == capture['binding'],
                     'Interleaved original Python source, kind or binding differs')
    identity = capture['id']
    prefix = identity.rsplit('.', 1)[1] + ' (' + identity + ') ... '
    lines = text.splitlines(keepends=True)
    starts = [index for index, line in enumerate(lines) if line.startswith(prefix)]
    receipts.require(len(starts) == 1, 'Interleaved original Python frame ambiguous')
    start = starts[0]
    frame = lines[start:start + 7]
    receipts.require(len(frame) == 7 and frame[0].startswith(prefix + 'COMPILE_ONLY ')
                     and frame[1] == 'ADOPTED_COMPLETE_UNIT_COMMAND wal:237\n'
                     and all(line.startswith('COMPILE_ONLY ') for line in frame[2:6])
                     and frame[6] == 'ok\n', 'Interleaved original Python terminal differs')
    return ''.join(lines[:start] + [prefix + 'ok\n'] + lines[start + 7:])


def blob(proof, reference):
    receipts.require(isinstance(reference, dict) and set(reference) == {'sha256_ref'},
                     'Unknown source-cohort blob reference')
    digest = reference['sha256_ref']
    receipts.require(isinstance(digest, str) and retention.DIGEST.fullmatch(digest)
                     and digest in proof['logs'] and proof['logs'][digest]['sha256'] == digest,
                     'Source-cohort blob unavailable or aliased')
    return retention.decoded_log(proof['logs'][digest]).encode()


def canonical_python(test_id, kind, universe):
    prefix = 'tests.' + kind + '.'
    canonical = test_id[len(prefix):] if test_id.startswith(prefix) else test_id
    receipts.require(canonical in universe, 'Unknown Python source-cohort identity')
    return canonical


def named_python_entrypoint(kind, command, binding, binding_raw, raw):
    """Keep one immutable original invocation notation without resolving paths."""
    if command[0] == binding['python_executable']:
        return True
    return (kind == 'validation'
            and retention.digest(binding_raw) ==
                'd9213642172d63ab1b2bded499712418936a04026091dc0b20bcd5767f14e135'
            and binding['source_commit'] == 'ddf2397a410ef165036a5373717b38539ffc65d3'
            and retention.digest(raw) ==
                'e9fd6d5f078f77f7a842908037f4f121196cd545ec0669356f0341f8316a45f6'
            and command == binding['argv']
            and command[0] == '/opt/homebrew/opt/python@3.14/bin/python3.14'
            and binding['python_executable'] ==
                '/opt/homebrew/Cellar/python@3.14/3.14.7/Frameworks/Python.framework/Versions/3.14/bin/python3.14')


def python_rows(text, kind, universe, other=()):
    # unittest's first docstring line can appear between the identity and result.
    pattern = r'^\w+ \(([^)\n]+)\)(?: \.\.\. |\n[^\n]* \.\.\. )(ok|FAIL|ERROR|skipped)(?:\s|$)'
    pairs = re.findall(pattern, text, re.M)
    result, all_ids = {}, set()
    for raw_id, status in pairs:
        prefix = 'tests.' + kind + '.'
        foreign_prefix = 'tests.' + ('operations' if kind == 'validation' else 'validation') + '.'
        if raw_id.startswith(prefix):
            test_id, own = raw_id[len(prefix):], True
            receipts.require(test_id in universe, 'Unknown native Python identity')
        elif raw_id.startswith(foreign_prefix):
            test_id, own = raw_id[len(foreign_prefix):], False
            receipts.require(test_id in other, 'Unknown foreign native Python identity')
        else:
            receipts.require((raw_id in universe) != (raw_id in other), 'Ambiguous bare Python namespace')
            test_id, own = raw_id, raw_id in universe
        qualified = ('own:' if own else 'other:') + test_id
        receipts.require(qualified not in all_ids and status != 'skipped',
                         'Repeated or skipped native Python outcome')
        all_ids.add(qualified)
        if own:
            result[test_id] = 'pass' if status == 'ok' else 'fail'
    receipts.require(result, 'No actual named Python outcomes for command')
    summaries = re.findall(r'^Ran (\d+) tests? in [0-9.]+s$', text, re.M)
    receipts.require(len(summaries) == 1 and int(summaries[0]) == len(all_ids),
                     'Named Python outcomes differ from actual outer count')
    return result


def selected_python_outcomes(text, capture, selected):
    """Opaque selected cohort with only actual explicit failure identities."""
    expected = capture['count']
    receipts.require(len(selected) == expected, 'Original selected source membership differs from native count')
    if capture['mode'] == 'python-selected-partial':
        headings = re.findall(r'^(?:ERROR|FAIL): (\w+) \(([^)]+)\)$', text, re.M)
        failed = {test_id.removeprefix('tests.validation.').removeprefix('tests.operations.')
                  for _, test_id in headings}
        receipts.require(headings and len(headings) == len(failed)
                         and failed == set(capture['failed_ids']) and failed <= selected
                         and len(re.findall(r'^Ran ' + str(expected) + r' tests? in [0-9.]+s\n\s*\nFAILED \(errors=' + str(len(failed)) + r'\)$', text, re.M)) == 1,
                         'Original explicit error cohort differs or hides failures/skips')
        return {test_id: 'fail' if test_id in failed else 'opaque-pass' for test_id in selected}
    receipts.require(capture['mode'] == 'python-selected'
                     and len(re.findall(r'^Ran ' + str(expected) + r' tests? in [0-9.]+s\n\s*\nOK$', text, re.M)) == 1,
                     'Original selected command did not reach unique outer plain-OK')
    if capture.get('exact_selected_repair'):
        receipts.require(expected == 1 and selected == set(capture['selectors']) == set(capture['exact_selected_repair']),
                         'A repair requires one exact actual selector and native one-case OK')
    return {test_id: 'opaque-pass' for test_id in selected}



def require_source_bound_repairs(failures, repairs):
    """Finite original exact selector closes its historical event only."""
    for (source, test_id, failed_log), sequence in failures.items():
        edge = ORIGINAL_PYTHON_REPAIR_EDGES.get(failed_log)
        receipts.require(edge is not None and edge['source'] == source and edge['id'] == test_id
                         and repairs.get((source, test_id, edge['repair']), 0) > sequence,
                         'Original source-variant failure lacks later exact same-source repair')


def junit_rows(raw, universe):
    receipts.require(len(raw) <= receipts.MAX_BYTES and b'<!DOCTYPE' not in raw
                     and b'<!ENTITY' not in raw, 'Unsafe native JUnit XML')
    root = ET.fromstring(raw)
    suites = [root] if root.tag == 'testsuite' else list(root)
    receipts.require(root.tag in {'testsuite', 'testsuites'} and suites
                     and all(suite.tag == 'testsuite' for suite in suites), 'Unknown JUnit XML layout')
    result = {}
    for suite in suites:
        receipts.require(int(suite.attrib.get('skipped', '0')) == 0, 'Skipped JVM suite cannot qualify')
        cases = suite.findall('testcase')
        receipts.require(int(suite.attrib['tests']) == len(cases), 'JUnit native count mismatch')
        failures = errors = 0
        for case in cases:
            klass, method = case.attrib['classname'], case.attrib['name']
            test_id = klass + '/' + method
            receipts.require('.' in klass and '/' not in klass and '/' not in method
                             and test_id in universe and test_id not in result,
                             'Unknown or repeated fully qualified JUnit outcome')
            receipts.require(all(child.tag in {'failure', 'error', 'system-out', 'system-err'} for child in case)
                             and len(case.findall('failure')) <= 1 and len(case.findall('error')) <= 1
                             and not (case.find('failure') is not None and case.find('error') is not None)
                             and case.find('skipped') is None, 'Skipped JVM body cannot qualify a source cohort')
            failures += case.find('failure') is not None
            errors += case.find('error') is not None
            result[test_id] = 'fail' if case.find('failure') is not None or case.find('error') is not None else 'pass'
        receipts.require(int(suite.attrib['failures']) == failures and int(suite.attrib['errors']) == errors,
                         'JUnit failure totals contradict native rows')
    receipts.require(result, 'Empty actual JUnit result')
    return result


def junit_archive(proof, reference, expected_digest, universe):
    envelope = receipts.bounded_json(blob(proof, reference))
    receipts.require(set(envelope) == {'base64', 'sha256'} and envelope['sha256'] == expected_digest,
                     'Native XML archive differs from original measured sidecar')
    raw = base64.b64decode(envelope['base64'], validate=True)
    receipts.require(0 < len(raw) <= receipts.MAX_BYTES and retention.digest(raw) == expected_digest,
                     'Native XML archive digest mismatch')
    result, paths, expanded = {}, set(), 0
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:gz') as archive:
        for member in archive:
            from pathlib import PurePosixPath
            path = PurePosixPath(member.name)
            receipts.require(len(paths) < 128 and member.isfile() and not path.is_absolute()
                             and '..' not in path.parts and path.suffix == '.xml'
                             and member.name not in paths and 0 < member.size <= receipts.MAX_BYTES,
                             'Unsafe or repeated original XML archive member')
            paths.add(member.name); expanded += member.size
            receipts.require(expanded <= receipts.MAX_BYTES, 'Expanded XML archive outside bound')
            rows = junit_rows(archive.extractfile(member).read(), universe)
            receipts.require(set(rows).isdisjoint(result), 'Repeated native case across XML files')
            result.update(rows)
    receipts.require(result, 'No native cases in original XML archive')
    return result


def jvm_metadata(proof, reference, source):
    metadata = receipts.bounded_json(blob(proof, reference))
    receipts.require(metadata['kind'] == 'compiled-junit4-discovery-not-runtime-outcomes'
                     and metadata['unit_runtime_outcomes'] is None,
                     'Compiled discovery must not assert runtime outcomes')
    ids = []
    for row in metadata['classes']:
        klass = row['class']
        source_path = row['source_path']
        source_bytes = retention.git_bytes(source, source_path)
        from pathlib import PurePosixPath
        path = PurePosixPath(source_path)
        receipts.require(source_bytes is not None and not path.is_absolute() and '..' not in path.parts
                         and path.suffix in {'.kt', '.java'}
                         and path.name == row['source_file']
                         and source_path.endswith('/' + klass.rsplit('.', 1)[0].replace('.', '/') + '/' + row['source_file']),
                         'Missing or incorrectly owned compiled JVM test source')
        receipts.require(isinstance(klass, str) and '.' in klass and '/' not in klass
                         and row['ids'] and row['ids'] == sorted(set(row['ids']))
                         and row['source_path'].startswith('clients/android/app/src/test/')
                         and retention.digest(source_bytes) == row['source_sha256'],
                         'Compiled JVM class source witness differs')
        for method_id in row['ids']:
            prefix = klass + '.'
            receipts.require(method_id.startswith(prefix) and method_id[len(prefix):]
                             and '/' not in method_id[len(prefix):], 'Ambiguous JVM compiled identity')
            ids.append(klass + '/' + method_id[len(prefix):])
    receipts.require(ids and len(ids) == len(set(ids)), 'Repeated compiled JVM discovery identity')
    return sorted(ids)


def modern_jvm_argv(command):
    prefix = ['./gradlew', '--no-daemon', '--max-workers=1', ':app:testDebugUnitTest',
              '-Dorg.gradle.jvmargs=-Xmx4g']
    receipts.require(command[:5] == prefix and len(command) > 5 and (len(command) - 5) % 2 == 0,
                     'Unreviewed JVM executable, task, option or semantic override')
    pairs = list(zip(command[5::2], command[6::2]))
    receipts.require(all(flag == '--tests' and re.fullmatch(r'[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+){3,}', selector)
                         for flag, selector in pairs), 'Unknown or non-exact JVM selection option')
    selectors = [selector for _, selector in pairs]
    receipts.require(len(selectors) == len(set(selectors)), 'Repeated actual JVM selector')
    return selectors


# One closed incoming CI pair; these are original Linux source-fixture outcomes.
# Numeric suite exits and interpreter binary hashes were not recorded. Nothing
# here changes the ordinary same-effort or measured local-capture contracts.
LINKED_PYTHON_CI_SCOPE = {'base': 'main',
 'branch': 'codex/continuous-quality-hardware-status',
 'pr': 1048,
 'repository': 1,
 'workflow': 'main-fast-lane.yml'}
LINKED_PYTHON_CI_RUNS = {5249: {'job': 51932,
        'job_status': 'failure',
        'journal_artifact_sha256': '1997a87ed005994083ab0dc6f043ab9c5d0ff6558f94fbf00135129f114d8ee2',
        'journal_pass_count': 1372,
        'producer_inputs': {'.github/workflows/main-fast-lane.yml': 'd2b4528963ded33c03255685dea791d65634773eb7236aa4f580900fd87ca5e4',
                            'validation/main_preflight_adoption.py': 'b9851a5404ef4cfe213b8244a67adc2d8b92df730390e19884aa2e7619c053c8',
                            'validation/main_unit_receipts.py': 'b4112a92f0dd6acb63937c837eeb899db76ab8d8dfad8025f12e7924622ac5f4',
                            'validation/python_unit_receipts.py': 'f8fec1cbd51b0004e96cb984ffdb0227325930deb0daae9615f8d279d6e86eef'},
        'raw_sha256': 'b87afee73c137c624dadd48e755cdc3610770048e995f601c02b65df1e82cde0',
        'source': '213e4cd453bf139ad741fc3dd7e14630f46b77e0',
        'wire_journals': {'final': 'dede5745304052758fde8bba9161cede8b5e183048994245d6d0bfa28583f0e6',
                          'start': 'd9dd239a902589bcda5f672c55617bb322d8a73fb9e25fe1f62e436c0647fb9d'}},
 5254: {'job': 51968,
        'job_status': 'success',
        'journal_artifact_sha256': '0195b19e5753d7db3234f7e264afa3d5d76f5fdb99780374671c38de581f680d',
        'journal_pass_count': 1388,
        'producer_inputs': {'.github/workflows/main-fast-lane.yml': 'd2b4528963ded33c03255685dea791d65634773eb7236aa4f580900fd87ca5e4',
                            'validation/main_preflight_adoption.py': 'b9851a5404ef4cfe213b8244a67adc2d8b92df730390e19884aa2e7619c053c8',
                            'validation/main_unit_receipts.py': 'b4112a92f0dd6acb63937c837eeb899db76ab8d8dfad8025f12e7924622ac5f4',
                            'validation/python_unit_receipts.py': 'f8fec1cbd51b0004e96cb984ffdb0227325930deb0daae9615f8d279d6e86eef'},
        'raw_sha256': '06c6d8d08ba41238c3f572a8a68e71fd83615d416f9e40cece9ff40c7d45639a',
        'source': '77ed3f5968c5d03c35f839635be432f9cc5febc4',
        'wire_journals': {'final': '778105690ed623f8acf716c5bcd3e4ff81f6afd39679d84f1b2013f43ab081b4',
                          'start': 'd5eef4887a7d76ffa84be72a9d2b7cc9159424e981de3d5e8214357f53dec956'}}}
LINKED_PYTHON_CI_FAILURES = ('validation:test_rolling_producer_ownership_inventory.RollingProducerOwnershipInventoryTest.test_module_wide_legacy_and_deadline_sentinels_have_reviewed_counts',
 'validation:test_rolling_producer_ownership_inventory.RollingProducerOwnershipInventoryTest.test_module_wide_task_timer_and_process_shapes_have_reviewed_counts',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a05_boundary_named_fences_preserve_original_scope_and_no_allowlist_growth',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a_direct_still_current_call_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a_fence_used_from_another_continuation_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a_fence_without_a_census_row_and_a_row_without_a_fence_are_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a_new_hand_written_fence_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a_scope_the_attempt_type_does_not_declare_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_a_second_copy_of_an_excused_fence_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_all_fences_swapped_at_once_are_each_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_an_allow_list_row_that_outlived_its_fence_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_prose_about_a_fence_is_not_a_fence',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_reverting_any_migrated_fence_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_swapping_any_fences_scope_set_is_reported',
 'operations:test_attempt_scope_census.AttemptScopeCensusCase.test_the_repository_agrees_with_its_own_census',
 'operations:test_doc_status_audit.DocStatusAuditCase.test_current_index_has_no_mechanical_contradictions')
LINKED_PYTHON_CI_DOCS = ('operations:test_docs_index.DocsIndexTest.test_every_index_link_resolves',
 'operations:test_docs_index.DocsIndexTest.test_relative_links_between_documents_resolve')

def linked_python_ci_policy(record):
    execution = record['origin']['execution']
    digest = execution.get('legacy_capture')
    matches = [(run, row) for run, row in LINKED_PYTHON_CI_RUNS.items()
               if row['raw_sha256'] == digest]
    receipts.require(len(matches) == 1, 'Unknown linked Python CI capture')
    run, policy = matches[0]
    kind = execution.get('kind')
    receipts.require(kind in {'validation', 'operations'}
                     and record['source_commit'] == policy['source']
                     and record['log'] == {'sha256_ref': digest}
                     and record['origin']['kind'] == 'authenticated-ci'
                     and record['origin']['identity'] == f"run:{run}/job:{policy['job']}"
                     and record['origin']['command'] is None
                     and execution == {'mode': 'source-cohort-v1', 'kind': kind,
                                       'legacy_capture': digest, 'job_status': policy['job_status']}
                     and record['coverage'] == {'mode': 'python-named'}
                     and record['exit_code'] is None,
                     'Linked Python CI origin, source or unknown numeric exit changed')
    return run, policy


def linked_python_ci_pair(proof, discovery):
    """Read both whole originals; journals never manufacture native outcomes."""
    from validation import main_unit_receipts as main
    snapshots, texts = {}, {}
    for run, policy in LINKED_PYTHON_CI_RUNS.items():
        raw = blob(proof, {'sha256_ref': policy['raw_sha256']})
        receipts.require(retention.digest(raw) == policy['raw_sha256'], 'Linked CI raw changed')
        lines = main.log_lines(raw)
        markers = re.findall(r'^MAIN-UNIT-JOURNAL (start|final) [1-9][0-9]* ([0-9a-f]{64})$',
                             '\n'.join(lines), re.M)
        receipts.require(markers == [(phase, policy['wire_journals'][phase]) for phase in ('start', 'final')],
                         'Linked CI journal order or wire identity changed')
        phases = main.read_snapshots(lines)
        artifact = receipts.bounded_json(blob(proof, {'sha256_ref': policy['journal_artifact_sha256']}))
        receipts.require(set(phases) == {'start', 'final'} and artifact == phases['final'],
                         'Original pretty artifact and printed journal differ')
        for phase, journal in phases.items():
            receipts.require(set(journal) == {'version', 'scope', 'commit', 'applicability_commit',
                                             'run', 'complete', 'fixture_errors', 'passes'}
                             and journal['version'] == 1 and journal['scope'] == LINKED_PYTHON_CI_SCOPE
                             and journal['commit'] == journal['applicability_commit'] == policy['source']
                             and journal['run'] == run and journal['complete'] is (phase == 'final')
                             and journal['fixture_errors'] == [] and isinstance(journal['passes'], dict),
                             'Linked CI journal source, scope or completion differs')
        receipts.require(len(artifact['passes']) == policy['journal_pass_count'],
                         'Linked CI original pass membership count differs')
        snapshots[run], texts[run] = phases, '\n'.join(lines)
    first, repaired = sorted(LINKED_PYTHON_CI_RUNS)
    initial, final = snapshots[first]['final']['passes'], snapshots[repaired]['final']['passes']
    receipts.require(snapshots[first]['start']['passes'] == {}
                     and snapshots[repaired]['start']['passes'] == initial
                     and all(value == {'commit': LINKED_PYTHON_CI_RUNS[first]['source'], 'run': first}
                             for value in initial.values())
                     and all(final.get(key) == value for key, value in initial.items())
                     and set(final) - set(initial) == set(LINKED_PYTHON_CI_FAILURES)
                     and all(final[key] == {'commit': LINKED_PYTHON_CI_RUNS[repaired]['source'],
                                            'run': repaired} for key in LINKED_PYTHON_CI_FAILURES)
                     and set(LINKED_PYTHON_CI_DOCS) <= set(initial),
                     'Inherited journal membership was relabeled or repair ownership changed')
    for run, phases in snapshots.items():
        own = {kind + ':' + name for kind in ('validation', 'operations')
               for name in discovery(LINKED_PYTHON_CI_RUNS[run]['source'], kind)}
        receipts.require(set(phases['final']['passes']) <= own,
                         'Linked journal includes an unknown source-archived identity')
    # Original full run contains nested test fixtures. Select only these pinned
    # real native identities, retaining every original failure and the whole log.
    wanted = set(LINKED_PYTHON_CI_FAILURES) | set(LINKED_PYTHON_CI_DOCS)
    found = {}
    pattern = r'^\w+ \(([^)\n]+)\)(?: \.\.\. |\n[^\n]* \.\.\. )(ok|FAIL|ERROR|skipped)(?:\s|$)'
    for name, status in re.findall(pattern, texts[first], re.M):
        matches = [key for key in wanted if key.split(':', 1)[1] == name
                   or 'tests.' + key.replace(':', '.', 1) == name]
        if not matches:
            continue
        receipts.require(len(matches) == 1 and matches[0] not in found,
                         'Repeated linked original native identity')
        found[matches[0]] = 'pass' if status == 'ok' else 'fail' if status in {'FAIL', 'ERROR'} else 'ignored'
    expected = {key: 'fail' for key in LINKED_PYTHON_CI_FAILURES}
    expected.update({key: 'pass' for key in LINKED_PYTHON_CI_DOCS})
    receipts.require(found == expected, 'Original linked native failures or docs passes differ')
    # Repair ran two genuine unittest suites, each with its own outer terminal.
    # Frame boundaries are derived from exact native method starts/summary ends;
    # no reconstructed output or inherited positive row enters this decoding.
    results, lines = {}, texts[repaired].splitlines()
    cursor = 0
    for kind in ('validation', 'operations'):
        selected = {key.split(':', 1)[1] for key in LINKED_PYTHON_CI_FAILURES
                    if key.startswith(kind + ':')}
        starts = [i for i in range(cursor, len(lines))
                  if any('(' + name + ')' in lines[i] or '(tests.' + kind + '.' + name + ')' in lines[i]
                         for name in selected)]
        receipts.require(starts, 'Missing linked repair native suite')
        start = min(starts)
        ends = [i for i in range(start, len(lines)) if lines[i] == 'OK']
        receipts.require(ends, 'Missing linked repair outer terminal')
        end = ends[0]
        body = '\n'.join(lines[start:end + 1])
        other = {key.split(':', 1)[1] for key in LINKED_PYTHON_CI_FAILURES
                 if not key.startswith(kind + ':')}
        actual = python_rows(body, kind, selected, other)
        receipts.require(actual == {name: 'pass' for name in selected}
                         and re.search(r'^Ran ' + str(len(selected)) + r' tests in [0-9.]+s\n\s*\nOK$', body, re.M),
                         'Repair native frame membership or terminal changed')
        results.update({kind + ':' + name: value for name, value in actual.items()})
        cursor = end + 1
    receipts.require(set(results) == set(LINKED_PYTHON_CI_FAILURES),
                     'Linked repair promoted an inherited pass to native credit')
    return {first: found, repaired: results}


def authenticate_linked_python_ci(api, proof, record, document, repository, live_logs, runs, jobs_by_run):
    """The exact old PR pair is the only exception to same-effort CI scope."""
    from validation import main_unit_receipts as main
    linked_python_ci_policy(record)
    receipts.require(document['pr'] == 1045 and document['branch'] == 'effort/dv-hdr-repair'
                     and repository == LINKED_PYTHON_CI_SCOPE['repository'],
                     'Linked original CI is outside its one main-bound promotion')
    for rid, policy in LINKED_PYTHON_CI_RUNS.items():
        if rid not in runs:
            runs[rid] = api.get(f'/actions/runs/{rid}')
            jobs_by_run[rid] = api.pages(f'/actions/runs/{rid}/jobs')
        run = runs[rid]
        main.authenticate_run(LINKED_PYTHON_CI_SCOPE, run)
        jobs = [job for job in jobs_by_run[rid] if job['id'] == policy['job']]
        receipts.require(run['repository']['id'] == repository and run['commit_sha'] == policy['source']
                         and len(jobs) == 1 and jobs[0]['name'] == main.JOB
                         and jobs[0]['attempt'] == 1 and jobs[0]['run_id'] == rid
                         and jobs[0]['repo_id'] == repository and jobs[0]['task_id'] > 0
                         and jobs[0]['status'] == policy['job_status'],
                         'Linked original CI run/job/attempt/disposition changed')
        for path, digest in policy['producer_inputs'].items():
            raw = retention.git_bytes(policy['source'], path)
            receipts.require(raw is not None and retention.digest(raw) == digest,
                             'Linked original CI workflow or receipt producer changed')
        live = live_logs.get(api, rid, policy['job'])
        receipts.require(retention.digest(live) == policy['raw_sha256']
                         and blob(proof, {'sha256_ref': policy['raw_sha256']}) == live,
                         'Linked original CI whole live log differs')


def require_capture_custody(proof, execution, binding_raw, binding):
    """Preserve an absent historical measurement; reconcile only a pinned one."""
    reference = execution.get('custody_supplement')
    if 'source_clean_after' not in binding or binding['source_clean_after'] is True:
        receipts.require('custody_supplement' not in execution, 'Unneeded custody supplement cannot alter a capture')
        return
    registered = RECONCILED_CUSTODY.get(retention.digest(binding_raw))
    receipts.require(binding['source_clean_after'] is False and registered is not None
                     and reference == {'sha256_ref': registered['supplement']},
                     'Failed source cleanliness needs its exact finite custody supplement')
    supplement = receipts.bounded_json(blob(proof, reference))
    receipts.require(binding['source_commit'] == registered['source']
                     and binding['log_sha256'] == registered['log']
                     and supplement['original_binding_sha256'] == retention.digest(binding_raw)
                     and supplement['original_raw_log_sha256'] == binding['log_sha256']
                     and supplement['source_commit'] == binding['source_commit']
                     and supplement['tree'] == binding['tree']
                     and supplement['original_source_archive_sha256'] == binding['source_archive_sha256']
                     and supplement['original_started_utc'] == binding['started_utc']
                     and supplement['original_finished_utc'] == binding['finished_utc']
                     and supplement['original_native_exit'] == binding['exit_code'] == 0
                     and supplement['original_wrapper_exit'] == 1
                     and supplement['original_source_clean_after'] is False
                     and supplement['original_dirty_paths'] is None
                     and supplement['tracked_entries'] == 5874
                     and supplement['tracked_byte_type_git_executable_mismatches'] == []
                     and supplement['tracked_entries_with_mtime_or_ctime_after_original_start'] == []
                     and supplement['current_head'] == registered['source']
                     and supplement['current_porcelain'] == ''
                     and supplement['unit_bodies_executed'] == 0,
                     'Finite source-custody supplement contradicts the original capture')


def current_binding(proof, record, raw, candidate, archive_digest):
    """New local captures retain their original measured sidecar, without edits."""
    execution = record['origin']['execution']
    binding_raw = blob(proof, execution['binding'])
    binding = receipts.bounded_json(binding_raw)
    source = record['source_commit']
    archive = binding.get('source_archive_sha256', binding.get('archive_sha256'))
    command = binding.get('argv', binding.get('command'))
    receipts.require(binding['source_commit'] == source and binding['branch'].startswith('effort/')
                     and archive == archive_digest(source)
                     and binding['log_sha256'] == retention.digest(raw)
                     and binding['exit_code'] == record['exit_code']
                     and binding.get('source_stable') is True
                     and isinstance(command, list) and command and all(isinstance(token, str) for token in command)
                     and record['origin']['command'] == command,
                     'Actual local source/archive/command/completion binding differs')
    require_capture_custody(proof, execution, binding_raw, binding)
    kind = execution['kind']
    if kind in {'validation', 'operations'}:
        receipts.require(binding['python_version'].startswith('3.14.7')
                         and binding['python_binary_sha256'] == MODERN_PYTHON_BINARY
                         and binding['platform'] == 'darwin',
                         'Measured Python unit contract unavailable')
        if not (kind == 'validation' and NODELESS_MODERN_CAPTURES.get(retention.digest(raw)) == source):
            receipts.require(binding['node_version'] == 'v26.8.1'
                             and binding['node_binary_sha256'] == MODERN_NODE_BINARY,
                             'Measured operations Node runtime unavailable')
    else:
        receipts.require(binding['java_version'].startswith('openjdk version "25.0.2"')
                         and binding['java_binary_sha256'] == MODERN_JVM_BINARY
                         and binding['platform'] == 'darwin',
                         'Measured JVM unit runtime unavailable')
        environment = binding['environment']
        receipts.require(isinstance(environment, dict)
                         and set(environment) == {'JAVA_HOME', 'ANDROID_HOME', 'GRADLE_USER_HOME'}
                         and all(isinstance(value, str) and value.startswith('/') for value in environment.values()),
                         'Original JVM execution environment shape differs')
        if retention.digest(binding_raw) == MODERN_JVM_ORIGINAL_BINDING:
            receipts.require(source == '4a44a2279275f93e04de0e379b400a4bdb5709c9'
                             and retention.digest(raw) == '64149a12b291e78248bf4d99498e85ba724ade7601f98f387dc39f4dfaf7f6ed'
                             and retention.digest(json.dumps(environment, sort_keys=True, separators=(',', ':')).encode()) == MODERN_JVM_ORIGINAL_ENVIRONMENT,
                             'Finite original measured JVM capture differs')
        else:
            receipts.require(binding['gradle_semantic_environment'] == {} and binding['gradle_init_scripts'] == [],
                             'New JVM capture contains unknown environment or Gradle init behavior')
        modern_jvm_argv(command)
    return command, binding



def original_jvm_failure_witness(proof, capture):
    """An original failed command is a witness, never inferred passing rows."""
    witness = capture['prior_failure']
    text = blob(proof, {'sha256_ref': witness['log_sha256']}).decode()
    klass, method = witness['id'].rsplit('/', 1)
    receipts.require(re.search(r'^> Task :app:testDebugUnitTest FAILED[ \t]*$', text, re.M)
                     and re.search(r'^' + re.escape(klass.rsplit('.', 1)[1] + ' > ' + method) + r' FAILED$', text, re.M)
                     and re.search(r'^' + str(witness['native_count']) + r' tests completed, '
                                   + str(witness['native_failures']) + r' failed$', text, re.M)
                     and witness['native_failures'] == 1
                     and re.search(r'^BUILD FAILED in ', text, re.M),
                     'Original explicit JVM failure witness differs')
    commands = receipts.bounded_json(blob(proof, {'sha256_ref': witness['original_commands_sha256']}))['items']
    failed = [(index, row) for index, row in enumerate(commands) if row['id'] == witness['tool']]
    repaired = [(index, row) for index, row in enumerate(commands) if row['id'] == capture['tool']]
    receipts.require(len(failed) == len(repaired) == 1 and failed[0][0] < repaired[0][0]
                     and failed[0][1]['exitCode'] == 1 and repaired[0][1]['exitCode'] == 0
                     and failed[0][1]['source_turn'] == repaired[0][1]['source_turn'],
                     'Original JVM failure and native repair command chronology differs')
    return witness['id']


def legacy_jvm_outcomes(proof, record, capture, raw, universe):
    """Retain finite source equivalence without inventing execution measurements."""
    origin, coverage = record['origin'], record['coverage']
    digest = record['log']['sha256_ref']
    receipts.require(origin['kind'] == 'attested-local' and origin['identity'] == capture['tool']
                     and origin['command'] is None
                     and record['source_commit'] == capture['reviewed_source_equivalence']
                     and record['exit_code'] == capture['exit_code']
                     and origin['execution'] == {'mode': 'source-cohort-v1', 'kind': 'android_jvm',
                                                 'legacy_capture': digest, 'actual_execution_source_commit': None,
                                                 'runtime_binary_sha256': None, 'image_digest': None},
                     'Finite legacy JVM execution/source-equivalence boundary differs')
    receipts.require(all(retention.git_bytes(record['source_commit'], path) is not None
                         and retention.digest(retention.git_bytes(record['source_commit'], path)) == expected
                         for path, expected in capture['reviewed_input_sha256'].items()),
                     'Reviewed equivalent native source input witness differs')
    text = raw.decode()
    from validation.qualification_unit_continuation import clean_log
    text = clean_log(text)
    receipts.require(re.search(r'^> Task :app:testDebugUnitTest[ \t]*$', text, re.M)
                     and not re.search(r'^> Task :app:testDebugUnitTest (?:UP-TO-DATE|FROM-CACHE|SKIPPED|NO-SOURCE)', text, re.M)
                     and bool(re.search(r'^BUILD SUCCESSFUL in ', text, re.M)) == (record['exit_code'] == 0)
                     and (record['exit_code'] == 0 or re.search(r'^BUILD FAILED in ', text, re.M)),
                     'Original legacy JVM task did not execute with its actual disposition')
    membership = set(capture['membership_ids'])
    receipts.require(membership and membership <= universe, 'Unknown legacy source-cohort membership')
    if capture['mode'] == 'jvm-selected':
        receipts.require(set(coverage) == {'mode', 'observation'} and coverage['mode'] == 'jvm-selected'
                         and coverage['observation'] == {'sha256_ref': capture['observation_sha256']}
                         and record['exit_code'] == 0 and len(membership) == capture['native_count'],
                         'Original opaque selected JVM observation differs')
        blob(proof, coverage['observation'])  # Exact original observed suite summary, not recreated XML.
        return {test_id: 'opaque-pass' for test_id in membership}
    receipts.require(set(coverage) == {'mode', 'xml'} and coverage['mode'] == 'junit-xml'
                     and isinstance(coverage['xml'], list)
                     and sorted(reference['sha256_ref'] for reference in coverage['xml']) == capture['xml_sha256'],
                     'Original exact native XML set differs')
    result = {}
    for reference in coverage['xml']:
        rows = junit_rows(blob(proof, reference), universe)
        receipts.require(set(rows).isdisjoint(result), 'Repeated legacy native XML case')
        result.update(rows)
    receipts.require(set(result) == membership
                     and (record['exit_code'] == 0) == all(value == 'pass' for value in result.values()),
                     'Legacy XML membership or native disposition contradicts original execution')
    if capture.get('prior_failure'):
        failed_id = original_jvm_failure_witness(proof, capture)
        receipts.require(result.get(failed_id) == 'pass', 'Original named JVM failure lacks native XML repair')
    return result


def require_legacy_chronology(legacy_sequences):
    chronology = ['9b07da0a9bcb25fdf1183a3c4f41056ab96b179aab9a247b0dda7e4dad9fdad6',
                  '12ffb580c4ae2bf03f9a76b42b7bfff87678744eb4294ee5905b8cef6f08fdeb',
                  'd998e79e3563d80208b23b20b16e0e547a7972aca1ed614d6187a23af7cdc5be']
    receipts.require(all(digest in legacy_sequences for digest in chronology)
                     and [legacy_sequences[digest] for digest in chronology] == sorted(legacy_sequences[digest] for digest in chronology),
                     'Original JVM negative/clock/Main repair chronology incomplete or reordered')


def validate(item, proof, candidate, discovery, changed_inputs, clean_log, archive_digest):
    receipts.require(set(item) == {'name', 'invocation', 'protocol', 'contract', 'inventory', 'records', 'obligations'}
                     and item['protocol'] == 'source-cohort-v1' and item['name'] in KINDS
                     and item['contract'] == CONTRACTS[item['name']] and item['obligations'] == [],
                     'Unknown source-cohort command fields or runtime contract')
    kind = item['name']
    if kind == 'android_jvm':
        receipts.require(all(retention.digest(retention.git_bytes(candidate, path) or b'') == digest
                             for path, digest in CURRENT_JVM_BUILD_INPUTS.items()),
                         'Current JVM toolchain, dependency or discovery configuration differs')
        receipts.require(set(item['inventory']) == {'metadata'}, 'Unknown JVM inventory fields')
        ids = jvm_metadata(proof, item['inventory']['metadata'], candidate)
    else:
        ids = list(discovery(candidate, kind))
        receipts.require(item['inventory'] == {'ids': ids} and ids, 'Current Python source discovery differs')
    universe = set(ids)
    receipts.require(isinstance(item['records'], list) and item['records'], 'No actual source-cohort executions')
    sequences, retained, owners, failures, passes, opaque_sources = set(), {}, {}, {}, {}, []
    legacy_sequences = {}
    source_failures, source_repairs = {}, {}
    original_jvm_failures = set()
    linked_ci_sequences = {}
    for record in sorted(item['records'], key=lambda row: row['sequence']):
        receipts.require(set(record) == {'sequence', 'source_commit', 'origin', 'changes', 'log', 'coverage', 'exit_code'},
                         'Unknown source-cohort execution fields')
        sequence = record['sequence']
        receipts.require(type(sequence) is int and sequence > 0 and sequence not in sequences,
                         'Ambiguous source-cohort execution chronology')
        sequences.add(sequence)
        receipts.sha(record['source_commit'])
        origin = record['origin']
        receipts.require(set(origin) == {'kind', 'identity', 'command', 'execution'}
                         and origin['kind'] in {'authenticated-ci', 'attested-local'}
                         and origin['identity'], 'Missing actual source-cohort execution origin')
        execution = origin['execution']
        receipts.require(execution.get('mode') == 'source-cohort-v1' and execution.get('kind') == kind,
                         'Execution belongs to another source-cohort command')
        invalid = changed_inputs(record, candidate, proof['logs'])
        receipts.require(invalid <= universe, 'Unknown source-impact identity')
        raw = blob(proof, record['log']); text = clean_log(raw.decode())
        coverage = record['coverage']; mode = coverage['mode']
        opaque = mode in {'python-discovery', 'jvm-whole-command'}
        exact_selected_repairs = set()
        source_variant_failure = False
        legacy = execution.get('legacy_capture')
        if legacy in {row['raw_sha256'] for row in LINKED_PYTHON_CI_RUNS.values()}:
            run, _policy = linked_python_ci_policy(record)
            receipts.require(run not in linked_ci_sequences, 'Linked CI capture enrolled twice')
            linked_ci_sequences[run] = sequence
            pair = linked_python_ci_pair(proof, discovery)
            actual = {key.split(':', 1)[1]: value for key, value in pair[run].items()
                      if key.startswith(kind + ':')}
            receipts.require(actual and set(actual) <= universe, 'Linked native identity left current cohort')
        elif legacy is not None and kind == 'android_jvm':
            capture = LEGACY_JVM.get(legacy)
            receipts.require(capture is not None, 'Unreviewed original JVM capture')
            actual = legacy_jvm_outcomes(proof, record, capture, raw, universe)
            if capture.get('prior_failure'):
                original_jvm_failures.add(capture['prior_failure']['id'])
            receipts.require(legacy not in legacy_sequences, 'Original JVM capture enrolled twice')
            legacy_sequences[legacy] = sequence
            if capture['mode'] == 'jvm-selected':
                opaque_sources.append({'sequence': sequence, 'source_commit': record['source_commit'],
                                       'log_sha256': legacy, 'membership_ids': sorted(test_id for test_id in set(actual) - invalid if actual[test_id] == 'opaque-pass'),
                                       'runtime_named_outcomes': None})
        elif legacy is not None:
            capture = LEGACY_PYTHON.get(legacy)
            receipts.require(kind != 'android_jvm' and capture is not None
                             and record['log']['sha256_ref'] == legacy
                             and record['source_commit'] == capture['source']
                             and origin['kind'] == 'attested-local' and origin['identity'] == capture['tool']
                             and record['exit_code'] == capture.get('exit_code', 0) and origin['command'] is None
                             and execution == {'mode': 'source-cohort-v1', 'kind': kind, 'legacy_capture': legacy,
                                               **({'actual_execution_source_commit': None} if 'actual_execution_source_commit' in capture else {})}
                             and coverage == {'mode': capture['mode']},
                             'Historical local capture differs from finite original authority')
            originals = {suite: set(discovery(record['source_commit'], suite))
                         for suite in ('validation', 'operations')}
            own, other = originals[kind], originals['operations' if kind == 'validation' else 'validation']
            if mode == 'python-named':
                actual = python_rows(text, kind, own, other)
                receipts.require(all(value == 'pass' for value in actual.values()),
                                 'Original named capture did not pass')
            else:
                receipts.require(mode in {'python-selected', 'python-selected-partial'}, 'Unknown finite Python capture format')
                all_originals = own | other
                selected = set()
                for selector in capture['selectors']:
                    matching = {test_id for test_id in all_originals
                                if test_id == selector or test_id.startswith(selector + '.')}
                    receipts.require(matching, 'Original selector has no source-archived membership')
                    selected.update(matching)
                selected_outcomes = selected_python_outcomes(text, capture, selected)
                actual = {test_id: value for test_id, value in selected_outcomes.items() if test_id in own}
                exact_selected_repairs = set(capture.get('exact_selected_repair', []))
                source_variant_failure = mode == 'python-selected-partial'
                opaque_sources.append({'sequence': sequence, 'source_commit': record['source_commit'],
                                       'log_sha256': legacy, 'membership_ids': sorted(test_id for test_id in set(actual) - invalid if actual[test_id] == 'opaque-pass'),
                                       'runtime_named_outcomes': None})
            receipts.require(set(actual) <= universe, 'Original Python capture contains removed unit')
            if mode == 'python-named':
                counts = re.findall(r'^Ran (\d+) tests? in [0-9.]+s$', text, re.M)
                receipts.require(counts == [str(capture['count'])], 'Finite original named count differs')
        elif opaque:
            expected = BASE_JVM_LOG if kind == 'android_jvm' else BASE_PYTHON_LOG
            receipts.require(origin['kind'] == 'authenticated-ci' and record['source_commit'] == BASE_SOURCE
                             and record['log']['sha256_ref'] == expected and record['exit_code'] is None
                             and execution == {'mode': 'source-cohort-v1', 'kind': kind, 'job_status': 'success'},
                             'Opaque source-cohort baseline is outside finite authenticated authority')
            if kind == 'android_jvm':
                receipts.require(set(coverage) == {'mode', 'metadata'}
                                 and coverage['metadata'] == {'sha256_ref': BASE_JVM_METADATA},
                                 'Opaque JVM baseline metadata differs')
                old_ids = jvm_metadata(proof, coverage['metadata'], BASE_SOURCE)
                receipts.require(all(re.search(r'^> Task :app:' + task + r'\s*$', text, re.M)
                                     for task in ('testDebugUnitTest', 'lintDebug'))
                                 and re.search(r'^BUILD SUCCESSFUL in ', text, re.M)
                                 and not re.search(r'^BUILD FAILED|^FAILURE:', text, re.M),
                                 'Original unfiltered JVM test/lint command did not succeed')
            else:
                receipts.require(set(coverage) == {'mode', 'ids'}, 'Unknown opaque Python coverage fields')
                old_ids = list(discovery(BASE_SOURCE, kind))
                receipts.require(coverage['ids'] == old_ids and len(old_ids) == (359 if kind == 'validation' else 1021),
                                 'Original source-archived Python cohort differs')
                summaries = re.findall(r'^Ran ' + str(len(old_ids)) + r' tests in [0-9.]+s\n\s*\nOK$', text, re.M)
                receipts.require(len(summaries) == 1, 'Original outer plain-OK Python summary ambiguous')
            receipts.require(set(old_ids) <= universe, 'Removed baseline unit requires explicit cohort reconciliation')
            actual = {test_id: 'opaque-pass' for test_id in old_ids}
            opaque_sources.append({'sequence': sequence, 'source_commit': BASE_SOURCE, 'log_sha256': expected,
                                   'membership_ids': sorted(set(old_ids) - invalid),
                                   'runtime_named_outcomes': None})
        else:
            receipts.require(origin['kind'] == 'attested-local'
                             and set(execution) in ({'mode', 'kind', 'binding'},
                                                    {'mode', 'kind', 'binding', 'custody_supplement'}),
                             'Named source-cohort capture lacks actual local binding')
            command, binding = current_binding(proof, record, raw, candidate, archive_digest)
            if mode == 'python-named':
                receipts.require(kind != 'android_jvm' and set(coverage) == {'mode'}, 'Unknown named Python coverage')
                text = clean_log(named_python_capture_text(
                    raw, record['source_commit'], kind, blob(proof, execution['binding'])))
                actual = python_rows(text, kind, universe, set(discovery(candidate, 'operations' if kind == 'validation' else 'validation')))
                receipts.require(len(command) > 4
                                 and named_python_entrypoint(kind, command, binding,
                                                            blob(proof, execution['binding']), raw)
                                 and command[1:4] == ['-m', 'unittest', '-v']
                                 and all(re.fullmatch(r'tests\.(?:validation|operations)\.[A-Za-z0-9_]+\.[A-Za-z0-9_]+\.[A-Za-z0-9_]+', token) for token in command[4:])
                                 and len(command[4:]) == len(set(command[4:])),
                                 'Named Python capture did not select verbose unittest')
                original_own = set(discovery(record['source_commit'], kind))
                original_other = set(discovery(record['source_commit'], 'operations' if kind == 'validation' else 'validation'))
                selected = {token.removeprefix('tests.validation.').removeprefix('tests.operations.') for token in command[4:]}
                receipts.require(selected <= original_own | original_other
                                 and set(actual) == selected & universe
                                 and re.search(r'^Ran ' + str(len(selected)) + r' tests? in ', text, re.M),
                                 'Actual Python source selection differs from native outcomes')
            else:
                receipts.require(kind == 'android_jvm' and mode == 'junit-xml'
                                 and set(coverage) == {'mode', 'xml_archive'}, 'Unknown actual JVM coverage')
                receipts.require(all(retention.digest(retention.git_bytes(record['source_commit'], path) or b'') == digest
                                     for path, digest in ORIGINAL40_JVM_BUILD_INPUTS.items()),
                                 'Original JVM build configuration differs from reviewed contract')
                actual = junit_archive(proof, coverage['xml_archive'], binding['xml_archive_sha256'], universe)
                selectors = modern_jvm_argv(command)
                expected_selectors = sorted(test_id.replace('/', '.') for test_id in actual)
                receipts.require(sorted(selectors) == expected_selectors
                                 and ':app:testDebugUnitTest' in command
                                 and re.search(r'^> Task :app:testDebugUnitTest[ \t]*$', text, re.M)
                                 and not re.search(r'^> Task :app:testDebugUnitTest (?:UP-TO-DATE|FROM-CACHE|SKIPPED|NO-SOURCE)', text, re.M)
                                 and bool(re.search(r'^BUILD SUCCESSFUL in ', text, re.M)) == (record['exit_code'] == 0)
                                 and (record['exit_code'] == 0 or re.search(r'^BUILD FAILED in ', text, re.M)),
                                 'Actual JVM selectors or Gradle completion differ from native XML outcomes')
            receipts.require(type(record['exit_code']) is int
                             and (record['exit_code'] == 0) == all(value == 'pass' for value in actual.values()),
                             'Actual local command disposition contradicts native outcomes')
        for test_id, outcome in actual.items():
            if source_variant_failure and outcome == 'fail':
                source_failures[(record['source_commit'], test_id, legacy)] = sequence
            elif outcome == 'fail':
                failures[test_id] = sequence
            if outcome == 'opaque-pass' and test_id in exact_selected_repairs:
                source_repairs[(record['source_commit'], test_id, legacy)] = sequence
            if test_id in invalid:
                continue
            retained[test_id] = outcome
            owners[test_id] = sequence
            if outcome == 'pass' or (outcome == 'opaque-pass' and test_id in exact_selected_repairs):
                passes[test_id] = sequence
    if linked_ci_sequences:
        first, repair = sorted(LINKED_PYTHON_CI_RUNS)
        receipts.require(set(linked_ci_sequences) == {first, repair}
                         and linked_ci_sequences[first] < linked_ci_sequences[repair],
                         'Linked original failures and repairs must both remain ordered')
    require_source_bound_repairs(source_failures, source_repairs)
    receipts.require(all(test_id in passes for test_id in original_jvm_failures),
                     'Original named JVM failure lacks a current-applicable native pass')
    if kind == 'android_jvm':
        require_legacy_chronology(legacy_sequences)
    receipts.require(set(retained) == universe and all(value in {'pass', 'opaque-pass'} for value in retained.values())
                     and all(passes.get(test_id, 0) > sequence for test_id, sequence in failures.items()),
                     'Incomplete source-applicable cohort or failure without later actual named pass')
    # Opaque membership and named overlays form a disjoint final partition.
    named = sorted(test_id for test_id, outcome in retained.items() if outcome == 'pass')
    for source in opaque_sources:
        source['membership_ids'] = sorted(test_id for test_id in source['membership_ids']
                                          if retained[test_id] == 'opaque-pass' and owners[test_id] == source['sequence'])
    opaque_ids = [test_id for source in opaque_sources for test_id in source['membership_ids']]
    receipts.require(len(opaque_ids) == len(set(opaque_ids)) and set(opaque_ids).isdisjoint(named)
                     and set(opaque_ids) | set(named) == universe, 'Ambiguous opaque/named source-cohort partition')
    cohort_summaries = [{'source_commit': source['source_commit'], 'log_sha256': source['log_sha256'],
                         'membership_ids_sha256': retention.digest(json.dumps(source['membership_ids'], separators=(',', ':')).encode()),
                         'membership_count': len(source['membership_ids']), 'runtime_named_outcomes': None}
                        for source in opaque_sources]
    source_owners = None
    if kind == 'android_jvm':
        metadata = receipts.bounded_json(blob(proof, item['inventory']['metadata']))
        owners = sorted([row['class'], row['source_path'], row['source_sha256']] for row in metadata['classes'])
        source_owners = retention.digest(json.dumps(owners, separators=(',', ':')).encode())
    return {'mode': 'retained-source-cohort-v1', 'commands': [kind],
            'jvm_source_owners_sha256': source_owners,
            'ids_sha256': retention.digest(json.dumps(ids, separators=(',', ':')).encode()), 'ids_count': len(ids),
            'opaque_source_cohorts': cohort_summaries,
            'actual_named_pass_ids_sha256': retention.digest(json.dumps(named, separators=(',', ':')).encode()),
            'actual_named_pass_count': len(named),
            'contract': item['contract'], 'validator': retention.digest(Path(__file__).read_bytes())}
