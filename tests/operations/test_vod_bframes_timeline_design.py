"""Executable design contract for a possible reordered encoded-VOD recipe.

This is deliberately independent of production code.  S-12 is a design
item: the production landing validator continues to reject every nonzero
composition offset until a later implementation replaces that refusal with
this presentation-grid proof.
"""

from __future__ import annotations

import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
CASES = ROOT / "tests/playback/vod-bframes-timeline-cases.json"
PLAN = ROOT / "docs/streaming/VOD-BFRAMES-TIMELINE-DESIGN.md"

U32_MAX = 2**32 - 1
U64_MAX = 2**64 - 1
I64_MIN = -(2**63)
I64_MAX = 2**63 - 1


def judge(case: dict[str, object]) -> str:
    """Return the first fail-closed contract result for one resolved traf."""

    start = int(case["entry_start"])
    length = int(case["entry_duration"])
    frame = int(case["frame_ticks"])
    track_timescale = int(case["track_timescale"])
    plan_timescale = int(case["plan_timescale"])
    tfdt = int(case["base_decode_time"])
    samples = list(case["samples"])

    if not bool(case["clean_random_access"]):
        return "random_access"
    if not (0 < track_timescale <= U32_MAX and 0 < plan_timescale <= U32_MAX):
        return "box_value"
    if track_timescale != plan_timescale:
        return "timescale"
    if bool(case["has_edit_list"]):
        return "edit_list"
    if not (0 <= start <= U64_MAX and 0 < length <= U64_MAX and 0 < frame <= U32_MAX):
        return "box_value"
    if not (0 <= tfdt <= U64_MAX):
        return "box_value"
    if tfdt != start:
        return "decode_start"
    if length % frame != 0:
        return "entry_divisibility"

    values: list[tuple[int, int]] = []
    dts = tfdt
    for sample in samples:
        duration = int(sample["duration"])
        cto = int(sample["cto"])
        if not (0 < duration <= U32_MAX):
            return "box_value"
        if duration != frame:
            return "frame_duration"
        if not I64_MIN <= cto <= I64_MAX:
            return "box_value"
        values.append((dts + cto, duration))
        dts += duration

    if len(samples) != length // frame:
        return "sample_count"
    if dts != start + length:
        return "decode_duration"

    presentation = [pts for pts, _ in values]
    if min(presentation, default=start) != start:
        return "presentation_start"
    if max((pts + duration for pts, duration in values), default=start) != start + length:
        return "presentation_end"
    expected = list(range(start, start + length, frame))
    if sorted(presentation) != expected:
        return "presentation_grid"
    return "ok"


def judge_candidate_wire_shape(case: dict[str, object]) -> str:
    """Check B0 producer evidence which current runtime types do not retain."""

    version = int(case["trun_version"])
    offsets = [int(sample["cto"]) for sample in case["samples"]]
    if version == 0:
        if any(not 0 <= offset <= U32_MAX for offset in offsets):
            return "box_value"
    elif version == 1:
        if any(not -(2**31) <= offset <= 2**31 - 1 for offset in offsets):
            return "box_value"
    else:
        return "box_value"
    if any(offset != 0 for offset in offsets) and version != 1:
        return "trun_version"
    return "ok"


class VodBframesTimelineDesignTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.document = json.loads(CASES.read_text(encoding="utf-8"))

    def test_schema_and_case_names_are_stable(self) -> None:
        self.assertEqual("plurx-vod-presentation-grid-v2", self.document["schema"])
        names = [case["name"] for case in self.document["cases"]]
        self.assertEqual(len(names), len(set(names)))
        self.assertIn("version-one-closed-gop-reorder", names)
        self.assertIn("leading-picture-precedes-entry", names)
        self.assertIn("duplicate-presentation-slot", names)
        self.assertIn("track-timescale-does-not-match-plan", names)
        self.assertIn("edit-list-shifts-presentation", names)

    def test_every_timeline_has_its_declared_result(self) -> None:
        for case in self.document["cases"]:
            with self.subTest(case=case["name"]):
                expected = case["expect"]
                observed = judge(case)
                self.assertEqual(expected["reason"], observed)
                self.assertEqual(expected["accepted"], observed == "ok")

    def test_valid_reorder_fails_closed_on_init_contract_changes(self) -> None:
        valid = next(
            case
            for case in self.document["cases"]
            if case["name"] == "version-one-closed-gop-reorder"
        )
        mismatched_clock = dict(valid, track_timescale=90_000)
        shifted_presentation = dict(valid, has_edit_list=True)
        self.assertEqual("timescale", judge(mismatched_clock))
        self.assertEqual("edit_list", judge(shifted_presentation))

    def test_wire_shape_is_b0_evidence_not_a_runtime_parser_claim(self) -> None:
        cases = {case["name"]: case for case in self.document["cases"]}
        self.assertEqual(
            "ok",
            judge_candidate_wire_shape(cases["version-one-closed-gop-reorder"]),
        )
        self.assertEqual(
            "trun_version",
            judge_candidate_wire_shape(
                cases["unsigned-offset-reorder-is-not-the-candidate"]
            ),
        )
        self.assertEqual(
            "box_value",
            judge_candidate_wire_shape(
                cases["composition-offset-outside-version-one-box-range"]
            ),
        )

    def test_plan_names_the_executable_oracle_and_keeps_refusal(self) -> None:
        plan = PLAN.read_text(encoding="utf-8")
        self.assertIn("tests/playback/vod-bframes-timeline-cases.json", plan)
        self.assertRegex(plan, r"Option A is the only\s+admissible reordered design")
        self.assertRegex(plan, r"current nonzero-CTO\s+refusal stays deployed")
        self.assertIn("current `Init` does not expose this fact", plan)
        self.assertIn("discard the raw `trun` version and presence flag", plan)
        self.assertIn("advisory-only", plan)


if __name__ == "__main__":
    unittest.main()
