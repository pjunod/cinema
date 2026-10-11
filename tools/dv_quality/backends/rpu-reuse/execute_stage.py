"""Execute one bounded decode/cache stage and bind its actual consumed bytes."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def digest(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError("stage requires regular input/tool/output")
    return hashlib.sha256(path.read_bytes()).hexdigest()


root = Path(sys.argv[1]).resolve()
stage, mode = sys.argv[2:4]
helper = Path(sys.argv[4]).resolve()
if stage == "decode":
    if mode not in ("normal", "missing-rpu", "seek-reset"):
        raise ValueError("unknown decode mode")
    directory = root / mode
    input_path = root / ("missing-rpu.mkv" if mode == "missing-rpu" else "normal.mkv")
    decoder_mode = "seek-reset" if mode == "seek-reset" else "normal"
    arguments = [str(helper), str(input_path), str(directory), decoder_mode]
    consumed = [helper, input_path]
    stdout, stderr = directory / "events.jsonl", directory / "decoder.stderr"
    status_path = directory / "decode-status.txt"
    receipt = directory / "execution.json"
elif stage == "cache":
    if mode not in ("same", "cold", "flush", "wrong-compression", "no-reset"):
        raise ValueError("unknown cache mode")
    input_path = root / "normal.mkv"
    seed = root / "normal/epoch-0-bl-frame-000.rpu.rbsp"
    reuse = root / "normal/epoch-0-bl-frame-001.rpu.rbsp"
    arguments = [str(helper), str(input_path), str(seed), str(reuse), mode]
    consumed = [helper, input_path, seed, reuse]
    stdout, stderr = root / f"cache-{mode}.jsonl", root / f"cache-{mode}.stderr"
    status_path = root / f"cache-{mode}.status"
    receipt = root / f"cache-{mode}.execution.json"
else:
    raise ValueError("unknown stage")


def relative(path):
    return str(path.relative_to(root))


before = {relative(path): digest(path) for path in consumed}
with stdout.open("x") as output, stderr.open("x") as diagnostic:
    process = subprocess.run(arguments, stdout=output, stderr=diagnostic)
status = process.returncode if process.returncode >= 0 else 128 - process.returncode
status_path.write_text(f"{status}\n")
after = {relative(path): digest(path) for path in consumed}
if before != after:
    raise ValueError("consumed bytes changed during execution")
outputs = [stdout, stderr, status_path]
if stage == "decode":
    outputs.extend(sorted(directory.glob("*.yuv420p10le")))
    outputs.extend(sorted(directory.glob("*.rpu.nal")))
record = {"schema": 1, "stage": stage, "mode": mode,
          "argv": [relative(Path(arg)) if Path(arg).is_absolute() else arg for arg in arguments],
          "process_returncode": process.returncode, "shell_status": status,
          "consumed_before": before, "consumed_after": after,
          "outputs": {relative(path): digest(path) for path in outputs}}
with receipt.open("x") as output:
    output.write(json.dumps(record, indent=2, allow_nan=False) + "\n")
sys.exit(status)
