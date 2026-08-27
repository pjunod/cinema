from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tests/playback/rolling-producer-owners.toml"


class RollingProducerOwnershipInventoryTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.catalog = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
        cls.source_path = ROOT / cls.catalog["source"]
        source = cls.source_path.read_text(encoding="utf-8")
        end_anchor = cls.catalog["production_end_anchor"]
        if source.count(end_anchor) != 1:
            raise AssertionError(
                f"production boundary {end_anchor!r} must occur exactly once in "
                f"{cls.catalog['source']}"
            )
        cls.production_source = source.split(end_anchor, 1)[0]
        module_root = ROOT / cls.catalog["module_root"]
        cls.module_paths = tuple(sorted(module_root.rglob("*.rs")))
        cls.module_source = "\n".join(
            path.read_text(encoding="utf-8") for path in cls.module_paths
        )

    def test_catalog_has_unique_complete_rows(self) -> None:
        self.assertEqual(self.catalog.get("version"), 1)
        symbols = self.catalog["symbols"]
        module_symbols = self.catalog["module_symbols"]
        entries = self.catalog["entrypoints"]
        symbol_ids = [entry["id"] for entry in symbols]
        module_symbol_ids = [entry["id"] for entry in module_symbols]
        entry_ids = [entry["id"] for entry in entries]
        self.assertEqual(len(symbol_ids), len(set(symbol_ids)))
        self.assertEqual(len(module_symbol_ids), len(set(module_symbol_ids)))
        self.assertEqual(len(entry_ids), len(set(entry_ids)))
        self.assertGreaterEqual(len(symbols), 27)
        self.assertGreaterEqual(len(module_symbols), 13)
        self.assertGreaterEqual(len(entries), 7)
        self.assertGreaterEqual(len(self.module_paths), 20)

        self.assertEqual(
            {"id", "pattern", "expected_occurrences", "kind", "replacement"},
            set(symbols[0]),
        )
        for row in symbols:
            with self.subTest(symbol=row["id"]):
                self.assertEqual(
                    {
                        "id",
                        "pattern",
                        "expected_occurrences",
                        "kind",
                        "replacement",
                    },
                    set(row),
                )
                self.assertGreater(row["expected_occurrences"], 0)
                self.assertTrue(row["replacement"].strip())

        for row in module_symbols:
            with self.subTest(module_symbol=row["id"]):
                self.assertEqual(
                    {"id", "pattern", "expected_occurrences"}, set(row)
                )
                self.assertGreaterEqual(row["expected_occurrences"], 0)

        for row in entries:
            with self.subTest(entrypoint=row["id"]):
                self.assertEqual(
                    {"id", "anchor", "owns", "replacement"}, set(row)
                )
                self.assertTrue(row["owns"])
                self.assertTrue(row["replacement"].strip())

    def test_every_tracked_symbol_has_the_reviewed_production_count(self) -> None:
        for row in self.catalog["symbols"]:
            with self.subTest(symbol=row["id"]):
                try:
                    pattern = re.compile(row["pattern"])
                except re.error as error:
                    self.fail(f"invalid pattern for {row['id']}: {error}")
                actual = len(pattern.findall(self.production_source))
                self.assertEqual(
                    actual,
                    row["expected_occurrences"],
                    f"{row['id']} changed in {self.catalog['source']}; update the "
                    "source and ownership ledger together",
                )

    def test_module_wide_legacy_and_deadline_sentinels_have_reviewed_counts(self) -> None:
        for row in self.catalog["module_symbols"]:
            with self.subTest(module_symbol=row["id"]):
                try:
                    pattern = re.compile(row["pattern"])
                except re.error as error:
                    self.fail(f"invalid module pattern for {row['id']}: {error}")
                actual = len(pattern.findall(self.module_source))
                self.assertEqual(
                    actual,
                    row["expected_occurrences"],
                    f"{row['id']} changed anywhere under {self.catalog['module_root']}; "
                    "update the module-wide owner allowlist in the same reviewed change",
                )

    def test_every_named_entrypoint_is_live_and_unique(self) -> None:
        for row in self.catalog["entrypoints"]:
            with self.subTest(entrypoint=row["id"]):
                self.assertEqual(
                    self.production_source.count(row["anchor"]),
                    1,
                    f"{row['id']} lost or duplicated {row['anchor']!r}",
                )


if __name__ == "__main__":
    unittest.main()
