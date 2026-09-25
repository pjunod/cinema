"""The release-evidence metric set is documented where operators read it.

C-08 M5 (docs/server/OBSERVABILITY-BASELINE.md, section 3.5) adds the series a
release comparison reads. A family that renders on /metrics but has no row in
docs/OPERATIONS.md is a number nobody knows how to read, and a documented
family the server no longer renders is a query that silently returns nothing.
Both directions are checked here, plus the two reserved names that must stay
documented and must NOT yet be rendered.
"""

from __future__ import annotations

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
OPERATIONS = ROOT / "docs" / "OPERATIONS.md"
TELEMETRY = ROOT / "crates" / "plurxd" / "src" / "telemetry.rs"

RENDERED = [
    "plurx_ttff_ms",
    "plurx_seek_to_picture_ms",
    "plurx_seeks_total",
    "plurx_stalled_seconds_total",
    "plurx_watched_seconds_total",
    "plurx_delivered_bytes_total",
    "plurx_admission_wait_seconds",
]

# Names reserved by the plan so two efforts cannot invent two spellings. Each
# becomes a RENDERED entry in the PR that builds it.
RESERVED = [
    "plurx_scratch_bytes",
    "plurx_start_outcomes_total",
]


class ReleaseEvidenceMetricsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.operations = OPERATIONS.read_text(encoding="utf-8")
        self.telemetry = TELEMETRY.read_text(encoding="utf-8")
        start = self.operations.index("### Release evidence")
        end = self.operations.index("\n### ", start + 1)
        self.section = self.operations[start:end]

    def test_every_rendered_family_has_a_reading_row(self) -> None:
        for family in RENDERED:
            with self.subTest(family=family):
                self.assertTrue(
                    f'"{family}"' in self.telemetry or f"# TYPE {family} " in self.telemetry,
                    f"{family} is not rendered by telemetry.rs",
                )
                self.assertIn(f"| `{family}{{", self.section)

    def test_reserved_names_are_documented_and_not_yet_rendered(self) -> None:
        crates = ROOT / "crates"
        for name in RESERVED:
            with self.subTest(name=name):
                self.assertIn(f"`{name}{{", self.section)
                rendered = [
                    path
                    for path in crates.rglob("*.rs")
                    if any(
                        needle in path.read_text(encoding="utf-8", errors="ignore")
                        for needle in (f'"{name}"', f"# TYPE {name}")
                    )
                ]
                self.assertEqual(
                    rendered,
                    [],
                    f"{name} is rendered now: move it to RENDERED and give it a reading row",
                )

    def test_the_exit_counter_template_carries_every_field(self) -> None:
        for field in ("Event", "Series", "Denominator", "Interval", "Threshold", "Owner"):
            with self.subTest(field=field):
                self.assertIn(f"\n  {field} ", self.section)


if __name__ == "__main__":
    unittest.main()
