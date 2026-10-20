#!/usr/bin/env python3
"""Best-effort, job-local resource evidence; never replace the test exit status.

CLI: start --output DIR --cell ID; finish --output DIR. The detached sampler
reads counters every five seconds. It never records argv, full environments,
network data, credentials, or per-process command lines.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import uuid

INTERVAL = 5.0
MAX_PIDS = 8192
ALLOWED_ENV = (
    'GITHUB_SHA', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_JOB',
    'GITHUB_EVENT_NAME', 'FORGEJO_SHA', 'FORGEJO_RUN_ID', 'FORGEJO_JOB',
    'CARGO_BUILD_JOBS', 'RUST_TEST_THREADS',
)


def now():
    return datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z')


def read(path):
    try:
        return Path(path).read_text(encoding='utf-8')[:65536]
    except (OSError, UnicodeError):
        return None


def write_json(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + '.tmp-' + str(os.getpid()))
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    temporary.replace(path)


def pairs(text):
    result = {}
    for line in (text or '').splitlines():
        fields = line.split()
        if len(fields) == 2:
            try:
                result[fields[0]] = int(fields[1])
            except ValueError:
                pass
    return result


def integer(path):
    value = read(path)
    try:
        return int(value.strip()) if value is not None else None
    except ValueError:
        return None


def proc_stat(text):
    """Parse Linux /proc/PID/stat, including comm values containing spaces/')."""
    if not text or '(' not in text or ')' not in text:
        return None
    begin, end = text.find('('), text.rfind(')')
    fields = text[end + 1:].split()
    try:
        return {
            'comm': text[begin + 1:end][:64], 'cpu_ticks': int(fields[11]) + int(fields[12]),
            'threads': int(fields[17]), 'start_ticks': int(fields[19]), 'rss_pages': int(fields[21]),
        }
    except (IndexError, ValueError):
        return None


def same_process(state, proc_root=Path('/proc')):
    pid, ticks = state.get('pid'), state.get('start_ticks')
    if not isinstance(pid, int) or pid <= 1 or not isinstance(ticks, int):
        return False
    observed = proc_stat(read(proc_root / str(pid) / 'stat'))
    return observed is not None and observed['start_ticks'] == ticks


def discover_cgroup(proc_root=Path('/proc')):
    """Resolve the current cgroup through mountinfo; no host-wide PID fallback."""
    memberships = []
    for line in (read(proc_root / 'self/cgroup') or '').splitlines():
        fields = line.split(':', 2)
        if len(fields) == 3:
            memberships.append((fields[1].split(',') if fields[1] else [], fields[2]))
    mounts = []
    for line in (read(proc_root / 'self/mountinfo') or '').splitlines():
        if ' - ' not in line:
            continue
        left, right = line.split(' - ', 1)
        fields, tail = left.split(), right.split()
        if len(fields) >= 5 and len(tail) >= 3 and tail[0] in ('cgroup', 'cgroup2'):
            mounts.append((tail[0], fields[3], fields[4], tail[2].split(',')))
    roots = {}
    for controllers, path in memberships:
        for kind, mount_root, mountpoint, options in mounts:
            if (kind == 'cgroup2' and controllers) or (kind == 'cgroup' and not set(controllers) & set(options)):
                continue
            if path == '/':
                relative = ''  # Namespace-relative root of this job's mount.
            elif mount_root == '/':
                relative = path.lstrip('/')
            elif path == mount_root or path.startswith(mount_root.rstrip('/') + '/'):
                relative = path[len(mount_root):].lstrip('/')
            else:
                continue
            # No parent traversal out of the advertised mount.
            if '..' in Path(relative).parts:
                continue
            root = Path(mountpoint) / relative
            if kind == 'cgroup2':
                roots['v2'] = root
            else:
                for controller in set(controllers) & set(options):
                    roots[controller] = root
    return roots


def cgroup_sample(roots):
    values = {}
    missing = []
    pids_root = None
    if 'v2' in roots:
        root = roots['v2']
        pids_root = root
        for filename in ('cpu.stat', 'memory.events'):
            text = read(root / filename)
            if text is not None:
                values[filename] = pairs(text)
            else:
                missing.append(filename)
        for filename in ('memory.current', 'memory.peak', 'memory.swap.current', 'pids.current'):
            value = integer(root / filename)
            if value is not None:
                values[filename] = value
            else:
                missing.append(filename)
        for filename in ('cpu.max', 'memory.max', 'pids.max'):
            value = read(root / filename)
            if value is not None:
                values[filename] = value.strip()
        io = {}
        io_text = read(root / 'io.stat')
        if io_text is None:
            missing.append('io.stat')
        for line in (io_text or '').splitlines():
            fields = line.split()
            if not fields:
                continue
            device = {}
            for field in fields[1:]:
                if '=' in field:
                    key, value = field.split('=', 1)
                    try:
                        device[key] = int(value)
                    except ValueError:
                        pass
            io[fields[0]] = device
        if io_text is not None:
            values['io.stat'] = io
    else:
        # v1 blkio counters are not collected by this helper. Process IO sums
        # exclude exited processes and cannot stand in for cgroup IO evidence.
        missing.append('cgroup v1 IO counters unavailable (blkio not sampled)')
        specifications = {
            'cpuacct': ('cpuacct.usage',),
            'cpu': ('cpu.stat',),
            'memory': ('memory.usage_in_bytes', 'memory.max_usage_in_bytes', 'memory.limit_in_bytes', 'memory.failcnt'),
            'pids': ('pids.current', 'pids.max'),
        }
        for controller, filenames in specifications.items():
            root = roots.get(controller)
            if root is None:
                missing.append(controller)
                continue
            pids_root = pids_root or root
            for filename in filenames:
                value = read(root / filename)
                if value is not None:
                    values[filename] = pairs(value) if filename.endswith('.stat') else value.strip()
                else:
                    missing.append(filename)
    return values, missing, pids_root


def process_sample(cgroup_root, proc_root=Path('/proc')):
    if cgroup_root is None:
        return {'available': False, 'reason': 'current cgroup unavailable; host process scan disabled'}
    text = read(cgroup_root / 'cgroup.procs')
    if text is None:
        return {'available': False, 'reason': 'current cgroup PID list unavailable'}
    names = Counter()
    totals = Counter()
    pids = sorted({int(pid) for pid in text.split() if pid.isdigit()})
    skipped = 0
    for pid in pids[:MAX_PIDS]:
        value = proc_stat(read(proc_root / str(pid) / 'stat'))
        if value is None:
            skipped += 1
            continue
        names[value['comm']] += 1
        totals['cpu_ticks_live_processes'] += value['cpu_ticks']
        totals['threads'] += value['threads']
        totals['rss_bytes_sum'] += value['rss_pages'] * os.sysconf('SC_PAGE_SIZE')
        io = pairs(read(proc_root / str(pid) / 'io'))
        totals['read_bytes_live_processes'] += io.get('read_bytes:', 0)
        totals['write_bytes_live_processes'] += io.get('write_bytes:', 0)
    return {'available': True, 'scope': 'current cgroup only; nested cgroups excluded',
            'listed_processes': len(pids), 'observed_processes': sum(names.values()),
            'raced_or_unreadable_processes': skipped, 'truncated': len(pids) > MAX_PIDS,
            'by_comm': dict(names.most_common()), **totals}


def disk_sample(targets):
    values = {}
    for label, path in targets.items():
        try:
            info = os.statvfs(path)
            values[label] = {'available_bytes': info.f_bavail * info.f_frsize,
                             'total_bytes': info.f_blocks * info.f_frsize,
                             'available_inodes': info.f_favail}
        except OSError:
            values[label] = {'available': False}
    return values


def sample_once(roots, targets, started):
    counters, missing, process_root = cgroup_sample(roots)
    return {'at': now(), 'elapsed_s': round(time.monotonic() - started, 6),
            'cgroup': counters, 'missing_counters': missing,
            'processes': process_sample(process_root), 'disk': disk_sample(targets)}


def command_value(args):
    try:
        result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                text=True, timeout=8, check=False)
        return result.stdout.strip().splitlines()[0][:200] if result.returncode == 0 and result.stdout.strip() else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def execution_observation(execution_scope):
    if execution_scope == 'external-containers':
        return {'available': False, 'cpu': 'unavailable', 'memory': 'unavailable', 'io': 'unavailable',
                'reason': 'tests execute in sibling containers outside the sampled current-job cgroup'}
    if sys.platform != 'linux':
        return {'available': False, 'cpu': 'unavailable', 'memory': 'unavailable', 'io': 'unavailable',
                'reason': 'Linux cgroup counters unavailable on this platform'}
    return {'available': None, 'reason': 'current-job counters only; availability depends on collected counters'}


def metadata(cell, execution_scope='current-job'):
    return {'schema_version': 1, 'cell': cell, 'started_at': now(),
            'candidate_sha': command_value(['git', 'rev-parse', 'HEAD']),
            'candidate_tree': command_value(['git', 'rev-parse', 'HEAD^{tree}']),
            'rustc': command_value(['rustc', '--version']), 'cargo': command_value(['cargo', '--version']),
            'environment': {key: os.environ[key][:200] for key in ALLOWED_ENV if key in os.environ},
            'interval_s': INTERVAL, 'platform': sys.platform,
            'logical_cpu_count': os.cpu_count(),
            'affinity_cpu_count': len(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else None,
            'execution_scope': execution_scope, 'observed_counter_scope': 'current-job cgroup only',
            'actual_test_resources': execution_observation(execution_scope),
            'scope': 'current job cgroup counters; current cgroup process aggregates; filesystem free space',
            'limitations': ['Process RSS sum may double-count shared pages.',
                            'Live-process CPU/IO sums can decrease when processes exit; use cgroup cumulative counters for rates.',
                            'Nested cgroup processes are excluded from process-name aggregation.',
                            'Missing counters remain unavailable, never zero.',
                            'This helper does not execute or change the test suite.']}


def incomplete(output, reason):
    output.mkdir(parents=True, exist_ok=True)
    write_json(output / 'telemetry-incomplete.json', {'at': now(), 'status': 'incomplete', 'reason': reason})
    print('baseline resource telemetry incomplete: ' + reason, file=sys.stderr)


def start(output, cell, execution_scope='current-job'):
    output.mkdir(parents=True, exist_ok=True)
    if (output / 'metadata.json').exists() or (output / 'state.json').exists():
        incomplete(output, 'output directory already contains a telemetry session; refusing to overwrite')
        return
    if sys.platform != 'linux':
        write_json(output / 'metadata.json', metadata(cell, execution_scope))
        incomplete(output, 'Linux cgroup sampler unavailable on this platform')
        return
    token = uuid.uuid4().hex
    write_json(output / 'metadata.json', metadata(cell, execution_scope))
    with (output / 'sampler.log').open('a', encoding='utf-8') as stream:
        process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), 'sample',
                                    '--output', str(output.resolve()), '--token', token],
                                   stdin=subprocess.DEVNULL, stdout=stream, stderr=stream,
                                   start_new_session=True, close_fds=True)
    # Linux start ticks bind finish to the exact spawned process, not a reused PID.
    identity = proc_stat(read(Path('/proc') / str(process.pid) / 'stat'))
    write_json(output / 'state.json', {'pid': process.pid, 'start_ticks': identity['start_ticks'] if identity else None,
                                      'token': token, 'status': 'sampling'})
    if identity is None:
        incomplete(output, 'could not establish sampler process identity')


def sample(output, token):
    running = True

    def stop_signal(_number, _frame):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, stop_signal)
    signal.signal(signal.SIGINT, stop_signal)
    roots = discover_cgroup()
    session_metadata = json.loads(read(output / 'metadata.json') or '{}')
    execution_scope = session_metadata.get('execution_scope', 'current-job')
    actual_test_resources = execution_observation(execution_scope)
    targets = {'workspace': Path.cwd()}
    target = os.environ.get('CARGO_TARGET_DIR')
    if target:
        targets['cargo_target'] = Path(target)
    started = time.monotonic()
    count = 0
    peak_memory = peak_rss = None
    low_disk = {}
    missing = set()
    if actual_test_resources['available'] is False:
        missing.add('actual test CPU/memory/IO: ' + actual_test_resources['reason'])
    with (output / 'samples.jsonl').open('a', encoding='utf-8', buffering=1) as stream:
        while running:
            value = sample_once(roots, targets, started)
            value['observed_counter_scope'] = 'current-job cgroup only'
            value['actual_test_resources'] = actual_test_resources
            stream.write(json.dumps(value, sort_keys=True) + '\n')
            count += 1
            memory = value['cgroup'].get('memory.current')
            if memory is None and 'memory.usage_in_bytes' in value['cgroup']:
                memory = int(value['cgroup']['memory.usage_in_bytes'])
            if memory is not None:
                peak_memory = max(peak_memory if peak_memory is not None else memory, memory)
            if value['processes']['available']:
                rss = value['processes'].get('rss_bytes_sum', 0)
                peak_rss = max(peak_rss if peak_rss is not None else rss, rss)
            missing.update(value['missing_counters'])
            if not value['processes']['available']:
                missing.add('process aggregates')
            for label, measurement in value['disk'].items():
                if 'available_bytes' in measurement:
                    low_disk[label] = min(low_disk.get(label, measurement['available_bytes']), measurement['available_bytes'])
                else:
                    missing.add('disk:' + label)
            if (output / 'stop').exists():
                break
            deadline = time.monotonic() + INTERVAL
            while running and time.monotonic() < deadline and not (output / 'stop').exists():
                time.sleep(min(0.25, max(0, deadline - time.monotonic())))
    write_json(output / 'summary.json', {'schema_version': 1, 'token': token, 'status': 'complete' if count and not missing else 'incomplete',
                                       'execution_scope': execution_scope,
                                       'observed_counter_scope': 'current-job cgroup only',
                                       'actual_test_resources': actual_test_resources,
                                       'ended_at': now(), 'elapsed_s': round(time.monotonic() - started, 6),
                                       'samples': count, 'peak_sampled_cgroup_memory_bytes': peak_memory,
                                       'peak_sampled_process_rss_sum_bytes': peak_rss,
                                       'minimum_sampled_disk_available_bytes': low_disk, 'unavailable': sorted(missing)})


def finish(output):
    output.mkdir(parents=True, exist_ok=True)
    raw_metadata = read(output / 'metadata.json')
    if raw_metadata is not None:
        final_metadata = json.loads(raw_metadata)
        final_metadata['finished_at'] = now()
        if final_metadata.get('started_at'):
            began = datetime.fromisoformat(final_metadata['started_at'].replace('Z', '+00:00'))
            ended = datetime.fromisoformat(final_metadata['finished_at'].replace('Z', '+00:00'))
            final_metadata['observed_telemetry_window_s'] = round((ended - began).total_seconds(), 6)
        write_json(output / 'metadata.json', final_metadata)
    raw = read(output / 'state.json')
    if raw is None:
        incomplete(output, 'sampler state missing; start did not complete')
        return
    state = json.loads(raw)
    (output / 'stop').touch()
    deadline = time.monotonic() + 12
    while same_process(state) and not (output / 'summary.json').exists() and time.monotonic() < deadline:
        time.sleep(0.25)
    if not (output / 'summary.json').exists():
        if same_process(state):
            os.kill(state['pid'], signal.SIGTERM)
            final_deadline = time.monotonic() + 3
            while same_process(state) and time.monotonic() < final_deadline:
                time.sleep(0.1)
            if same_process(state):
                os.kill(state['pid'], signal.SIGKILL)
        incomplete(output, 'sampler did not publish a final summary before its bounded shutdown')
        return
    summary = json.loads((output / 'summary.json').read_text())
    if summary.get('token') != state.get('token'):
        incomplete(output, 'sampler identity does not match final summary')
        return
    summary['final_rustc'] = command_value(['rustc', '--version'])
    summary['final_cargo'] = command_value(['cargo', '--version'])
    summary.pop('token', None)
    write_json(output / 'summary.json', summary)
    state['status'] = 'finished'
    state.pop('token', None)
    write_json(output / 'state.json', state)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['start', 'sample', 'finish'])
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--cell')
    parser.add_argument('--execution-scope', choices=['current-job', 'external-containers'], default='current-job',
                        help='Use external-containers when actual tests run in sibling Docker containers')
    parser.add_argument('--token', help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.action == 'start':
            if not args.cell or len(args.cell) > 200 or any(ord(char) < 32 for char in args.cell):
                raise ValueError('cell must be a short printable identifier')
            start(args.output, args.cell, args.execution_scope)
        elif args.action == 'sample':
            sample(args.output, args.token)
        else:
            finish(args.output)
    except Exception as error:
        # Report the exception class only: filesystem/subprocess errors can carry
        # private host paths or command text. Never change the suite's exit code.
        try:
            incomplete(args.output, 'resource helper error: ' + type(error).__name__)
        except OSError:
            print('baseline resource telemetry incomplete: cannot write receipt', file=sys.stderr)
    return 0


if __name__ == '__main__':
    sys.exit(main())
