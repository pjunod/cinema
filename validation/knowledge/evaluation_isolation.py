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

    def __post_init__(self):
        for name in ("engine", "owned_root", "candidate_root", "grader_root", "runtime_root"):
            object.__setattr__(self, name, Path(getattr(self, name)))
        object.__setattr__(self, "acceptance_argv", tuple(tuple(argv) for argv in self.acceptance_argv))
        object.__setattr__(self, "environment", tuple(tuple(pair) for pair in self.environment))
        object.__setattr__(self, "readonly_caches", tuple(self.readonly_caches))


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
_IMAGE = re.compile(r"(?:[a-z0-9][a-z0-9._:/-]*@)?sha256:[a-f0-9]{64}\Z")
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


def _read_member(root: Path, path: Path, expected):
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
            _require(stat.S_ISREG(before.st_mode) and before.st_size <= 512 * 1024 * 1024, "bounded regular mount file required")
            raw = stream.read(512 * 1024 * 1024 + 1)
            after = os.fstat(stream.fileno())
        current = os.stat(parts[-1], dir_fd=directory, follow_symlinks=False)
    finally:
        os.close(directory)
    def identity(value):
        return (value.st_dev, value.st_ino, value.st_size, value.st_mode, value.st_mtime_ns, value.st_ctime_ns)
    _require(identity(expected) == identity(before) == identity(after) == identity(current), "mount source changed during capture")
    return raw


def _manifest(root: Path, *, candidate=False):
    rows = []
    total = 0
    members = 0
    for directory, folders, files in os.walk(root, followlinks=False):
        if candidate and Path(directory) == root and ".runtime" in folders:
            runtime = root / ".runtime"
            _require(not runtime.is_symlink() and not any(runtime.iterdir()), "candidate runtime mountpoint must be empty")
            folders.remove(".runtime")
        for name in sorted([*folders, *files]):
            members += 1
            _require(members <= 100000, "mount path count bound exceeded")
            path = Path(directory) / name
            _require(not _private_name(name) and not path.is_symlink(), "private or symlink mount member refused")
            info = path.lstat()
            _require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode), "special mount member refused")
            if stat.S_ISREG(info.st_mode):
                total += info.st_size
                _require(total <= 512 * 1024 * 1024 and len(rows) < 100000, "mount corpus bound exceeded")
                raw = _read_member(root, path, info)
                rows.append((path.relative_to(root).as_posix(), stat.S_IMODE(info.st_mode), _bytes_digest(raw)))
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
                                (config.memory_bytes, 64 * 1024 * 1024, 16 * 1024**3), (config.pids_limit, 8, 256)):
        _require(type(value) is int and lower <= value <= upper, "finite execution bounds required")
    _require(type(config.cpus) in {int, float} and 0.1 <= config.cpus <= 16, "finite CPU bound required")
    _require(type(config.require_rust) is bool, "explicit Rust requirement required")
    for path in (config.python_executable, config.rustc_executable, config.cargo_executable):
        _container_executable(path)
    _require(0 < len(config.acceptance_argv) <= 32, "fixed acceptance argv required")
    for argv in config.acceptance_argv:
        _require(0 < len(argv) <= 256 and argv[0] in {config.python_executable, config.cargo_executable, config.rustc_executable}, "trusted runtime callback required")
        _require(all(isinstance(arg, str) and "\0" not in arg for arg in argv) and sum(map(len, argv)) <= 32768, "bounded literal argv required")
        _require(config.require_rust or argv[0] == config.python_executable, "Rust callbacks require pinned Rust verification")
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
    sources = {"candidate": _manifest(config.candidate_root, candidate=True), "grader": _manifest(config.grader_root),
               "runtime": _manifest(config.runtime_root),
               "caches": [_manifest(Path(cache.source)) for cache in config.readonly_caches]}
    payload = {"engine": str(config.engine), "engine_digest": engine_digest, "endpoint": config.endpoint,
               "image": config.image, "root": str(root), "mounts": [(str(p), target, ro) for p, target, ro in _mounts(config)],
               "mount_identity": [(path.stat().st_dev, path.stat().st_ino) for path in mounts],
               "sources": sources, "argv": config.acceptance_argv, "environment": sorted(_environment(config).items()),
               "limits": [config.timeout_seconds, config.max_output_bytes, config.memory_bytes, config.cpus, config.pids_limit],
               "user": [config.uid, config.gid], "runtimes": [config.python_executable, config.rustc_executable, config.cargo_executable, config.require_rust],
               "supervisor": _bytes_digest(_SUPERVISOR.encode()), "probe": _bytes_digest(_PROBE_CODE.encode())}
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


def _command(config, arguments, *, timeout=20, limit=1024 * 1024):
    """Bound both output streams together; Docker cleanup is a separate proof."""
    try:
        process = subprocess.Popen([str(config.engine), *arguments], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=_engine_environment(config), close_fds=True)
    except OSError:
        return _Command(None, b"", b"engine unavailable")
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


def execute(config: IsolationConfig, argv: tuple[str, ...], receipt: ProbeReceipt) -> ExecutionResult:
    private, payload = _prepare(config)
    _require(isinstance(argv, tuple) and argv in config.acceptance_argv, "callback argv not registered")
    path = private / ("execution-" + uuid.uuid4().hex + ".json")
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
    if not valid:
        body = {"status": "unavailable", "reason": "unverified-or-changed-isolation", "probe_digest": getattr(receipt, "digest", "")}
        return ExecutionResult("unavailable", _write_private(path, body), path)
    outcome = None
    observed = None
    cleaned = False
    name = None
    try:
        name, observed = _configuration_probe(config, private)
        previous = issued[1]["observations"]
        # Ignore the new random sentinel's content digest, retaining every
        # observed security/runtime/network/filesystem condition.
        _require({k: v for k, v in observed.items() if k != "sentinel_digest"} ==
                 {k: v for k, v in previous.items() if k != "sentinel_digest"}, "observed isolation changed")
        _require(_prepare(config)[1] == payload, "configuration changed before callback")
        outcome = _command(config, ["exec", "--workdir=/work", name, *argv],
                           timeout=config.timeout_seconds, limit=config.max_output_bytes)
    except (IsolationError, OSError, ValueError, KeyError, TypeError):
        pass
    finally:
        if name is not None:
            cleaned = _cleanup(config, name)
    status = "unavailable"
    if outcome is not None and cleaned and not outcome.timed_out and not outcome.output_limited and outcome.returncode is not None:
        status = "passed" if outcome.returncode == 0 else "failed"
    body = {"status": status, "probe_digest": receipt.digest, "config_digest": _digest(payload),
            "cleanup_proved": bool(cleaned), "cleanup_observations": getattr(cleaned, "observations", None), "observations": observed,
            "output": None if outcome is None else {"stdout_base64": base64.b64encode(outcome.stdout).decode("ascii"),
                "stderr_base64": base64.b64encode(outcome.stderr).decode("ascii"), "returncode": outcome.returncode,
                "timed_out": outcome.timed_out, "output_limited": outcome.output_limited}}
    return ExecutionResult(status, _write_private(path, body), path)
