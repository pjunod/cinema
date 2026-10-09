"""Freeze slim text evidence and explicit runtime inventory after successful replay."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import check_reuse


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    with path.open("x") as output:
        output.write(json.dumps(value, indent=2, allow_nan=False) + "\n")


bundle = Path(__file__).resolve().parent
runtime = Path(sys.argv[1]).resolve()
result = check_reuse.inspect(runtime)
if result != json.loads((runtime / "results.json").read_text()):
    raise ValueError("fresh results differ from current evidence")
for filename in ("results.json", "checker-tests.log", "driver-tests.log", "driver-results.json", "dependencies.json", "toolchain-observation.txt"):
    shutil.copyfile(runtime / filename, bundle / filename)
traces = bundle / "traces"
traces.mkdir()
for mode in ("normal", "seek-reset", "missing-rpu"):
    destination = traces / mode
    destination.mkdir()
    for source in sorted((runtime / mode).iterdir()):
        if source.suffix in (".json", ".jsonl", ".stderr", ".txt", ".log"):
            shutil.copyfile(source, destination / source.name)
for source in sorted(runtime.glob("cache-*")):
    if source.is_file():
        shutil.copyfile(source, traces / source.name)
for name in ("p7-refusal.jsonl", "p7-refusal.stderr", "p7-refusal.status", "parsed-provenance.json"):
    shutil.copyfile(runtime / name, traces / name)

source_tree = runtime / "FFmpeg-bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa"
libdovi = runtime / "dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1"
source_evidence = json.loads((bundle / "source-evidence.json").read_text())["files"]
for name, checksum in source_evidence.items():
    component, relative = name.split("/", 1)
    actual = (source_tree if component == "ffmpeg" else libdovi) / relative
    if sha(actual) != checksum:
        raise ValueError("compiled source anchor differs from pinned inspection")
identity_files = [runtime / name for name in ("mux_reuse", "decode_reuse", "cache_probe", "image-id.txt", "rust-version.txt")]
identity_files.extend(sorted((runtime / "ffmpeg-prefix/lib").glob("*.a")))
identity_files.extend([source_tree / "config.h", source_tree / "ffbuild/config.mak",
                       libdovi / "target/release/examples/m0_reuse", libdovi / "target/release/examples/m0_inspect_reuse"])
identity = {"schema": 1, "image_id": (runtime / "image-id.txt").read_text().strip(),
            "ffmpeg_revision": "bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa",
            "libdovi_revision": "83e1fdad6dcd5995556235946e7c5c0f9010d5a1",
            "fresh_builds": ["static FFmpeg prefix", "standalone Rust author/inspector examples", "C mux/decode/cache helpers"],
            "reused_dependencies": ["verified prior Docker image", "393-file verified public offline Cargo cache/index"],
            "ffmpeg_patch": "none", "libdovi_patch": "appended workspace plus reviewed resolved Cargo lock",
            "system_dependency_resolution": "prior image uses live apt/pip; not a deterministic rebuild lock",
            "files": {str(path.relative_to(runtime)): sha(path) for path in identity_files}}
save(runtime / "build-identity.json", identity)
shutil.copyfile(runtime / "build-identity.json", bundle / "build-identity.json")

selected = {path for path in runtime.iterdir() if path.is_file()}
for directory in ("normal", "missing-rpu", "seek-reset", "fresh-rpus", "driver-evidence", "ffmpeg-prefix", "cargo-home/registry/cache", "cargo-home/registry/index"):
    selected.update(path for path in (runtime / directory).rglob("*") if path.is_file())
selected.update(identity_files)
if any(path.is_symlink() for path in selected):
    raise ValueError("inventoried evidence must be regular files")
selected.discard(runtime / "artifact-inventory.json")
files = {str(path.relative_to(runtime)): sha(path) for path in sorted(selected)}
inventory = {"schema": 1,
             "scope": "all top-level replay files plus declared evidence/dependency trees and actual build identity files",
             "included_trees": ["normal", "missing-rpu", "seek-reset", "fresh-rpus", "driver-evidence", "ffmpeg-prefix", "cargo-home/registry/cache", "cargo-home/registry/index"],
             "excluded_trees": ["extracted FFmpeg source/build intermediates except listed config files", "extracted libdovi source/target except listed example binaries", "extracted Cargo crate sources and package bookkeeping", "scratch-local temporary directory"],
             "files": files}
save(runtime / "artifact-inventory.json", inventory)
shutil.copyfile(runtime / "artifact-inventory.json", bundle / "artifact-inventory.json")
receipt = {"schema": 1, "status": "executed; adversarial result approval pending",
           "scope": "synthetic Profile 8 compatibility ID 6 limited explicit mapping reuse; no conformance/P8.1/HDR10/FEL qualification",
           "design_sha256": sha(bundle / "DESIGN.md"), "design_source_evidence_sha256": sha(bundle / "source-evidence.json"),
           "design_inference_correction": "Proposed HDR10-compatible description not established; actual serialized compatibility ID 6 is not labelled P8.1",
           "exact_source_replay": "replay 2, fresh FFmpeg/libdovi/example/helper builds; no-network execution",
           "results_sha256": sha(runtime / "results.json"), "build_identity_sha256": sha(runtime / "build-identity.json"),
           "artifact_inventory_sha256": sha(runtime / "artifact-inventory.json"), "runtime_inventory_entries": len(files),
           "checker_controls": 27, "actual_late_failure_controls": 4,
           "accepted_decoded_frames": 9, "explicit_reuse_frames": 6,
           "negative_stages": ["cold cache parse", "flush cache parse", "wrong compression parse", "declared new epoch without reset acceptance", "omitted raw RPU acceptance", "Profile 7 compressor initialization"],
           "tolerances": "Exact byte/typed semantic checks; no numerical quality threshold. Syntax/cohort, A/B coefficients and timeline declared before output inspection; fixed single-threaded lifecycle/DTS snapshots and stale-cache observations are bounded regression expectations recorded after execution.",
           "remaining": ["Profile/container conformance", "certified Dolby reference", "general/P7 reuse", "multiple-ID fallback", "extended/DM compression", "compressor cross-epoch state", "recovery after metadata parser error", "production adapter", "FEL/render/target-mapping/trims/performance/playback qualification"]}
save(bundle / "receipt.json", receipt)
ledger = {str(path.relative_to(bundle)): sha(path) for path in sorted(bundle.rglob("*")) if path.is_file() and path.name != "bundle-hashes.json"}
save(bundle / "bundle-hashes.json", ledger)
print(json.dumps({"source_ledger_entries": len(ledger), "runtime_inventory_entries": len(files),
                  "receipt_sha256": sha(bundle / "receipt.json"), "ledger_sha256": sha(bundle / "bundle-hashes.json"),
                  "inventory_sha256": sha(bundle / "artifact-inventory.json")}, indent=2))
