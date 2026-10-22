"""Pinned Ripwire over one materialized, visible-only study corpus.

The controller supplies verified installation bytes. No setup/download, ambient
Git history, credential environment, shell command, or candidate execution is
available through this tool. Every arm receives the same fresh backend.
"""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import selectors
import shutil
import signal
import stat
import subprocess
import tempfile
import time

from .evaluation import CaseSpec, EvaluationError, MAX_TOOL_BYTES

MODES = frozenset({'find', 'outline', 'body', 'callers', 'impact'})
LIMITATIONS = 'Heuristic navigation of visible corpus only; empty results never prove absence.'
MAX_VIEW_BYTES = 32 * 1024 * 1024
MAX_FILES = 512
OPERATIONAL = frozenset({'scripts/ripwire', 'scripts/ripwire.lock.json'})


def _require(ok, message):
    if not ok:
        raise EvaluationError(message)


def _json(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)


def _sha(raw):
    return hashlib.sha256(raw).hexdigest()


def _regular(path, limit):
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_CLOEXEC', 0)
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        _require(stat.S_ISREG(before.st_mode) and before.st_size <= limit, 'invalid navigator file')
        with os.fdopen(descriptor, 'rb', closefd=False) as stream:
            raw = stream.read(limit + 1)
        after = os.fstat(descriptor)
        _require(len(raw) <= limit and (before.st_ino, before.st_size, before.st_mtime_ns)
                 == (after.st_ino, after.st_size, after.st_mtime_ns), 'navigator file changed')
        return raw
    finally:
        os.close(descriptor)


@dataclass(frozen=True)
class NavigatorAssets:
    adapter: bytes
    lock: bytes
    binary: bytes
    installation: bytes
    expected_adapter_sha256: str
    expected_lock_sha256: str
    expected_binary_sha256: str

    def verify(self):
        for raw, expected in ((self.adapter, self.expected_adapter_sha256),
                              (self.lock, self.expected_lock_sha256),
                              (self.binary, self.expected_binary_sha256)):
            _require(isinstance(raw, bytes) and 0 < len(raw) <= 256 * 1024 * 1024
                     and _sha(raw) == expected, 'navigator asset identity mismatch')
        lock, installation = json.loads(self.lock), json.loads(self.installation)
        system = {'Darwin': 'macos', 'Linux': 'linux'}.get(platform.system())
        arch = {'arm64': 'arm64', 'aarch64': 'arm64', 'x86_64': 'x64'}.get(platform.machine())
        key = str(system) + '-' + str(arch)
        _require(lock.get('schema_version') == 1 and lock.get('upstream') == 'redhat-et/ripwire'
                 and lock.get('version') == '0.5.0' and key in lock.get('platforms', {}),
                 'unsupported navigator pin or platform')
        _require(installation.get('version') == lock['version']
                 and installation.get('platform') == key
                 and installation.get('archive_sha256') == lock['platforms'][key]['archive_sha256']
                 and installation.get('source_commit') == lock['source_commit']
                 and installation.get('binary_sha256') == self.expected_binary_sha256,
                 'navigator installation does not match release pin')
        return lock, key

    @property
    def digest(self):
        return _sha(_json({'adapter': self.expected_adapter_sha256, 'lock': self.expected_lock_sha256,
                           'binary': self.expected_binary_sha256,
                           'installation': _sha(self.installation)}).encode())


@dataclass(frozen=True)
class NavigationConfig:
    owned_root: Path
    assets: NavigatorAssets
    git_executable: Path
    python_executable: Path
    timeout_seconds: int = 20
    max_response_bytes: int = MAX_TOOL_BYTES

    def validate(self):
        root = Path(self.owned_root).absolute()
        _require(root.resolve() == root and root.is_dir() and not root.is_symlink(),
                 'navigator needs an existing non-symlink owned directory')
        _require('/.hermes/hdr-collaboration/workspaces/' in str(root)
                 or (str(root).startswith('/work/') and len(root.parts) >= 3),
                 'navigator requires durable agent-owned storage')
        _require(type(self.timeout_seconds) is int and 1 <= self.timeout_seconds <= 60,
                 'finite navigation deadline required')
        _require(type(self.max_response_bytes) is int and 512 <= self.max_response_bytes <= MAX_TOOL_BYTES,
                 'invalid navigation output budget')
        for path in (self.git_executable, self.python_executable):
            _require(Path(path).is_absolute() and Path(path).is_file() and os.access(path, os.X_OK),
                     'explicit tool executable required')
        self.assets.verify()
        return root


def _run(argv, root, environment, timeout, output_limit=128 * 1024):
    """Bound both streams and cancel the trusted wrapper through its handler."""
    process = subprocess.Popen(list(argv), cwd=root, env=environment, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    streams = [bytearray(), bytearray()]
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, 0)
    selector.register(process.stderr, selectors.EVENT_READ, 1)
    deadline, refused = time.monotonic() + timeout, None
    try:
        while selector.get_map():
            if time.monotonic() >= deadline:
                refused = 'deadline'
                break
            for entry, _ in selector.select(min(.1, max(0, deadline - time.monotonic()))):
                block = os.read(entry.fileobj.fileno(), 65536)
                if not block:
                    selector.unregister(entry.fileobj)
                    continue
                data = streams[entry.data]
                if len(data) + len(block) > output_limit:
                    refused = 'output-bound'
                    break
                data.extend(block)
            if refused:
                break
        if refused:
            raise EvaluationError('navigator ' + refused)
        process.wait(timeout=max(.1, deadline - time.monotonic()))
        return process.returncode, bytes(streams[0]), bytes(streams[1])
    finally:
        selector.close()
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()


def _visible(case):
    _require(isinstance(case, CaseSpec) and 0 < len(case.files) <= MAX_FILES, 'invalid navigation corpus')
    files, total = {}, 0
    for file in case.files:
        path = PurePosixPath(file.path)
        _require(file.path not in OPERATIONAL and path.parts[0] not in {'.git', 'target'}
                 and all(part not in {'.git', '..'} for part in path.parts),
                 'visible source collides with navigation control files')
        lines = file.content.splitlines(keepends=True)
        # Re-project even already-redacted CaseSpec bytes before an external index.
        text = ''.join(line if any(start <= number <= end for start, end in file.visible_line_ranges)
                       else ('\n' if line.endswith('\n') else '')
                       for number, line in enumerate(lines, 1))
        raw = text.encode('utf-8')
        total += len(raw)
        _require(total <= MAX_VIEW_BYTES, 'navigation corpus exceeds byte bound')
        files[file.path] = raw
    return files


class VisibleRipwire:
    """Fresh per run; index caches warm only within this one solver session."""

    def __init__(self, config: NavigationConfig, case: CaseSpec):
        owned = config.validate()
        self.config, self.root, self.closed = config, None, False
        self.audit = {'assets_sha256': config.assets.digest, 'queries': [],
                      'cache_policy': 'fresh-session; within-session wrapper cache',
                      'source_identity_kind': 'synthetic visible-only Git, not frozen source HEAD'}
        self.files = _visible(case)
        self.audit['view_sha256'] = _sha(_json({k: _sha(v) for k, v in sorted(self.files.items())}).encode())
        self.root = Path(tempfile.mkdtemp(prefix='navigation-run-', dir=owned))
        self.root.chmod(0o700)
        try:
            lock, key = config.assets.verify()
            target = self.root / 'target'
            for child in ('home', 'scratch', 'hooks', 'template'):
                (target / child).mkdir(parents=True, mode=0o700)
            self.environment = {'PATH': str(Path(config.git_executable).parent) + os.pathsep
                                + str(Path(config.python_executable).parent) + os.pathsep + '/usr/bin:/bin',
                                'HOME': str(target / 'home'), 'TMPDIR': str(target / 'scratch'),
                                'LANG': 'C', 'LC_ALL': 'C', 'PYTHONUTF8': '1',
                                'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
                                'GIT_TERMINAL_PROMPT': '0', 'PYTHONDONTWRITEBYTECODE': '1',
                                'PYTHONNOUSERSITE': '1', 'PYTHONHASHSEED': '0',
                                'GIT_AUTHOR_NAME': 'Corpus', 'GIT_AUTHOR_EMAIL': 'corpus@invalid',
                                'GIT_COMMITTER_NAME': 'Corpus', 'GIT_COMMITTER_EMAIL': 'corpus@invalid',
                                'GIT_AUTHOR_DATE': '2000-01-01T00:00:00+00:00',
                                'GIT_COMMITTER_DATE': '2000-01-01T00:00:00+00:00'}
            self.controls = dict(self.files)
            self.controls.update({'scripts/ripwire': config.assets.adapter,
                                  'scripts/ripwire.lock.json': config.assets.lock})
            for name, raw in self.controls.items():
                dest = self.root / name
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(raw)
                dest.chmod(0o400)
            install = target / 'ripwire/tools' / lock['version'] / key
            install.mkdir(parents=True)
            (install / 'ripwire').write_bytes(config.assets.binary)
            (install / 'ripwire').chmod(0o500)
            (install / 'install.json').write_bytes(config.assets.installation)
            (install / 'install.json').chmod(0o400)
            self.install = install
            self._git('-c', 'init.defaultBranch=corpus', 'init', '--template=' + str(target / 'template'), '.')
            (self.root / '.git/info').mkdir(exist_ok=True)
            (self.root / '.git/info/exclude').write_text('/scripts/ripwire\n/scripts/ripwire.lock.json\n/target/\n')
            self._git('config', 'core.hooksPath', str(target / 'hooks'))
            self._git('add', '-f', '--', *sorted(self.files))
            self._git('commit', '-q', '-m', 'Visible corpus only')
            self.audit['synthetic_head'] = self._git('rev-parse', 'HEAD').strip()
            self.audit['synthetic_tree'] = self._git('rev-parse', 'HEAD^{tree}').strip()
            self._verify()
        except BaseException:
            self.close()
            raise

    def _git(self, *args):
        code, out, _ = _run([str(self.config.git_executable), *args], self.root,
                            self.environment, self.config.timeout_seconds)
        _require(code == 0, 'synthetic Git operation failed')
        return out.decode('utf-8')

    def _verify(self):
        _require(not self.closed and self.root.is_dir() and not self.root.is_symlink(), 'navigator closed')
        found = set()
        for base, dirs, files in os.walk(self.root, followlinks=False):
            for directory in list(dirs):
                path = Path(base) / directory
                _require(not path.is_symlink(), 'symlink in navigator view')
                if Path(base) == self.root and directory in {'.git', 'target'}:
                    dirs.remove(directory)
            for name in files:
                path = Path(base) / name
                rel = path.relative_to(self.root).as_posix()
                _require(rel in self.controls and _regular(path, MAX_VIEW_BYTES) == self.controls[rel],
                         'navigator source changed or unexpected member')
                found.add(rel)
        _require(found == self.controls.keys(), 'navigator member removed')
        _require(_sha(_regular(self.install / 'ripwire', 256 * 1024 * 1024))
                 == self.config.assets.expected_binary_sha256
                 and _regular(self.install / 'install.json', 65536) == self.config.assets.installation,
                 'navigator tool changed')
        _require(self._git('rev-parse', 'HEAD').strip() == self.audit['synthetic_head'],
                 'navigator Git identity changed')

    def __call__(self, mode, selector):
        _require(mode in MODES and isinstance(selector, str) and selector
                 and len(selector.encode('utf-8')) <= 4096
                 and not any(c in selector for c in ('\0', '\r', '\n')), 'invalid navigator query')
        _require(not selector.startswith(('/', '~', '\\')) and '://' not in selector
                 and not any(part in {'.git', '..'} for part in selector.replace('\\', '/').split('/')),
                 'outside-corpus navigator selector')
        if mode != 'find':
            _require(':' in selector and selector.split(':', 1)[0] in self.files,
                     'navigator requires a visible file:symbol selector')
        self._verify()
        argv = [str(self.config.python_executable), str(self.root / 'scripts/ripwire'), mode,
                '--timeout', str(self.config.timeout_seconds), '--', selector]
        started = time.monotonic()
        try:
            code, out, err = _run(argv, self.root, self.environment, self.config.timeout_seconds + 10)
            receipt = json.loads(err.splitlines()[0])
            _require(receipt.get('binary_sha256') == self.config.assets.expected_binary_sha256
                     and receipt.get('head') == self.audit['synthetic_head']
                     and receipt.get('root') == str(self.root), 'navigator receipt mismatch')
            self._verify()
            self.audit['queries'].append({'mode': mode, 'receipt': receipt,
                                          'stdout_sha256': _sha(out), 'stderr_sha256': _sha(err),
                                          'elapsed_seconds': time.monotonic() - started})
            if code != 0:
                return {'status': 'unavailable', 'mode': mode, 'text': '', 'coverage': LIMITATIONS,
                        'exit': code, 'reason': 'pinned navigator refused'}
            text = out.decode('utf-8').replace(str(self.root), '<visible-corpus>')
            result = {'status': 'ok', 'mode': mode, 'text': text, 'omitted_utf8_bytes': 0,
                      'coverage': LIMITATIONS}
            if len(_json(result).encode()) <= self.config.max_response_bytes:
                return result
            raw, lower, upper = text.encode(), 0, len(text)
            result['status'] = 'partial'
            while lower < upper:
                middle = (lower + upper + 1) // 2
                result.update(text=text[:middle], omitted_utf8_bytes=len(raw) - len(text[:middle].encode()))
                if len(_json(result).encode()) <= self.config.max_response_bytes:
                    lower = middle
                else:
                    upper = middle - 1
            result.update(text=text[:lower], omitted_utf8_bytes=len(raw) - len(text[:lower].encode()))
            _require(len(_json(result).encode()) <= self.config.max_response_bytes, 'navigator envelope too large')
            return result
        except (UnicodeError, json.JSONDecodeError, IndexError) as error:
            raise EvaluationError('invalid navigator response') from error

    def close(self):
        if self.closed:
            return
        if self.root is not None and self.root.exists():
            _require(self.root.parent == Path(self.config.owned_root).absolute()
                     and self.root.name.startswith('navigation-run-') and not self.root.is_symlink(),
                     'navigator cleanup root mismatch')
            # Keep only source commitments and query receipts; tool text lives in
            # the caller's private RunLog and is never copied into this receipt.
            audit = Path(self.config.owned_root) / 'audit'
            audit.mkdir(mode=0o700, exist_ok=True)
            _require(not audit.is_symlink(), 'navigator audit path must be owned')
            record = audit / (self.root.name + '.json')
            descriptor = os.open(record, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, 'w') as stream:
                stream.write(_json(self.audit) + '\n')
            shutil.rmtree(self.root)
        self.closed = True


@dataclass(frozen=True)
class VisibleRipwireFactory:
    """Concrete common-tool factory, frozen alongside the study manifest."""

    config: NavigationConfig

    def __post_init__(self):
        self.config.validate()

    @property
    def identity(self):
        return {'schema_version': 1, 'navigator': 'redhat-et/ripwire', 'version': '0.5.0',
                'assets_sha256': self.config.assets.digest,
                'timeout_seconds': self.config.timeout_seconds,
                'max_response_bytes': self.config.max_response_bytes,
                'cache_policy': 'fresh-session; within-session wrapper cache',
                'modes': sorted(MODES), 'visible_only': True}

    def __call__(self, spec, case):
        _require(spec.case_id == case.id, 'navigator run/case identity mismatch')
        return VisibleRipwire(self.config, case)
