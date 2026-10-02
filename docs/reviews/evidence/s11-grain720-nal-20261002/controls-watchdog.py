#!/usr/bin/env python3
"""One host-only NEW synthetic-control attempt; no Docker/corpus/tool invocation.

External 60s wall clock, 45s child CPU, 512MiB observed child RSS,
1MiB aggregate artifact budget and exact owned process-group teardown.
"""
import ast
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time

PARSER = Path('/private/tmp/s11-retained-ts-h264-nal-parser-20261002.py')
CONTROL = Path('/private/tmp/s11-ts-h264-nal-new-controls-20261002.py')
BUDGET = 1024 * 1024
CAPTURE = 512 * 1024


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_once(path, value):
    data = json.dumps(value, sort_keys=True, indent=2).encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)


def main():
    started = time.monotonic()
    root = Path(tempfile.mkdtemp(prefix='s11-nal-new-controls-', dir='/private/tmp'))
    parsed = ast.parse(CONTROL.read_text())
    ids = ['S11NALNewControls.' + method.name for node in parsed.body
           if isinstance(node, ast.ClassDef) and node.name == 'S11NALNewControls'
           for method in node.body if isinstance(method, ast.FunctionDef)
           and method.name.startswith('test_')]
    selected = sys.argv[1:] or ids
    if not selected or len(set(selected)) != len(selected) or not set(selected) <= set(ids):
        raise ValueError('unknown or duplicated NEW control ID')
    pins = {'parser_sha256': digest(PARSER), 'controls_sha256': digest(CONTROL),
            'watchdog_sha256': digest(Path(__file__))}
    write_once(root / 'initial-admission.json', {
        **pins, 'selected_new_ids': selected, 'input_budget_bytes': BUDGET,
        'artifact_aggregate_budget_bytes': BUDGET, 'wall_seconds': 60,
        'child_cpu_seconds': 45, 'child_rss_observed_cap_bytes': 512 * 1024 * 1024,
        'uid': os.getuid(), 'started_unix': time.time(),
        'surface': 'host-only NEW synthetic headers; no retained corpus/Docker/probes/decoder',
        'scope': 'syntax/header controls, not decodable pictures or original acceptance'})
    journal = root / 'journal.json'
    bootstrap = ('import resource,runpy,sys; '
                 'resource.setrlimit(resource.RLIMIT_CPU,(45,45)); '
                 'resource.setrlimit(resource.RLIMIT_FSIZE,(1048576,1048576)); '
                 'resource.setrlimit(resource.RLIMIT_NOFILE,(64,64)); '
                 'resource.setrlimit(resource.RLIMIT_CORE,(0,0)); '
                 'sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0],run_name="__main__")')
    env = {'PATH': '/usr/bin:/bin', 'S11_NAL_CONTROL_JOURNAL': str(journal)}
    child = subprocess.Popen([sys.executable, '-I', '-B', '-c', bootstrap,
                              str(CONTROL), *selected], env=env,
                             stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, start_new_session=True)
    write_once(root / 'launch.json', {'pid': child.pid, 'process_group': child.pid,
                                     'monotonic_launch': time.monotonic()})
    selector = selectors.DefaultSelector()
    captured = {'stdout': bytearray(), 'stderr': bytearray()}
    for name, stream in [('stdout', child.stdout), ('stderr', child.stderr)]:
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ, name)
    stopped = None
    peak_rss = 0
    next_rss = started
    try:
        while selector.get_map() or child.poll() is None:
            now = time.monotonic()
            if now - started >= 60:
                stopped = 'external 60s deadline'
                break
            if now >= next_rss and child.poll() is None:
                sample = subprocess.run(['/bin/ps', '-o', 'rss=', '-p', str(child.pid)],
                                        capture_output=True, timeout=1)
                raw = sample.stdout.strip()
                if raw:
                    peak_rss = max(peak_rss, int(raw) * 1024)
                    if peak_rss > 512 * 1024 * 1024:
                        stopped = 'observed child RSS cap'
                        break
                next_rss = now + 0.1
            for key, unused in selector.select(timeout=min(0.05, max(0, 60 - (now - started)))):
                value = os.read(key.fileobj.fileno(), 4096)
                if not value:
                    selector.unregister(key.fileobj)
                    continue
                captured[key.data].extend(value)
                if sum(len(x) for x in captured.values()) > CAPTURE:
                    stopped = 'aggregate stdout/stderr bound'
                    break
            # Admission+logs+journal+reserved final receipt are one aggregate budget.
            files = sum(p.stat().st_size for p in root.iterdir() if p.is_file())
            if files + sum(len(x) for x in captured.values()) + 65536 > BUDGET:
                stopped = 'aggregate artifact bound'
            if stopped:
                break
    except BaseException as error:
        stopped = 'external diagnostic exception: ' + type(error).__name__ + ': ' + str(error)
    finally:
        if stopped or child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
        code = child.wait(timeout=2)
        selector.close()
        child.stdout.close()
        child.stderr.close()
    for name, value in captured.items():
        with open(root / (name + '.txt'), 'xb') as stream:
            stream.write(value)
    # Bound the journal before parsing, including on unsuccessful inner exit.
    if journal.exists() and journal.stat().st_size <= 65536:
        outcome = json.loads(journal.read_bytes())
    else:
        outcome = {'successful_ids': [], 'missing_or_oversize_journal': True}
    try:
        os.killpg(child.pid, 0)
        group_absent = False
    except ProcessLookupError:
        group_absent = True
    write_once(root / 'terminal.json', {
        **pins, 'pid': child.pid, 'process_group': child.pid,
        'returncode': code, 'stopped': stopped, 'elapsed_seconds': time.monotonic() - started,
        'peak_child_rss_bytes_observed': peak_rss, 'process_group_absent': group_absent,
        'selected_new_ids': selected, 'outcome': outcome,
        'source_hashes_unchanged': pins['parser_sha256'] == digest(PARSER) and
                                 pins['controls_sha256'] == digest(CONTROL)})
    total = sum(p.stat().st_size for p in root.iterdir() if p.is_file())
    if total > BUDGET:
        raise RuntimeError('final aggregate artifact bound')
    for file in root.iterdir():
        file.chmod(0o400)
    print(json.dumps({'root': str(root), 'returncode': code, 'stopped': stopped,
                      'artifact_bytes': total, 'group_absent': group_absent,
                      'successful_ids': outcome.get('successful_ids', [])}, sort_keys=True))
    return 0 if code == 0 and stopped is None and group_absent else 1


if __name__ == '__main__':
    raise SystemExit(main())
