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

I32_MIN = -(2**31)
I32_MAX = 2**31 - 1
U32_MAX = 2**32 - 1
U64_MAX = 2**64 - 1


def judge(case: dict[str, object]) -> str:
    """Return the first fail-closed contract result for one resolved traf."""

    start = int(case["entry_start"])
    length = int(case["entry_duration"])
    frame = int(case["frame_ticks"])
    tfdt = int(case["base_decode_time"])
    trun_version = int(case["trun_version"])
    samples = list(case["samples"])

    if not bool(case["clean_random_access"]):
        return "random_access"
    if not (0 <= start <= U64_MAX and 0 < length <= U64_MAX and 0 < frame <= U32_MAX):
        return "box_value"
    if not (0 <= tfdt <= U64_MAX):
        return "box_value"
    if tfdt != start:
        return "decode_start"
    if length % frame != 0:
        return "entry_divisibility"

    values: list[tuple[int, int]] = []
    has_offset = False
    dts = tfdt
    for sample in samples:
        duration = int(sample["duration"])
        cto = int(sample["cto"])
        if not (0 < duration <= U32_MAX):
            return "box_value"
        if duration != frame:
            return "frame_duration"
        if trun_version == 1:
            if not I32_MIN <= cto <= I32_MAX:
                return "box_value"
        elif trun_version == 0:
            if not 0 <= cto <= U32_MAX:
                return "box_value"
        else:
            return "box_value"
        has_offset = has_offset or cto != 0
        values.append((dts + cto, duration))
        dts += duration

    # The selected reordered producer form is signed trun v1.  Zero-offset
    # output remains compatible with today's version-0 representation.
    if has_offset and trun_version != 1:
        return "trun_version"
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


class VodBframesTimelineDesignTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.document = json.loads(CASES.read_text(encoding="utf-8"))

    def test_schema_and_case_names_are_stable(self) -> None:
        self.assertEqual("plurx-vod-presentation-grid-v1", self.document["schema"])
        names = [case["name"] for case in self.document["cases"]]
        self.assertEqual(len(names), len(set(names)))
        self.assertIn("version-one-closed-gop-reorder", names)
        self.assertIn("leading-picture-precedes-entry", names)
        self.assertIn("duplicate-presentation-slot", names)

    def test_every_timeline_has_its_declared_result(self) -> None:
        for case in self.document["cases"]:
            with self.subTest(case=case["name"]):
                expected = case["expect"]
                observed = judge(case)
                self.assertEqual(expected["reason"], observed)
                self.assertEqual(expected["accepted"], observed == "ok")

    def test_plan_names_the_executable_oracle_and_keeps_refusal(self) -> None:
        plan = PLAN.read_text(encoding="utf-8")
        self.assertIn("tests/playback/vod-bframes-timeline-cases.json", plan)
        self.assertRegex(plan, r"Option A is the only\s+admissible reordered design")
        self.assertRegex(plan, r"current nonzero-CTO\s+refusal stays deployed")
        self.assertIn("advisory-only", plan)


if __name__ == "__main__":
    unittest.main()
