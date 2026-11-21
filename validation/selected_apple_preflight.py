"""Read the original complete desktop, shared XCTest and TV navigation outcomes."""
from __future__ import annotations

import hashlib
import os
import re

from validation import native_receipts as native
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r
from validation import qualification_unit_continuation as commands
from validation import retained_apple_job as apple
from validation.retained_lane_commands import scoped_workflow
from validation.workspace_rust_continuation import selected_preflight_projection

POLICY = 'selected-apple-native-preflight-v1'
READER = 'crates/plurxd/src/web/reader.js'
READER_TRANSITION = (
    '582f41bfe1a5325a2625c89d39a050c4d9721d3551b1dbea764b8233db575f56',
    '148fe8b220e4473cadbf3fe44d4ba38b4e0ce567aee6bfa5d353f1bea76fbae8',
)
PROJECT = 'clients/apple/project.yml'
PROJECT_SHA = 'b35e2b3f9e1ec9cde24b02fef0f674c3c95e790c7c5631d801fedd668648f0b8'
PIP_CASE = 'PictureInPictureAttachmentTests/testUnsupportedAttachmentDoesNotRepublishDuringViewUpdates'
PIP_SOURCE = 'clients/apple/Tests/PictureInPictureAttachmentTests.swift'
PIP_SHA = '3a9964c15e5fdf882bf1bc13d54eb6245207b68571bcc4baa4ae5b8a72dc9038'
# Complete unprefixed spans from the original job. Test identity text comes
# from the first and last raw fragments, never from the intervening diagnostics.
FRAGMENTS = (
    ("Test Case '-[plurx_iOSTests.PlayerOperationOwnershipTests testFailedVODResume",
     "StopsAndRetainsTheSavedPosition]' passed (0.081 seconds).", 2,
     '1d2bc967cf3d1d8b3ec7c6d82b7f8040f9b89637b9fb5877abd591ceb9b616ba'),
    ("Test Case '-[plu", "rx_iOSTests.PlayerResumeTests testExpiredAttemptCannotAdmitARepair]' started.", 2,
     '81fd726a54271a0b2829201cba8b9304903af9d72b34ffcb0013b7405964de7b'),
    ("Test Case '-[plurx_iOSTests.EffectiveProcessingPresentationTests testReportGatedEnhancementShowsOnlyWithThe",
     "CurrentReport]' started.", 4, '7ab3fabacad3ab9e388a2527d6fadfa3515bc28929a2132eeafed9967f6e5431'),
    ("Test Case '-[plurx_iOSTests.LiveTvTests testLiveTvOldRouteLossKeepsVisibleP",
     "layFunctional]' started.", 3, 'a20b31f328174b04eef78ae724ee6edcfa7dd342f1f1c4a587e16321dbe223cc'),
)
SHARED_PROFILES = {'iphone': ('iPhone', 'iOS', 1288, 0),
                   'ipad': ('iPad', 'iOS', 1288, 1),
                   'tvos': ('Apple TV', 'tvOS', 1277, 0)}


def desktop_proof(api, descriptor, scope, current, current_run):
    r.require(isinstance(descriptor, dict) and set(descriptor) == {'run', 'job', 'artifact', 'archive_sha256'},
              'Unknown selected desktop preflight description')
    run = r.positive(descriptor['run'])
    artifact = artifact_for(api, descriptor, f'desktop-macos-check-{run}-1')
    origin = native.origin(api, artifact, scope, current, current_run)
    r.require(origin['job'] == descriptor['job'] and origin['pr'] == scope['pr']
              and native.unchanged(origin['commit'], current, 'desktop_macos'),
              'Desktop test origin or current inputs differ')
    log = api.bytes(f"/actions/jobs/{origin['job']}/logs")
    require_archive_creator(log, artifact['name'], descriptor['archive_sha256'])
    with authenticated_download(api, artifact, descriptor['archive_sha256']) as stream:
        data = native.archive_member(stream, 'desktop-macos-check.log')
    outcomes = desktop_outcomes(data)
    inputs = native.input_digest(current, {'inputs': native.INPUTS['desktop_macos']})
    return {'origin': origin, 'archive_sha256': descriptor['archive_sha256'],
            'log_sha256': retention.digest(data), 'input_digest': inputs, 'outcomes': outcomes}


def require_archive_creator(log, name, expected):
    r.require(isinstance(expected, str) and retention.DIGEST.fullmatch(expected)
              and log.count(f'Artifact {name} has been successfully uploaded!'.encode()) == 1
              and log.count(f'SHA256 hash of uploaded artifact zip is {expected}'.encode()) == 1,
              'Native artifact lacks its creator log digest')


def desktop_outcomes(data):
    native.verify_desktop(data)
    cases = re.compile(r"Test Case '-\[(PlurxTrayUITests|PlurxTrayCoreTests)\.([A-Za-z_]\w*) "
                       r"(test\w+)\]' (started\.|passed \([0-9]+(?:\.[0-9]+)? seconds\)\.)")
    sections = re.split(r"(?m)^Test Suite 'All tests' started at [^\n]+\n", data.decode('utf-8'))
    expected = {'PlurxTrayUITests': 14, 'PlurxTrayCoreTests': 61}
    r.require(len(sections) == len(expected) + 1, 'Missing native desktop XCTest suite')
    outcomes = {}
    for (namespace, count), text in zip(expected.items(), sections[1:], strict=True):
        r.require(len(re.findall(r"(?m)^Test Suite 'All tests' passed at [^\n]+\n"
                                 + rf'\s*Executed {count} tests, with 0 failures \(0 unexpected\)', text)) == 1,
                  'Desktop XCTest terminal does not match its required suite')
        started, passed = set(), set()
        for line in text.splitlines():
            if not line.startswith('Test Case '):
                continue
            event = cases.fullmatch(line)
            r.require(event is not None and event[1] == namespace, 'Unexpected desktop XCTest case or terminal')
            case = event[2] + '/' + event[3]
            if event[4] == 'started.':
                r.require(case not in started, 'Repeated desktop XCTest start')
                started.add(case)
            else:
                r.require(case in started and case not in passed, 'Unbound or repeated desktop XCTest outcome')
                passed.add(case)
        r.require(started == passed and len(passed) == count, 'Incomplete desktop XCTest case census')
        outcomes[namespace] = {'pass_count': count, 'cases_sha256': retention.digest('\n'.join(sorted(passed)).encode())}
    return outcomes


def artifact_for(api, descriptor, name):
    artifact_id = r.positive(descriptor['artifact'])
    run = r.positive(descriptor['run'])
    rows = [row for row in api.pages(f'/actions/runs/{run}/artifacts') if row['id'] == artifact_id]
    r.require(len(rows) == 1 and rows[0]['name'] == name and rows[0]['run_id'] == run
              and rows[0]['expired'] is False, 'Native artifact outside its actual job')
    return rows[0]


def authenticated_download(api, artifact, expected):
    stream = native.download(api, artifact)
    try:
        r.require(hashlib.file_digest(stream, 'sha256').hexdigest() == expected,
                  'Actual native artifact bytes differ from their enrolled digest')
        stream.seek(0)
        return stream
    except BaseException:
        stream.close()
        raise


def reviewed_mobile_inputs(source, current):
    choices = []
    for rev in (source, current):
        native.regular_inputs(rev, 'tv_navigation')
        for path in native.FAMILY_SPECS['tv_navigation']:
            raw = retention.git_bytes(rev, path)
            r.require(raw is not None and (
                native.reviewed_build_spec(path, raw)
                or (path == 'scripts/apple-tv-navigation-test' and retention.digest(raw) == native.CASE_RUNNER_SHA256)
            ), 'Unreviewed native build specification: ' + path)
        r.require(retention.digest(retention.git_bytes(rev, PROJECT)) == PROJECT_SHA
                  and retention.digest(retention.git_bytes(rev, PIP_SOURCE)) == PIP_SHA,
                  'Reviewed Apple target boundary or PiP guard changed')
        paths = [path for path in native.INPUTS['tv_navigation'] if path != READER]
        choices.append(native.input_digest(rev, {'inputs': paths}))
    reader = tuple(retention.digest(retention.git_bytes(rev, READER)) for rev in (source, current))
    # reader.js belongs to the iOS app only. This reviewed CSS/style injection
    # change is outside XCTest assertions and the tvOS target's resource graph.
    r.require(choices[0] == choices[1] and (reader[0] == reader[1] or reader == READER_TRANSITION),
              'Native Apple test inputs changed outside the reviewed reader resource transition')
    return {'input_digest': choices[1], 'reader_resource_sha256': list(reader), 'project_sha256': PROJECT_SHA}


def rejoin_fragments(text):
    lines = text.splitlines()
    for prefix, suffix, length, sha in FRAGMENTS:
        matches = [index for index, line in enumerate(lines) if line.startswith(prefix)
                   and index + length <= len(lines) and lines[index + length - 1] == suffix
                   and retention.digest('\n'.join(lines[index:index + length]).encode()) == sha]
        r.require(len(matches) <= 1, 'Repeated original Apple XCTest fragment')
        if matches:
            index = matches[0]
            lines[index:index + length] = [prefix + suffix]
    return '\n'.join(lines)


def shared_outcomes(log, run):
    text = re.sub(r'(?m)^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', log.decode('utf-8', 'strict'))
    text = rejoin_fragments(text)
    r.require(text.count('\nXcode 27.2\nBuild version 27B52\n') == 1,
              'Actual Apple toolchain differs')
    results = {}
    for profile, (device, os_name, total, skipped) in SHARED_PROFILES.items():
        boots = re.findall(r'^Monitoring boot status for plurx CI ' + re.escape(device)
                           + rf' {run}-1 \(([0-9A-F-]{{36}})\)\.$', text, re.M)
        r.require(len(boots) == 1, 'Apple profile lacks its original simulator')
        command = (f'/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild '
                   f'-project plurx.xcodeproj -scheme plurx-{os_name} '
                   f'-destination "platform={os_name} Simulator,id={boots[0]}" '
                   '-derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO test-without-building')
        block = apple.invocation_block(text, command)
        expected = (rf"Test Suite 'All tests' passed[^\n]*\n[^\n]*Executed {total} tests, with "
                    + ('1 test skipped and ' if skipped else '') + r'0 failures \(0 unexpected\)')
        r.require(len(re.findall(expected, block)) == 1 and block.count('.xcresult') == 1,
                  'Apple profile has no complete original XCTest summary')
        starts = re.findall(r"Test Case '-\[\w+\.(\w+) (test\w+)\]' started\.", block)
        outcomes = retention.log_outcomes(block, 'xctest-class')
        ignored = sorted(case for case, result in outcomes.items() if result == 'ignored')
        r.require(len(starts) == len(outcomes) == total
                  and {name + '/' + method for name, method in starts} == set(outcomes)
                  and set(outcomes.values()) <= {'pass', 'ignored'}
                  and ignored == ([PIP_CASE] if skipped else []),
                  'Apple named outcomes or profile capability skips differ from actual execution')
        if profile == 'iphone':
            r.require(outcomes.get(PIP_CASE) == 'pass', 'PiP unsupported path did not execute on iPhone')
        if skipped:
            r.require('This regression exercises a device without PiP support' in block,
                      'Unexpected iPad skip reason')
        lines = sorted(f'{name}:{outcome}' for name, outcome in outcomes.items())
        results[profile] = {'pass_count': total - skipped, 'ignored': ignored,
                            'outcomes_sha256': retention.digest('\n'.join(lines).encode())}
    return results


def mobile_proof(api, descriptor, scope, current, base, current_run):
    fields = {'run', 'job', 'pr', 'source_commit', 'artifact', 'archive_sha256', 'job_log_sha256'}
    r.require(isinstance(descriptor, dict) and set(descriptor) == fields, 'Unknown selected Apple mobile proof')
    rid, job_id, pr = (r.positive(descriptor[k]) for k in ('run', 'job', 'pr'))
    source = r.sha(descriptor['source_commit'])
    run = api.get(f'/actions/runs/{rid}')
    payload = r.bounded_json(run['event_payload'].encode())
    pull = api.get(f'/pulls/{pr}')
    repository = scope['repository']
    inputs = payload['inputs']
    r.require((current_run is None or rid < current_run) and run['id'] == rid and run['commit_sha'] == source
              and run['repository']['id'] == payload['repository']['id'] == repository
              and run['workflow_id'] == 'ci.yml' and run['trigger_event'] == 'workflow_dispatch'
              and pull['number'] == pr and pull['state'] == 'closed' and pull['merged'] is True
              and pull['head']['sha'] == source and pull['base']['ref'] == 'main'
              and pull['head']['repo']['id'] == pull['base']['repo']['id'] == repository
              and inputs['promotion_pr'] == str(pr) and inputs['promotion_head_sha'] == source
              and payload['ref'] == 'refs/heads/' + run['prettyref']
              and not any(inputs.get(k) for k in inputs if 'evidence' in k or 'retained_units' in k),
              'Original mobile result is not an independent bound native CI execution')
    from validation.main_python_continuation import main_ancestor
    r.require(main_ancestor(source, base) and main_ancestor(r.sha(pull['merge_commit_sha']), base),
              'Original mobile source is not merged into the current main')
    raw_workflow = retention.git_bytes(source, '.github/workflows/ci.yml')
    workflow = selected_preflight_projection(retention.git_bytes(current, '.github/workflows/ci.yml'))
    r.require(scoped_workflow(raw_workflow, 'apple') == scoped_workflow(workflow, 'apple'),
              'Original Apple executor differs from current unselected recipe')
    commands.authenticate_scope(api, rid, run, raw_workflow.decode(), pull, source, repository)
    jobs = [job for job in api.pages(f'/actions/runs/{rid}/jobs') if job['name'] == 'Apple iOS + tvOS tests']
    r.require(len(jobs) == 1 and jobs[0]['id'] == job_id and jobs[0]['run_id'] == rid
              and jobs[0]['repo_id'] == repository and jobs[0]['attempt'] == 1
              and jobs[0]['status'] == 'success' and jobs[0]['runs_on'] == native.RUNS_ON,
              'Original Apple mobile job did not pass its native environment contract')
    log = api.bytes(f'/actions/jobs/{job_id}/logs')
    artifact_name = f'tv-navigation-{rid}-1'
    r.require(retention.digest(log) == descriptor['job_log_sha256'], 'Actual mobile job log differs')
    require_archive_creator(log, artifact_name, descriptor['archive_sha256'])
    projection = reviewed_mobile_inputs(source, current)
    profiles = shared_outcomes(log, rid)
    with authenticated_download(api, artifact_for(api, descriptor, artifact_name), descriptor['archive_sha256']) as stream:
        receipt = native.archive_member(stream, 'receipt.json')
        stream.seek(0)
        navigation_log = native.archive_member(stream, 'xcodebuild.log', 32 * 1024 * 1024)
    parsed = r.bounded_json(receipt)
    passed = native.verify_tv_cases(parsed, navigation_log.decode('utf-8', 'strict'), source)
    required = native.required_tests(current)
    r.require(parsed['status'] == 'passed' and parsed['selected_tests'] == required and set(required) == passed,
              'Incomplete current TV navigation coverage')
    return {'origin': descriptor, 'profiles': profiles, 'inputs': projection,
            'navigation': {'passed': sorted(passed), 'receipt_sha256': retention.digest(receipt),
                           'log_sha256': retention.digest(navigation_log)}}


def prepare(api, descriptor, scope, binding, candidate):
    r.require(isinstance(descriptor, dict) and set(descriptor) == {'version', 'desktop', 'mobile'}
              and descriptor['version'] == POLICY, 'Unknown selected native Apple preflight')
    current_run = int(os.environ['GITHUB_RUN_ID']) if os.environ.get('GITHUB_RUN_ID') else None
    return {'provider': POLICY, 'desktop': desktop_proof(api, descriptor['desktop'], scope, candidate, current_run),
            'mobile': mobile_proof(api, descriptor['mobile'], scope, candidate, binding['base_sha'], current_run)}
