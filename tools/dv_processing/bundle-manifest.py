"""Write and verify the installed helper identity; no GPU or encoder invocation."""
import argparse
import hashlib
import json
from pathlib import Path

INPUTS = ("base_dv_renderer.h", "dv_trace.h", "fel_renderer.h", "nlq_clipping.h",
          "nut_timing.h", "rgb48.h", "segment_decode_render.c")
GRAPH = "p7-fel-linear-dz-bt2020-pq-master-clip-v1"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build(source, root):
    source_digest = hashlib.sha256()
    for name in sorted(INPUTS):
        source_digest.update(name.encode() + b"\0" + bytes.fromhex(sha(source / name)))
    abi = json.loads((root / "sources/abi.json").read_text())
    if abi.get("libplacebo") != 374 or any(abi.get(name, 0) <= 0 for name in ("avcodec", "avformat", "avutil")):
        raise ValueError("unsupported installed helper ABI")
    manifest = {"schema": 1, "graph": GRAPH, "abi": abi,
        "tools": {name: {"path": "bin/" + filename, "sha256": sha(root / "bin" / filename)}
                  for name, filename in (("renderer", "segment_decode_render"), ("muxer", "mux_rgb"), ("author", "author_p81"))},
        "libraries": [{"path": "lib/libplacebo.so.374", "sha256": sha(root / "lib/libplacebo.so.374")}],
        "parser_sha256": sha(root / "sources/libdovi.a"),
        "source_sha256": source_digest.hexdigest(),
        "environment": {"LD_LIBRARY_PATH": "lib"}}
    (root / "build-identity.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify(source, root)


def verify(source, root):
    manifest = json.loads((root / "build-identity.json").read_text())
    assert set(manifest) == {"schema", "graph", "abi", "tools", "libraries", "parser_sha256", "source_sha256", "environment"}
    assert manifest["schema"] == 1 and manifest["graph"] == GRAPH
    assert manifest["abi"] == json.loads((root / "sources/abi.json").read_text())
    assert manifest["abi"].get("libplacebo") == 374
    assert all(type(manifest["abi"].get(name)) is int and manifest["abi"][name] > 0
               for name in ("avcodec", "avformat", "avutil"))
    assert manifest["environment"] == {"LD_LIBRARY_PATH": "lib"}, "leave Vulkan device discovery untouched"
    digest = hashlib.sha256()
    for name in sorted(INPUTS):
        digest.update(name.encode() + b"\0" + bytes.fromhex(sha(source / name)))
    assert digest.hexdigest() == manifest["source_sha256"]
    assert sha(root / "sources/libdovi.a") == manifest["parser_sha256"]
    for artifact in [*manifest["tools"].values(), *manifest["libraries"]]:
        target = (root / artifact["path"]).resolve()
        assert target.is_relative_to(root.resolve()) and sha(target) == artifact["sha256"]
    print(json.dumps({"schema": manifest["schema"], "abi": manifest["abi"],
                      "manifest_sha256": sha(root / "build-identity.json"), "source_sha256": manifest["source_sha256"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("root", type=Path)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    (verify if args.verify else build)(args.source, args.root)
