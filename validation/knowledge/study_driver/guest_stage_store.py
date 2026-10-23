"""Host-owned native guest stage capabilities and a closed filesystem transport.

Provider, checkpoint, outcome and issued isolation-receipt authority stay on the
host. A guest response describes different filesystem objects, never host inodes.
"""
from __future__ import annotations
import base64
from dataclasses import dataclass, field
import hashlib
from functools import wraps
import json
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import time
from types import SimpleNamespace
import uuid

from .guest_stage_helper import (CHUNK_BYTES, FRAME_BYTES, MEMBERS, RESPONSE_BYTES,
    ROOTS, SOURCE_BYTES, TRANSFER_BYTES, _fingerprint, _identity, _json, _relative, _sha, _tree_digest)


class NativeGuestError(ValueError):
    pass


def _require(value, reason):
    if not value:raise NativeGuestError(reason)


def _regular_bytes(path, *, limit=8 * 1024**2):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        _require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size <= limit, "BoundedOriginalHostFileRequired")
        raw = stream.read(limit + 1)
        _require(_fingerprint(before) == _fingerprint(os.fstat(stream.fileno())) == _fingerprint(Path(path).lstat()), "HostFileChanged")
    return raw, before


def _write_receipt(directory, name, body):
    raw = _json(body)
    _require(len(raw) <= RESPONSE_BYTES, "NativeHostReceiptBound")
    fd = os.open(directory / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw);stream.flush();os.fsync(stream.fileno())
    parent = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:os.fsync(parent)
    finally:os.close(parent)
    info = (directory / name).lstat()
    return {"path": directory / name, "sha256": _sha(raw), "bytes": len(raw), "identity": _identity(info)}


@dataclass(frozen=True)
class GuestTransportSpec:
    executable: Path
    executable_sha256: str
    arguments: tuple[str, ...]
    environment: tuple[tuple[str, str], ...]
    expected_binding: dict
    source_hashes: dict
    timeout_seconds: int = 1800

    def __post_init__(self):
        object.__setattr__(self, "executable", Path(self.executable).resolve())
        object.__setattr__(self, "arguments", tuple(self.arguments))
        object.__setattr__(self, "environment", tuple(tuple(row) for row in self.environment))
        _require(self.executable.is_absolute() and re.fullmatch(r"[a-f0-9]{64}", self.executable_sha256)
                 and 1 <= len(self.arguments) <= 64 and self.arguments.count("{session}") == 1
                 and all(type(arg) is str and "\0" not in arg and "\n" not in arg and len(arg) <= 4096 for arg in self.arguments),
                 "PinnedGuestTransportRequired")
        environment = dict(self.environment)
        _require(len(environment) == len(self.environment) and set(environment) <= {"PATH", "HOME", "LIMA_HOME"}
                 and all(type(value) is str and "\0" not in value and len(value) <= 4096 for value in environment.values()),
                 "ClosedGuestTransportEnvironment")
        _require(type(self.timeout_seconds) is int and 1 <= self.timeout_seconds <= 1800
                 and type(self.source_hashes) is dict and 1 <= len(self.source_hashes) <= 64
                 and all(re.fullmatch(r"[a-f0-9]{64}", value) for value in self.source_hashes.values()), "BoundedGuestTransportRequired")
        _require(set(self.expected_binding) == {"filesystem_uuid", "boot_id", "identities", "ancestry", "mounts", "alias", "physical_work", "control"},
                 "ExactNativeBindingRequired")


class FramedGuestTransport:
    """One exact child process; no commands or filesystem paths supplied by roles."""
    def __init__(self, spec, *, session):
        _require(type(spec) is GuestTransportSpec, "TypedGuestTransportRequired")
        raw, _ = _regular_bytes(spec.executable, limit=128 * 1024**2)
        _require(_sha(raw) == spec.executable_sha256 and os.access(spec.executable, os.X_OK), "GuestTransportExecutableChanged")
        self.spec, self.session, self.sequence = spec, session, 0
        self.buffer = bytearray();self.errors = bytearray();self.failed = False
        self.process = subprocess.Popen([str(spec.executable), *(session if arg == "{session}" else arg for arg in spec.arguments)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=dict(spec.environment), close_fds=True)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):os.set_blocking(stream.fileno(), False)

    def _ready(self, descriptor, event, deadline):
        with selectors.DefaultSelector() as selector:
            selector.register(descriptor, event)
            if descriptor != self.process.stderr.fileno():selector.register(self.process.stderr, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                _require(remaining > 0, "GuestOperationTimedOut")
                events = selector.select(min(remaining, 1.0))
                for key, observed in events:
                    if key.fd == descriptor:return
                    chunk = os.read(self.process.stderr.fileno(), 4096)
                    if chunk:
                        self.errors.extend(chunk);_require(len(self.errors) <= 65536, "GuestTransportErrorBound")
                    else:selector.unregister(self.process.stderr)
                _require(self.process.poll() is None or events, "GuestTransportEnded")

    def _send(self, body, deadline):
        raw = _json(body) + b"\n"
        _require(len(raw) <= FRAME_BYTES, "GuestRequestFrameBound")
        offset = 0
        while offset < len(raw):
            self._ready(self.process.stdin.fileno(), selectors.EVENT_WRITE, deadline)
            try:count = os.write(self.process.stdin.fileno(), raw[offset:])
            except BlockingIOError:continue
            _require(count > 0, "GuestRequestIncomplete");offset += count

    def _receive(self, deadline):
        while b"\n" not in self.buffer:
            self._ready(self.process.stdout.fileno(), selectors.EVENT_READ, deadline)
            try:chunk = os.read(self.process.stdout.fileno(), 65536)
            except BlockingIOError:continue
            _require(chunk, "GuestResponseIncomplete")
            self.buffer.extend(chunk);_require(len(self.buffer) <= FRAME_BYTES + 65536, "GuestResponseFrameBound")
        raw, _, rest = self.buffer.partition(b"\n");self.buffer = bytearray(rest)
        _require(len(raw) < FRAME_BYTES, "GuestResponseFrameBound")
        def pairs(items):
            value = {}
            for key, item in items:
                _require(key not in value, "DuplicateGuestResponseKey");value[key] = item
            return value
        value = json.loads(raw, object_pairs_hook=pairs,
                           parse_constant=lambda value: (_ for _ in ()).throw(NativeGuestError("NonfiniteGuestNumber")))
        _require(type(value) is dict, "GuestResponseObjectRequired")
        return value, len(raw)

    def exchange(self, op, body, *, entries=None, blob=None):
        _require(not self.failed, "GuestTransportPoisoned")
        deadline = time.monotonic() + self.spec.timeout_seconds
        sequence = self.sequence
        try:
            self._send({"sequence": sequence, "session": self.session, "op": op, "body": body}, deadline)
            def chunks(values):
                for chunk in values:
                    _require(type(chunk) is bytes and 0 < len(chunk) <= CHUNK_BYTES, "HostTransferChunkBound")
                    self._send({"sequence": sequence, "kind": "chunk", "data": base64.b64encode(chunk).decode()}, deadline)
                self._send({"sequence": sequence, "kind": "file_end"}, deadline)
            if blob is not None:chunks(blob)
            if entries is not None:
                for row in entries:
                    value = {key: item for key, item in row.items() if key != "chunks"}
                    self._send({"sequence": sequence, "kind": "entry", "entry": value}, deadline)
                    if row["kind"] == "file" and "seed_path" not in row:chunks(row["chunks"])
                self._send({"sequence": sequence, "kind": "entries_end"}, deadline)
            response, size = self._receive(deadline)
            _require(set(response) == {"sequence", "status", "body", "entries"} and response["sequence"] == sequence
                     and response["status"] == "ok" and type(response["body"]) is dict
                     and type(response["entries"]) is int and 0 <= response["entries"] <= MEMBERS, "GuestOperationUnproved")
            result = response["body"];rows = []
            for _ in range(response["entries"]):
                frame, count = self._receive(deadline);size += count
                _require(size <= RESPONSE_BYTES and set(frame) == {"sequence", "entry"} and frame["sequence"] == sequence
                         and type(frame["entry"]) is dict, "GuestObservationBound")
                rows.append(frame["entry"])
            ending, _ = self._receive(deadline)
            _require(ending == {"sequence": sequence, "end": True}, "GuestResponseEndRequired")
            if op == "observe":result["entries"] = rows
            else:_require(not rows, "UnexpectedGuestObservation")
            self.sequence += 1
            return result
        except BaseException:
            self.failed = True;self.close()
            raise

    def close(self):
        process = getattr(self, "process", None)
        if process is None:return
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:stream.close()
        if process.poll() is None:
            process.terminate()
            try:process.wait(timeout=3)
            except subprocess.TimeoutExpired:process.kill();process.wait(timeout=3)
        self.process = None


@dataclass(frozen=True)
class GuestRootRef:
    capability: str
    device: int
    inode: int


@dataclass
class GuestStageRef:
    host_root: Path
    host_identity: tuple[int, int]
    stage: str
    session: str
    identity: tuple[int, int]
    roots: dict[str, GuestRootRef]
    transfer_sha256: str
    pinned: bool
    proofs: dict = field(default_factory=dict)
    closed: bool = False
    retired: bool = False


@dataclass(frozen=True)
class NativeGuestBinding:
    store: object
    stage: GuestStageRef

    def public(self):
        self.store._ref(self.stage)
        return {"kind": "guest-native-stage-v1", "session": self.stage.session, "stage": self.stage.stage,
                "stage_identity": list(self.stage.identity), "transfer_sha256": self.stage.transfer_sha256, "binding_sha256": self.store.binding_sha256,
                "source_hashes": self.store.transport_spec.source_hashes,
                "roots": self.store._root_data(self.stage)}

    def prepare(self, candidate, runtime, *, quiet):
        _, source_role, relative = self.store._locate(candidate)
        _, runtime_role, other = self.store._locate(runtime)
        _require(not relative and not other, "FixedNativeMountRootsRequired")
        self.store._exchange("fixed_layout", {"stage_id": self.stage.stage, "candidate_role": source_role,
                                               "runtime_role": runtime_role, "quiet": quiet})

    def inventory(self, root, *, max_bytes=SOURCE_BYTES):
        return self.store.observe(root, max_bytes=max_bytes)

    def info(self, path):
        view = self.store.observe(path, metadata_only=True)
        values = view["entries"][0]["stat"] if view["entries"] else view["root_stat"]
        _require(type(values) is list and len(values) == 6 and all(type(value) is int for value in values), "GuestStatRequired")
        return SimpleNamespace(**dict(zip(("st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns", "st_ctime_ns"), values)))

    def outside_begin(self, path, content):
        _require(Path(path).parent == self.stage.host_root / ".isolation" and type(content) is bytes and len(content) == 32,
                 "FixedHostOutsideChallengeRequired")
        result = self.store._exchange("outside_sentinel", {"stage_id": self.stage.stage, "name": Path(path).name,
            "content_base64": base64.b64encode(content).decode()})
        _require(result.get("scope") == "outside-all-role-mounts" and result.get("stage") == self.stage.stage
                 and result.get("name") == Path(path).name and result.get("present") is True and result.get("bytes") == 32
                 and result.get("sha256") == _sha(content) and result.get("mode") == 0o600
                 and result.get("device") == self.stage.identity[0] and type(result.get("inode")) is int and result["inode"] > 0,
                 "GuestOutsideSentinelUnproved")
        return result

    def outside_end(self, result):
        response = self.store._exchange("outside_sentinel", {"stage_id": self.stage.stage, "name": result["name"], "expected": result})
        _require(response == dict(result, present=False, removed=True), "GuestOutsideSentinelCleanupUnproved")
        return response

    def mounted_identities(self, config):
        result={}
        for path,target in ((config.candidate_root,"/work"),(config.runtime_root,"/work/.runtime"),(config.grader_root,"/grader")):
            info=self.info(path);result[target]=[info.st_dev,info.st_ino]
        return result

    def member_digest(self, root, path, expected, *, max_bytes, git_blob=None):
        view = self.store.observe(path, max_bytes=max_bytes)
        rows = view["entries"]
        _require(len(rows) == 1 and rows[0]["kind"] == "file" and tuple(rows[0]["stat"]) == _fingerprint(expected), "GuestTrustedMemberChanged")
        if git_blob is not None:_require(rows[0]["git_blob"] == git_blob, "GuestTrustedBlobChanged")
        return rows[0]["sha256"]


# Original native receipts plus their later immutable archive share the same
# external 2 GiB run budget. Reserve this entire original-receipt allowance in
# SourceStager before archive writes, rather than double-counting free space.
HOST_AUDIT_MAX = 64 * 1024**2


def _fail_closed(method):
    @wraps(method)
    def checked(self, *args, **kwargs):
        try:return method(self, *args, **kwargs)
        except BaseException as error:
            self.failed = True
            # Exact trusted transport diagnostics only, never candidate-tree logs.
            try:
                transport = self.transport
                self._audit_receipt(self._audit, "failure-" + uuid.uuid4().hex + ".json", {
                    "operation": method.__name__, "status": "unknown-or-refused", "error_class": type(error).__name__,
                    "sequence": getattr(transport, "sequence", None),
                    "stderr_base64": base64.b64encode(bytes(getattr(transport, "errors", b""))).decode()})
            except BaseException:pass  # The original failure still forbids later calls.
            self.close_transport()
            raise
    return checked


class NativeGuestStageStore:
    """One operator-bound backend. No candidate/environment backend selection."""
    def __init__(self, transport, *, owned_root, baseline, mirror_max_bytes, admission, receipt_root):
        _require(type(transport) is GuestTransportSpec and type(mirror_max_bytes) is int
                 and TRANSFER_BYTES <= mirror_max_bytes <= 128 * 1024**3, "BoundedNativeStoreRequired")
        from .runtime_bindings import no_links
        self.owned_root, self.receipt_root = no_links(owned_root), no_links(receipt_root)
        for path in (self.owned_root, self.receipt_root):
            _require(path.is_dir() and stat.S_IMODE(path.stat().st_mode) == 0o700, "PrivateNativeHostRootRequired")
        _require(self.owned_root.stat().st_dev != self.receipt_root.stat().st_dev
                 and not self.receipt_root.is_relative_to(self.owned_root) and not self.owned_root.is_relative_to(self.receipt_root)
                 and Path(transport.expected_binding["alias"]) == self.owned_root, "SeparateNativeHostEvidenceRequired")
        self.host_identity, self.receipt_identity = _identity(self.owned_root.stat()), _identity(self.receipt_root.stat())
        self.transport_spec, self.baseline, self.admission = transport, baseline, admission
        self.mirror_max_bytes = mirror_max_bytes;self.session = uuid.uuid4().hex
        self.stages, self.reservations = {}, {};self.failed = False;self.transport = None;self._audit_bytes = 0
        self.binding_sha256 = _sha(_json(transport.expected_binding));self.seed_rows = {}
        self._audit = self.receipt_root / self.session
        self._audit.mkdir(mode=0o700)
        parent = os.open(self.receipt_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:os.fsync(parent)
        finally:os.close(parent)

    def _audit_receipt(self, directory, name, body):
        amount = len(_json(body)) + 4096
        _require(self._audit_bytes + amount <= HOST_AUDIT_MAX, "NativeOriginalReceiptBudgetExceeded")
        self._audit_bytes += amount  # A failed write cannot reset the allowance.
        return _write_receipt(directory, name, body)

    def _check(self):
        self.admission.assert_current()
        _require(not self.failed and _identity(self.owned_root.lstat()) == self.host_identity
                 and _identity(self.receipt_root.lstat()) == self.receipt_identity
                 and not self.owned_root.is_symlink() and not self.receipt_root.is_symlink(), "NativeHostStoreUnavailable")

    @_fail_closed
    def reserve(self, root):
        self._check()
        _require(Path(root).parent == self.owned_root and re.fullmatch(r"[a-f0-9]{32}", Path(root).name)
                 and Path(root).name not in self.reservations, "OwnedMirrorReservationRequired")
        used = sum(row["bytes"] for row in self.reservations.values())
        _require(used + TRANSFER_BYTES <= self.mirror_max_bytes, "NativeHostMirrorQuotaExceeded")
        self.reservations[Path(root).name] = {"bytes": TRANSFER_BYTES, "published": False}

    def _chunks(self, path, expected):
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, "rb") as stream:
            _require(_fingerprint(os.fstat(stream.fileno())) == _fingerprint(expected), "HostTransferSourceChanged")
            while chunk := stream.read(CHUNK_BYTES):yield chunk
            _require(_fingerprint(os.fstat(stream.fileno())) == _fingerprint(expected) == _fingerprint(Path(path).lstat()), "HostTransferSourceChanged")

    def _ensure(self):
        self._check()
        if self.transport is not None:return
        try:
            self.transport = FramedGuestTransport(self.transport_spec, session=self.session)
            bound = self.transport.exchange("bind", {})
            _require(set(bound) == {"binding", "source_hashes"}
                     and _json(bound["binding"]) == _json(self.transport_spec.expected_binding)
                     and bound["source_hashes"] == self.transport_spec.source_hashes, "GuestSourceOrEnvironmentChanged")
            self._audit_receipt(self._audit, "binding.json", bound)
            if self.baseline is None:return  # Explicit public fixture transfer; no frozen-source seed claim.
            archive_info = self.baseline.path.lstat()
            _require(_fingerprint(archive_info) == _fingerprint(self.baseline.initial) and self.baseline._stable(),
                     "OriginalFrozenArchiveRequired")
            permits = {"schema_version": 1, "source_commit": self.baseline.commit, "source_tree": self.baseline.tree,
                       "archive_sha256": self.baseline.expected, "members": list(self.baseline.permits.values())}
            result = self.transport.exchange("seed", {"size": archive_info.st_size, "archive_sha256": self.baseline.expected,
                "source_commit": self.baseline.commit, "source_tree": self.baseline.tree, "source_exceptions": self.baseline.exceptions,
                "source_permits": permits}, blob=self._chunks(self.baseline.path, archive_info))
            _require(result == {"archive_sha256": self.baseline.expected, "source_commit": self.baseline.commit,
                                "source_tree": self.baseline.tree}, "GuestFrozenSeedUnproved")
            _require(self.baseline._stable(), "OriginalFrozenArchiveChanged")
            self._audit_receipt(self._audit, "seed.json", result)
            for name in self.baseline.rows:
                raw = self.baseline.read_raw(name);self.seed_rows[name] = (_sha(raw), len(raw))
        except BaseException:
            self.failed = True
            if self.transport is not None:self.transport.close()
            raise

    def _exchange(self, op, body, **kwargs):
        self._ensure();self._check()
        mutating = op in {"publish", "fixed_layout", "copy_qualified_pair", "outside_sentinel", "close", "retire", "finish"}
        sequence = self.transport.sequence
        if mutating:
            self._audit_receipt(self._audit, "request-" + str(sequence) + ".json", {
                "sequence": sequence, "operation": op, "body": body, "status": "issued-outcome-unknown"})
        try:
            result = self.transport.exchange(op, body, **kwargs)
            if mutating:
                self._audit_receipt(self._audit, "response-" + str(sequence) + ".json", {
                    "sequence": sequence, "operation": op, "response_sha256": _sha(_json(result)), "status": "returned"})
            return result
        except BaseException:
            self.failed = True
            raise

    @staticmethod
    def _root_data(ref):
        return {role: {"capability": value.capability, "device": value.device, "inode": value.inode} for role, value in ref.roots.items()}

    def _ref(self, ref):
        self._check()
        _require(type(ref) is GuestStageRef and self.stages.get(ref.stage) is ref and not ref.retired
                 and ref.session == self.session and _identity(ref.host_root.lstat()) == ref.host_identity
                 and not ref.host_root.is_symlink(), "OriginalNativeStageRequired")
        return ref

    def _locate(self, path):
        path = Path(path)
        _require(path.is_relative_to(self.owned_root), "NativePathOutsideStage")
        parts = path.relative_to(self.owned_root).parts
        _require(len(parts) >= 2 and parts[0] in self.stages, "RegisteredNativePathRequired")
        ref = self._ref(self.stages[parts[0]])
        _require(parts[1] in ref.roots, "RegisteredNativeRoleRequired")
        relative = "/".join(parts[2:])
        if relative:_relative(relative)
        return ref, parts[1], relative

    def _response_roots(self, result, *, stage):
        _require(result.get("stage") == stage and result.get("session") == self.session
                 and type(result.get("identity")) is list and len(result["identity"]) == 2
                 and type(result.get("roots")) is dict, "GuestStageResponseMismatch")
        roots = {}
        for role, row in result["roots"].items():
            _require(role in ROOTS and set(row) == {"capability", "device", "inode"}
                     and re.fullmatch(r"[a-f0-9]{32}", row["capability"]) and type(row["device"]) is int
                     and type(row["inode"]) is int and row["inode"] > 0 and row["device"] >= 0
                     and row["device"] == result["identity"][0], "OriginalGuestRootRequired")
            roots[role] = GuestRootRef(**row)
        _require(set(roots) == {"candidate", "grader", "runtime"} or set(roots) == ROOTS, "CompleteGuestRootsRequired")
        return roots

    def _proof(self, ref, name, body):
        self._ref(ref)
        directory = self._audit / ref.stage
        directory.mkdir(mode=0o700, exist_ok=True)
        record = self._audit_receipt(directory, name, body)
        ref.proofs["native-" + name] = record
        return record

    @_fail_closed
    def publish(self, handle, *, pinned=False):
        self._check();stage = handle.root.name
        _require(stage in self.reservations and not self.reservations[stage]["published"]
                 and handle.root.parent == self.owned_root, "ReservedNativeStageRequired")
        rows, total = [], 0
        for role in sorted(ROOTS):
            root = handle.root / role
            if not root.exists():continue
            _require(root.is_dir() and not root.is_symlink(), "NativeHostMirrorDirectoryRequired")
            rows.append({"path": role, "kind": "dir", "mode": stat.S_IMODE(root.lstat().st_mode)})
            for directory, folders, files in os.walk(root, followlinks=False):
                for name in sorted(folders):
                    path = Path(directory) / name;info = path.lstat()
                    _require(stat.S_ISDIR(info.st_mode), "HostMirrorLinkRefused")
                    rows.append({"path": path.relative_to(handle.root).as_posix(), "kind": "dir", "mode": stat.S_IMODE(info.st_mode)})
                for name in sorted(files):
                    path = Path(directory) / name;info = path.lstat()
                    _require(stat.S_ISREG(info.st_mode) and info.st_size <= SOURCE_BYTES, "HostMirrorFileBound")
                    digest = hashlib.sha256()
                    for chunk in self._chunks(path, info):digest.update(chunk)
                    row = {"path": path.relative_to(handle.root).as_posix(), "kind": "file", "mode": stat.S_IMODE(info.st_mode),
                           "size": info.st_size, "sha256": digest.hexdigest()}
                    total += info.st_size;_require(total <= TRANSFER_BYTES and len(rows) < 6 * MEMBERS, "HostMirrorCorpusBound")
                    row["chunks"] = self._chunks(path, info)
                    relative = path.relative_to(root).as_posix()
                    if role in {"candidate", "worker-source"} and self.seed_rows.get(relative) == (row["sha256"], row["size"]):row["seed_path"] = relative
                    rows.append(row)
        # Seed is initialized before deciding which original files can be reused.
        self._ensure()
        for row in rows:
            if row["kind"] == "file" and row["path"].split("/", 1)[0] in {"candidate", "worker-source"}:
                relative = row["path"].split("/", 1)[1]
                if self.seed_rows.get(relative) == (row["sha256"], row["size"]):row["seed_path"] = relative
        manifest = [{key: value for key, value in row.items() if key not in {"chunks", "seed_path"}} for row in rows]
        _require(len(_json(manifest)) <= RESPONSE_BYTES, "HostTransferManifestBound")
        expected = _sha(_json(manifest))
        self._audit_receipt(self._audit, "transfer-intent-" + stage + ".json", {
            "stage": stage, "host_root_identity": list(_identity(handle.root.lstat())),
            "host_manifest_sha256": expected, "bytes": total, "pinned": pinned})
        result = self._exchange("publish", {"stage": stage, "pinned": pinned}, entries=rows)
        roots = self._response_roots(result, stage=stage)
        _require(result.get("transfer_sha256") == expected and result.get("transferred_bytes") == total
                 and result.get("pinned") is pinned, "GuestTransferManifestMismatch")
        ref = GuestStageRef(handle.root, _identity(handle.root.lstat()), stage, self.session, tuple(result["identity"]), roots, expected, pinned)
        self.stages[stage] = ref
        self._proof(ref, "transfer.json", {"host_root_identity": list(ref.host_identity), "host_manifest_sha256": expected,
            "guest": result, "binding_sha256": self.binding_sha256, "source_hashes": self.transport_spec.source_hashes})
        self.reservations[stage] = {"bytes": total, "published": True}
        return NativeGuestBinding(self, ref)

    @_fail_closed
    def observe(self, path, *, max_bytes=SOURCE_BYTES, metadata_only=False):
        ref, role, relative = self._locate(path)
        result = self._exchange("observe", {"stage_id": ref.stage, "role": role, "relative": relative or None,
            "max_bytes": max_bytes, "metadata_only": metadata_only})
        _require(result.get("stage") == ref.stage and result.get("role") == role
                 and result.get("root") == self._root_data(ref)[role] and type(result.get("entries")) is list,
                 "GuestObservationRootChanged")
        return result

    def validate(self, handle):
        binding = handle.native_binding
        _require(type(binding) is NativeGuestBinding and binding.store is self, "OriginalNativeBindingRequired")
        self._ref(binding.stage)
        self.observe(handle.candidate, metadata_only=True)

    def measure(self, path, *, candidate=False, max_bytes=SOURCE_BYTES):
        view = self.observe(path, max_bytes=max_bytes)
        if candidate:_require(not any(row["path"].startswith(".runtime/") for row in view["entries"]), "NativeCandidateRuntimeNotEmpty")
        rows = [(row["path"], row["mode"], row["sha256"]) for row in view["entries"] if row["kind"] == "file"]
        return _tree_digest(rows)

    @_fail_closed
    def copy_qualified(self, handle, baseline, body):
        ref, source = self._ref(handle.native_binding.stage), self._ref(baseline.native_binding.stage)
        old = self._root_data(ref)
        names = {"runtime", "worker_runtime"} if "worker-runtime" in ref.roots else {"runtime"}
        result = self._exchange("copy_qualified_pair", {"stage_id": ref.stage, "baseline_id": source.stage,
            "expected_roots": old, "digests": {name: body["after"][name] for name in names}})
        roots = self._response_roots(result, stage=ref.stage)
        _require(tuple(result["identity"]) == ref.identity and result.get("pinned") is ref.pinned
                 and all(roots[name] == ref.roots[name] for name in set(ref.roots) - {"runtime", "worker-runtime"}),
                 "GuestCopyScopeChanged")
        transition = {"namespace": "guest-native", "before": old, "after": result["roots"],
                      "baseline_stage": source.stage, "prerequisite_sha256": body.get("receipt_sha256")}
        self._proof(ref, "adoption.json", transition)
        ref.roots = roots
        return transition

    @_fail_closed
    def close(self, handle):
        ref = self._ref(handle.native_binding.stage)
        if ref.closed:return
        result = self._exchange("close", {"stage_id": ref.stage})
        _require(self._response_roots(result, stage=ref.stage) == ref.roots and tuple(result["identity"]) == ref.identity,
                 "GuestCloseRootChanged")
        self._proof(ref, "closure.json", result);ref.closed = True

    @_fail_closed
    def proofs(self, handle):
        ref = self._ref(handle.native_binding.stage)
        _require(ref.closed and {"native-transfer.json", "native-closure.json"} <= set(ref.proofs), "NativeStageClosureRequired")
        for record in ref.proofs.values():
            raw, info = _regular_bytes(record["path"], limit=RESPONSE_BYTES)
            _require(_identity(info) == record["identity"] and len(raw) == record["bytes"] and _sha(raw) == record["sha256"],
                     "NativeOriginalProofChanged")
        return dict(ref.proofs)

    @_fail_closed
    def retire(self, handle, authorization, *, terminal, archive_sha256):
        ref = self._ref(handle.native_binding.stage)
        self.proofs(handle)
        result = self._exchange("retire", {"stage_id": ref.stage, "authorization": authorization, "terminal": terminal,
            "expected_roots": self._root_data(ref), "archive_sha256": archive_sha256})
        _require(result.get("stage") == ref.stage and result.get("session") == self.session and result.get("retired") is True
                 and result.get("authorization") == authorization and result.get("archive_sha256") == archive_sha256
                 and result.get("root_handles_closed") is True, "NativeRetirementUnproved")
        proof=self._proof(ref, "retirement.json", result);ref.retired = True
        return dict(result,host_receipt_sha256=proof["sha256"])

    @_fail_closed
    def mirror_retired(self, handle):
        self.reservations.pop(handle.root.name, None)
        if self.stages and all(ref.retired for ref in self.stages.values()):
            result = self._exchange("finish", {})
            _require(result == {"finished": True}, "NativeSeedCleanupUnproved")
            self._audit_receipt(self._audit, "finished.json", result)
            self.transport.close()

    def close_transport(self):
        if self.transport is not None:self.transport.close()
