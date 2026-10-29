"""Authenticate original native passes and retain them for unchanged inputs.

This emits adoption evidence, never a replacement native pass. The current
Apple job still verifies its pinned tools. A successful original job and its
reviewed producer bind both build and test operations to the unchanged inputs.
Whole-suite adoption still requires a successful original job. A separate
case planner retains authenticated native passes from failed original jobs;
only its missing or inapplicable cases remain due. Builds retain their full
original-job guard.
The schema-1 producer did not record a macOS build fingerprint: compatibility
here is the original workflow's pinned Xcode/architecture/runtime contract,
not a claim that the host OS or physical-device acceptance was reproduced.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import http.client
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import zlib

from validation.main_preflight_adoption import (
    UNADOPTABLE, api_context, bind_event, commit_available, git, input_digest,
    matches_input, reason, terminal_status,
)
from validation.python_unit_receipts import (
    IntegrityError, ReceiptError, atomic_json, bounded_json, positive, require,
)

JOB = 'Apple compile and native TV navigation'
WORKFLOW = '.github/workflows/main-fast-lane.yml'
RUNS_ON = ['self-hosted', 'macOS', 'ARM64', 'lab', 'apple', 'xcode-27.1']
XCODE = 'Xcode 27.1\nBuild version 27A9275'
RUNTIME = 'com.apple.CoreSimulator.SimRuntime.tvOS-26-5'
# Reviewed schema-1 producer: unconditional pinned-tool checks; native make
# commands conditional only on affected scope; failure-preserving artifacts.
CASE_LEGACY_JOB_SHA256 = '2543b6deb3f55e0743422125cff95f0fb3d7ecee18c4d9e07cd322b7d4e831d8'
LEGACY_JOB_SHA256 = 'c6b5c40ac94f5505d0afd1f9d724a9fec8ca4f5a077fff01a6e738933bd8a415'
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_EXPANDED = 2 * 1024 * 1024 * 1024
MAX_MEMBERS = 50000
MAX_SOURCE = 1024 * 1024 * 1024
MAX_CANDIDATES = 16
MAX_ARTIFACT_PAGES = 20
UNADOPTABLE_NATIVE = UNADOPTABLE + (zlib.error,)
REPORT = Path('.main-preflight-receipts/native/receipt.json')
COMMON = ['Makefile', '.gitattributes']
# Reviewed project.yml resources; native fixture servers read bundled assets.
# Exact build-specification hashes below bind this reviewed resource boundary.
# Any spec change, including new quoted paths or build scripts, withholds reuse
# until the boundary is reviewed again; this is not a generic YAML parser.
APPLE_EXTERNAL_INPUTS = [
    'crates/plurxd/src/web/reader.js', 'crates/plurxd/src/web/offline-reader.js',
    'crates/plurxd/src/web/offline-reader.html',
    'crates/plurx-core/tests/fixtures/diagnostics-events-v1.json',
    'crates/plurx-core/tests/fixtures/diagnostics-v1.json',
    'crates/plurx-core/tests/fixtures/remote-control-v1.json',
    'tests/contracts/channel-subject-wire.json', 'tests/contracts/native-api.json',
    'tests/contracts/library-sort-cases.json',
    'tests/playback/player-input-contract.json', 'tests/playback/playback-info-fields.json',
    'tests/playback/playback-surface-contract.json', 'tests/playback/live-tv-guide-cases.json',
    'tests/playback/live-tv-start-cases.json', 'tests/playback/pgs-overlay-cases.json',
    'tests/playback/dvr-visibility-cases.json', 'tests/sharing/protocol-cases.json',
]
BUILD_SPECS = {
    'Makefile': 'a8dfee1e29ba93b43cf58ee5233a5aef1aeddd381765a759d47b05c8126b23c2',
    # Reviewed 0.8.0 build 283: only marketing version and build 282 changed.
    'clients/apple/project.yml': '78c9c78c7d65c8940a628cb42c633fc0a6992fe205d5dff83bd9a41e659ffe43',
    'scripts/apple-tv-navigation-test': 'c758a25eb73a8991508d5ed165e615dd12c49b5e8edbb990f97826ddc1dc0b4e',
    'clients/server-tray/macos/Package.swift': '8e5ac76e4b9bece61bd85f3f92ecdceca2482bd99cfbd23bbdf4e0b6dee0c322',
    'clients/server-tray/macos/build': 'b7135b8d5de4ae53f86a5260965c72f257cd73ccfdac56477ee6688bd86456b0',
}
FAMILY_SPECS = {
    'tv_navigation': ['Makefile', 'clients/apple/project.yml', 'scripts/apple-tv-navigation-test'],
    'desktop_macos': ['Makefile', 'clients/server-tray/macos/Package.swift', 'clients/server-tray/macos/build'],
}
INPUTS = {
    'tv_navigation': COMMON + ['clients/apple/**', 'scripts/apple-tv-navigation-test',
        *APPLE_EXTERNAL_INPUTS],
    'desktop_macos': COMMON + ['clients/server-tray/macos/**', 'LICENSE', 'VERSION', 'Cargo.toml',
        'brand/export/icon*'],
}


def native_block(workflow):
    matches = list(re.finditer(r'(?ms)^  apple_compile:\n.*?(?=^  [A-Za-z0-9_]+:|\Z)', workflow))
    require(len(matches) == 1, 'Ambiguous Apple workflow producer')
    return matches[0].group()


def producer(commit, current):
    old = native_block(git('show', commit + ':' + WORKFLOW))
    now = native_block(git('show', current + ':' + WORKFLOW))
    require(hashlib.sha256(old.encode()).hexdigest() in (LEGACY_JOB_SHA256, CASE_LEGACY_JOB_SHA256) or old == now,
            'Unknown native workflow producer')
    # The adapter's own adoption records are deliberately never accepted as
    # native origin evidence; producer output must still be an original pass.


def build_specs(commit, family):
    """Only the reviewed deterministic producers define this input closure."""
    for path in FAMILY_SPECS[family]:
        data = subprocess.check_output(['git', 'show', commit + ':' + path], timeout=10)
        require(hashlib.sha256(data).hexdigest() == BUILD_SPECS[path],
                'Unreviewed native build specification: ' + path)


def regular_inputs(commit, family):
    patterns = INPUTS[family]
    for row in git('ls-tree', '-r', '-z', commit).split('\0'):
        if not row:
            continue
        header, path = row.split('\t', 1)
        mode, kind, _ = header.split()
        # Also reject symlink/gitlink ancestors of a selected subtree: replacing
        # clients/apple itself with a link must not make its inputs disappear.
        if any(matches_input(path, pattern) or pattern.startswith(path + '/')
               for pattern in patterns):
            require(mode in ('100644', '100755') and kind == 'blob',
                    'Native input closure contains a symlink/submodule: ' + path)


def unchanged(commit, current, family):
    for source in (commit, current):
        build_specs(source, family)
        regular_inputs(source, family)
    row = {'inputs': INPUTS[family]}
    return input_digest(commit, row) == input_digest(current, row)


def full_source_digest(commit):
    """Reproduce schema-1 path+NUL+file-content hash from clean Git blobs.

    Submodules and symlinks cannot be reconstructed as that producer's ordinary
    worktree files with certainty and therefore withhold reuse. Closure hashes
    independently bind file modes, names, removals and additions.
    """
    entries = []
    for row in git('ls-tree', '-r', '-z', commit).split('\0'):
        if not row:
            continue
        header, path = row.split('\t', 1)
        mode, kind, oid = header.split()
        require(mode in ('100644', '100755') and kind == 'blob',
                'Legacy full-tree proof cannot resolve symlinks/submodules')
        entries.append((path, oid))
    require(len(entries) <= MAX_MEMBERS, 'Source file bound exceeded')
    digest, total = hashlib.sha256(), 0
    with subprocess.Popen(['git', 'cat-file', '--batch'], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE) as process:
        try:
            for path, oid in sorted(entries):
                process.stdin.write((oid + '\n').encode())
                process.stdin.flush()
                header = process.stdout.readline(128).decode().split()
                require(len(header) == 3 and header[:2] == [oid, 'blob'], 'Invalid source blob')
                size = int(header[2])
                require(0 <= size <= MAX_SOURCE - total, 'Source byte bound exceeded')
                total += size
                digest.update(path.encode())
                digest.update(b'\0')
                remaining = size
                while remaining:
                    chunk = process.stdout.read(min(remaining, 1024 * 1024))
                    require(chunk, 'Truncated source blob')
                    digest.update(chunk)
                    remaining -= len(chunk)
                require(process.stdout.read(1) == b'\n', 'Invalid source blob framing')
        finally:
            process.stdin.close()
            process.stdout.close()
            process.wait(timeout=10)
    return digest.hexdigest()


class PublicRedirect(urllib.request.HTTPRedirectHandler):
    """Artifact redirects may be signed; repository credentials never follow."""
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        parsed = urllib.parse.urlsplit(newurl)
        require(parsed.scheme in ('https', 'http') and parsed.hostname
                and not parsed.username and not parsed.password and not parsed.fragment,
                'Unsafe artifact redirect')
        # Do not use the parent's copied request headers, even for the same host.
        return urllib.request.Request(newurl, headers={'Accept': 'application/octet-stream'})


def safe_storage():
    require(all(not parent.is_symlink() for parent in (REPORT, *REPORT.parents)),
            'Receipt storage symlink')


def download(api, artifact):
    """Stream into task-local storage; do not load/extract large bundles."""
    size = artifact['size_in_bytes']
    require(type(size) is int and 0 < size <= MAX_ARCHIVE, 'Artifact size outside bound')
    ident = positive(artifact['id'])
    url = api.root + f'/actions/artifacts/{ident}/zip'
    request = urllib.request.Request(url, headers={'Authorization': 'token ' + api.token})
    opener = urllib.request.build_opener(PublicRedirect())
    # Explicit local directory avoids unowned system-temp evidence/cache files.
    directory = REPORT.parent / 'downloads'
    safe_storage()
    require(not directory.is_symlink(), 'Artifact directory symlink')
    directory.mkdir(parents=True, exist_ok=True)
    result = tempfile.TemporaryFile(dir=directory)
    try:
        with opener.open(request, timeout=30) as response:
            total = 0
            while True:
                chunk = response.read(min(1024 * 1024, MAX_ARCHIVE + 1 - total))
                if not chunk:
                    break
                total += len(chunk)
                require(total <= MAX_ARCHIVE, 'Artifact download exceeds bound')
                result.write(chunk)
        require(total == size, 'Artifact download size mismatch')
        result.seek(0)
        return result
    except (OSError, http.client.HTTPException) as error:
        result.close()
        raise ReceiptError('Artifact download unavailable: ' + type(error).__name__) from None
    except BaseException:
        result.close()
        raise


def archive_member(stream, name, maximum=4 * 1024 * 1024):
    with zipfile.ZipFile(stream) as archive:
        members = archive.infolist()
        require(0 < len(members) <= MAX_MEMBERS, 'Archive member bound exceeded')
        names, expanded = set(), 0
        for member in members:
            path = PurePosixPath(member.filename)
            require(member.filename not in names and not path.is_absolute()
                    and '..' not in path.parts and '\\' not in member.filename,
                    'Unsafe or duplicate archive member')
            names.add(member.filename)
            expanded += member.file_size
            require(expanded <= MAX_EXPANDED and not member.flag_bits & 1,
                    'Archive expansion/encryption bound exceeded')
        require(name in names, 'Original receipt member unavailable')
        member = archive.getinfo(name)
        require(not member.is_dir() and member.file_size <= maximum,
                'Receipt member exceeds bound')
        return archive.read(member)  # CRC checked; never extract any artifact path.


def required_tests(commit):
    source = git('show', commit + ':scripts/apple-tv-navigation-test')
    tree = ast.parse(source)
    values = [ast.literal_eval(node.value) for node in tree.body
              if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name)
                    and target.id == 'REQUIRED' for target in node.targets)]
    require(len(values) == 1 and isinstance(values[0], list) and values[0]
            and len(values[0]) == len(set(values[0]))
            and all(isinstance(value, str) for value in values[0]), 'Invalid required native suite')
    return values[0]


def verify_tv(receipt, commit):
    required = required_tests(commit)
    require(receipt.get('schema') == 1 and receipt.get('source_sha') == commit
            and receipt.get('source_dirty') is False
            and receipt.get('source_tree_sha256') == full_source_digest(commit),
            'Original native source snapshot mismatch')
    require(receipt.get('required_tests') == receipt.get('selected_tests') == required
            and receipt.get('status') == 'passed' and receipt.get('native_suite_complete') is True
            and receipt.get('xcode') == XCODE and receipt.get('runtime') == RUNTIME,
            'Native suite completeness/toolchain mismatch')
    summary = receipt.get('test_summary', {})
    require(summary.get('passedTests') == len(required) and summary.get('failedTests') == 0
            and summary.get('skippedTests') == 0, 'Incomplete executed native summary')
    command = receipt.get('command', [])
    require(isinstance(command, list) and command[:2] == ['xcodebuild', 'test']
            and [item for item in command if item.startswith('-only-testing:')] ==
            ['-only-testing:plurx-tvOSUITests/' + test for test in required],
            'Native executed selection mismatch')


def verify_desktop(data):
    log = data.decode('utf-8')
    # XCTest summary, not Swift Testing's trailing zero-test compatibility line.
    require("Test Suite 'All tests' passed" in log
            and re.search(r'Executed [1-9][0-9]* tests?, with 0 failures \(0 unexpected\)', log)
            and not re.search(r"Test (?:Suite|Case) .* failed", log),
            'Original desktop native test completion unavailable')


def origin(api, artifact, scope, current, current_run, *, tv_cases=False):
    rid = positive(artifact['run_id'])
    require(rid < current_run and artifact.get('expired') is False, 'Expired/current/future origin')
    run = api.get(f'/actions/runs/{rid}')
    require(run['id'] == rid and run['repository']['id'] == scope['repository'], 'Foreign origin run')
    event = bounded_json(run['event_payload'].encode())
    old_scope = {'repository': scope['repository'], 'pr': positive(event['number'])}
    bind_event(run, old_scope)
    commit = run['commit_sha']
    require(commit_available(commit), 'Origin source unavailable')
    require(subprocess.run(['git', 'merge-base', '--is-ancestor', commit, current],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode == 0,
            'Origin source is not an ancestor')
    if old_scope['pr'] != scope['pr']:
        pull = api.get(f"/pulls/{old_scope['pr']}")
        require(pull['merged'] is True and pull['state'] == 'closed'
                and pull['head']['sha'] == commit
                and pull['head']['repo']['id'] == pull['base']['repo']['id'] == scope['repository']
                and pull['base']['ref'] == 'main', 'Cross-PR origin was not merged from this source')
        merged = pull['merge_commit_sha']
        require(commit_available(merged)
                and subprocess.run(['git', 'merge-base', '--is-ancestor', merged, current],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode == 0,
                'Origin merge is not an ancestor')
    producer(commit, current)
    jobs = api.pages(f'/actions/runs/{rid}/jobs')
    jobs = [job for job in jobs if job['name'] == JOB]
    require(len(jobs) == 1, 'Ambiguous original native job')
    job = jobs[0]
    require(job['run_id'] == rid and job['repo_id'] == scope['repository']
            and job['attempt'] == 1 and (terminal_status(job) == 'success'
                or (tv_cases and artifact['name'].startswith('tv-navigation-')
                    and terminal_status(job) == 'failure'))
            and job['runs_on'] == RUNS_ON, 'Original native job did not pass its environment contract')
    name = artifact['name']
    require(name in (f'tv-navigation-{rid}-1', f'desktop-macos-check-{rid}-1'), 'Misnamed original artifact')
    log = api.bytes(f"/actions/jobs/{positive(job['id'])}/logs").decode()
    require(commit + f":refs/remotes/pull/{old_scope['pr']}/head" in log
            and log.count(f'Artifact {name} has been successfully uploaded!') == 1
            and 'Xcode 27.1' in log and 'Build version 27A9275' in log,
            'Original checkout/toolchain/publication proof unavailable')
    return {'repository': scope['repository'], 'pr': old_scope['pr'], 'run': rid,
            'job': job['id'], 'attempt': 1, 'commit': commit, 'artifact': artifact['id'],
            'artifact_name': name, 'environment_contract': {'runs_on': RUNS_ON, 'xcode': XCODE}}


def native_artifacts(api, current_run):
    """Bound discovery work; incomplete history never discards found evidence.

    API ordering can affect which positives are found, but cannot authenticate
    one: adopt independently checks every candidate's original run and source.
    """
    candidates, seen = [], set()
    for page in range(1, MAX_ARTIFACT_PAGES + 1):
        try:
            batch = api.get('/actions/artifacts', {'page': page, 'limit': 50})
            require(isinstance(batch, list) and len(batch) <= 50, 'Invalid artifact pagination')
        except IntegrityError:
            raise
        except UNADOPTABLE_NATIVE as error:
            print(f'Native history page unavailable — {reason(error)}; retaining found candidates')
            break
        for artifact in batch:
            if not isinstance(artifact, dict):
                continue
            name, rid, ident = artifact.get('name'), artifact.get('run_id'), artifact.get('id')
            if (isinstance(name, str) and re.fullmatch(
                    r'(?:tv-navigation|desktop-macos-check)-[1-9][0-9]*-1', name)
                    and type(rid) is int and 0 < rid < current_run
                    and type(ident) is int and ident > 0 and ident not in seen
                    and artifact.get('expired') is False):
                candidates.append(artifact)
                seen.add(ident)
                if len(candidates) == MAX_CANDIDATES:
                    return candidates
        if len(batch) < 50:
            break
    return candidates


def adopt(api, scope, current, run_id, artifacts):
    adopted = {}
    candidates = [item for item in artifacts if re.fullmatch(
        r'(?:tv-navigation|desktop-macos-check)-[1-9][0-9]*-1', str(item.get('name', '')))]
    candidates.sort(key=lambda item: item.get('run_id', 0), reverse=True)
    for artifact in candidates[:MAX_CANDIDATES]:
        family = 'tv_navigation' if artifact['name'].startswith('tv-navigation-') else 'desktop_macos'
        if family in adopted:
            continue
        try:
            record = origin(api, artifact, scope, current, run_id)
            require(unchanged(record['commit'], current, family), 'Native input closure changed')
            with download(api, artifact) as stream:
                data = archive_member(stream, 'receipt.json' if family == 'tv_navigation' else 'desktop-macos-check.log')
            if family == 'tv_navigation':
                verify_tv(bounded_json(data), record['commit'])
                record['environment_contract']['runtime'] = RUNTIME
            else:
                verify_desktop(data)
            # A TV original artifact requires apple scope, whose reviewed job
            # runs apple-build before the journeys. A desktop original XCTest
            # log requires desktop-macos-check, including both native slices.
            # Adopted jobs emit no TV original artifact and no desktop XCTest
            # summary, so adoption records cannot become original evidence.
            operations = (['make apple-build', 'make apple-tv-navigation-test']
                          if family == 'tv_navigation' else ['make desktop-macos-check'])
            adopted[family] = {**record, 'disposition': 'adopted', 'operations': operations,
                'original_member_sha256': hashlib.sha256(data).hexdigest(),
                'inputs': INPUTS[family],
                'input_digest': input_digest(record['commit'], {'inputs': INPUTS[family]})}
            print(f"Adopted {family}: original run {record['run']} job {record['job']} source {record['commit']}")
        except IntegrityError:
            raise
        except UNADOPTABLE_NATIVE as error:
            print(f"Native artifact {artifact.get('id')}: not adopted — {reason(error)}; builds and tests remain due")
    return adopted


# Exact reviewed fixture transitions preserve earlier successful assertions.
# Nothing else in the input closure may change. Failed journeys are excluded
# even where their sibling methods' full files have an approved transition.
FIXTURE_TRANSITIONS = {
    'clients/apple/UITests/TVNavigationTestCase.swift': (
        '5d25989409706e2e7b82c3c563f44313b2a020deddbad98f3d2def80fd3106d8',
        '87ebaaf6158e6114cdb939ba66c00fa05303a336c4f86e71afd50a2174a3f21e'),
    'clients/apple/UITests/TVLiveNavigationTests.swift': (
        '464293ad77eaabb18cd3e53eddfb0e99a44b8e8da20b07bfe1a83e209f373873',
        'ee78be532ccdd614ec4acd108b4191345b817f065d332de6ffa68cd768956846'),
}
TRANSITION_INVALIDATES = {
    'TVLiveNavigationTests/testLiveFullscreenRevealMoreReportAndBackRetainSession',
    'TVMeridianNavigationTests/testPanelOwnsNativeFocusAndBackPreservesLibraryRoute',
}
# Reviewed schema-2 runner; changes require another explicit producer review.
CASE_RUNNER_SHA256 = '8c9f0687923af53d97608cf90ced64c26d75abce3dd4380cef403aacf0269c68'


def case_input_compatibility(commit, current):
    """Compare every closure entry, allowing only exact reviewed byte pairs."""
    files = []
    for source in (commit, current):
        regular_inputs(source, 'tv_navigation')
        tree = {}
        for row in git('ls-tree', '-r', '-z', source).split('\0'):
            if row:
                header, path = row.split('\t', 1)
                if any(matches_input(path, pattern) for pattern in INPUTS['tv_navigation']):
                    tree[path] = tuple(header.split())
        files.append(tree)
    old, now = files
    require(old.keys() == now.keys(), 'Native input names changed')
    changed_fixture = False
    allowed = {**FIXTURE_TRANSITIONS, 'scripts/apple-tv-navigation-test': (
        BUILD_SPECS['scripts/apple-tv-navigation-test'], CASE_RUNNER_SHA256)}
    for path in old:
        if old[path] == now[path]:
            continue
        require(old[path][:2] == now[path][:2] and path in allowed,
                'Native input changed outside reviewed transitions: ' + path)
        pair = tuple(hashlib.sha256(subprocess.check_output(
            ['git', 'show', source + ':' + path], timeout=10)).hexdigest()
            for source in (commit, current))
        require(pair == allowed[path], 'Unreviewed native input transition: ' + path)
        changed_fixture |= path in FIXTURE_TRANSITIONS
    # Every build specification retains its independently reviewed boundary.
    for source in (commit, current):
        for path in FAMILY_SPECS['tv_navigation']:
            digest = hashlib.sha256(subprocess.check_output(
                ['git', 'show', source + ':' + path], timeout=10)).hexdigest()
            accepted = (BUILD_SPECS[path], CASE_RUNNER_SHA256) if path == 'scripts/apple-tv-navigation-test' else (BUILD_SPECS[path],)
            require(digest in accepted, 'Unreviewed native build specification: ' + path)
    return TRANSITION_INVALIDATES if changed_fixture else set()


def case_outcomes(log, selected):
    """Strict XCTest events, never a substring search for a passing name."""
    require(isinstance(selected, list) and len(selected) == len(set(selected))
            and all(isinstance(test, str) for test in selected), 'Invalid native selection')
    started, outcomes = set(), {}
    pattern = re.compile(r"^Test Case '-\[plurx_tvOSUITests\.([A-Za-z0-9_]+) ([A-Za-z0-9_]+)\]' "
                         r"(started\.|(passed|failed|skipped) \([0-9]+(?:\.[0-9]+)? seconds\)\.)$")
    for line in log.splitlines():
        if not line.startswith('Test Case '):
            continue
        event = pattern.fullmatch(line)
        require(event is not None, 'Unrecognized native test event')
        test = event[1] + '/' + event[2]
        require(test in selected, 'Unexpected native test identity: ' + test)
        if event[3] == 'started.':
            require(test not in started, 'Repeated native test start: ' + test)
            started.add(test)
        else:
            require(test in started and test not in outcomes, 'Ambiguous native test outcome: ' + test)
            outcomes[test] = event[4]
    return outcomes


def verify_tv_cases(receipt, log, commit):
    required = required_tests(commit)
    selected = receipt.get('selected_tests')
    require(type(receipt.get('schema')) is int and receipt['schema'] in (1, 2)
            and receipt.get('source_sha') == commit
            and receipt.get('source_dirty') is False
            and receipt.get('source_tree_sha256') == full_source_digest(commit)
            and receipt.get('required_tests') == required
            and isinstance(selected, list) and selected and len(selected) == len(set(selected))
            and all(test in required for test in selected)
            and receipt.get('status') in ('passed', 'failed')
            and receipt.get('xcode') == XCODE and receipt.get('runtime') == RUNTIME,
            'Native case source/selection/toolchain mismatch')
    command = receipt.get('command', [])
    require(isinstance(command, list) and command[:2] == ['xcodebuild', 'test']
            and all(isinstance(item, str) for item in command)
            and [item for item in command if item.startswith('-only-testing:')] ==
            ['-only-testing:plurx-tvOSUITests/' + test for test in selected],
            'Native case command selection mismatch')
    if receipt['schema'] == 1:
        # This reviewed producer's failed branch lacks an end-snapshot field.
        # Its initial full Git snapshot, immutable CI checkout, pinned script,
        # original job and archived raw output are the legacy trust boundary.
        digest = hashlib.sha256(subprocess.check_output(
            ['git', 'show', commit + ':scripts/apple-tv-navigation-test'], timeout=10)).hexdigest()
        require(digest == BUILD_SPECS['scripts/apple-tv-navigation-test'] and selected == required,
                'Unsupported legacy native case producer')
    else:
        require(receipt.get('source_unchanged') is True
                and receipt.get('finished_source') == {
                    'sha': commit, 'dirty': False, 'tree_sha256': receipt['source_tree_sha256']},
                'Native case end-source proof unavailable')
    outcomes = case_outcomes(log, selected)
    summary = receipt.get('test_summary', {})
    require(all(type(summary.get(field)) is int and summary[field] == list(outcomes.values()).count(value)
                for field, value in (('passedTests', 'passed'), ('failedTests', 'failed'), ('skippedTests', 'skipped'))),
            'Native named outcomes and summary disagree')
    if receipt['schema'] == 2:
        require(receipt.get('case_results') == outcomes, 'Native case inventory differs from original log')
    if receipt['status'] == 'passed':
        require(set(outcomes) == set(selected) and set(outcomes.values()) == {'passed'},
                'Native success omitted selected cases')
    return {test for test, outcome in outcomes.items() if outcome == 'passed'}


def adopt_tv_cases(api, scope, current, run_id, artifacts):
    passes = {}
    for artifact in sorted(artifacts, key=lambda item: item.get('run_id', 0), reverse=True)[:MAX_CANDIDATES]:
        if not re.fullmatch(r'tv-navigation-[1-9][0-9]*-1', str(artifact.get('name', ''))):
            continue
        try:
            record = origin(api, artifact, scope, current, run_id, tv_cases=True)
            invalidated = case_input_compatibility(record['commit'], current)
            with download(api, artifact) as stream:
                data = archive_member(stream, 'receipt.json')
                stream.seek(0)
                raw_log = archive_member(stream, 'xcodebuild.log', 32 * 1024 * 1024)
            tests = verify_tv_cases(bounded_json(data), raw_log.decode('utf-8'), record['commit'])
            evidence = {**record, 'environment_contract': {**record['environment_contract'], 'runtime': RUNTIME},
                        'receipt_sha256': hashlib.sha256(data).hexdigest(),
                        'log_sha256': hashlib.sha256(raw_log).hexdigest(),
                        'input_digest': input_digest(record['commit'], {'inputs': INPUTS['tv_navigation']}),
                        'current_input_digest': input_digest(current, {'inputs': INPUTS['tv_navigation']})}
            for test in sorted(tests - invalidated):
                passes.setdefault(test, {**evidence, 'test': test})
        except IntegrityError:
            raise
        except UNADOPTABLE_NATIVE as error:
            print(f"Native cases from artifact {artifact.get('id')}: not retained — {reason(error)}")
    required = required_tests(current)
    passes = {test: source for test, source in passes.items() if test in required}
    return {'required_tests': required, 'passes': passes,
            'pending': [test for test in required if test not in passes]}



def planned_artifacts(api, cases):
    """Resolve only the immutable origins already selected by this plan.

    Repository-wide discovery is intentionally absent: unrelated uploads during
    compilation must not evict or supersede this plan's authenticated evidence.
    Every resolved row still goes through the full original-case verifier.
    """
    require(isinstance(cases, dict) and isinstance(cases.get('passes'), dict),
            'Native plan pass map malformed')
    passes = cases['passes']
    require(len(passes) <= len(cases.get('required_tests', []))
            and all(isinstance(source, dict) for source in passes.values()),
            'Native plan original records malformed')
    identifiers = sorted({positive(source.get('artifact')) for source in passes.values()})
    require(len(identifiers) <= MAX_CANDIDATES, 'Native plan origin bound exceeded')
    artifacts = []
    for ident in identifiers:
        artifact = api.get(f'/actions/artifacts/{ident}')
        require(isinstance(artifact, dict) and type(artifact.get('id')) is int
                and artifact['id'] == ident, 'Native plan artifact identity mismatch')
        artifacts.append(artifact)
    return artifacts


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--verify-tv-plan', action='store_true',
                        help='Reauthenticate every original case immediately before native execution')
    options = parser.parse_args(argv)
    api, scope, current, run_id = api_context()
    if options.verify_tv_plan:
        safe_storage()
        plan = bounded_json(REPORT.read_bytes())
        require(plan.get('scope') == scope and plan.get('current_commit') == current
                and plan.get('current_run') == run_id, 'Native plan current CI identity mismatch')
        actual = adopt_tv_cases(api, scope, current, run_id, planned_artifacts(api, plan.get('tv_cases')))
        require(plan.get('tv_cases') == actual, 'Native plan lacks reauthenticated original case evidence')
        print(f"Reauthenticated native plan: {len(actual['passes'])} retained; {len(actual['pending'])} due")
        return
    # api_context authenticates live same-repository PR/head/base/readiness and
    # rejects stale worktrees, reruns, and forged local event metadata.
    try:
        artifacts = native_artifacts(api, run_id)
        adopted = adopt(api, scope, current, run_id, artifacts)
        tv_cases = adopt_tv_cases(api, scope, current, run_id, artifacts)
    except IntegrityError:
        raise
    except UNADOPTABLE_NATIVE as error:
        print(f'Native history unavailable — {reason(error)}; builds and tests remain due')
        adopted = {}
        required = required_tests(current)
        tv_cases = {'required_tests': required, 'passes': {}, 'pending': required}
    report = {'version': 1, 'scope': scope, 'current_commit': current,
              'current_run': run_id, 'adoptions': adopted, 'tv_cases': tv_cases,
              'compatibility': 'Pinned workflow toolchain contract; schema 1 has no host OS fingerprint'}
    safe_storage()
    atomic_json(REPORT, report)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        for family in INPUTS:
            output.write(f'{family}={str(family in adopted).lower()}\n')


if __name__ == '__main__':
    main()
