#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Create a source-only review archive. No runtime, native libraries or credentials."""
import argparse
from pathlib import Path
import zipfile

FILES = ("native_host.py", "errors.py", "kernel_cec.py", "libcec_backend.py", "install.py", "README.md",
         "extension/manifest.json", "extension/package.json", "extension/worker.js", "extension/bridge.js",
         "extension/content.js", "extension/popup.html", "extension/popup.js", "extension/popup.css")

def package(output):
    root = Path(__file__).resolve().parent
    with zipfile.ZipFile(output, "x", zipfile.ZIP_DEFLATED) as archive:
        for name in FILES:
            archive.write(root / name, "cinema-desktop-remote/" + name)

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    package(parser.parse_args().output)
