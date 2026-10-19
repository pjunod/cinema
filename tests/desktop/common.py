"""Native qualification support. Receipts describe observations, never simulated passes."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time


def git_environment(env=None):
    # Git hooks export repository/index routing variables. Fixture repositories
    # must never inherit those bindings from their caller's checkout.
    return {key: value for key, value in (os.environ if env is None else env).items()
            if not key.startswith("GIT_")}


def run(args, *, timeout=45, check=True, env=None):
    if Path(str(args[0])).name == "git":
        env = git_environment(env)
    result = subprocess.run([str(a) for a in args], text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=timeout, env=env)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} exited {result.returncode}: {result.stderr[-3000:]}")
    return result


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".partial")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def check_identity(run_id, candidate, dispatched, run_record, repository):
    if not re.fullmatch(r"[1-9][0-9]*", str(run_id)):
        raise ValueError("candidate run ID must be a positive decimal integer")
    if not re.fullmatch(r"[0-9a-f]{40}", candidate) or candidate != dispatched:
        raise ValueError("candidate SHA must exactly equal the dispatched workflow SHA")
    expected = {"id": int(run_id), "head_sha": candidate, "status": "completed", "conclusion": "success",
                "event": "workflow_dispatch", "path": ".github/workflows/github-release.yml"}
    for key, value in expected.items():
        if run_record.get(key) != value:
            raise ValueError(f"candidate run {key} differs from required {value!r}")
    if run_record.get("repository", {}).get("full_name") != repository:
        raise ValueError("candidate artifacts must come from this repository")
    return expected


def hosted_guard(candidate, *, mutate=False):
    """Deliberately reject owner machines and self-hosted runners before mutations."""
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError("qualification requires a disposable GitHub-hosted VM")
    if os.environ.get("GITHUB_SERVER_URL") != "https://github.com":
        raise RuntimeError("qualification is GitHub-only")
    if not re.fullmatch(r"[0-9a-f]{40}", candidate) or candidate != os.environ.get("GITHUB_SHA"):
        raise RuntimeError("candidate differs from dispatched workflow commit")
    if mutate and os.environ.get("PLURX_QUALIFICATION_MUTATE") != "disposable-hosted-vm":
        raise RuntimeError("native mutation requires the explicit disposable-hosted-vm acknowledgement")
    if os.uname().sysname == "Linux":
        if Path("/.dockerenv").exists() or Path("/run/.containerenv").exists():
            raise RuntimeError("a container cannot provide native host/session qualification")
        if run(["systemd-detect-virt", "--container"], check=False).returncode == 0:
            raise RuntimeError("container execution is forbidden")
        if not Path("/opt/hostedtoolcache").is_dir() or Path("/proc/1/comm").read_text().strip() != "systemd":
            raise RuntimeError("expected hosted runner VM with systemd PID 1")
    elif os.uname().sysname == "Darwin":
        if Path.home() != Path("/Users/runner"):
            raise RuntimeError("only the disposable hosted macOS runner account is permitted")
    else:
        raise RuntimeError("unsupported qualification host")


def wait_for(predicate, *, seconds=45, description="observation"):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            last = predicate()
            if last:
                return last
        except (RuntimeError, OSError, ValueError) as error:
            last = str(error)
        time.sleep(0.5)
    raise RuntimeError(f"timed out waiting for {description}: {last}")


class Receipt:
    def __init__(self, path, candidate, scope):
        self.path = Path(path)
        self.value = {"schema_version": 1, "candidate": candidate, "scope": scope,
                      "status": "running", "checks": [], "limitations": []}
        write_json(self.path, self.value)

    def check(self, name, observation):
        self.value["checks"].append({"name": name, "observation": observation})
        write_json(self.path, self.value)

    def __enter__(self):
        return self

    def __exit__(self, kind, error, _traceback):
        self.value["status"] = "failed" if error else "passed"
        if error:
            self.value["failure"] = str(error)
        write_json(self.path, self.value)
        return False


def check_build_artifacts(record):
    required = {"release-linux-amd64", "release-linux-arm64", "release-macos", "release-windows", "desktop-update-candidate"}
    if record.get("total_count", 0) > 100:
        raise ValueError("unexpected candidate artifact count; refuse an incomplete paginated inventory")
    matches = {}
    for artifact in record.get("artifacts", []):
        name = artifact.get("name")
        if name in required:
            if name in matches or artifact.get("expired") is not False or artifact.get("size_in_bytes", 0) <= 0:
                raise ValueError("duplicate, expired or empty candidate build artifact")
            matches[name] = {key: artifact[key] for key in ("id", "name", "size_in_bytes")}
    if set(matches) != required:
        raise ValueError("prior run is not a complete dry-run native build")
    return list(matches.values())


def check_build_metadata(manifest, signature, candidate, tree, assets):
    if len(signature) != 64:
        raise ValueError("prior run lacks the dry-run-only raw64 fixture signature")
    if manifest.get("source_commit") != candidate or manifest.get("packaging_commit") != candidate or manifest.get("source_tree") != tree:
        raise ValueError("prior build source/packaging/tree differ from the exact dispatched candidate")
    observed = {}
    for artifact in manifest["artifacts"]:
        name = artifact["name"]
        if not re.fullmatch(r"[a-zA-Z0-9._+-]+", name) or name in (".", "..") or name in observed:
            raise ValueError("unsafe or duplicate build asset name")
        path = Path(assets) / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size != artifact["size"] or sha256(path) != artifact["sha256"]:
            raise ValueError("downloaded native artifact differs from the exact build's metadata")
        observed[name] = {"size": artifact["size"], "sha256": artifact["sha256"]}
    return observed
