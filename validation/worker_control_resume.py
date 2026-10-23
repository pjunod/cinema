"""Hydrate bounded, same-PR worker proof evidence for strict runner resumption."""

from datetime import datetime, timezone
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import urllib.error
import urllib.request
import zipfile
import io

from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts
from validation import python_unit_receipts as receipts


MAX_ARCHIVE_BYTES = 32 * 1024 * 1024
MAX_ARCHIVE_FILES = 400
MAX_MEMBER_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 100 * 1024 * 1024
MAX_HYDRATED_BYTES = 16 * 1024 * 1024
MAX_HYDRATED_FILES = 100
PROOF_FILE = re.compile(r"t[1-6]_[a-z0-9_]+\.(?:json|log)\Z")
SOURCE_FILE = re.compile(r"source/src/(?:lib|library_driver|library_lifecycle|proofs)\.rs\Z")
CONTROL_DIR = {"control-cleanup-order", "control-bind-gate", "control-wrapping-add"}
PROOF_JOB = "production worker controller proof"
WORKFLOW = "main-fast-lane.yml"

INVOCATION_PATHS = ("scripts/check-worker-control", ".github/workflows/main-fast-lane.yml",
                    "verification/worker-control/toolchain.json")
REVIEWED_POLICY = "verification/worker-control/reviewed-invocations.json"
MAX_INVOCATION_BYTES = 128 * 1024
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


@dataclass(frozen=True)
class TransportSpec:
    """Explicit authenticated transport identity; defaults retain legacy policy."""
    workflow: str = WORKFLOW
    proof_job: str = PROOF_JOB
    artifact_prefix: str = "worker-control-proof"
    trust: object = None
    max_archive_bytes: int = MAX_ARCHIVE_BYTES
    label: str = "worker proof"
    eligible_archive: object = None


@dataclass(frozen=True)
class ExtractionSpec:
    """Bounded selection only; the common ZIP integrity checks always apply."""
    select: object = None
    max_archive_bytes: int = MAX_ARCHIVE_BYTES
    max_files: int = MAX_ARCHIVE_FILES
    max_member_bytes: int = MAX_MEMBER_BYTES
    max_total_bytes: int = MAX_TOTAL_BYTES
    max_hydrated_bytes: int = MAX_HYDRATED_BYTES
    max_hydrated_files: int = MAX_HYDRATED_FILES


def legacy_transport_spec():
    # Resolve module constants at call time, retaining legacy fixture seams.
    return TransportSpec(workflow=WORKFLOW,proof_job=PROOF_JOB,
      artifact_prefix='worker-control-proof',max_archive_bytes=MAX_ARCHIVE_BYTES,
      eligible_archive=legacy_archive_has_target_pair)


def legacy_extraction_spec():
    return ExtractionSpec(max_archive_bytes=MAX_ARCHIVE_BYTES,max_files=MAX_ARCHIVE_FILES,
      max_member_bytes=MAX_MEMBER_BYTES,max_total_bytes=MAX_TOTAL_BYTES,
      max_hydrated_bytes=MAX_HYDRATED_BYTES,max_hydrated_files=MAX_HYDRATED_FILES)


def invocation_digests(commit: str, workspace: Path, *, paths=None) -> dict:
    """Hash actual bounded Git blobs, preserving every byte including newlines."""
    receipts.require(isinstance(commit, str) and COMMIT.fullmatch(commit) is not None,
                     "Invocation commit must be a full lowercase Git commit")
    def raw(*arguments):
        try:
            return subprocess.check_output(["git", *arguments], cwd=workspace,
                                           stderr=subprocess.DEVNULL, timeout=10)
        except (OSError, subprocess.SubprocessError):
            raise receipts.ReceiptError("Invocation commit/blob unavailable in local history") from None
    receipts.require(raw("cat-file", "-t", commit).strip() == b"commit",
                     "Invocation revision is not a commit")
    result = {}
    for path in (INVOCATION_PATHS if paths is None else paths):
        ref = commit + ":" + path
        size_bytes = raw("cat-file", "-s", ref).strip()
        receipts.require(re.fullmatch(rb"[0-9]{1,9}", size_bytes) is not None,
                         "Invocation blob size is invalid")
        size = int(size_bytes)
        receipts.require(0 < size <= MAX_INVOCATION_BYTES, "Invocation blob exceeds size bound")
        receipts.require(raw("cat-file", "-t", ref).strip() == b"blob", "Invocation path is not a blob")
        data = raw("show", ref)
        receipts.require(len(data) == size, "Invocation blob size changed")
        result[path] = hashlib.sha256(data).hexdigest()
    return result


def reviewed_invocations(path: Path) -> tuple[list, str]:
    def unique_pairs(pairs):
        value = {}
        for key, item in pairs:
            receipts.require(key not in value, "Duplicate invocation policy key")
            value[key] = item
        return value
    try:
        with path.open("rb") as stream:
            data = stream.read(MAX_INVOCATION_BYTES + 1)
        receipts.require(len(data) <= MAX_INVOCATION_BYTES, "Invocation policy exceeds size bound")
        policy = json.loads(data, object_pairs_hook=unique_pairs)
    except (OSError, ValueError, UnicodeError):
        raise receipts.ReceiptError("Reviewed invocation policy is unavailable or invalid") from None
    receipts.require(isinstance(policy, dict) and set(policy) == {"schema_version", "entries"}
                     and type(policy["schema_version"]) is int and policy["schema_version"] == 1
                     and isinstance(policy["entries"], list), "Unsupported invocation policy schema")
    seen_commits, seen_tuples = set(), set()
    for entry in policy["entries"]:
        receipts.require(isinstance(entry, dict) and set(entry) == {
            "source_commit", "digests", "review_basis", "coordinator_attestation"},
            "Invalid reviewed invocation entry")
        commit, digests, basis = entry["source_commit"], entry["digests"], entry["review_basis"]
        receipts.require(isinstance(commit, str) and COMMIT.fullmatch(commit) is not None,
                         "Invalid reviewed invocation commit")
        receipts.require(isinstance(digests, dict) and set(digests) == set(INVOCATION_PATHS)
                         and all(isinstance(value, str) and SHA256.fullmatch(value)
                                 for value in digests.values()), "Invalid reviewed invocation digests")
        receipts.require(isinstance(basis, dict) and set(basis) == {"original_review", "delta_review"}
                         and all(isinstance(value, str) and 0 < len(value) <= 2048 for value in basis.values())
                         and isinstance(entry["coordinator_attestation"], str)
                         and 0 < len(entry["coordinator_attestation"]) <= 4096,
                         "Missing reviewed invocation basis")
        identity = tuple(digests[path] for path in INVOCATION_PATHS)
        receipts.require(commit not in seen_commits and identity not in seen_tuples,
                         "Duplicate reviewed invocation entry")
        seen_commits.add(commit)
        seen_tuples.add(identity)
    return policy["entries"], hashlib.sha256(data).hexdigest()


def invocation_trust(prior_commit: str, current_commit: str, workspace: Path) -> dict:
    entries, policy_sha256 = reviewed_invocations(workspace / REVIEWED_POLICY)
    prior = invocation_digests(prior_commit, workspace)
    current = invocation_digests(current_commit, workspace)
    decision = "identical_invocation"
    reviewed = None
    if prior != current:
        reviewed = next((entry for entry in entries if entry["source_commit"] == prior_commit
                         and entry["digests"] == prior), None)
        receipts.require(reviewed is not None, "Prior invocation differs without exact reviewed compatibility")
        decision = "explicit_reviewed_compatibility"
    return {"prior_commit": prior_commit, "current_commit": current_commit,
            "prior_digests": prior, "current_digests": current, "decision": decision,
            "policy_path": REVIEWED_POLICY, "policy_sha256": policy_sha256,
            "reviewed_compatibility": reviewed}


def safe_member(name: str) -> tuple[str, ...]:
    if not name or "\\" in name or "\x00" in name or name.startswith("/"):
        raise receipts.ReceiptError("Unsafe proof archive member path")
    raw_parts = name[:-1].split("/") if name.endswith("/") else name.split("/")
    path = PurePosixPath(name)
    parts = path.parts
    if any(part == "" for part in raw_parts):
        raise receipts.ReceiptError("Unsafe proof archive member path")
    if not parts or any(part in ("", ".", "..") or ":" in part for part in parts):
        raise receipts.ReceiptError("Unsafe proof archive member path")
    return parts


def selected_path(parts: tuple[str, ...], is_dir: bool) -> bool:
    if is_dir:
        return False
    name = "/".join(parts)
    if len(parts) == 1:
        return PROOF_FILE.fullmatch(name) is not None
    if parts[0] == "source" and (name == "source/Cargo.toml" or SOURCE_FILE.fullmatch(name)):
        return True
    if parts[0] in CONTROL_DIR and len(parts) >= 2:
        relative = "/".join(parts[1:])
        return (PROOF_FILE.fullmatch(relative) is not None
                or relative == "mutation.json"
                or relative == "source/Cargo.toml"
                or SOURCE_FILE.fullmatch(relative) is not None)
    return False


def legacy_archive_has_target_pair(data: bytes) -> bool:
    """Select only archives with a complete case file pair for legacy resume.

    This is transport usefulness, not proof acceptance. The runner still
    reparses each exact JSON/log pair against its current source and tools.
    """
    receipts.require(len(data) <= MAX_ARCHIVE_BYTES, "Proof archive exceeds download limit")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        receipts.require(0 < len(members) <= MAX_ARCHIVE_FILES,
                         "Proof archive member count is invalid")
        total = hydrated = count = 0
        seen: set[str] = set()
        pairs: dict[tuple[tuple[str, ...], str], set[str]] = {}
        for member in members:
            parts = safe_member(member.filename.rstrip("/") if member.is_dir() else member.filename)
            canonical = "/".join(parts)
            receipts.require(canonical not in seen, "Proof archive contains duplicate paths")
            seen.add(canonical)
            mode = member.external_attr >> 16
            receipts.require(not stat.S_ISLNK(mode) and not member.flag_bits & 1,
                             "Proof archive contains a link or encrypted entry")
            receipts.require(member.file_size <= MAX_MEMBER_BYTES,
                             "Proof archive member exceeds size limit")
            total += member.file_size
            receipts.require(total <= MAX_TOTAL_BYTES,
                             "Proof archive expanded size exceeds limit")
            receipts.require(not (member.file_size and member.compress_size == 0),
                             "Proof archive has invalid compression metadata")
            receipts.require(not (member.compress_size
                                  and member.file_size / member.compress_size > 250),
                             "Proof archive compression ratio exceeds limit")
            if member.is_dir() or not selected_path(parts, False):
                continue
            receipts.require(member.file_size <= MAX_HYDRATED_BYTES - hydrated
                             and count < MAX_HYDRATED_FILES,
                             "Selected proof evidence exceeds hydration limit")
            hydrated += member.file_size
            count += 1
            match = PROOF_FILE.fullmatch(parts[-1])
            if match:
                stem, suffix = parts[-1].rsplit(".", 1)
                pairs.setdefault((parts[:-1], stem), set()).add(suffix)
    return any(suffixes == {"json", "log"} for suffixes in pairs.values())


def extract_evidence(data: bytes, destination: Path, *, spec=None) -> tuple[int, int]:
    spec = legacy_extraction_spec() if spec is None else spec
    if len(data) > spec.max_archive_bytes:
        raise receipts.ReceiptError("Proof archive exceeds download limit")
    total = hydrated = count = 0
    seen: set[str] = set()
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        if not members or len(members) > spec.max_files:
            raise receipts.ReceiptError("Proof archive member count is invalid")
        for member in members:
            parts = safe_member(member.filename.rstrip("/") if member.is_dir() else member.filename)
            canonical = "/".join(parts)
            if canonical in seen:
                raise receipts.ReceiptError("Proof archive contains duplicate paths")
            seen.add(canonical)
            mode = member.external_attr >> 16
            if stat.S_ISLNK(mode) or member.flag_bits & 1:
                raise receipts.ReceiptError("Proof archive contains a link or encrypted entry")
            if member.file_size > spec.max_member_bytes:
                raise receipts.ReceiptError("Proof archive member exceeds size limit")
            total += member.file_size
            if total > spec.max_total_bytes:
                raise receipts.ReceiptError("Proof archive expanded size exceeds limit")
            if member.file_size and member.compress_size == 0:
                raise receipts.ReceiptError("Proof archive has invalid compression metadata")
            if member.compress_size and member.file_size / member.compress_size > 250:
                raise receipts.ReceiptError("Proof archive compression ratio exceeds limit")
            if not (selected_path if spec.select is None else spec.select)(parts, member.is_dir()):
                continue
            if member.file_size > spec.max_hydrated_bytes - hydrated or count >= spec.max_hydrated_files:
                raise receipts.ReceiptError("Selected proof evidence exceeds hydration limit")
            target = destination.joinpath(*parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            if any(parent.is_symlink() for parent in (target, *target.parents) if parent != destination.parent):
                raise receipts.ReceiptError("Hydration path contains a symbolic link")
            with archive.open(member, "r") as source, target.open("xb") as output:
                copied = 0
                while True:
                    chunk = source.read(64 * 1024)
                    if not chunk:
                        break
                    copied += len(chunk)
                    if copied > member.file_size or copied > spec.max_member_bytes:
                        raise receipts.ReceiptError("Proof archive member expanded beyond its header")
                    output.write(chunk)
            if copied != member.file_size:
                raise receipts.ReceiptError("Proof archive member size mismatch")
            hydrated += copied
            count += 1
    return count, hydrated


def archive_bytes(api: receipts.API, artifact_id: int, *, spec=None) -> bytes:
    spec = legacy_transport_spec() if spec is None else spec
    api.requests += 1
    receipts.require(api.requests <= 200, "Receipt request budget exhausted")
    path = f"/actions/artifacts/{artifact_id}/zip"
    request = urllib.request.Request(api.root + path, headers={
        "Authorization": "token " + api.token,
        "Accept": "application/zip",
    })
    try:
        with api.opener.open(request, timeout=60) as response:
            data = response.read(spec.max_archive_bytes + 1)
    except urllib.error.HTTPError as error:
        raise receipts.ReceiptHTTPError(error.code, "/repos/" + api.repository + path) from None
    except (OSError, TimeoutError) as error:
        raise receipts.ReceiptError(f"{type(error).__name__} downloading prior proof; evidence unavailable") from None
    receipts.require(len(data) <= spec.max_archive_bytes, "Proof archive exceeds download limit")
    return data


def artifact_for_run(api: receipts.API, scope: dict, run: dict, current_run: int,
                     current_commit: str, workspace: Path, *, spec=None):
    spec = legacy_transport_spec() if spec is None else spec
    run_id = receipts.positive(run.get("id"))
    if run_id == current_run:
        return None
    prior = api.get(f"/actions/runs/{run_id}")
    if prior.get("id") != run_id or prior.get("workflow_id") != spec.workflow:
        return None
    if prior.get("repository", {}).get("id") != scope["repository"]:
        return None
    main_unit_receipts.authenticate_run(scope, prior)
    trust = (invocation_trust if spec.trust is None else spec.trust)(prior["commit_sha"], current_commit, workspace)
    jobs = api.pages(f"/actions/runs/{run_id}/jobs")
    valid_jobs = [job for job in jobs if job.get("name") == spec.proof_job
                  and job.get("repo_id") == scope["repository"] and job.get("run_id") == run_id
                  and type(job.get("attempt")) is int and job["attempt"] > 0
                  and type(job.get("task_id")) is int and job["task_id"] > 0]
    for attempt in sorted({job["attempt"] for job in valid_jobs}, reverse=True):
        proof_jobs = [job for job in valid_jobs if job["attempt"] == attempt]
        if len(proof_jobs) != 1:
            continue
        job = proof_jobs[0]
        # A cancelled job is eligible only as a container; strict per-target
        # parsing still rejects missing, truncated or unsuccessful evidence.
        if job.get("status") not in {"success", "failure", "cancelled"}:
            continue
        name = f"{spec.artifact_prefix}-{run_id}-{attempt}"
        artifacts = [item for item in api.pages("/actions/artifacts", {"name": name})
                     if item.get("name") == name and item.get("run_id") == run_id
                     and not item.get("expired")]
        if len(artifacts) != 1:
            continue
        artifact = artifacts[0]
        size = artifact.get("size_in_bytes")
        if type(size) is not int or size <= 0 or size > spec.max_archive_bytes:
            continue
        log_lines = main_unit_receipts.log_lines(
            api.bytes(f"/actions/jobs/{receipts.positive(job.get('id'))}/logs"))
        log = "\n".join(log_lines)
        if f"{prior['commit_sha']}:refs/remotes/pull/{scope['pr']}/head" not in log:
            continue
        artifact_id = receipts.positive(artifact.get("id"))
        upload_lines = [re.search(
            rf"^Artifact {re.escape(name)} has been successfully uploaded! Final size is ([0-9]+) bytes\. Artifact ID is ([1-9][0-9]*)$",
            line) for line in log.splitlines()]
        upload_lines = [match for match in upload_lines if match]
        zip_hashes = re.findall(r"^SHA256 hash of uploaded artifact zip is ([0-9a-f]{64})$",
                                log, flags=re.MULTILINE)
        if (len(upload_lines) != 1 or int(upload_lines[0][1]) != size
                or int(upload_lines[0][2]) != artifact_id or len(zip_hashes) != 1):
            continue
        return artifact, job, prior, attempt, zip_hashes[0], trust
    return None


def find_prior(api: receipts.API, scope: dict, current_run: int, current_commit: str, workspace: Path, *, spec=None):
    policy = legacy_transport_spec() if spec is None else spec
    runs = api.pages("/actions/runs", {
        "workflow_id": policy.workflow,
        "ref": f"refs/pull/{scope['pr']}/head",
    }, "workflow_runs")
    for run in sorted(runs, key=lambda item: item.get("id", 0), reverse=True):
        if run.get("prettyref") != f"#{scope['pr']}" or run.get("event") != "pull_request":
            continue
        try:
            candidate = (artifact_for_run(api, scope, run, current_run, current_commit, workspace) if spec is None
                         else artifact_for_run(api, scope, run, current_run, current_commit, workspace, spec=spec))
            if candidate is None:
                continue
            artifact, job, prior, attempt, uploaded_sha256, trust = candidate
            data = (archive_bytes(api, receipts.positive(artifact.get("id"))) if spec is None
                    else archive_bytes(api, receipts.positive(artifact.get("id")), spec=spec))
            archive_sha256 = hashlib.sha256(data).hexdigest()
            receipts.require(archive_sha256 == uploaded_sha256,
                            "downloaded proof archive SHA differs from its upload receipt")
            if policy.eligible_archive is not None and not policy.eligible_archive(data):
                print(f"Prior {policy.label} artifact has no complete reusable target pair; checking older evidence")
                continue
            return {
                "data": data,
                "artifact_id": receipts.positive(artifact["id"]),
                "run_id": receipts.positive(prior["id"]),
                "attempt": attempt,
                "job_id": receipts.positive(job["id"]),
                "job_result": job["status"],
                "source_commit": prior["commit_sha"],
                "archive_sha256": archive_sha256,
                "invocation_trust": trust,
            }
        except (receipts.ReceiptError, KeyError, TypeError, ValueError, zipfile.BadZipFile) as error:
            print(f"Prior {policy.label} candidate not adopted ({type(error).__name__}); checking older evidence")
    return None


def current_context():
    api, context_scope, commit, current_run = adoption.api_context()
    pr = receipts.positive(context_scope["pr"])
    scope = main_unit_receipts.identity(api, pr, commit)
    receipts.require(scope["repository"] == context_scope["repository"]
                     and scope["pr"] == pr,
                     "Current receipt context and canonical PR identity differ")
    return api, scope, commit, current_run


def main() -> int:
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
    run_id = receipts.positive(int(os.environ["GITHUB_RUN_ID"]))
    attempt = receipts.positive(int(os.environ.get("GITHUB_RUN_ATTEMPT", "1")))
    verification = workspace / "target/verification"
    if verification.exists() and verification.is_symlink():
        raise receipts.ReceiptError("Verification output directory is a symbolic link")
    if (workspace / "target").is_symlink():
        raise receipts.ReceiptError("Workspace target directory is a symbolic link")
    verification.mkdir(parents=True, exist_ok=True)
    suffix = f"{run_id}-{attempt}"
    destination = verification / f"worker-control-resume-input-{suffix}"
    origin_path = verification / f"worker-control-resume-origin-{suffix}.json"
    if destination.exists() or origin_path.exists():
        raise receipts.ReceiptError("Resume output already exists")
    destination.mkdir()
    origin = {
        "schema_version": 1,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "available": False,
        "current_run_id": run_id,
        "current_attempt": attempt,
        "hydrated_files": 0,
        "hydrated_bytes": 0,
    }
    try:
        api, scope, commit, current_run = current_context()
        candidate = find_prior(api, scope, current_run, commit, workspace)
        if candidate:
            files, byte_count = extract_evidence(candidate.pop("data"), destination)
            origin.update(candidate)
            origin.update({"available": files > 0, "hydrated_files": files,
                           "hydrated_bytes": byte_count, "pr_number": scope["pr"],
                           "candidate_commit": commit})
            if files:
                print(f"Hydrated {files} prior worker proof files ({byte_count} bytes) from same-PR run {origin['run_id']}")
            else:
                origin["reason"] = "prior artifact contained no reusable proof files"
                print("Prior proof artifact contained no reusable files; the full proof set will run")
        else:
            origin["reason"] = "no compatible same-PR proof artifact was available"
            print("No compatible prior worker proof artifact; the full proof set will run")
    except Exception as error:
        shutil.rmtree(destination)
        destination.mkdir()
        origin["reason"] = f"prior evidence unavailable ({type(error).__name__}); the full proof set will run"
        print(origin["reason"])
    origin_path.write_text(json.dumps(origin, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    output_path = os.environ.get("GITHUB_OUTPUT")
    if not output_path:
        raise receipts.ReceiptError("GitHub step output is unavailable")
    with Path(output_path).open("a", encoding="utf-8") as stream:
        stream.write(f"resume_dir={destination}\n")
        stream.write(f"origin_path={origin_path}\n")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"Worker proof resume preparation failed ({type(error).__name__})", file=sys.stderr)
        raise SystemExit(2)
