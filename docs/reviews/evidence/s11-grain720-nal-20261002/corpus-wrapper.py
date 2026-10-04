#!/usr/bin/env python3
"""Private offline byte oracle wrapper. Default/admit mode never creates Docker resources.

Execution needs a separately approved immutable admission path and full hash.
No encoder, decoder, ffprobe, network, shipping code or old test is invoked.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

SOURCE = '/private/tmp/codex-s11-census-c9d10b5d892e45bbb004efd5/export/census'
PARSER = '/private/tmp/s11-retained-ts-h264-nal-parser-20261002.py'
PARSER_SHA = '56825d98839fa31757112bbc7ebeba731e8266cf3bc8ea93ec15cb45af689c80'
RECEIPT_SHA = '6039c85c357b7a6d2129b69677e8f5881a8489d214cef71b455f17bb2c88a18f'
IMAGE = 'sha256:b7bc6f794e9d6aebae8fd54c32c513c3507173729e40c1b5048097e09e23d303'
IMAGE_NONCE = '156983f6a36e4bf8b22821401d432d1e'
IMAGE_EXPIRY = 1791002576.098867
UID = 501
INPUT_CAP = 512 * 1024 * 1024
RESULT_CAP = 1024 * 1024
WORKER_CAP = 950000
CLI_CAP = 32768
CLI_DEADLINE = None


class CommandFailure(RuntimeError):
    def __init__(self, reason, code, out, err):
        super().__init__(reason)
        self.code, self.out, self.err = code, out, err


def need(value, reason):
    if not value:
        raise ValueError(reason)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def script_hash():
    return digest(Path(__file__).read_bytes())


def oracle(path=PARSER):
    need(digest(Path(path).read_bytes()) == PARSER_SHA, 'parser hash')
    spec = importlib.util.spec_from_file_location('private_s11_oracle', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def tree_bytes(root, cap=RESULT_CAP):
    total, nodes = 0, 0
    for parent, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            nodes += 1
            need(nodes <= 256, 'bounded result walk nodes')
            entry = Path(parent) / name
            value = entry.lstat()
            need(not stat.S_ISLNK(value.st_mode), 'result symlink')
            if stat.S_ISREG(value.st_mode):
                total += value.st_size
                need(total <= cap, 'aggregate result byte cap')
    return total


def held_generated_result(path):
    """Hash a generated/exported result, rejecting symlink/swap/truncation."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(fd)
        need(stat.S_ISREG(before.st_mode) and before.st_size <= 900000, 'generated result regular/byte cap')
        chunks, count = [], 0
        while True:
            chunk = os.read(fd, min(65536, 900001 - count))
            if not chunk:
                break
            count += len(chunk)
            need(count <= 900000, 'generated result read cap')
            chunks.append(chunk)
        after = os.fstat(fd)
        named = os.stat(path, follow_symlinks=False)
        identity = lambda value: (value.st_dev, value.st_ino, value.st_size,
                                  value.st_mtime_ns, value.st_ctime_ns, value.st_mode)
        need(identity(before) == identity(after) == identity(named) and count == before.st_size,
             'changed held generated result')
        data = b''.join(chunks)
        return data, {'bytes': count, 'sha256': digest(data), 'dev': before.st_dev,
                      'inode': before.st_ino, 'uid': before.st_uid, 'mode': oct(before.st_mode & 0o777)}
    finally:
        os.close(fd)


def write_once(path, value, cap=RESULT_CAP):
    data = json.dumps(value, sort_keys=True, indent=2).encode()
    need(len(data) <= cap, 'JSON result cap')
    # Host receipts reserve the worker's whole950000B allowance in advance.
    parent = Path(path).parent
    if parent.name.startswith('s11-nal-corpus-'):
        existing = sum(p.stat().st_size for p in parent.iterdir() if p.is_file())
        need(existing + len(data) <= RESULT_CAP - WORKER_CAP - 8192,
             'outer metadata aggregate reserve')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def bounded_command(args, seconds=10, cap=CLI_CAP, pass_fds=(), on_start=None):
    """Bound the CLI itself, including aggregate stdout+stderr, then reap it."""
    begin = time.monotonic()
    process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True,
                               pass_fds=pass_fds)
    selector = selectors.DefaultSelector()
    buffers = [bytearray(), bytearray()]
    for index, pipe in enumerate((process.stdout, process.stderr)):
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ, index)
    error = None
    try:
        if on_start:
            on_start(process.pid)
        while selector.get_map() or process.poll() is None:
            if time.monotonic() - begin >= seconds:
                error = 'bounded CLI timeout'
                break
            for key, unused in selector.select(0.05):
                chunk = os.read(key.fileobj.fileno(), 4096)
                if not chunk:
                    selector.unregister(key.fileobj)
                else:
                    remaining = cap - sum(map(len, buffers))
                    buffers[key.data].extend(chunk[:remaining])
                    if len(chunk) > remaining:
                        error = 'bounded CLI aggregate output'
                        break
            if error:
                break
    except BaseException as failure:
        error = 'CLI observer exception: ' + repr(failure)
    finally:
        if error or process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        code = process.wait(timeout=2)
        selector.close()
        process.stdout.close()
        process.stderr.close()
    if error:
        raise CommandFailure(error, code, bytes(buffers[0]), bytes(buffers[1]))
    return code, bytes(buffers[0]), bytes(buffers[1])


def docker(args, seconds=10, allow_fail=False):
    if CLI_DEADLINE is not None:
        remaining = CLI_DEADLINE - time.monotonic()
        need(remaining > 0, 'aggregate cleanup60s deadline')
        seconds = min(seconds, remaining)
    code, out, err = bounded_command(['docker', *args], seconds)
    need(allow_fail or code == 0, 'Docker CLI failed: ' + err[:1024].decode(errors='replace'))
    return code, out, err


def admission_facts():
    need(os.getuid() == UID, 'host UID501')
    need(time.time() + 300 < IMAGE_EXPIRY, 'unchanged runtime expiry remaining')
    code, out, err = docker(['ps', '-q'])
    need(not out.strip(), 'Docker not idle')
    fields = '{{json .Id}}|{{json .Architecture}}|{{json .Config.Labels}}'
    image_raw = docker(['image', 'inspect', '--format', fields, IMAGE])[1].decode().strip()
    image, arch, labels = map(json.loads, image_raw.split('|'))
    need(image == IMAGE and arch == 'arm64' and labels.get('codex.owner') ==
         's11-public-codec-runtime' and labels.get('codex.nonce') == IMAGE_NONCE,
         'runtime image identity')
    code, expiry_raw, err = bounded_command(['/bin/ps', '-p', '18248', '-o',
                                           'pid=,lstart=,command='])
    need(code == 0 and IMAGE_NONCE.encode() in expiry_raw and
         str(IMAGE_EXPIRY).encode() in expiry_raw, 'original runtime expiry process binding')
    disk = shutil.disk_usage('/private/tmp')
    need(disk.free >= 2 * INPUT_CAP, 'host disk free >=1GiB')
    # Only requested scalar fields; Docker MemTotal is not current VM free memory.
    info_raw = docker(['info', '--format', '{{json .OSType}}|{{json .Architecture}}|{{json .MemTotal}}'])[1]
    ostype, architecture, configured_memory = map(json.loads, info_raw.decode().strip().split('|'))
    need(ostype == 'linux' and configured_memory >= 2 * 1024 ** 3, 'Linux VM configured capacity')
    module = oracle()
    manifest, count = [], 0
    with module.OwnedRoot(SOURCE, UID, time.monotonic() + 30) as root:
        raw, receipt_fact = root.read('receipt.json', RECEIPT_SHA, 1024 * 1024)
        receipt = module.unique_json(raw)
        entries = receipt['segments']
        names = [f'seg{i:05d}.ts' for i in range(35)]
        need(len(entries) == 35 and [x['name'] for x in entries] == names, '35 original entries')
        need(sorted(x for x in os.listdir(root.fd) if x.endswith('.ts')) == names, 'exact media tree')
        provenance = receipt['provenance']
        need(provenance['source_commit'] == 'fb4360792a3ae077aa73a40ee30cfcdd024ff445' and
             provenance['source_sha256'] == 'df310256db516c37559f76f0e5f3945609b5c2824b1c548912731de1f49e65e1'
             and provenance['rung'] == 720, 'original Grain720 context')
        wanted = [('receipt.json', RECEIPT_SHA),
                  (receipt['playlist']['name'], receipt['playlist']['sha256'])]
        wanted += [(x[key], x[key_sha]) for x in entries
                   for key, key_sha in [('name', 'sha256'), ('probe_name', 'probe_sha256')]]
        need(len({x[0] for x in wanted}) == 72, 'input manifest duplicates')
        for name, expected in wanted:
            if name == 'receipt.json':
                fact = receipt_fact
            else:
                data, fact = root.read(name, expected, 32 * 1024 * 1024)
            count += fact['bytes']
            need(count <= INPUT_CAP, 'aggregate source bytes')
            manifest.append(fact)
        root_fact = {'dev': root.start.st_dev, 'inode': root.start.st_ino,
                     'uid': root.start.st_uid, 'mode': oct(root.start.st_mode & 0o777)}
    return {'admitted_at_unix': time.time(), 'host_uid': UID, 'source_root': SOURCE,
            'source_root_identity': root_fact, 'held_input_manifest': manifest,
            'held_input_bytes': count, 'receipt_sha256': RECEIPT_SHA,
            'parser_sha256': PARSER_SHA, 'wrapper_sha256': script_hash(),
            'image': image, 'architecture': arch, 'image_labels': labels,
            'runtime_expiry_pid': 18248, 'runtime_expiry_deadline': IMAGE_EXPIRY,
            'runtime_expiry_process_sha256': digest(expiry_raw),
            'runtime_expiry_process_observed': True, 'Docker_idle': True,
            'host_disk_free_bytes': disk.free, 'VM_configured_memory_bytes_NOT_free': configured_memory,
            'VM_free_memory_not_measured': True, 'fresh_media_parse_or_probe_count': 0,
            'bounds': {'cpus': 2, 'memory_bytes': 1024**3, 'swap_total_bytes': 1024**3,
                       'pids': 64, 'network': 'none', 'readonly_runtime': True,
                       'parser_seconds': 60, 'outer_seconds': 90, 'input_bytes': INPUT_CAP,
                       'aggregate_results_bytes': RESULT_CAP, 'tmpfs_bytes': 64*1024**2,
                       'docker_log_bound_bytes': 64*1024**2,
                       'prep_seconds': 30, 'export_seconds': 20, 'cleanup_seconds': 60,
                       'additional_watcher_wait_margin_seconds': 15,
                       'fallback_receipt_reserve_bytes': 8192},
            'limits': 'synthetic old fb436 internal delivered bytes; not current/public/native/fidelity qualification'}


def prepare(source, expected_manifest):
    """Host-side held descriptor copy; executed only after separate grant."""
    module = oracle()
    stage = Path(source)
    stage.mkdir(mode=0o700)
    census = stage / 'census'
    census.mkdir(mode=0o700)
    with module.OwnedRoot(SOURCE, UID, time.monotonic() + 30) as root:
        for entry in expected_manifest:
            data, fact = root.read(entry['name'], entry['sha256'], 32*1024**2)
            need(fact == entry, 'changed original held identity since admission')
            fd = os.open(census / entry['name'], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o400)
            with os.fdopen(fd, 'wb') as stream:
                stream.write(data)
    for original, name, expected in [(PARSER, 'parser.py', PARSER_SHA),
                                      (__file__, 'worker.py', script_hash())]:
        data = Path(original).read_bytes()
        need(digest(data) == expected, 'changed script before copy')
        with open(stage / name, 'xb') as stream:
            stream.write(data)
        (stage / name).chmod(0o400)
    write_once(stage / 'copy-manifest.json', expected_manifest)
    (stage / 'copy-manifest.json').chmod(0o400)
    need(tree_bytes(stage, INPUT_CAP) <= INPUT_CAP, 'staging aggregate cap')


def inner_prepare():
    """Root init has only CHOWN capability; exact fresh nonce volumes only."""
    need(os.getuid() == 0, 'preparer UID')
    for mount in ['/input', '/results']:
        need(not os.listdir(mount), 'fresh volume not empty')
    wanted = json.loads(Path('/staged/copy-manifest.json').read_bytes())
    need(len(wanted) == 72, 'copy manifest bound')
    Path('/input/census').mkdir(mode=0o700)
    count = 0
    module = oracle('/staged/parser.py')
    with module.OwnedRoot('/staged/census', 0, time.monotonic() + 30) as held:
        for entry in wanted:
            dst = Path('/input/census') / entry['name']
            data, fact = held.read(entry['name'], entry['sha256'], 32*1024**2)
            count += len(data)
            need(count <= INPUT_CAP and len(data) == entry['bytes'], 'staged byte/hash cap')
            with open(dst, 'xb') as stream:
                stream.write(data)
            dst.chmod(0o400)
            os.chown(dst, UID, 20)
    for name in ['parser.py', 'worker.py']:
        data = (Path('/staged') / name).read_bytes()
        need(len(data) <= 65536, 'private script cap')
        need(digest(data) == (PARSER_SHA if name == 'parser.py' else script_hash()),
             'staged code identity')
        with open(Path('/input') / name, 'xb') as stream:
            stream.write(data)
        (Path('/input') / name).chmod(0o400)
        os.chown(Path('/input') / name, UID, 20)
    for path in ['/input/census', '/input', '/results']:
        os.chmod(path, 0o700)
        os.chown(path, UID, 20)
    print(json.dumps({'copied_files': len(wanted), 'bytes': count, 'uid': UID}))


def inner_worker():
    """One parser child; no probe/decoder. Preserve partials even on refusal."""
    need(os.getuid() == UID, 'worker UID501')
    need(Path('/results').stat().st_uid == UID and not os.listdir('/results'), 'fresh owned result tree')
    module = oracle('/input/parser.py')
    with module.OwnedRoot('/input/census', UID) as root:
        receipt, fact = root.read('receipt.json', RECEIPT_SHA, 1024**2)
    held = os.open('/input/parser.py', os.O_RDONLY | os.O_NOFOLLOW)
    need(digest(os.read(held, 65536)) == PARSER_SHA, 'held parser source')
    argv = [sys.executable, '-I', '-B', '/input/parser.py', '--root', '/input/census',
            '--receipt-sha256', RECEIPT_SHA, '--output', '/results/nal-result.json',
            '--uid', '501', '--deadline-seconds', '60']
    write_once('/results/intent.json', {'pid': os.getpid(), 'uid': UID,
               'parser_source_fd': held, 'parser_fstat': list(os.fstat(held)[:3]),
               'parser_sha256': PARSER_SHA, 'wrapper_sha256': script_hash(),
               'argv_nul_sha256': digest(b'\0'.join(x.encode() for x in argv) + b'\0'),
               'receipt_identity': fact, 'argv': argv})
    # The parser alone cannot consume the full aggregate result allowance:
    # individual file cap900000 + combined captured pipes32768 + worker/status
    # reserve stay <=950000, leaving >98KiB for outer immutable receipts.
    resource.setrlimit(resource.RLIMIT_FSIZE, (900000, 900000))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    child_fact = {}
    def started(pid):
        child_fact['pid'] = pid
        process = Path('/proc') / str(pid)
        raw = (process / 'cmdline').read_bytes()
        expected = b'\0'.join(x.encode() for x in argv) + b'\0'
        need(raw == expected, 'actual parser NUL argv mismatch')
        inherited = os.stat(process / 'fd' / str(held))
        source = os.fstat(held)
        need((inherited.st_dev, inherited.st_ino) == (source.st_dev, source.st_ino),
             'inherited held parser source mismatch')
        write_once('/results/actual-parser-process.json', {
            'pid': pid, 'parent_pid': os.getpid(), 'argv_nul_sha256': digest(raw),
            'argv': raw[:-1].decode().split('\0'),
            'exe': os.readlink(process / 'exe'),
            'proc_stat': (process / 'stat').read_text()[:2048],
            'held_source_fd': held, 'source_dev': source.st_dev,
            'source_inode': source.st_ino, 'source_sha256': PARSER_SHA})
    begin = time.monotonic()
    code, out, err = None, b'', b''
    error = None
    try:
        code, out, err = bounded_command(argv, seconds=60, cap=CLI_CAP,
                                         pass_fds=(held,), on_start=started)
    except CommandFailure as failure:
        code, out, err = failure.code, failure.out, failure.err
        error = repr(failure)
    except BaseException as failure:
        error = repr(failure)
    finally:
        for name, data in [('parser.stdout', out), ('parser.stderr', err)]:
            with open('/results/' + name, 'xb') as stream:
                stream.write(data)
        witness = {}
        for name in ['memory.peak', 'memory.events', 'memory.max', 'memory.swap.max',
                     'pids.max', 'cpu.max']:
            path = Path('/sys/fs/cgroup') / name
            if path.is_file():
                witness[name] = path.read_text()[:2048]
        result_identity, result_observer_error = None, None
        try:
            if Path('/results/nal-result.json').exists():
                result_data, result_identity = held_generated_result('/results/nal-result.json')
        except BaseException as failure:
            result_observer_error = repr(failure)[:1024]
        write_once('/results/worker-terminal.json', {
            'returncode': code, 'error': error, 'elapsed_seconds': time.monotonic() - begin,
            'cgroup': witness, 'parser_sha256': PARSER_SHA,
            'result_exists': Path('/results/nal-result.json').exists(),
            'result_identity': result_identity, 'result_observer_error': result_observer_error,
            'held_parser_inode': os.fstat(held).st_ino,
            'parser_pid': child_fact.get('pid'),
            'parser_process_group_absent': process_absent(child_fact['pid']) if child_fact else None})
        os.close(held)
        tree_bytes('/results', WORKER_CAP)
    return 0 if code == 0 and error is None and result_identity is not None and result_observer_error is None else 1


def configuration(nonce, image, volumes, mounts, name, user, mode):
    args = ['create', '--name', name, '--label', 'codex.owner=s11-retained-nal',
            '--label', 'codex.nonce=' + nonce, '--user', user, '--network', 'none',
            '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
            '--cpus', '2', '--memory', '1g', '--memory-swap', '1g', '--pids-limit', '64',
            '--restart', 'no', '--log-driver', 'local', '--log-opt', 'max-size=64m',
            '--log-opt', 'max-file=1', '--log-opt', 'compress=false',
            '--tmpfs', '/tmp:rw,nosuid,nodev,size=64m,uid=501,gid=20']
    if mode == 'inner-prepare':
        args += ['--cap-add', 'CHOWN']
    for mount in mounts:
        args += ['--mount', mount]
    args += ['--entrypoint', '/opt/playwright-venv/bin/python', image, '-I', '-B',
             '/staged/worker.py' if mode == 'inner-prepare' else '/input/worker.py', '--' + mode]
    return args


def exact_remove(cid, nonce):
    need(re.fullmatch('[0-9a-f]{64}', cid) is not None, 'exact full container ID')
    code, raw, err = docker(['inspect', '--format', '{{json .Id}}|{{json .Config.Labels}}', cid], allow_fail=True)
    if code:
        need(cid not in container_inventory(), 'failed inspect is not absence')
        return {'absence_inventory': 'successful full container listing'}
    identity, labels = map(json.loads, raw.decode().strip().split('|'))
    need(identity == cid and labels.get('codex.nonce') == nonce and
         labels.get('codex.owner') == 's11-retained-nal', 'cleanup container identity')
    docker(['rm', '-f', cid], seconds=15)
    need(cid not in container_inventory(), 'container absence inventory')
    return {'absence_inventory': 'successful full container listing'}


def exact_volume_remove(volume, nonce):
    code, raw, err = docker(['volume', 'inspect', '--format', '{{json .Labels}}', volume], allow_fail=True)
    if code:
        need(volume not in volume_inventory(), 'failed volume inspect is not absence')
        return {'absence_inventory': 'successful full volume listing'}
    labels = json.loads(raw)
    need(labels.get('codex.nonce') == nonce and labels.get('codex.owner') ==
         's11-retained-nal', 'cleanup volume identity')
    need(not docker(['ps', '-aq', '--filter', 'volume=' + volume])[1].strip(), 'volume holder remains')
    docker(['volume', 'rm', volume], seconds=15)
    need(volume not in volume_inventory(), 'volume absence inventory')
    return {'absence_inventory': 'successful full volume listing'}


def container_inventory(filters=()):
    args = ['ps', '-aq', '--no-trunc']
    for value in filters:
        args += ['--filter', value]
    rows = docker(args)[1].decode().split()
    need(len(set(rows)) == len(rows) and all(re.fullmatch('[0-9a-f]{64}', x) for x in rows),
         'bounded full container inventory syntax')
    return rows


def volume_inventory(filters=()):
    args = ['volume', 'ls', '-q']
    for value in filters:
        args += ['--filter', value]
    rows = docker(args)[1].decode().split()
    need(len(set(rows)) == len(rows) and all(re.fullmatch('[A-Za-z0-9_.-]{1,128}', x) for x in rows),
         'bounded volume inventory syntax')
    return rows


def owned_inventory(nonce):
    need(re.fullmatch('[0-9a-f]{32}', nonce) is not None, 'cleanup nonce syntax')
    filters = ['label=codex.owner=s11-retained-nal', 'label=codex.nonce=' + nonce]
    # Successful daemon queries discover resources even if create lost its reply.
    return {'containers': container_inventory(filters), 'volumes': volume_inventory(filters)}


def guardian(root, nonce, deadline):
    """Finite exact owned resource expiry even if controller disappears."""
    while time.time() < deadline:
        if (root / 'guardian-done').exists():
            return 0
        time.sleep(0.25)
    intent = json.loads((root / 'resource-intent.json').read_bytes())
    need(intent['nonce'] == nonce, 'guardian nonce')
    failures = []
    global CLI_DEADLINE
    CLI_DEADLINE = time.monotonic() + 60
    try:
        discovered = owned_inventory(nonce)
        containers = set(intent.get('containers', [])) | set(discovered['containers'])
        volumes = set(intent['volumes']) | set(discovered['volumes'])
        for kind, rows in [('containers', containers), ('volumes', volumes)]:
            for target in sorted(rows):
                try:
                    (exact_remove if kind == 'containers' else exact_volume_remove)(target, nonce)
                except BaseException as failure:
                    failures.append({'kind': kind, 'target': target, 'error': repr(failure)[:1024]})
        remaining = owned_inventory(nonce)
        need(not remaining['containers'] and not remaining['volumes'], 'guardian owned resources remain')
    except BaseException as failure:
        failures.append({'observer': repr(failure)[:1024]})
    write_once(root / 'guardian-expiry.json', {'expired_at': time.time(), 'nonce': nonce,
                                             'failures': failures})
    return 1 if failures else 0


def container_watch(root, nonce, cid, deadline):
    """Independent90s outer timer kills, never removes, preserving result bytes."""
    while time.time() < deadline:
        if (root / 'parser-watcher-done').exists():
            return 0
        time.sleep(min(0.05, max(0, deadline - time.time())))
    exact_stop(cid, nonce)
    write_once(root / 'parser-watcher-fired.json', {'container': cid, 'nonce': nonce,
                                                  'at_unix': time.time()})
    return 0


def process_absent(pid):
    try:
        os.killpg(pid, 0)
        return False
    except ProcessLookupError:
        return True


def execute(admission_path, admission_sha):
    global CLI_DEADLINE
    admission_raw = Path(admission_path).read_bytes()
    need(digest(admission_raw) == admission_sha, 'immutable admission hash')
    facts = json.loads(admission_raw)
    need(facts['wrapper_sha256'] == script_hash() and facts['parser_sha256'] == PARSER_SHA,
         'admission source pin')
    need(time.time() - facts['admitted_at_unix'] <= 300, 'admission older than300s')
    fresh = admission_facts()
    need(fresh['held_input_manifest'] == facts['held_input_manifest'], 'fresh source changed')
    nonce = uuid.uuid4().hex
    root = Path(tempfile.mkdtemp(prefix='s11-nal-corpus-' + nonce[:8] + '-', dir='/private/tmp'))
    write_once(root / 'initial-admission.json', facts)
    write_once(root / 'execution-fresh-admission.json', fresh)
    stage = root / 'staged'
    input_volume, result_volume = 's11-nal-input-' + nonce, 's11-nal-results-' + nonce
    intent = {'nonce': nonce, 'containers': [], 'volumes': [input_volume, result_volume]}
    statuses, export_ok, watcher, outer = [], False, None, None
    success, error, parser_cid, result_identity = False, None, None, None
    def update_intent():
        # Mutable private guardian state is NOT the immutable initial admission.
        temporary = root / 'resource-intent.new'
        write_once(temporary, intent)
        os.replace(temporary, root / 'resource-intent.json')
    try:
        untouched = owned_inventory(nonce)
        need(not untouched['containers'] and not untouched['volumes'], 'new nonce already has resources')
        prepare(stage, facts['held_input_manifest'])
        update_intent()
        expiry = time.time() + 240
        watcher = subprocess.Popen([sys.executable, '-I', '-B', __file__, '--guardian',
                                   str(root), nonce, str(expiry)], start_new_session=True,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)
        write_once(root / 'guardian-launch.json', {'pid': watcher.pid, 'deadline_unix': expiry,
                                                  'nonce': nonce, 'wrapper_sha256': script_hash()})
        for volume in intent['volumes']:
            need(docker(['volume', 'create', '--label', 'codex.owner=s11-retained-nal',
                         '--label', 'codex.nonce=' + nonce, volume])[1].decode().strip() == volume,
                 'fresh volume create')
        prep_mounts = ['type=bind,src=' + str(stage) + ',dst=/staged,readonly',
                       'type=volume,src=' + input_volume + ',dst=/input',
                       'type=volume,src=' + result_volume + ',dst=/results']
        prep = docker(configuration(nonce, IMAGE, intent['volumes'], prep_mounts,
                                    's11-nal-prep-' + nonce, '0:0', 'inner-prepare'))[1].decode().strip()
        intent['containers'].append(prep); update_intent()
        docker(['start', prep])
        status = wait_container(prep, 30)
        statuses.append({'phase': 'prepare', 'container': prep, 'state': status})
        prep_log = docker(['logs', prep], seconds=10)[1]
        write_once(root / 'preparer-log.json', {'raw_utf8': prep_log.decode(errors='replace'),
                                              'sha256': digest(prep_log)}, cap=8192)
        need(status['ExitCode'] == 0 and not status['OOMKilled'], 'copy preparer failed')
        mounts = ['type=volume,src=' + input_volume + ',dst=/input,readonly',
                  'type=volume,src=' + result_volume + ',dst=/results']
        parser_cid = docker(configuration(nonce, IMAGE, intent['volumes'], mounts,
                                          's11-nal-parser-' + nonce, '501:20', 'inner-worker'))[1].decode().strip()
        intent['containers'].append(parser_cid); update_intent()
        inspection = json.loads(docker(['inspect', parser_cid])[1])[0]
        config = inspection['HostConfig']
        need(config['Memory'] == 1024**3 and config['MemorySwap'] == 1024**3 and
             config['NanoCpus'] == 2*10**9 and config['PidsLimit'] == 64 and
             config['NetworkMode'] == 'none' and config['ReadonlyRootfs'], 'actual runtime cap mismatch')
        write_once(root / 'actual-runtime-config.json', {'id': parser_cid,
                    'image': inspection['Image'], 'HostConfig': config,
                    'user': inspection['Config']['User'], 'nonce': nonce})
        parser_deadline = time.time() + 90
        outer = subprocess.Popen([sys.executable, '-I', '-B', __file__, '--container-watch',
                                  str(root), nonce, parser_cid, str(parser_deadline)],
                                 start_new_session=True, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        write_once(root / 'parser-watcher-launch.json', {'pid': outer.pid,
                   'container': parser_cid, 'deadline_unix': parser_deadline,
                   'wrapper_sha256': script_hash()})
        docker(['start', parser_cid])
        status = wait_container(parser_cid, 90)
        statuses.append({'phase': 'parser', 'container': parser_cid, 'state': status})
        export = root / 'export'
        export.mkdir(mode=0o700)
        docker(['cp', parser_cid + ':/results/.', str(export)], seconds=20)
        tree_bytes(export, WORKER_CAP)
        export_ok = True
        need(status['ExitCode'] == 0 and not status['OOMKilled'], 'new corpus parser failed')
        terminal = json.loads((export / 'worker-terminal.json').read_bytes())
        need(terminal['returncode'] == 0 and terminal['error'] is None and
             terminal['parser_process_group_absent'] is True and terminal['result_exists'] is True
             and terminal['result_observer_error'] is None, 'worker parser/refusal/terminal identity')
        generated, result_identity = held_generated_result(export / 'nal-result.json')
        inner_identity = terminal['result_identity']
        need(inner_identity and all(result_identity[key] == inner_identity[key] for key in ['sha256', 'bytes']),
             'exported NAL result bytes/hash mismatch')
        document = json.loads(generated)
        need(document['kind'] == 'NEW-retained-byte-H264-NAL-syntax-oracle' and
             document['receipt']['sha256'] == RECEIPT_SHA and len(document['segments']) == 35,
             'exported NAL result source/count/kind')
        printed = json.loads((export / 'parser.stdout').read_bytes())
        need(printed['sha256'] == result_identity['sha256'] and
             printed['bytes'] == result_identity['bytes'] and printed['segments'] == 35,
             'actual parser output/result identity')
        success = True
    except BaseException as failure:
        error = repr(failure)
        # Capture raw partials on any failed inner stage before any destructive cleanup.
        if parser_cid and not export_ok:
            try:
                exact_stop(parser_cid, nonce)
                export = root / 'export'
                export.mkdir(mode=0o700, exist_ok=True)
                docker(['cp', parser_cid + ':/results/.', str(export)], seconds=20)
                tree_bytes(export, WORKER_CAP)
                export_ok = True
            except BaseException as export_error:
                error += '; partial export failure: ' + repr(export_error)
    finally:
        CLI_DEADLINE = time.monotonic() + 60
        cleanup = []
        final_errors = []
        discovered, remaining = None, None
        watcher_terminal = watcher_absent = None
        outer_terminal = outer_absent = None
        stage_removed = False
        def observe(label, action):
            nonlocal success
            try:
                return action()
            except BaseException as failure:
                final_errors.append({'observer': label, 'error': repr(failure)[:1024]})
                success = False
                return None
        discovered = observe('discover exact owner/nonce, including lost create replies',
                             lambda: owned_inventory(nonce))
        targets = list(intent['containers'])
        volumes = list(intent['volumes'])
        if discovered is not None:
            targets = sorted(set(targets) | set(discovered['containers']))
            volumes = sorted(set(volumes) | set(discovered['volumes']))
        for cid in reversed(targets):
            proof = observe('remove container ' + cid, lambda cid=cid: exact_remove(cid, nonce))
            cleanup.append({'container': cid, 'absent': proof is not None, 'proof': proof})
        # Never erase an unexported started result volume; finite guardian owns it.
        for volume in volumes:
            if volume == result_volume and parser_cid and not export_ok:
                cleanup.append({'volume': volume, 'retained_until_guardian_expiry': True})
                success = False
                continue
            proof = observe('remove volume ' + volume,
                            lambda volume=volume: exact_volume_remove(volume, nonce))
            cleanup.append({'volume': volume, 'absent': proof is not None, 'proof': proof})
        remaining = observe('postcleanup owner/nonce successful inventory', lambda: owned_inventory(nonce))
        all_absent = (discovered is not None and remaining is not None and
                      not remaining['containers'] and not remaining['volumes'] and
                      all(x.get('absent') for x in cleanup))
        success = success and all_absent
        # Up to15s bounded observer margin follows the aggregate60s Docker cleanup
        # deadline. If waits/absence checks fail, guardian retains uncertain ownership.
        if outer:
            observe('signal parser watcher', lambda: (root / 'parser-watcher-done').touch(mode=0o600))
            outer_terminal = observe('parser watcher wait12s', lambda: outer.wait(timeout=12))
            outer_absent = observe('parser watcher process absence', lambda: process_absent(outer.pid))
            success = success and outer_terminal == 0 and outer_absent
        if watcher and all_absent:
            observe('signal guardian', lambda: (root / 'guardian-done').touch(mode=0o600))
            watcher_terminal = observe('guardian wait3s', lambda: watcher.wait(timeout=3))
            watcher_absent = observe('guardian process absence', lambda: process_absent(watcher.pid))
            success = success and watcher_terminal == 0 and watcher_absent
        def remove_stage():
            need(stage.parent == root and stage.name == 'staged' and
                 stage.lstat().st_uid == UID and not stage.is_symlink(), 'exact staged cleanup boundary')
            shutil.rmtree(stage)
            return True
        if all_absent and (export_ok or not parser_cid):
            stage_exists = observe('stage existence', lambda: stage.exists())
            if stage_exists:
                stage_removed = observe('exact owned stage removal', remove_stage)
        result_bytes = observe('bounded outer metadata accounting',
                               lambda: sum(p.stat().st_size for p in root.iterdir() if p.is_file()))
        export_exists = observe('export existence', lambda: (root / 'export').exists())
        if export_exists:
            exported_bytes = observe('bounded exported result walk', lambda: tree_bytes(root / 'export', WORKER_CAP))
            if result_bytes is not None and exported_bytes is not None:
                result_bytes += exported_bytes
            else:
                result_bytes = None
        observe('pre-final aggregate result cap',
                lambda: need(result_bytes is not None and result_bytes <= RESULT_CAP - 16384,
                             'final receipt reserved result budget'))
        final = {'nonce': nonce, 'success': bool(success), 'error': error,
                   'states': statuses, 'partial_or_complete_exported': export_ok,
                   'cleanup': cleanup, 'guardian_pid': watcher.pid if watcher else None,
                   'guardian_terminal': watcher_terminal, 'all_resources_absent': all_absent,
                   'guardian_process_group_absent': watcher_absent,
                   'parser_watcher_pid': outer.pid if outer else None,
                   'parser_watcher_terminal': outer_terminal,
                   'parser_watcher_process_group_absent': outer_absent,
                   'wrapper_sha256': facts['wrapper_sha256'], 'parser_sha256': PARSER_SHA,
                   'source_preserved': SOURCE, 'no_producer_probe_decoder_old_test': True,
                   'discovered_resources': discovered, 'postcleanup_inventory': remaining,
                   'final_observer_errors': final_errors, 'host_stage_removed': stage_removed,
                   'exported_nal_result_identity': result_identity,
                   'result_bytes_before_final': result_bytes,
                   'cleanup_Docker_seconds': 60, 'watcher_wait_additional_seconds': 15}
        # Atomic final receipt prevents a failed write leaving a partial final.json.
        # All cleanup/observer failures above are data, not escaping exceptions.
        try:
            encoded = json.dumps(final, sort_keys=True, indent=2).encode()
            need(len(encoded) <= 16384, 'bounded final receipt16KiB')
            pending = root / 'final.pending'
            write_once(pending, final, cap=16384)
            os.replace(pending, root / 'final.json')
            print(json.dumps({'root': str(root), 'success': bool(success),
                              'result_bytes': result_bytes + len(encoded) if result_bytes is not None else None,
                              'all_resources_absent': all_absent}, sort_keys=True))
        except BaseException as receipt_failure:
            success = False
            final['success'] = False
            final['final_observer_errors'].append({'observer': 'primary final receipt',
                                                   'error': repr(receipt_failure)[:1024]})
            # Separate bounded minimal receipt path if metadata budget observation
            # itself failed; no overwrite/deletion of prior/partial evidence.
            try:
                fallback = {'nonce': nonce, 'success': False, 'error': error,
                            'final_observer_errors': final['final_observer_errors'][-8:],
                            'all_resources_absent': all_absent,
                            'uncertain_resources_retained': not all_absent,
                            'wrapper_sha256': facts['wrapper_sha256'], 'parser_sha256': PARSER_SHA}
                data = json.dumps(fallback, sort_keys=True).encode()
                need(len(data) <= 8192, 'minimal fallback receipt cap')
                fd = os.open(root / 'final.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                with os.fdopen(fd, 'wb') as stream:
                    stream.write(data)
                print(json.dumps({'root': str(root), 'success': False, 'fallback_final': True}))
            except BaseException as storage_failure:
                print(json.dumps({'root': str(root), 'success': False,
                                  'final_receipt_storage_unavailable': repr(storage_failure)[:1024]}))
    return 0 if success else 1


def exact_stop(cid, nonce):
    info = json.loads(docker(['inspect', cid])[1])[0]
    need(info['Id'] == cid and info['Config']['Labels'].get('codex.nonce') == nonce and
         info['Config']['Labels'].get('codex.owner') == 's11-retained-nal',
         'stop exact container identity')
    if info['State']['Running']:
        docker(['kill', cid], seconds=10)


def wait_container(cid, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        state = json.loads(docker(['inspect', '--format', '{{json .State}}', cid])[1])
        if not state['Running']:
            return state
        time.sleep(0.2)
    # Caller preserves partials, then exact labelled teardown.
    raise TimeoutError('container outer deadline')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--admit-only', action='store_true')
    mode.add_argument('--execute', action='store_true')
    mode.add_argument('--inner-prepare', action='store_true')
    mode.add_argument('--inner-worker', action='store_true')
    mode.add_argument('--guardian', nargs=3)
    mode.add_argument('--container-watch', nargs=4)
    parser.add_argument('--admission')
    parser.add_argument('--admission-sha256')
    args = parser.parse_args()
    if args.admit_only:
        facts = admission_facts()
        root = Path(tempfile.mkdtemp(prefix='s11-nal-readonly-admission-', dir='/private/tmp'))
        path = root / 'initial.json'
        write_once(path, facts)
        path.chmod(0o400)
        print(json.dumps({'path': str(path), 'sha256': digest(path.read_bytes()),
                          'bytes': path.stat().st_size, 'input_bytes': facts['held_input_bytes'],
                          'Docker_idle': True, 'corpus_executed': False}))
        return 0
    if args.inner_prepare:
        inner_prepare(); return 0
    if args.inner_worker:
        return inner_worker()
    if args.guardian:
        return guardian(Path(args.guardian[0]), args.guardian[1], float(args.guardian[2]))
    if args.container_watch:
        return container_watch(Path(args.container_watch[0]), args.container_watch[1],
                               args.container_watch[2], float(args.container_watch[3]))
    need(args.admission and args.admission_sha256, 'execution needs approved immutable admission')
    return execute(args.admission, args.admission_sha256)


if __name__ == '__main__':
    raise SystemExit(main())
