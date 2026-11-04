"""Fetch pinned current-runtime sources without changing historical probe pins.

Reuse the historical downloader's bounded extraction, never its library pin.
The generic utility is retained in bundle build-inputs for source-only relinking.
"""
import argparse
import json
import os
from pathlib import Path
import runpy
import shutil

LIBPLACEBO = {
    "component": "libplacebo",
    "revision": "v7.387.0",
    "filename": "libplacebo-source.tar.gz",
    "url": "https://code.videolan.org/videolan/libplacebo/-/archive/v7.387.0/libplacebo-v7.387.0.tar.gz",
    "sha256": "3bf5d72786682c194387c33d563872f6bfe202a9816f9dde6535a31a9b2e0d74",
}


def fetch_runtime(root, inputs):
    utility = runpy.run_path(str(inputs / "libplacebo/fetch_sources.py"))
    # The Vulkan headers pin is shared; the old renderer source remains historical.
    headers_source = next(source for source in utility["SOURCES"]
                          if source["component"] == "Vulkan-Headers")
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    library = utility["fetch"](root, LIBPLACEBO)
    headers = utility["fetch"](root, headers_source)
    shutil.copytree(headers, library / "3rdparty/Vulkan-Headers", dirs_exist_ok=True)
    (root / "source-lock.json").write_text(json.dumps(
        [LIBPLACEBO, headers_source], indent=2, allow_nan=False) + "\n")
    return library


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    args = parser.parse_args()
    inputs = Path(os.environ.get("PLURX_DV_BUILD_INPUTS",
                  str(Path(__file__).resolve().parent / "../dv_quality/backends")))
    fetch_runtime(args.root, inputs)
