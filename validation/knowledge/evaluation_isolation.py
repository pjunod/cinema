"""Opt-in local OCI boundary for private acceptance callbacks.

Nothing launches or imports a container engine at module import. The custodian
supplies a pinned, trusted Linux image and exact callback argv. Raw observations
and output stay in the owned private directory; public results expose only a
status and digest. Missing or unproved isolation is unavailable qualification.

The controller authenticates a pinned trusted driver and its report, not
assertion integrity inside a candidate-controlled child. Drivers must keep
candidate execution outside the controller and independently establish actual
completed checks; child exit codes/stdout alone never qualify an outcome.
"""
from __future__ import annotations

from dataclasses import dataclass, replace
import base64
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
import threading
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
class TrustedSourceMember:
    """Exact tracked source export member, never a host credential exception."""
    path: str
    sha256: str
    git_blob: str


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
    candidate_readonly: bool = False
    working_directory: str = "/work"
    trusted_source_members: tuple[TrustedSourceMember, ...] = ()
    native_binding: object | None = None

    def __post_init__(self):
        for name in ("engine", "owned_root", "candidate_root", "grader_root", "runtime_root"):
            object.__setattr__(self, name, Path(getattr(self, name)))
        object.__setattr__(self, "engine", self.engine.resolve())
        if self.native_binding is not None:
            from .study_driver.guest_stage_store import NativeGuestBinding
            _require(type(self.native_binding) is NativeGuestBinding, "typed native stage binding required")
        object.__setattr__(self, "trusted_source_members", tuple(self.trusted_source_members))
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
class ExecutionStep:
    """One sealed command; writes authorizes only declared mounted state changes."""
    id: str
    argv: tuple[str, ...]
    cwd: str = "/work"
    environment: tuple[tuple[str, str], ...] = ()
    writes: frozenset[str] = frozenset()
    kind: str = "assertion"
    protected_readonly: bool = True

    def __post_init__(self):
        object.__setattr__(self, "argv", tuple(self.argv))
        object.__setattr__(self, "environment", tuple(sorted(tuple(pair) for pair in self.environment)))
        object.__setattr__(self, "writes", frozenset(self.writes))


@dataclass(frozen=True)
class ProtectedWorkerPlan:
    """Candidate-only RPC role; no protected grader/outcome material is mounted."""
    config: IsolationConfig
    driver: TrustedSourceFile
    argv: tuple[str, ...]
    operations: tuple[str, ...]
    schema_sha256: str
    proxy_sha256: str
    exception_types: tuple[str, ...] = ()
    max_frame_bytes: int = 65536
    max_requests: int = 4096
    max_total_bytes: int = 16 * 1024 * 1024

    def __post_init__(self):
        for name in ("argv", "operations", "exception_types"):
            object.__setattr__(self, name, tuple(getattr(self, name)))


@dataclass(frozen=True)
class FamilyPlanV2:
    family: str
    prerequisite_steps: tuple[ExecutionStep, ...]
    acceptance_steps: tuple[ExecutionStep, ...]
    driver: TrustedSourceFile
    schema_version: int = 2
    worker: ProtectedWorkerPlan | None = None

    def __post_init__(self):
        for name in ("prerequisite_steps", "acceptance_steps"):
            object.__setattr__(self, name, tuple(getattr(self, name)))


@dataclass(frozen=True)
class StepReceipt:
    status: str
    digest: str
    private_path: Path
    classification: str
    family: str
    plan_digest: str
    binding_digest: str
    mode: str
    index: int
    prerequisite_digest: str = ""

    def public(self):
        return {"status": self.status, "digest": self.digest}


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
_STEPS: dict[int, tuple[StepReceipt, dict[str, Any]]] = {}
_CONSUMED_STEPS: set[int] = set()
_PROBE_SEQUENCE = 0
_CHAIN_LOCK = threading.Lock()
_ACTIVE_ROOTS: set[str] = set()
_RESERVED_STEPS: set[int] = set()
_IMAGE = re.compile(r"(?:[a-z0-9][a-z0-9._:/-]*@)?sha256:[a-f0-9]{64}\Z")
# These are tracked Rust sources, never credential stores. Adding an exception
# or wrapper family requires a source change and review, not a model tool flag.
_SOURCE_EXCEPTIONS = frozenset({"crates/plurx-core/src/secrets.rs", "crates/plurx-compat-jellyfin/src/credentials.rs"})
_WRAPPERS = frozenset({"scripts/quiet"})
_SUPERVISOR = "import time; time.sleep(86400)"
# Both controllers see their own independently verified /work bind mount.
# Keep isolated Python startup; never recover ambient PYTHONPATH/site roots.
_ROLE_IMPORT_BOOTSTRAP = r'''
source_root="/work"
if not os.path.isdir(source_root) or os.path.islink(source_root):raise RuntimeError("role source mount unavailable")
sys.path.insert(0,source_root)
'''
_PROBE_CODE = r'''
import errno,json,os,platform,socket,subprocess,sys
sentinel, rustc, cargo, rust_required = sys.argv[1:]
guest_codes={name:getattr(errno,name) for name in ("EPERM","ENOENT","EACCES","EROFS","ENETUNREACH","EHOSTUNREACH")}
if any(errno.errorcode.get(code)!=name for name,code in guest_codes.items()):raise RuntimeError("guest errno namespace inconsistent")
guest_errno={"schema_version":1,"platform":sys.platform,"codes":guest_codes}
def error_observation(code):
    return {"errno":code,"error":"OK" if type(code) is int and code==0 else errno.errorcode.get(code,"UNKNOWN")}
filesystem={}
for label,directory in (("candidate","/work"),("runtime","/work/.runtime/tmp"),("grader","/grader"),("quiet_logs","/work/target/quiet")):
    path=directory+"/.isolation-write-probe-"+str(os.getpid())
    try:
        descriptor=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        os.write(descriptor,b"probe")
        os.close(descriptor)
        os.unlink(path)
        filesystem[label]={"writable":True,**error_observation(0)}
    except OSError as error:
        filesystem[label]={"writable":False,**error_observation(error.errno)}
try:
    with open(sentinel,"rb") as stream:
        stream.read(1)
    outside = {"readable":True,**error_observation(0)}
except OSError as error:
    outside = {"readable":False,**error_observation(error.errno)}
sock=socket.socket(socket.AF_INET,socket.SOCK_STREAM)
sock.settimeout(1)
try:
    network_errno=sock.connect_ex(("1.1.1.1",53))
except OSError as error:
    network_errno=error.errno
finally:
    sock.close()
interfaces=sorted(name for _,name in socket.if_nameindex())
if len(interfaces)>256:raise RuntimeError("network link bound")
links={}
for name in interfaces:
    if not name or "/" in name or name in {".",".."}:raise RuntimeError("network link identity")
    with open("/sys/class/net/"+name+"/flags") as stream:flags=stream.read(33)
    with open("/sys/class/net/"+name+"/operstate") as stream:operstate=stream.read(33)
    if len(flags)>32 or len(operstate)>32:raise RuntimeError("network link state bound")
    links[name]={"flags":int(flags,16),"operstate":operstate.strip().lower()}
def route_lines(path):
    with open(path) as stream:raw=stream.read(512*1024+1)
    if len(raw)>512*1024:raise RuntimeError("network route byte bound")
    rows=raw.splitlines()
    if len(rows)>4096:raise RuntimeError("network route count bound")
    return rows
ipv4_lines=route_lines("/proc/net/route")
if not ipv4_lines or ipv4_lines[0].split()!=["Iface","Destination","Gateway","Flags","RefCnt","Use","Metric","Mask","MTU","Window","IRTT"]:
    raise RuntimeError("IPv4 route header schema")
ipv4_routes=[]
for line in ipv4_lines[1:]:
    row=line.split()
    if len(row)!=11:raise RuntimeError("IPv4 route schema")
    ipv4_routes.append({"interface":row[0],"destination":row[1].lower(),"gateway":row[2].lower(),
                        "flags":int(row[3],16),"mask":row[7].lower()})
ipv6_routes=[]
for line in route_lines("/proc/net/ipv6_route"):
    row=line.split()
    if len(row)!=10:raise RuntimeError("IPv6 route schema")
    ipv6_routes.append({"interface":row[9],"destination":row[0].lower(),"destination_prefix":int(row[1],16),
                        "source":row[2].lower(),"source_prefix":int(row[3],16),"gateway":row[4].lower(),"flags":int(row[8],16)})
ipv4_routes.sort(key=lambda row:json.dumps(row,sort_keys=True))
ipv6_routes.sort(key=lambda row:json.dumps(row,sort_keys=True))
versions={"python":platform.python_version(),"python_executable":sys.executable,"platform":platform.platform(),"machine":platform.machine()}
if rust_required == "yes":
    for name,path in (("rustc",rustc),("cargo",cargo)):
        outcome=subprocess.run([path,"--version"],cwd="/",stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
        versions[name]={"returncode":outcome.returncode,"output":outcome.stdout.decode("utf-8","replace").strip()}
build_environment={key:os.environ.get(key) for key in ("CARGO_BUILD_JOBS","CARGO_PROFILE_DEV_DEBUG","CARGO_PROFILE_TEST_DEBUG")} if rust_required=="yes" else {}
native_mounts={path:[os.stat(path).st_dev,os.stat(path).st_ino] for path in ("/work","/work/.runtime","/grader")}
print(json.dumps({"native_mounts":native_mounts,"outside":outside,"network":{"interfaces":interfaces,"links":links,"ipv4_routes":ipv4_routes,"ipv6_routes":ipv6_routes,**error_observation(network_errno)},"guest_errno":guest_errno,"versions":versions,"filesystem":filesystem,"build_environment":build_environment},sort_keys=True))
'''


_OUTCOME_CONTROLLER = r'''
import base64,contextlib,ctypes,hashlib,hmac,json,os,sys
''' + _ROLE_IMPORT_BOOTSTRAP + r'''
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
    if context.get("protocol")==2:
        os.chdir(context["step"]["cwd"])
        os.environ.clear()
        os.environ.update(context["effective_environment"])
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

# The paired role uses separate stdio pipes, never a shared outcome socket or
# a credential/key mounted into the candidate container.
_PROTECTED_RPC = r'''
wire_out=sys.__stdout__;wire_in=sys.__stdin__.buffer;rpc_sequence=0
def rpc(method,args,kwargs):
    global rpc_sequence
    rpc_sequence+=1
    frame={"kind":"rpc","request":{"protocol":2,"id":rpc_sequence,"method":method,"args":args,"kwargs":kwargs}}
    raw=canonical(frame)
    bound=context["rpc_limits"]["max_frame_bytes"]
    if len(raw.encode())>bound:raise RuntimeError("RPC request bound")
    wire_out.write(raw+"\n");wire_out.flush()
    line=wire_in.readline(bound+2)
    if not line.endswith(b"\n") or len(line)>bound+1:raise RuntimeError("RPC reply framing")
    reply=json.loads(line)
    if set(reply)!={"protocol","id","result"} or reply["protocol"]!=2 or reply["id"]!=rpc_sequence:raise RuntimeError("RPC reply identity")
    return reply["result"]
'''
_PROTECTED_CONTROLLER = _OUTCOME_CONTROLLER.replace(
    'auth=json.loads(sys.stdin.buffer.read(4097));secret=bytes.fromhex(auth["secret"])',
    'auth=json.loads(sys.stdin.buffer.readline(4097));secret=bytes.fromhex(auth["secret"])\n' + _PROTECTED_RPC).replace(
    'namespace={"__name__":"_trusted_family_driver","__file__":source}',
    'namespace={"__name__":"_trusted_family_driver","__file__":source,"rpc":rpc}').replace(
    'print(canonical({"body":body,"hmac":hmac.new(secret,canonical(body).encode(),hashlib.sha256).hexdigest()}))',
    'print(canonical({"kind":"outcome","packet":{"body":body,"hmac":hmac.new(secret,canonical(body).encode(),hashlib.sha256).hexdigest()}}))')

_WORKER_CONTROLLER = r'''
import hashlib,json,os,sys
''' + _ROLE_IMPORT_BOOTSTRAP + r'''
source,digest,argv,context,bound=sys.argv[1:]
argv=json.loads(argv);context=json.loads(context);bound=int(bound)
with open(source,"rb") as stream:code=stream.read(8*1024*1024+1)
if len(code)>8*1024*1024 or hashlib.sha256(code).hexdigest()!=digest:raise RuntimeError("worker driver identity")
namespace={"__name__":"_candidate_rpc_driver","__file__":source}
exec(compile(code,source,"exec"),namespace)
sequence=0
while True:
    line=sys.stdin.buffer.readline(bound+2)
    if not line:break
    if not line.endswith(b"\n") or len(line)>bound+1:raise RuntimeError("worker framing")
    request=json.loads(line);sequence+=1
    if set(request)!={"protocol","id","method","args","kwargs"} or request["protocol"]!=2 or request["id"]!=sequence:raise RuntimeError("worker request identity")
    result=namespace["dispatch"](request["method"],request["args"],request["kwargs"],context)
    raw=json.dumps({"protocol":2,"id":sequence,"result":result},sort_keys=True,separators=(",",":"),allow_nan=False)
    if len(raw.encode())>bound:raise RuntimeError("worker output bound")
    sys.stdout.write(raw+"\n");sys.stdout.flush()
'''
_RESOURCE_CODE = r'''
import json
result={}
for name in ("memory.events","pids.events"):
    with open("/sys/fs/cgroup/"+name) as stream:
        result[name]={key:int(value) for key,value in (line.split() for line in stream)}
print(json.dumps(result,sort_keys=True))
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


def _hard_private_name(name):
    """Source permits never authorize host credential/control roots or gold."""
    name = name.lower()
    return name in {".git", ".aws", ".ssh", ".gnupg", ".codex", ".hermes", ".docker", ".isolation",
                    "evaluation-private", "gold", "gold.json", "auth.json", "credentials", "credentials.json",
                    "credentials.toml", "id_rsa", "id_ed25519", ".env", ".env.local", ".env.production"} or name.endswith((".pem", ".key", ".fleet.json"))


def _workdir(value, config=None):
    _require(isinstance(value, str) and (value == "/work" or value.startswith("/work/")) and len(value) <= 1024
             and all(part not in {"", ".", ".."} and not _hard_private_name(part) for part in value.split("/")[1:])
             and "\\" not in value and "\0" not in value, "sealed candidate workdir required")
    if config is not None:
        relative = value.removeprefix("/work").lstrip("/")
        path = config.candidate_root / relative
        if relative == ".runtime" or relative.startswith(".runtime/"):
            path = config.runtime_root / relative.removeprefix(".runtime").lstrip("/")
        _no_symlinks(path, directory=True)
    return value


def _read_member(root: Path, path: Path, expected, *, max_bytes=512 * 1024 * 1024, git_blob=None, native=None):
    """Open through no-follow directory descriptors, including race windows."""
    if native is not None:
        return native.member_digest(root, path, expected, max_bytes=max_bytes, git_blob=git_blob)
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
            blob = hashlib.sha1(("blob " + str(before.st_size) + "\0").encode()) if git_blob else None
            count = 0
            while chunk := stream.read(1024 * 1024):
                count += len(chunk)
                _require(count <= max_bytes, "mount byte bound exceeded during capture")
                digest.update(chunk)
                if blob is not None:
                    blob.update(chunk)
            if blob is not None:
                _require(blob.hexdigest() == git_blob, "tracked source blob changed")
            after = os.fstat(stream.fileno())
        current = os.stat(parts[-1], dir_fd=directory, follow_symlinks=False)
    finally:
        os.close(directory)
    def identity(value):
        return (value.st_dev, value.st_ino, value.st_size, value.st_mode, value.st_mtime_ns, value.st_ctime_ns)
    _require(identity(expected) == identity(before) == identity(after) == identity(current), "mount source changed during capture")
    return digest.hexdigest()


def _mount_info(config, path):
    if config.native_binding is not None:
        return config.native_binding.info(path)
    _no_symlinks(path)
    return path.lstat()


def _native_manifest(root, native, *, candidate, max_bytes, exceptions, source_members):
    view = native.inventory(root, max_bytes=max_bytes)
    rows, paths, seen_exceptions, seen_members = [], set(), set(), set()
    ancestors = {str(parent) for name in source_members for parent in Path(name).parents if str(parent) != "."}
    for row in view["entries"]:
        relative = row["path"];parts = Path(relative).parts
        _require(isinstance(relative, str) and relative and not relative.startswith("/") and "\\" not in relative
                 and str(Path(relative)) == relative and all(part not in {"", ".", ".."} for part in parts)
                 and relative not in paths and len(paths) < 100000, "native manifest member scope refused")
        paths.add(relative)
        permitted = candidate and (relative in exceptions and relative in _SOURCE_EXCEPTIONS
            or relative in source_members or relative in ancestors and row["kind"] == "dir")
        _require((not _private_name(parts[-1]) or permitted)
                 and (not any(_private_name(part) for part in parts[:-1]) or permitted), "private native mount member refused")
        _require(row["kind"] in {"dir", "file"} and type(row["mode"]) is int and 0 <= row["mode"] <= 0o777,
                 "special native mount member refused")
        if candidate and relative.startswith(".runtime/"):
            _require(False, "candidate runtime mountpoint must be empty")
        if row["kind"] == "file":
            _require(type(row["size"]) is int and row["size"] >= 0 and re.fullmatch(r"[a-f0-9]{64}", row["sha256"]),
                     "native file digest unproved")
            member = source_members.get(relative)
            if member is not None:
                _require(row["sha256"] == member.sha256 and row["git_blob"] == member.git_blob, "tracked source member changed")
                seen_members.add(relative)
            if relative in exceptions:
                _require(row["sha256"] == exceptions[relative], "trusted source exception content changed")
                seen_exceptions.add(relative)
            rows.append((relative, row["mode"], row["sha256"]))
    _require(view["bytes"] <= max_bytes and seen_exceptions == set(exceptions) and seen_members == set(source_members),
             "native manifest bounds or required source missing")
    return _digest(sorted(rows))


def _manifest(root: Path, *, candidate=False, max_bytes=512 * 1024 * 1024, exceptions=None, source_members=None, native=None):
    if native is not None:
        return _native_manifest(root, native, candidate=candidate, max_bytes=max_bytes,
                                exceptions=exceptions or {}, source_members=source_members or {})
    rows = []
    total = 0
    members = 0
    exceptions = exceptions or {}
    seen_exceptions = set()
    source_members = source_members or {}
    seen_members = set()
    ancestors = {str(parent) for name in source_members for parent in Path(name).parents if str(parent) != "."}
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
            permitted_source = candidate and (relative in exceptions and relative in _SOURCE_EXCEPTIONS
                or relative in source_members or relative in ancestors and name in folders)
            _require((not _private_name(name) or permitted_source) and not path.is_symlink(), "private or symlink mount member refused")
            _require(not any(_private_name(part) for part in Path(relative).parts[:-1]) or permitted_source,
                     "private source ancestor requires an exact permitted member")
            info = path.lstat()
            _require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode), "special mount member refused")
            if stat.S_ISREG(info.st_mode):
                total += info.st_size
                _require(total <= max_bytes and len(rows) < 100000, "mount corpus bound exceeded")
                member = source_members.get(relative)
                digest = _read_member(root, path, info, max_bytes=max_bytes, git_blob=member.git_blob if member else None)
                if member is not None:
                    _require(digest == member.sha256, "tracked source member changed")
                    seen_members.add(relative)
                if relative in exceptions:
                    _require(digest == exceptions[relative], "trusted source exception content changed")
                    seen_exceptions.add(relative)
                rows.append((relative, stat.S_IMODE(info.st_mode), digest))
    _require(seen_exceptions == set(exceptions), "trusted source exception missing")
    _require(seen_members == set(source_members), "tracked source member missing")
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
    _require(type(config.require_rust) is bool and type(config.candidate_readonly) is bool, "explicit runtime/mount policy required")
    for path in (config.python_executable, config.rustc_executable, config.cargo_executable):
        _container_executable(path)
    _workdir(config.working_directory, config)
    trusted = (*config.trusted_source_files, *config.trusted_wrappers, *config.trusted_source_members)
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
    _require(len(config.trusted_source_members) <= 64, "tracked source permit bound exceeded")
    source_members = {}
    for member in config.trusted_source_members:
        _require(isinstance(member, TrustedSourceMember) and isinstance(member.path, str)
                 and 0 < len(member.path) <= 1024 and not member.path.startswith("/") and "\\" not in member.path
                 and all(part not in {"", ".", ".."} and not _hard_private_name(part) for part in member.path.split("/"))
                 and re.fullmatch(r"[a-f0-9]{64}", member.sha256) and re.fullmatch(r"[a-f0-9]{40}", member.git_blob)
                 and member.path not in source_members and member.path not in exceptions,
                 "exact tracked source permit required")
        source_members[member.path] = member
    wrapper_commands = {"/work/" + relative for relative in wrappers}
    for relative, expected in wrappers.items():
        path = config.candidate_root / relative
        info = _mount_info(config, path)
        _require(info.st_mode & 0o111 and _read_member(config.candidate_root, path, info, native=config.native_binding) == expected, "trusted wrapper content/mode changed")
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
    reserved = {"HOME", "TMPDIR", "TMP", "TEMP", "CARGO_HOME", "RUSTUP_HOME", "CARGO_TARGET_DIR", "CARGO_NET_OFFLINE", "PYTHONDONTWRITEBYTECODE", "XDG_CACHE_HOME", "PATH", "RUSTC", "CARGO", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTUP_TOOLCHAIN", "DEBIAN_FRONTEND"}
    for key, value in config.environment:
        _require(isinstance(key, str) and re.fullmatch(r"[A-Z][A-Z0-9_]{0,63}", key)
                 and key not in reserved and key not in {"LD_PRELOAD", "LD_LIBRARY_PATH", "BASH_ENV", "ENV"}
                 and not any(word in key for word in ("TOKEN", "SECRET", "PASSWORD", "CREDENTIAL")), "unsafe callback environment key")
        _require(isinstance(value, str) and "\0" not in value and len(value) <= 4096, "bounded callback environment value required")
    # The immutable quiet wrapper has one fixed output path. Only a registered,
    # hash-verified protected wrapper receives this runtime-backed child mount.
    if _quiet_log_mount(config):
        for path in (config.candidate_root / "target", config.candidate_root / "target" / "quiet"):
            if not path.exists():
                path.mkdir(mode=0o755)
            _no_symlinks(path, directory=True)
        _require(not any((config.candidate_root / "target" / "quiet").iterdir()),
                 "quiet log mountpoint must contain no source files")
        logs = config.runtime_root / "quiet-logs"
        if not logs.exists():
            logs.mkdir(mode=0o777)
        _no_symlinks(logs, directory=True)
        logs.chmod(0o777)
    if config.native_binding is not None:
        _require(not config.readonly_caches, "native stages do not expose external caches")
        config.native_binding.prepare(config.candidate_root, config.runtime_root, quiet=_quiet_log_mount(config))
    # Runtime artifacts, including protected wrapper logs, share one measured
    # durable state tree. Empty source mountpoints add no hidden source bytes.
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
    sources = {"candidate": _manifest(config.candidate_root, candidate=True, exceptions=exceptions, source_members=source_members, native=config.native_binding), "grader": _manifest(config.grader_root, native=config.native_binding),
               "runtime": _manifest(config.runtime_root, max_bytes=config.runtime_max_bytes, native=config.native_binding),
               "caches": [_manifest(Path(cache.source), max_bytes=config.cache_max_bytes) for cache in config.readonly_caches]}
    payload = {"engine": str(config.engine), "engine_digest": engine_digest, "endpoint": config.endpoint,
               "image": config.image, "root": str(root), "mounts": [(str(p), target, ro) for p, target, ro in _mounts(config)],
               "mount_identity": [(_mount_info(config, path).st_dev, _mount_info(config, path).st_ino) for path, _, _ in _mounts(config)],
               "quiet_log_mount": _quiet_log_mount(config),
               "mountpoint_identity": [(str(path.relative_to(config.candidate_root)), _mount_info(config, path).st_dev, _mount_info(config, path).st_ino)
                                       for path in _candidate_mountpoints(config)],
               "sources": sources, "argv": config.acceptance_argv, "candidate_readonly": config.candidate_readonly, "working_directory": config.working_directory, "environment": sorted(_environment(config).items()),
               "limits": [config.timeout_seconds, config.max_output_bytes, config.memory_bytes, config.cpus, config.pids_limit,
                          config.runtime_max_bytes, config.cache_max_bytes],
               "source_archive": {"commit": config.source_commit, "sha256": config.source_archive_sha256,
                                  "exceptions": sorted(exceptions.items()), "wrappers": sorted(wrappers.items()),
                                  "members": sorted((v.path, v.sha256, v.git_blob) for v in source_members.values())},
               "user": [config.uid, config.gid], "runtimes": [config.python_executable, config.rustc_executable, config.cargo_executable, config.require_rust],
               "supervisor": _bytes_digest(_SUPERVISOR.encode()), "probe": _bytes_digest(_PROBE_CODE.encode()),
               "outcome_controller": _bytes_digest(_OUTCOME_CONTROLLER.encode()),
               "role_import_bootstrap": _bytes_digest(_ROLE_IMPORT_BOOTSTRAP.encode()),
               "executor": _bytes_digest(Path(__file__).read_bytes())}
    if config.native_binding is not None:payload["native_store"] = config.native_binding.public()
    return private, payload


def _environment(config):
    return {"HOME": "/work/.runtime/home", "TMPDIR": "/work/.runtime/tmp", "TMP": "/work/.runtime/tmp",
            "TEMP": "/work/.runtime/tmp", "CARGO_HOME": "/work/.runtime/cargo", "RUSTUP_HOME": "/work/.runtime/rustup",
            "CARGO_TARGET_DIR": "/work/.runtime/target", "XDG_CACHE_HOME": "/work/.runtime/cache",
            "CARGO_NET_OFFLINE": "true", "PYTHONDONTWRITEBYTECODE": "1", "DEBIAN_FRONTEND": "noninteractive",
            "PATH": ":".join(dict.fromkeys((str(Path(config.python_executable).parent), str(Path(config.rustc_executable).parent), str(Path(config.cargo_executable).parent), "/usr/bin", "/bin"))),
            "RUSTC": config.rustc_executable, "CARGO": config.cargo_executable, "RUSTC_WRAPPER": "",
            "RUSTC_WORKSPACE_WRAPPER": "", "RUSTUP_TOOLCHAIN": "1.97.1",
            **({"CARGO_BUILD_JOBS": "1", "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0"} if config.require_rust else {}), **dict(config.environment)}


def _quiet_log_mount(config):
    return any(entry.path == "scripts/quiet" for entry in config.trusted_wrappers)


def _candidate_mountpoints(config):
    points = [config.candidate_root / ".runtime"]
    if _quiet_log_mount(config):
        points.extend((config.candidate_root / "target", config.candidate_root / "target/quiet"))
    return points


def _mounts(config):
    logs = [(config.runtime_root / "quiet-logs", "/work/target/quiet", False)] if _quiet_log_mount(config) else []
    return [(config.candidate_root, "/work", config.candidate_readonly), (config.runtime_root, "/work/.runtime", False),
            (config.grader_root, "/grader", True), *logs,
            *((Path(cache.source), cache.target, True) for cache in config.readonly_caches)]


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
                 "--init", "--user", f"{config.uid}:{config.gid}", "--workdir=" + config.working_directory, "--ipc=private", "--cgroupns=private"]
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
    _require(container["User"] == f"{config.uid}:{config.gid}" and container["WorkingDir"] == config.working_directory
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


_GUEST_ERRNO_LABELS = frozenset({"EPERM", "ENOENT", "EACCES", "EROFS", "ENETUNREACH", "EHOSTUNREACH"})


def _guest_errno_valid(protocol):
    """Interpret only the hash-pinned isolated probe's guest builtin namespace."""
    if (not isinstance(protocol, dict) or set(protocol) != {"schema_version", "platform", "codes"}
            or type(protocol["schema_version"]) is not int or protocol["schema_version"] != 1
            or protocol["platform"] != "linux"):
        return False
    codes = protocol["codes"]
    return (isinstance(codes, dict) and set(codes) == _GUEST_ERRNO_LABELS
            and all(type(value) is int and 0 < value <= 4095 for value in codes.values())
            and len(set(codes.values())) == len(codes))


def _guest_error_matches(observation, protocol, allowed):
    # Raw numbers are checked against the guest's own builtin table, never the
    # host's errno module. This table has authority only through _configuration_probe.
    if (not _guest_errno_valid(protocol) or not isinstance(observation, dict)
            or type(observation.get("errno")) is not int or not isinstance(observation.get("error"), str)):
        return False
    code, label = observation["errno"], observation["error"]
    if label not in allowed:
        return False
    if label == "OK":
        return code == 0
    return code > 0 and label in protocol["codes"] and protocol["codes"][label] == code


def _network_denied(network, guest_errno):
    """Prove inactive external topology, rather than assuming device names."""
    if not isinstance(network, dict) or set(network) != {"interfaces", "links", "ipv4_routes", "ipv6_routes", "errno", "error"}:
        return False
    links = network.get("links")
    if (not isinstance(links, dict) or not 0 < len(links) <= 256 or "lo" not in links
            or any(not isinstance(name, str) for name in links)
            or network.get("interfaces") != sorted(links)):
        return False
    for name, state in links.items():
        if (not isinstance(state, dict) or set(state) != {"flags", "operstate"}
                or type(state["flags"]) is not int or not 0 <= state["flags"] < 2**32
                or not isinstance(state["operstate"], str)):
            return False
        flags, operstate = state["flags"], state["operstate"]
        if name == "lo":
            # Linux reports operstate=unknown for the ordinary UP loopback.
            if not flags & 0x8 or operstate not in {"unknown", "up", "down"}:
                return False
            if (operstate == "up" and not flags & 0x1) or (operstate == "down" and flags & 0x1):
                return False
        elif operstate != "down" or flags & (0x1 | 0x40):
            # IFF_UP and IFF_RUNNING must both be clear on dormant kernel links.
            return False
    for family in ("ipv4_routes", "ipv6_routes"):
        rows = network.get(family)
        if not isinstance(rows, list) or len(rows) > 4096:
            return False
        fields = ({"interface", "destination", "gateway", "flags", "mask"} if family == "ipv4_routes" else
                  {"interface", "destination", "destination_prefix", "source", "source_prefix", "gateway", "flags"})
        for row in rows:
            if (not isinstance(row, dict) or set(row) != fields or row["interface"] != "lo"
                    or type(row["flags"]) is not int or not 0 <= row["flags"] < 2**32):
                return False
            addresses = ("destination", "gateway", "mask") if family == "ipv4_routes" else ("destination", "source", "gateway")
            width = 8 if family == "ipv4_routes" else 32
            if any(not isinstance(row[key], str) or not re.fullmatch(r"[a-f0-9]{" + str(width) + r"}", row[key]) for key in addresses):
                return False
            if family == "ipv6_routes" and any(type(row[key]) is not int or not 0 <= row[key] <= 128 for key in ("destination_prefix", "source_prefix")):
                return False
    return _guest_error_matches(network, guest_errno, {"ENETUNREACH", "EHOSTUNREACH", "EACCES", "EPERM"})


def _configuration_probe(config, private):
    name = "plurx-knowledge-" + uuid.uuid4().hex
    sentinel = private / ("outside-" + uuid.uuid4().hex)
    sentinel.write_bytes(os.urandom(32))
    sentinel.chmod(0o600)
    sentinel_digest = _bytes_digest(sentinel.read_bytes())
    _OWNED_CONTAINERS.add(name)
    observations = None
    cleanup = False
    native_sentinel = None
    try:
        if config.native_binding is not None:
            native_sentinel = config.native_binding.outside_begin(sentinel, sentinel.read_bytes())
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
        if config.native_binding is not None:
            _require(native_sentinel is not None and observed.get("native_mounts") == config.native_binding.mounted_identities(config),
                     "native mounted guest identity unproved")
        guest_errno = observed["guest_errno"]
        _require(_guest_errno_valid(guest_errno), "guest errno protocol unproved")
        _require(observed["outside"]["readable"] is False and
                 _guest_error_matches(observed["outside"], guest_errno, {"ENOENT", "EACCES"}), "outside sentinel readable")
        _require(_network_denied(observed["network"], guest_errno), "network denial unproved")
        _require(observed["filesystem"]["candidate"]["writable"] is (not config.candidate_readonly)
                 and _guest_error_matches(observed["filesystem"]["candidate"], guest_errno,
                                          {"EROFS", "EACCES"} if config.candidate_readonly else {"OK"})
                 and observed["filesystem"]["runtime"]["writable"] is True
                 and _guest_error_matches(observed["filesystem"]["runtime"], guest_errno, {"OK"})
                 and observed["filesystem"]["grader"]["writable"] is False
                 and _guest_error_matches(observed["filesystem"]["grader"], guest_errno, {"EROFS", "EACCES"}), "filesystem permissions unproved")
        quiet = observed["filesystem"]["quiet_logs"]
        _require(quiet["writable"] is _quiet_log_mount(config) and
                 _guest_error_matches(quiet, guest_errno, {"OK"} if _quiet_log_mount(config) else {"EROFS", "EACCES", "ENOENT"}),
                 "protected quiet log write boundary unproved")
        _require(bool(re.fullmatch(r"3\.[0-9]+\.[0-9]+", observed["versions"]["python"])), "Python runtime unavailable")
        if config.require_rust:
            build_keys = ("CARGO_BUILD_JOBS", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG")
            _require(observed["build_environment"] == {key: _environment(config).get(key) for key in build_keys},
                     "Rust build environment unproved")
            rust = observed["versions"]["rustc"]
            cargo = observed["versions"]["cargo"]
            _require(rust["returncode"] == 0 and re.match(r"rustc 1\.97\.1(?: |$)", rust["output"]), "pinned Rust runtime unavailable")
            _require(cargo["returncode"] == 0 and cargo["output"].startswith("cargo "), "Cargo runtime unavailable")
        _require(_bytes_digest(sentinel.read_bytes()) == sentinel_digest, "outside sentinel changed")
        observations = {"security": security, "observed_probes": observed, "sentinel_digest": sentinel_digest}
        if native_sentinel is not None:observations["native_outside_sentinel"] = native_sentinel
        return name, observations
    finally:
        # Successful callers retain this live container for their one callback;
        # every refusal cleans it before propagating an unavailable result.
        if observations is None:
            cleanup = _cleanup(config, name)
            if not cleanup:
                raise IsolationError("container cleanup unproved")
        try:
            if native_sentinel is not None:
                try:config.native_binding.outside_end(native_sentinel)
                except BaseException:
                    # A late guest-cleanup refusal must not strand the otherwise
                    # successful probe container before its handle is returned.
                    if observations is not None and not _cleanup(config, name):
                        raise IsolationError("container cleanup unproved after native sentinel refusal")
                    raise
        finally:
            try:
                sentinel.unlink(missing_ok=True)
            except OSError:
                pass


def probe(config: IsolationConfig) -> ProbeReceipt:
    global _PROBE_SEQUENCE
    _PROBE_SEQUENCE += 1
    sequence = _PROBE_SEQUENCE
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
    body = {"status": "verified" if observations is not None and cleaned else "unavailable", "sequence": sequence,
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
            "limits": payload["limits"], "user": payload["user"], "controller": _bytes_digest(_OUTCOME_CONTROLLER.encode()),
            "quiet_log_mount": payload["quiet_log_mount"], "role_import_bootstrap": payload["role_import_bootstrap"],
            "probe": payload["probe"], "executor": payload["executor"]}


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
    _require(_read_member(config.grader_root, path, _mount_info(config, path), native=config.native_binding) == driver.sha256, "family driver changed")
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


def qualify_family(config: IsolationConfig, family: FamilyPlan | FamilyPlanV2, receipt: ProbeReceipt) -> PrerequisiteReceipt:
    """Actually qualify baseline prerequisites; security/version probes do not.

    Frozen gold assertions need not pass on the baseline. Only separately named
    import/build/dependency prerequisites with a reviewed completion driver can
    qualify. Unsupported families remain unavailable without scoring a patch.
    """
    if isinstance(family, FamilyPlanV2):
        return _qualify_plan(config, family, receipt)
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


def _worker_plan_data(worker):
    _require(isinstance(worker, ProtectedWorkerPlan) and isinstance(worker.config, IsolationConfig), "typed protected worker required")
    descriptor = _plan_data(FamilyPlan("protected-worker", (worker.argv,), (worker.argv,), worker.driver))
    allowed = {"call", "constant", "cache_get", "cache_set", "cache_clear", "cache_update", "cache_keys",
               "cache_snapshot", "cache_root", "health", "git_stub", "git_restore"}
    _require(0 < len(worker.operations) <= len(allowed) and len(set(worker.operations)) == len(worker.operations)
             and set(worker.operations) <= allowed, "closed RPC operations required")
    _require(all(isinstance(name, str) and re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,63}", name) for name in worker.exception_types)
             and len(set(worker.exception_types)) == len(worker.exception_types) <= 32, "closed RPC exception types required")
    for digest in (worker.schema_sha256, worker.proxy_sha256):
        _require(isinstance(digest, str) and re.fullmatch(r"[a-f0-9]{64}", digest), "pinned RPC schema/proxy required")
    for value, low, high in ((worker.max_frame_bytes, 1024, 65536), (worker.max_requests, 1, 100000),
                             (worker.max_total_bytes, 1024, 64 * 1024 * 1024)):
        _require(type(value) is int and low <= value <= high, "finite RPC bounds required")
    return {"driver": descriptor["driver"], "argv": worker.argv, "operations": worker.operations,
            "schema_sha256": worker.schema_sha256, "proxy_sha256": worker.proxy_sha256,
            "exception_types": worker.exception_types, "max_frame_bytes": worker.max_frame_bytes,
            "max_requests": worker.max_requests, "max_total_bytes": worker.max_total_bytes}


def _worker_configuration(config, plan, step=None):
    worker = plan.worker
    _worker_plan_data(worker)
    current = replace(worker.config, environment=config.environment, working_directory=config.working_directory)
    trusted_install = step is not None and step.kind == "setup" and "candidate" in step.writes and step.protected_readonly is False
    _require((config.candidate_readonly or trusted_install) and current.candidate_readonly and config.uid != current.uid,
             "protected source policy and readonly worker/distinct UID required")
    _require(worker.argv in current.acceptance_argv and current.python_executable == worker.argv[0], "registered fixed worker runtime required")
    _require(not current.trusted_wrappers and not _quiet_log_mount(current), "worker cannot receive protected wrapper logs")
    for left, target, readonly in _mounts(config):
        for right, other_target, other_readonly in _mounts(current):
            shared_cache = (readonly and other_readonly and target.startswith("/offline-cache/")
                            and other_target.startswith("/offline-cache/") and left == right)
            _require(shared_cache or not left.is_relative_to(right) and not right.is_relative_to(left), "protected/worker mounts overlap")
    private, payload = _prepare(current)
    expected = {worker.driver.path, "rpc-contract.json"}
    actual = ({row["path"] for row in current.native_binding.inventory(current.grader_root)["entries"] if row["kind"] == "file"}
              if current.native_binding is not None else
              {path.relative_to(current.grader_root).as_posix() for path in current.grader_root.rglob("*") if stat.S_ISREG(path.lstat().st_mode)})
    _require(actual == expected, "worker export must contain only its driver and closed contract")
    for relative, expected_digest in ((worker.driver.path, worker.driver.sha256), ("rpc-contract.json", worker.schema_sha256)):
        path = current.grader_root / relative
        _require(_read_member(current.grader_root, path, _mount_info(current, path), native=current.native_binding) == expected_digest, "worker trusted export changed")
    return current, payload


def _rpc_inert(value, *, depth=0):
    _require(depth <= 16, "RPC nesting bound exceeded")
    if value is None or type(value) in {bool, str}:
        return
    if type(value) is int:
        _require(-(2**63) <= value < 2**63, "bounded RPC integer required")
        return
    if type(value) is float:
        _require(value == value and abs(value) <= 1.7976931348623157e308, "finite RPC number required")
        return
    if isinstance(value, list):
        for item in value:
            _rpc_inert(item, depth=depth + 1)
        return
    _require(isinstance(value, dict) and all(isinstance(key, str) for key in value), "inert RPC JSON required")
    for item in value.values():
        _rpc_inert(item, depth=depth + 1)


def _wire_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            _require(key not in result, "duplicate RPC object key")
            result[key] = value
        return result
    def invalid(value):
        raise IsolationError("non-finite RPC constant")
    return json.loads(raw, object_pairs_hook=unique, parse_constant=invalid)


def _rpc_request(frame, worker, expected):
    _require(isinstance(frame, dict) and set(frame) == {"kind", "request"} and frame["kind"] == "rpc", "protected RPC frame required")
    request = frame["request"]
    _require(isinstance(request, dict) and set(request) == {"protocol", "id", "method", "args", "kwargs"}
             and type(request["protocol"]) is int and request["protocol"] == 2
             and type(request["id"]) is int and request["id"] == expected
             and request["method"] in worker.operations, "closed ordered RPC request required")
    _rpc_inert(request["args"])
    _rpc_inert(request["kwargs"])
    return request


def _rpc_reply(frame, worker, expected):
    _require(isinstance(frame, dict) and set(frame) == {"protocol", "id", "result"}
             and type(frame["protocol"]) is int and frame["protocol"] == 2
             and type(frame["id"]) is int and frame["id"] == expected, "ordered worker reply required")
    result = frame["result"]
    _require(isinstance(result, dict) and (set(result) == {"value"} or set(result) == {"exception"}), "closed worker result branch required")
    if "exception" in result:
        error = result["exception"]
        _require(isinstance(error, dict) and set(error) == {"type", "message"} and error["type"] in worker.exception_types
                 and isinstance(error["message"], str) and len(error["message"]) <= 4096, "declared inert exception required")
    else:
        _rpc_inert(result["value"])
    return frame


def _resource_counts(config, name, observer_uid):
    result = _command(config, ["exec", "--user", str(observer_uid), "--workdir=/", name,
                              config.python_executable, "-I", "-S", "-c", _RESOURCE_CODE])
    _require(result.returncode == 0 and not result.timed_out and not result.output_limited, "worker resource observation unavailable")
    data = json.loads(result.stdout)
    _require(isinstance(data, dict) and set(data) == {"memory.events", "pids.events"}, "worker resource counters unavailable")
    for key, required in (("memory.events", {"oom", "oom_kill", "max"}), ("pids.events", {"max"})):
        _require(isinstance(data[key], dict) and required <= set(data[key])
                 and all(type(value) is int and value >= 0 for value in data[key].values()), "worker resource counter schema")
    return data


def _duplex_broker(config, name, argv, plan, mode, context, worker_config, worker_name):
    """Bounded relay: worker stdout can provide values, never grader outcomes."""
    worker = plan.worker
    secret, nonce = os.urandom(32), uuid.uuid4().hex
    controller_context = {**context, "rpc_limits": {key: getattr(worker, key) for key in
                          ("max_frame_bytes", "max_requests", "max_total_bytes")}}
    protected_args = ["exec", "--interactive", "--workdir=/", name, config.python_executable, "-I", "-S", "-c",
        _PROTECTED_CONTROLLER, "/grader/" + plan.driver.path, plan.driver.sha256, mode, _json(argv),
        _json(controller_context), str(config.max_output_bytes)]
    worker_context = {"protocol": 2, "schema_sha256": worker.schema_sha256, "argv": list(worker.argv)}
    worker_args = ["exec", "--interactive", "--workdir=" + worker_config.working_directory, worker_name,
        worker_config.python_executable, "-I", "-S", "-c", _WORKER_CONTROLLER, "/grader/" + worker.driver.path,
        worker.driver.sha256, _json(worker.argv), _json(worker_context), str(worker.max_frame_bytes)]
    processes = {}
    selector = selectors.DefaultSelector()
    pending = {role: bytearray() for role in ("protected", "worker")}
    buffers = {role: bytearray() for role in pending}
    output = {role + "_" + stream: bytearray() for role in pending for stream in ("stdout", "stderr")}
    deadline = time.monotonic() + min(config.timeout_seconds, worker_config.timeout_seconds)
    total = requests = 0
    waiting = None
    final_packet = None
    timed_out = limited = False
    unavailable = None

    def queue(role, payload):
        raw = _json(payload).encode() + b"\n"
        _require(len(raw) <= worker.max_frame_bytes + 1, "RPC frame too large")
        pending[role].extend(raw)
        stream = processes[role].stdin
        try:
            selector.get_key(stream)
        except KeyError:
            selector.register(stream, selectors.EVENT_WRITE, (role, "stdin"))

    try:
        for role, current, arguments in (("worker", worker_config, worker_args), ("protected", config, protected_args)):
            process = subprocess.Popen([str(current.engine), *arguments], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, env=_engine_environment(current), close_fds=True)
            processes[role] = process
            for stream in (process.stdin, process.stdout, process.stderr):
                os.set_blocking(stream.fileno(), False)
            for label in ("stdout", "stderr"):
                selector.register(getattr(process, label), selectors.EVENT_READ, (role, label))
        queue("protected", {"secret": secret.hex(), "nonce": nonce})
        while selector.get_map():
            if time.monotonic() >= deadline:
                timed_out = True
                raise IsolationError("paired RPC deadline")
            for key, _ in selector.select(min(0.1, max(0, deadline - time.monotonic()))):
                role, stream = key.data
                if stream == "stdin":
                    written = os.write(key.fileobj.fileno(), pending[role])
                    del pending[role][:written]
                    if not pending[role]:
                        selector.unregister(key.fileobj)
                    continue
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    if stream == "stdout":
                        _require(not buffers[role] and final_packet is not None, "premature or partial RPC EOF")
                    continue
                total += len(chunk)
                limit = min(config.max_output_bytes, worker_config.max_output_bytes, worker.max_total_bytes)
                if total > limit:
                    limited = True
                    raise IsolationError("paired RPC total/output bound")
                output[role + "_" + stream].extend(chunk)
                if stream == "stderr":
                    continue
                buffers[role].extend(chunk)
                while b"\n" in buffers[role]:
                    line, _, rest = buffers[role].partition(b"\n")
                    buffers[role] = bytearray(rest)
                    _require(len(line) <= worker.max_frame_bytes, "RPC frame bound")
                    frame = _wire_json(line.decode("utf-8"))
                    if role == "worker":
                        _require(waiting is not None and final_packet is None, "unsolicited worker frame")
                        reply = _rpc_reply(frame, worker, waiting)
                        queue("protected", reply)
                        waiting = None
                    elif isinstance(frame, dict) and frame.get("kind") == "outcome":
                        _require(set(frame) == {"kind", "packet"} and final_packet is None and waiting is None
                                 and requests > 0, "protected completion order")
                        final_packet = frame["packet"]
                        for target in pending:
                            _require(not pending[target], "completion with pending RPC data")
                            processes[target].stdin.close()
                    else:
                        _require(final_packet is None and waiting is None and requests < worker.max_requests, "RPC order/request bound")
                        request = _rpc_request(frame, worker, requests + 1)
                        requests += 1
                        waiting = requests
                        queue("worker", request)
                _require(len(buffers[role]) <= worker.max_frame_bytes, "RPC unterminated frame bound")
        _require(final_packet is not None and waiting is None and all(process.wait(timeout=5) == 0 for process in processes.values()),
                 "paired process completion unavailable")
        body = final_packet["body"]
        signature = hmac.new(secret, _json(body).encode(), hashlib.sha256).hexdigest()
        _require(isinstance(final_packet["hmac"], str) and hmac.compare_digest(signature, final_packet["hmac"]), "protected outcome signature")
        _require(body["nonce"] == nonce and body["argv"] == list(argv) and body["mode"] == mode
                 and body["context"] == controller_context and body["driver_sha256"] == plan.driver.sha256,
                 "protected outcome identity mismatch")
        body["broker_proof"] = {"requests": requests, "bytes": total, "schema_sha256": worker.schema_sha256,
                                "proxy_sha256": worker.proxy_sha256, "topology": "separate-container-stdio-v1"}
    except (IsolationError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        unavailable = type(error).__name__
        body = None
    finally:
        selector.close()
        for process in processes.values():
            if process.poll() is None:
                process.kill()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                body = None
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None and not stream.closed:
                    stream.close()
    raw = _json({key: base64.b64encode(value).decode() for key, value in output.items()}).encode()
    return _Command(0 if body is not None else None, raw, (unavailable or "").encode(), timed_out, limited), body


def _protected_packet(config, name, argv, plan, mode, context):
    driver = config.grader_root / plan.driver.path
    _require(_read_member(config.grader_root, driver, _mount_info(config, driver), native=config.native_binding) == plan.driver.sha256, "protected driver changed")
    current_step = (plan.prerequisite_steps if mode == "prerequisite" else plan.acceptance_steps)[context["step_index"]]
    worker_config, before = _worker_configuration(config, plan, current_step)
    private = worker_config.owned_root / ".isolation"
    worker_name = None
    cleaned = False
    outcome = _Command(None, b"", b"worker unavailable")
    packet = None
    try:
        issued_probe = probe(worker_config)
        _require(_issued_probe(issued_probe, private, _prepare(worker_config)[1]) is not None, "worker isolation unqualified")
        worker_name, observations = _configuration_probe(worker_config, private)
        previous = _ISSUED[id(issued_probe)][1]["observations"]
        _require({key: value for key, value in observations.items() if key != "sentinel_digest"} ==
                 {key: value for key, value in previous.items() if key != "sentinel_digest"}, "worker isolation changed")
        expected_versions = context.get("expected_worker_versions")
        _require(expected_versions is None or observations["observed_probes"]["versions"] == expected_versions,
                 "qualified worker runtime changed")
        _require(_prepare(worker_config)[1] == before, "worker state changed before RPC")
        counters = _resource_counts(worker_config, worker_name, config.uid)
        paired_context = {**context, "protected_sources": _prepare(config)[1]["sources"],
                          "worker_sources": before["sources"], "schema_sha256": plan.worker.schema_sha256,
                          "proxy_sha256": plan.worker.proxy_sha256}
        outcome, packet = _duplex_broker(config, name, argv, plan, mode, paired_context, worker_config, worker_name)
        after_counters = _resource_counts(worker_config, worker_name, config.uid)
        _require(all(after_counters[group].get(key, 0) == counters[group].get(key, 0)
                     for group, keys in (("memory.events", ("oom", "oom_kill", "max")), ("pids.events", ("max",))) for key in keys),
                 "worker resource failure")
        inspected = _command(worker_config, ["inspect", worker_name])
        _require(inspected.returncode == 0 and not inspected.timed_out and not inspected.output_limited, "worker inspection unavailable")
        state = json.loads(inspected.stdout)[0]["State"]
        _require(state.get("Running") is True and state.get("OOMKilled") is False, "worker executor failure")
        _require(packet is not None, "protected assertion packet unavailable")
        packet["worker_proof"] = {"probe_digest": issued_probe.digest, "observations": observations,
                                  "resources_before": counters, "resources_after": after_counters, "engine_state": state}
    except (IsolationError, OSError, ValueError, KeyError, TypeError):
        packet = None
    finally:
        if worker_name is not None:
            cleaned = _cleanup(worker_config, worker_name)
    if not cleaned:
        packet = None
    elif packet is not None:
        packet["worker_proof"]["cleanup"] = cleaned.observations
    return outcome, packet


def _plan_v2_data(plan):
    _require(isinstance(plan, FamilyPlanV2) and type(plan.schema_version) is int and plan.schema_version == 2,
             "versioned typed family plan required")
    _require(0 < len(plan.prerequisite_steps) <= 32 and 0 < len(plan.acceptance_steps) <= 32,
             "bounded ordered step chains required")
    result = {"schema_version": 2, "family": plan.family}
    for name, permitted in (("prerequisite_steps", {"setup", "prerequisite"}),
                            ("acceptance_steps", {"setup", "assertion", "compile"})):
        steps = getattr(plan, name)
        ids = set()
        rows = []
        for step in steps:
            _require(isinstance(step, ExecutionStep) and isinstance(step.id, str)
                     and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", step.id) and step.id not in ids,
                     "unique typed step identity required")
            ids.add(step.id)
            _workdir(step.cwd)
            _require(step.kind in permitted and step.writes <= {"candidate", "runtime"}, "closed step kind/write policy required")
            _require(type(step.protected_readonly) is bool and (step.protected_readonly
                     or step.kind == "setup" and "candidate" in step.writes), "only trusted setup may write protected source")
            _require(len(step.environment) <= 32 and len(dict(step.environment)) == len(step.environment),
                     "unique bounded step environment required")
            rows.append({"id": step.id, "argv": step.argv, "cwd": step.cwd, "environment": step.environment,
                         "writes": sorted(step.writes), "kind": step.kind, "protected_readonly": step.protected_readonly})
        result[name] = rows
    _require(any(step.kind in {"assertion", "compile"} for step in plan.acceptance_steps),
             "setup-only acceptance cannot qualify an outcome")
    legacy = _plan_data(FamilyPlan(plan.family, tuple(s.argv for s in plan.prerequisite_steps),
                                   tuple(s.argv for s in plan.acceptance_steps), plan.driver))
    _require(legacy["driver"] is not None, "immutable structured family driver required")
    result["driver"] = legacy["driver"]
    result["worker"] = _worker_plan_data(plan.worker) if plan.worker is not None else None
    return result


def _step_config(config, step, *, protected=False):
    changes = {"environment": step.environment, "working_directory": step.cwd}
    if protected:
        changes["candidate_readonly"] = step.protected_readonly
    return replace(config, **changes)


def _v2_policy(config, payload, plan, step=None):
    # Staging paths/inodes may differ between baseline and patched exports.
    # Content, runtime identities and every effective step environment may not.
    result = {"plan": _plan_v2_data(plan), "image": payload["image"], "endpoint": payload["endpoint"],
            "engine": payload["engine_digest"], "grader": payload["sources"]["grader"],
            "caches": payload["sources"]["caches"], "source_archive": payload["source_archive"],
            "runtimes": payload["runtimes"], "limits": payload["limits"], "user": payload["user"],
            "registered_argv": payload["argv"], "readonly_targets": [cache.target for cache in config.readonly_caches],
            "effective_environments": [[sorted(_environment(_step_config(config, step)).items()) for step in steps]
                                       for steps in (plan.prerequisite_steps, plan.acceptance_steps)],
            "supervisor": payload["supervisor"], "probe": payload["probe"], "controller": payload["outcome_controller"],
            "candidate_readonly": payload["candidate_readonly"], "quiet_log_mount": payload["quiet_log_mount"],
            "role_import_bootstrap": payload["role_import_bootstrap"], "executor": payload["executor"]}
    if plan.worker is not None:
        worker_config, worker_payload = _worker_configuration(config, plan, step)
        result["candidate_readonly"] = [[part.protected_readonly for part in steps]
                                       for steps in (plan.prerequisite_steps, plan.acceptance_steps)]
        result["worker_policy"] = {key: worker_payload[key] for key in
            ("engine_digest", "endpoint", "image", "argv", "candidate_readonly", "limits", "source_archive", "user", "runtimes")}
        result["worker_policy"]["grader"] = worker_payload["sources"]["grader"]
        result["worker_policy"]["caches"] = worker_payload["sources"]["caches"]
        result["worker_policy"]["quiet_log_mount"] = worker_payload["quiet_log_mount"]
        result["worker_policy"]["role_import_bootstrap"] = worker_payload["role_import_bootstrap"]
        result["broker"] = _bytes_digest(_PROTECTED_CONTROLLER.encode() + _WORKER_CONTROLLER.encode() + _RESOURCE_CODE.encode())
    return result


def _plan_sources(config, payload, plan, step=None):
    result = dict(payload["sources"])
    if plan.worker is not None:
        _, worker_payload = _worker_configuration(config, plan, step)
        result.update({"worker_" + key: value for key, value in worker_payload["sources"].items()})
    return result


def _issued_step(receipt):
    issued = _STEPS.get(id(receipt))
    if issued is None or issued[0] is not receipt:
        return None
    try:
        _no_symlinks(receipt.private_path)
        _require(_bytes_digest(receipt.private_path.read_bytes()) == receipt.digest, "step receipt changed")
    except (OSError, IsolationError):
        return None
    return issued[1]


def _classification_v2(packet, step, mode):
    if packet is None or not all(packet.get(key) is True for key in
            ("driver_started", "driver_completed", "hardened", "resources_verified")) or packet.get("resource_failure") is not False:
        return "unavailable"
    result = packet.get("result")
    if not isinstance(result, dict) or type(result.get("protocol")) is not int or result["protocol"] != 2 or result.get("completed") is not True:
        return "unavailable"
    evidence = result.get("evidence")
    if not isinstance(evidence, dict) or not isinstance(evidence.get("identity"), str) or not evidence["identity"]:
        return "unavailable"
    if not isinstance(evidence.get("sha256"), str) or not re.fullmatch(r"[a-f0-9]{64}", evidence["sha256"]):
        return "unavailable"
    counts = [evidence.get(key) for key in ("completed_checks", "failed_checks", "skipped_checks")]
    if not all(type(count) is int and 0 <= count <= 1000000 for count in counts):
        return "unavailable"
    completed, failed, _ = counts
    if completed == 0 or failed > completed:
        return "unavailable"
    outcome = result.get("outcome")
    if evidence.get("kind") != step.kind:
        return "unavailable"
    if step.kind == "setup":
        return "setup-passed" if outcome == "setup-passed" and failed == 0 else "unavailable"
    if mode == "prerequisite":
        return "prerequisite-passed" if outcome == "prerequisite-passed" and failed == 0 else "unavailable"
    if outcome == "passed" and failed == 0:
        return "passed"
    expected = "compile-failed" if step.kind == "compile" else "assertion-failed"
    return expected if outcome == expected and failed > 0 else "unavailable"


def _unavailable_step(private, plan, mode, index, reason, prerequisite=None):
    body = {"schema_version": 2, "status": "unavailable", "classification": "unavailable", "reason": reason,
            "plan_digest": _digest(_plan_v2_data(plan)), "mode": mode, "index": index,
            "prerequisite_digest": getattr(prerequisite, "digest", "")}
    path = private / ("step-" + uuid.uuid4().hex + ".json")
    return StepReceipt("unavailable", _write_private(path, body), path, "unavailable", plan.family,
                       body["plan_digest"], "", mode, index, body["prerequisite_digest"])


def _run_step(config, plan, index, receipt, *, mode, prerequisite=None, previous=None):
    # Refuse overlapping writers and concurrent reuse of the same chain link.
    root = str(_no_symlinks(config.owned_root, directory=True))
    with _CHAIN_LOCK:
        _require(root not in _ACTIVE_ROOTS and (previous is None or id(previous) not in _RESERVED_STEPS),
                 "step chain already active")
        _ACTIVE_ROOTS.add(root)
        if previous is not None:
            _RESERVED_STEPS.add(id(previous))
    try:
        return _run_step_inner(config, plan, index, receipt, mode=mode, prerequisite=prerequisite, previous=previous)
    finally:
        with _CHAIN_LOCK:
            _ACTIVE_ROOTS.discard(root)
            if previous is not None:
                _RESERVED_STEPS.discard(id(previous))


def _run_step_inner(config, plan, index, receipt, *, mode, prerequisite=None, previous=None):
    plan_data = _plan_v2_data(plan)
    steps = plan.prerequisite_steps if mode == "prerequisite" else plan.acceptance_steps
    _require(type(index) is int and 0 <= index < len(steps), "bounded step index required")
    step = steps[index]
    _require(config.working_directory == step.cwd and tuple(sorted(config.environment)) == step.environment,
             "configuration must match exact sealed step cwd/environment")
    _require(step.argv in config.acceptance_argv, "step argv not registered")
    private, before = _prepare(config)
    _require(plan.worker is None or config.candidate_readonly == step.protected_readonly, "exact sealed protected mount mode required")
    before_sources = _plan_sources(config, before, plan, step)
    policy = _v2_policy(config, before, plan, step)
    policy_digest = _digest(policy)
    issued = _issued_probe(receipt, private, before)
    if issued is None:
        return _unavailable_step(private, plan, mode, index, "fresh-probe-required", prerequisite)
    versions = issued[1]["observations"]["observed_probes"]["versions"]
    parent = _issued_step(previous) if previous is not None else None
    work_identity = {key: before[key] for key in ("root", "mount_identity", "mountpoint_identity")}
    work_identity["mounts"] = [(path, target) for path, target, readonly in before["mounts"]]
    if "native_store" in before:work_identity["native_store"] = before["native_store"]
    if plan.worker is not None:
        _, worker_identity = _worker_configuration(config, plan, step)
        work_identity["worker"] = {key: worker_identity[key] for key in ("root", "mounts", "mount_identity", "mountpoint_identity")}
    baseline = before_sources.get("worker_candidate", before_sources["candidate"])
    qualification = None
    if mode == "acceptance":
        entry = _QUALIFIED.get(id(prerequisite))
        if entry is not None and entry[0] is prerequisite and prerequisite.status == "qualified":
            try:
                _no_symlinks(prerequisite.private_path)
                _require(_bytes_digest(prerequisite.private_path.read_bytes()) == prerequisite.digest, "prerequisite changed")
                qualification = entry[1]
            except (OSError, IsolationError):
                pass
        if (qualification is None or qualification.get("schema_version") != 2
                or qualification.get("policy_digest") != policy_digest or qualification.get("versions") != versions):
            return _unavailable_step(private, plan, mode, index, "qualified-family-required", prerequisite)
        baseline = qualification["baseline_source_digest"]
    if index == 0:
        valid_order = previous is None
        if mode == "acceptance":
            valid_order = valid_order and all(before_sources[key] == qualification["after"][key]
                for key in ("runtime", "worker_runtime") if key in before_sources)
            if plan.worker is not None:
                valid_order = valid_order and before_sources["candidate"] == qualification["initial_protected_source"]
    else:
        valid_order = (parent is not None and id(previous) not in _CONSUMED_STEPS and previous.status == "passed"
                       and previous.mode == mode and previous.index == index - 1
                       and previous.plan_digest == _digest(plan_data) and parent["policy_digest"] == policy_digest
                       and parent["versions"] == versions and parent["after"] == before_sources
                       and parent["work_identity"] == work_identity
                       and parent["prerequisite_digest"] == getattr(prerequisite, "digest", "")
                       and issued[1]["sequence"] >= parent["post_probe_sequence"])
        if parent is not None:
            baseline = parent["baseline_source_digest"]
    if not valid_order:
        return _unavailable_step(private, plan, mode, index, "ordered-state-lineage-required", prerequisite)
    context = {"protocol": 2, "family": plan.family, "plan_digest": _digest(plan_data),
               "step": plan_data["prerequisite_steps" if mode == "prerequisite" else "acceptance_steps"][index],
               "step_index": index, "previous_step_digest": getattr(previous, "digest", ""),
               "prerequisite_digest": getattr(prerequisite, "digest", ""), "baseline_source_digest": baseline,
               "effective_environment": _environment(config)}
    if plan.worker is not None:
        context["expected_worker_versions"] = (qualification or parent or {}).get("worker_versions")
    context = json.loads(_json(context))
    packet = outcome = observed = state = None
    cleaned = False
    name = None
    after = None
    post_issued = None
    after_sources = None
    try:
        name, observed = _configuration_probe(config, private)
        _require({k: v for k, v in observed.items() if k != "sentinel_digest"} ==
                 {k: v for k, v in issued[1]["observations"].items() if k != "sentinel_digest"}, "observed isolation changed")
        _require(_prepare(config)[1] == before and _plan_sources(config, before, plan, step) == before_sources, "step inputs changed before callback")
        if plan.worker is None:
            outcome, packet = _driver_packet(config, name, step.argv, plan, mode, context)
        else:
            outcome, packet = _protected_packet(config, name, step.argv, plan, mode, context)
        inspected = _command(config, ["inspect", name])
        _require(inspected.returncode == 0 and not inspected.timed_out and not inspected.output_limited, "step executor inspection unavailable")
        state = json.loads(inspected.stdout)[0]["State"]
        _require(state.get("Running") is True and state.get("OOMKilled") is False, "step executor resource/start failure")
    except (IsolationError, OSError, ValueError, KeyError, TypeError):
        packet = None
    finally:
        if name is not None:
            cleaned = _cleanup(config, name)
    classification = _classification_v2(packet, step, mode) if cleaned else "unavailable"
    if classification != "unavailable":
        try:
            _, after = _prepare(config)
            _require(_v2_policy(config, after, plan, step) == policy, "immutable step policy changed")
            after_sources = _plan_sources(config, after, plan, step)
            for target in ("candidate", "runtime", "worker_candidate", "worker_runtime"):
                if target in before_sources:
                    write_target = target.removeprefix("worker_")
                    _require(target != "worker_candidate" and write_target in step.writes or before_sources[target] == after_sources[target], "undeclared step state change")
            if plan.worker is not None:
                worker_config, worker_after = _worker_configuration(config, plan, step)
                worker_post = probe(worker_config)
                worker_private, worker_final = _prepare(worker_config)
                _require(_issued_probe(worker_post, worker_private, worker_final) is not None and worker_after == worker_final
                         and _ISSUED[id(worker_post)][1]["observations"]["observed_probes"]["versions"] == packet["worker_proof"]["observations"]["observed_probes"]["versions"],
                         "worker post-step state proof unavailable")
            post_probe = probe(config)
            final_private, final = _prepare(config)
            post_issued = _issued_probe(post_probe, final_private, final)
            _require(post_issued is not None and final == after
                     and _plan_sources(config, final, plan, step) == after_sources
                     and post_issued[1]["observations"]["observed_probes"]["versions"] == versions,
                     "fresh post-step isolation/state proof unavailable")
        except (IsolationError, OSError, ValueError, KeyError, TypeError):
            classification = "unavailable"
    status = "unavailable" if classification == "unavailable" else "failed" if classification.endswith("-failed") else "passed"
    body = {"schema_version": 2, "status": status, "classification": classification, "family": plan.family,
            "plan_digest": _digest(plan_data), "policy_digest": policy_digest, "policy": policy, "mode": mode, "index": index,
            "step": context["step"], "work_identity": work_identity, "previous_step_digest": context["previous_step_digest"],
            "prerequisite_digest": context["prerequisite_digest"], "baseline_source_digest": baseline, "versions": versions,
            "worker_versions": packet["worker_proof"]["observations"]["observed_probes"]["versions"]
                if packet is not None and plan.worker is not None else None,
            "before": before_sources, "after": after_sources,
            "probe_digest": receipt.digest, "post_probe_digest": post_probe.digest if post_issued is not None else "",
            "post_probe_sequence": post_issued[1]["sequence"] if post_issued is not None else 0,
            "cleanup_proved": bool(cleaned), "cleanup_observations": getattr(cleaned, "observations", None),
            "observations": observed, "engine_state": state, "structured_outcome": packet,
            "output": None if outcome is None else {"stdout_base64": base64.b64encode(outcome.stdout).decode(),
                "stderr_base64": base64.b64encode(outcome.stderr).decode(), "returncode": outcome.returncode,
                "timed_out": outcome.timed_out, "output_limited": outcome.output_limited}}
    semantic = {key: body[key] for key in ("plan_digest", "policy_digest", "mode", "index",
                "baseline_source_digest", "versions", "worker_versions", "before", "after", "classification")}
    semantic["previous_binding"] = previous.binding_digest if previous is not None else ""
    semantic["prerequisite_binding"] = prerequisite.binding_digest if prerequisite is not None else ""
    binding = _digest(semantic)
    body["binding_digest"] = binding
    path = private / ("step-" + uuid.uuid4().hex + ".json")
    result = StepReceipt(status, _write_private(path, body), path, classification, plan.family,
                         body["plan_digest"], binding, mode, index, body["prerequisite_digest"])
    if status != "unavailable":
        _STEPS[id(result)] = (result, body)
        if previous is not None:
            _CONSUMED_STEPS.add(id(previous))
    return result


def qualify_step(config: IsolationConfig, plan: FamilyPlanV2, index: int, receipt: ProbeReceipt, *,
                 previous: StepReceipt | None = None) -> StepReceipt:
    """Run one ordered baseline setup/dependency step, never patch assertions."""
    return _run_step(config, plan, index, receipt, mode="prerequisite", previous=previous)


def seal_prerequisites(plan: FamilyPlanV2, steps: tuple[StepReceipt, ...]) -> PrerequisiteReceipt:
    """Seal only the complete, actually completed process-issued baseline chain."""
    plan_data = _plan_v2_data(plan)
    _require(isinstance(steps, tuple) and len(steps) == len(plan.prerequisite_steps), "complete prerequisite chain required")
    records = [_issued_step(step) for step in steps]
    _require(all(record is not None for record in records), "issued prerequisite steps required")
    for index, (step, record) in enumerate(zip(steps, records)):
        _require(step.status == "passed" and step.mode == "prerequisite" and step.index == index
                 and step.plan_digest == _digest(plan_data) and record["policy_digest"] == records[0]["policy_digest"]
                 and record["previous_step_digest"] == (steps[index - 1].digest if index else "")
                 and (index == 0 or record["before"] == records[index - 1]["after"]), "ordered complete prerequisites required")
    final = records[-1]
    binding_data = {"plan": plan_data, "policy_digest": final["policy_digest"], "versions": final["versions"],
                    "baseline_source_digest": records[0]["baseline_source_digest"], "after": final["after"],
                    "worker_versions": final.get("worker_versions"), "initial_protected_source": records[0]["before"]["candidate"],
                    "step_bindings": [step.binding_digest for step in steps]}
    binding = _digest(binding_data)
    body = {**binding_data, "schema_version": 2, "status": "qualified", "family": plan.family,
            "binding_digest": binding, "steps": [step.digest for step in steps]}
    path = steps[-1].private_path.parent / ("prerequisite-" + uuid.uuid4().hex + ".json")
    result = PrerequisiteReceipt("qualified", _write_private(path, body), path, plan.family, binding)
    _QUALIFIED[id(result)] = (result, body)
    return result


def _qualify_plan(config, plan, receipt):
    _plan_v2_data(plan)
    completed = []
    previous = None
    for index, step in enumerate(plan.prerequisite_steps):
        current = _step_config(config, step, protected=plan.worker is not None)
        current_probe = receipt if index == 0 else probe(current)
        result = qualify_step(current, plan, index, current_probe, previous=previous)
        if result.status != "passed":
            path = result.private_path.parent / ("prerequisite-" + uuid.uuid4().hex + ".json")
            body = {"schema_version": 2, "status": "unavailable", "failed_step_digest": result.digest}
            return PrerequisiteReceipt("unavailable", _write_private(path, body), path, plan.family, "")
        completed.append(result)
        previous = result
    return seal_prerequisites(plan, tuple(completed))


def execute_step(config: IsolationConfig, plan: FamilyPlanV2, index: int, receipt: ProbeReceipt, *,
                 prerequisite: PrerequisiteReceipt, previous: StepReceipt | None = None) -> StepReceipt:
    """Run exactly the next sealed step against the freshly probed state."""
    return _run_step(config, plan, index, receipt, mode="acceptance", prerequisite=prerequisite, previous=previous)


def execute_plan(config: IsolationConfig, receipt: ProbeReceipt, *, family: FamilyPlanV2,
                 prerequisite: PrerequisiteReceipt) -> ExecutionResult:
    """Execute ordered setup/checks; setup-only or unknown completion cannot pass."""
    _plan_v2_data(family)
    previous = None
    completed = []
    status = classification = "unavailable"
    for index, step in enumerate(family.acceptance_steps):
        current = _step_config(config, step, protected=family.worker is not None)
        current_probe = receipt if index == 0 else probe(current)
        result = execute_step(current, family, index, current_probe, prerequisite=prerequisite, previous=previous)
        completed.append(result)
        status, classification = result.status, result.classification
        if status != "passed":
            break
        previous = result
    if status == "passed":
        # _plan_v2_data forbids chains with no actual assertion/compile step.
        classification = "passed"
    private = completed[-1].private_path.parent
    body = {"schema_version": 2, "status": status, "classification": classification,
            "family": family.family, "plan_digest": _digest(_plan_v2_data(family)),
            "prerequisite_digest": prerequisite.digest, "steps": [step.digest for step in completed]}
    path = private / ("execution-" + uuid.uuid4().hex + ".json")
    return ExecutionResult(status, _write_private(path, body), path, classification, prerequisite.digest)


def execute(config: IsolationConfig, argv: tuple[str, ...], receipt: ProbeReceipt, *,
            family: FamilyPlan | None = None, prerequisite: PrerequisiteReceipt | None = None) -> ExecutionResult:
    """A raw exit code is never a validated patch outcome (R4).

    Missing dependencies, Docker exec/start errors, OOM/resource failures,
    unsupported families and absent authenticated completion remain unavailable.
    A completed trusted assertion/compiler failure is retained only after the
    exact family's baseline/dependency prerequisites actually qualified.
    """
    _require(not isinstance(family, FamilyPlanV2), "version2 requires ordered execute_step/execute_plan")
    if family is None:
        private, _ = _prepare(config)
        path = private / ("execution-" + uuid.uuid4().hex + ".json")
        body = {"status": "unavailable", "classification": "unavailable", "reason": "family-prerequisite-required"}
        return ExecutionResult("unavailable", _write_private(path, body), path)
    return _execute_family(config, argv, receipt, family, mode="acceptance", prerequisite=prerequisite)
