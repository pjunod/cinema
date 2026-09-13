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
        # The web count went 16 -> 36 when M1 closed the spellings the review
        # found: the modern DOM replacements (`innerText`, `insertAdjacentHTML`,
        # `append`, `prepend`, `replaceChildren`, `replaceWith`), optional-call
        # and reflected `setLoading`, and every one of them again against the
        # two surfaces M1 added. An exact count, because a pattern that stops
        # matching is a hole, and so is a fixture line nobody notices is dead.
        expected = {"web": 37, "swift": 10, "kotlin": 8}
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

    def test_an_allowed_region_covers_its_body_and_not_its_anchors(self):
        # No render anchors exist in the tree until M1/M2/M3 land, so this is
        # the only coverage `region_allowed_lines` has. The anchors themselves
        # are OUTSIDE the region: `setLoading(false); // …:end` on the end
        # anchor would otherwise be exempt.
        path = next(iter(self.fence.REGIONS))
        begin, end, _reason, _required = self.fence.REGIONS[path][0]
        lines = [
            "before();",
            f"  {begin}",
            "  inside();",
            f"  {end}",
            "after();",
        ]
        self.assertEqual(self.fence.region_allowed_lines(path, lines), {3})

    def test_a_broken_region_fails_rather_than_silently_allowing_nothing(self):
        path = next(iter(self.fence.REGIONS))
        begin, end, _reason, _required = self.fence.REGIONS[path][0]
        with self.assertRaises(ValueError):
            self.fence.region_allowed_lines(path, [f"  {begin}", "  inside();"])
        with self.assertRaises(ValueError):
            self.fence.region_allowed_lines(path, [f"  {begin}", f"  {begin}", "  x();", f"  {end}"])
        # A region that has not been written yet is not an error: M1/M2/M3 add
        # the anchors with the render they wrap.
        self.assertEqual(self.fence.region_allowed_lines(path, ["nothing();"]), set())

    def test_a_write_inside_the_region_is_allowed_and_one_outside_is_not(self):
        path = next(iter(self.fence.REGIONS))
        begin, end, _reason, _required = self.fence.REGIONS[path][0]
        lines = [
            'setLoading(true, "outside", "", "");',
            f"  {begin}",
            '  setLoading(true, "inside", "", "");',
            f"  {end}",
        ]
        allowed = self.fence.region_allowed_lines(path, lines)
        hits = self.fence.scan_text("web", "\n".join(lines), allowed)
        self.assertEqual([number for number, _token in hits], [1])

    def test_a_write_split_over_two_lines_is_still_one_write(self):
        text = 'setLoading\n  (true, "x", "", "");\n'
        hits = self.fence.scan_text("web", text)
        self.assertEqual(hits, [(1, "setLoading")])

    def test_the_fence_passes_the_repository_as_it_stands(self):
        failures = self.fence.scan()
        self.assertEqual(failures, [], "\n".join(failures))

    def test_no_budget_is_unscanned(self):
        # A milestone that lands removes its file's entry entirely and the fence
        # holds it at zero from then on, so a scanned file with no entry is the
        # finished state rather than an oversight — `test_the_budget_is_tight_
        # against_the_tree` below is what proves it really is at zero. A budget
        # for a file nobody scans is still nonsense.
        for path in self.fence.MIGRATION_BUDGET:
            self.assertIn(path, self.fence.SCANNED, f"{path} is budgeted but never scanned")

    def test_a_migrated_file_is_budgeted_at_zero_without_an_entry(self):
        # M2's own ratchet: PlayerController no longer has a budget row, and
        # the fence must still hold it to zero rather than to "unbudgeted".
        player = Path("clients/apple/Sources/PlayerController.swift")
        self.assertIn(player, self.fence.SCANNED)
        self.assertNotIn(player, self.fence.MIGRATION_BUDGET)
        budget, _reason = self.fence.MIGRATION_BUDGET.get(player, (0, ""))
        self.assertEqual(budget, 0)

    def test_the_budget_is_tight_against_the_tree(self):
        # The fence itself is a ratchet (`len(hits) > budget`), so nothing can
        # be ADDED. This assertion is the other half: the budget must equal
        # what the file has, so a milestone that removes write sites is
        # required to lower it in the same commit rather than leaving slack for
        # the next one to hide in. A commit that legitimately removes a site
        # and is surprised by this failure has one number to change, and the
        # message says which.
        for path, kind in self.fence.SCANNED.items():
            source = self.fence.ROOT / path
            if not source.is_file():
                continue
            lines = source.read_text(encoding="utf-8").splitlines()
            allowed = self.fence.region_allowed_lines(path, lines)
            hits = self.fence.scan_text(kind, "\n".join(lines), allowed)
            budget, _reason = self.fence.MIGRATION_BUDGET.get(path, (0, ""))
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
