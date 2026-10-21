"""Opt-in local OCI boundary for private acceptance callbacks.

Nothing launches or imports a container engine at module import. The custodian
supplies a pinned, trusted Linux image and exact callback argv. Raw observations
and output stay in the owned private directory; public results expose only a
status and digest. Missing or unproved isolation is unavailable qualification.
"""
from __future__ import annotations

from dataclasses import dataclass
import base64
import errno
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import time
from typing import Any
import uuid


class IsolationError(ValueError):
    """Unsafe or malformed configuration; no acceptance may execute."""


def _require(condition, message):
    if not condition:
        raise IsolationError(message)


def _json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def _digest(value):
    return hashlib.sha256(_json(value).encode()).hexdigest()


def _bytes_digest(value):
    return hashlib.sha256(value).hexdigest()


@dataclass(frozen=True)
class ReadonlyCache:
    source: Path
    target: str


@dataclass(frozen=True)
class TrustedSourceFile:
    """Exact tracked archive member declared by the trusted source stager."""
    path: str
    sha256: str


@dataclass(frozen=True)
class IsolationConfig:
    engine: Path
    endpoint: str
    image: str
    owned_root: Path
    candidate_root: Path
    grader_root: Path
    runtime_root: Path
    acceptance_argv: tuple[tuple[str, ...], ...]
    python_executable: str = "/usr/bin/python3"
    rustc_executable: str = "/usr/bin/rustc"
    cargo_executable: str = "/usr/bin/cargo"
    require_rust: bool = False
    readonly_caches: tuple[ReadonlyCache, ...] = ()
    environment: tuple[tuple[str, str], ...] = ()
    timeout_seconds: int = 300
    max_output_bytes: int = 1024 * 1024
    memory_bytes: int = 1024 * 1024 * 1024
    cpus: float = 1.0
    pids_limit: int = 64
    uid: int = 65534
    gid: int = 65534
    source_commit: str = ""
    source_archive_sha256: str = ""
    trusted_source_files: tuple[TrustedSourceFile, ...] = ()
    trusted_wrappers: tuple[TrustedSourceFile, ...] = ()
    runtime_max_bytes: int = 32 * 1024**3
    cache_max_bytes: int = 32 * 1024**3

    def __post_init__(self):
        for name in ("engine", "owned_root", "candidate_root", "grader_root", "runtime_root"):
            object.__setattr__(self, name, Path(getattr(self, name)))
        object.__setattr__(self, "engine", self.engine.resolve())
        object.__setattr__(self, "trusted_source_files", tuple(self.trusted_source_files))
        object.__setattr__(self, "trusted_wrappers", tuple(self.trusted_wrappers))
        object.__setattr__(self, "acceptance_argv", tuple(tuple(argv) for argv in self.acceptance_argv))
        object.__setattr__(self, "environment", tuple(tuple(pair) for pair in self.environment))
        object.__setattr__(self, "readonly_caches", tuple(self.readonly_caches))


@dataclass(frozen=True)
class FamilyPlan:
    family: str
    prerequisite_argv: tuple[tuple[str, ...], ...]
    acceptance_argv: tuple[tuple[str, ...], ...]
    driver: TrustedSourceFile | None = None

    def __post_init__(self):
        for name in ("prerequisite_argv", "acceptance_argv"):
            object.__setattr__(self, name, tuple(tuple(argv) for argv in getattr(self, name)))


@dataclass(frozen=True)
class PrerequisiteReceipt:
    status: str
    digest: str
    private_path: Path | None
    family: str
    binding_digest: str

    def public(self):
        return {"status": self.status, "digest": self.digest}


@dataclass(frozen=True)
class ProbeReceipt:
    status: str
    digest: str
    private_path: Path | None
    config_digest: str

    def public(self):
        return {"status": self.status, "digest": self.digest}


@dataclass(frozen=True)
class ExecutionResult:
    status: str
    digest: str
    private_path: Path | None
    classification: str = "unavailable"
    prerequisite_digest: str = ""

    def public(self):
        return {"status": self.status, "digest": self.digest}


@dataclass(frozen=True)
class _Command:
    returncode: int | None
    stdout: bytes
    stderr: bytes
    timed_out: bool = False
    output_limited: bool = False


@dataclass(frozen=True)
class _Cleanup:
    proved: bool
    observations: dict[str, Any]

    def __bool__(self):
        return self.proved


# Only exact receipts issued by successful probes in this process are accepted.
# A copied JSON object, arbitrary booleans or a recomputed public hash cannot
# manufacture an attestation or revive one after a custodian process restart.
_ISSUED: dict[int, tuple[ProbeReceipt, dict[str, Any]]] = {}
_OWNED_CONTAINERS: set[str] = set()
_QUALIFIED: dict[int, tuple[PrerequisiteReceipt, dict[str, Any]]] = {}
_IMAGE = re.compile(r"(?:[a-z0-9][a-z0-9._:/-]*@)?sha256:[a-f0-9]{64}\Z")
# These are tracked Rust sources, never credential stores. Adding an exception
# or wrapper family requires a source change and review, not a model tool flag.
_SOURCE_EXCEPTIONS = frozenset({"crates/plurx-core/src/secrets.rs", "crates/plurx-compat-jellyfin/src/credentials.rs"})
_WRAPPERS = frozenset({"scripts/quiet"})
_SUPERVISOR = "import time; time.sleep(86400)"
_PROBE_CODE = r'''
import errno,json,os,platform,socket,subprocess,sys
sentinel, rustc, cargo, rust_required = sys.argv[1:]
filesystem={}
for label,directory in (("candidate","/work"),("runtime","/work/.runtime/tmp"),("grader","/grader")):
    path=directory+"/.isolation-write-probe-"+str(os.getpid())
    try:
        descriptor=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        os.write(descriptor,b"probe")
        os.close(descriptor)
        os.unlink(path)
        filesystem[label]={"writable":True,"errno":0}
    except OSError as error:
        filesystem[label]={"writable":False,"errno":error.errno}
try:
    with open(sentinel,"rb") as stream:
        stream.read(1)
    outside = {"readable":True,"errno":0}
except OSError as error:
    outside = {"readable":False,"errno":error.errno}
sock=socket.socket(socket.AF_INET,socket.SOCK_STREAM)
sock.settimeout(1)
try:
    network_errno=sock.connect_ex(("1.1.1.1",53))
except OSError as error:
    network_errno=error.errno
finally:
    sock.close()
versions={"python":platform.python_version(),"python_executable":sys.executable,"platform":platform.platform(),"machine":platform.machine()}
if rust_required == "yes":
    for name,path in (("rustc",rustc),("cargo",cargo)):
        outcome=subprocess.run([path,"--version"],cwd="/",stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
        versions[name]={"returncode":outcome.returncode,"output":outcome.stdout.decode("utf-8","replace").strip()}
print(json.dumps({"outside":outside,"network":{"interfaces":sorted(name for _,name in socket.if_nameindex()),"errno":network_errno},"versions":versions,"filesystem":filesystem},sort_keys=True))
'''


_OUTCOME_CONTROLLER = r'''
import base64,contextlib,ctypes,hashlib,hmac,json,os,sys
source,digest,mode,argv,context,limit=sys.argv[1:]
argv=json.loads(argv);context=json.loads(context);limit=int(limit)
auth=json.loads(sys.stdin.buffer.read(4097));secret=bytes.fromhex(auth["secret"])
def canonical(value):
    return json.dumps(value,sort_keys=True,separators=(",",":"),allow_nan=False)
def resources():
    result={}
    try:
        for name in ("memory.events","pids.events"):
            with open("/sys/fs/cgroup/"+name) as stream:
                result[name]={key:int(value) for key,value in (line.split() for line in stream)}
    except (OSError,ValueError):
        return None
    return result
captured_bytes=0
class Bounded:
    def __init__(self):self.raw=bytearray()
    def write(self,value):
        global captured_bytes
        value=value.encode("utf-8","replace")
        if captured_bytes+len(value)>limit:raise RuntimeError("driver output bound")
        captured_bytes+=len(value)
        self.raw.extend(value);return len(value)
    def flush(self):pass
out=Bounded();err=Bounded();started=completed=hardened=False
result={"protocol":1,"outcome":"unavailable","completed":False}
before=resources()
try:
    libc=ctypes.CDLL(None,use_errno=True)
    hardened=libc.prctl(4,0,0,0,0)==0 and libc.prctl(3,0,0,0,0)==0
    if not hardened:raise RuntimeError("controller protection unavailable")
    with open(source,"rb") as stream:code=stream.read(8*1024*1024+1)
    if len(code)>8*1024*1024 or hashlib.sha256(code).hexdigest()!=digest:raise RuntimeError("driver identity changed")
    namespace={"__name__":"_trusted_family_driver","__file__":source}
    with contextlib.redirect_stdout(out),contextlib.redirect_stderr(err):
        exec(compile(code,source,"exec"),namespace)
        started=True
        result=namespace["run"](mode,argv,context)
        completed=True
    if len(canonical(result))>8192:raise RuntimeError("driver result bound")
except BaseException:
    result={"protocol":1,"outcome":"unavailable","completed":False}
after=resources()
verified=before is not None and after is not None
failure=not verified
if verified:
    failure=any(after[name].get(key,0)>before[name].get(key,0) for name,keys in
        (("memory.events",("oom","oom_kill","max")),("pids.events",("max",))) for key in keys)
body={"nonce":auth["nonce"],"argv":argv,"mode":mode,"context":context,"driver_sha256":digest,
      "driver_started":started,"driver_completed":completed,"hardened":hardened,
      "resources_verified":verified,"resource_failure":failure,"resources_before":before,"resources_after":after,
      "result":result,"stdout_base64":base64.b64encode(out.raw).decode(),"stderr_base64":base64.b64encode(err.raw).decode()}
print(canonical({"body":body,"hmac":hmac.new(secret,canonical(body).encode(),hashlib.sha256).hexdigest()}))
'''

def _no_symlinks(path: Path, *, directory=False) -> Path:
    _require(path.is_absolute() and "," not in str(path) and "\n" not in str(path), "absolute literal path required")
    current = Path(path.anchor)
    for part in path.parts[1:]:
        _require(part not in {".", ".."}, "path traversal refused")
        current /= part
        try:
            _require(not stat.S_ISLNK(current.lstat().st_mode), "symlink path refused")
        except FileNotFoundError:
            raise IsolationError("required owned path unavailable") from None
    _require(path.is_dir() if directory else path.is_file(), "required path type unavailable")
    return path


def _private_name(name: str):
    name = name.lower()
    return (name in {".git", ".aws", ".ssh", ".codex", ".hermes", ".docker", ".cargo", ".isolation", "evaluation-private", "gold", "gold.json", "auth.json"}
            or name.startswith((".env", "credentials", "secret")) or name.endswith((".pem", ".key", ".fleet.json")))


def _read_member(root: Path, path: Path, expected, *, max_bytes=512 * 1024 * 1024):
    """Open through no-follow directory descriptors, including race windows."""
    parts = path.relative_to(root).parts
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        descriptor = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            _require(stat.S_ISREG(before.st_mode) and before.st_size <= max_bytes, "bounded regular mount file required")
            digest = hashlib.sha256()
            count = 0
            while chunk := stream.read(1024 * 1024):
                count += len(chunk)
                _require(count <= max_bytes, "mount byte bound exceeded during capture")
                digest.update(chunk)
            after = os.fstat(stream.fileno())
        current = os.stat(parts[-1], dir_fd=directory, follow_symlinks=False)
    finally:
        os.close(directory)
    def identity(value):
        return (value.st_dev, value.st_ino, value.st_size, value.st_mode, value.st_mtime_ns, value.st_ctime_ns)
    _require(identity(expected) == identity(before) == identity(after) == identity(current), "mount source changed during capture")
    return digest.hexdigest()


def _manifest(root: Path, *, candidate=False, max_bytes=512 * 1024 * 1024, exceptions=None):
    rows = []
    total = 0
    members = 0
    exceptions = exceptions or {}
    seen_exceptions = set()
    for directory, folders, files in os.walk(root, followlinks=False):
        if candidate and Path(directory) == root and ".runtime" in folders:
            runtime = root / ".runtime"
            _require(not runtime.is_symlink() and not any(runtime.iterdir()), "candidate runtime mountpoint must be empty")
            folders.remove(".runtime")
        for name in sorted([*folders, *files]):
            members += 1
            _require(members <= 100000, "mount path count bound exceeded")
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            permitted_source = candidate and relative in exceptions and relative in _SOURCE_EXCEPTIONS
            _require((not _private_name(name) or permitted_source) and not path.is_symlink(), "private or symlink mount member refused")
            info = path.lstat()
            _require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode), "special mount member refused")
            if stat.S_ISREG(info.st_mode):
                total += info.st_size
                _require(total <= max_bytes and len(rows) < 100000, "mount corpus bound exceeded")
                digest = _read_member(root, path, info, max_bytes=max_bytes)
                if relative in exceptions:
                    _require(digest == exceptions[relative], "trusted source exception content changed")
                    seen_exceptions.add(relative)
                rows.append((relative, stat.S_IMODE(info.st_mode), digest))
    _require(seen_exceptions == set(exceptions), "trusted source exception missing")
    return _digest(sorted(rows))


def _container_executable(path):
    _require(isinstance(path, str) and path.startswith(("/usr/", "/opt/"))
             and ".." not in path.split("/") and "\0" not in path, "runtime must be a fixed image executable")


def _prepare(config: IsolationConfig):
    _require(isinstance(config, IsolationConfig), "typed isolation config required")
    root = _no_symlinks(config.owned_root, directory=True)
    marker = "/.hermes/hdr-collaboration/workspaces/"
    _require((marker in str(root) and str(root).split(marker, 1)[1])
             or (str(root).startswith("/work/") and len(root.parts) >= 3), "dedicated durable agent root required")
    _require(not any(part in {"tmp", "private", "var"} for part in root.parts[1:2]), "temporary/broad root refused")
    mounts = (config.candidate_root, config.grader_root, config.runtime_root, *(Path(cache.source) for cache in config.readonly_caches))
    for path in mounts:
        _no_symlinks(path, directory=True)
        _require(path != root and path.is_relative_to(root), "mount must be a dedicated owned child")
        _require(not any(_private_name(part) for part in path.relative_to(root).parts), "private mount root refused")
    for n, path in enumerate(mounts):
        _require(all(not path.is_relative_to(other) and not other.is_relative_to(path) for other in mounts[n + 1:]), "overlapping mount roots refused")
    _require(_IMAGE.fullmatch(config.image), "Linux image must be digest pinned")
    _require(config.endpoint.startswith("unix:///") and "\n" not in config.endpoint and "\0" not in config.endpoint, "local Unix engine endpoint required")
    _require(type(config.uid) is int and type(config.gid) is int and 0 < config.uid < 2**31 and 0 < config.gid < 2**31, "unprivileged numeric user required")
    for value, lower, upper in ((config.timeout_seconds, 1, 1800), (config.max_output_bytes, 1024, 16 * 1024 * 1024),
                                (config.memory_bytes, 64 * 1024 * 1024, 16 * 1024**3), (config.pids_limit, 8, 256),
                                (config.runtime_max_bytes, 1, 128 * 1024**3), (config.cache_max_bytes, 1, 128 * 1024**3)):
        _require(type(value) is int and lower <= value <= upper, "finite execution bounds required")
    _require(type(config.cpus) in {int, float} and 0.1 <= config.cpus <= 16, "finite CPU bound required")
    _require(type(config.require_rust) is bool, "explicit Rust requirement required")
    for path in (config.python_executable, config.rustc_executable, config.cargo_executable):
        _container_executable(path)
    trusted = (*config.trusted_source_files, *config.trusted_wrappers)
    if trusted:
        _require(re.fullmatch(r"(?:[a-f0-9]{40}|[a-f0-9]{64})", config.source_commit)
                 and re.fullmatch(r"[a-f0-9]{64}", config.source_archive_sha256), "verified source archive provenance required")
    _require(len(config.trusted_source_files) <= len(_SOURCE_EXCEPTIONS) and len(config.trusted_wrappers) <= len(_WRAPPERS), "trusted source allowlist bound exceeded")
    exceptions = {}
    wrappers = {}
    for entries, allowed, destination in ((config.trusted_source_files, _SOURCE_EXCEPTIONS, exceptions),
                                          (config.trusted_wrappers, _WRAPPERS, wrappers)):
        for entry in entries:
            _require(isinstance(entry, TrustedSourceFile) and entry.path in allowed
                     and re.fullmatch(r"[a-f0-9]{64}", entry.sha256) and entry.path not in destination, "exact trusted source member required")
            destination[entry.path] = entry.sha256
    wrapper_commands = {"/work/" + relative for relative in wrappers}
    for relative, expected in wrappers.items():
        path = config.candidate_root / relative
        _no_symlinks(path)
        info = path.lstat()
        _require(info.st_mode & 0o111 and _read_member(config.candidate_root, path, info) == expected, "trusted wrapper content/mode changed")
    _require(0 < len(config.acceptance_argv) <= 32, "fixed acceptance argv required")
    for argv in config.acceptance_argv:
        _require(0 < len(argv) <= 256 and argv[0] in {config.python_executable, config.cargo_executable, config.rustc_executable} | wrapper_commands, "trusted runtime callback required")
        _require(all(isinstance(arg, str) and "\0" not in arg for arg in argv) and sum(map(len, argv)) <= 32768, "bounded literal argv required")
        _require(config.require_rust or argv[0] == config.python_executable, "Rust callbacks require pinned Rust verification")
        if argv[0] in wrapper_commands:
            _require(len(argv) >= 2 and argv[1] == config.cargo_executable, "trusted wrapper must invoke the fixed Cargo executable")
    _require(len(config.readonly_caches) <= 8, "readonly cache count bound exceeded")
    targets = set()
    for cache in config.readonly_caches:
        _require(isinstance(cache, ReadonlyCache) and re.fullmatch(r"/offline-cache/[a-z0-9][a-z0-9_-]{0,63}", cache.target), "dedicated readonly cache target required")
        _require(cache.target not in targets, "duplicate cache target")
        targets.add(cache.target)
    environment = dict(config.environment)
    _require(len(environment) == len(config.environment) and len(environment) <= 32, "unique bounded callback environment required")
    reserved = {"HOME", "TMPDIR", "TMP", "TEMP", "CARGO_HOME", "RUSTUP_HOME", "CARGO_TARGET_DIR", "CARGO_NET_OFFLINE", "PYTHONDONTWRITEBYTECODE", "XDG_CACHE_HOME", "PATH", "RUSTC", "CARGO", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTUP_TOOLCHAIN"}
    for key, value in config.environment:
        _require(isinstance(key, str) and re.fullmatch(r"[A-Z][A-Z0-9_]{0,63}", key)
                 and key not in reserved and key not in {"LD_PRELOAD", "LD_LIBRARY_PATH", "BASH_ENV", "ENV"}
                 and not any(word in key for word in ("TOKEN", "SECRET", "PASSWORD", "CREDENTIAL")), "unsafe callback environment key")
        _require(isinstance(value, str) and "\0" not in value and len(value) <= 4096, "bounded callback environment value required")
    # Runtime artifacts have a dedicated durable mount; raw logs are outside it.
    mountpoint = config.candidate_root / ".runtime"
    if not mountpoint.exists():
        mountpoint.mkdir(mode=0o755)
    for name in ("tmp", "home", "cargo", "rustup", "target", "cache"):
        path = config.runtime_root / name
        if not path.exists():
            path.mkdir(mode=0o777)
        _no_symlinks(path, directory=True)
        path.chmod(0o777)
    private = root / ".isolation"
    if not private.exists():
        private.mkdir(mode=0o700)
    _no_symlinks(private, directory=True)
    private.chmod(0o700)
    engine_home = private / "engine-home"
    for path in (engine_home, engine_home / "docker", engine_home / "tmp"):
        if not path.exists():
            path.mkdir(mode=0o700)
        _no_symlinks(path, directory=True)
    _require(not any((engine_home / "docker").iterdir()), "engine credential/context config refused")
    # Missing executable is runtime-unavailable, but malformed/broad paths refuse.
    _require(config.engine.is_absolute(), "absolute engine executable required")
    engine_digest = None
    if config.engine.exists():
        _no_symlinks(config.engine)
        _require(os.access(config.engine, os.X_OK), "engine is not executable")
        engine_digest = _bytes_digest(config.engine.read_bytes())
    for mount in mounts:
        _no_symlinks(mount, directory=True)
    sources = {"candidate": _manifest(config.candidate_root, candidate=True, exceptions=exceptions), "grader": _manifest(config.grader_root),
               "runtime": _manifest(config.runtime_root, max_bytes=config.runtime_max_bytes),
               "caches": [_manifest(Path(cache.source), max_bytes=config.cache_max_bytes) for cache in config.readonly_caches]}
    payload = {"engine": str(config.engine), "engine_digest": engine_digest, "endpoint": config.endpoint,
               "image": config.image, "root": str(root), "mounts": [(str(p), target, ro) for p, target, ro in _mounts(config)],
               "mount_identity": [(path.stat().st_dev, path.stat().st_ino) for path in mounts],
               "sources": sources, "argv": config.acceptance_argv, "environment": sorted(_environment(config).items()),
               "limits": [config.timeout_seconds, config.max_output_bytes, config.memory_bytes, config.cpus, config.pids_limit,
                          config.runtime_max_bytes, config.cache_max_bytes],
               "source_archive": {"commit": config.source_commit, "sha256": config.source_archive_sha256,
                                  "exceptions": sorted(exceptions.items()), "wrappers": sorted(wrappers.items())},
               "user": [config.uid, config.gid], "runtimes": [config.python_executable, config.rustc_executable, config.cargo_executable, config.require_rust],
               "supervisor": _bytes_digest(_SUPERVISOR.encode()), "probe": _bytes_digest(_PROBE_CODE.encode()),
               "outcome_controller": _bytes_digest(_OUTCOME_CONTROLLER.encode())}
    return private, payload


def _environment(config):
    return {"HOME": "/work/.runtime/home", "TMPDIR": "/work/.runtime/tmp", "TMP": "/work/.runtime/tmp",
            "TEMP": "/work/.runtime/tmp", "CARGO_HOME": "/work/.runtime/cargo", "RUSTUP_HOME": "/work/.runtime/rustup",
            "CARGO_TARGET_DIR": "/work/.runtime/target", "XDG_CACHE_HOME": "/work/.runtime/cache",
            "CARGO_NET_OFFLINE": "true", "PYTHONDONTWRITEBYTECODE": "1",
            "PATH": ":".join(dict.fromkeys((str(Path(config.python_executable).parent), str(Path(config.rustc_executable).parent), str(Path(config.cargo_executable).parent), "/usr/bin", "/bin"))),
            "RUSTC": config.rustc_executable, "CARGO": config.cargo_executable, "RUSTC_WRAPPER": "",
            "RUSTC_WORKSPACE_WRAPPER": "", "RUSTUP_TOOLCHAIN": "1.97.1", **dict(config.environment)}


def _mounts(config):
    return [(config.candidate_root, "/work", False), (config.runtime_root, "/work/.runtime", False),
            (config.grader_root, "/grader", True), *((Path(cache.source), cache.target, True) for cache in config.readonly_caches)]


def _engine_environment(config):
    home = config.owned_root / ".isolation" / "engine-home"
    return {"PATH": str(config.engine.parent) + ":/usr/bin:/bin", "HOME": str(home),
            "DOCKER_CONFIG": str(home / "docker"), "DOCKER_HOST": config.endpoint,
            "TMPDIR": str(home / "tmp"), "TMP": str(home / "tmp"), "TEMP": str(home / "tmp")}


def _command(config, arguments, *, timeout=20, limit=1024 * 1024, input_data=None):
    """Bound both output streams together; Docker cleanup is a separate proof."""
    try:
        process = subprocess.Popen([str(config.engine), *arguments], stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=_engine_environment(config), close_fds=True)
    except OSError:
        return _Command(None, b"", b"engine unavailable")
    if input_data is not None:
        _require(isinstance(input_data, bytes) and len(input_data) <= 4096, "bounded private controller input required")
        try:
            os.set_blocking(process.stdin.fileno(), False)
            _require(os.write(process.stdin.fileno(), input_data) == len(input_data), "controller input incomplete")
        except (OSError, IsolationError):
            process.kill()
            process.wait(timeout=5)
            return _Command(None, b"", b"controller input unavailable")
        finally:
            process.stdin.close()
    output = {"stdout": bytearray(), "stderr": bytearray()}
    selector = selectors.DefaultSelector()
    for name in output:
        stream = getattr(process, name)
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ, name)
    deadline = time.monotonic() + timeout
    timed_out = limited = False
    try:
        while selector.get_map():
            if time.monotonic() >= deadline:
                timed_out = True
                process.kill()
                break
            for key, _ in selector.select(min(0.1, max(0, deadline - time.monotonic()))):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                remaining = limit - sum(map(len, output.values()))
                output[key.data].extend(chunk[:max(0, remaining)])
                if len(chunk) > remaining:
                    limited = True
                    process.kill()
                    break
            if limited:
                break
        try:
            code = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            code = process.wait(timeout=5)
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()
    return _Command(code, bytes(output["stdout"]), bytes(output["stderr"]), timed_out, limited)


def _container_arguments(config, name):
    arguments = ["run", "--detach", "--rm", "--name", name, "--pull=never", "--network=none",
                 "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--pids-limit", str(config.pids_limit),
                 "--memory", str(config.memory_bytes), "--memory-swap", str(config.memory_bytes), "--cpus", str(config.cpus),
                 "--init", "--user", f"{config.uid}:{config.gid}", "--workdir=/work", "--ipc=private", "--cgroupns=private"]
    for path, target, readonly in _mounts(config):
        arguments.extend(("--mount", f"type=bind,src={path},dst={target},bind-propagation=rprivate" + (",readonly" if readonly else "")))
    for key, value in sorted(_environment(config).items()):
        arguments.extend(("--env", key + "=" + value))
    # Override image entrypoint: neither shell startup nor candidate code may run
    # before inspection/probes. -I -S isolates trusted probes from sitecustomize.
    arguments.extend(("--entrypoint", config.python_executable, config.image, "-I", "-S", "-c", _SUPERVISOR))
    return arguments


def _observed_security(config, raw):
    _require(isinstance(raw, list) and len(raw) == 1, "container inspection unavailable")
    data = raw[0]
    host, container = data["HostConfig"], data["Config"]
    expected_mounts = sorted((str(path), target, not readonly) for path, target, readonly in _mounts(config))
    mounts = sorted((row["Source"], row["Destination"], row["RW"]) for row in data["Mounts"])
    _require(mounts == expected_mounts and all(row["Type"] == "bind" for row in data["Mounts"]), "unexpected container mount")
    _require(host["NetworkMode"] == "none" and host["ReadonlyRootfs"] is True and host["Privileged"] is False,
             "container network/root privilege mismatch")
    _require(set(host["CapDrop"]) == {"ALL"} and not host.get("CapAdd"), "container capability mismatch")
    _require(set(host["SecurityOpt"]) == {"no-new-privileges"} and host["Init"] is True and host["AutoRemove"] is True,
             "container process restrictions mismatch")
    _require(host["PidsLimit"] == config.pids_limit and host["Memory"] == config.memory_bytes
             and host["MemorySwap"] == config.memory_bytes and host["NanoCpus"] == int(config.cpus * 1_000_000_000), "container resource limit mismatch")
    _require(host.get("PidMode", "") == "" and host.get("UTSMode", "") == "" and host.get("UsernsMode", "") == "" and host.get("IpcMode") == "private"
             and host.get("CgroupnsMode") == "private" and not host.get("Devices") and not host.get("DeviceRequests"), "host namespace/device access refused")
    _require(container["User"] == f"{config.uid}:{config.gid}" and container["WorkingDir"] == "/work"
             and container["Image"] == config.image and container["Entrypoint"] == [config.python_executable]
             and container["Cmd"] == ["-I", "-S", "-c", _SUPERVISOR], "container fixed runtime mismatch")
    actual_env = dict(row.split("=", 1) for row in container["Env"])
    _require(all(actual_env.get(key) == value for key, value in _environment(config).items()), "container environment mismatch")
    _require(set(actual_env) <= set(_environment(config)) | {"LANG", "LC_ALL", "TZ", "SSL_CERT_FILE"}, "undeclared image environment")
    _require(data["State"]["Running"] is True, "container supervisor not running")
    checked_host = {key: host.get(key) for key in ("NetworkMode", "ReadonlyRootfs", "Privileged", "CapDrop", "CapAdd",
        "SecurityOpt", "Init", "AutoRemove", "PidsLimit", "Memory", "MemorySwap", "NanoCpus", "PidMode", "UTSMode",
        "UsernsMode", "IpcMode", "CgroupnsMode", "Devices", "DeviceRequests")}
    checked_container = {key: container[key] for key in ("User", "WorkingDir", "Image", "Entrypoint", "Cmd")}
    checked_container["Env"] = sorted(actual_env.items())
    return {"host": checked_host, "mounts": mounts, "container": checked_container, "image_id": data["Image"]}


def _write_private(path, payload):
    raw = _json(payload).encode()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    return _bytes_digest(raw)


def _cleanup(config, name):
    _require(name in _OWNED_CONTAINERS and re.fullmatch(r"plurx-knowledge-[a-f0-9]{32}", name), "unowned container cleanup refused")
    removed = _command(config, ["rm", "--force", "--volumes", name], timeout=20)
    absent = _command(config, ["container", "ls", "--all", "--filter", "name=^/" + name + "$", "--format", "{{.ID}}"], timeout=20)
    proved = absent.returncode == 0 and not absent.stdout.strip() and not absent.timed_out and not absent.output_limited
    if proved:
        _OWNED_CONTAINERS.discard(name)
    observed = {"owned_container": name,
                "remove": {"returncode": removed.returncode, "timed_out": removed.timed_out,
                           "output_limited": removed.output_limited, "stdout_digest": _bytes_digest(removed.stdout),
                           "stderr_digest": _bytes_digest(removed.stderr)},
                "list": {"returncode": absent.returncode, "timed_out": absent.timed_out,
                         "output_limited": absent.output_limited, "stdout": absent.stdout.decode("utf-8", "replace"),
                         "stderr_digest": _bytes_digest(absent.stderr)}}
    return _Cleanup(proved, observed)


def _configuration_probe(config, private):
    name = "plurx-knowledge-" + uuid.uuid4().hex
    sentinel = private / ("outside-" + uuid.uuid4().hex)
    sentinel.write_bytes(os.urandom(32))
    sentinel.chmod(0o600)
    sentinel_digest = _bytes_digest(sentinel.read_bytes())
    _OWNED_CONTAINERS.add(name)
    observations = None
    cleanup = False
    try:
        image = _command(config, ["image", "inspect", config.image])
        _require(image.returncode == 0 and not image.output_limited and not image.timed_out, "pinned image unavailable")
        image_data = json.loads(image.stdout)
        _require(len(image_data) == 1 and image_data[0]["Os"] == "linux" and not image_data[0]["Config"].get("Volumes"), "trusted Linux image unavailable")
        if config.image.startswith("sha256:"):
            _require(image_data[0]["Id"] == config.image, "immutable image ID unavailable")
        else:
            _require(config.image in image_data[0].get("RepoDigests", ()), "image digest unavailable")
        started = _command(config, _container_arguments(config, name))
        _require(started.returncode == 0 and not started.timed_out and not started.output_limited, "container unavailable")
        inspected = _command(config, ["inspect", name])
        _require(inspected.returncode == 0 and not inspected.timed_out and not inspected.output_limited, "container inspection unavailable")
        security = _observed_security(config, json.loads(inspected.stdout))
        _require(security["image_id"] == image_data[0]["Id"], "container image identity mismatch")
        checked = _command(config, ["exec", "--workdir=/", name, config.python_executable, "-I", "-S", "-c", _PROBE_CODE,
                            str(sentinel), config.rustc_executable, config.cargo_executable, "yes" if config.require_rust else "no"])
        _require(checked.returncode == 0 and not checked.timed_out and not checked.output_limited, "isolation probes unavailable")
        observed = json.loads(checked.stdout)
        _require(observed["outside"]["readable"] is False and observed["outside"]["errno"] in {errno.ENOENT, errno.EACCES}, "outside sentinel readable")
        _require(observed["network"]["interfaces"] == ["lo"] and observed["network"]["errno"] in {errno.ENETUNREACH, errno.EHOSTUNREACH, errno.EACCES, errno.EPERM}, "network denial unproved")
        _require(observed["filesystem"]["candidate"]["writable"] is True and observed["filesystem"]["runtime"]["writable"] is True
                 and observed["filesystem"]["grader"]["writable"] is False and observed["filesystem"]["grader"]["errno"] in {errno.EROFS, errno.EACCES}, "filesystem permissions unproved")
        _require(bool(re.fullmatch(r"3\.[0-9]+\.[0-9]+", observed["versions"]["python"])), "Python runtime unavailable")
        if config.require_rust:
            rust = observed["versions"]["rustc"]
            cargo = observed["versions"]["cargo"]
            _require(rust["returncode"] == 0 and re.match(r"rustc 1\.97\.1(?: |$)", rust["output"]), "pinned Rust runtime unavailable")
            _require(cargo["returncode"] == 0 and cargo["output"].startswith("cargo "), "Cargo runtime unavailable")
        _require(_bytes_digest(sentinel.read_bytes()) == sentinel_digest, "outside sentinel changed")
        observations = {"security": security, "observed_probes": observed, "sentinel_digest": sentinel_digest}
        return name, observations
    finally:
        # Successful callers retain this live container for their one callback;
        # every refusal cleans it before propagating an unavailable result.
        if observations is None:
            cleanup = _cleanup(config, name)
            if not cleanup:
                raise IsolationError("container cleanup unproved")
        try:
            sentinel.unlink(missing_ok=True)
        except OSError:
            pass


def probe(config: IsolationConfig) -> ProbeReceipt:
    private, payload = _prepare(config)
    config_digest = _digest(payload)
    path = private / ("probe-" + uuid.uuid4().hex + ".json")
    observations = None
    cleaned = False
    try:
        name, observations = _configuration_probe(config, private)
        cleaned = _cleanup(config, name)
        _require(cleaned, "container cleanup unproved")
        _require(_prepare(config)[1] == payload, "configuration changed during probe")
    except (IsolationError, OSError, ValueError, KeyError, TypeError):
        observations = None
    body = {"status": "verified" if observations is not None and cleaned else "unavailable",
            "config_digest": config_digest, "configuration": payload, "observations": observations,
            "cleanup_proved": bool(cleaned), "cleanup_observations": getattr(cleaned, "observations", None)}
    digest = _write_private(path, body)
    receipt = ProbeReceipt(body["status"], digest, path, config_digest)
    if receipt.status == "verified":
        _ISSUED[id(receipt)] = (receipt, body)
    return receipt


def _plan_data(plan: FamilyPlan):
    _require(isinstance(plan, FamilyPlan) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", plan.family), "fixed family plan required")
    for commands in (plan.prerequisite_argv, plan.acceptance_argv):
        _require(0 < len(commands) <= 32, "bounded family commands required")
        for argv in commands:
            _require(0 < len(argv) <= 256 and all(isinstance(arg, str) and "\0" not in arg for arg in argv)
                     and sum(map(len, argv)) <= 32768, "literal family command required")
    driver = plan.driver
    if driver is not None:
        _require(isinstance(driver, TrustedSourceFile) and re.fullmatch(r"[a-f0-9]{64}", driver.sha256), "pinned family driver required")
        _require(isinstance(driver.path, str) and not driver.path.startswith("/")
                 and all(part not in {"", ".", ".."} and not _private_name(part) for part in driver.path.split("/"))
                 and "\\" not in driver.path and len(driver.path) <= 1024, "safe immutable driver path required")
    return {"family": plan.family, "prerequisite_argv": plan.prerequisite_argv, "acceptance_argv": plan.acceptance_argv,
            "driver": None if driver is None else {"path": driver.path, "sha256": driver.sha256}}


def _family_binding(payload, plan, baseline_source_digest, observed_runtime):
    # Candidate bytes intentionally differ after patching; baseline bytes are
    # retained separately. Every dependency/runtime/grader/policy input remains
    # bound, including full offline-cache membership and image identity.
    return {"plan": _plan_data(plan), "baseline_source_digest": baseline_source_digest,
            "observed_runtime": observed_runtime, "image": payload["image"], "engine": payload["engine_digest"],
            "source_archive": payload["source_archive"], "grader": payload["sources"]["grader"],
            "runtime_dependencies": payload["sources"]["runtime"], "caches": payload["sources"]["caches"],
            "runtimes": payload["runtimes"], "environment": payload["environment"],
            "limits": payload["limits"], "user": payload["user"], "controller": _bytes_digest(_OUTCOME_CONTROLLER.encode())}


def _issued_probe(receipt, private, payload):
    issued = _ISSUED.get(id(receipt))
    valid = (issued is not None and issued[0] is receipt and receipt.status == "verified"
             and receipt.config_digest == _digest(payload) and receipt.private_path is not None)
    if valid:
        try:
            _require(receipt.private_path.parent == private, "receipt outside private storage")
            _no_symlinks(receipt.private_path)
            valid = _bytes_digest(receipt.private_path.read_bytes()) == receipt.digest
        except (OSError, IsolationError):
            valid = False
    return issued if valid else None


def _qualified_for(receipt, payload, plan, observed_runtime):
    issued = _QUALIFIED.get(id(receipt))
    if issued is None or issued[0] is not receipt or receipt.status != "qualified" or receipt.family != plan.family:
        return None
    try:
        _no_symlinks(receipt.private_path)
        _require(_bytes_digest(receipt.private_path.read_bytes()) == receipt.digest, "prerequisite receipt changed")
        _require(receipt.binding_digest == _digest(_family_binding(payload, plan, issued[1]["baseline_source_digest"], observed_runtime)), "qualified family inputs changed")
    except (IsolationError, OSError):
        return None
    return issued[1]


def _driver_packet(config, name, argv, plan, mode, context):
    driver = plan.driver
    _require(driver is not None, "family has no reviewed structured outcome driver")
    path = config.grader_root / driver.path
    _no_symlinks(path)
    _require(_read_member(config.grader_root, path, path.lstat()) == driver.sha256, "family driver changed")
    secret = os.urandom(32)
    nonce = uuid.uuid4().hex
    arguments = ["exec", "--interactive", "--workdir=/", name, config.python_executable, "-I", "-S", "-c", _OUTCOME_CONTROLLER,
                 "/grader/" + driver.path, driver.sha256, mode, _json(argv), _json(context), str(config.max_output_bytes)]
    result = _command(config, arguments, timeout=config.timeout_seconds, limit=2 * config.max_output_bytes + 16384,
                      input_data=_json({"secret": secret.hex(), "nonce": nonce}).encode())
    if result.returncode != 0 or result.timed_out or result.output_limited:
        return result, None
    try:
        packet = json.loads(result.stdout)
        body = packet["body"]
        expected = hmac.new(secret, _json(body).encode(), hashlib.sha256).hexdigest()
        _require(isinstance(packet["hmac"], str) and hmac.compare_digest(expected, packet["hmac"]), "untrusted outcome packet")
        _require(body["nonce"] == nonce and body["argv"] == list(argv) and body["mode"] == mode
                 and body["context"] == context and body["driver_sha256"] == driver.sha256, "outcome packet binding mismatch")
        return result, body
    except (ValueError, KeyError, TypeError, IsolationError):
        return result, None


def _completed_classification(packet, mode):
    if packet is None or packet.get("driver_started") is not True or packet.get("driver_completed") is not True:
        return "unavailable"
    if packet.get("hardened") is not True or packet.get("resources_verified") is not True or packet.get("resource_failure") is not False:
        return "unavailable"
    result = packet.get("result", {})
    if not isinstance(result, dict) or type(result.get("protocol")) is not int or result.get("protocol") != 1 or result.get("completed") is not True:
        return "unavailable"
    evidence = result.get("evidence", {})
    if not isinstance(evidence, dict) or not (isinstance(evidence.get("identity"), str) and evidence["identity"]
            and isinstance(evidence.get("sha256"), str) and re.fullmatch(r"[a-f0-9]{64}", evidence["sha256"])):
        return "unavailable"
    outcome = result.get("outcome")
    if mode == "prerequisite":
        return "prerequisite-passed" if outcome == "prerequisite-passed" and evidence.get("kind") == "prerequisite" else "unavailable"
    allowed = {"passed": {"inert-bytes", "assertion", "compile"}, "assertion-failed": {"inert-bytes", "assertion"}, "compile-failed": {"compile"}}
    return outcome if outcome in allowed and evidence.get("kind") in allowed[outcome] else "unavailable"


def _execute_family(config, argv, receipt, plan, *, mode, prerequisite=None, baseline_source_digest=""):
    private, payload = _prepare(config)
    _require(isinstance(argv, tuple) and argv in config.acceptance_argv, "callback argv not registered")
    plan_data = _plan_data(plan)
    commands = plan.prerequisite_argv if mode == "prerequisite" else plan.acceptance_argv
    _require(argv in commands, "callback outside frozen family plan")
    path = private / ("execution-" + uuid.uuid4().hex + ".json")
    issued = _issued_probe(receipt, private, payload)
    runtime = issued[1]["observations"]["observed_probes"]["versions"] if issued is not None else None
    qualified = _qualified_for(prerequisite, payload, plan, runtime) if mode == "acceptance" else None
    if issued is None or plan.driver is None or (mode == "acceptance" and qualified is None):
        body = {"status": "unavailable", "classification": "unavailable", "reason": "unqualified-or-unsupported-family",
                "probe_digest": getattr(receipt, "digest", ""), "plan": plan_data}
        return ExecutionResult("unavailable", _write_private(path, body), path)
    prerequisite_digest = prerequisite.digest if qualified is not None else ""
    context = {"family": plan.family, "plan_digest": _digest(plan_data), "prerequisite_digest": prerequisite_digest,
               "baseline_source_digest": qualified["baseline_source_digest"] if qualified is not None else baseline_source_digest}
    outcome = packet = observed = engine_state = None
    cleaned = False
    name = None
    try:
        name, observed = _configuration_probe(config, private)
        previous = issued[1]["observations"]
        _require({k: v for k, v in observed.items() if k != "sentinel_digest"} ==
                 {k: v for k, v in previous.items() if k != "sentinel_digest"}, "observed isolation changed")
        _require(_prepare(config)[1] == payload, "configuration changed before callback")
        outcome, packet = _driver_packet(config, name, argv, plan, mode, context)
        inspected = _command(config, ["inspect", name])
        _require(inspected.returncode == 0 and not inspected.timed_out and not inspected.output_limited, "executor state unavailable")
        state = json.loads(inspected.stdout)[0]["State"]
        engine_state = {"running": state.get("Running"), "oom_killed": state.get("OOMKilled")}
        _require(engine_state == {"running": True, "oom_killed": False}, "executor resource/start failure")
    except (IsolationError, OSError, ValueError, KeyError, TypeError):
        packet = None
    finally:
        if name is not None:
            cleaned = _cleanup(config, name)
    classification = _completed_classification(packet, mode) if cleaned else "unavailable"
    status = "passed" if classification in {"passed", "prerequisite-passed"} else "failed" if classification in {"assertion-failed", "compile-failed"} else "unavailable"
    body = {"status": status, "classification": classification, "mode": mode, "family": plan.family,
            "prerequisite_digest": prerequisite_digest, "probe_digest": receipt.digest, "config_digest": _digest(payload),
            "plan_digest": _digest(plan_data), "baseline_source_digest": context["baseline_source_digest"],
            "cleanup_proved": bool(cleaned), "cleanup_observations": getattr(cleaned, "observations", None),
            "observations": observed, "engine_state": engine_state, "structured_outcome": packet,
            "output": None if outcome is None else {"stdout_base64": base64.b64encode(outcome.stdout).decode("ascii"),
                "stderr_base64": base64.b64encode(outcome.stderr).decode("ascii"), "returncode": outcome.returncode,
                "timed_out": outcome.timed_out, "output_limited": outcome.output_limited}}
    return ExecutionResult(status, _write_private(path, body), path, classification, prerequisite_digest)


def qualify_family(config: IsolationConfig, family: FamilyPlan, receipt: ProbeReceipt) -> PrerequisiteReceipt:
    """Actually qualify baseline prerequisites; security/version probes do not.

    Frozen gold assertions need not pass on the baseline. Only separately named
    import/build/dependency prerequisites with a reviewed completion driver can
    qualify. Unsupported families remain unavailable without scoring a patch.
    """
    private, initial = _prepare(config)
    plan = _plan_data(family)
    _require(all(argv in config.acceptance_argv for argv in family.prerequisite_argv), "baseline prerequisites not registered")
    paths = []
    qualified = family.driver is not None and _issued_probe(receipt, private, initial) is not None
    baseline = initial["sources"]["candidate"]
    if qualified:
        for number, argv in enumerate(family.prerequisite_argv):
            current_probe = receipt if number == 0 else probe(config)
            result = _execute_family(config, argv, current_probe, family, mode="prerequisite", baseline_source_digest=baseline)
            paths.append({"digest": result.digest, "classification": result.classification})
            if result.classification != "prerequisite-passed":
                qualified = False
                break
    final_probe = probe(config) if qualified else None
    final_payload = _prepare(config)[1]
    final_issued = _issued_probe(final_probe, private, final_payload) if final_probe is not None else None
    qualified = qualified and final_issued is not None
    runtime = final_issued[1]["observations"]["observed_probes"]["versions"] if final_issued is not None else None
    binding_data = _family_binding(final_payload, family, baseline, runtime)
    binding = _digest(binding_data)
    body = {"status": "qualified" if qualified else "unavailable", "family": family.family, "plan": plan,
            "binding": binding_data, "binding_digest": binding,
            "baseline_source_digest": baseline, "baseline_configuration": initial, "executions": paths}
    path = private / ("prerequisite-" + uuid.uuid4().hex + ".json")
    result = PrerequisiteReceipt(body["status"], _write_private(path, body), path, family.family, binding)
    if qualified:
        _QUALIFIED[id(result)] = (result, body)
    return result


def execute(config: IsolationConfig, argv: tuple[str, ...], receipt: ProbeReceipt, *,
            family: FamilyPlan | None = None, prerequisite: PrerequisiteReceipt | None = None) -> ExecutionResult:
    """A raw exit code is never a validated patch outcome (R4).

    Missing dependencies, Docker exec/start errors, OOM/resource failures,
    unsupported families and absent authenticated completion remain unavailable.
    A completed trusted assertion/compiler failure is retained only after the
    exact family's baseline/dependency prerequisites actually qualified.
    """
    if family is None:
        private, _ = _prepare(config)
        path = private / ("execution-" + uuid.uuid4().hex + ".json")
        body = {"status": "unavailable", "classification": "unavailable", "reason": "family-prerequisite-required"}
        return ExecutionResult("unavailable", _write_private(path, body), path)
    return _execute_family(config, argv, receipt, family, mode="acceptance", prerequisite=prerequisite)
