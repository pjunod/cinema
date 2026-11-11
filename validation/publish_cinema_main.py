"""Publish the Cinema main release after its authentic split-run checks finish.

This bounded recovery controller retains successful unit execution instead of
claiming that an overall failed historical workflow passed. Each outstanding
runtime lane must independently finish successfully on its reviewed controller.
"""
from __future__ import annotations

import hashlib
import copy
from collections import Counter
import os
import json
import re
from pathlib import Path
import subprocess

from validation import postmerge_unit_retention as retention
from validation import promotion_unit_retention as evidence
from validation import qualification_unit_continuation as commands
from validation.landed_python_completion import verify_python_completion, native_job
from validation import python_unit_receipts as receipts
from validation import qualification
from validation import workspace_rust_outcomes

LANDING = 'c470d5aa033e84b4a2de1f8d36aa4180d62cd971'
UNITS = {'sha256': '8d53a0fec705f9e060e5a32c65a833a5c9bd4a02af1eb0a790491732b0b4c466',
         'attachment_id': 149, 'attachment_uuid': '31fc459d-0c43-484b-8696-6a61ae0ece4e'}
PYTHON = {'sha256': 'e75639ed83941e3be2aeca57c6a6c7e5c919ea5d91533445e149a11a8df887db',
          'attachment_id': 148, 'attachment_uuid': '23f429f1-f204-449e-b96d-5217ee48ebfd'}
# Filled only after reviewing the final package fix and terminal lane records.
REVIEWED_BUILD_TREE = '950f4942e612d23eba31e94f0685d366735cd34fb33351ab7a76e7b9497ad7e9'
CONTROL_PATHS = frozenset({
    '.github/workflows/cinema-main-publish.yml',
    'validation/publish_cinema_main.py',
})
JOBS = [
    (6818, 64189, 'cluster daemon contracts', 'cinema-landed-main-runtime.yml', '874ed05f1966c803bbce576731da632c240005b6'),
    (6833, 64248, 'cluster transport recovery voter role', 'cinema-landed-transport-paired.yml', '24b52fddb265569d6622754c25076c5472e509fc'),
    (6833, 64249, 'cluster transport recovery learner role', 'cinema-landed-transport-paired.yml', '24b52fddb265569d6622754c25076c5472e509fc'),
    (6833, 64250, 'cluster transport recovery campaign', 'cinema-landed-transport-paired.yml', '24b52fddb265569d6622754c25076c5472e509fc'),
    (6849, 64289, 'film-addressed VOD browser acceptance', 'cinema-landed-main-vod.yml', '806bff9caf314ac80653ff352713e06ee96f9a7c'),
    (6882, 64395, 'package and smoke (arm64)', 'cinema-main-package-retry.yml', '4a83e13d9252a97826fb0d2b88ad41cc0feafcf7'),
    (6890, 64417, 'package and smoke (amd64)', 'cinema-main-package-retry.yml', '4ecac497fd447228870b45c45b75f6f0b9a9db0a'),
]
AMD_IMAGE_ID = 'sha256:54621635d6c34a4d76b6426abb52c190fedc84c62b52f7ed556c627496a16389'
RUNTIME = {'sha256': '58d52ac43c18fe0de30bd85d4cd151cd4ad183b697e152441356c31cd2b33ea3',
           'attachment_id': 152, 'attachment_uuid': 'e17f205c-1950-4c63-80c2-2397b8a4295f'}
RUST = {"sha256": "b0724ffb983580aaadbfeb1fc0c2175a7533a546cd54114b5f1e5d57761ebefe",
        "attachment_id": 146, "attachment_uuid": "1649bb38-8987-4a63-b91a-7e07620ab46f"}

NATIVE_BINARIES = {'3ea9de3e5c10c3d79b33a90cd45866b92af4598e': 'a9570eb6dc64ab88fa0252bdb29ca5cb7a1f345645e70ee5c9fb64c9697c1006',
 '4f417e8527d7fa54b752969e556b8fd9a751ae38': '12f1d6c91487454039e4de701612fcc2c99fefe77f1084ec14c8c9b286a924ad',
 '8fab88105e5d046e3fc2b29e9f9e9c9a8ef55efe': 'e6da3225eea6fb7d5e9ebe07013fa68704482f184c06c4fe0e367afcfb76e4d8',
 '9fde9d14821a12f382c958744739c50a765da8de': '124585d9bcf01f8f0733714f743842ad8d6eb42c911ea6d5d3f0eeb6573e5d69',
 'c32bf261c13b7d137449432f39c1b020da1b0f4b': 'bb3d0713c1d979f92bd65240ffa7952a31a1e313d92a2e184ed6843efedb5b1c',
 'c5245d27e950ff6bf206e0dd15beacd486b5707b': '9db3966bdff2ab4832d1a1e03e0dc95ed1540e2f0d255bdc45bd335b2d927803',
 'd9a0ebc7668145a0de0a3851de2c808db1fe77c4': 'c1b10f570cb324122936ba411f6f2dccfec826ea72ec81d300583ce050c91336'}



class CachedReadAPI:
    """Reuse one run's immutable evidence reads within the existing API budget."""
    def __init__(self, api):
        self.api = api
        self.cache = {}

    def __getattr__(self, name):
        return getattr(self.api, name)

    def read(self, method, path, query=None, field=None):
        if path.startswith('/branches/'):
            return getattr(self.api, method)(path, query)
        key = (method, path, json.dumps(query, sort_keys=True), field)
        if key not in self.cache:
            call = getattr(self.api, method)
            self.cache[key] = call(path, query, field=field) if method == 'pages' else call(path, query)
        return copy.deepcopy(self.cache[key])

    def get(self, path, query=None):
        return self.read('get', path, query)

    def bytes(self, path, query=None):
        return self.read('bytes', path, query)

    def pages(self, path, query=None, field=None):
        return self.read('pages', path, query, field)


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


def topology_completion(api):
    digest = '823b850074c7caa8a576acc960944058d96fc99d0af11ad217668b338c9e4b00'
    uuid = 'f1516045-5d18-4dba-b11b-a7837217fc88'
    binding = retention.landing_binding(api, 1399, LANDING)
    raw = evidence.attachment_bytes(api, 1399, 151, uuid, digest)
    doc, _, _, attestations = evidence.verify_identity(
        api, raw, digest, binding, LANDING, 151, uuid)
    require = receipts.require
    require(doc['kind'] == 'landed-topology-nonunit-completion-v1'
            and doc['result'] == 'success' and doc['source_commit'] == LANDING
            and doc['source_tree'] == git('.', 'rev-parse', LANDING + '^{tree}')
            and doc['source_archive_sha256'] == commands.source_archive_digest(LANDING)
            and doc['current_unit_bodies_executed'] == 0,
            'Unexpected topology completion source or scope')
    measured = {}
    for name, row in doc['raw_evidence'].items():
        raw = evidence.decoded_log({key: row[key] for key in ('gzip_base64', 'sha256')}).encode()
        require(len(raw) == row['bytes'], 'Truncated native nonunit capture')
        measured[name] = raw
    for state in doc['runtime']['terminal_states'].values():
        require(state['status'] == 'exited' and state['exit'] == 0 and state['oom_killed'] is False,
                'Native nonunit campaign did not finish successfully')
    topology = receipts.bounded_json(measured['cluster-topology-semantic.json'])
    drills = receipts.bounded_json(measured['cluster-failure-drills.json'])
    require(topology['schema_version'] == 3 and topology['topology_order'] == [3, 4]
            and topology['build_sha'] == drills['build_sha'] == LANDING
            and drills['schema_version'] == 1
            and all(run['errors'] == run['catalogue_read_errors'] == 0
                    and run['physical_commit_entries'] == 64
                    and run['window_background_entries'] == 0 for run in topology['runs'])
            and drills['follower_loss']['writes_preserved']
            and drills['leader_loss']['writes_preserved']
            and drills['hls_backend_loss']['hls_session_survived_backend_loss'],
            'Native topology or failure-drill outcome differs')
    successful_job(api, 1, (6713, 63628, 'replicated topology contracts', 'ci.yml',
                            '65905cc9ae76d428374d5a383246ed1d7d12d5c2'))
    require(hashlib.sha256(api.bytes('/actions/jobs/63628/logs')).hexdigest()
            == doc['prior_ancillary']['origin']['log_sha256'],
            'Original vendor and spike command capture differs')
    return {'source': LANDING, 'origin': doc['runtime']['origin'],
            'attachment_id': 151, 'sha256': digest, 'attestations': attestations,
            'unit_bodies_reexecuted': 0}



def validate_daemon_partition(inventory, ignored, passing):
    require = receipts.require
    require(len(inventory) == 4927 and len(ignored) == 49
            and len(passing) == 4878 and ignored <= inventory
            and not ignored & passing and ignored | passing == inventory,
            'Current daemon names are not fully and disjointly accounted for')



def require_single_native_pass(log, name):
    """Require explicit libtest success, including actual nocapture line breaks."""
    clean = re.sub(r'\x1b\[[0-9;]*m', '', log)
    headers = re.findall(r'(?m)^test (\S+) \.\.\. ', clean)
    results = re.findall(r'(?m)^test result: (.*)$', clean)
    dispositions = re.findall(r'(?m)^test ' + re.escape(name) + r' \.\.\. (FAILED|ignored)(?:$|[ ;])', clean)
    receipts.require(headers == [name] and not dispositions
                     and not re.search(r'(?m)^(?:FAILED|ignored)(?:,.*)?$', clean)
                     and len(results) == 1
                     and results[0].startswith('ok. 1 passed; 0 failed; 0 ignored;')
                     and (re.search(r'(?m)^test ' + re.escape(name) + r' \.\.\. ok$', clean)
                          or re.search(r'(?m)^ok\n\ntest result: ok\. 1 passed; 0 failed; 0 ignored;', clean)),
                     'Native capture lacks one explicit successful expected case')



def partial_workspace_outcomes(raw, rust):
    """Read the actual filtered continuation, retaining its interrupted target."""
    require = receipts.require
    targets, header, active = {}, None, None
    for line in workspace_rust_outcomes.clean_lines(raw):
        match = workspace_rust_outcomes.HEADER.fullmatch(line)
        if match:
            require(active is None or targets[active]['complete'], 'Prior target ended without a terminal disposition')
            source, binary, docs = match.groups()
            require(docs is None, 'Unexpected doctest in the filtered native continuation')
            header, active = binary + '@' + source, None
        selected = re.fullmatch(r'(\S+): (\d+) discovered, (\d+) authenticated passes, (\d+) selected', line)
        if selected:
            key, total, retained, count = selected.groups()
            total, retained, count = map(int, (total, retained, count))
            require(key == header and key not in targets and count + retained == total
                    and retained == len(rust['retained'].get(key, [])), 'Filtered target selector differs')
            targets[key] = {'outcomes': {}, 'selected': count, 'retained': retained, 'complete': count == 0}
            active = key
        elif active is not None and not targets[active]['complete']:
            row = targets[active]
            body = workspace_rust_outcomes.BODY.fullmatch(line)
            summary = re.fullmatch(r'test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; 0 measured; (\d+) filtered out; finished in [\d.]+s', line)
            if body:
                name, outcome = body.groups()
                require(name not in row['outcomes'] and name not in rust['retained'].get(active, []),
                        'Duplicate or already-retained target-scoped native outcome')
                row['outcomes'][name] = workspace_rust_outcomes.OUTCOMES[outcome]
            elif summary:
                polarity, passed, failed, ignored, filtered = summary.groups()
                require(Counter(row['outcomes'].values()) == Counter({'pass': int(passed), 'fail': int(failed), 'ignored': int(ignored)})
                        and len(row['outcomes']) == row['selected'] and int(filtered) == row['retained']
                        and (polarity == 'ok') == (int(failed) == 0),
                        'Filtered native target summary contradicts its literal outcomes')
                row['complete'] = True
            elif line == 'fatal runtime error: stack overflow, aborting':
                require(active == 'plurxd@src/main.rs' and len(row['outcomes']) < row['selected'],
                        'Unexpected native continuation interruption')
                row['complete'] = True
                row['interrupted'] = True
    require(targets and targets.get('plurxd@src/main.rs', {}).get('interrupted')
            and sum(bool(row.get('interrupted')) for row in targets.values()) == 1
            and all(row['complete'] for row in targets.values()),
            'Original partial continuation framing differs')
    return targets


def runtime_completion(api, release, rust):
    require = receipts.require
    require(RUNTIME is not None, 'Native runtime completion is not enrolled')
    digest, attachment, uuid = RUNTIME['sha256'], RUNTIME['attachment_id'], RUNTIME['attachment_uuid']
    binding = retention.landing_binding(api, 1399, LANDING)
    raw = evidence.attachment_bytes(api, 1399, attachment, uuid, digest)
    doc, _, _, attestations = evidence.verify_identity(
        api, raw, digest, binding, LANDING, attachment, uuid)
    require(doc['kind'] == 'landed-cinema-runtime-completion-v1'
            and doc['result'] == 'success' and doc['reviewed_release_main'] == release
            and doc['inventory_source'] == 'c32bf261c13b7d137449432f39c1b020da1b0f4b'
            and git('.', 'diff', '--name-only', release, doc['inventory_source']).splitlines()
            == ['crates/plurxd/src/http/jellyfin/tests.rs'],
            'Native runtime completion source differs')
    captured = {}
    for name, row in doc['raw_evidence'].items():
        raw = evidence.decoded_log({key: row[key] for key in ('gzip_base64', 'sha256')}).encode()
        require(len(raw) == row['bytes'], 'Truncated native completion capture')
        captured[name] = raw.decode()
    inventory = set(re.findall(r'(?m)^(\S+): test$', captured['daemon-list.log']))
    ignored = set(re.findall(r'(?m)^(\S+): test$', captured['daemon-ignored-list.log']))
    old = native_job(api, 6821, 64195, 'cinema-landed-main-rust.yml', 'workflow_dispatch',
                     'dd085e9d6a6ced9d3da742b5e6d29838b624ef33', 'failure',
                     'b7739dc290019f071a0457c6394779abcc32dfed2e712a87d90ab2dd4b417582')
    require(old == captured['ci64195.log'], 'Enrolled partial Rust capture differs from its actual CI origin')
    layout = native_job(api, 6818, 64190, 'cinema-landed-main-runtime.yml', 'workflow_dispatch',
                        '874ed05f1966c803bbce576731da632c240005b6', 'failure',
                        '3ee5f2047843cddcd0859cb902465ccfea116595b87e6d0079aa88bdf9068fd7')
    layout = '\n'.join(workspace_rust_outcomes.clean_lines(layout.encode()))
    translation = 'progressive translation retains the shipped video, literal cues, original captions and Stop intent'
    require(re.search(r'(?m)^104 captures in [0-9]+s \(14933 structural facts\), no console or page errors\.$', layout)
            and 'PASS  reporter completed 3 exchanges in a real browser' in layout
            and len(re.findall(r'(?m)^✔ ', layout)) == 28
            and set(re.findall(r'(?m)^✖ (.*?) \([0-9.]+ms\)$', layout)) == {translation}
            and re.findall(r'(?m)^ℹ tests ([0-9]+)$', layout) == ['18', '7', '2', '3']
            and re.findall(r'(?m)^ℹ pass ([0-9]+)$', layout) == ['17', '7', '2', '2']
            and re.findall(r'(?m)^ℹ fail ([0-9]+)$', layout) == ['0', '0', '0', '1'],
            'Prior passing layout and browser outcomes or the one failed case differ')
    require(hashlib.sha256(old.encode()).hexdigest()
            == 'b7739dc290019f071a0457c6394779abcc32dfed2e712a87d90ab2dd4b417582',
            'Original partial Rust execution capture differs')
    fresh = doc['fresh_ci_passes']
    observed = partial_workspace_outcomes(old.encode(), rust)
    require(sum(len(names) for names in fresh.values()) == 525
            and all(len(names) == len(set(names))
                    and all(observed.get(target, {}).get('outcomes', {}).get(name) == 'pass'
                            for name in names) for target, names in fresh.items())
            and all(set(names) <= set(rust['retained'].get(target, [])) | set(fresh.get(target, []))
                    for target, names in rust['must_pass'].items()),
            'Required new Rust outcomes are missing from the observed continuation')
    daemon = 'plurxd@src/main.rs'
    require(daemon in rust['retained'] and daemon in fresh,
            'Daemon retention identity differs')
    passing = set(rust['retained'][daemon]) | set(fresh[daemon])
    for key in ('daemon-missing.log', 'daemon-missing-stage2.log'):
        # Exclude quoted child harnesses in failed-test diagnostics.
        outer = captured[key].split('\nfailures:\n', 1)[0]
        outcomes = dict(re.findall(r'(?m)^test (\S+) \.\.\. (ok|FAILED|ignored)(?:[^\n]*)$', outer))
        passing.update(name for name, outcome in outcomes.items() if outcome == 'ok' and name not in ignored)
    bindings = receipts.bounded_json(captured['source-build-bindings.json'].encode())['phases']
    require(len(bindings) == len(doc['repair_phases'])
            and len({row['source'] for row in bindings}) == len(bindings),
            'Native phase source bindings are incomplete or duplicated')
    bindings = {row['source']: row for row in bindings}
    sources = set()
    for phase in doc['repair_phases']:
        bound = bindings[phase['source']]
        artifacts = bound['plurxd_test_compiler_artifacts']
        require(bound['source_git_identity_matches_at_observation'] is True
                and bound['compiler_source_mount_verified_from_existing_container'] is True
                and bound['rustc_pinned_1_97_1_observed'] is True
                and bound['git_archive_sha256'] == phase['source_archive_sha256']
                and bound['saved_container_inspect']['image'].rsplit('@', 1)[-1]
                == 'sha256:3d84b711936233da699308d0301ac1e91f12e76c6b928967c41817e841f5e855'
                and bound['build_finished'] == {'reason': 'build-finished', 'success': True}
                and len(artifacts) == 1 and artifacts[0]['profile']['test'] is True
                and artifacts[0]['features'] == []
                and bound['recorded_execution_binary_sha256'] == [NATIVE_BINARIES[phase['source']]],
                'Native recorded execution does not bind to its reviewed source/build evidence')
        require(phase['source_archive_sha256'] == commands.source_archive_digest(phase['source']),
                'Native repair archive does not match its recorded source')
        sources.add(phase['source'])
        rows = [json.loads(line) for line in captured[phase['status_capture']].splitlines()]
        for row in rows:
            if row['exit_code'] != 0:
                continue
            name = row['case']
            log = captured[phase['case_logs'][name]]
            require(row['source'] == phase['source']
                    and row['binary_sha256'] == NATIVE_BINARIES[phase['source']]
                    and hashlib.sha256(log.encode()).hexdigest() == phase['case_log_sha256'][name]
                    and ('log_sha256' not in row or row['log_sha256'] == phase['case_log_sha256'][name]),
                    'A repaired case lacks its actual named native pass')
            require_single_native_pass(log, name)
            passing.add(name)
    final = bindings[doc['inventory_source']]
    binary = final['terminal_binary_proof']
    final_case = 'http::jellyfin::tests::jellyfin_native_hls_encoded_without_copy_index_preserves_ranges_clock_and_stop'
    require(final['independent_preserved_binary_bytes_verified'] is True
            and binary['sha256'] == NATIVE_BINARIES[doc['inventory_source']]
            and binary['source'] == doc['inventory_source'] and binary['case'] == final_case
            and binary['stable_during_read'] is True and binary['matches_recorded_execution_status'] is True
            and binary['terminal_container_success'] is True and binary['explicit_named_libtest_pass'] is True
            and binary['exact_saved_container_command_matches_argv'] is True,
            'Final native executable bytes do not match their source-bound terminal pass')
    hdr = api.get('/pulls/1432')
    hdr_name = 'transcode::manager_create::processed_reuse_tests::processed_cache_args_match_the_destination_window_encoder'
    require(hdr['merged'] and hdr['state'] == 'closed'
            and hdr['head']['sha'] == doc['hdr_prior_pass']['source']
            and hashlib.sha256(hdr['body'].encode()).hexdigest() == doc['hdr_prior_pass']['body_sha256']
            and 'passed all four cases inside the named regression on Linux' in hdr['body'],
            'The independently retained HDR native regression report differs')
    receipts.verify_attestor(api, {'repository': 1}, hdr['user'])
    passing.add(hdr_name)
    passing &= inventory
    validate_daemon_partition(inventory, ignored, passing)
    terminal = receipts.bounded_json(captured['terminal-state.json'].encode())
    require(terminal['Status'] == 'exited' and terminal['ExitCode'] == 0
            and terminal['OOMKilled'] is False, 'Final native completion has not exited successfully')
    browser = captured['translation-case.log']
    require(all(re.search(r'(?m)^ℹ ' + key + r' ' + count + r'$', browser)
                    for key, count in [('tests', '1'), ('pass', '1'), ('fail', '0'), ('skipped', '0')])
            and re.search(r'(?m)^✔ progressive translation retains the shipped video, literal cues, original captions and Stop intent \([0-9.]+ms\)$', browser)
            and doc['browser']['exit_code'] == 0
            and doc['browser']['source'] == '8fab88105e5d046e3fc2b29e9f9e9c9a8ef55efe',
            'The previously failed browser case has no measured corrected-source pass')
    ui = doc['learner_ui']
    require(set(ui['source_files']) == {'tests/web/activity-node-names.test.js',
                                         'crates/plurxd/src/web/pages/activity.js'}
            and ui['exit_code'] == 0 and ui['filter']['PLURX_ACTIVITY_TEST_NAME']
            == 'learner worker telemetry contributes capacity independently of voter activity'
            and hashlib.sha256(captured['learner-ui-focused.log'].encode()).hexdigest() == ui['log_sha256']
            and all(hashlib.sha256(evidence.git_bytes(release, path)).hexdigest() == digest
                    for path, digest in ui['source_files'].items()),
            'The learner UI observation differs from the shipped sources')
    return {'attachment_id': attachment, 'sha256': digest, 'attestations': attestations,
            'daemon_native_passes': len(passing) - 1, 'daemon_retained_author_report_passes': 1,
            'daemon_ignored': len(ignored),
            'fresh_original_ci_passes': 525, 'sources': sorted(sources),
            'historical_failures_preserved': True, 'successful_bodies_replayed': 0}


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
        python = verify_python_completion(api, LANDING,
            retention.select(api, 'python', PYTHON, 1399, LANDING))
        topology = topology_completion(api)
        rust = retention.select(api, "rust", RUST, 1399, LANDING)
        runtime = runtime_completion(api, source_sha, rust)
    finally:
        os.chdir(previous)
    outcomes = [successful_job(api, repo_id, expected) for expected in JOBS]
    return {'source': source_sha, 'tree': git(source, 'rev-parse', 'HEAD^{tree}'),
            'retained_units': retained, 'python': python, 'topology': topology, 'runtime': runtime, 'jobs': outcomes}


def main():
    api = CachedReadAPI(receipts.API(os.environ['GITHUB_API_URL'], os.environ['GITHUB_REPOSITORY'],
                                     os.environ['GITHUB_TOKEN']))
    record = verify(api, os.environ, Path('source').resolve(), Path('landing').resolve())
    receipts.atomic_json(Path(os.environ['RUNNER_TEMP']) / 'cinema-publication.json', record)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        print('image_id=' + AMD_IMAGE_ID, file=output)
    print('Verified actual main source and every required Cinema check', record['source'])


if __name__ == '__main__':
    main()
