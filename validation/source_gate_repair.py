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
    return {'source_commit': receipts.sha(source), 'log_sha256': retention.digest(raw),
            'compile_tasks': list(ANDROID_COMPILE_TASKS), 'test_bodies': 0}


def prior_run(api, value, binding, repository, *, current_run=None):
    """Require the exact earlier manual run for the same PR, branch and main."""
    run_id = receipts.positive(value['predecessor_run'])
    receipts.require(current_run is None or run_id < current_run, 'Source gate repair requires an earlier run')
    run = api.get(f'/actions/runs/{run_id}')
    event = receipts.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    prior = receipts.sha(run['commit_sha'])
    branch = binding['head_ref']
    receipts.require(run['id'] == run_id and run['workflow_id'] == 'ci.yml'
                     and run['status'] == 'failure' and run['repository']['id'] == repository
                     and run['trigger_event'] == 'workflow_dispatch' and run['prettyref'] == branch
                     and event['ref'] == 'refs/heads/' + branch
                     and inputs['promotion_pr'] == str(binding['pull_request'])
                     and inputs['promotion_head_sha'] == prior
                     and inputs['promotion_base_sha'] == binding['base_sha'],
                     'Earlier run is not the failed source gate for this PR and current main')
    return run_id, {**binding, 'head_sha': prior}, inputs


def predecessor_attachment(api, descriptor, binding, pull):
    from validation import paperwork_continuation as paperwork
    expected = {'sha256', 'attachment_id', 'attachment_uuid'}
    receipts.require(set(descriptor) == expected, 'Invalid predecessor object descriptor')
    raw = retention.attachment_bytes(api, binding['pull_request'], descriptor['attachment_id'],
                                     descriptor['attachment_uuid'], descriptor['sha256'])
    return paperwork.local_attestation(api, raw, descriptor, binding, binding['head_sha'], pull)


def rust_coverage(api, descriptor, old, pull, run, job, artifacts, root):
    from validation import source_gate_landing as landing
    nextest = landing.nextest
    source = old['head_sha']
    record, repository = predecessor_attachment(api, descriptor, old, pull)
    proof = nextest.authenticate(api, record, old, source, repository, current_run=run)
    members = landing.uploaded(api, artifacts, f'selected-pr-rust-source-gate-{run}', job, 'check',
                               {'receipt.json', 'listing.json'})
    config = nextest.units.Config.load(nextest.units.Tree(source, root).text(str(nextest.units.CONFIG)))
    current, due = landing.completion_census({**proof, 'digest': descriptor['sha256']}, members['receipt.json'],
                                             members['listing.json'], config,
                                             members['__sha256__']['listing.json'])
    worker_input = landing.require_unchanged_worker_inputs(source, old['base_sha'], proof['prior'], root)
    repo = api.get('')
    worker_jobs = api.pages(f"/actions/runs/{proof['run']}/jobs")
    worker = landing.one_job(worker_jobs, 'production worker controller proof', proof['run'],
                             repository, repo['owner']['id'])
    receipts.require(worker['status'] == 'success', 'Required production worker proof did not succeed')
    worker_log = api.bytes(f"/actions/jobs/{worker['id']}/logs").decode('utf-8', 'strict')
    task = nextest.units.TASK_LINE.search(worker_log[:4096])
    receipts.require(task is not None and task.groups() == ('worker_control_proof', 'pull_request'),
                     'Historical production worker proof task differs')
    return {'executed': sorted(due), 'retained': proof['retained_ids'], 'census_count': len(current),
            'source_run': run, 'listing_sha256': members['__sha256__']['listing.json'],
            'completion_sha256': members['__sha256__']['receipt.json'],
            'worker_inputs_sha256': worker_input, 'worker_log_sha256': retention.digest(worker_log.encode())}


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
    observed, runtime = paperwork.current_outcomes(raw, plan, capture_runtime=True)
    rules, retained_rules, directly_due = paperwork.rule_inventory(plan, root)
    receipts.require(directly_due <= set(observed) and set(rules) == retained_rules | directly_due,
                     'Earlier preflight did not complete exact paperwork coverage')
    node = selected_node_preflight.coverage(raw, plan)
    reader = selected_apple_preflight.reader_build_proof(api, repository, old, plan, run)
    return {'direct_passes': sorted(observed), 'retained_passes': sorted(
                name for suite in plan['suites'].values() for name in suite['retained_source_records']),
            'paperwork': sorted(rules), 'node': node, 'dependency_runtime': runtime,
            'apple': plan['apple_preflight'], 'ios_reader_build': reader,
            'root_ownership': plan['root_ownership'], 'formal_retained': plan['formal_preflight']}


ANDROID_INPUTS = ('clients/android', 'Makefile')
ROSTER_MEMBER = re.compile(r"classes/(?:[A-Za-z0-9_$-]+/)*[A-Za-z0-9 _$'-]+\.class")
ROSTER_JAR = 'robolectric-4.17.jar'


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
    historical = {**binding, 'head_sha': original}
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
    # Linux compilation/lint and the preserved source-equivalent original class bytes
    # have different actual origins. Keep both in the current repair receipt.
    custody = {'compile_lint': command, 'source_commit': original,
               'archive_sha256': envelope['archive_sha256'], 'evidence_sha256': snapshot['sha256'],
               'compiled_class_count': len(classfiles), 'ids_count': unit['ids_count'],
               'ids_sha256': unit['ids_sha256'], 'jvm_source_owners_sha256': unit['jvm_source_owners_sha256'],
               'compiler_inputs': envelope['compiler_inputs']}
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


def formal_coverage(api, artifacts, job, run, old, current):
    from validation import source_gate_landing as landing
    raw = landing.uploaded_archive(api, artifacts, f'formal-model-{run}-1', job, 'preflight')
    result = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        receipts.require(len(entries) == len({entry.filename for entry in entries})
                         and sum(entry.file_size for entry in entries) <= receipts.MAX_BYTES,
                         'Unexpected formal evidence archive layout')
        for package in ('durable-jobs', 'operation-progress'):
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
                receipts.require(retention.git_bytes(old, path) == retention.git_bytes(current, path)
                                 and retention.digest(retention.git_bytes(old, path)) == digest,
                                 'A completed formal input changed: ' + path)
            result[package] = {'artifact_sha256': retention.digest(raw),
                               'receipt_sha256': retention.digest(archive.read(names[0])),
                               'source_commit': old}
    return result


REPAIR_WORKFLOW = 'source-gate-repair.yml'
REPAIR_WORKFLOW_SHA256 = '1cf08f179642f19aaf0c7a38de80c975851edc74995d5f01e2320fc34fae204b'
REPAIR_PATHS = {
    '.github/workflows/' + REPAIR_WORKFLOW,
    'tests/validation/test_selected_pr_jvm_discovery.py',
    'tests/validation/test_source_gate_repair.py',
    'validation/jvm_unit_discovery.py',
    'validation/main_python_continuation.py',
    'validation/paperwork_continuation.py',
    'validation/source_gate_landing.py',
    'validation/source_gate_repair.py',
}
REPAIR_AFFECTED = {
    'operations:test_ci_cache.CiCacheContractCase.test_github_yaml_has_no_duplicate_mapping_keys',
    'operations:test_ci_throughput.PullRequestEventTests.test_no_workflow_passes_the_description_through_the_environment',
    'operations:test_contracts.OperationsContractCase.test_external_actions_are_immutable_and_checkout_drops_credentials',
    'operations:test_contracts.OperationsContractCase.test_ci_artifacts_are_bounded_and_pr_builds_do_not_retain_binaries',
    'operations:test_contracts.OperationsContractCase.test_ci_jobs_use_the_intended_runner_trust_boundary',
    'operations:test_contracts.OperationsContractCase.test_every_general_lane_job_runs_in_the_pinned_ci_image',
    'operations:test_contracts.OperationsContractCase.test_every_self_hosted_runs_on_is_satisfiable_by_a_rostered_runner',
    'operations:test_contracts.OperationsContractCase.test_fleet_registry_variable_always_carries_its_fallback',
    'operations:test_contracts.OperationsContractCase.test_forgejo_workflows_do_not_declare_ignored_github_permissions',
    'operations:test_contracts.OperationsContractCase.test_no_workflow_pipes_git_show_into_an_early_exiting_grep',
    'operations:test_contracts.OperationsContractCase.test_root_job_containers_restore_persistent_workspace_ownership',
    'operations:test_contracts.OperationsContractCase.test_workflows_have_no_hosted_runner_escape',
    'operations:test_contracts.OperationsContractCase.test_private_origin_fetches_use_only_step_scoped_credentials',
    'validation:test_paperwork_continuation.PaperworkContinuationTests.test_enrollment_is_unique_closed_and_bound_to_an_immutable_descriptor',
    'validation:test_paperwork_continuation.PaperworkContinuationTests.test_preliminary_retained_coverage_never_claims_deferred_methods_passed',
    'validation:test_selected_pr_jvm_discovery.SelectedPrJvmDiscoveryCase.test_selected_runner_binds_source_sdk_config_version_and_actual_jar_bytes',
}


def source_delta(root, old, current):
    from validation import paperwork_continuation as paperwork
    from validation import source_gate_landing as landing

    receipts.require(old != current and paperwork.ancestor(root, old, current),
                     'Source gate repair must extend the original source')
    delta = retention.changed(old, current)
    receipts.require({'validation/jvm_unit_discovery.py', 'tests/validation/test_selected_pr_jvm_discovery.py',
                      'validation/source_gate_repair.py', '.github/workflows/' + REPAIR_WORKFLOW} <= delta
                     and delta <= REPAIR_PATHS,
                     'Source gate repair changed inputs outside the closed Android verifier repair')
    workflow = retention.git_bytes(old, '.github/workflows/ci.yml')
    receipts.require(workflow is not None and workflow == retention.git_bytes(current, '.github/workflows/ci.yml')
                     and retention.digest(workflow) == landing.WORKFLOW_SHA256,
                     'Source gate repair changed the original command workflow')
    changes = {}
    for path in sorted(delta):
        before = retention.git_bytes(old, path)
        after = retention.git_bytes(current, path)
        receipts.require(after is not None, 'Source-gate repair removed a governed file')
        changes[path] = {'before': retention.digest(before) if before is not None else None,
                         'after': retention.digest(after)}
    return changes


def python_inventory(source):
    from validation import qualification_unit_continuation as commands
    before = {kind: sorted(commands.frozen_python_ids(source, kind)) for kind in receipts.SUITES}
    after = {kind: sorted(test.id() for test in receipts.discover(kind)) for kind in receipts.SUITES}
    old = {kind + ':' + name for kind, names in before.items() for name in names}
    current = {kind + ':' + name for kind, names in after.items() for name in names}
    added = current - old
    receipts.require(old <= current and REPAIR_AFFECTED <= old and added
                     and all(name.startswith('validation:test_source_gate_repair.') for name in added),
                     'Source gate repair changed the Python discovery cohort unexpectedly')
    return before, sorted(REPAIR_AFFECTED | added)


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


def prior_coverage(api, value, binding, pull, root, current_run=None):
    from validation import qualification
    from validation import source_gate_landing as landing

    repo = api.get('')
    repository, owner = repo['id'], repo['owner']['id']
    run, old, inputs = prior_run(api, value, binding, repository, current_run=current_run)
    current, prior = binding['head_sha'], old['head_sha']
    changes = source_delta(root, prior, current)
    old_inventory, due = python_inventory(prior)
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')
    required = (qualification.REQUIRED_JOBS - {'package_smoke'} |
                {'package_amd64', 'package_arm64', 'rust_compile', 'android_apks', 'cluster_store_legacy',
                 'cluster_transport_recovery_contracts', 'cluster_transport_recovery_voter',
                 'cluster_transport_recovery_learner'})
    data, job_by_key, logs = {}, {}, {}
    for key in sorted(required):
        name, task = job_definition(key)
        job = landing.one_job(jobs, name, run, repository, owner)
        if key == 'android_jvm':
            receipts.require(job['status'] == 'failure', 'Expected only a failed Android metadata suffix')
            log = execution_log(api, job, task)
        elif key == 'package_arm64' and job['status'] == 'failure':
            log = execution_log(api, job, task)
            receipts.require(b"install: cannot install 'binary-export/plurxd.dwp' to 'release-bin/plurxd.dwp': No space left on device" in log
                             and b'exporting to client directory' in log
                             and b'Run scripts/container-smoke' not in log,
                             'Unexpected earlier arm64 package failure')
        else:
            log = successful_job(api, job, task)
        job_by_key[key], logs[key] = job, log
        data[key] = {'id': job['id'], 'task_id': job['task_id'], 'attempt': job['attempt'],
                     'source_commit': prior, 'log_sha256': retention.digest(log), 'status': job['status']}
    android = android_compile_lint(logs['android_jvm'], prior)
    android_descriptor = {'sha256': inputs['retained_units_sha256'],
                          'attachment_id': receipts.positive(int(inputs['retained_units_attachment_id'])),
                          'attachment_uuid': inputs['retained_units_attachment_uuid']}
    android, jvm, snapshot = android_witness(api, android_descriptor,
        {**binding, 'android_snapshot': value['android_snapshot']}, pull, old, android, root)
    python = python_coverage(api, receipts.bounded_json(inputs['python_unit_evidence'].encode()), old,
                             pull, run, logs['preflight'], old_inventory, root)
    rust = rust_coverage(api, receipts.bounded_json(inputs['workspace_rust_evidence'].encode()), old,
                         pull, run, job_by_key['rust'], artifacts, root)
    formal = formal_coverage(api, artifacts, job_by_key['preflight'], run, prior, current)
    packages = {}
    for arch, target in (('amd64', 'x86_64-unknown-linux-gnu'), ('arm64', 'aarch64-unknown-linux-gnu')):
        job = job_by_key['package_' + arch]
        if job['status'] == 'success':
            packages[arch] = package_coverage(api, artifacts, job, 'package-smoke-receipt-' + arch,
                                              'package_smoke', target, prior, root)
    return {'run': run, 'source_commit': prior, 'base': binding['base_sha'], 'source_delta': changes,
            'jobs': data, 'rust': rust, 'python': python, 'formal': formal, 'packages': packages,
            'android': android, 'python_due': due}, jvm, snapshot


def repair_event(api, number, candidate, base, root, run_id, states):
    from validation import paperwork_continuation as paperwork
    from validation import qualification

    repo = api.get('')
    pull = api.get(f'/pulls/{number}')
    value = paperwork.enrollment(pull['body'])
    branch = pull['head']['ref']
    run = api.get(f'/actions/runs/{run_id}')
    event = receipts.bounded_json(run['event_payload'].encode())
    expected = {'promotion_pr': str(number), 'promotion_head_sha': candidate,
                'promotion_base_sha': base, 'predecessor_run': str(value['predecessor_run'])}
    receipts.require(value['version'] == 2 and (value['pr'], value['head'], value['base']) == (number, candidate, base)
                     and run['id'] == run_id and run['repository']['id'] == repo['id']
                     and run['workflow_id'] == REPAIR_WORKFLOW and run['status'] in states
                     and run['trigger_event'] == 'workflow_dispatch' and run['commit_sha'] == candidate
                     and run['prettyref'] == branch and event['ref'] == 'refs/heads/' + branch
                     and event['inputs'] == expected
                     and retention.digest(retention.git_bytes(candidate, '.github/workflows/' + REPAIR_WORKFLOW)) ==
                         REPAIR_WORKFLOW_SHA256
                     and api.get('/branches/main')['commit']['id'] == base
                     and [row['commit']['id'] for row in api.pages('/branches') if row['name'] == branch] == [candidate]
                     and paperwork.git(root, 'rev-parse', 'HEAD') == candidate
                     and not qualification.promotion_ref(branch),
                     'Actual repair workflow, dispatch, PR enrollment or current main differs')
    env = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_RUN_ATTEMPT': '1',
           'GITHUB_REPOSITORY': repo['full_name'], 'PLURX_PROMOTION_PR': str(number),
           'PLURX_PROMOTION_HEAD_SHA': candidate, 'PLURX_PROMOTION_BASE_SHA': base,
           'GITHUB_SHA': candidate, 'GITHUB_REF': event['ref']}
    binding = qualification.manual_binding(env, pull, candidate, base, candidate,
                                           paperwork.ancestor(root, base, candidate))
    receipts.require(binding['schema'] == 2, 'Repair only admits an ordinary current promotion PR')
    return repo, pull, binding, value


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


def current_repair_tests(raw, due):
    from validation import nonrust_unit_cohort as cohort
    text = native.clean(raw.decode('utf-8', 'strict'))
    begins = list(re.finditer(r'^SOURCE-GATE-REPAIR-TESTS-BEGIN (.+)$', text, re.M))
    ends = list(re.finditer(r'^SOURCE-GATE-REPAIR-TESTS-END (.+)$', text, re.M))
    receipts.require(len(begins) == len(ends) == 2 and begins[0].end() < ends[0].start() < begins[1].start()
                     and begins[1].end() < ends[1].start(), 'Incomplete exact repair test framing')
    for kind, begin, end in zip(('validation', 'operations'), begins, ends, strict=True):
        selected = [name.split(':', 1)[1] for name in due if name.startswith(kind + ':')]
        identity = {'kind': kind, 'ids': selected}
        receipts.require(receipts.bounded_json(begin[1].encode()) == identity
                         and receipts.bounded_json(end[1].encode()) == identity,
                         'Current repair test body selection differs')
        block = text[begin.end():end.start()]
        if selected:
            found = cohort.python_rows(block, kind, set(selected))
            receipts.require(found == dict.fromkeys(selected, 'pass'), 'Repair test bodies did not all pass')
        else:
            receipts.require(re.search(r'(?m)^Ran \d+ tests?', block) is None,
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


def repair_result(api, repo, pull, binding, value, root, run):
    from validation import source_gate_landing as landing
    reference, jvm, snapshot = prior_coverage(api, value, binding, pull, root, current_run=run)
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')
    owner = repo['owner']['id']
    preparer = landing.one_job(jobs, 'Authenticate source-gate repair evidence', run, repo['id'], owner)
    android = landing.one_job(jobs, 'Verify the preserved Android JVM discovery', run, repo['id'], owner)
    arm = landing.one_job(jobs, 'Complete the outstanding arm64 package smoke', run, repo['id'], owner)
    prepare_log = successful_job(api, preparer, 'prepare')
    snapshot_archive = landing.uploaded_archive(api, artifacts, f'source-gate-repair-inputs-{run}', preparer, 'prepare')
    payload = expected_inventory_payload(binding, jvm, snapshot)
    verify_input_archive(snapshot_archive, payload)
    android_log = successful_job(api, android, 'android')
    android_rows = native.clean(android_log.decode('utf-8', 'strict')).splitlines()
    marker = 'Android repair unit binding ' + retention.digest(payload[UNIT_BINDING_PATH])
    terminal = f"Current compiled JVM source cohort verified: {jvm['ids_count']} methods; zero bodies executed"
    receipts.require(android_rows.count(marker) == android_rows.count(terminal) == 1
                     and android_rows.index(terminal) < android_rows.index(marker),
                     'Missing direct Android metadata verification of the same downloaded source cohort')
    arm_result = None
    if reference['jobs']['package_arm64']['status'] == 'failure':
        arm_result = package_coverage(api, artifacts, arm, f'package-smoke-repair-arm64-{run}',
                                      'arm64', 'aarch64-unknown-linux-gnu', reference['source_commit'], root)
    else:
        receipts.require(arm['status'] == 'skipped', 'A successful applicable arm64 package smoke was repeated')
    executed = current_repair_tests(prepare_log, reference['python_due'])
    pointer = [line.removeprefix('Manual-Source-Gate: ') for line in pull['body'].splitlines()
               if line.startswith('Manual-Source-Gate: ')]
    receipts.require(pointer == [str(run)], 'The live PR is not sealed to this actual repair gate')
    return {'version': 1, 'kind': 'selected-pr-source-gate-repair', 'run': run, 'binding': binding,
            'body_sha256': retention.digest(pull['body'].encode()),
            'title_sha256': retention.digest(landing.semantic_pr_title(pull['title']).encode()),
            'predecessor': reference, 'python_current_passes': executed, 'prepare_job': preparer['id'],
            'prepare_log_sha256': retention.digest(prepare_log), 'android_job': android['id'],
            'android_log_sha256': retention.digest(android_log), 'arm64_current': arm_result}


def paperwork_check(api, value, environment, root, mode):
    from validation import paperwork_continuation as paperwork
    if mode == 'final':
        run = receipts.positive(int(environment['GITHUB_RUN_ID']))
        repo, pull, binding, enrolled = repair_event(api, value['pr'], value['head'], value['base'], root, run, {'running'})
        receipts.require(enrolled == value, 'Repair paperwork differs from live enrollment')
        complete = repair_result(api, repo, pull, binding, value, root, run)
        return {'complete': True, 'coverage': complete, 'body': pull['body']}
    binding, pull = paperwork.binding(api, value, environment, root, mode)
    if mode == 'preliminary' and not pull['draft']:
        pointers = [line.removeprefix('Manual-Source-Gate: ') for line in pull['body'].splitlines()
                    if line.startswith('Manual-Source-Gate: ')]
        receipts.require(len(pointers) == 1, 'Ready repair requires its actual completed source gate')
        verify(api, value['pr'], value['head'], value['base'], root, receipts.positive(int(pointers[0])))
        return {'complete': True, 'body': pull['body']}
    repo = api.get('')
    run, old, inputs = prior_run(api, value, binding, repo['id'])
    source_delta(root, old['head_sha'], binding['head_sha'])
    old_inventory, due = python_inventory(old['head_sha'])
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    from validation import source_gate_landing as landing
    preflight = landing.one_job(jobs, landing.JOB_NAMES['preflight'], run, repo['id'], repo['owner']['id'])
    coverage = python_coverage(api, receipts.bounded_json(inputs['python_unit_evidence'].encode()), old, pull,
                               run, successful_job(api, preflight, 'preflight'), old_inventory, root)
    pending = set(coverage['paperwork']) & set(due)
    return {'complete': False, 'body': pull['body'],
            'retained': len(coverage['paperwork']) - len(pending), 'deferred': len(pending)}


def verify(api, number, head, main, root, run):
    from validation import source_gate_landing as landing
    repo, pull, binding, value = repair_event(api, number, head, main, root, run, {'success'})
    jobs = api.pages(f'/actions/runs/{run}/jobs')
    gate = landing.one_job(jobs, 'Main promotion gate', run, repo['id'], repo['owner']['id'])
    successful_job(api, gate, 'pr_gate')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')
    record = landing.uploaded(api, artifacts, f'source-gate-repair-{run}', gate, 'pr_gate', {'receipt.json'})
    expected = repair_result(api, repo, pull, binding, value, root, run)
    receipts.require(record['receipt.json'] == expected, 'Repair receipt contradicts current authenticated evidence')
    return {'run': run, 'job': gate['id'], 'base': main, 'head': head,
            'worker_inputs_sha256': expected['predecessor']['rust']['worker_inputs_sha256'],
            'url': api.root.split('/api/v1/repos/')[0] + '/' + repo['full_name'] + '/actions/runs/' + str(run)}


def main(operation):
    root = Path.cwd()
    env = os.environ
    receipts.require(env.get('GITHUB_RUN_ATTEMPT') == '1' and env.get('GITHUB_EVENT_NAME') == 'workflow_dispatch',
                     'Repair must run as an actual first manual dispatch')
    event = receipts.bounded_json(Path(env['GITHUB_EVENT_PATH']).read_bytes())
    inputs = event['inputs']
    number = receipts.positive(int(inputs['promotion_pr']))
    run = receipts.positive(int(env['GITHUB_RUN_ID']))
    head, base = receipts.sha(env['GITHUB_SHA']), receipts.sha(inputs['promotion_base_sha'])
    api = retention.runner_api(env)
    repo, pull, binding, value = repair_event(api, number, head, base, root, run, {'running'})
    receipts.require(inputs == {'promotion_pr': str(number), 'promotion_head_sha': head, 'promotion_base_sha': base,
                                'predecessor_run': str(value['predecessor_run'])}
                     and event['ref'] == 'refs/heads/' + binding['head_ref'],
                     'Runner event differs from authenticated repair dispatch')
    if operation == 'prepare':
        reference, jvm, snapshot = prior_coverage(api, value, binding, pull, root, current_run=run)
        repair_tests(reference['python_due'])
        stage_android(root, expected_inventory_payload(binding, jvm, snapshot))
        with open(env['GITHUB_OUTPUT'], 'a') as output:
            print('predecessor_sha=' + reference['source_commit'], file=output)
            print('arm64_due=' + str(reference['jobs']['package_arm64']['status'] == 'failure').lower(), file=output)
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
    print('Completed source-gate repair for', head, 'against', base, flush=True)


if __name__ == '__main__':
    receipts.require(len(sys.argv) == 2, 'Select the source-gate repair operation')
    main(sys.argv[1])
