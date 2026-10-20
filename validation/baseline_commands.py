"""Run a frozen candidate's explicit command cells, retaining every original failure."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time


def utc():
    return datetime.now(timezone.utc).isoformat()


def source_identity(root):
    def git(*args):
        return subprocess.check_output(["git", "-C", str(root), *args], text=True, stderr=subprocess.PIPE).strip()
    result = dict(candidate_sha=git("rev-parse", "HEAD"), candidate_tree=git("rev-parse", "HEAD^{tree}"))
    if subprocess.run(["git", "-C", str(root), "diff", "--quiet", "HEAD", "--"], check=False).returncode:
        raise ValueError("tracked source differs from the frozen candidate")
    return result


def normalize(manifest, root):
    if not isinstance(manifest, dict):
        raise ValueError("manifest must be an object")
    for field in ("candidate_sha", "candidate_tree"):
        if not isinstance(manifest.get(field), str) or not re.fullmatch(r"[0-9a-f]{40}", manifest[field]):
            raise ValueError(f"{field} must be an exact Git object identity")
    cells = manifest.get("cells")
    if not isinstance(cells, list) or not cells:
        raise ValueError("manifest must contain nonempty cells")
    seen = set()
    normalized = []
    for cell in cells:
        if not isinstance(cell, dict) or set(cell) - {"id", "argv", "cwd", "timeout_seconds"}:
            raise ValueError("cell contains unsupported fields")
        name = cell.get("id")
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,100}", name) or name in seen:
            raise ValueError("cell id is missing, invalid or duplicated")
        seen.add(name)
        argv = cell.get("argv")
        if not isinstance(argv, list) or not argv or any(not isinstance(arg, str) or "\0" in arg for arg in argv) or not argv[0]:
            raise ValueError(f"{name}: argv must be a nonempty string array")
        timeout = cell.get("timeout_seconds")
        if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
            raise ValueError(f"{name}: timeout_seconds must be positive and finite")
        cwd = cell.get("cwd", ".")
        if not isinstance(cwd, str) or Path(cwd).is_absolute():
            raise ValueError(f"{name}: cwd must be relative")
        resolved = (root / cwd).resolve()
        if not resolved.is_relative_to(root) or not resolved.is_dir():
            raise ValueError(f"{name}: cwd escapes the source or does not exist")
        normalized.append(dict(id=name, argv=argv, cwd=cwd, timeout_seconds=timeout))
    return normalized


def terminate_group(process, grace):
    """Do not mistake leader exit for descendant cleanup."""
    signals = []
    for sig, allowance in ((signal.SIGTERM, grace), (signal.SIGKILL, 1.0)):
        try:
            os.killpg(process.pid, sig)
            signals.append(signal.Signals(sig).name)
        except ProcessLookupError:
            break
        deadline = time.monotonic() + allowance
        while time.monotonic() < deadline:
            process.poll()
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                break
            time.sleep(min(0.05, max(0, deadline - time.monotonic())))
    process.wait()
    return signals


def run_cell(cell, root, output, grace):
    result = dict(cell, started_utc=utc(), timed_out=False, returncode=None, success=False,
                  leaked_process_group=False)
    started = time.monotonic()
    log = output / (cell["id"] + ".log")
    result["log"] = log.name
    process = None
    try:
        with log.open("wb") as stream:
            process = subprocess.Popen(cell["argv"], cwd=root / cell["cwd"], stdout=stream,
                                       stderr=subprocess.STDOUT, start_new_session=True)
            try:
                result["returncode"] = process.wait(timeout=cell["timeout_seconds"])
            except subprocess.TimeoutExpired:
                result["timed_out"] = True
                result["cleanup_signals"] = terminate_group(process, grace)
                result["returncode"] = process.returncode
            if not result["timed_out"]:
                try:
                    os.killpg(process.pid, 0)
                except ProcessLookupError:
                    pass
                else:
                    # A reaped leader does not prove that its owned group has
                    # exited. Preserve the original status, fail the leak, and
                    # reap the group before the next independent cell starts.
                    result["leaked_process_group"] = True
                    result["cleanup_signals"] = terminate_group(process, grace)
        result["success"] = (result["returncode"] == 0 and not result["timed_out"]
                             and not result["leaked_process_group"])
    except (OSError, subprocess.SubprocessError) as error:
        result["error"] = str(error)
        if process is not None:
            try:
                result["cleanup_signals"] = terminate_group(process, grace)
            except (OSError, subprocess.SubprocessError) as cleanup_error:
                # A sandbox or OS refusal is failed cleanup evidence, not a
                # reason to lose this cell's original return code and log.
                result["cleanup_error"] = str(cleanup_error)
                result["cleanup_error_errno"] = getattr(cleanup_error, "errno", None)
            result["returncode"] = process.returncode
    finally:
        result["finished_utc"] = utc()
        result["seconds"] = round(time.monotonic() - started, 3)
    return result


def run_manifest(manifest_path, output, root=None, grace=5.0):
    root = Path(root or Path.cwd()).resolve()
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = dict(schema=1, success=False, started_utc=utc(), cells=[], errors=[], blocked_cells=[])
    try:
        raw = Path(manifest_path).read_bytes()
        report["manifest_sha256"] = hashlib.sha256(raw).hexdigest()
        (output / "input-manifest.json").write_bytes(raw)
        manifest = json.loads(raw)
        cells = normalize(manifest, root)
        actual = source_identity(root)
        report["source"] = actual
        if any(actual[field] != manifest[field] for field in ("candidate_sha", "candidate_tree")):
            raise ValueError("manifest differs from the frozen source candidate")
        report["normalized_cells"] = cells
        for index, cell in enumerate(cells):
            outcome = run_cell(cell, root, output, grace)
            report["cells"].append(outcome)
            # Persist progress before starting an independent cell.
            (output / "results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
            if outcome.get("cleanup_error"):
                report["errors"].append(f"{cell['id']}: process group cleanup failed; remaining cells blocked")
                report["blocked_cells"] = [remaining["id"] for remaining in cells[index + 1:]]
                break
        if source_identity(root) != actual:
            raise ValueError("source identity changed during command execution")
        report["success"] = (not report["errors"] and len(report["cells"]) == len(cells)
                             and all(cell["success"] for cell in report["cells"]))
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        report["errors"].append(str(error))
    report["finished_utc"] = utc()
    temporary = output / "results.json.tmp"
    temporary.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    temporary.replace(output / "results.json")
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    report = run_manifest(args.manifest, args.output)
    print(json.dumps(dict(success=report["success"], outcomes=[dict(id=cell["id"], success=cell["success"],
                     returncode=cell["returncode"], timed_out=cell["timed_out"]) for cell in report["cells"]], errors=report["errors"])))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
