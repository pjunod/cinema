"""Archive the retained public-input/cache prerequisite, never credentials or tools."""
import hashlib
import json
from pathlib import Path
import sys
import tarfile

bundle = Path(__file__).resolve().parent
inputs, registry, output = map(Path, sys.argv[1:4])
if output.exists() or output.is_symlink():
    raise ValueError("new prerequisite archive required")
lock = json.loads((bundle / "input-lock.json").read_text())
cache = json.loads((bundle / "cargo-cache-lock.json").read_text())
selected = {"inputs/" + name: inputs / name for name in lock}
selected["inputs/image-id.txt"] = inputs / "image-id.txt"
selected.update({"registry/" + name: registry / name for name in cache})
expected = {"inputs/" + name: checksum for name, checksum in lock.items()}
expected.update({"registry/" + name: checksum for name, checksum in cache.items()})
expected["inputs/image-id.txt"] = hashlib.sha256(selected["inputs/image-id.txt"].read_bytes()).hexdigest()
for name, path in selected.items():
    if path.is_symlink() or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected[name]:
        raise ValueError("retained prerequisite identity mismatch")
with output.open("xb") as destination, tarfile.open(fileobj=destination, mode="w") as archive:
    for name, path in sorted(selected.items()):
        info = tarfile.TarInfo(name)
        info.size = path.stat().st_size
        info.mode = 0o444
        info.mtime = 0
        with path.open("rb") as payload:
            archive.addfile(info, payload)
record = {"schema": 1, "archive_name": output.name,
          "archive_sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
          "archive_bytes": output.stat().st_size, "files": expected,
          "image_id": (inputs / "image-id.txt").read_text().strip(),
          "scope": "retained public source/fixture inputs plus exact public Cargo cache/index; no image contents/tool binaries/credentials",
          "bootstrap_limit": "fetching today's sparse index is not promised byte-identical; image must already be available or separately rebuilt and reviewed"}
with (bundle / "prerequisites.json").open("x") as metadata:
    metadata.write(json.dumps(record, indent=2, allow_nan=False) + "\n")
print(json.dumps({key: value for key, value in record.items() if key != "files"}, indent=2))
