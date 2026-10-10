"""Bind the just-completed pinned parser outputs to their actual raw inputs."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
inspector = root / "dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/target/release/examples/m0_inspect_reuse"
rows = {}
for mode in ("normal", "missing-rpu", "seek-reset"):
    for raw in sorted((root / mode).glob("*.rpu.nal")):
        files = (raw, raw.with_suffix(".json"), raw.with_suffix(".rbsp"))
        rows[str(raw.relative_to(root))] = {
            str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in files
        }
rows["inspector"] = {str(inspector.relative_to(root)): hashlib.sha256(inspector.read_bytes()).hexdigest()}
with (root / "parsed-provenance.json").open("x") as output:
    output.write(json.dumps(rows, indent=2, allow_nan=False) + "\n")
