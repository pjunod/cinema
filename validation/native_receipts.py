"""Authenticate original native passes and retain them for unchanged inputs.

This emits adoption evidence, never a replacement native pass. The current
Apple job still verifies its pinned tools. A successful original job and its
reviewed producer bind both build and test operations to the unchanged inputs.
Historical failures or incomplete proof leave both builds and tests due.
The schema-1 producer did not record a macOS build fingerprint: compatibility
here is the original workflow's pinned Xcode/architecture/runtime contract,
not a claim that the host OS or physical-device acceptance was reproduced.
"""
from __future__ import annotations

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
    'clients/apple/project.yml': 'e9e1e028125da79afc8c63f583aa5a24cfff51ae00d365777585f4c44b2708a3',
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
    require(hashlib.sha256(old.encode()).hexdigest() == LEGACY_JOB_SHA256 or old == now,
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


def archive_member(stream, name):
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
        require(not member.is_dir() and member.file_size <= 4 * 1024 * 1024,
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


def origin(api, artifact, scope, current, current_run):
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
            and job['attempt'] == 1 and terminal_status(job) == 'success'
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


def main():
    api, scope, current, run_id = api_context()
    # api_context authenticates live same-repository PR/head/base/readiness and
    # rejects stale worktrees, reruns, and forged local event metadata.
    try:
        artifacts = native_artifacts(api, run_id)
        adopted = adopt(api, scope, current, run_id, artifacts)
    except IntegrityError:
        raise
    except UNADOPTABLE_NATIVE as error:
        print(f'Native history unavailable — {reason(error)}; builds and tests remain due')
        adopted = {}
    report = {'version': 1, 'scope': scope, 'current_commit': current,
              'current_run': run_id, 'adoptions': adopted,
              'compatibility': 'Pinned workflow toolchain contract; schema 1 has no host OS fingerprint'}
    safe_storage()
    atomic_json(REPORT, report)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        for family in INPUTS:
            output.write(f'{family}={str(family in adopted).lower()}\n')


if __name__ == '__main__':
    main()
