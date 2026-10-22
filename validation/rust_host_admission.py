"""Fail-closed admission for cooperative M3 pilots sharing one Docker daemon.

This does not reserve resources against other workflows, repositories or VMs.
A roster is eligibility metadata, not a measurement of current host capacity.
"""
from __future__ import annotations

from contextlib import contextmanager
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import time
import tomllib

from validation.baseline_resources import discover_cgroup

GIB = 1024 ** 3
VOLUME = 'plurx-rust-pilot-admission'
LOCK = 'rust-pilot.lock'  # Never unlink: replacement creates independent locks.


class AdmissionError(ValueError):
    """The current execution boundary cannot establish safe pilot admission."""


def _unescape(value):
    return re.sub(r'\\([0-7]{3})', lambda match: chr(int(match[1], 8)), value)


def validate_directory(directory, *, repository=None, proc_root=Path('/proc'), volume_name=VOLUME):
    """Require an existing distinct RW Docker named-volume mount, not a checkout."""
    directory = Path(directory)
    if not directory.is_absolute() or not directory.is_dir():
        raise AdmissionError('admission directory must already exist as an absolute directory')
    if directory.is_symlink():
        raise AdmissionError('admission directory must not be a symlink')
    directory = directory.resolve()
    repository = Path(repository or Path.cwd()).resolve()
    if directory == repository or repository in directory.parents or directory in repository.parents:
        raise AdmissionError('admission directory must be separate from the checkout')
    if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_.-]*', volume_name):
        raise AdmissionError('invalid named volume identity')
    records = []
    try:
        for line in (Path(proc_root) / 'self/mountinfo').read_text().splitlines():
            left, right = line.split(' - ', 1)
            fields, tail = left.split(), right.split()
            if len(fields) >= 6 and len(tail) >= 3 and _unescape(fields[4]) == str(directory):
                records.append((fields, tail))
    except (OSError, ValueError) as error:
        raise AdmissionError('cannot inspect admission mount') from error
    if len(records) != 1:
        raise AdmissionError('admission directory must be one exact mount, not an ephemeral subdirectory')
    fields, tail = records[0]
    if not _unescape(fields[3]).endswith('/volumes/' + volume_name + '/_data'):
        raise AdmissionError('admission mount does not identify the configured Docker named volume')
    if 'rw' not in fields[5].split(',') or tail[0] in ('tmpfs', 'overlay', 'ramfs'):
        raise AdmissionError('admission mount must be persistent and writable')
    return {'volume': volume_name, 'mount_id': fields[0],
            'scope': 'cooperative-pilots-on-one-docker-daemon-only'}


def _write_marker(fd, state, reason=None):
    value = {'schema': 1, 'state': state}
    if reason is not None:
        value['reason'] = reason
    content = (json.dumps(value, sort_keys=True) + '\n').encode()
    # Keep one inode/open file description. A partial write is nonempty and
    # therefore remains fenced; it cannot be interpreted as a healthy lease.
    os.ftruncate(fd, len(content))
    offset = 0
    while offset < len(content):
        written = os.pwrite(fd, content[offset:], offset)
        if written <= 0:
            raise OSError('admission marker write made no progress')
        offset += written
    os.fsync(fd)


def quarantine(lease, reason):
    """Fence this lease after unqualified cleanup; never automatically recover.

    Callers supply a bounded diagnostic reason, without credentials or private
    infrastructure. External teardown proof and explicit manual recovery are
    required before a subsequent pilot may use this persistent lock inode.
    """
    lease['cleanup_qualified'] = False
    if not isinstance(reason, str) or not reason.strip() or len(reason) > 500:
        raise AdmissionError('quarantine requires a bounded diagnostic reason')
    try:
        _write_marker(lease['lock_fd'], 'QUARANTINED', reason)
    except (OSError, KeyError, TypeError) as error:
        raise AdmissionError('cannot persist quarantine; active admission fence must remain') from error


@contextmanager
def acquire(directory, timeout=60, *, repository=None, proc_root=Path('/proc'), volume_name=VOLUME):
    """Hold a persistent kernel lock for the entire owned execution/cleanup.

    The caller must join all children before normal return, and quarantine
    the lease if cleanup is unqualified. Exceptions/controller loss retain a
    persistent fence even after the kernel lock is released. Inherited child
    descriptors additionally keep the kernel lock until those children exit.
    There is deliberately no automatic fence-clearing/recovery API.
    """
    if not math.isfinite(timeout) or timeout < 0:
        raise AdmissionError('lock timeout must be finite and nonnegative')
    metadata = validate_directory(directory, repository=repository, proc_root=proc_root, volume_name=volume_name)
    try:
        directory_fd = os.open(Path(directory).resolve(), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except OSError as error:
        raise AdmissionError('admission directory is not safely accessible') from error
    fd = None
    try:
        fd = os.open(LOCK, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600, dir_fd=directory_fd)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise AdmissionError('admission lock must be one regular non-linked file')
        deadline = time.monotonic() + timeout
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AdmissionError('timed out waiting for cooperative pilot admission')
                time.sleep(min(remaining, 0.05))
        if os.fstat(fd).st_size != 0:
            raise AdmissionError('admission is fenced; independently confirm teardown before manual recovery')
        _write_marker(fd, 'ACTIVE')
        lease = {**metadata, 'lock_fd': fd, 'lock_device': info.st_dev,
                 'lock_inode': info.st_ino, 'cleanup_qualified': True}
        # Code after yield runs only on normal body return. BaseException,
        # including cancellation/interrupt, leaves ACTIVE intact.
        yield lease
        if lease.get('cleanup_qualified') is True:
            try:
                os.ftruncate(fd, 0)
                os.fsync(fd)
            except OSError:
                # A failed healthy-clear must restore a nonempty fence. If
                # storage refuses that too, propagate rather than claim health.
                _write_marker(fd, 'ACTIVE', 'qualified-clear failed')
                raise
    except OSError as error:
        raise AdmissionError('admission lock is not safely writable') from error
    finally:
        # Close instead of LOCK_UN: an inherited child lease must remain held.
        if fd is not None:
            os.close(fd)
        os.close(directory_fd)


def inspect_environment(runner_name, repository, directory, *, proc_root=Path('/proc'),
                        max_cpu=12, max_memory_bytes=15 * GIB, reserve_bytes=8 * GIB,
                        max_pids=4096, volume_name=VOLUME):
    """Observe eligibility and actual local cgroup-v2 caps without changing them.

    MemAvailable is a point-in-time procfs observation, not a reservation or a
    claim about unobserved ancestor limits/other jobs/physical-host topology.
    """
    if (not math.isfinite(max_cpu) or max_cpu <= 0
            or any(type(value) is not int or value <= 0
                   for value in (max_memory_bytes, reserve_bytes, max_pids))):
        raise AdmissionError('resource policy bounds must be positive and finite')
    repository, proc_root = Path(repository), Path(proc_root)
    roster_path = repository / 'validation/runner-fleet.toml'
    try:
        source = roster_path.read_bytes()
        roster = tomllib.loads(source.decode())
        runners = [row for row in roster['runners'] if row['name'] == runner_name]
        if len(runners) != 1:
            raise AdmissionError('runner identity is absent or ambiguous in the roster')
        runner = runners[0]
        required = {'lab', 'general', 'high-cpu', 'baremetal'}
        labels = set(runner['labels'])
        if (runner['os'] != 'Linux' or runner['arch'] != 'X64' or not required <= labels
                or labels & {'android-kvm', 'apple', 'ci-coverage', 'laptop-vm'}
                or runner['host'] in roster['voter_hosts']):
            raise AdmissionError('runner is not an eligible non-voter baremetal general pilot host')
        roots = discover_cgroup(proc_root)
        root = roots.get('v2')
        if root is None:
            raise AdmissionError('current cgroup-v2 boundary is unavailable')
        quota, period = (root / 'cpu.max').read_text().split()
        memory = int((root / 'memory.max').read_text().strip())
        swap = int((root / 'memory.swap.max').read_text().strip())
        pids = int((root / 'pids.max').read_text().strip())
        cpus = int(quota) / int(period)
        if (min(int(quota), int(period), memory, pids) <= 0 or cpus > max_cpu
                or memory > max_memory_bytes or pids > max_pids or swap != 0):
            raise AdmissionError('actual job cgroup exceeds the configured CPU/memory/pid bounds')
        matches = re.findall(r'^MemAvailable:\s+(\d+) kB$', (proc_root / 'meminfo').read_text(), re.M)
        if len(matches) != 1:
            raise AdmissionError('MemAvailable observation is unavailable or ambiguous')
        available = int(matches[0]) * 1024
        if available < memory + reserve_bytes:
            raise AdmissionError('observed MemAvailable does not preserve the requested reserve')
    except (OSError, KeyError, TypeError, ValueError, ZeroDivisionError) as error:
        if isinstance(error, AdmissionError):
            raise
        raise AdmissionError('eligibility/resource evidence is missing, unlimited or malformed') from error
    mount = validate_directory(directory, repository=repository, proc_root=proc_root, volume_name=volume_name)
    return {**mount, 'runner': runner_name, 'host': runner['host'],
            'roster_sha256': hashlib.sha256(source).hexdigest(),
            'cpu_limit': cpus, 'memory_limit_bytes': memory, 'pids_limit': pids,
            'swap_limit_bytes': swap,
            'observed_mem_available_bytes': available, 'required_reserve_bytes': reserve_bytes,
            'other_workflows_reserved': False, 'live_fleet_verified': False}
