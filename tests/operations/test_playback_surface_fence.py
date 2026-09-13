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
import re
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
        #
        # 37 -> 39 with M5: the review found the hole every property-name
        # pattern has by construction — a computed member write never spells
        # the property — so the fence closed the DOOR instead, and the fixture
        # gained the three lookups that get hold of a surface element.
        expected = {"web": 39, "swift": 17, "kotlin": 19, "kotlin_owner": 12}
        names = {
            "web": "must_trip.web.html",
            "swift": "must_trip.swift",
            "kotlin": "must_trip.kt",
            "kotlin_owner": "must_trip.owner.kt",
        }
        for kind, name in names.items():
            with self.subTest(kind=kind):
                hits = self.fence.scan_text(kind, (FIXTURES / name).read_text(encoding="utf-8"))
                self.assertEqual(
                    len(hits),
                    expected[kind],
                    f"{name}: expected {expected[kind]} write sites, found {hits}",
                )

    def test_must_not_trip_fixtures_are_silent(self):
        names = {
            "web": "must_not_trip.web.html",
            "swift": "must_not_trip.swift",
            "kotlin": "must_not_trip.kt",
            "kotlin_owner": "must_not_trip.owner.kt",
        }
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

    # One must-trip and one must-not-trip file per scanned kind. A kind with no
    # fixture is a kind nobody proved, which is a gate rather than a fence.
    FIXTURE_NAMES = {
        "web": ("must_trip.web.html", "must_not_trip.web.html"),
        "swift": ("must_trip.swift", "must_not_trip.swift"),
        "kotlin": ("must_trip.kt", "must_not_trip.kt"),
        "kotlin_owner": ("must_trip.owner.kt", "must_not_trip.owner.kt"),
    }

    def test_every_scanned_kind_has_both_fixtures(self):
        for kind in sorted(set(self.fence.SCANNED.values())):
            with self.subTest(kind=kind):
                self.assertIn(kind, self.fence.PATTERNS, f"{kind} is scanned with no patterns")
                self.assertIn(kind, self.FIXTURE_NAMES, f"{kind} has no fixture pair")
                for name in self.FIXTURE_NAMES[kind]:
                    self.assertTrue((FIXTURES / name).is_file(), f"{name} is missing")

    def test_a_blocking_raise_without_the_owners_stop_is_named(self):
        # `PlaybackSurfaceOwner.kt`'s doc comment says every blocking site does
        # its own stop first. Until M5 nothing checked it: the generic-raise
        # pattern required a `surfaceOwner.` receiver, which never appears
        # inside that file, so a bare `raiseBlocking(...)` with no stop passed.
        trips = self.fence.owner_stop_failures(
            (FIXTURES / "must_trip.owner_stop.kt").read_text(encoding="utf-8")
        )
        self.assertEqual(
            [number for number, _token in trips],
            [8, 18, 24],
            f"expected the three unstopped raises, found {trips}",
        )
        self.assertEqual(
            self.fence.owner_stop_failures(
                (FIXTURES / "must_not_trip.owner.kt").read_text(encoding="utf-8")
            ),
            [],
            "a blocking site that does stop first is not a failure",
        )

    def test_a_presenter_with_a_side_effect_is_named(self):
        # Contract v2's whole thesis (§3.0): the presenter owns the pixels and
        # nothing else. `PRESENTER_FILES` exempted those files from the write
        # patterns wholesale, which left the thesis itself unfenced — a free
        # function calling `player.pause()` inside the presenter passed.
        trips = self.fence.presenter_failures(
            (FIXTURES / "must_trip.presenter.swift").read_text(encoding="utf-8")
        )
        self.assertEqual(
            sorted({token for _number, token in trips}),
            ["control-plane call", "player call", "timer creation"],
            f"every forbidden kind must be named, found {trips}",
        )
        self.assertEqual(len(trips), 11, f"one hit per offending line, found {trips}")
        self.assertEqual(
            self.fence.presenter_failures(
                (FIXTURES / "must_not_trip.presenter.swift").read_text(encoding="utf-8")
            ),
            [],
            "a presenter that NAMES the verbs it refuses to perform is not performing them",
        )

    def test_the_shipped_presenters_have_no_side_effects(self):
        for relative in self.fence.PRESENTER_FILES:
            path = self.fence.ROOT / relative
            if not path.is_file():
                continue
            with self.subTest(path=relative):
                self.assertEqual(
                    self.fence.presenter_failures(path.read_text(encoding="utf-8")),
                    [],
                    f"{relative} moves the player, arms a timer, or reports to the control plane",
                )

    def test_the_fence_passes_the_repository_as_it_stands(self):
        failures = self.fence.scan()
        self.assertEqual(failures, [], "\n".join(failures))

    def test_no_budget_is_unscanned_and_every_budget_states_its_reason(self):
        # A milestone that lands removes its file's entry entirely and the fence
        # holds it at zero from then on, so a scanned file with no entry is the
        # finished state rather than an oversight — `test_the_budget_is_tight_
        # against_the_tree` below is what proves it really is at zero, and M1's
        # `index.html` and M3's two Android rows both left that way. A budget
        # for a file nobody scans is still nonsense, and so is one with no
        # reason attached.
        for path in self.fence.MIGRATION_BUDGET:
            self.assertIn(path, self.fence.SCANNED, f"{path} is budgeted but never scanned")
            _budget, reason = self.fence.MIGRATION_BUDGET[path]
            self.assertTrue(reason.strip(), f"{path} is budgeted with no reason")

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

    def test_the_surface_itself_is_fenced_not_only_the_fields_it_replaced(self):
        # A fence that guards only deleted names guards nothing. Each spelling
        # below got a surface write past the first M2 fence during review, so
        # each one is asserted individually rather than trusted to the fixture
        # count — a count can be satisfied by seventeen copies of one pattern.
        bypasses = [
            "        surface = PlaybackSurfaceModel()",
            "        self.surface.apply(.raise(fault, context: .attached), now: .now)",
            "        self[keyPath: \\.surface] = PlaybackSurfaceModel()",
            "        _surface = Published(initialValue: PlaybackSurfaceModel())",
            '        setValue(nil, forKey: "surfaceHistory")',
            "        surfaceHistory = PlaybackSurfaceHistory()",
            "        surfaceHistory.record(entry, atMs: 0, player: snapshot)",
        ]
        for line in bypasses:
            with self.subTest(line=line.strip()):
                self.assertEqual(
                    len(self.fence.scan_text("swift", line)), 1,
                    f"the fence let this surface write through: {line.strip()}",
                )

    def test_reading_the_surface_is_not_writing_it(self):
        # The contract says the fence says nothing about reads, and the view,
        # the ledger and every recovery ladder depend on that.
        reads = [
            "        let surface = controller.surface.surface",
            "        let history = controller.surfaceHistory.ledgerSummary",
            "        if surface.kind == .blocking { return history }",
            "    @Published private(set) var surface = PlaybackSurfaceModel()",
            "    private(set) var surfaceHistory = PlaybackSurfaceHistory()",
            "        if item.status == .failed { return }",
        ]
        for line in reads:
            with self.subTest(line=line.strip()):
                self.assertEqual(
                    self.fence.scan_text("swift", line), [],
                    f"the fence tripped on a read: {line.strip()}",
                )

    def test_the_apple_publish_anchors_may_not_simply_be_deleted(self):
        # M2 has landed, so its region is `required`: removing the two anchors
        # used to exempt the whole file silently, which is the one answer a
        # fence must never give by accident.
        path = Path("clients/apple/Sources/PlayerController.swift")
        begin, end, _reason, required = self.fence.REGIONS[path][0]
        self.assertTrue(required, "a landed milestone's region must be required")
        with self.assertRaises(ValueError):
            self.fence.region_allowed_lines(path, ["nothing();"])

    def test_the_player_controllers_published_surface_inventory_is_pinned(self):
        # The one bypass no name-based pattern can catch is a BRAND-NEW
        # published field used as a second surface. It cannot be caught by a
        # regex, so it is caught by inventory: adding one is a visible diff
        # here, and the reviewer's question is "why does the player publish two
        # surfaces?".
        source = (ROOT / "clients/apple/Sources/PlayerController.swift").read_text(encoding="utf-8")
        published = sorted(set(re.findall(r"@Published\s+(?:private\(set\)\s+)?var\s+(\w+)", source)))
        self.assertEqual(published, [
            "currentMs", "decision", "deliveredDolbyVisionProfile", "deliveredRange",
            "encoder", "finished", "isChangingStream", "isPlaying", "isVOD",
            "knownDurationMs", "lastTTFFMs", "pgsOverlayStatus", "pgsOverlayWindow",
            "playbackControlSummary", "preparedFallbackInterruptionMs",
            "selectedAudio", "selectedHeight", "selectedQualityIsOriginal",
            "selectedSubtitle", "sessionStatus", "surface",
        ])

    def test_the_android_player_carries_no_pre_contract_surface_write(self):
        # M3's own ratchet: the two Android files the contract migrated have no
        # budget left to spend, and the owner's surface arm is scanned too, so
        # a reintroduced `onError`/`playFailure`/`playbackNotice` fails here
        # rather than at review.
        android = [path for path, kind in self.fence.SCANNED.items() if kind == "kotlin"]
        self.assertTrue(android, "no Kotlin file is scanned at all")
        for path in android:
            self.assertNotIn(
                path, self.fence.MIGRATION_BUDGET,
                f"{path} still has a migration budget after M3",
            )
            source = self.fence.ROOT / path
            self.assertTrue(source.is_file(), f"{path} is scanned but missing")
            lines = source.read_text(encoding="utf-8").splitlines()
            allowed = self.fence.region_allowed_lines(path, lines)
            hits = self.fence.scan_text("kotlin", "\n".join(lines), allowed)
            self.assertEqual(hits, [], f"{path}: {hits}")

    def test_the_android_publish_region_wraps_the_single_surface_write(self):
        # The anchors are not decoration: they are where the fence would allow a
        # write, and M3 is required to have exactly one region in Controller.kt.
        path = self.fence.ROOT.joinpath(
            "clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt",
        )
        lines = path.read_text(encoding="utf-8").splitlines()
        allowed = self.fence.region_allowed_lines(
            self.fence.Path("clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"),
            lines,
        )
        self.assertEqual(len(allowed), 1, "the publish region should wrap exactly one line")
        self.assertIn("_surface.value", lines[sorted(allowed)[0] - 1])

    def test_the_kotlin_arm_catches_the_six_bypasses_of_the_deleted_channel(self):
        # M3 deleted `onError`, `playFailure` and `playbackNotice`, so the three
        # patterns that used to be the whole Kotlin arm now guard nothing that
        # exists — the arm was a no-op and six rewrites of the surface walked
        # through it. Each is named here so removing a pattern fails a test that
        # says which bypass it reopened.
        text = (FIXTURES / "must_trip.kt").read_text(encoding="utf-8")
        tokens = {token for _number, token in self.fence.scan_text("kotlin", text)}
        for token in (
            "surface flow write",        # `_surface.value =` outside the anchors,
                                         # and tryEmit / emit / update
            "surface constructed",       # PlaybackSurface.Blocking(...) outside the presenter
            "fault constructed",         # a PlaybackFault built by hand
            "generic surface raise",     # a site reaching past its named method,
                                         # including the spelling without the stop
            "screen-held failure string",  # the imperative channel under a new name
        ):
            self.assertIn(token, tokens, f"the Kotlin arm no longer catches: {token}")

    def test_the_android_publish_region_is_required(self):
        # Deleting both anchors used to open `Controller.kt` silently, because
        # a missing region is not an error while a milestone is still ahead.
        # M3 has landed, so it is an error now.
        path = self.fence.Path(
            "clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt",
        )
        _begin, _end, _reason, required = self.fence.REGIONS[path][0]
        self.assertTrue(required, "the Android publish region must be required after M3")
        with self.assertRaises(ValueError):
            self.fence.region_allowed_lines(path, ["_surface.value = surface"])

    def test_out_of_scope_files_are_named_not_forgotten(self):
        for path in self.fence.NEVER_SCANNED:
            self.assertNotIn(path, self.fence.SCANNED)
            self.assertTrue(self.fence.NEVER_SCANNED[path].strip(), f"{path} is excluded with no reason")


if __name__ == "__main__":
    unittest.main()
