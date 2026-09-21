from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LEDGERS = (
    (ROOT / "vendor/hiqlite/PLURX-PATCH.md", 16),
    (ROOT / "vendor/hiqlite-wal/PLURX-PATCH.md", 3),
)


def table_rows(body: str) -> list[list[str]]:
    rows = []
    for line in body.splitlines():
        if not re.match(r"^\| \d+ \|", line):
            continue
        rows.append([field.strip() for field in line.strip("|").split("|")])
    return rows


class HiqlitePatchLedgerCase(unittest.TestCase):
    def test_each_prose_patch_has_one_owned_ledger_row(self):
        for path, expected in LEDGERS:
            with self.subTest(ledger=path.parent.name):
                body = path.read_text(encoding="utf-8")
                rows = table_rows(body)
                prose = body.split(rows[-1][4], 1)[1].split("\nRemove this vendor", 1)[0]

                self.assertIn("**Owner:** Paul Junod (repository owner).", body)
                self.assertEqual([int(row[0]) for row in rows], list(range(1, expected + 1)))
                self.assertEqual(len(rows), expected)
                self.assertEqual(
                    len(re.findall(r"(?m)^- ", prose)),
                    expected,
                    "the table and the explanatory patch bullets must move together",
                )

                for number, _patch, kind, upstream, drop_condition in rows:
                    self.assertIn(kind, {"generic bug", "plurx policy", "dependency-only"})
                    self.assertTrue(drop_condition, f"patch {number} has no exit condition")
                    if kind == "generic bug":
                        self.assertTrue(
                            upstream == "pending M6" or upstream.startswith(("http://", "https://")),
                            f"generic patch {number} needs an honest pending marker or public URL",
                        )
                    else:
                        self.assertEqual(upstream, "—")
                        self.assertTrue(drop_condition.startswith("Never;"))

    def test_unused_s3_edge_is_gated_and_vendor_override_is_gone(self):
        hiqlite = tomllib.loads((ROOT / "vendor/hiqlite/Cargo.toml").read_text())
        cryptr = hiqlite["dependencies"]["cryptr"]
        self.assertEqual(cryptr, {"version": "0.10", "default-features": False})
        self.assertEqual(hiqlite["features"]["s3"], ["backup", "cryptr/s3"])

        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
        self.assertNotIn("s3-simple", workspace["patch"]["crates-io"])
        self.assertNotIn("vendor/s3-simple", workspace["workspace"]["exclude"])
        self.assertFalse((ROOT / "vendor/s3-simple").exists())
        self.assertNotIn('("s3-simple", "0.8.0")', (ROOT / "scripts/vendor-audit-lock").read_text())


if __name__ == "__main__":
    unittest.main()
