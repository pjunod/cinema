from __future__ import annotations

import unittest
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DESIGN = ROOT / "docs/cluster/CLOCK-SKEW-GUARD-DESIGN.md"

LOCAL_DISCONTINUITY_TOLERANCE_MS = 250
OBSERVATION_MAX_AGE_MS = 25_000
RTT_QUANTIZATION_FLOOR_US = 1_000


def four_timestamp_sample(t1: int, t2: int, t3: int, t4: int) -> tuple[int, int, int]:
    service_ms = t3 - t2
    round_trip_ms = (t4 - t1) - service_ms
    if service_ms < 0 or round_trip_ms < 0 or round_trip_ms > 2_000:
        raise ValueError("unusable sample")
    offset_us = ((t2 - t1) + (t3 - t4)) * 500
    uncertainty_us = round_trip_ms * 500 + 1_000
    return offset_us, uncertainty_us, round_trip_ms * 1_000


@dataclass(frozen=True)
class DelaySample:
    round_trip_us: int
    observed_at_ms: int


class DelayFilter:
    def __init__(self) -> None:
        self.samples: list[DelaySample] = []

    def offer(self, candidate: DelaySample) -> bool:
        self.samples = [
            sample
            for sample in self.samples
            if candidate.observed_at_ms - sample.observed_at_ms
            <= OBSERVATION_MAX_AGE_MS
        ]
        if self.samples:
            minimum = min(
                max(sample.round_trip_us, RTT_QUANTIZATION_FLOOR_US)
                for sample in self.samples
            )
            candidate_delay = max(
                candidate.round_trip_us, RTT_QUANTIZATION_FLOOR_US
            )
            if candidate_delay > 4 * minimum:
                return False
        self.samples.append(candidate)
        self.samples = self.samples[-8:]
        return True


class ContinuityGuard:
    def __init__(self, wall_ms: int, monotonic_ms: int) -> None:
        self.anchor_wall_ms = wall_ms
        self.anchor_monotonic_ms = monotonic_ms
        self.clock_generation = 0
        self.state_generation = 0

    def observe(self, wall_ms: int, monotonic_ms: int) -> bool:
        expected_wall_ms = self.anchor_wall_ms + (
            monotonic_ms - self.anchor_monotonic_ms
        )
        if abs(wall_ms - expected_wall_ms) <= LOCAL_DISCONTINUITY_TOLERANCE_MS:
            return True
        self.clock_generation += 1
        self.state_generation += 1
        self.anchor_wall_ms = wall_ms
        self.anchor_monotonic_ms = monotonic_ms
        return False

    def ticket(self, wall_ms: int, monotonic_ms: int) -> tuple[int, int]:
        if not self.observe(wall_ms, monotonic_ms):
            raise ValueError("local clock discontinuity")
        return self.clock_generation, self.state_generation

    def validates(self, ticket: tuple[int, int], wall_ms: int, monotonic_ms: int) -> bool:
        return self.observe(wall_ms, monotonic_ms) and ticket == (
            self.clock_generation,
            self.state_generation,
        )


def removal_allowed(
    *,
    local_continuity: bool,
    survivors_bounded: bool,
    fence_exists: bool,
    target_applied_or_authoritatively_unreachable: bool,
    stable_reachability_generation: bool,
) -> bool:
    return all(
        (
            local_continuity,
            survivors_bounded,
            fence_exists,
            target_applied_or_authoritatively_unreachable,
            stable_reachability_generation,
        )
    )


class ClockSkewGuardDesignCase(unittest.TestCase):
    def test_four_timestamp_and_zero_to_one_ms_fixture(self):
        self.assertEqual(four_timestamp_sample(1000, 1011, 1013, 1024), (0, 12_000, 22_000))
        self.assertEqual(
            four_timestamp_sample(2000, 2000, 2000, 2001),
            (-500, 1_500, 1_000),
        )

        window = DelayFilter()
        self.assertTrue(window.offer(DelaySample(0, 0)))
        self.assertTrue(window.offer(DelaySample(1_000, 10_000)))

    def test_rejected_minimum_expires_by_time_not_accepted_count(self):
        window = DelayFilter()
        self.assertTrue(window.offer(DelaySample(0, 0)))
        self.assertFalse(window.offer(DelaySample(5_000, 10_000)))
        self.assertFalse(window.offer(DelaySample(5_000, 20_000)))
        self.assertTrue(window.offer(DelaySample(5_000, 26_000)))
        self.assertEqual(window.samples, [DelaySample(5_000, 26_000)])

    def test_local_and_common_mode_steps_invalidate_decision_generation(self):
        guard = ContinuityGuard(1_000, 1_000)
        takeover = guard.ticket(2_000, 2_000)
        self.assertFalse(guard.validates(takeover, 5_001, 3_000))

        probe = guard.ticket(5_101, 3_100)
        self.assertFalse(guard.validates(probe, 7_202, 4_200))

        membership = guard.ticket(7_302, 4_300)
        self.assertFalse(guard.validates(membership, 9_403, 5_400))

        # Equal peer wall-clock jumps leave relative offsets at zero. The local
        # wall/monotonic invariant still changes the generation.
        common_mode = guard.ticket(9_503, 5_500)
        peer_offset_ms = (12_504 - 12_504)
        self.assertEqual(peer_offset_ms, 0)
        self.assertFalse(guard.validates(common_mode, 12_504, 6_500))

    def test_target_removal_excludes_only_the_fenced_target(self):
        safe = dict(
            local_continuity=True,
            survivors_bounded=True,
            fence_exists=True,
            target_applied_or_authoritatively_unreachable=True,
            stable_reachability_generation=True,
        )
        self.assertTrue(removal_allowed(**safe))
        for failed_rule in safe:
            case = safe | {failed_rule: False}
            with self.subTest(failed_rule=failed_rule):
                self.assertFalse(removal_allowed(**case))

    def test_document_keeps_the_executable_contract(self):
        body = DESIGN.read_text(encoding="utf-8")
        for required in (
            "CLOCK_RTT_QUANTIZATION_FLOOR_US = 1_000",
            "CLOCK_OBSERVATION_MAX_AGE = 25 s",
            "CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS = 250",
            "ClockDecisionTicket { clock_generation,",
            "Remove the fenced target",
            "unreachable_learner_removal_preserves_fence",
            "lost_follower_removal_preserves_quorum",
        ):
            self.assertIn(required, body)


if __name__ == "__main__":
    unittest.main()
