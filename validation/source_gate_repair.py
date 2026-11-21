"""Authenticate reusable commands from an interrupted ordinary PR source gate."""

import base64
import binascii
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import zipfile

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import retained_lane_commands as native


ANDROID_COMPILE_ARGV = ('plurx-android-build ./gradlew --no-daemon '
                        'compileDebugUnitTestKotlin compileDebugUnitTestJavaWithJavac lintDebug')
ANDROID_COMPILE_TASKS = (
    '> Task :app:compileDebugUnitTestKotlin',
    '> Task :app:compileDebugUnitTestJavaWithJavac',
    '> Task :app:lintDebug',
)
ANDROID_VERIFY_ERROR = "ModuleNotFoundError: No module named 'tomllib'"
TASK_HEADER = re.compile(r'received task ([1-9]\d*) of job ([\w-]+), triggered by event: workflow_dispatch\b')


def execution_log(api, job, key):
    """Bind bounded, timestamped runner output to the authenticated job's actual task."""
    job_id = receipts.positive(job['id'])
    raw = api.bytes(f'/actions/jobs/{job_id}/logs')
    opened = TASK_HEADER.search(raw.decode('utf-8', 'strict')[:4096])
    receipts.require(opened is not None and opened.groups() == (str(receipts.positive(job['task_id'])), key),
                     'Source-gate repair log does not identify the actual workflow task')
    return raw


def android_compile_lint(raw, source):
    """Retain completed Android Gradle tasks independently of a failed verifier."""
    lines = [line.strip() for line in native.clean(raw.decode('utf-8', 'strict')).splitlines()]
    tasks = [i for i, line in enumerate(lines) if line.startswith(ANDROID_COMPILE_TASKS)]
    builds = [i for i, line in enumerate(lines) if re.fullmatch(r'BUILD SUCCESSFUL in .+', line)]
    receipts.require(lines.count(source) == 1 and lines.count(ANDROID_COMPILE_ARGV) == 1
                     and lines.count(ANDROID_VERIFY_ERROR) == 1 and lines.count('🏁  Job failed') == 1
                     and len(builds) == 1 and tuple(lines[i] for i in tasks) == ANDROID_COMPILE_TASKS,
                     'Missing complete historical Android compilation and lint')
    argv = lines.index(ANDROID_COMPILE_ARGV)
    build = builds[0]
    error = lines.index(ANDROID_VERIFY_ERROR)
    start = lines.index(source)
    receipts.require(start < argv < tasks[0] < tasks[1] < tasks[2] < build < error < lines.index('🏁  Job failed')
                     and not any(re.match(r'BUILD FAILED|FAILURE: Build failed|> Task .*:test\S*UnitTest\b', line)
                                 for line in lines[argv:error])
                     and any(re.fullmatch(r'File "/usr/lib/python3\.10/runpy\.py", line \d+, in _run_module_as_main',
                                          line) for line in lines[build:error])
                     and 'ids = verify_adopted(Path.cwd(), proof[\'command_units\'][\'android_jvm\'])' in lines[build:error]
                     and 'import tomllib' in lines[build:error],
                     'Android failure did not occur solely in the subsequent metadata verifier')
    images = [line.removeprefix('Digest: ') for line in lines[:argv]
              if re.fullmatch(r'Digest: sha256:[0-9a-f]{64}', line)]
    receipts.require(len(images) == 1, 'Original Android compile image digest is missing or contradictory')
    return {'source_commit': receipts.sha(source), 'log_sha256': retention.digest(raw),
            'compile_tasks': list(ANDROID_COMPILE_TASKS), 'test_bodies': 0, 'image_digest': images[0]}


PREDECESSOR = {
    'run': 8816,
    'head': 'ae5605765e2bb99a8424ecec60ef29f22e056a0b',
    'base': '214e4fb7821d494d84bdf368a355075e3ebed86c',
}
WORKFLOW = 'ci.yml'
WORKFLOW_SHA256 = '2d07ac2f4b4501cc1647cacfe26e69bcb7e0470de6f860bef2a235b973246e16'


def prior_run(api, value, binding, repository, *, current_run=None):
    """Bind the exact earlier manual run; its main can precede current main."""
    run_id = receipts.positive(value['predecessor_run'])
    receipts.require(run_id == PREDECESSOR['run'] and (current_run is None or run_id < current_run),
                     'Source gate continuation requires the reviewed earlier run')
    run = api.get(f'/actions/runs/{run_id}')
    event = receipts.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    prior = receipts.sha(run['commit_sha'])
    branch = binding['head_ref']
    receipts.require(run['id'] == run_id and run['workflow_id'] == 'ci.yml'
                     and run['status'] in {'running', 'failure', 'cancelled'} and run['repository']['id'] == repository
                     and prior == PREDECESSOR['head']
                     and run['trigger_event'] == 'workflow_dispatch' and run['prettyref'] == branch
                     and event['ref'] == 'refs/heads/' + branch
                     and inputs['promotion_pr'] == str(binding['pull_request'])
                     and inputs['promotion_head_sha'] == prior
                     and inputs['promotion_base_sha'] == PREDECESSOR['base']
                     and not inputs.get('predecessor_run'),
                     'Earlier run is not the reviewed manual source gate for this PR')
    return run_id, {**binding, 'head_sha': prior, 'base_sha': PREDECESSOR['base']}, inputs


def predecessor_attachment(api, descriptor, binding, pull):
    from validation import paperwork_continuation as paperwork
    expected = {'sha256', 'attachment_id', 'attachment_uuid'}
    receipts.require(set(descriptor) == expected, 'Invalid predecessor object descriptor')
    raw = retention.attachment_bytes(api, binding['pull_request'], descriptor['attachment_id'],
                                     descriptor['attachment_uuid'], descriptor['sha256'])
    return paperwork.local_attestation(api, raw, descriptor, binding, binding['head_sha'], pull)


KERNEL_DIAGNOSTIC = r'Nextest kernel: [A-Za-z0-9.+_-]{1,80}'
SPLIT_PREDECESSOR_RESULTS = {
    'test_manual_source_guard.ManualSourceGuardTests.test_actual_authenticated_gate_omits_only_duplicate_lane_and_prints_real_provenance':
        re.escape('Reused actual ci / Main promotion gate run 12 job 13: https://forge/actual-ci/12'),
    'test_nextest_shard_continuation.NextestShardContinuationTests.test_failed_child_pidfd_signal_still_kills_reaps_and_closes_owned_probe':
        re.escape('Adopted tv_navigation: original run 10 job 8 source original\n'
                  'Adopted desktop_macos: original run 10 job 8 source original\n') + KERNEL_DIAGNOSTIC,
    'test_nextest_shard_continuation.NextestShardContinuationTests.test_owned_child_pidfd_probe_observes_full_lifecycle_and_reaps':
        KERNEL_DIAGNOSTIC + re.escape(
            '\nNextest PIDfd: self/child open, signal STOP/CONT/TERM, waitid stop/continue/exit passed; child reaped'),
    'test_nextest_shard_continuation.NextestShardContinuationTests.test_pidfd_capability_refuses_enosys_before_any_body':
        KERNEL_DIAGNOSTIC,
}


def predecessor_python_lines(raw):
    # A few named fixtures write known diagnostics while unittest is printing
    # their outcome. Recover the contiguous terminal only in that exact form.
    text = native.clean(raw.decode('utf-8', 'strict'))
    for identity, diagnostics in SPLIT_PREDECESSOR_RESULTS.items():
        method = identity.rsplit('.', 1)[1]
        prefix = f'{method} ({identity}) ... '
        pattern = re.compile('^' + re.escape(prefix) + diagnostics + r'\nok$', re.M)
        matches = list(pattern.finditer(text))
        receipts.require(len(matches) <= 1, 'Repeated interleaved Python outcome')
        text = pattern.sub(lambda _: prefix + 'ok', text)
    return text.encode()


def python_coverage(api, descriptor, old, pull, run, raw, inventories, root):
    from validation import main_python_continuation as main
    from validation import paperwork_continuation as paperwork
    from validation import selected_apple_preflight, selected_node_preflight

    source = old['head_sha']
    record, repository = predecessor_attachment(api, descriptor, old, pull)
    receipts.require(record['version'] == main.NODE_PREFLIGHT_POLICY,
                     'Earlier source gate did not preserve typed command provenance')
    plan = main.prepare(api, record, old, source, repository, inventories=inventories)
    paperwork.bind_selection(plan, old['base_sha'], source)
    plan['evidence_sha256'] = descriptor['sha256']
    observed, runtime = paperwork.current_outcomes(predecessor_python_lines(raw), plan, capture_runtime=True)
    rules, retained_rules, directly_due = paperwork.rule_inventory(plan, root)
    receipts.require(directly_due <= set(observed) and set(rules) == retained_rules | directly_due,
                     'Earlier preflight did not complete exact paperwork coverage')
    node = selected_node_preflight.coverage(raw, plan)
    reader = selected_apple_preflight.reader_build_proof(api, repository, old, plan, run)
    return {'direct_passes': sorted(observed), 'retained_passes': {key: value for suite in plan['suites'].values()
                                for key, value in suite['retained_source_records'].items()},
            'paperwork': sorted(rules), 'node': node, 'dependency_runtime': runtime,
            'apple': plan['apple_preflight'], 'ios_reader_build': reader,
            'root_ownership': plan['root_ownership'], 'formal_retained': plan['formal_preflight']}


ANDROID_INPUTS = ('clients/android', 'Makefile')
ROSTER_MEMBER = re.compile(r"classes/(?:[A-Za-z0-9_$-]+/)*[A-Za-z0-9 _$'-]+\.class")
ROSTER_JAR = 'robolectric-4.17.jar'
ORIGINAL_ANDROID_CAPTURE = {
    'completion.json': '37454b6815fe522b2f9e239550deb5423e775fb4f8c6c8b23485c7610ae57473',
    'compiled-inventory-after.json': '9d80915090589a8d65d70f080fd1fc88bb9dd4eeca235fdd83031a46b4b432c7',
    'unit-class-hashes-after.json': '87692456c591843293aa6f7410d0a214deecf760cd87afb7e0479d27cdb75a87',
}


def original_android_capture(envelope, unit):
    from validation import qualification_unit_continuation as commands
    raw = base64.b64decode(envelope['capture_receipts'], validate=True)
    receipts.require(len(raw) <= 256 * 1024, 'Original Android capture archive exceeds bound')
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        receipts.require(len(entries) == len(ORIGINAL_ANDROID_CAPTURE)
                         and {entry.filename for entry in entries} == set(ORIGINAL_ANDROID_CAPTURE)
                         and sum(entry.file_size for entry in entries) <= 1024 * 1024,
                         'Android capture archive contains unrelated evidence')
        records = {}
        for name, digest in ORIGINAL_ANDROID_CAPTURE.items():
            data = archive.read(name)
            receipts.require(retention.digest(data) == digest, 'Original Android runtime custody differed: ' + name)
            records[name] = receipts.bounded_json(data)
    capture = records['completion.json']
    inventory = records['compiled-inventory-after.json']
    names = sorted(row['class'] + '/' + name[len(row['class']) + 1:]
                   for row in inventory['classes'] for name in row['ids'])
    receipts.require(capture['source_commit'] == envelope['source_commit']
                     and capture['source_archive_sha256'] == envelope['source_archive_sha256']
                     and commands.source_archive_digest(capture['source_commit']) == capture['source_archive_sha256']
                     and capture['android_tree'] == envelope['compiler_inputs']['clients/android']
                     and capture['metadata_only'] is True and capture['runtime_outcomes'] is None
                     and capture['source_clean_start'] is True and capture['source_clean_end'] is True
                     and capture['compiled_test_class_hashes_identical'] is True and capture['exit_code'] == 0
                     and capture['compiled_class_count'] == len(envelope['compiled_class_sha256'])
                     and capture['compiled_test_methods'] == len(names) == unit['ids_count']
                     and inventory['unit_runtime_outcomes'] is None and len(set(names)) == len(names)
                     and retention.digest(canonical(names)) == unit['ids_sha256']
                     and records['unit-class-hashes-after.json'] == envelope['compiled_class_sha256']
                     and envelope['completion_sha256'] == ORIGINAL_ANDROID_CAPTURE['completion.json']
                     and envelope['compiled_inventory_sha256'] == ORIGINAL_ANDROID_CAPTURE['compiled-inventory-after.json'],
                     'Original Android class capture does not prove the adopted compilation census')
    return {'origin': capture['source_commit'], 'source_archive_sha256': capture['source_archive_sha256'],
            'compiler_sha256': capture['java_binary_after']['sha256'],
            'capture_sha256': retention.digest(raw), 'receipts': ORIGINAL_ANDROID_CAPTURE}


def android_witness(api, descriptor, binding, pull, old, command, root):
    from validation import jvm_unit_discovery as discovery
    from validation import paperwork_continuation as paperwork
    from validation import qualification_unit_continuation as commands

    source, repository = predecessor_attachment(api, descriptor, old, pull)
    proof = retention.expanded_document(source, selected_pr=True)
    receipts.require(source['source_commit'] == old['head_sha'] and source.get('version') == 2
                     and not proof['lanes'] and [row['name'] for row in proof['command_units']['commands']] == ['android_jvm'],
                     'Earlier gate Android evidence contains an unrelated lane')
    commands.authenticate(api, proof['command_units'], proof, repository)
    _, adopted = retention.validate(source, old['head_sha'], selected_pr=True)
    unit = adopted['android_jvm']
    receipts.require(unit['actual_named_pass_count'] == unit['ids_count'] and not unit['opaque_source_cohorts']
                     and unit.get('selected_pr_discovery_contract') == discovery.SELECTED_PR_DISCOVERY,
                     'Android predecessor is missing actual named coverage')

    snapshot = binding['android_snapshot']
    raw = retention.attachment_bytes(api, binding['pull_request'], snapshot['attachment_id'],
                                     snapshot['attachment_uuid'], snapshot['sha256'])
    envelope = receipts.bounded_json(raw)
    original = receipts.sha(envelope['source_commit'])
    historical = {**old, 'head_sha': original}
    paperwork.local_attestation(api, raw, snapshot, historical, original, pull)
    receipts.require(envelope['kind'] == 'android-source-gate-compiled-census'
                     and envelope['version'] == 1 and envelope['junit_discovery_count'] == unit['ids_count']
                     and envelope['compiler_inputs'] == {path: paperwork.git(root, 'rev-parse', original + ':' + path)
                                                         for path in ANDROID_INPUTS},
                     'Unknown original compiled Android witness')
    for revision in (old['head_sha'], binding['head_sha']):
        receipts.require(envelope['compiler_inputs'] == {path: paperwork.git(root, 'rev-parse', revision + ':' + path)
                                                         for path in ANDROID_INPUTS},
                         'Android compiler inputs changed from the original captured source')
    for field in ('source_archive_sha256', 'completion_sha256', 'compiled_inventory_sha256', 'archive_sha256'):
        receipts.require(retention.DIGEST.fullmatch(envelope[field]) is not None, 'Missing Android compiler custody digest')
    try:
        archive = base64.b64decode(envelope['archive'], validate=True)
    except (TypeError, binascii.Error):
        raise receipts.ReceiptError('Invalid Android compiled census archive') from None
    receipts.require(retention.digest(archive) == envelope['archive_sha256'], 'Android compiler archive digest differs')
    classfiles, jar = compiled_members(archive, envelope['compiled_class_sha256'])
    receipts.require(retention.digest(jar) == discovery.ROBOLECTRIC_JAR_SHA256,
                     'Android runtime JAR differs from the reviewed exact dependency')
    captured = original_android_capture(envelope, unit)
    # Linux compilation/lint and the preserved source-equivalent original class bytes
    # have different actual origins. Keep both in the current repair receipt.
    custody = {'compile_lint': command, 'source_commit': original,
               'archive_sha256': envelope['archive_sha256'], 'evidence_sha256': snapshot['sha256'],
               'compiled_class_count': len(classfiles), 'ids_count': unit['ids_count'],
               'ids_sha256': unit['ids_sha256'], 'jvm_source_owners_sha256': unit['jvm_source_owners_sha256'],
               'compiler_inputs': envelope['compiler_inputs'], 'original_capture': captured}
    return custody, unit, (classfiles, jar)


def compiled_members(raw, expected):
    receipts.require(isinstance(expected, dict) and 0 < len(expected) <= 4096,
                     'Android compiled roster outside bound')
    try:
        with zipfile.ZipFile(io.BytesIO(raw)) as archive:
            entries = archive.infolist()
            receipts.require(len(entries) == len(expected) + 1 and len({item.filename for item in entries}) == len(entries)
                             and sum(item.file_size for item in entries) <= 16 * 1024 * 1024
                             and all(0 < item.file_size <= 8 * 1024 * 1024 and not item.is_dir() for item in entries)
                             and all(item.filename == ROSTER_JAR or ROSTER_MEMBER.fullmatch(item.filename) for item in entries)
                             and {item.filename for item in entries} == {ROSTER_JAR} | {'classes/' + p for p in expected},
                             'Unsafe or incomplete Android compiler archive')
            classfiles = {path: archive.read('classes/' + path) for path in expected}
            jar = archive.read(ROSTER_JAR)
    except receipts.ReceiptError:
        raise
    except (OSError, RuntimeError, zipfile.BadZipFile):
        raise receipts.ReceiptError('Unreadable Android compiler archive') from None
    receipts.require({path: retention.digest(raw) for path, raw in classfiles.items()} == expected,
                     'Android compiler class inventory contradicts captured bytes')
    return classfiles, jar


FORMAL_SOURCE_REFINEMENTS = {
    'durable-jobs': {
        # Only the already-measured title test's panic diagnostics changed.
        'crates/plurx-core/tests/store_contract/dv_title_jobs.rs': (
            '5bcf53c4f3bd8a98b21c6940a102b7c80e8ace99524c07f8d47780ea3bc79bcc',
            '95646795c4013d1dd79a799aa26e7dc2cae324dd32d5b32b5b105f9a85ea4ec0',
        ),
    },
    'operation-progress': {
        # This initialization adds an empty, Linux-only backend identity.
        # The progress protocol, synchronization, and wakeup paths are unchanged.
        'crates/plurxd/src/transcode/manager/construct.rs': (
            'd56b41d8524d65b5ae52b5dbe31d63ffa561d8057e6ec9e0f42ed4da6aa9493c',
            '16f88ecf66ee3e256536cec33dad37a72d2aa5cc3dd304fd30acacb14334cb3c',
        ),
    },
}


CLUSTER_FORMAL_SOURCE = 'a3b158a6c7514e2fdcfd406cc1147c3cdaba4e70'
CLUSTER_FORMAL_MAIN = '32f30addc4987648b50b2443976164fca6030044'
CLUSTER_FORMAL_ARCHIVE_SHA256 = '2f7f35a07590b64c2a70981508f35c3645094dea4bf4bc5f64539687555e3f4c'
# PR1732's reviewed head has the exact tree of merged main 32f30addc. The combined
# source adds descriptor custody and macOS FEL support. Existing Linux operation
# progress/fence transitions, the formal models and their mappings are unchanged.
CLUSTER_FORMAL_REFINEMENTS = {'durable-jobs': {'crates/plurx-core/tests/store_contract.rs': ('21dd32d4770dfaced49b10653fef5a9a306371e1f8e6a9e175246f792de7015f',
                                                                '8e479af1916c91dcee8c1b7286536aa012206449b560ca162dc6e2671276984c'),
                  'crates/plurxd/src/background_jobs.rs': ('6e92572cc988a94a20929ddfb77409deace22a7be8d7ffffdc96daa128a5ad69',
                                                           'e9db0d8ff3d92c80cd6253a8ca53474331f1d9aef75814bea880cf624a204a87'),
                  'crates/plurxd/src/main.rs': ('bb5d9762fbbbe290b9e1019e9a065d4d6f54b73dd5c919ad9363f4ef5cdfbbd3',
                                                '4054953f31c6245ee0460a37e74410e2f8302eb687909474f59e3d99d7ac64c6'),
                  'crates/plurxd/src/source_custody.rs': ('c330d2b9d1fdc071cde44d4d9343dea4ecdfbd8add3d2f89f2582faacb039a8d',
                                                          '46598edb65395ba28bd9892c65782f01acbe44dbb3b67da53f8023b36b5c9e70'),
                  'crates/plurxd/src/state.rs': ('4d0b84520092fe1f2ed8f8f6ef6cf3415819a8ad88bbb2698f1ba2e2bf5d6e9b',
                                                 '523fb8912bd479b035627627a89af53d2e0292a2fd81efc87fb117da44b3745c')},
 'operation-progress': {'crates/plurx-core/src/process/bounded.rs': ('7472ba52e7f2487bb0d9016d348a07ab573107fa746d437413aeabfce1a607b6',
                                                                     '2df515372d2f057498dbcae8dd2454419a2812f511a47f5ec80c3679f5cfa78d'),
                        'crates/plurxd/src/background_jobs.rs': ('6e92572cc988a94a20929ddfb77409deace22a7be8d7ffffdc96daa128a5ad69',
                                                                 'e9db0d8ff3d92c80cd6253a8ca53474331f1d9aef75814bea880cf624a204a87'),
                        'crates/plurxd/src/main.rs': ('bb5d9762fbbbe290b9e1019e9a065d4d6f54b73dd5c919ad9363f4ef5cdfbbd3',
                                                      '4054953f31c6245ee0460a37e74410e2f8302eb687909474f59e3d99d7ac64c6'),
                        'crates/plurxd/src/source_custody.rs': ('c330d2b9d1fdc071cde44d4d9343dea4ecdfbd8add3d2f89f2582faacb039a8d',
                                                                '46598edb65395ba28bd9892c65782f01acbe44dbb3b67da53f8023b36b5c9e70'),
                        'crates/plurxd/src/state.rs': ('4d0b84520092fe1f2ed8f8f6ef6cf3415819a8ad88bbb2698f1ba2e2bf5d6e9b',
                                                       '523fb8912bd479b035627627a89af53d2e0292a2fd81efc87fb117da44b3745c'),
                        'crates/plurxd/src/transcode/manager/construct.rs': ('70b9f483f060bfecd7b308c1652e1bd26a701e844e8acf48b61184800dec1214',
                                                                             '21d2f5d929f54a9fb6ef765d438b9afd7dad9165b2922fac8120881d6e339a35')},
 'worker-refinement': {'crates/plurxd/src/background_jobs.rs': ('6e92572cc988a94a20929ddfb77409deace22a7be8d7ffffdc96daa128a5ad69',
                                                                'e9db0d8ff3d92c80cd6253a8ca53474331f1d9aef75814bea880cf624a204a87')}}


def formal_input_projection(package, document, current, refinements=FORMAL_SOURCE_REFINEMENTS):
    old_inputs = document['input_sha256']
    observed = {path: retention.digest(retention.git_bytes(current, path)) for path in old_inputs}
    changes = {path: (expected, observed[path]) for path, expected in old_inputs.items() if expected != observed[path]}
    receipts.require(not changes or changes == refinements[package],
                     'Formal concrete source changed beyond the reviewed refinement: ' + package)
    return observed, changes


def formal_solver_profile(document):
    cases = []
    for case in document['configs']:
        receipts.require(case['complete'] is True and case['expected_verdict'] == case['actual_verdict']
                         and retention.DIGEST.fullmatch(case['config_sha256']) is not None,
                         'An actual formal configuration did not produce its required complete outcome')
        cases.append({key: case[key] for key in ('config', 'config_sha256', 'expected_verdict', 'purpose')})
    receipts.require(cases and len({case['config'] for case in cases}) == len(cases),
                     'Formal model configuration census is absent or repeated')
    return {**{key: document[key] for key in ('model_sha256', 'runner_sha256', 'toolchain', 'resource_policy', 'mapping_sha256')},
            'configs': cases}


def current_formal_mapping(package, current, current_inputs, profile):
    # Static correspondence only. The pinned solver has already completed all
    # the configurations; this command confirms the combined source mapping.
    runner = 'scripts/check-formal'
    runner_raw = retention.git_bytes(current, runner)
    receipts.require(Path(runner).read_bytes() == runner_raw
                     and retention.digest(runner_raw) == profile['runner_sha256'],
                     'Current formal runner differs from the actual solver producer')
    models = [sha for path, sha in current_inputs.items()
              if path.startswith('verification/' + package + '/') and path.endswith('.tla')]
    receipts.require(models == [profile['model_sha256']], 'Current formal model differs from the actual solver input')
    env = os.environ.copy()
    for secret in ('GITHUB_TOKEN', 'FORGEJO_TOKEN', 'ACTIONS_RUNTIME_TOKEN'):
        env.pop(secret, None)
    raw = subprocess.check_output(
        [sys.executable, '-B', runner, 'check-mapping', '--package', package],
        timeout=60, env=env,
    )
    mapping = receipts.bounded_json(raw)
    receipts.require(mapping['package'] == package and mapping['source_drift'] == []
                     and mapping['mapping_sha256'] == profile['mapping_sha256']
                     and mapping['input_sha256'] == current_inputs,
                     'Current static correspondence does not match the previous solver profile')
    for case in profile['configs']:
        matches = [digest for path, digest in current_inputs.items() if path.endswith('/' + case['config'])]
        receipts.require(matches == [case['config_sha256']], 'Current formal configuration digest differs')
    return {'mapping_sha256': mapping['mapping_sha256'], 'receipt_sha256': retention.digest(raw)}


def formal_coverage(api, artifacts, job, run, old, current):
    from validation import source_gate_landing as landing
    raw = landing.uploaded_archive(api, artifacts, f'formal-model-{run}-1', job, 'preflight')
    return formal_archive(raw, old, current, FORMAL_SOURCE_REFINEMENTS)


def cluster_formal_coverage(api, repository, base, current):
    from validation import main_python_continuation as ancestor
    from validation import main_unit_receipts as main
    from validation import selected_apple_preflight as native_artifact

    run, pr, artifact_id, job_id = 8849, 1732, 12845, 84657
    scope = {'repository': repository, 'pr': pr, 'branch': 'effort/shared-dv-hdr', 'base': 'main'}
    pull = api.get(f'/pulls/{pr}')
    receipts.require(base == CLUSTER_FORMAL_MAIN and pull['number'] == pr and pull['merged'] is True
                     and pull['merge_commit_sha'] == base and pull['head']['sha'] == CLUSTER_FORMAL_SOURCE
                     and pull['head']['repo']['id'] == pull['base']['repo']['id'] == repository
                     and pull['base']['ref'] == 'main' and ancestor.main_ancestor(CLUSTER_FORMAL_SOURCE, base)
                     and ancestor.main_ancestor(base, current), 'Formal cohort is outside current merged main')
    runs = api.pages('/actions/runs', {'workflow_id': main.WORKFLOW, 'ref': f'refs/pull/{pr}/head'},
                     'workflow_runs')
    selected = [row for row in runs if row['id'] == run]
    receipts.require(len(selected) == 1 and selected[0]['commit_sha'] == CLUSTER_FORMAL_SOURCE,
                     'Unknown actual merged-main formal run')
    main.authenticate_run(scope, selected[0])
    job = main.preflight_job(api, scope, run)
    receipts.require(job['id'] == job_id and job['status'] == 'success',
                     'Actual formal preflight job did not succeed')
    name = f'formal-model-{run}-1'
    matches = [row for row in api.pages(f'/actions/runs/{run}/artifacts') if row['name'] == name]
    receipts.require(len(matches) == 1 and matches[0]['id'] == artifact_id
                     and matches[0]['run_id'] == run and matches[0]['expired'] is False,
                     'Actual merged-main formal archive differs')
    log = api.bytes(f'/actions/jobs/{job_id}/logs')
    task = re.search(r'received task ([1-9]\d*) of job preflight, triggered by event: pull_request\b',
                     log[:4096].decode('utf-8','strict'))
    receipts.require(task is not None and int(task[1]) == job['task_id'],
                     'Formal artifact creator task differs')
    native_artifact.require_archive_creator(log, name, CLUSTER_FORMAL_ARCHIVE_SHA256)
    raw = api.bytes(f'/actions/artifacts/{artifact_id}/zip')
    receipts.require(len(raw) == matches[0]['size_in_bytes'] and retention.digest(raw) == CLUSTER_FORMAL_ARCHIVE_SHA256,
                     'Actual merged-main formal archive digest differs')
    result = formal_archive(raw, CLUSTER_FORMAL_SOURCE, current, CLUSTER_FORMAL_REFINEMENTS)
    return {package: {**proof, 'origin_run': run, 'origin_job': job_id, 'origin_artifact': artifact_id}
            for package, proof in result.items()}


def formal_archive(raw, old, current, refinements):
    result = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        receipts.require(len(entries) == len({entry.filename for entry in entries})
                         and sum(entry.file_size for entry in entries) <= receipts.MAX_BYTES,
                         'Unexpected formal evidence archive layout')
        for package in refinements:
            names = [entry.filename for entry in entries
                     if re.fullmatch(package + r'/[0-9]+T[0-9.]+Z/receipt\.json', entry.filename)]
            receipts.require(len(names) == 1, 'Missing actual formal model receipt: ' + package)
            document = receipts.bounded_json(archive.read(names[0]))
            model = receipts.bounded_json(archive.read(package + '/ci-inputs.json'))
            source = document['source_git']
            receipts.require(document['package'] == model['package'] == package
                             and document['status'] == 'complete' and document['mode'] == 'smoke'
                             and source['commit'] == old and source['dirty'] is False
                             and model['affected'] is True and model['input_sha256'] == document['input_sha256']
                             and model['mapping_sha256'] == document['mapping_sha256'],
                             'Earlier formal execution or its input mapping is incomplete')
            for path, digest in document['input_sha256'].items():
                receipts.require(retention.digest(retention.git_bytes(old, path)) == digest,
                                 'Historical model receipt lists another concrete input: ' + path)
            current_inputs, changes = formal_input_projection(package, document, current, refinements)
            profile = formal_solver_profile(document)
            result[package] = {'artifact_sha256': retention.digest(raw),
                               'receipt_sha256': retention.digest(archive.read(names[0])),
                               'source_commit': old, 'solver_profile': profile,
                               'current_input_sha256': current_inputs,
                               'reviewed_source_refinements': {path: list(pair) for path, pair in changes.items()}}
            result[package]['current_mapping'] = current_formal_mapping(package, current, current_inputs, profile)
    return result


def python_inventory(source):
    from validation import qualification_unit_continuation as commands
    return {kind: list(commands.frozen_python_ids(source, kind)) for kind in receipts.SUITES}


def python_selection(old, binding, outcomes, inventories, current_inventory, changes):
    from validation import main_python_continuation as main
    source, candidate = old['head_sha'], binding['head_sha']
    before = {kind + ':' + name for kind, ids in inventories.items() for name in ids}
    present = {kind + ':' + name for kind, ids in current_inventory.items() for name in ids}
    direct = {key: {'run': PREDECESSOR['run'], 'commit': source} for key in outcomes['direct_passes']}
    historical = {**outcomes['retained_passes'], **direct}
    receipts.require(not set(direct) & set(outcomes['retained_passes']) and set(historical) == before,
                     'Earlier positive Python rows do not partition the actual discovery')
    affected = main.origin_impacts(source, candidate, changes, before | present)
    applicability = receipts.SourceApplicability(candidate)
    retained = {key: row for key, row in historical.items() if key in present and key not in affected
                and applicability.matches(key, {'commit': source})}
    due = sorted(present - set(retained))
    return retained, due


def reviewed_inputs(old, current, paths):
    from validation import paperwork_continuation as paperwork
    trees = [{path: paperwork.git('.', 'rev-parse', revision + ':' + path) for path in paths}
             for revision in (old, current)]
    receipts.require(trees[0] == trees[1], 'Retained command inputs changed: ' + ', '.join(paths))
    return trees[1]


def job_definition(key):
    from validation import source_gate_landing as landing
    return {
        **{surface: (name, surface) for surface, name in landing.JOB_NAMES.items()},
        'rust': (landing.JOB_NAMES['rust'], 'check'),
        'rust_compile': ('fast Rust gate', 'rust_compile'),
        'cluster_store_legacy': ('replicated Store contracts (legacy)', 'cluster_store_legacy'),
        'cluster_transport_recovery_contracts': ('cluster transport recovery contracts', 'cluster_transport_recovery_contracts'),
        'cluster_transport_recovery_voter': ('cluster transport recovery voter role', 'cluster_transport_recovery_voter'),
        'cluster_transport_recovery_learner': ('cluster transport recovery learner role', 'cluster_transport_recovery_learner'),
        'android_apks': ('Build Android instrumentation APKs', 'android_apks'),
        'package_amd64': ('package and smoke (amd64)', 'package_smoke'),
        'package_arm64': ('package and smoke (arm64)', 'package_smoke'),
    }[key]


def successful_job(api, job, key):
    raw = execution_log(api, job, key)
    receipts.require(job['status'] == 'success'
                     and native.clean(raw.decode('utf-8', 'strict')).splitlines().count('🏁  Job succeeded') == 1,
                     'Required predecessor command did not complete: ' + key)
    return raw


HDR_CASE = ('test_absolute_pq_rgb_inverse_matches_independent_h273_patches '
            '(test_dv_rgb_conversion.DvRgbConversionTests.'
            'test_absolute_pq_rgb_inverse_matches_independent_h273_patches) ... ok')


def hdr_coverage(raw, prior, current):
    inputs = reviewed_inputs(prior, current, ('Makefile', '.github/actions/ffmpeg',
                             'tests/media/test_dv_rgb_conversion.py', 'tools/dv_quality/bt2020_pq_rgb48.filter'))
    text = native.clean(raw.decode('utf-8', 'strict'))
    command = ('PLURX_FFMPEG="/usr/lib/jellyfin-ffmpeg/ffmpeg" '
               "python3 -m unittest discover -s tests/media -p test_dv_rgb_conversion.py -v")
    begin = text.find(command)
    end = text.find('\nci-require-disk:', begin)
    window = text[begin:end] if 0 <= begin < end else ''
    platform = re.findall(r'^expected major=8 resolved=(ffmpeg version [^\n]+ apt=jellyfin-ffmpeg8 [^\n]+)$',
                          text[:begin], re.M)
    receipts.require(len(platform) == 1 and window.splitlines().count(command) == window.splitlines().count(HDR_CASE) == 1
                     and len(re.findall(r'^Ran 1 test in [0-9.]+s$', window, re.M)) == 1
                     and window.splitlines().count('OK') == 1,
                     'Previous Rust job did not complete its separate HDR H.273 command')
    return {'source_commit': prior, 'log_sha256': retention.digest(raw), 'inputs': inputs, 'case': HDR_CASE, 'runtime': platform[0]}


def topology_coverage(api, artifacts, job, prior, current, base, root):
    from validation import nextest_base_bridge
    from validation import source_gate_landing as landing

    # The native projection admits exactly the video/encoder changes audited
    # for the FEL merge. Neither topology workload decodes media. Its HLS
    # failure drill serves synthetic responses to the proxy.
    inputs = reviewed_inputs(prior, current, ('Makefile', 'crates/plurx-cluster-check', 'vendor/hiqlite',
                            'spikes/hiqlite-m0', 'crates/plurx-core/src/store', 'validation/ci_lane_receipt.py'))
    raw = retention.git_bytes(current, nextest_base_bridge.PROFILE)
    receipts.require(retention.digest(raw) == nextest_base_bridge.PROFILE_SHA256,
                     'Unknown topology runtime source projection')
    profile = receipts.bounded_json(raw)
    receipts.require(profile['source_before'] == prior, 'Topology projection names another execution source')
    selection = nextest_base_bridge.current_projection(profile, current, base)
    evidence = cluster_contract_receipt(api, artifacts, job, 'cluster_topology', prior, root)
    receipts.require(evidence['commands'] == ['make hiqlite-vendor-clippy', 'make cluster-harness-check',
                     'cargo clippy --locked --manifest-path spikes/hiqlite-m0/Cargo.toml --tests --no-deps -- -D warnings',
                     'make hiqlite-spike'], 'Missing required actual topology or isolated spike command')
    for archive in ('cluster-topology-semantic', 'cluster-failure-drills'):
        member = archive + '.json'
        result = landing.uploaded(api, artifacts, archive, job, 'cluster_topology', {member})
        report = result[member]
        receipts.require(report['build_sha'] == prior and report['evidence_scope'] == 'semantic_ci',
                         'Topology campaign has another source or workload scope')
        if archive == 'cluster-topology-semantic':
            receipts.require(report['topology_order'] == [3, 4]
                             and report['workload']['id'] == 'settings-put-and-local-catalogue-read-v3',
                             'Historical topology campaign did not exercise both required voter layouts')
        else:
            receipts.require(report['workload']['id'] == 'split-loss-quorum-write-v1'
                             and report['hls_backend_loss']['hls_session_survived_backend_loss'] is True,
                             'Historical failure campaigns lack their required workload or proxy success')
        evidence[archive] = result['__sha256__'][member]
    return {'source_commit': prior, 'job': job['id'], 'inputs': inputs,
            'projection': selection['projection'], **evidence}


def store_coverage(api, artifacts, job, raw, prior, current, root):
    from validation import nextest_base_bridge

    inputs = reviewed_inputs(prior, current, ('Makefile', 'crates/plurx-core/src/store', 'vendor/hiqlite',
                            'crates/plurx-core/tests/schema_lineage_upgrade.rs', 'validation/ci_lane_receipt.py'))
    changes = nextest_base_bridge.source_range(current, prior)
    test_paths = {path for path in changes if path.startswith('crates/plurx-core/tests/store_contract')}
    expected = FORMAL_SOURCE_REFINEMENTS['durable-jobs']
    receipts.require(test_paths == set(expected) and all(
        (nextest_base_bridge.file_digest(prior, path), nextest_base_bridge.file_digest(current, path)) == hashes
        for path, hashes in expected.items()), 'Store contract tests changed beyond two panic diagnostics')
    # The two exact unwrap-to-expect changes preserve the positive assertions.
    evidence = cluster_contract_receipt(api, artifacts, job, 'cluster_store_legacy', prior, root)
    text = native.clean(raw.decode('utf-8', 'strict'))
    suite = r'^[^\n]*Running tests/(store_contract|schema_lineage_upgrade)(?:/main)?\.rs .*\n'
    terminals = re.findall(suite + r'.*?^test result: ok\. ([1-9]\d*) passed; 0 failed;',
                           text, re.M | re.S)
    receipts.require(evidence['commands'] == ['make cluster-store-check']
                     and [name for name, _ in terminals] == ['store_contract', 'schema_lineage_upgrade'],
                     'Historical Store or schema-lineage suite did not actually finish')
    return {'source_commit': prior, 'job': job['id'], 'inputs': inputs,
            'retained_suites': dict(terminals), **evidence}


def android_device_source_projection(prior, candidate):
    header = (
        b'      predecessor_run:\n'
        b'        type: string\n'
        b'        description: "Earlier interrupted source gate identified in the live PR enrollment"\n'
        b'        required: false\n'
        b'        default: ""\n'
    )
    guard = b" && needs.preflight.outputs.android_device_retained != 'true'"

    def projected(source, path):
        raw = retention.git_bytes(source, path)
        if path == '.github/workflows/ci.yml':
            receipts.require(raw is not None and retention.digest(raw) == WORKFLOW_SHA256
                             and raw.count(header) == 1 and raw.count(guard) == 2,
                             'Unknown continuation Android workflow projection')
            return raw.replace(header, b'').replace(guard, b'')
        return raw

    before = retention.android_device_inputs(retention.git_bytes, prior)
    current = retention.android_device_inputs(projected, candidate)
    receipts.require(before == current, 'Android test or emulator action inputs changed')
    return current


def prior_coverage(api, value, binding, pull, root, current_run=None):
    from validation import main_preflight_adoption as main_commands
    from validation import paperwork_continuation as paperwork
    from validation import selected_apple_preflight as apple
    from validation import selected_root_custody as root_custody
    from validation import source_gate_landing as landing

    repo = api.get('')
    repository, owner = repo['id'], repo['owner']['id']
    run, old, inputs = prior_run(api, value, binding, repository, current_run=current_run)
    current, prior = binding['head_sha'], old['head_sha']
    receipts.require(paperwork.ancestor(root, prior, current)
                     and paperwork.ancestor(root, old['base_sha'], binding['base_sha']),
                     'Predecessor is outside the current source ancestry')
    old_inventory = python_inventory(prior)
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')

    def job_for(key):
        name, _ = job_definition(key)
        return landing.one_job(jobs, name, run, repository, owner)

    preflight = job_for('preflight')
    preflight_log = successful_job(api, preflight, 'preflight')
    python = python_coverage(api, receipts.bounded_json(inputs['python_unit_evidence'].encode()), old, pull,
                             run, preflight_log, old_inventory, root)
    impact, _ = predecessor_attachment(api, value['python_impact'], binding, pull)
    receipts.require(set(impact) == {'version', 'repository', 'pr', 'branch', 'source_commit', 'base_sha',
                                     'predecessor', 'changes'}
                     and impact['version'] == 'preflight-continuation-impact-v1'
                     and impact['predecessor'] == PREDECESSOR,
                     'Unknown current Python source projection')
    inventory = python_inventory(current)
    tests, python_due = python_selection(old, binding, python, old_inventory, inventory, impact['changes'])

    families_path = str(main_commands.MANIFEST)
    manifest = reviewed_inputs(prior, current, (families_path,))
    families = receipts.bounded_json(retention.git_bytes(current, families_path))['families']
    node_inputs = {}
    for script in native.WEB_SCRIPTS:
        witness = main_commands.witness('node:' + script, families)
        before = main_commands.input_digest(prior, witness)
        receipts.require(before == main_commands.input_digest(current, witness),
                         'Node program inputs changed since the actual positive: ' + script)
        node_inputs[script] = before
    reader = python['ios_reader_build']
    receipts.require(reader is not None and apple.reader_identity(prior)['reader_sha256'] ==
                     apple.reader_identity(current)['reader_sha256'], 'Current iOS reader packaging inputs changed')
    from validation import native_receipts
    native_inputs = {lane: native_receipts.input_digest(current, {'inputs': paths})
                     for lane, paths in native_receipts.INPUTS.items()}
    receipts.require(native_inputs == {lane: native_receipts.input_digest(prior, {'inputs': paths})
                                      for lane, paths in native_receipts.INPUTS.items()},
                     'Native Apple test or current reader packaging inputs changed')
    python['apple']['mobile']['current_inputs'] = apple.reviewed_mobile_inputs(
        python['apple']['mobile']['origin']['source_commit'], current)

    android_job = job_for('android_jvm')
    receipts.require(android_job['status'] == 'failure', 'Expected the earlier Android verifier-only failure')
    android = android_compile_lint(execution_log(api, android_job, 'android_jvm'), prior)
    android_descriptor = {'sha256': inputs['retained_units_sha256'],
                          'attachment_id': receipts.positive(int(inputs['retained_units_attachment_id'])),
                          'attachment_uuid': inputs['retained_units_attachment_uuid']}
    android, jvm, snapshot = android_witness(api, android_descriptor,
        {**binding, 'android_snapshot': value['android_snapshot']}, pull, old, android, root)
    device_inputs = android_device_source_projection(prior, current)
    apk_job, device_job = job_for('android_apks'), job_for('android_device')
    device = android_device_coverage(api, artifacts, apk_job, device_job,
                                     successful_job(api, apk_job, 'android_apks'),
                                     successful_job(api, device_job, 'android_device'))
    if binding['base_sha'] == CLUSTER_FORMAL_MAIN:
        formal = cluster_formal_coverage(api, repository, binding['base_sha'], current)
    else:
        formal = {**formal_coverage(api, artifacts, preflight, run, prior, current),
                  'worker-refinement': python['formal_retained']}
    current_root = reviewed_inputs(prior, current, (*root_custody.INPUTS, 'validation/selected_root_custody.py'))
    root_custody.coverage_shape(python['root_ownership'])
    retained_lanes = {'apple': {'native': python['apple'], 'current_build': reader, 'inputs': native_inputs},
                      'android_device': {**device, 'source_commit': prior, 'inputs': device_inputs}}
    topology = job_for('cluster_topology')
    successful_job(api, topology, 'cluster_topology')
    retained_lanes['cluster_topology'] = topology_coverage(api, artifacts, topology, prior, current,
                                                           binding['base_sha'], root)
    store = job_for('cluster_store_legacy')
    store_log = successful_job(api, store, 'cluster_store_legacy')
    retained_lanes['cluster_store'] = store_coverage(api, artifacts, store, store_log, prior, current, root)
    rust = job_for('rust')
    hdr = hdr_coverage(execution_log(api, rust, 'check'), prior, current)
    return {'predecessor': PREDECESSOR, 'preflight_job': preflight['id'],
            'preflight_log_sha256': retention.digest(preflight_log),
            'retained_lanes': retained_lanes, 'python': tests, 'python_due': python_due,
            'node': {'coverage': python['node'], 'input_digests': node_inputs, 'manifest': manifest},
            'formal': formal,
            'root_ownership': {'coverage': python['root_ownership'], 'inputs': current_root},
            'android': android, 'hdr': hdr}, jvm, snapshot


def dispatch_inputs(inputs, binding, value):
    expected = {'promotion_pr': str(binding['pull_request']), 'promotion_head_sha': binding['head_sha'],
                'promotion_base_sha': binding['base_sha'], 'predecessor_run': str(value['predecessor_run'])}
    receipts.require({key: item for key, item in inputs.items() if item and key != 'workspace_rust_evidence'}
                     == expected and receipts.bounded_json(inputs.get('workspace_rust_evidence', '').encode())
                     == value['workspace_rust_evidence'],
                     'Continuation dispatch differs from live PR enrollment')


def repair_event(api, number, candidate, base, root, run_id, states):
    from validation import paperwork_continuation as paperwork
    from validation import qualification

    repo = api.get('')
    pull = api.get(f'/pulls/{number}')
    value = paperwork.enrollment(pull['body'])
    branch = pull['head']['ref']
    run = api.get(f'/actions/runs/{run_id}')
    event = receipts.bounded_json(run['event_payload'].encode())
    receipts.require(value['version'] == 2 and (value['pr'], value['head'], value['base']) == (number, candidate, base)
                     and run['id'] == run_id and run['repository']['id'] == repo['id']
                     and run['workflow_id'] == WORKFLOW and run['status'] in states
                     and run['trigger_event'] == 'workflow_dispatch' and run['commit_sha'] == candidate
                     and run['prettyref'] == branch and event['ref'] == 'refs/heads/' + branch
                     and retention.digest(retention.git_bytes(candidate, '.github/workflows/' + WORKFLOW))
                     == WORKFLOW_SHA256
                     and api.get('/branches/main')['commit']['id'] == base
                     and [row['commit']['id'] for row in api.pages('/branches') if row['name'] == branch] == [candidate]
                     and paperwork.git(root, 'rev-parse', 'HEAD') == candidate
                     and not qualification.promotion_ref(branch),
                     'Actual CI workflow, dispatch, PR enrollment or current main differs')
    env = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_RUN_ATTEMPT': '1',
           'GITHUB_REPOSITORY': repo['full_name'], 'PLURX_PROMOTION_PR': str(number),
           'PLURX_PROMOTION_HEAD_SHA': candidate, 'PLURX_PROMOTION_BASE_SHA': base,
           'GITHUB_SHA': candidate, 'GITHUB_REF': event['ref']}
    binding = qualification.manual_binding(env, pull, candidate, base, candidate,
                                           paperwork.ancestor(root, base, candidate))
    receipts.require(binding['schema'] == 2, 'Continuation only admits an ordinary current promotion PR')
    dispatch_inputs(event['inputs'], binding, value)
    return repo, pull, binding, value


def runner_event(api, environment, root):
    receipts.require(environment.get('GITHUB_RUN_ATTEMPT') == '1'
                     and environment.get('GITHUB_EVENT_NAME') == 'workflow_dispatch',
                     'Continuation must run as an actual first manual dispatch')
    event = receipts.bounded_json(Path(environment['GITHUB_EVENT_PATH']).read_bytes())
    inputs = event['inputs']
    number = receipts.positive(int(inputs['promotion_pr']))
    head, base = receipts.sha(environment['GITHUB_SHA']), receipts.sha(inputs['promotion_base_sha'])
    run = receipts.positive(int(environment['GITHUB_RUN_ID']))
    result = repair_event(api, number, head, base, root, run, {'running'})
    _, _, binding, value = result
    dispatch_inputs(inputs, binding, value)
    receipts.require(event['ref'] == 'refs/heads/' + binding['head_ref'] and
                     environment.get('PLURX_PREDECESSOR_RUN') == str(value['predecessor_run']),
                     'Runner event differs from authenticated continuation dispatch')
    return result


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


UNIT_BINDING_PATH = 'source-gate-android-units.json'
CLASS_DESTINATION = 'clients/android/app/build/intermediates/built_in_kotlinc/debugUnitTest/compileDebugUnitTestKotlin/classes/'
JAR_DESTINATION = ('target/validation/source-gate-gradle/caches/modules-2/files-2.1/'
                   'org.robolectric/robolectric/4.17/source-gate-custody/' + ROSTER_JAR)


def expected_inventory_payload(binding, jvm, snapshot):
    classfiles, jar = snapshot
    payload = {CLASS_DESTINATION + path: raw for path, raw in classfiles.items()}
    payload[JAR_DESTINATION] = jar
    payload[UNIT_BINDING_PATH] = canonical({'candidate_sha': binding['head_sha'], 'command_units': {'android_jvm': jvm}})
    return payload


def stage_android(root, payload):
    destination = root / 'target/validation/source-gate-repair-inputs'
    destination.mkdir(parents=True, exist_ok=False)
    for name, raw in payload.items():
        path = destination / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)


TEST_SELECTION = Path('target/validation/source-gate-repair-selection.json')


def execute_selected_tests(environment):
    for secret in ('GITHUB_TOKEN', 'FORGEJO_TOKEN', 'ACTIONS_RUNTIME_TOKEN',
                   'ACTIONS_RUNTIME_URL', 'ACTIONS_CACHE_URL', 'ACTIONS_RESULTS_URL'):
        os.environ.pop(secret, None)
    raw = TEST_SELECTION.read_bytes()
    value = receipts.bounded_json(raw)
    receipts.require(retention.digest(raw) == environment['SOURCE_GATE_REPAIR_SELECTION_SHA256']
                     and set(value) == {'head', 'base', 'run', 'due'}
                     and value['head'] == environment['GITHUB_SHA']
                     and value['head'] == subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
                     and value['run'] == receipts.positive(int(environment['GITHUB_RUN_ID']))
                     and value['due'] == sorted(set(value['due'])),
                     'Current repair test selection was altered or belongs to another run')
    base = receipts.sha(value['base'])
    os.environ['PLURX_VALIDATION_BASE'] = base
    os.environ['PLURX_VALIDATION_MERGE_TARGET'] = base
    repair_tests(value['due'])


def repair_tests(due):
    from validation import native_python_execute as native_python
    for kind in ('validation', 'operations'):
        tests = receipts.discover(kind)
        selected = [name.split(':', 1)[1] for name in due if name.startswith(kind + ':')]
        marker = canonical({'kind': kind, 'ids': selected}).decode()
        print('SOURCE-GATE-REPAIR-TESTS-BEGIN ' + marker, flush=True)
        native_python.run_selection({'inventory': sorted(test.id() for test in tests),
                                     'uncovered': selected, 'prior_discovery': []}, tests)
        print('SOURCE-GATE-REPAIR-TESTS-END ' + marker, flush=True)


def repair_marker(text):
    # A truncated or corrupted log is a refused receipt, not an interpreter error.
    try:
        return receipts.bounded_json(text.encode())
    except ValueError as error:
        raise receipts.ReceiptError('Malformed repair test framing') from error


def current_repair_tests(raw, due):
    from validation import nonrust_unit_cohort as cohort
    text = predecessor_python_lines(raw).decode()
    begins = list(re.finditer(r'^SOURCE-GATE-REPAIR-TESTS-BEGIN (.+)$', text, re.M))
    ends = list(re.finditer(r'^SOURCE-GATE-REPAIR-TESTS-END (.+)$', text, re.M))
    receipts.require(len(begins) == len(ends) == 2 and begins[0].end() < ends[0].start() < begins[1].start()
                     and begins[1].end() < ends[1].start(), 'Incomplete exact repair test framing')
    for kind, begin, end in zip(('validation', 'operations'), begins, ends, strict=True):
        selected = [name.split(':', 1)[1] for name in due if name.startswith(kind + ':')]
        identity = {'kind': kind, 'ids': selected}
        receipts.require(repair_marker(begin[1]) == identity and repair_marker(end[1]) == identity,
                         'Current repair test body selection differs')
        block = text[begin.end():end.start()]
        if selected:
            found = cohort.python_rows(block, kind, set(selected))
            receipts.require(found == dict.fromkeys(selected, 'pass'), 'Repair test bodies did not all pass')
        else:
            receipts.require(not re.search(r'(?m)^(?:Ran \d+ tests?|test_\w+ \([^\n]+\))', block),
                             'Unrequested tests ran in the empty repair suite')
    return due


def verify_input_archive(raw, expected):
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        receipts.require(len(entries) == len(expected) and set(expected) == {item.filename for item in entries}
                         and all(item.file_size == len(expected[item.filename]) for item in entries),
                         'Repair input upload differs from the authenticated snapshot')
        receipts.require(all(archive.read(name) == data for name, data in expected.items()),
                         'A staged Android class, JAR or source binding changed')


def package_coverage(api, artifacts, job, name, task, target, source, root):
    from validation import paperwork_continuation as paperwork
    from validation import source_gate_landing as landing
    raw = successful_job(api, job, task)
    text = native.clean(raw.decode('utf-8', 'strict'))
    receipts.require(len(re.findall(r'^container smoke passed:', text, re.M)) == 1
                     and len(re.findall(r'^container restore smoke passed in ', text, re.M)) == 1,
                     'Missing actual container lifecycle or restore smoke')
    archive_raw = landing.uploaded_archive(api, artifacts, name, job, task)
    with zipfile.ZipFile(io.BytesIO(archive_raw)) as archive:
        names = archive.namelist()
        binaries = {'plurxd', 'plurx-cluster-check'}
        expected_names = {'build-manifest.json'} | {name + suffix for name in binaries for suffix in ('.sha256', '.dwp.sha256')}
        receipts.require(len(names) == len(expected_names) and set(names) == expected_names
                         and sum(item.file_size for item in archive.infolist()) <= receipts.MAX_BYTES,
                         'Incomplete package receipt artifact')
        manifest = receipts.bounded_json(archive.read('build-manifest.json'))
        receipts.require(manifest['git_commit'] == manifest['build_ref'] == source
                         and manifest['git_tree'] == paperwork.git(root, 'rev-parse', source + '^{tree}')
                         and manifest['target'] == target
                         and set(manifest['binaries']) == binaries
                         and set(manifest['debug_artifacts']) == {name + '.dwp' for name in binaries},
                         'Package smoke used another source, target or binary set')
        for name, record in {**manifest['binaries'], **manifest['debug_artifacts']}.items():
            receipts.require(retention.DIGEST.fullmatch(record['sha256']) is not None and archive.read(name + '.sha256')
                             == f"{record['sha256']}  {name}\n".encode(),
                             'Artifact digest sidecar differs from the actual verified manifest')
    return {'job': job['id'], 'source_commit': source,
            'log_sha256': retention.digest(raw), 'artifact_sha256': retention.digest(archive_raw)}


def require_gate_pointer(pull, run):
    pointer = [line.removeprefix('Manual-Source-Gate: ') for line in pull['body'].splitlines()
               if line.startswith('Manual-Source-Gate: ')]
    receipts.require(pointer == [str(run)], 'The live PR is not sealed to this actual source gate')


def repair_result(api, repo, pull, binding, value, root, run):
    from validation import qualification
    from validation.publish_cinema_main import CachedReadAPI
    from validation import source_gate_landing as landing
    from validation import workspace_rust_continuation as workspace

    # This cache is restricted to one proof calculation after repair_event
    # fetched the live PR, title, body, and branch tips. Re-authenticate these
    # through the live API before each new proof calculation.
    api = CachedReadAPI(api)
    require_gate_pointer(pull, run)
    reference, jvm, snapshot = prior_coverage(api, value, binding, pull, root, current_run=run)
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')
    owner, repository = repo['owner']['id'], repo['id']
    retained = reference['retained_lanes']

    def job_for(key):
        name, _ = job_definition(key)
        return landing.one_job(jobs, name, run, repository, owner)

    verdicts = {}
    for key in sorted(qualification.REQUIRED_JOBS - {'package_smoke'} | {'rust_compile', 'android_apks'}):
        job = job_for(key)
        # The existing Apple job may run its checkout and cleanup. Its prior
        # native commands remain required evidence independently of that job.
        skipped = key in retained and key != 'apple' or key == 'android_apks'
        receipts.require(job['status'] == ('skipped' if skipped else 'success'),
                         'Current source gate did not use the required retained or executed surface: ' + key)
        verdicts[key] = {'job': job['id'], 'status': job['status'], 'attempt': job['attempt']}

    preflight, android, rust = (job_for(key) for key in ('preflight', 'android_jvm', 'rust'))
    prepare_log = successful_job(api, preflight, 'preflight')
    snapshot_archive = landing.uploaded_archive(api, artifacts, f'source-gate-repair-inputs-{run}', preflight, 'preflight')
    payload = expected_inventory_payload(binding, jvm, snapshot)
    verify_input_archive(snapshot_archive, payload)
    android_log = successful_job(api, android, 'android_jvm')
    android_rows = native.clean(android_log.decode('utf-8', 'strict')).splitlines()
    marker = 'Android repair unit binding ' + retention.digest(payload[UNIT_BINDING_PATH])
    terminal = f"Current compiled JVM source cohort verified: {jvm['ids_count']} methods; zero bodies executed"
    image_digest = reference['android']['compile_lint']['image_digest']
    receipts.require(android_rows.count(marker) == android_rows.count(terminal) == 1
                     and android_rows.index(terminal) < android_rows.index(marker)
                     and android_rows.count('Digest: ' + image_digest) == 1
                     and not any('gradlew --no-daemon' in row for row in android_rows),
                     'Missing direct metadata verification of the same original Android source cohort')

    candidate, base = binding['head_sha'], binding['base_sha']
    proof = workspace.authenticate(api, value['workspace_rust_evidence'], binding, candidate, current_run=run)
    receipts.require(proof.get('provider') == landing.nextest.POLICY and proof.get('supplemental_origin'),
                     'Current source gate requires the reviewed nextest bridge')
    members = landing.uploaded(api, artifacts, f'selected-pr-rust-source-gate-{run}', rust, 'check',
                               {'receipt.json', 'listing.json'})
    config = landing.nextest.units.Config.load(landing.nextest.units.Tree(candidate, root).text(
        str(landing.nextest.units.CONFIG)))
    _, rust_due = landing.completion_census(proof, members['receipt.json'], members['listing.json'], config,
                                             members['__sha256__']['listing.json'])
    worker = landing.worker_proof(api, candidate, base, proof, root, repository, owner)
    commands = ('scope', 'mobile_version', 'rust_compile',
                'cluster_transport_recovery_contracts', 'cluster_transport_recovery_voter',
                'cluster_transport_recovery_learner', 'cluster_transport_recovery',
                'cluster_wal', 'cluster_daemon', 'web_layout', 'vod_web')
    command_jobs = {key: job_for(key) for key in commands}
    logs = {key: successful_job(api, job, job_definition(key)[1]) for key, job in command_jobs.items()}
    current_commands = independent_command_coverage(api, artifacts, command_jobs, logs, candidate, root)
    packages = {arch: package_coverage(api, artifacts, job_for('package_' + arch),
                             'package-smoke-receipt-' + arch, 'package_smoke', target, candidate, root)
                for arch, target in (('amd64', 'x86_64-unknown-linux-gnu'),
                                     ('arm64', 'aarch64-unknown-linux-gnu'))}
    executed = current_repair_tests(prepare_log, reference['python_due'])
    return {'version': 1, 'kind': 'selected-pr-source-gate-continuation', 'run': run, 'binding': binding,
            'body_sha256': retention.digest(pull['body'].encode()),
            'title_sha256': retention.digest(landing.semantic_pr_title(pull['title']).encode()),
            'prior': reference, 'python_current_passes': executed, 'current_jobs': verdicts,
            'preflight_log_sha256': retention.digest(prepare_log),
            'android_log_sha256': retention.digest(android_log), 'current_packages': packages,
            'current_commands': current_commands,
            'rust': {'executed': sorted(rust_due), 'origin': proof['supplemental_origin'],
                     'artifact_sha256': members['__sha256__']}, 'worker_inputs_sha256': worker}


def paperwork_check(api, value, environment, root, mode):
    from validation import paperwork_continuation as paperwork
    if mode == 'final':
        run = receipts.positive(int(environment['GITHUB_RUN_ID']))
        repo, pull, binding, enrolled = repair_event(api, value['pr'], value['head'], value['base'], root, run, {'running'})
        from validation import source_gate_landing as landing
        checked = receipts.bounded_json(Path(environment['GITHUB_EVENT_PATH']).read_bytes())['pull_request']
        receipts.require(enrolled == value and 'PR_BODY' not in environment and 'PR_TITLE' not in environment
                         and checked['body'] == pull['body']
                         and landing.semantic_pr_title(checked['title']) == landing.semantic_pr_title(pull['title']),
                         'Checked continuation paperwork body or title differs from the current PR')
        complete = repair_result(api, repo, pull, binding, value, root, run)
        return {'complete': True, 'coverage': complete, 'body': pull['body']}
    binding, pull = paperwork.binding(api, value, environment, root, mode)
    if mode == 'preliminary' and not pull['draft']:
        pointers = [line.removeprefix('Manual-Source-Gate: ') for line in pull['body'].splitlines()
                    if line.startswith('Manual-Source-Gate: ')]
        receipts.require(len(pointers) == 1, 'Ready continuation requires its actual completed source gate')
        verify(api, value['pr'], value['head'], value['base'], root, receipts.positive(int(pointers[0])))
        return {'complete': True, 'body': pull['body']}
    run = int(environment['GITHUB_RUN_ID']) if environment.get('GITHUB_RUN_ID') else None
    reference, _, _ = prior_coverage(api, value, binding, pull, root, current_run=run)
    inventory = set(paperwork.current_rule_inventory(root))
    pending = inventory & set(reference['python_due'])
    receipts.require(inventory == (inventory & set(reference['python'])) | pending,
                     'Paperwork coverage does not partition current discovery')
    return {'complete': False, 'body': pull['body'],
            'retained': len(inventory) - len(pending), 'deferred': len(pending)}


def verify(api, number, head, main, root, run):
    from validation import source_gate_landing as landing
    repo, pull, binding, value = repair_event(api, number, head, main, root, run, {'success'})
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    gate = landing.one_job(jobs, 'Main promotion gate', run, repo['id'], repo['owner']['id'])
    successful_job(api, gate, 'pr_gate')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')
    record = landing.uploaded(api, artifacts, f'source-gate-repair-{run}', gate, 'pr_gate', {'receipt.json'})
    expected = repair_result(api, repo, pull, binding, value, root, run)
    receipts.require(record['receipt.json'] == expected, 'Continuation receipt contradicts current authenticated evidence')
    return {'run': run, 'job': gate['id'], 'base': main, 'head': head,
            'worker_inputs_sha256': expected['worker_inputs_sha256'],
            'url': api.root.split('/api/v1/repos/')[0] + '/' + repo['full_name'] + '/actions/runs/' + str(run)}


def main(operation):
    root = Path.cwd()
    env = os.environ
    if operation == 'test':
        execute_selected_tests(env)
        return
    api = retention.runner_api(env)
    _, pull, binding, value = runner_event(api, env, root)
    run = receipts.positive(int(env['GITHUB_RUN_ID']))
    head, base = binding['head_sha'], binding['base_sha']
    if operation == 'prepare':
        receipts.require(env.get('PLURX_CONTINUATION_STORE_MODE') == 'legacy',
                         'This continuation requires the authenticated legacy Store command profile')
        reference, jvm, snapshot = prior_coverage(api, value, binding, pull, root, current_run=run)
        stage_android(root, expected_inventory_payload(binding, jvm, snapshot))
        selection = canonical({'head': head, 'base': base, 'run': run, 'due': reference['python_due']})
        with TEST_SELECTION.open('xb') as output:
            output.write(selection)
        with open(env['GITHUB_OUTPUT'], 'a') as output:
            print('repair_selection_sha256=' + retention.digest(selection), file=output)
            print('android_continuation_image=' + reference['android']['compile_lint']['image_digest'], file=output)
            print('root_ownership_retained=true', file=output)
            print('formal_worker_refinement_retained=true', file=output)
            print('apple_native_retained=true', file=output)
            print('android_device_retained=true', file=output)
            print('topology_retained=true', file=output)
            print('store_retained=true', file=output)
            print('hdr_rgb_retained=true', file=output)
        return
    receipts.require(operation == 'finish', 'Unknown source-gate repair operation')
    directory = root / 'target/validation/source-gate-repair'
    directory.mkdir(parents=True, exist_ok=False)
    body = directory / 'event.json'
    body.write_bytes(canonical({'pull_request': pull}))
    receipt = directory / 'receipt.json'
    command_env = {**env, 'GITHUB_EVENT_PATH': str(body), 'PR_HEAD_SHA': head,
                   'PLURX_PAPERWORK_CONTINUATION': 'final', 'PLURX_PAPERWORK_COVERAGE_OUTPUT': str(receipt)}
    command_env.pop('PR_BODY', None)
    command_env.pop('PR_TITLE', None)
    subprocess.run([sys.executable, '-B', 'scripts/paperwork', '--base', base, '--merge-target', base, '--body-env'],
                   check=True, env=command_env)
    print('Completed source-gate continuation for', head, 'against', base, flush=True)


def android_device_coverage(api, artifacts, builder, device, build_log, device_log):
    from validation import source_gate_landing as landing
    artifact_name = 'android-tv-instrumentation-apks'
    item, _, archive_digest = landing.uploaded_identity(api, artifacts, artifact_name, builder,
                                                         'android_apks', 64 * 1024 * 1024)
    builder_text = native.clean(build_log.decode('utf-8', 'strict'))
    build_command = 'plurx-android-build ./gradlew --no-daemon assembleDebug assembleDebugAndroidTest'
    receipts.require(builder_text.count(build_command) == 1
                     and len(re.findall(r'^BUILD SUCCESSFUL in .+$', builder_text, re.M)) == 1
                     and '> Task :app:assembleDebug' in builder_text
                     and '> Task :app:assembleDebugAndroidTest' in builder_text,
                     'Historical job did not build both Android APKs')
    text = native.clean(device_log.decode('utf-8', 'strict'))
    downloaded = f"- {artifact_name} (ID: {item['id']}, Size: {item['size_in_bytes']})"
    receipts.require(text.splitlines().count(downloaded) == 1, 'Android instrumentation acquired another APK artifact')
    text = text.replace('[command]/bin/sh -c ' + native.DEVICE_COMMAND,
                        '[command]/usr/bin/sh -c ' + native.DEVICE_COMMAND)
    count = native.device_summary(text)
    raw = landing.uploaded_archive(api, artifacts, 'android-instrumentation-reports', device, 'android_device')
    report_path = 'target/validation/android-instrumentation.txt'
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        receipts.require(archive.namelist() == [report_path] and archive.infolist()[0].file_size <= receipts.MAX_BYTES,
                         'Unexpected Android instrumentation report archive')
        report = archive.read(report_path).decode('utf-8', 'strict')
    receipts.require(re.findall(r'^OK \(([1-9]\d*) tests\)$', report, re.M) == [str(count)]
                     and not re.search(r'^FAILURES!!!|^INSTRUMENTATION_FAILED:', report, re.M),
                     'Archive contradicts the real instrumentation command outcome')
    return {'apk_job': builder['id'], 'apk_artifact': item['id'], 'apk_sha256': archive_digest,
            'instrumentation_job': device['id'], 'instrumentation_report_sha256': retention.digest(raw),
            'test_count': count}


LANE_RECEIPTS = {
    'cluster_store_legacy': ('cluster-store-legacy-contracts', 'cluster-store', 'cluster-store-legacy',
                             ('make cluster-store-check',)),
    'cluster_topology': ('cluster-topology-contracts', 'cluster-topology', 'cluster-topology',
                         ('make hiqlite-vendor-clippy', 'make cluster-harness-check')),
    'cluster_transport_recovery': ('cluster-transport-recovery', 'cluster-transport-recovery',
                                   'cluster-transport-recovery', ('make cluster-transport-recovery-contracts',
                                   'make cluster-transport-recovery-voter-check',
                                   'make cluster-transport-recovery-learner-check', 'transport-recovery-assemble')),
}


def cluster_contract_receipt(api, artifacts, job, key, source, root):
    from validation import paperwork_continuation as paperwork
    from validation import source_gate_landing as landing

    artifact, prefix, lane, commands = LANE_RECEIPTS[key]
    raw = landing.uploaded_archive(api, artifacts, artifact, job, key)
    receipt_name, log_name = prefix + '-receipt.json', prefix + '.log'
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        items = archive.infolist()
        receipts.require(len(items) == len({item.filename for item in items})
                         and sum(item.file_size for item in items) <= receipts.MAX_BYTES,
                         'Unexpected contract receipt archive layout')
        receipt = receipts.bounded_json(archive.read(receipt_name))
        log = archive.read(log_name)
        receipts.require(receipt['schema'] == 1 and receipt['kind'] == 'ci-lane'
                         and receipt['lane'] == lane and receipt['result'] == 'success'
                         and receipt['run_id'] == str(job['run_id']) and receipt['job'] == key
                         and receipt['run_attempt'] == '1' and receipt['repository'] == api.repository
                         and receipt['tested_sha'] == source
                         and receipt['tested_tree'] == paperwork.git(root, 'rev-parse', source + '^{tree}')
                         and set(commands) <= set(receipt['commands'])
                         and receipt['log'] == {'name': log_name, 'sha256': retention.digest(log), 'bytes': len(log)},
                         'Contract artifact does not prove the actual required command')
        if key == 'cluster_transport_recovery':
            evidence = archive.read(lane + '.json')
            receipts.require(receipt['evidence'] == {'name': lane + '.json', 'sha256': retention.digest(evidence),
                                                     'bytes': len(evidence), 'build_sha': source},
                             'Transport recovery aggregate did not validate its exact campaign evidence')
    return {'artifact': artifact, 'sha256': retention.digest(raw), 'commands': receipt['commands']}


def wal_coverage(raw, recipes):
    from validation import qualification_unit_continuation as commands

    text = native.clean(raw.decode('utf-8', 'strict'))
    blocks = list(re.finditer(r'^running (\d+) tests?\n.*?^test result: (?:ok|FAILED)\. [^\n]+$', text, re.M | re.S))
    receipts.require(len(blocks) == len(recipes)
                     and len(re.findall(r'^test result:', text, re.M)) == len(blocks)
                     and len(re.findall(r'^running \d+ tests?$', text, re.M)) == len(blocks),
                     'Historical WAL harness windows differ from the required ordered recipe')
    results = []
    for recipe, block in zip(recipes, blocks, strict=True):
        profile = commands.rust_identity(recipe)
        selector = profile['filter']
        exact = '--exact' in profile['harness']
        count = 1 if exact else int(profile['environment']['PLURX_EXPECT_TEST_COUNT'])
        actual = retention.log_outcomes(block[0], 'rust')
        receipts.require(int(block[1]) == len(actual) == count
                         and all(outcome == 'pass' for outcome in actual.values())
                         and (set(actual) == {selector} if exact else all(selector in name for name in actual)),
                         'Missing or foreign outcome for a historical WAL selector: ' + selector)
        commands.require_native_terminals(block[0], actual, 0, recipe)
        results.append({'selector': selector, 'passed': sorted(actual)})
    return results


def independent_command_coverage(api, artifacts, jobs, logs, source, root):
    from validation import qualification_unit_continuation as commands
    from validation import source_gate_landing as landing

    evidence = {key: cluster_contract_receipt(api, artifacts, jobs[key], key, source, root) for key in LANE_RECEIPTS if key in jobs}
    source_steps = native.clean(logs['scope'].decode('utf-8', 'strict'))
    observed = re.findall(r'^\[command\]/usr/bin/git log -1 --format=%H\n([0-9a-f]{40})$', source_steps, re.M)
    receipts.require(observed == [source], 'Predecessor validation scope checked another source')
    for key, terminal in {
        'mobile_version': r'^mobile versions and changed-app build counters are valid$',
        'rust_compile': r'^\s*Finished `dev` profile .* target\(s\) in .+$',
        'web_layout': r'^CHECK +[1-9]\d* structural facts match .+$',
        'vod_web': r'^RESULT +([1-9]\d*)/\1 passed · outcome passed$',
    }.items():
        text = native.clean(logs[key].decode('utf-8', 'strict'))
        receipts.require(len(re.findall(terminal, text, re.M)) == 1, 'Missing actual completed lane command: ' + key)
        evidence[key] = {'terminal': re.search(terminal, text, re.M)[0].strip()}
    rust = native.clean(logs['rust_compile'].decode('utf-8', 'strict'))
    receipts.require(re.findall(r'^cargo (?:check|fmt).+$', rust, re.M) ==
                     ['cargo fmt --all --check', 'cargo check --workspace --locked --all-targets'],
                     'Rust compiler job used another command profile')

    wal = commands.make_commands(retention.git_bytes(source, 'Makefile').decode('utf-8'), 'cluster-wal-check')
    evidence['cluster_wal'] = {'commands': wal_coverage(logs['cluster_wal'], wal)}
    daemon = native.clean(logs['cluster_daemon'].decode('utf-8', 'strict'))
    receipts.require('--test cluster_activation -- --nocapture' in daemon
                     and '--test cluster_activity -- --nocapture --test-threads=1' in daemon
                     and len(re.findall(r'^test result: ok\. [1-9]\d* passed; 0 failed;', daemon, re.M)) == 2,
                     'Missing one of the two actual daemon integration suites')
    evidence['cluster_daemon'] = {'suites': ['cluster_activation', 'cluster_activity']}

    web = native.clean(logs['web_layout'].decode('utf-8', 'strict'))
    receipts.require('PASS no account token on capability requests; no VOD or account write from any Live TV path' in web
                     and len(re.findall(r'^(?:#|ℹ) fail 0$', web, re.M)) >= 6,
                     'Web layout job lacks actual browser-suite completion outputs')
    transport = {}
    for key, name in (('cluster_transport_recovery_contracts', 'cluster-transport-recovery-contracts'),
                      ('cluster_transport_recovery_voter', 'cluster-transport-recovery-voter'),
                      ('cluster_transport_recovery_learner', 'cluster-transport-recovery-learner')):
        job = jobs[key]
        item, _, digest = landing.uploaded_identity(api, artifacts, name, job, key, 64 * 1024 * 1024)
        transport[key] = {'artifact_id': item['id'], 'sha256': digest}
    evidence['cluster_transport_recovery']['required_producers'] = transport
    return evidence


if __name__ == '__main__':
    receipts.require(len(sys.argv) == 2, 'Select the source-gate repair operation')
    main(sys.argv[1])
