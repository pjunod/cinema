"""Exact run3972 positive events after its deadline prevented final upload.

This witness reconstructs an incomplete historical success map, never a final
artifact or a completed attempt. It enrolls no other interrupted run.
"""
import hashlib
import io
from pathlib import Path
import re
import zipfile

from validation import python_unit_receipts as receipts


SCOPE = {"repository": 1, "pr": 767,
         "branch": "codex/ci-windows-baremetal-routing",
         "base": "effort/architecture-review-2026-09-20"}
COMMIT = "2d08b4bc71315c14e48755229f84a6d91c23dc22"
DESCRIPTOR_SHA256 = "3e7134e0b02bf8082c93c1f6990f64e27099a5fba024d272bf25f0fb4c21da10"
PREPARE_SHA256 = "f17328b9800a00a750ece1e6dbb2c8ce7a52c3db01509ae7af30a2d12324c909"


def prepare_case():
    path = Path(__file__).with_name("python-unit-preunit-failure3985.json")
    receipts.require(path.is_file() and not path.is_symlink(), "Prepare3985 witness unavailable")
    with path.open("rb") as stream:
        raw = stream.read(receipts.MAX_BYTES + 1)
    receipts.require(len(raw) <= receipts.MAX_BYTES
                     and hashlib.sha256(raw).hexdigest() == PREPARE_SHA256,
                     "Unknown or corrupt prepare3985 witness")
    return receipts.bounded_json(raw), PREPARE_SHA256


def recover_prepare_pr767(api, scope, prior, jobs):
    """One authenticated zero-unit CLI refusal; never import a success map."""
    proof, digest = prepare_case()
    receipts.require(proof["version"] == 1 and proof["scope"] == SCOPE
                     and proof["run"] == 3985 and proof["job"] == 41443
                     and proof["task"] == 15438
                     and proof["commit"] == "0782c1ac4d04e7ee3592f2a97e4b40cbfed31f09",
                     "Prepare3985 exact-case identity mismatch")
    receipts.authenticate_lost_journal(api, scope, digest)
    receipts.require(prior["commit_sha"] == proof["commit"], "Prepare3985 prior source mismatch")
    actual = api.get("/actions/runs/3985")
    receipts.require(actual["id"] == 3985 and actual["repository"]["id"] == 1
                     and actual["commit_sha"] == proof["commit"]
                     and actual["prettyref"] == scope["branch"]
                     and actual["workflow_id"] == "effort-ci.yml" and actual["status"] == "failure",
                     "Prepare3985 terminal run metadata mismatch")
    receipts.require(len(jobs) == len(proof["jobs"])
                     and sorted(({field: job[field] for field in ("id", "task_id", "name", "status")}
                                 for job in jobs), key=lambda row: row["id"]) == proof["jobs"]
                     and all(job["run_id"] == 3985 and job["repo_id"] == 1
                             and job["attempt"] == 1 for job in jobs),
                     "Prepare3985 complete terminal job inventory mismatch")
    receipts.require(set(proof["source_hashes"]) == {
        ".github/workflows/effort-ci.yml", "validation/python_unit_receipts.py",
        "validation/python_unit_interrupted_recovery.py"}, "Prepare3985 source inventory mismatch")
    for path, expected in proof["source_hashes"].items():
        source = api.bytes("/raw/" + path, {"ref": proof["commit"]})
        receipts.require(len(source) <= receipts.MAX_BYTES
                         and hashlib.sha256(source).hexdigest() == expected,
                         "Prepare3985 original parser/source mismatch")
    raw = api.bytes("/actions/jobs/41443/logs")
    receipts.require(len(raw) <= receipts.MAX_BYTES
                     and hashlib.sha256(raw).hexdigest() == proof["log_sha256"],
                     "Prepare3985 original terminal log mismatch")
    lines = [re.sub(r"^\d{4}-\d\d-\d\dT[0-9:.]+Z ", "", line)
             for line in raw.decode("utf-8").splitlines()]
    refusal = ("Python receipt refusal: ReceiptHTTPError: HTTP 403 at "
               "/repos/noirr/plurx/collaborators/pjunod/permission; evidence unavailable")
    start = "skipping post step for 'Publish Python attempt-start marker'; main step was skipped"
    final = "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped"
    log = "\n".join(lines)
    receipts.require(raw.endswith(b"\n") and proof["commit"] in lines
                     and lines.count(refusal) == lines.count(start) == lines.count(final) == 1
                     and lines.index(refusal) < min(lines.index(start), lines.index(final))
                     and lines[-1] == "Job 'Python unit receipts' failed"
                     and not any(marker in log for marker in (
                         "discovered=", "historical-passes=", "pending=", "Ran ",
                         "Unit discovery failed", "fixture_errors", "... ok", "... FAIL", "... ERROR",
                         "has been successfully uploaded!")),
                     "Prepare3985 contradicts zero-unit terminal phase evidence")
    receipts.recovery_absence(api, scope, 3985)
    print("Recovered failed prepare run3985/job41443: zero units, no successes imported")
    return True, None


def interrupted_case():
    path = Path(__file__).with_name("python-unit-interrupted3972.json")
    receipts.require(path.is_file() and not path.is_symlink(),
                     "Interrupted journal witness unavailable")
    with path.open("rb") as stream:
        raw = stream.read(receipts.MAX_BYTES + 1)
    receipts.require(len(raw) <= receipts.MAX_BYTES
                     and hashlib.sha256(raw).hexdigest() == DESCRIPTOR_SHA256,
                     "Unknown or corrupt interrupted journal witness")
    return receipts.bounded_json(raw), DESCRIPTOR_SHA256


def positive_events(proof, raw):
    """Reject every outcome except the fixed positives and one interrupted ID."""
    receipts.require(len(raw) == proof["log"]["bytes"] <= receipts.MAX_BYTES
                     and hashlib.sha256(raw).hexdigest() == proof["log"]["sha256"],
                     "Interrupted journal terminal log mismatch")
    lines = [re.sub(r"^\d{4}-\d\d-\d\dT[0-9:.]+Z ", "", line)
             for line in raw.decode("utf-8").splitlines()]
    receipts.require(raw.endswith(b"\n") and COMMIT in raw.decode("utf-8")
                     and lines.count(proof["deadline"]) == 1
                     and lines[-1].startswith("Error occurred running finally:")
                     and "context deadline exceeded" in lines[-1],
                     "Interrupted journal terminal deadline evidence missing")
    deadline = lines.index(proof["deadline"])
    upload = (f"Artifact {receipts.key(SCOPE)}-start-3972 has been successfully uploaded! "
              f"Final size is {proof['start']['bytes']} bytes. Artifact ID is {proof['start']['id']}")
    receipts.require(lines.count(upload) == 1 and lines.index(upload) < deadline,
                     "Interrupted journal start publication missing")
    headers = []
    for i, line in enumerate(lines):
        match = re.match(r"^(test\w+) \((test_[A-Za-z0-9_.]+)\)", line)
        if match:
            receipts.require(match.group(1) == match.group(2).rsplit(".", 1)[1],
                             "Interrupted journal method header mismatch")
            headers.append((i, match.group(2)))
    expected = proof["event_ids"]
    receipts.require(len(expected) == len(set(expected)) == 383
                     and len(headers) == 384
                     and len({identity for _, identity in headers}) == 384
                     and headers[0][0] > lines.index(upload)
                     and headers[-1][0] < deadline,
                     "Interrupted journal individual events ambiguous")
    by_identity = {key.split(":", 1)[1]: key for key in [*expected, proof["interrupted_id"]]}
    receipts.require(len(by_identity) == 384
                     and set(by_identity) == {identity for _, identity in headers}
                     and [by_identity[identity] for _, identity in headers[:-1]] == expected
                     and by_identity[headers[-1][1]] == proof["interrupted_id"],
                     "Interrupted journal individual inventory mismatch")
    seen = set()
    for position, (i, identity) in enumerate(headers):
        end = headers[position + 1][0] if position + 1 < len(headers) else deadline
        block = lines[i:end]
        outcomes = [line for line in block if line == "ok" or line.endswith(" ... ok")]
        receipts.require(not any(re.search(
            r"(?:^|\.\.\. )(?:FAIL|ERROR|skipped|expected failure|unexpected success)(?:\b|:)", line)
            for line in block), "Interrupted journal non-success outcome")
        key = by_identity[identity]
        if key == proof["interrupted_id"]:
            receipts.require(not outcomes and position == len(headers) - 1,
                             "Interrupted journal interrupted ID has an outcome")
        else:
            receipts.require(len(outcomes) == 1,
                             "Interrupted journal lacks one positive event per ID")
            seen.add(key)
    receipts.require(seen == set(expected)
                     and sum(key.startswith("validation:") for key in seen) == 285
                     and sum(key.startswith("operations:") for key in seen) == 98,
                     "Interrupted journal positive inventory mismatch")
    summary = proof["validation_summary"]
    receipts.require(lines.count(summary) == lines.count("OK") == 1,
                     "Interrupted journal completed validation summary mismatch")
    summary_at, okay_at = lines.index(summary), lines.index("OK")
    validation_headers = [i for i, identity in headers if by_identity[identity].startswith("validation:")]
    operations_headers = [i for i, identity in headers if by_identity[identity].startswith("operations:")]
    receipts.require(max(validation_headers) < summary_at < okay_at < min(operations_headers)
                     and not any(re.match(r"^(?:ERROR|FAIL):|^(?:setUp|tearDown)(?:Class|Module) ", line)
                                 for line in lines),
                     "Interrupted journal fixture or phase evidence ambiguous")
    return seen


def recover_interrupted_pr767(api, scope, prior, jobs, legacy, current_run):
    """Recover only the bound missing-final case, with original attributions."""
    if scope == SCOPE and prior["id"] == 3985:
        return recover_prepare_pr767(api, scope, prior, jobs)
    if scope != SCOPE or prior["id"] != 3972:
        return False, None
    proof, digest = interrupted_case()
    receipts.require(proof["version"] == 1 and proof["scope"] == SCOPE
                     and proof["run"] == 3972 and proof["job"] == 41333
                     and proof["task"] == 15382 and proof["commit"] == COMMIT
                     and proof["start"]["id"] == 1650 and current_run != 3972,
                     "Interrupted journal exact-case identity mismatch")
    receipts.authenticate_lost_journal(api, scope, digest)
    receipts.require(prior["commit_sha"] == COMMIT, "Interrupted journal prior source mismatch")
    actual = api.get("/actions/runs/3972")
    receipts.require(actual["id"] == 3972 and actual["repository"]["id"] == 1
                     and actual["commit_sha"] == COMMIT and actual["prettyref"] == scope["branch"]
                     and actual["workflow_id"] == "effort-ci.yml" and actual["status"] == "failure",
                     "Interrupted journal original run metadata mismatch")
    matching = [job for job in jobs if job["name"] == receipts.job_name(scope)]
    receipts.require(len(matching) == 1 and matching[0]["id"] == 41333
                     and matching[0]["run_id"] == 3972 and matching[0]["repo_id"] == 1
                     and matching[0]["attempt"] == 1 and matching[0]["task_id"] == 15382
                     and matching[0]["status"] == "failure",
                     "Interrupted journal original job metadata mismatch")
    pull, candidate = api.get("/pulls/767"), api.get(f"/actions/runs/{current_run}")
    receipts.require(pull["state"] == "open" and pull["head"]["ref"] == scope["branch"]
                     and pull["base"]["ref"] == scope["base"]
                     and pull["head"]["repo"]["id"] == pull["base"]["repo"]["id"] == 1
                     and candidate["id"] == current_run and candidate["repository"]["id"] == 1
                     and candidate["commit_sha"] == pull["head"]["sha"]
                     and candidate["prettyref"] == scope["branch"]
                     and candidate["workflow_id"] == "effort-ci.yml",
                     "Interrupted journal current PR/run identity mismatch")
    required = {".github/workflows/effort-ci.yml", "validation/python_unit_receipts.py"}
    required.update(receipts.test_source_path(key)[0]
                    for key in [*proof["event_ids"], proof["interrupted_id"]])
    receipts.require(set(proof["source_hashes"]) == required and len(required) <= 64,
                     "Interrupted journal authenticated source inventory mismatch")
    total = 0
    for path, expected in proof["source_hashes"].items():
        source = api.bytes("/raw/" + path, {"ref": COMMIT})
        total += len(source)
        receipts.require(len(source) <= receipts.MAX_BYTES and total <= receipts.MAX_BYTES
                         and hashlib.sha256(source).hexdigest() == expected,
                         "Interrupted journal original parser/test source mismatch")
    artifacts = api.get("/actions/runs/3972/artifacts")
    artifacts = artifacts.get("artifacts", []) if isinstance(artifacts, dict) else artifacts
    start_name = receipts.key(scope) + "-start-3972"
    markers = api.pages("/actions/artifacts", {"name": start_name})
    finals = api.pages("/actions/artifacts", {"name": receipts.key(scope)})
    receipts.require(len(artifacts) == len(markers) == 1 and artifacts[0] == markers[0]
                     and not any(artifact["run_id"] == 3972 for artifact in finals),
                     "Interrupted journal live artifact inventory changed")
    marker = markers[0]
    receipts.require(marker["id"] == 1650 and marker["run_id"] == 3972
                     and marker["name"] == start_name and not marker["expired"]
                     and marker["size_in_bytes"] == proof["start"]["bytes"],
                     "Interrupted journal start artifact identity mismatch")
    marker_raw = api.bytes("/actions/artifacts/1650/zip")
    receipts.require(len(marker_raw) == proof["start"]["bytes"]
                     and hashlib.sha256(marker_raw).hexdigest() == proof["start"]["zip_sha256"],
                     "Interrupted journal start archive changed")
    start = receipts.artifact_json(marker_raw)
    with zipfile.ZipFile(io.BytesIO(marker_raw)) as archive:
        start_json = archive.read("receipt.json")
    receipts.require(hashlib.sha256(start_json).hexdigest() == proof["start"]["json_sha256"],
                     "Interrupted journal start JSON changed")
    receipts.validate_journal(start, scope, 3972, COMMIT, completed=False)
    receipts.require(start["complete"] is False and start["applicability_commit"] == COMMIT
                     and start["passes"] == proof["baseline"] and len(start["passes"]) == 1
                     and all(legacy.get(key) == attribution for key, attribution in start["passes"].items()),
                     "Interrupted journal inherited authenticated map changed")
    events = positive_events(proof, api.bytes("/actions/jobs/41333/logs"))
    receipts.require(not events.intersection(start["passes"]),
                     "Interrupted journal retained method was re-executed")
    journal = {"version": receipts.VERSION, "scope": dict(scope), "run": 3972,
               "commit": COMMIT, "complete": False, "fixture_errors": [],
               "passes": {**start["passes"], **{key: {"commit": COMMIT, "run": 3972}
                                               for key in sorted(events)}}}
    receipts.validate_journal(journal, scope, 3972, COMMIT, completed=False)
    receipts.require(len(journal["passes"]) == 384, "Interrupted journal recovered inventory mismatch")
    print("Recovered 383 original positive events and one authenticated local pass from run3972; "
          "attempt remains incomplete; interrupted method is pending")
    return True, journal
