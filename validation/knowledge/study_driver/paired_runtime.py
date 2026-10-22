"""Gold-free paired-role staging; inert recipes enter only from the custodian.

Candidate source and private assertion artifacts never share a role. Source
preparers are reviewed, pinned engineering callbacks, not model-provided code.
No executor, registry or provider is started by importing this module.
"""
from dataclasses import dataclass, replace
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import stat
import uuid

from .runtime_bindings import (PinnedCallback, canonical, file_sha,
                               no_links, relative, require, sha, write_grader)


@dataclass(frozen=True)
class PinnedRoleSource:
    path: Path
    sha256: str
    filename: str

    def read(self):
        path = no_links(self.path)
        require(str(relative(self.filename)) == self.filename, "RoleSourceFilename")
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_size <= 8 * 1024**2, "BoundedRoleSource")
            raw = stream.read(8 * 1024**2 + 1)
            after = os.fstat(stream.fileno())
        require(_identity(before) == _identity(after) == _identity(path.lstat()), "RoleSourceChanged")
        require(sha(raw) == self.sha256, "RoleSourceDigestMismatch")
        return raw


@dataclass(frozen=True)
class PairedRoleBinding:
    worker_driver: PinnedRoleSource
    schema: PinnedRoleSource
    protected_driver: PinnedRoleSource
    source_preparer: PinnedCallback | None = None
    worker_uid: int = 1000
    worker_gid: int | None = None
    operations: tuple[str, ...] = ("health", "call")
    exception_types: tuple[str, ...] = ()
    worker_argv: tuple[str, ...] = ()
    require_rust: bool = False
    max_requests: int = 4096
    max_frame_bytes: int = 65536
    max_total_bytes: int = 16 * 1024**2

    def __post_init__(self):
        for name in ("operations", "exception_types", "worker_argv"):
            object.__setattr__(self, name, tuple(getattr(self, name)))
        require(all(type(source) is PinnedRoleSource for source in
                    (self.worker_driver, self.schema, self.protected_driver)), "PinnedRoleSourcesRequired")
        require(self.source_preparer is None or type(self.source_preparer) is PinnedCallback,
                "PinnedEngineeringSourcePreparerRequired")
        require(type(self.worker_uid) is int and self.worker_uid > 0 and (self.worker_gid is None or type(self.worker_gid) is int
                and self.worker_gid > 0) and type(self.require_rust) is bool, "UnprivilegedWorkerIdentity")
        require(self.schema.filename == "rpc-contract.json", "ExactWorkerSchemaFilename")
        # Reading sources later verifies the actual bytes, not just these claims.


def _identity(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def readable_tree(root):
    """Exact files retain modes; all directory ancestors are role-traversable."""
    root = no_links(root)
    for directory, folders, files in os.walk(root, followlinks=False):
        directory = no_links(directory)
        directory.chmod(0o755)
        for name in (*folders, *files):
            path = directory / name
            info = path.lstat()
            require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode), "RoleLinksOrSpecialFiles")
            require(not stat.S_ISREG(info.st_mode) or info.st_mode & 0o444 == 0o444,
                    "RoleSourceMustBeReadable")


def runtime_layout(root):
    root = no_links(root)
    root.chmod(0o777)
    for name in ("tmp", "home", "cargo", "rustup", "target", "cache"):
        path = root / name
        path.mkdir(exist_ok=True, mode=0o777)
        no_links(path).chmod(0o777)


def _manifest(rows):
    raw = json.dumps(sorted(rows), sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    return hashlib.sha256(raw).hexdigest()


def measure_tree(root, *, candidate=False, max_bytes=512 * 1024**2):
    root = no_links(root)
    rows, identities, total = [], {}, 0
    for directory, folders, files in os.walk(root, followlinks=False):
        directory = no_links(directory)
        if candidate and directory == root and ".runtime" in folders:
            require(not (root / ".runtime").is_symlink() and not any((root / ".runtime").iterdir()), "EmptyRuntimeMountpointRequired")
            folders.remove(".runtime")
        for name in folders:
            require(stat.S_ISDIR((directory / name).lstat().st_mode), "ManifestDirectoryLink")
        for name in files:
            path = directory / name
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, "rb") as stream:
                before = os.fstat(stream.fileno())
                require(stat.S_ISREG(before.st_mode), "ManifestSpecialFile")
                total += before.st_size
                require(total <= max_bytes and len(rows) < 100000, "ManifestBound")
                digest, size = hashlib.sha256(), 0
                while chunk := stream.read(1024**2):
                    size += len(chunk)
                    require(size <= before.st_size, "ManifestFileChanged")
                    digest.update(chunk)
                after = os.fstat(stream.fileno())
            require(size == before.st_size and _identity(before) == _identity(after) == _identity(path.lstat()), "ManifestFileChanged")
            key = path.relative_to(root).as_posix()
            identities[key] = _identity(before)
            rows.append((key, stat.S_IMODE(before.st_mode), digest.hexdigest()))
    current = {}
    for directory, folders, files in os.walk(root, followlinks=False):
        no_links(directory)
        if candidate and Path(directory) == root and ".runtime" in folders:
            folders.remove(".runtime")
        for name in folders:
            require(stat.S_ISDIR((Path(directory) / name).lstat().st_mode), "ManifestDirectoryChanged")
        for name in files:
            path = Path(directory) / name
            current[path.relative_to(root).as_posix()] = _identity(path.lstat())
    require(current == identities, "ManifestMembershipChanged")
    return _manifest(rows)


def copy_qualified_runtime(source, target, expected_sha256, *, max_bytes=32 * 1024**3):
    """Copy a stopped, issued qualified runtime with content/mode/race checks."""
    source, target = no_links(source), no_links(target)
    require(source != target and source.is_dir() and target.is_dir()
            and not source.is_relative_to(target) and not target.is_relative_to(source),
            "DistinctQualifiedRuntimes")
    staging = target.parent / ("runtime-copy-" + uuid.uuid4().hex)
    staging.mkdir(mode=0o700)
    rows, identities, count = [], {}, 0
    try:
        for directory, folders, files in os.walk(source, followlinks=False):
            directory = no_links(directory)
            rel = directory.relative_to(source)
            (staging / rel).mkdir(parents=True, exist_ok=True, mode=0o755)
            for name in folders:
                require(stat.S_ISDIR((directory / name).lstat().st_mode), "RuntimeDirectoryLink")
            for name in files:
                origin = directory / name
                fd = os.open(origin, os.O_RDONLY | os.O_NOFOLLOW)
                with os.fdopen(fd, "rb") as incoming:
                    before = os.fstat(incoming.fileno())
                    require(stat.S_ISREG(before.st_mode), "RuntimeSpecialFile")
                    count += before.st_size
                    require(count <= max_bytes and len(rows) < 100000, "RuntimeCopyBound")
                    destination = staging / rel / name
                    output = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                    digest, size = hashlib.sha256(), 0
                    with os.fdopen(output, "wb") as outgoing:
                        while chunk := incoming.read(1024**2):
                            size += len(chunk)
                            require(size <= before.st_size, "RuntimeGrewDuringCopy")
                            digest.update(chunk)
                            outgoing.write(chunk)
                        os.fchmod(outgoing.fileno(), stat.S_IMODE(before.st_mode))
                        outgoing.flush()
                        os.fsync(outgoing.fileno())
                    after = os.fstat(incoming.fileno())
                require(size == before.st_size and _identity(before) == _identity(after) == _identity(origin.lstat()),
                        "RuntimeChangedDuringCopy")
                key = (rel / name).as_posix()
                identities[key] = _identity(before)
                rows.append((key, stat.S_IMODE(before.st_mode), digest.hexdigest()))
        current = {}
        for directory, folders, files in os.walk(source, followlinks=False):
            no_links(directory)
            for name in folders:
                require(stat.S_ISDIR((Path(directory) / name).lstat().st_mode), "RuntimeDirectoryChanged")
            for name in files:
                path = Path(directory) / name
                current[path.relative_to(source).as_posix()] = _identity(path.lstat())
        require(current == identities and _manifest(rows) == expected_sha256, "QualifiedRuntimeIdentityMismatch")
        require(measure_tree(staging,max_bytes=max_bytes)==expected_sha256,"CopiedRuntimeIdentityMismatch")
        # The target is fresh stage-owned state, never another writer's cache.
        no_links(target)
        shutil.rmtree(target)
        os.replace(staging, target)
        runtime_layout(target)
    finally:
        if staging.exists():
            shutil.rmtree(staging)


def family_identity(plan):
    """Portable sealed command/role identity; runtime bytes are measured separately."""
    def step(value):
        return {"id": value.id, "argv": value.argv, "cwd": value.cwd,
                "environment": value.environment, "writes": sorted(value.writes), "kind": value.kind,
                "protected_readonly": value.protected_readonly}
    if getattr(plan, "schema_version", 1) != 2:
        return sha(canonical({"family": plan.family, "prerequisite_argv": plan.prerequisite_argv,
            "acceptance_argv": plan.acceptance_argv, "driver": vars(plan.driver)}))
    result = {"family": plan.family, "schema_version": 2, "driver": vars(plan.driver),
              "prerequisite_steps": [step(value) for value in plan.prerequisite_steps],
              "acceptance_steps": [step(value) for value in plan.acceptance_steps], "worker": None}
    worker = plan.worker
    if worker is not None:
        result["worker"] = {"driver": vars(worker.driver), "argv": worker.argv, "operations": worker.operations,
            "exception_types": worker.exception_types, "schema_sha256": worker.schema_sha256,
            "proxy_sha256": worker.proxy_sha256, "max_requests": worker.max_requests,
            "max_frame_bytes": worker.max_frame_bytes, "max_total_bytes": worker.max_total_bytes,
            "policy": {name: getattr(worker.config, name) for name in
                ("image", "uid", "gid", "candidate_readonly", "require_rust", "python_executable",
                 "rustc_executable", "cargo_executable", "timeout_seconds", "max_output_bytes",
                 "memory_bytes", "cpus", "pids_limit", "runtime_max_bytes", "cache_max_bytes")}}
    return sha(canonical(result))


class PairedRoleStager:
    def __init__(self, source_stager, roles):
        require(isinstance(roles, dict) and roles and all(type(role) is PairedRoleBinding for role in roles.values()),
                "ConcretePairedRoleCatalogRequired")
        self.owner, self.roles = source_stager, dict(roles)

    def prepare(self, stage):
        owner, plan = self.owner, stage.family_v2
        role = self.roles.get(plan.family)
        require(role is not None, "MissingPairedFamilyBinding")
        require(role.protected_driver.filename == plan.driver.path
                and role.protected_driver.sha256 == plan.driver.sha256, "ProtectedRecipeDriverMismatch")
        protected_bytes = role.protected_driver.read()
        require(sha((stage.grader / plan.driver.path).read_bytes()) == sha(protected_bytes), "ProtectedDriverStageMismatch")
        stage.worker_candidate = stage.root / "worker-source"
        os.replace(stage.candidate, stage.worker_candidate)
        stage.candidate.mkdir(mode=0o700)
        owner.baseline.materialize(stage.candidate)
        stage.worker_grader, stage.worker_runtime = stage.root / "worker-export", stage.root / "worker-runtime"
        stage.worker_grader.mkdir(mode=0o755)
        stage.worker_runtime.mkdir(mode=0o777)
        if role.source_preparer is not None:
            role.source_preparer(stage.candidate, stage.worker_candidate, owner.baseline)
        for root in (stage.candidate, stage.worker_candidate, stage.grader):
            readable_tree(root)
            (root / ".runtime").mkdir(exist_ok=True, mode=0o755) if root != stage.grader else None
        write_grader(stage.worker_grader, role.worker_driver.filename, role.worker_driver.read())
        write_grader(stage.worker_grader, "rpc-contract.json", role.schema.read())
        readable_tree(stage.worker_grader)
        runtime_layout(stage.runtime)
        runtime_layout(stage.worker_runtime)
        if owner.seeder is not None:
            owner.seeder(stage.worker_runtime)
        stage.paired_role = role
        # Configuration is bound after all source preparation and registry seeding.
        stage.family_v2 = self.bind_plan(stage, plan)

    def bind_plan(self, stage, plan):
        owner, role = self.owner, stage.paired_role
        require(role.worker_uid != os.getuid(), "WorkerUidMustDifferFromProtected")
        first = plan.acceptance_steps[0]
        worker_argv = role.worker_argv or (owner.python, "/grader/" + role.worker_driver.filename)
        config = owner.configuration(stage, owner.step_command(first), _worker=True)
        config = replace(config, candidate_root=stage.worker_candidate, grader_root=stage.worker_grader,
                         runtime_root=stage.worker_runtime, acceptance_argv=(worker_argv,),
                         uid=role.worker_uid, gid=os.getgid() if role.worker_gid is None else role.worker_gid, candidate_readonly=True,
                         trusted_wrappers=(), require_rust=role.require_rust)
        worker = owner.api.ProtectedWorkerPlan(config, owner.api.TrustedSourceFile(role.worker_driver.filename,
                    role.worker_driver.sha256), worker_argv, role.operations, role.schema.sha256,
                    role.protected_driver.sha256, role.exception_types, role.max_frame_bytes,
                    role.max_requests, role.max_total_bytes)
        return replace(plan, worker=worker)

    def adopt(self, stage, baseline, body):
        require(stage.paired_role == baseline.paired_role and body["initial_protected_source"] ==
                self.owner.source_manifest(stage.candidate), "PristineProtectedViewRequired")
        copy_qualified_runtime(baseline.worker_runtime, stage.worker_runtime, body["after"]["worker_runtime"])
        # Rebind to this stage, never the baseline candidate/worker configuration.
        stage.family_v2 = self.bind_plan(stage, stage.family_v2)


class DependencyImageSeed:
    """Pin dependency acquisition and write deterministic offline Cargo config.

    A build receipt is not execution qualification. Probes still have to observe
    the exact image and both role runtimes before any acceptance can execute.
    """
    def __init__(self, receipt_path, receipt_sha256, *, image, baseline, admission, review_gate):
        self.path, self.receipt_sha256 = no_links(receipt_path), receipt_sha256
        self.image, self.baseline = image, baseline
        self.admission, self.review_gate = admission, review_gate

    BASE_IMAGE = "sha256:769ee1d1e2205ca810a1f281106a7d30f7aec9c4e375231027c62542ecb896fa"
    FETCH_RECEIPT_SHA256 = "f31ab7c5ab3a4427ded73c499f2588f62253e5b7e4674889f744a3997ca3c034"
    RECEIPT_FIELDS = frozenset(("schema_version", "kind", "image_id", "base_image_id",
        "base_rootfs_diff_ids", "image_rootfs_diff_ids", "source_commit", "source_archive_sha256",
        "cargo_lock_sha256", "dependency_fetch_receipt_sha256", "vendor_manifest_sha256",
        "dockerfile_sha256", "context_manifest_sha256", "vendor_directory"))

    def _verified(self):
        require(isinstance(self.receipt_sha256, str) and
                re.fullmatch(r"[a-f0-9]{64}", self.receipt_sha256), "DependencyReceiptDigestRequired")
        descriptor = os.open(no_links(self.path), os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_size <= 1024**2,
                    "BoundedDependencyImageReceipt")
            raw = stream.read(1024**2 + 1)
            after = os.fstat(stream.fileno())
        require(_identity(before) == _identity(after) == _identity(self.path.lstat()),
                "DependencyImageReceiptChanged")
        require(sha(raw) == self.receipt_sha256, "DependencyImageReceiptChanged")
        def unique(pairs):
            result = {}
            for key, value in pairs:
                require(key not in result, "DuplicateDependencyReceiptField")
                result[key] = value
            return result
        body = json.loads(raw, object_pairs_hook=unique)
        require(isinstance(body, dict) and set(body) == self.RECEIPT_FIELDS and
                type(body["schema_version"]) is int and body["schema_version"] == 1 and
                body["kind"] == "locked-dependency-image", "DependencyImageReceiptSchema")
        require(body["image_id"] == self.image and body["source_commit"] == self.baseline.commit
                and body["source_archive_sha256"] == self.baseline.expected
                and body["cargo_lock_sha256"] == sha(self.baseline.read_raw("Cargo.lock"))
                and body["vendor_directory"] == "/opt/locked-crates", "DependencyImageBaselineMismatch")
        require(body["base_image_id"] == self.BASE_IMAGE and body["image_id"] != self.BASE_IMAGE
                and body["dependency_fetch_receipt_sha256"] == self.FETCH_RECEIPT_SHA256,
                "DependencyImageInputMismatch")
        for key in ("image_id", "base_image_id"):
            require(isinstance(body[key], str) and re.fullmatch(r"sha256:[a-f0-9]{64}", body[key]),
                    "ImmutableDependencyImage")
        for key in ("source_archive_sha256", "cargo_lock_sha256", "dependency_fetch_receipt_sha256",
                    "vendor_manifest_sha256", "dockerfile_sha256", "context_manifest_sha256"):
            require(isinstance(body[key], str) and re.fullmatch(r"[a-f0-9]{64}", body[key]),
                    "DependencyImageInputIdentity")
        prefix, layers = body["base_rootfs_diff_ids"], body["image_rootfs_diff_ids"]
        require(isinstance(prefix, list) and prefix and isinstance(layers, list) and len(layers) > len(prefix)
                and len(layers) <= 256 and layers[:len(prefix)] == prefix and all(isinstance(value, str)
                and re.fullmatch(r"sha256:[a-f0-9]{64}", value) for value in layers),
                "DependencyImageBaseLayerMismatch")
        return body

    def __call__(self, runtime):
        self.admission.assert_current()
        require(self.review_gate() is True, "DependencyImageSetupNotReviewed")
        self._verified()
        cargo = no_links(runtime) / "cargo"
        require(no_links(cargo).is_dir(), "StagedCargoDirectoryRequired")
        text = ('# dependency image acquisition sha256:' + self.receipt_sha256 + '\n'
                '[source.crates-io]\nreplace-with = "locked-pilot"\n'
                '[source.locked-pilot]\ndirectory = "/opt/locked-crates"\n'
                '[net]\noffline = true\n')
        path = cargo / "config.toml"
        if path.exists():
            require(not path.is_symlink() and stat.S_ISREG(path.lstat().st_mode)
                    and stat.S_IMODE(path.lstat().st_mode) == 0o644
                    and path.read_bytes() == text.encode(), "DependencyConfigConflict")
        else:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(text.encode());os.fchmod(stream.fileno(), 0o644);stream.flush();os.fsync(stream.fileno())
        require(file_sha(self.path) == self.receipt_sha256, "DependencyImageReceiptChangedDuringSetup")
        return sha(text.encode())
