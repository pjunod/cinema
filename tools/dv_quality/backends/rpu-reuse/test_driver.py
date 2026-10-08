"""Run actual decode then inject late exit/crash; driver must preserve real failure."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import check_reuse

root = Path(sys.argv[1])
evidence = root / "driver-evidence"
evidence.mkdir()
rows = []
for mode in ("missing-rpu", "seek-reset"):
    for failure, expected in (("exit", 37), ("crash", 139)):
        scratch = evidence / f"{mode}-{failure}"
        scratch.mkdir()
        for name in ("normal.mkv", "missing-rpu.mkv"):
            shutil.copyfile(root / name, scratch / name)
        wrapper = scratch / "decoder-wrapper.py"
        wrapper.write_text(
            "#!/usr/bin/python3\n"
            "import os,signal,subprocess,sys\n"
            f"status=subprocess.run([{str(root / 'decode_reuse')!r},*sys.argv[1:]]).returncode\n"
            "if status: sys.exit(status)\n"
            f"if os.path.basename(sys.argv[2]) == {mode!r}:\n"
            + ("    os.kill(os.getpid(),signal.SIGSEGV)\n" if failure == "crash" else "    sys.exit(37)\n")
        )
        wrapper.chmod(0o755)
        result = subprocess.run(["sh", str(root / "decode_cases.sh"), str(scratch), str(wrapper)],
                                capture_output=True, text=True)
        (scratch / "driver.stdout").write_text(result.stdout)
        (scratch / "driver.stderr").write_text(result.stderr)
        recorded = (scratch / mode / "decode-status.txt").read_text()
        if result.returncode != expected or recorded != f"{expected}\n":
            raise ValueError("driver did not propagate exact late failure")
        if (scratch / "decode-success.txt").exists():
            raise ValueError("late failure produced a clean success marker")
        events = check_reuse.lines(scratch / mode / "events.jsonl")
        check_reuse.exact(events, check_reuse.expected_events(mode), "injection did not complete actual expected decode")
        try:
            check_reuse.inspect_decoded(scratch, mode)
        except ValueError as error:
            if str(error) != "decode process did not exit cleanly":
                raise
        else:
            raise ValueError("crash after complete writes falsely accepted")
        rows.append({"case": mode, "failure": failure, "driver_status": result.returncode,
                     "recorded_status": int(recorded), "complete_frame_events": sum(e["kind"] == "decoded_frame" for e in events),
                     "acceptance": "refused before reading artifacts",
                     "files": {str(p.relative_to(scratch)): hashlib.sha256(p.read_bytes()).hexdigest()
                               for p in sorted(scratch.rglob("*")) if p.is_file()}})
with (root / "driver-results.json").open("x") as output:
    output.write(json.dumps(rows, indent=2, allow_nan=False) + "\n")
print("4 actual late failure controls passed (exit 37 / SIGSEGV 139).")
