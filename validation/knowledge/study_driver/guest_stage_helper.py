"""Credential-free native guest filesystem helper; never an outcome authority.

The transport entry point attests the native mount before exposing this private
FD-based core. Only the host authorizes stage publication/copy/retirement.
"""
from __future__ import annotations
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import time
import uuid

ROOTS = frozenset({"candidate", "grader", "runtime", "worker-source", "worker-export", "worker-runtime"})
SOURCE_BYTES = 512 * 1024**2
TRANSFER_BYTES = 2 * 1024**3
MEMBERS = 100000
FRAME_BYTES = 1024**2
CHUNK_BYTES = 32768
RESPONSE_BYTES = 64 * 1024**2


class GuestStageError(RuntimeError):
    pass


def _require(value, reason):
    if not value:
        raise GuestStageError(reason)


def _json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def _sha(value):
    return hashlib.sha256(value).hexdigest()


def _tree_digest(rows):
    # Preserve the pre-existing paired_runtime/evaluation_isolation manifest
    # format, including JSON ASCII escaping for non-ASCII relative paths.
    raw = json.dumps(sorted(rows), sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    return _sha(raw)


def _identity(info):
    return info.st_dev, info.st_ino


def _fingerprint(info):
    return (*_identity(info), info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _relative(value):
    _require(isinstance(value, str) and 0 < len(value) <= 2048 and "\\" not in value
             and str(PurePosixPath(value)) == value and not value.startswith("/")
             and all(part not in {"", ".", ".."} for part in value.split("/")), "InvalidRelativeMember")
    return value.split("/")


def _open_chain(path):
    path = Path(path)
    _require(path.is_absolute(), "AbsoluteOwnedRootRequired")
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    ancestry = [("/", _identity(os.fstat(descriptor)))]
    current = Path("/")
    try:
        for part in path.parts[1:]:
            _relative(part)
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
            current /= part
            ancestry.append((str(current), _identity(os.fstat(descriptor))))
        return descriptor, ancestry
    except BaseException:
        os.close(descriptor)
        raise


def _durable(directory, name, body):
    raw = _json(body)
    _require(len(raw) <= RESPONSE_BYTES, "HelperReceiptBound")
    fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    os.fsync(directory)
    return _sha(raw)


def _copy_runtime(*args, **kwargs):
    from .paired_runtime import copy_qualified_runtime
    return copy_qualified_runtime(*args, **kwargs)


def _remove_tree(parent, name, expected):
    from .paired_runtime import _remove_captured_runtime
    return _remove_captured_runtime(parent, name, expected)


class GuestStageFilesystem:
    """One private guest namespace; this core alone makes no ext4 admission claim."""
    def __init__(self, work_root, control_root, *, session):
        _require(re.fullmatch(r"[a-f0-9]{32}", session) is not None, "InvalidGuestSession")
        self.work_path, self.control_path = Path(work_root), Path(control_root)
        self.work, self.work_ancestry = _open_chain(self.work_path)
        try:
            self.control, self.control_ancestry = _open_chain(self.control_path)
        except BaseException:
            os.close(self.work)
            raise
        for descriptor in (self.work, self.control):
            info = os.fstat(descriptor)
            _require(info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700, "PrivateHelperRootRequired")
        _require(_identity(os.fstat(self.work)) != _identity(os.fstat(self.control))
                 and os.fstat(self.work).st_dev == os.fstat(self.control).st_dev
                 and not self.work_path.is_relative_to(self.control_path)
                 and not self.control_path.is_relative_to(self.work_path), "DistinctNativeControlRequired")
        self.session, self.stages, self.sentinels = session, {}, {}
        self.failed = False
        os.mkdir(session, mode=0o700, dir_fd=self.control)
        self.records = os.open(session, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.control)
        _durable(self.records, "session.json", {"session": session, "work": list(_identity(os.fstat(self.work)))})

    def _check(self):
        _require(not self.failed, "GuestStorePoisoned")
        for path, expected in ((self.work_path, self.work_ancestry), (self.control_path, self.control_ancestry)):
            descriptor, ancestry = _open_chain(path)
            try:
                _require(ancestry == expected, "GuestParentIdentityChanged")
            finally:
                os.close(descriptor)

    def _record(self, stage_id, *, open_only=False):
        self._check()
        record = self.stages.get(stage_id)
        _require(record is not None and record["phase"] not in {"poisoned", "retired"}, "UnknownOrPoisonedGuestStage")
        _require(not open_only or record["phase"] == "open", "ClosedGuestStage")
        directory = os.open(stage_id, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.work)
        try:
            _require(_identity(os.fstat(directory)) == record["identity"], "GuestStageIdentityChanged")
            for role, token in record["roots"].items():
                info = os.stat(role, dir_fd=directory, follow_symlinks=False)
                _require(stat.S_ISDIR(info.st_mode) and _identity(info) == (token["device"], token["inode"]),
                         "GuestChildIdentityChanged")
        finally:
            os.close(directory)
        return record

    def _root(self, stage_id, role):
        record = self._record(stage_id)
        _require(role in record["roots"], "UnknownGuestRole")
        directory = os.open(stage_id, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.work)
        try:
            root = os.open(role, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            token = record["roots"][role]
            _require(_identity(os.fstat(root)) == (token["device"], token["inode"]), "GuestChildIdentityChanged")
            return root
        finally:
            os.close(directory)

    def _public(self, stage_id):
        record = self.stages[stage_id]
        return {"stage": stage_id, "session": self.session, "identity": list(record["identity"]),
                "roots": {role: dict(token) for role, token in record["roots"].items()}, "pinned": record["pinned"]}

    def publish(self, stage_id, entries, *, pinned=False):
        self._check()
        _require(re.fullmatch(r"[a-f0-9]{32}", stage_id) is not None and stage_id not in self.stages
                 and len(self.stages) < 512 and type(pinned) is bool, "FreshGuestStageRequired")
        os.mkdir(stage_id, mode=0o700, dir_fd=self.work)
        directory = os.open(stage_id, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.work)
        record = {"identity": _identity(os.fstat(directory)), "roots": {}, "phase": "publishing", "pinned": pinned}
        self.stages[stage_id] = record
        manifest, seen, total = [], set(), 0
        try:
            for row in entries:
                kind = row.get("kind")
                fields = {"path", "kind", "mode"} | ({"size", "sha256", "chunks"} if kind == "file" else set())
                _require(set(row) == fields and kind in {"dir", "file"}, "ClosedTransferMemberRequired")
                parts = _relative(row["path"])
                _require(parts[0] in ROOTS and row["path"] not in seen and len(seen) < 6 * MEMBERS,
                         "TransferMemberScope")
                _require(type(row["mode"]) is int and row["mode"] in {0o600, 0o644, 0o700, 0o755, 0o777}, "TransferMode")
                seen.add(row["path"])
                parent = os.dup(directory)
                try:
                    for part in parts[:-1]:
                        child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                        os.close(parent);parent = child
                    if kind == "dir":
                        os.mkdir(parts[-1], mode=row["mode"], dir_fd=parent)
                        made = os.open(parts[-1], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                        try:os.fchmod(made, row["mode"])
                        finally:os.close(made)
                        if len(parts) == 1:
                            info = os.stat(parts[-1], dir_fd=parent, follow_symlinks=False)
                            record["roots"][parts[0]] = {"capability": uuid.uuid4().hex, "device": info.st_dev, "inode": info.st_ino}
                    else:
                        _require(len(parts) > 1 and type(row["size"]) is int and 0 <= row["size"] <= SOURCE_BYTES
                                 and re.fullmatch(r"[a-f0-9]{64}", row["sha256"]) is not None, "BoundedTransferFile")
                        total += row["size"]
                        _require(total <= TRANSFER_BYTES, "TransferCorpusBound")
                        output = os.open(parts[-1], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, row["mode"], dir_fd=parent)
                        count, digest = 0, hashlib.sha256()
                        with os.fdopen(output, "wb") as stream:
                            for chunk in row["chunks"]:
                                _require(type(chunk) is bytes and 0 < len(chunk) <= CHUNK_BYTES, "TransferChunkBound")
                                count += len(chunk)
                                _require(count <= row["size"], "TransferFileGrew")
                                digest.update(chunk);stream.write(chunk)
                            _require(count == row["size"] and digest.hexdigest() == row["sha256"], "TransferDigestMismatch")
                            os.fchmod(stream.fileno(), row["mode"]);stream.flush();os.fsync(stream.fileno())
                    manifest.append({key: value for key, value in row.items() if key != "chunks"})
                    os.fsync(parent)
                finally:
                    os.close(parent)
            roles = set(record["roots"])
            _require(roles == {"candidate", "grader", "runtime"} or roles == ROOTS, "CompleteRoleTransferRequired")
            record["phase"] = "open"
            record["transfer_sha256"] = _sha(_json(manifest))
            result = dict(self._public(stage_id), transfer_sha256=record["transfer_sha256"], transferred_bytes=total)
            _durable(self.records, stage_id + "-published.json", result)
            self._record(stage_id)
            return result
        except BaseException:
            record["phase"] = "poisoned"
            raise
        finally:
            os.close(directory)

    def observe(self, stage_id, role, *, relative=None, max_bytes=SOURCE_BYTES, metadata_only=False):
        _require(type(max_bytes) is int and 0 < max_bytes <= 128 * 1024**3, "ObservationBound")
        root = self._root(stage_id, role)
        entries, total = [], 0
        def walk(directory, prefix, selected=None):
            nonlocal total
            before = sorted(os.listdir(directory)) if selected is None else [selected]
            for name in before:
                _relative(name)
                path = prefix + name
                info = os.stat(name, dir_fd=directory, follow_symlinks=False)
                _require(info.st_dev == os.fstat(root).st_dev and len(entries) < MEMBERS, "ObservationScopeBound")
                _require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode), "GuestLinksOrSpecialFilesRefused")
                row = {"path": path, "kind": "dir" if stat.S_ISDIR(info.st_mode) else "file", "mode": stat.S_IMODE(info.st_mode),
                       "stat": list(_fingerprint(info)), "size": info.st_size}
                if row["kind"] == "dir":
                    entries.append(row)
                    if selected is None:
                        child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
                        try:
                            _require(_fingerprint(os.fstat(child)) == _fingerprint(info), "GuestEntryChanged")
                            walk(child, path + "/")
                        finally:
                            os.close(child)
                else:
                    total += info.st_size
                    _require(total <= max_bytes, "GuestManifestByteBound")
                    file = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
                    with os.fdopen(file, "rb") as stream:
                        _require(_fingerprint(os.fstat(stream.fileno())) == _fingerprint(info), "GuestEntryChanged")
                        digest = hashlib.sha256();blob = hashlib.sha1(("blob " + str(info.st_size) + "\0").encode())
                        count = 0
                        while chunk := stream.read(1024**2):
                            count += len(chunk);_require(count <= info.st_size, "GuestFileGrew")
                            digest.update(chunk);blob.update(chunk)
                        _require(count == info.st_size and _fingerprint(os.fstat(stream.fileno())) == _fingerprint(info), "GuestEntryChanged")
                    row.update(sha256=digest.hexdigest(), git_blob=blob.hexdigest());entries.append(row)
                _require(_fingerprint(os.stat(name, dir_fd=directory, follow_symlinks=False)) == _fingerprint(info), "GuestEntryChanged")
            if selected is None:_require(sorted(os.listdir(directory)) == before, "GuestMembershipChanged")
        try:
            root_info = os.fstat(root)
            if metadata_only:
                _require(relative is None or isinstance(relative, str), "MetadataMemberRequired")
                if relative:
                    parts = _relative(relative);parent = os.dup(root)
                    try:
                        for part in parts[:-1]:
                            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                            os.close(parent);parent = child
                        info = os.stat(parts[-1], dir_fd=parent, follow_symlinks=False)
                        _require(info.st_dev == root_info.st_dev and (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)), "GuestLinksOrSpecialFilesRefused")
                        entries = [{"path": relative, "kind": "dir" if stat.S_ISDIR(info.st_mode) else "file",
                                    "mode": stat.S_IMODE(info.st_mode), "stat": list(_fingerprint(info)), "size": info.st_size}]
                    finally:os.close(parent)
                self._record(stage_id)
                return {"stage": stage_id, "role": role, "root_stat": list(_fingerprint(root_info)), "entries": entries,
                        "bytes": 0, "root": dict(self.stages[stage_id]["roots"][role])}
            if relative is None:
                walk(root, "")
            else:
                parts = _relative(relative);parent = os.dup(root)
                try:
                    for part in parts[:-1]:
                        child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                        os.close(parent);parent = child
                    walk(parent, "/".join(parts[:-1]) + ("/" if len(parts) > 1 else ""), selected=parts[-1])
                finally:os.close(parent)
                _require(len(entries) == 1, "GuestMemberMissing")
            self._record(stage_id)
            return {"stage": stage_id, "role": role, "root_stat": list(_fingerprint(root_info)), "entries": entries,
                    "bytes": total, "root": dict(self.stages[stage_id]["roots"][role])}
        finally:
            os.close(root)

    def layout(self, stage_id, candidate_role, runtime_role, *, quiet=False):
        self._record(stage_id, open_only=True)
        _require((candidate_role, runtime_role) in {("candidate", "runtime"), ("worker-source", "worker-runtime")}
                 and type(quiet) is bool and (not quiet or candidate_role == "candidate"), "FixedRoleLayoutRequired")
        def child(parent, name, mode, *, empty=False):
            try:os.mkdir(name, mode=mode, dir_fd=parent)
            except FileExistsError:pass
            fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
            if empty:_require(not os.listdir(fd), "GuestMountpointNotEmpty")
            os.fchmod(fd, mode)
            return fd
        source, runtime = self._root(stage_id, candidate_role), self._root(stage_id, runtime_role)
        try:
            point = child(source, ".runtime", 0o755, empty=True);os.close(point)
            if quiet:
                target = child(source, "target", 0o755)
                try:point = child(target, "quiet", 0o755, empty=True);os.close(point)
                finally:os.close(target)
                logs = child(runtime, "quiet-logs", 0o777);os.close(logs)
            for name in ("tmp", "home", "cargo", "rustup", "target", "cache"):
                fd = child(runtime, name, 0o777);os.close(fd)
            os.fsync(source);os.fsync(runtime)
            return self._public(stage_id)
        finally:
            os.close(source);os.close(runtime)

    def copy_qualified_pair(self, stage_id, baseline_id, expected_roots, digests):
        current, baseline = self._record(stage_id, open_only=True), self._record(baseline_id)
        _require(stage_id != baseline_id and baseline["pinned"] and current["roots"] == expected_roots, "QualifiedGuestBaselineRequired")
        names = ("runtime", "worker-runtime") if "worker-runtime" in current["roots"] else ("runtime",)
        _require(set(digests) == {"runtime", "worker_runtime"} if len(names) == 2 else set(digests) == {"runtime"}, "QualifiedRuntimeDigestsRequired")
        replacements = {}
        try:
            for role in names:
                old = expected_roots[role]
                token = _copy_runtime(self.work_path / baseline_id / role, self.work_path / stage_id / role,
                    digests["runtime" if role == "runtime" else "worker_runtime"],
                    expected_target_identity=(old["device"], old["inode"]), expected_parent_identity=current["identity"])
                replacements[role] = {"capability": uuid.uuid4().hex, "device": token[0], "inode": token[1]}
            for role, old in current["roots"].items():
                token = replacements.get(role, old)
                fd = os.open(self.work_path / stage_id / role, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                try:_require(_identity(os.fstat(fd)) == (token["device"], token["inode"]), "GuestChildIdentityChanged")
                finally:os.close(fd)
            self._record(baseline_id)
            self._check()
            _require(_identity((self.work_path / stage_id).lstat()) == current["identity"], "GuestStageIdentityChanged")
            now = time.time_ns()
            for role in ("candidate", "worker-source"):
                if role not in current["roots"]:continue
                # Same fresh-source ordering as the original host stager: copied
                # build artifacts must never mask a new stage's source mtimes.
                for directory, folders, files in os.walk(self.work_path / stage_id / role, followlinks=False):
                    parent = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                    try:
                        for name in folders:
                            _require(stat.S_ISDIR(os.stat(name, dir_fd=parent, follow_symlinks=False).st_mode), "GuestSourceLink")
                        for name in files:
                            _require(stat.S_ISREG(os.stat(name, dir_fd=parent, follow_symlinks=False).st_mode), "GuestSourceLink")
                            os.utime(name, ns=(now, now), dir_fd=parent, follow_symlinks=False)
                    finally:os.close(parent)
            current["roots"].update(replacements)
            result = self._public(stage_id)
            _durable(self.records, stage_id + "-adopted.json", result)
            return result
        except BaseException:
            current["phase"] = "poisoned"
            raise

    def outside_sentinel(self, stage_id, name, *, content=None, expected=None):
        record = self._record(stage_id)
        _require(re.fullmatch(r"outside-[a-f0-9]{32}", name) is not None, "FixedOutsideSentinelRequired")
        parent = os.open(stage_id, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.work)
        try:
            _require(_identity(os.fstat(parent)) == record["identity"], "GuestStageIdentityChanged")
            try:os.mkdir(".isolation", mode=0o700, dir_fd=parent)
            except FileExistsError:pass
            private = os.open(".isolation", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
            try:
                info = os.fstat(private)
                _require(info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700, "PrivateSentinelParentRequired")
                key = (stage_id, name)
                if content is not None:
                    _require(type(content) is bytes and len(content) == 32 and expected is None and key not in self.sentinels,
                             "ExactOutsideChallengeRequired")
                    file = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=private)
                    with os.fdopen(file, "wb") as stream:
                        stream.write(content);stream.flush();os.fsync(stream.fileno());info = os.fstat(stream.fileno())
                    result = {"scope": "outside-all-role-mounts", "stage": stage_id, "name": name,
                              "device": info.st_dev, "inode": info.st_ino, "sha256": _sha(content), "bytes": 32,
                              "mode": stat.S_IMODE(info.st_mode), "present": True}
                    self.sentinels[key] = result
                else:
                    _require(expected == self.sentinels.get(key) and expected is not None, "OriginalOutsideSentinelRequired")
                    file = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=private)
                    with os.fdopen(file, "rb") as stream:
                        info = os.fstat(stream.fileno());raw = stream.read(33)
                        _require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_size == 32
                                 and _identity(info) == (expected["device"], expected["inode"])
                                 and _sha(raw) == expected["sha256"]
                                 and _fingerprint(info) == _fingerprint(os.stat(name, dir_fd=private, follow_symlinks=False)),
                                 "OutsideSentinelChanged")
                    os.unlink(name, dir_fd=private);del self.sentinels[key]
                    result = dict(expected, present=False, removed=True)
                os.fsync(private)
                return result
            finally:os.close(private)
        finally:os.close(parent)

    def close(self, stage_id):
        record = self._record(stage_id)
        _require(not any(key[0] == stage_id for key in self.sentinels), "OutsideSentinelCleanupRequired")
        if record["phase"] == "closed":return self._public(stage_id)
        closure = {}
        for role in ("candidate", "grader", "worker-source", "worker-export"):
            if role in record["roots"]:
                view = self.observe(stage_id, role)
                rows = [(r["path"], r["mode"], r["sha256"]) for r in view["entries"] if r["kind"] == "file"]
                closure[role] = _tree_digest(rows)
        record["phase"] = "closed"
        result = dict(self._public(stage_id), source_identity=closure)
        _durable(self.records, stage_id + "-closed.json", result)
        return result

    def retire(self, stage_id, authorization, *, terminal=False, expected_roots, archive_sha256):
        record = self._record(stage_id)
        _require(record["phase"] == "closed" and (not record["pinned"] or terminal is True), "ClosedUnpinnedGuestStageRequired")
        _require(set(authorization) == {"receipt_sha256", "checkpoint_sha256", "event"}
                 and authorization["event"] == ("journal_finished" if terminal else "acceptance_finished")
                 and all(re.fullmatch(r"[a-f0-9]{64}", authorization[k]) for k in ("receipt_sha256", "checkpoint_sha256"))
                 and re.fullmatch(r"[a-f0-9]{64}", archive_sha256) and record["roots"] == expected_roots,
                 "HostRetirementAuthorizationRequired")
        before = os.fstatvfs(self.work)
        capture = "retire-" + uuid.uuid4().hex
        _durable(self.records, stage_id + "-retirement-intent.json", {"authorization": authorization,
            "archive_sha256": archive_sha256, "stage": self._public(stage_id)})
        try:
            os.mkdir(capture, mode=0o700, dir_fd=self.work)
            quarantine = os.open(capture, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.work)
            try:
                os.rename(stage_id, "old", src_dir_fd=self.work, dst_dir_fd=quarantine)
                _require(_identity(os.stat("old", dir_fd=quarantine, follow_symlinks=False)) == record["identity"], "CapturedGuestStageChanged")
                _remove_tree(quarantine, "old", record["identity"])
            finally:os.close(quarantine)
            os.rmdir(capture, dir_fd=self.work);os.fsync(self.work)
            self._check()
            after = os.fstatvfs(self.work)
            result = {"stage": stage_id, "session": self.session, "retired": True, "authorization": authorization,
                      "archive_sha256": archive_sha256, "root_handles_closed": True,
                      "free_bytes_before": before.f_bavail * before.f_frsize,
                      "free_bytes_after": after.f_bavail * after.f_frsize}
            _durable(self.records, stage_id + "-retired.json", result)
            record["phase"] = "retired"
            return result
        except BaseException:
            record["phase"] = "poisoned"
            raise

    def shutdown(self):
        for name in ("records", "control", "work"):
            descriptor = getattr(self, name, None)
            if descriptor is not None:os.close(descriptor);setattr(self, name, None)


def _read_frame(stream):
    raw = stream.readline(FRAME_BYTES + 1)
    _require(raw and len(raw) <= FRAME_BYTES and raw.endswith(b"\n"), "BoundedCompleteFrameRequired")
    def pairs(rows):
        result = {}
        for key, value in rows:
            _require(key not in result, "DuplicateProtocolKey")
            result[key] = value
        return result
    value = json.loads(raw, object_pairs_hook=pairs,
                       parse_constant=lambda value: (_ for _ in ()).throw(GuestStageError("NonfiniteProtocolNumber")))
    _require(type(value) is dict, "ProtocolObjectRequired")
    return value


def _write_frame(stream, body):
    raw = _json(body)
    _require(len(raw) < FRAME_BYTES, "ResponseFrameBound")
    stream.write(raw + b"\n");stream.flush()


def _file_chunks(stream, sequence, size):
    count = 0
    while True:
        frame = _read_frame(stream)
        _require(frame.get("sequence") == sequence, "TransferSequenceMismatch")
        if frame.get("kind") == "file_end":
            _require(set(frame) == {"sequence", "kind"} and count == size, "IncompleteTransferFile")
            return
        _require(set(frame) == {"sequence", "kind", "data"} and frame["kind"] == "chunk", "ClosedChunkRequired")
        raw = base64.b64decode(frame["data"], validate=True)
        _require(0 < len(raw) <= CHUNK_BYTES and count + len(raw) <= size, "TransferChunkBound")
        count += len(raw)
        yield raw


def _publish_entries(stream, sequence, baseline):
    count = 0
    while True:
        frame = _read_frame(stream)
        _require(frame.get("sequence") == sequence, "TransferSequenceMismatch")
        if frame.get("kind") == "entries_end":
            _require(set(frame) == {"sequence", "kind"}, "ClosedTransferEndRequired")
            return
        _require(set(frame) == {"sequence", "kind", "entry"} and frame["kind"] == "entry", "ClosedTransferEntryRequired")
        row = dict(frame["entry"]);count += 1
        _require(count <= 6 * MEMBERS, "TransferMemberScope")
        if row.get("kind") == "file":
            seed = row.pop("seed_path", None)
            if seed is not None:
                _require(baseline is not None, "VerifiedSeedRequired")
                raw = baseline.read_raw(seed)
                _require(_sha(raw) == row["sha256"] and len(raw) == row["size"], "SeedMemberChanged")
                row["chunks"] = (raw[i:i + CHUNK_BYTES] for i in range(0, len(raw), CHUNK_BYTES))
            else:
                row["chunks"] = _file_chunks(stream, sequence, row["size"])
        yield row


def _native_binding(alias, physical_work, control, filesystem_uuid, expected_boot):
    _require(os.geteuid() == 0 and re.fullmatch(r"[a-fA-F0-9-]{36}", filesystem_uuid), "RootNativeHelperRequired")
    actual_boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    _require(actual_boot == expected_boot, "GuestBootIdentityChanged")
    identities, ancestries = {}, {}
    for name, path in (("alias", alias), ("physical_work", physical_work), ("control", control)):
        fd, ancestry = _open_chain(path)
        try:
            for parent, identity in ancestry:
                info = os.stat(parent, follow_symlinks=False)
                _require(info.st_uid == 0 and info.st_mode & 0o022 == 0 and _identity(info) == identity,
                         "ControllerOwnedGuestAncestryRequired")
            identities[name] = list(_identity(os.fstat(fd)));ancestries[name] = ancestry
            _require(stat.S_IMODE(os.fstat(fd).st_mode) == 0o700, "PrivateNativeRootRequired")
        finally:os.close(fd)
    _require(identities["alias"] == identities["physical_work"] and identities["alias"] != identities["control"]
             and identities["alias"][0] == identities["control"][0], "NativeBindAliasRequired")
    device = os.stat("/dev/disk/by-uuid/" + filesystem_uuid)
    _require(stat.S_ISBLK(device.st_mode) and device.st_rdev == identities["alias"][0], "NativeFilesystemUUIDMismatch")
    matching = []
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        left, right = line.split(" - ", 1);fields = left.split();filesystem = right.split()[0]
        if fields[2] == str(os.major(device.st_rdev)) + ":" + str(os.minor(device.st_rdev)):
            _require(filesystem == "ext4", "NativeExt4Required")
            matching.append({"id": fields[0], "parent": fields[1], "root": fields[3], "target": fields[4], "filesystem": filesystem})
    _require(matching and any(row["target"] == str(Path(alias)) for row in matching), "ObservedAliasMountRequired")
    return {"filesystem_uuid": filesystem_uuid, "boot_id": actual_boot, "identities": identities,
            "ancestry": ancestries, "mounts": matching, "alias": str(alias), "physical_work": str(physical_work), "control": str(control)}


def _verified_pack(manifest_path, expected_sha):
    manifest_path = Path(manifest_path)
    fd, ancestry = _open_chain(manifest_path.parent)
    try:
        for path, identity in ancestry:
            info = os.stat(path, follow_symlinks=False)
            _require(info.st_uid == 0 and info.st_mode & 0o022 == 0 and _identity(info) == identity, "ImmutableHelperPackRequired")
        descriptor = os.open(manifest_path.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
        with os.fdopen(descriptor, "rb") as stream:
            info = os.fstat(stream.fileno());raw = stream.read(FRAME_BYTES + 1)
            _require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == 0 and info.st_mode & 0o022 == 0,
                     "ImmutableHelperManifestRequired")
        _require(len(raw) <= FRAME_BYTES and _sha(raw) == expected_sha, "HelperPackManifestChanged")
        manifest = json.loads(raw)
        _require(set(manifest) == {"schema_version", "modules"} and manifest["schema_version"] == 1
                 and type(manifest["modules"]) is dict and 1 <= len(manifest["modules"]) <= 64, "ClosedHelperPackRequired")
        expected_files = set()
        for name, row in manifest["modules"].items():
            _require(type(name) is str and (name == "validation" or name.startswith("validation."))
                     and all(part.isidentifier() for part in name.split(".")), "ClosedHelperModuleNamespace")
            _require(set(row) == {"path", "sha256", "package"} and type(row["package"]) is bool
                     and re.fullmatch(r"[a-f0-9]{64}", row["sha256"]), "HelperModuleIdentityRequired")
            expected = name.replace(".", "/") + ("/__init__.py" if row["package"] else ".py")
            _require(row["path"] == expected, "HelperModulePathMismatch")
            path = manifest_path.parent / expected
            parent, _ = _open_chain(path.parent)
            try:
                file = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
                with os.fdopen(file, "rb") as stream:
                    info = os.fstat(stream.fileno());data = stream.read(8 * 1024**2 + 1)
                    _require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == 0
                             and info.st_mode & 0o022 == 0 and len(data) <= 8 * 1024**2 and _sha(data) == row["sha256"],
                             "HelperModuleChanged")
            finally:os.close(parent)
            expected_files.add(path)
        actual = {p for p in manifest_path.parent.rglob("*") if p.suffix in {".py", ".pyc", ".so"}}
        _require(actual == expected_files and Path(__file__).absolute() in expected_files, "UnexpectedHelperImportSource")
        _require(_sha(Path(__file__).read_bytes()) == manifest["modules"]["validation.knowledge.study_driver.guest_stage_helper"]["sha256"],
                 "HelperSelfIdentityMismatch")
        return manifest
    finally:os.close(fd)


def serve(filesystem, binding, source_hashes, input_stream, output_stream):
    """Single authenticated host stream; role containers never receive this FD."""
    sequence = 0;baseline = None
    try:
        while True:
            request = _read_frame(input_stream)
            _require(set(request) == {"sequence", "session", "op", "body"}
                     and request["sequence"] == sequence and request["session"] == filesystem.session,
                     "OrderedHostOperationRequired")
            op, body = request["op"], request["body"]
            _require(op in {"bind", "seed", "publish", "observe", "fixed_layout", "copy_qualified_pair", "outside_sentinel", "close", "retire", "status", "finish"}
                     and type(body) is dict, "ClosedHostOperationRequired")
            try:
                observed = _native_binding(binding["alias"], binding["physical_work"], binding["control"],
                                           binding["filesystem_uuid"], binding["boot_id"])
                _require(_json(observed) == _json(binding), "NativeBindingChanged")
                if op == "bind":
                    _require(sequence == 0 and not body, "InitialGuestBindingRequired")
                    result = {"binding": binding, "source_hashes": source_hashes}
                elif op == "seed":
                    _require(baseline is None and sequence == 1 and set(body) == {"size", "archive_sha256", "source_commit", "source_tree", "source_exceptions", "source_permits"}, "OneFrozenSeedRequired")
                    _require(type(body["size"]) is int and 0 < body["size"] <= SOURCE_BYTES, "SeedArchiveBound")
                    path = filesystem.control_path / filesystem.session / "baseline.tar.gz"
                    file = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                    digest = hashlib.sha256()
                    with os.fdopen(file, "wb") as stream:
                        for chunk in _file_chunks(input_stream, sequence, body["size"]):digest.update(chunk);stream.write(chunk)
                        stream.flush();os.fsync(stream.fileno())
                    _require(digest.hexdigest() == body["archive_sha256"], "SeedArchiveDigestMismatch")
                    from .runtime_bindings import TarBaseline
                    baseline = TarBaseline(path, archive_sha256=body["archive_sha256"], source_commit=body["source_commit"],
                        source_tree=body["source_tree"], source_exceptions=body["source_exceptions"], source_permits=body["source_permits"])
                    result = {"archive_sha256": baseline.expected, "source_commit": baseline.commit, "source_tree": baseline.tree}
                elif op == "publish":
                    _require(set(body) == {"stage", "pinned"}, "ClosedPublishRequired")
                    result = filesystem.publish(body["stage"], _publish_entries(input_stream, sequence, baseline), pinned=body["pinned"])
                elif op == "observe":result = filesystem.observe(**body)
                elif op == "fixed_layout":result = filesystem.layout(**body)
                elif op == "copy_qualified_pair":result = filesystem.copy_qualified_pair(**body)
                elif op == "outside_sentinel":
                    if "content_base64" in body:
                        body = dict(body);body["content"] = base64.b64decode(body.pop("content_base64"), validate=True)
                    result = filesystem.outside_sentinel(**body)
                elif op == "close":result = filesystem.close(**body)
                elif op == "retire":result = filesystem.retire(**body)
                elif op == "status":
                    _require(set(body) == {"operation"} and type(body["operation"]) is int and 0 <= body["operation"] < sequence,
                             "ExactPriorOperationRequired")
                    path = filesystem.control_path / filesystem.session / ("operation-" + str(body["operation"]) + ".json")
                    result = json.loads(path.read_bytes())
                else:
                    _require(not body and all(row["phase"] == "retired" for row in filesystem.stages.values()), "TerminalGuestStagesRequired")
                    if baseline is not None:baseline.close();baseline = None
                    seed = filesystem.control_path / filesystem.session / "baseline.tar.gz"
                    if seed.exists():seed.unlink()
                    result = {"finished": True}
                entries = result.pop("entries", None)
                response = {"sequence": sequence, "status": "ok", "body": result, "entries": len(entries) if entries is not None else 0}
                if op not in {"observe", "status"}:
                    _durable(filesystem.records, "operation-" + str(sequence) + ".json", response)
                _write_frame(output_stream, response)
                for entry in entries or ():_write_frame(output_stream, {"sequence": sequence, "entry": entry})
                _write_frame(output_stream, {"sequence": sequence, "end": True})
                sequence += 1
                if op == "finish":return
            except BaseException as error:
                filesystem.failed = True
                _write_frame(output_stream, {"sequence": sequence, "status": "unavailable", "error_class": type(error).__name__})
                return
    finally:
        if baseline is not None:baseline.close()
        filesystem.shutdown()


def main():
    import argparse
    parser = argparse.ArgumentParser()
    for name in ("alias", "physical-work", "control", "filesystem-uuid", "boot-id", "session", "pack-manifest", "pack-sha256"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    pack = _verified_pack(args.pack_manifest, args.pack_sha256)
    # Only this fully verified controller pack enters imports; never worker roots.
    sys.path.insert(0, str(Path(args.pack_manifest).parent))
    global __package__
    __package__ = "validation.knowledge.study_driver"
    binding = _native_binding(args.alias, args.physical_work, args.control, args.filesystem_uuid, args.boot_id)
    filesystem = GuestStageFilesystem(args.alias, args.control, session=args.session)
    serve(filesystem, binding, {name: row["sha256"] for name, row in pack["modules"].items()}, sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    main()
