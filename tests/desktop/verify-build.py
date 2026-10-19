#!/usr/bin/env python3
"""Verify immutable build transport provenance; this is not production update trust."""
import argparse
import importlib.machinery
import importlib.util
import json
from pathlib import Path
from common import check_build_metadata, run, write_json

p = argparse.ArgumentParser()
p.add_argument("--candidate", required=True)
p.add_argument("--source", type=Path, required=True)
p.add_argument("--artifacts", type=Path, required=True)
p.add_argument("--metadata", type=Path, required=True)
p.add_argument("--output", type=Path, required=True)
a = p.parse_args()
loader = importlib.machinery.SourceFileLoader("qualification_build_manifest", str(a.source / "scripts/create-desktop-update-manifest"))
spec = importlib.util.spec_from_loader(loader.name, loader)
tool = importlib.util.module_from_spec(spec)
loader.exec_module(tool)
manifest = json.loads((a.metadata / "plurx-update-v1.json").read_bytes())
tool.validate(manifest)
if run(["git", "-C", a.source, "rev-parse", "HEAD"]).stdout.strip() != a.candidate:
    raise RuntimeError("qualification source checkout differs from exact dispatched candidate")
tree = run(["git", "-C", a.source, "rev-parse", "HEAD^{tree}"]).stdout.strip()
assets = check_build_metadata(manifest, (a.metadata / "plurx-update-v1.json.sig").read_bytes(), a.candidate, tree, a.artifacts)
actual = {p.name for p in a.artifacts.iterdir() if p.is_file()}
if actual != set(assets) | {f"plurxd-{manifest['version']}-windows-x86_64.zip"}:
    raise RuntimeError("downloaded build asset set is incomplete or includes unexpected members")
write_json(a.output, {"schema_version": 1, "scope": "immutable-dry-run-build-transport-provenance-not-production-update-authentication",
    "candidate": a.candidate, "source_tree": tree, "version": manifest["version"], "native_assets": assets})
