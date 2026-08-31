from __future__ import annotations

import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/vendor-audit-lock"


def packages(path: Path) -> list[tuple[str, str, str]]:
    parsed = tomllib.loads(path.read_text(encoding="utf-8"))
    return [
        (
            str(package["name"]),
            str(package["version"]),
            str(package.get("source", "")),
        )
        for package in parsed["package"]
    ]


class VendorAuditLockCase(unittest.TestCase):
    def test_additional_lock_is_completely_deduplicated_into_one_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            root_only = temporary / "root.lock"
            combined = temporary / "combined.lock"
            subprocess.run(
                [str(SCRIPT), "--output", str(root_only)],
                cwd=ROOT,
                check=True,
            )
            subprocess.run(
                [
                    str(SCRIPT),
                    "--output",
                    str(combined),
                    "--additional-lock",
                    "fuzz/Cargo.lock",
                ],
                cwd=ROOT,
                check=True,
            )

            root_packages = set(packages(root_only))
            fuzz_packages = set(packages(ROOT / "fuzz/Cargo.lock"))
            combined_packages = packages(combined)

            self.assertEqual(set(combined_packages), root_packages | fuzz_packages)
            self.assertEqual(len(combined_packages), len(set(combined_packages)))
            self.assertIn(
                (
                    "libfuzzer-sys",
                    "0.4.13",
                    "registry+https://github.com/rust-lang/crates.io-index",
                ),
                combined_packages,
            )


if __name__ == "__main__":
    unittest.main()
