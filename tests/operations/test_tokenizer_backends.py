"""K-08 M4's two-backend tokenizer comparison stays a comparison of plurx's
tokenizer.

`make tokenizer-backends` builds spikes/tokenizer-backends, a separate workspace
that depends only on `tokenizers`, once per regex backend. It proves something
about plurxd only while it builds the tokenizers release plurxd resolves, over
the dependency versions plurxd resolves. These assertions hold its manifest and
lock to the root lock so a tokenizers bump in plurxd cannot leave the harness
measuring an older release.
"""

from __future__ import annotations

import tomllib
import unittest
from collections import defaultdict
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HARNESS = ROOT / "spikes/tokenizer-backends"

# tokenizers 0.22 requires fancy-regex ^0.14 and plurxd never builds that
# backend, so this is the one crate version the root lock cannot share.
BACKEND_ONLY = {("fancy-regex", "0.14.0")}


def lock(path: Path) -> list[dict]:
    return tomllib.loads(path.read_text(encoding="utf-8"))["package"]


class TokenizerBackendHarnessCase(unittest.TestCase):
    def setUp(self):
        self.manifest = tomllib.loads((HARNESS / "Cargo.toml").read_text(encoding="utf-8"))
        self.root = defaultdict(set)
        for package in lock(ROOT / "Cargo.lock"):
            self.root[package["name"]].add(package["version"])

    def test_it_builds_the_tokenizers_release_plurxd_resolves(self):
        (resolved,) = self.root["tokenizers"]
        spec = self.manifest["dependencies"]["tokenizers"]
        self.assertEqual(spec["version"], f"={resolved}")
        self.assertIs(spec["default-features"], False)
        self.assertEqual(
            self.manifest["features"],
            {"onig": ["tokenizers/onig"], "fancy-regex": ["tokenizers/fancy-regex"]},
            "each harness feature must select exactly one tokenizers backend",
        )
        self.assertNotIn("default", self.manifest["features"])

    def test_its_lock_shares_the_root_lock_versions(self):
        drift = sorted(
            (package["name"], package["version"])
            for package in lock(HARNESS / "Cargo.lock")
            if "source" in package
            and package["version"] not in self.root.get(package["name"], set())
            and (package["name"], package["version"]) not in BACKEND_ONLY
        )
        self.assertEqual(
            drift,
            [],
            "the harness resolves crate versions plurxd does not; align them "
            "with `cargo update --manifest-path spikes/tokenizer-backends/"
            "Cargo.toml -p <crate>@<version> --precise <root version>`",
        )

    def test_it_is_its_own_workspace(self):
        self.assertIn("workspace", self.manifest)
        members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertNotIn("spikes/tokenizer-backends", members["workspace"]["members"])


if __name__ == "__main__":
    unittest.main()
