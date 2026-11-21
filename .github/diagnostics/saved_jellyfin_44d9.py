#!/usr/bin/env python3
"""One archived failed case, original source, diagnostic evidence only; never compile."""
import hashlib
import importlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request
import zipfile

HEAD = '44d9e4c0b78eb347070c1ce851af5ca6218c127f'
TREE = '22f37804bcf90a86149339584916baa17a9bdea2'
RUN, ARCHIVE_JOB, ARTIFACT = 8803, 84133, 12692
ARTIFACT_NAME = 'rust-units-archive-8803'
ZIP_BYTES = 455576292
PLAN_SHA = 'dde855732d19435a1a885cb145dc91fe20150d04247537495d68480ed1760626'
IMAGE = 'sha256:6a989b4d92f55b29befa9eae08f9cd59940b3067d771629549ebf5ab1f4d433d'
CASE = 'http::jellyfin::tests::jellyfin_native_hls_encoded_without_copy_index_preserves_ranges_clock_and_stop'
BINARY = 'plurxd::bin/plurxd'
FILTER = f'binary_id(={BINARY}) & test(={CASE})'
JOB_NAME = 'Saved Jellyfin case (44d9)'
RUNNERS = {
    '4ec02b95-6f4a-4944-a2b4-a1d8728d6be5': (60, 'gha-lab1-plurx-heavy-01'),
    'c9ace9ca-97ce-4746-a467-37f9c275e92b': (63, 'gha-lab2-plurx-heavy-01'),
}
PIN_SOURCE = 'Root fresh authenticated registry review supplied on2026-11-21; no runtime admin API dependency'
KEYS = {
    'plurx-compat-plex': '0fd85031f4bbe16f8ec43e812450b6539b1efe7d397d71e7d996c9ed0fab6461',
    'plurx-core': '52fe854528234b61641fe8f4137b106693c52550fc4389c6fdf6ce282296349f',
    'plurx-support': '9cfb2219fc77d61fd8c263f13c6d472a75abca2baa2109c5987c8412c65dc550',
    'plurxd': '1620b0ebbc71483cc6c79dc30e4a4d12e758aad518e8d105cd00005baf88cca0',
}
SOURCE = Path(os.environ['GITHUB_WORKSPACE']).resolve()
WORK = Path(os.environ['SAVED_CASE_ROOT']).resolve()
EVIDENCE = WORK / 'evidence'
RUNTIME = WORK / 'runtime'
MAX_LOG = 16 * 1024**2


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def record(name, value):
    path = EVIDENCE / name
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def digest(path, algorithm='sha256'):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def command(args, *, env=None, limit=120):
    result = subprocess.run(args, cwd=SOURCE, env=env, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=limit)
    require(result.returncode == 0, f'command failed: {args[0]} (exit {result.returncode})')
    return result.stdout


class NoCredentialRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        parsed = urllib.parse.urlsplit(newurl)
        require(parsed.scheme in ('http', 'https') and parsed.hostname and
                not parsed.username and not parsed.password, 'unsafe artifact redirect')
        # Never copy Authorization (including for same-origin redirects).
        return urllib.request.Request(newurl)


class Api:
    def __init__(self):
        root = os.environ['GITHUB_API_URL'].rstrip('/')
        parsed = urllib.parse.urlsplit(root)
        repo = os.environ['GITHUB_REPOSITORY']
        require(parsed.scheme in ('http', 'https') and parsed.hostname and
                not parsed.username and not parsed.password and not parsed.query and
                not parsed.fragment, 'invalid API origin')
        require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo), 'invalid repository')
        self.root = root + '/repos/' + repo
        self.token = os.environ.pop('GITHUB_TOKEN')
        require(bool(self.token), 'missing ephemeral repository token')
        self.opener = urllib.request.build_opener(NoCredentialRedirect())
        self.calls = 0

    def open(self, path):
        require(path.startswith('/') and '..' not in path, 'unsafe API path')
        self.calls += 1
        require(self.calls <= 24, 'API request budget exhausted')
        request = urllib.request.Request(self.root + path,
                                        headers={'Authorization': 'token ' + self.token})
        return self.opener.open(request, timeout=30)

    def get(self, path):
        with self.open(path) as response:
            payload = response.read(4 * 1024**2 + 1)
        require(len(payload) <= 4 * 1024**2, 'API response exceeded bound')
        return json.loads(payload)

    def header(self, job):
        with self.open(f'/actions/jobs/{job}/logs') as response:
            return response.read(8192).decode('utf-8', 'replace')


def placement(api):
    run = int(os.environ['GITHUB_RUN_ID'])
    require(run != RUN, 'diagnostic run must be separate from original qualification')
    jobs = api.get(f'/actions/runs/{run}/jobs')
    require(isinstance(jobs, list), 'unexpected job metadata')
    found = [job for job in jobs if job.get('name') == JOB_NAME]
    require(len(found) == 1, 'current diagnostic job identity is ambiguous')
    job = found[0]
    header = api.header(job['id'])
    require(IMAGE in header, 'authenticated job header does not name the pinned image')
    match = re.search(r'Runner ([0-9a-f-]{36}) .*?received task ([0-9]+) of job ', header)
    require(match and int(match[2]) == job.get('task_id'), 'task/log identity mismatch')
    expected = RUNNERS.get(match[1])
    require(expected is not None, 'runner UUID is not an approved managed nonvoter; no tests started')
    reported = os.environ.get('DIAGNOSTIC_RUNNER_NAME')
    require(reported == expected[1], 'context runner name differs from pinned UUID/name')
    labels = job.get('runs_on')
    if isinstance(labels, str):
        labels = json.loads(labels)
    require(isinstance(labels, list) and 'ci-nonvoter-compile-amd64' in labels,
            'authenticated job configuration lacks nonvoter label')
    record('placement.json', {'run': run, 'job': job['id'], 'task': job['task_id'],
           'runner_id': expected[0], 'runner_name': expected[1],
           'runner_uuid': match[1], 'configured_labels': labels, 'image': IMAGE,
           'image_evidence': 'authenticated job start header', 'pin_source': PIN_SOURCE,
           'kernel': platform.release(), 'machine': platform.machine()})


def resource_bounds():
    require(platform.system() == 'Linux' and platform.machine() == 'x86_64', 'Linux x86_64 required')
    roots = [Path('/sys/fs/cgroup')]
    for line in Path('/proc/self/cgroup').read_text().splitlines():
        hierarchy, controllers, relative = line.split(':', 2)
        if hierarchy == '0':
            require(relative.startswith('/') and '..' not in Path(relative).parts, 'unsafe cgroup path')
            roots.append(Path('/sys/fs/cgroup') / relative.lstrip('/'))
    root = next((path for path in roots if (path / 'memory.max').is_file()), None)
    require(root is not None, 'effective cgroup v2 bounds unavailable')
    values = {name: (root / name).read_text().strip()
              for name in ['memory.max', 'memory.swap.max', 'cpu.max', 'pids.max']}
    require(values['memory.max'] != 'max' and 0 < int(values['memory.max']) <= 7 * 1024**3,
            'memory cap exceeds shard profile')
    require(values['memory.swap.max'] != 'max' and int(values['memory.swap.max']) <= 7 * 1024**3,
            'swap cap exceeds shard profile')
    require(values['pids.max'] != 'max' and 0 < int(values['pids.max']) <= 4096, 'PID cap missing')
    affinity = sorted(os.sched_getaffinity(0))
    os.sched_setaffinity(0, affinity[:3])
    quota, period = values['cpu.max'].split()
    require(quota == 'max' or int(quota) / int(period) <= 3, 'CPU quota exceeds shard profile')
    free = shutil.disk_usage(WORK).free
    require(free >= 29 * 1024**3 // 2, 'less than original14.5GiB disk reserve')
    record('resources.json', {'cgroup': str(root), 'limits': values,
           'affinity_before': affinity, 'affinity_after': sorted(os.sched_getaffinity(0)),
           'free_disk_bytes': free})


def source_identity():
    require(command(['git', 'rev-parse', 'HEAD']).decode().strip() == HEAD, 'source HEAD is not original44d9')
    require(command(['git', 'rev-parse', 'HEAD^{tree}']).decode().strip() == TREE, 'source tree mismatch')
    require(not command(['git', 'status', '--porcelain', '--untracked-files=normal']).strip(), 'source checkout is dirty')
    sys.path.insert(0, str(SOURCE))
    units = importlib.import_module('validation.rust_units')
    tree = units.Tree(HEAD, str(SOURCE))
    keys = units.Keys(tree, units.Config.load((SOURCE / 'validation/rust-unit-inputs.toml').read_text()))
    require({name: keys.key(name) for name in KEYS} == KEYS, 'source crate content keys differ from archive origin')
    inputs = sorted(set().union(*(keys.inputs(name) for name in KEYS)))
    aggregate = hashlib.sha256()
    for relative in inputs:
        mode, oid = tree.entries[relative]
        path = SOURCE / relative
        if mode == '120000':
            data = os.readlink(path).encode()
            actual = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        else:
            info = path.stat(follow_symlinks=False)
            require(stat.S_ISREG(info.st_mode), 'source input is not a regular file')
            hasher = hashlib.sha1(b'blob ' + str(info.st_size).encode() + b'\0')
            with path.open('rb') as stream:
                for chunk in iter(lambda: stream.read(1024**2), b''):
                    hasher.update(chunk)
            actual = hasher.hexdigest()
        require(actual == oid, f'source input bytes changed: {relative}')
        aggregate.update(f'{mode} {oid} {relative}\n'.encode())
    record('source.json', {'head': HEAD, 'tree': TREE, 'crate_keys': KEYS,
           'verified_input_files': len(inputs), 'verified_input_vector_sha256': aggregate.hexdigest(),
           'workflow_head': os.environ.get('GITHUB_SHA'), 'diagnostic_only': True})
    return units


def fetch_archive(api):
    run = api.get(f'/actions/runs/{RUN}')
    require(run.get('id') == RUN and run.get('commit_sha') == HEAD, 'archive run/source mismatch')
    jobs = api.get(f'/actions/runs/{RUN}/jobs')
    archive_job = [job for job in jobs if job.get('id') == ARCHIVE_JOB]
    require(len(archive_job) == 1 and archive_job[0].get('name') == 'Rust units: build archive'
            and archive_job[0].get('status') == 'success', 'archive publisher authentication failed')
    artifacts = api.get(f'/actions/runs/{RUN}/artifacts?page=1&limit=100')
    rows = artifacts if isinstance(artifacts, list) else artifacts.get('artifacts')
    require(isinstance(rows, list), 'unexpected archive metadata')
    found = [entry for entry in rows if entry.get('id') == ARTIFACT]
    require(len(found) == 1, 'retained archive artifact absent or duplicated')
    artifact = found[0]
    require(artifact.get('name') == ARTIFACT_NAME and artifact.get('run_id') == RUN
            and artifact.get('expired') is False and artifact.get('size_in_bytes') == ZIP_BYTES,
            'retained archive identity, expiry or size changed')
    destination = RUNTIME / 'retained.zip'
    size = 0
    hasher = hashlib.sha256()
    deadline = time.monotonic() + 300
    with api.open(f'/actions/artifacts/{ARTIFACT}/zip') as response, destination.open('xb') as output:
        while True:
            require(time.monotonic() < deadline, 'artifact download exceeded300s bound')
            chunk = response.read(1024**2)
            if not chunk:
                break
            size += len(chunk)
            require(size <= ZIP_BYTES, 'artifact download exceeded exact authenticated size')
            hasher.update(chunk)
            output.write(chunk)
    require(size == ZIP_BYTES, 'artifact download truncated')
    declared = artifact.get('digest')
    require(not declared or declared.removeprefix('sha256:') == hasher.hexdigest(), 'artifact digest mismatch')
    paths = {}
    with zipfile.ZipFile(destination) as archive:
        for entry in archive.infolist():
            name = Path(entry.filename)
            require(not name.is_absolute() and '..' not in name.parts and '\\' not in entry.filename,
                    'unsafe ZIP path')
            require(not stat.S_ISLNK(entry.external_attr >> 16), 'ZIP symlink refused')
            if entry.is_dir():
                continue
            require(name.name in ('archive.tar.zst', 'plan.json') and name.name not in paths,
                    'unexpected or duplicate ZIP member')
            require(0 < entry.file_size <= 512 * 1024**2, 'ZIP member exceeds bound')
            target = RUNTIME / name.name
            copied = 0
            with archive.open(entry) as incoming, target.open('xb') as output:
                for chunk in iter(lambda: incoming.read(1024**2), b''):
                    copied += len(chunk)
                    require(copied <= entry.file_size, 'ZIP expansion exceeded declared size')
                    output.write(chunk)
            require(copied == entry.file_size, 'ZIP member truncated')
            paths[name.name] = target
    require(set(paths) == {'archive.tar.zst', 'plan.json'}, 'archive ZIP members missing')
    require(digest(paths['plan.json']) == PLAN_SHA, 'exact sealed archive plan bytes differ')
    plan = json.loads(paths['plan.json'].read_bytes())
    require(plan.get('head') == HEAD and plan.get('run') == RUN and plan.get('schema') == 1,
            'plan/source identity mismatch')
    require({name: plan['crates'][name]['key'] for name in KEYS} == KEYS, 'plan crate keys differ')
    require(f'{BINARY} {CASE}' in plan['crates']['plurxd']['remaining'], 'failed case absent from original plan')
    record('archive.json', {'run': RUN, 'publisher_job': ARCHIVE_JOB, 'artifact': ARTIFACT,
           'name': ARTIFACT_NAME, 'zip_bytes': size, 'zip_sha256': hasher.hexdigest(),
           'server_digest_present': bool(declared), 'plan_sha256': PLAN_SHA,
           'payload_sha256': digest(paths['archive.tar.zst']), 'crate_keys': KEYS})
    api.token = ''
    return paths['archive.tar.zst']


def owned_processes(root):
    pending, seen, rows = [root], set(), []
    while pending and len(seen) < 128:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        base = Path('/proc') / str(pid)
        row = {'pid': pid}
        try:
            text = (base / 'stat').read_text()
            tail = text[text.rfind(')') + 2:].split()
            row.update(comm=text[text.find('(') + 1:text.rfind(')')],
                       state=tail[0], ppid=int(tail[1]), utime=int(tail[11]),
                       stime=int(tail[12]), starttime=int(tail[19]))
            # Discover descendants before optional FD reads, which naturally
            # race close/exit. Never fall back to machine-wide process capture.
            row['task_states'] = []
            for task in list((base / 'task').iterdir())[:256]:
                try:
                    pending.extend(int(value) for value in (task / 'children').read_text().split())
                    row['task_states'].append({'tid': task.name, 'wchan': (task / 'wchan').read_text().strip()})
                except (OSError, ValueError):
                    continue
            row['wchan'] = (base / 'wchan').read_text().strip()
            row['io'] = (base / 'io').read_text()[:2048]
            row['fds'], row['fdinfo'] = {}, {}
            for fd in list((base / 'fd').iterdir())[:64]:
                try:
                    row['fds'][fd.name] = os.readlink(fd)
                except OSError:
                    continue
            for fd in list((base / 'fdinfo').iterdir())[:16]:
                try:
                    row['fdinfo'][fd.name] = fd.read_text()[:512]
                except OSError:
                    continue
        except (OSError, ValueError, IndexError) as error:
            row['read_error'] = type(error).__name__
        rows.append(row)
    return rows


def owned_media_files(scratch):
    # Jellyfin's tempfile owns transcode, runtime and rendition subtrees.
    # Their generated layout may nest generations, so do not assume depth.
    pending, visited, examined, files = [scratch], 0, 0, []
    while pending and visited < 128 and examined < 2048 and len(files) < 256:
        directory = pending.pop()
        visited += 1
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    examined += 1
                    if examined > 2048 or len(files) >= 256:
                        break
                    try:
                        if entry.is_symlink():
                            continue
                        if entry.is_dir(follow_symlinks=False):
                            pending.append(Path(entry.path))
                        elif re.fullmatch(r'(?:init\.mp4|seg\d+\.m4s)(?:\..*)?', entry.name):
                            info = entry.stat(follow_symlinks=False)
                            if stat.S_ISREG(info.st_mode):
                                files.append({'path': str(Path(entry.path).relative_to(scratch)),
                                              'bytes': info.st_size, 'inode': info.st_ino,
                                              'mtime_ns': info.st_mtime_ns})
                    except OSError:
                        continue
        except OSError:
            continue
    return files, {'directories': visited, 'entries': examined,
                   'bounded_walk_truncated': bool(pending) or examined >= 2048 or len(files) >= 256}


def observe(process, stopped):
    total = 0
    with (EVIDENCE / 'owned-observations.jsonl').open('x') as output:
        while not stopped.is_set():
            files, walk = owned_media_files(RUNTIME / 'scratch')
            line = json.dumps({'monotonic': time.monotonic(), 'processes': owned_processes(process.pid),
                               'files': files, 'file_walk': walk}, separators=(',', ':')) + '\n'
            total += len(line.encode())
            if total > MAX_LOG:
                output.write('{"observer_truncated":true}\n')
                return
            output.write(line)
            output.flush()
            stopped.wait(0.5)


def clean_environment():
    home = RUNTIME / 'home'
    scratch = RUNTIME / 'scratch'
    home.mkdir()
    scratch.mkdir()
    return {'PATH': os.environ['PATH'], 'HOME': str(home), 'TMPDIR': str(scratch),
            'LANG': 'C.UTF-8', 'RUST_MIN_STACK': '8388608', 'CARGO_TERM_COLOR': 'never',
            'CARGO_HOME': str(home / '.cargo'), 'PLURX_FFMPEG': '/usr/lib/jellyfin-ffmpeg/ffmpeg',
            'PLURX_FFPROBE': '/usr/lib/jellyfin-ffmpeg/ffprobe'}


def main():
    run = str(int(os.environ['GITHUB_RUN_ID']))
    attempt = str(int(os.environ.get('GITHUB_RUN_ATTEMPT', '1')))
    require(WORK.parent == SOURCE.parent and
            WORK.name == f'{SOURCE.name}-saved-jellyfin-{run}-{attempt}', 'unsafe owned sibling path')
    owner = json.loads((WORK / '.owner.json').read_text())
    require(owner == {'run': run, 'attempt': attempt, 'workspace': str(SOURCE)}, 'owned sibling identity mismatch')
    EVIDENCE.mkdir(exist_ok=False)
    RUNTIME.mkdir()
    record('intent.json', {'source_head': HEAD, 'original_run': RUN, 'case': CASE,
           'helper_sha256': digest(Path(__file__)), 'qualification_evidence': False,
           'compilation': False, 'automatic_retries': 0, 'passing_cases_replayed': 0})
    api = Api()
    placement(api)
    resource_bounds()
    units = source_identity()
    # Keep all nextest reports and generated fixture output outside original
    # tracked source while preserving the archive's compiled target path.
    target = SOURCE / 'target'
    require(not target.exists() and not target.is_symlink(), 'existing source target must be preserved')
    (RUNTIME / 'target').mkdir()
    target.symlink_to(RUNTIME / 'target', target_is_directory=True)
    archive = fetch_archive(api)
    env = clean_environment()
    require(command(['cargo-nextest', 'nextest', '--version'], env=env).decode().startswith('cargo-nextest 0.9.151 '),
            'nextest version differs')
    extract = RUNTIME / 'extract'
    common = ['--archive-file', str(archive), '--workspace-remap', str(SOURCE),
              '--extract-to', str(extract), '--extract-overwrite', '--profile', 'ci', '-E', FILTER]
    listing_path = EVIDENCE / 'listing.json'
    listing_path.write_bytes(command(['cargo-nextest', 'nextest', 'list', *common,
                                     '--message-format', 'json'], env=env, limit=300))
    listing = json.loads(listing_path.read_bytes())
    selected = []
    for binary_id, suite in listing.get('rust-suites', {}).items():
        for name, case in suite.get('testcases', {}).items():
            match = case.get('filter-match', {})
            if match.get('status') == 'matches':
                require(not case.get('ignored'), 'selected test unexpectedly ignored')
                selected.append((binary_id, name, suite))
    require(len(selected) == 1 and selected[0][:2] == (BINARY, CASE), 'archive filter did not select exactly original failed case')
    binary = Path(selected[0][2]['binary-path']).resolve()
    require(binary.is_relative_to(extract.resolve()) and binary.is_file(), 'binary path escaped owned archive extraction')
    before = digest(binary)
    junit = SOURCE / units.JUNIT_REPORT
    require(not junit.exists(), 'prior JUnit report present; refuse to adopt stale output')
    args = ['cargo-nextest', 'nextest', 'run', *common, '--test-threads', '1', '--retries', '0',
            '--no-fail-fast', '--no-tests=fail', '--color', 'never',
            '--success-output', 'immediate', '--failure-output', 'immediate']
    record('runtime-intent.json', {'argv': args, 'binary_sha256': before,
           'source_head': HEAD, 'qualification_evidence': False, 'test_environment_keys': sorted(env)})
    start = time.monotonic()
    stopped = threading.Event()
    with subprocess.Popen(args, cwd=SOURCE, env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT) as process:
        observer = threading.Thread(target=observe, args=(process, stopped), daemon=True)
        observer.start()
        written, truncated = 0, False
        with (EVIDENCE / 'case.log').open('xb') as output:
            while True:
                chunk = process.stdout.read(65536)
                if not chunk:
                    break
                if written + len(chunk) <= MAX_LOG:
                    output.write(chunk)
                    written += len(chunk)
                else:
                    truncated = True
        code = process.wait()
        stopped.set()
        observer.join(timeout=5)
    require(not observer.is_alive(), 'read-only observer did not settle')
    require(digest(binary) == before, 'extracted executable changed during execution')
    require(junit.is_file(), 'fresh original-profile JUnit report missing')
    outcomes = units.junit_outcomes(junit)
    require(set(outcomes) == {f'{BINARY} {CASE}'}, 'unexpected, missing or additional executed test')
    units.verify_exit(code, outcomes)
    shutil.copyfile(junit, EVIDENCE / 'case.junit.xml')
    record('result.json', {'source_head': HEAD, 'binary_sha256': before, 'exit': code,
           'duration_seconds': time.monotonic() - start, 'outcomes': outcomes,
           'log_truncated': truncated, 'qualification_evidence': False})
    require(not truncated, 'case log exceeded evidence bound')
    print(json.dumps({'diagnostic_only': True, 'case': CASE, 'exit': code, 'outcomes': outcomes}))
    return code


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as error:
        if EVIDENCE.is_dir():
            record('harness-failure.json', {'kind': type(error).__name__, 'reason': str(error)[:600],
                   'qualification_evidence': False})
        # Never print redirect URLs, API exception details or token-bearing requests.
        print('saved-case diagnostic stopped: ' + type(error).__name__, file=sys.stderr)
        raise SystemExit(2)
