"""Run a frozen candidate's explicit command cells, retaining every original failure."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import errno
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


def parse_proc_member(text):
    """Read identity/state without recording command lines or executable names."""
    end = text.rfind(")")
    if end < 0:
        raise ValueError("malformed proc stat")
    fields = text[end + 1:].split()
    return dict(pid=int(text.split("(", 1)[0]), state=fields[0],
                ppid=int(fields[1]), pgid=int(fields[2]), start_ticks=int(fields[19]))


def observe_group(pgid, proc_root=Path("/proc")):
    """Existence alone includes zombies; unknown observations remain fail-closed."""
    result = dict(pgid=pgid, observed_utc=utc(), status="unknown", members=[], errors=[])
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        result.update(status="absent", backend="killpg")
        return result
    except OSError as error:
        result.update(existence_probe_errno=error.errno, existence_probe_error=str(error))
        if error.errno not in (errno.EPERM, errno.EACCES):
            result.update(backend="killpg", error_errno=error.errno)
            result["errors"].append("group existence probe refused: " + str(error))
            return result
        # A permission refusal proves neither runnable work nor its absence.
        # Continue to state observation; only complete Z/X membership is safe.
    if sys.platform == "linux":
        result["backend"] = "proc"
        try:
            entries = list(proc_root.iterdir())
        except OSError as error:
            result["errors"].append("proc process inventory unavailable")
            result["error_errno"] = error.errno
            return result
        for entry in entries:
            if not entry.name.isdigit():
                continue
            try:
                member = parse_proc_member((entry / "stat").read_text())
            except (FileNotFoundError, ProcessLookupError):
                continue  # Process exited during this observation.
            except (OSError, ValueError, IndexError):
                # Its group is unknown, so it cannot safely be excluded.
                result["errors"].append("unreadable process identity: " + entry.name)
                continue
            if member["pgid"] == pgid:
                result["members"].append(member)
    elif sys.platform == "darwin":
        result["backend"] = "ps"
        try:
            observation = subprocess.run(["ps", "-axo", "pid=,ppid=,pgid=,state="],
                                         text=True, capture_output=True, timeout=2, check=False)
        except (OSError, subprocess.TimeoutExpired) as error:
            result["errors"].append("bounded process-state inventory unavailable")
            result["error_errno"] = getattr(error, "errno", None)
            return result
        if observation.returncode:
            result["errors"].append("process-state inventory returned nonzero")
            return result
        for line in observation.stdout.splitlines():
            fields = line.split()
            try:
                pid, ppid, group = map(int, fields[:3])
                state = fields[3][0]
            except (ValueError, IndexError):
                result["errors"].append("malformed process-state inventory")
                continue
            if group == pgid:
                result["members"].append(dict(pid=pid, ppid=ppid, pgid=group,
                                               state=state, start_ticks=None))
        result["identity_limit"] = "macOS ps supplies state but Linux start ticks are unavailable"
    else:
        result.update(backend="unavailable")
        result["errors"].append("process-state observation unavailable on this platform")
        return result
    result["members"].sort(key=lambda member: member["pid"])
    if any(member["state"] not in ("Z", "X") for member in result["members"]):
        result["status"] = "live"
    elif result["errors"]:
        result["status"] = "unknown"
    elif result["members"]:
        result["status"] = "nonexecuting"
    else:
        # A group can disappear while its /proc entries are being read. If the
        # group still exists with no readable member, absence is not proved.
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            result["status"] = "absent"
        except OSError as error:
            result["error_errno"] = error.errno
            result["errors"].append("final group probe refused")
    return result


def wait_owned_leader(process, timeout, use_waitid=True):
    """Observe completion without reaping the child that anchors its process group.

    This executor is the sole waiter for its Popen child. WNOWAIT leaves the
    owned leader present, even after exit, until the final bounded reap.
    """
    anchored = use_waitid and all(hasattr(os, name) for name in ("waitid", "P_PID", "WEXITED", "WNOWAIT", "WNOHANG"))
    deadline = time.monotonic() + timeout
    if anchored:
        while True:
            try:
                observed = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
            except OSError as error:
                if error.errno == errno.EINTR:
                    if time.monotonic() >= deadline:
                        return dict(held=True, mechanism="waitid-WNOWAIT", timed_out=True, returncode=None)
                    continue
                if error.errno in (errno.EINVAL, errno.ENOSYS):
                    break  # Facility unavailable: use the conservative fallback.
                return dict(held=False, mechanism="waitid-error", timed_out=False, returncode=None,
                            error="owned leader wait observation failed", error_errno=error.errno)
            if observed is not None and observed.si_pid == process.pid:
                code = observed.si_status if observed.si_code == os.CLD_EXITED else -observed.si_status
                return dict(held=True, mechanism="waitid-WNOWAIT", timed_out=False, returncode=code)
            if time.monotonic() >= deadline:
                return dict(held=True, mechanism="waitid-WNOWAIT", timed_out=True, returncode=None)
            time.sleep(min(0.02, max(0, deadline - time.monotonic())))
    try:
        code = process.wait(timeout=max(0, deadline - time.monotonic()))
        return dict(held=False, mechanism="reaped-fallback", timed_out=False, returncode=code)
    except subprocess.TimeoutExpired:
        # wait(timeout) did not reap the leader. No poll/wait occurs again until
        # cleanup ends, so this owned running/exited child still anchors PGID.
        return dict(held=True, mechanism="unreaped-timeout-fallback", timed_out=True, returncode=None)


def anchor_still_owned(process, anchor):
    if not anchor or not anchor.get("held") or process.returncode is not None:
        return False
    if anchor["mechanism"] == "waitid-WNOWAIT":
        try:
            os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
        except OSError:
            return False
    return True


def terminate_group(process, grace, anchor=None):
    """Signal only while an unreaped owned leader prevents numeric PGID reuse."""
    evidence = dict(signals=[], snapshots=[], complete=False)
    current = observe_group(process.pid)
    evidence["snapshots"].append(dict(stage="before-cleanup", **current))
    for sig, allowance in ((signal.SIGTERM, grace), (signal.SIGKILL, 1.0)):
        if current["status"] in ("absent", "nonexecuting"):
            break
        if not anchor_still_owned(process, anchor):
            evidence["error"] = "owned leader no longer anchors process group; refusing numeric PGID signal"
            return evidence
        try:
            os.killpg(process.pid, sig)
            evidence["signals"].append(signal.Signals(sig).name)
        except ProcessLookupError:
            current = observe_group(process.pid)
            evidence["snapshots"].append(dict(stage="group-disappeared", **current))
            break
        except OSError as error:
            evidence.update(error=str(error), error_errno=error.errno)
            evidence["snapshots"].append(dict(stage="signal-refused", **observe_group(process.pid)))
            return evidence
        deadline = time.monotonic() + allowance
        while True:
            # Do not poll: reaping the leader would release the PGID anchor.
            current = observe_group(process.pid)
            if current["status"] in ("absent", "nonexecuting") or time.monotonic() >= deadline:
                break
            time.sleep(min(0.05, max(0, deadline - time.monotonic())))
        evidence["snapshots"].append(dict(stage="after-" + signal.Signals(sig).name, **current))
    evidence["complete"] = current["status"] in ("absent", "nonexecuting")
    if not evidence["complete"]:
        evidence["error"] = "live or unknown process group remains after bounded cleanup"
        evidence["error_errno"] = current.get("error_errno")
    return evidence


def reap_owned_leader(process, result):
    try:
        code = process.wait(timeout=1.0)
        if not result.get("leader_wait_error"):
            result["returncode"] = code
        result["leader_reaped_after_cleanup"] = True
    except (OSError, subprocess.TimeoutExpired) as error:
        result["cleanup_complete"] = False
        result["cleanup_error"] = "owned leader could not be reaped after bounded cleanup"
        result["cleanup_error_errno"] = getattr(error, "errno", None)


def record_cleanup(result, cleanup):
    result["cleanup_signals"] = cleanup["signals"]
    result["group_observations"].extend(cleanup["snapshots"])
    result["cleanup_complete"] = cleanup["complete"]
    if not cleanup["complete"]:
        result["cleanup_error"] = cleanup.get("error", "process group cleanup unqualified")
        result["cleanup_error_errno"] = cleanup.get("error_errno")


def run_process(argv, stdout, stderr, timeout, *, cwd=None, grace=5.0):
    """Run with bounded, anchored ownership while preserving separate streams."""
    result = dict(started_utc=utc(), timed_out=False, returncode=None, success=False,
                  leaked_process_group=False, group_observations=[], cleanup_complete=False)
    started = time.monotonic()
    process = None
    anchor = None
    try:
        process = subprocess.Popen(argv, cwd=cwd, stdout=stdout, stderr=stderr,
                                   start_new_session=True)
        anchor = wait_owned_leader(process, timeout)
        result["leader_anchor"] = dict(pid=process.pid, **anchor)
        result["timed_out"] = anchor["timed_out"]
        result["returncode"] = anchor["returncode"]
        if anchor.get("error"):
            result["leader_wait_error"] = anchor["error"]
        current = observe_group(process.pid)
        result["group_observations"].append(dict(stage="after-wait-before-reap", **current))
        result["leaked_process_group"] = not result["timed_out"] and anchor["held"] and current["status"] == "live"
        result["cleanup_observation_unknown"] = (current["status"] == "unknown"
                                                 or (current["status"] == "live" and not anchor["held"]))
        if result["timed_out"] or current["status"] in ("live", "unknown"):
            record_cleanup(result, terminate_group(process, grace, anchor))
        else:
            result["cleanup_complete"] = True
        # The anchor is released only after the final group observation and
        # every signal. No further numeric process-group signal is issued.
        reap_owned_leader(process, result)
        result["success"] = (result["returncode"] == 0 and not result["timed_out"]
                             and not result["leaked_process_group"] and not result.get("leader_wait_error")
                             and not result["cleanup_observation_unknown"] and result["cleanup_complete"])
    except (OSError, subprocess.SubprocessError) as error:
        result["error"] = str(error)
        if process is not None:
            record_cleanup(result, terminate_group(process, grace, anchor))
            reap_owned_leader(process, result)
    finally:
        result["finished_utc"] = utc()
        result["seconds"] = round(time.monotonic() - started, 3)
    return result



def run_cell(cell, root, output, grace):
    log = output / (cell["id"] + ".log")
    try:
        with log.open("wb") as stream:
            result = run_process(cell["argv"], stream, subprocess.STDOUT,
                                 cell["timeout_seconds"], cwd=root / cell["cwd"], grace=grace)
    except OSError as error:
        result = dict(started_utc=utc(), finished_utc=utc(), seconds=0.0,
                      timed_out=False, returncode=None, success=False,
                      cleanup_complete=True, error=str(error))
    return dict(cell, **result, log=log.name)


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
