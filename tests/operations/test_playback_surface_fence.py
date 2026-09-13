"""The playback surface fence trips on writes and never on reads.

A fence nobody has proved is a fence with a gate in it. Each kind gets one
file that must trip on every line that writes a surface, and one that must not
trip at all — declarations, comparisons and raw-error reads included, because
the contract explicitly leaves reading alone (PLAYBACK-SURFACE-CONTRACT.md §4).
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests/operations/fence-fixtures/surface"


def load_fence():
    loader = importlib.machinery.SourceFileLoader("playback_surface_fence", str(ROOT / "scripts/playback-surface-fence"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class PlaybackSurfaceFenceTest(unittest.TestCase):
    def setUp(self):
        self.fence = load_fence()

    def test_must_trip_fixtures_trip_on_every_write(self):
        expected = {"web": 7, "swift": 5, "kotlin": 4}
        names = {"web": "must_trip.web.html", "swift": "must_trip.swift", "kotlin": "must_trip.kt"}
        for kind, name in names.items():
            with self.subTest(kind=kind):
                hits = self.fence.scan_text(kind, (FIXTURES / name).read_text(encoding="utf-8"))
                self.assertEqual(
                    len(hits),
                    expected[kind],
                    f"{name}: expected {expected[kind]} write sites, found {hits}",
                )

    def test_must_not_trip_fixtures_are_silent(self):
        names = {"web": "must_not_trip.web.html", "swift": "must_not_trip.swift", "kotlin": "must_not_trip.kt"}
        for kind, name in names.items():
            with self.subTest(kind=kind):
                hits = self.fence.scan_text(kind, (FIXTURES / name).read_text(encoding="utf-8"))
                self.assertEqual(hits, [], f"{name}: the fence tripped on a read")

    def test_an_allowed_region_silences_only_its_own_lines(self):
        text = (FIXTURES / "must_trip.web.html").read_text(encoding="utf-8")
        all_hits = self.fence.scan_text("web", text)
        allowed = {line for line, _token in all_hits}
        self.assertEqual(self.fence.scan_text("web", text, allowed), [])

    def test_the_fence_passes_the_repository_as_it_stands(self):
        failures = self.fence.scan()
        self.assertEqual(failures, [], "\n".join(failures))

    def test_every_scanned_file_has_a_budget_and_no_budget_is_unscanned(self):
        for path in self.fence.SCANNED:
            self.assertIn(path, self.fence.MIGRATION_BUDGET, f"{path} is scanned with no budget entry")
        for path in self.fence.MIGRATION_BUDGET:
            self.assertIn(path, self.fence.SCANNED, f"{path} is budgeted but never scanned")

    def test_the_budget_is_tight_against_the_tree(self):
        # A budget with slack is a place a new write site can hide. Each entry
        # must equal what the file actually has today.
        for path, kind in self.fence.SCANNED.items():
            source = self.fence.ROOT / path
            if not source.is_file():
                continue
            lines = source.read_text(encoding="utf-8").splitlines()
            allowed = self.fence.region_allowed_lines(path, lines)
            hits = self.fence.scan_text(kind, "\n".join(lines), allowed)
            budget, _reason = self.fence.MIGRATION_BUDGET[path]
            self.assertEqual(
                len(hits), budget,
                f"{path}: {len(hits)} write sites against a budget of {budget} — lower the budget in the same commit",
            )

    def test_out_of_scope_files_are_named_not_forgotten(self):
        for path in self.fence.NEVER_SCANNED:
            self.assertNotIn(path, self.fence.SCANNED)
            self.assertTrue(self.fence.NEVER_SCANNED[path].strip(), f"{path} is excluded with no reason")


if __name__ == "__main__":
    unittest.main()
