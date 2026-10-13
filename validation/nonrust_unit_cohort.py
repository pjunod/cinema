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
JVM_BUILD_INPUTS = {
    'clients/android/gradle/wrapper/gradle-wrapper.properties': 'd01b41913834fb3cb14e71f69a08898e6a47e663b08f5c9b8c7e0772c38889d2',
    'clients/android/gradle/libs.versions.toml': 'ced36ebe0658327618e6b959982f31acc1baae352a46ee2b8cc23b15250f00c2',
    'clients/android/app/build.gradle.kts': '7780da0bd369665d9dd71d73efdf9246efdeaee21fe03cf7cc98688314a885c8',
    'clients/android/build.gradle.kts': '0aec8d2e01e88e3abdd6b4690201ddd107b10c05b6e86a4014315cc4ab64fd46',
    'clients/android/settings.gradle.kts': '5891ed8c25e4412da6dd0fff31d21626fa2bb3dde4cc6326e62d39eb9db79055',
}
CONTRACTS = {'validation': 'python-source-cohort-v1',
             'operations': 'python-source-cohort-v1',
             'android_jvm': 'junit4-gradle9-java21-or25-v1'}

# Finite original local captures. Unknown historical patch/binary measurements
# remain unknown. These exact log/source/tool identities authorize no new run.
LEGACY_PYTHON = {
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
    return command, binding


def validate(item, proof, candidate, discovery, changed_inputs, clean_log, archive_digest):
    receipts.require(set(item) == {'name', 'invocation', 'protocol', 'contract', 'inventory', 'records', 'obligations'}
                     and item['protocol'] == 'source-cohort-v1' and item['name'] in KINDS
                     and item['contract'] == CONTRACTS[item['name']] and item['obligations'] == [],
                     'Unknown source-cohort command fields or runtime contract')
    kind = item['name']
    if kind == 'android_jvm':
        receipts.require(all(retention.digest(retention.git_bytes(candidate, path) or b'') == digest
                             for path, digest in JVM_BUILD_INPUTS.items()),
                         'Current JVM toolchain, dependency or discovery configuration differs')
        receipts.require(set(item['inventory']) == {'metadata'}, 'Unknown JVM inventory fields')
        ids = jvm_metadata(proof, item['inventory']['metadata'], candidate)
    else:
        ids = list(discovery(candidate, kind))
        receipts.require(item['inventory'] == {'ids': ids} and ids, 'Current Python source discovery differs')
    universe = set(ids)
    receipts.require(isinstance(item['records'], list) and item['records'], 'No actual source-cohort executions')
    sequences, retained, owners, failures, passes, opaque_sources = set(), {}, {}, {}, {}, []
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
        legacy = execution.get('legacy_capture')
        if legacy is not None:
            capture = LEGACY_PYTHON.get(legacy)
            receipts.require(kind != 'android_jvm' and capture is not None
                             and record['log']['sha256_ref'] == legacy
                             and record['source_commit'] == capture['source']
                             and origin['kind'] == 'attested-local' and origin['identity'] == capture['tool']
                             and record['exit_code'] == 0 and origin['command'] is None
                             and execution == {'mode': 'source-cohort-v1', 'kind': kind, 'legacy_capture': legacy}
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
                receipts.require(mode == 'python-selected', 'Unknown finite Python capture format')
                all_originals = own | other
                selected = set()
                for selector in capture['selectors']:
                    matching = {test_id for test_id in all_originals
                                if test_id == selector or test_id.startswith(selector + '.')}
                    receipts.require(matching, 'Original selector has no source-archived membership')
                    selected.update(matching)
                summaries = re.findall(r'^Ran (\d+) tests? in [0-9.]+s\n\s*\nOK$', text, re.M)
                receipts.require(len(summaries) == 1 and int(summaries[0]) == capture['count'] == len(selected),
                                 'Original selected cohort differs from actual outer count')
                actual = {test_id: 'opaque-pass' for test_id in selected & own}
                opaque_sources.append({'sequence': sequence, 'source_commit': record['source_commit'],
                                       'log_sha256': legacy, 'membership_ids': sorted(set(actual) - invalid),
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
                             and set(execution) == {'mode', 'kind', 'binding'},
                             'Named source-cohort capture lacks actual local binding')
            command, binding = current_binding(proof, record, raw, candidate, archive_digest)
            if mode == 'python-named':
                receipts.require(kind != 'android_jvm' and set(coverage) == {'mode'}, 'Unknown named Python coverage')
                actual = python_rows(text, kind, universe, set(discovery(candidate, 'operations' if kind == 'validation' else 'validation')))
                receipts.require(len(command) > 4 and command[0] == binding['python_executable']
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
                                     for path, digest in JVM_BUILD_INPUTS.items()),
                                 'Original JVM build configuration differs from reviewed contract')
                actual = junit_archive(proof, coverage['xml_archive'], binding['xml_archive_sha256'], universe)
                selectors = [command[i + 1] for i, token in enumerate(command[:-1]) if token == '--tests']
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
            if outcome == 'fail':
                failures[test_id] = sequence
            if test_id in invalid:
                continue
            retained[test_id] = outcome
            owners[test_id] = sequence
            if outcome == 'pass':
                passes[test_id] = sequence
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
