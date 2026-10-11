"""Bounded reuse of immutable controller scans across scope mutations."""

import unittest
from unittest.mock import patch

from validation import attempt_census


class AttemptCensusCacheCase(unittest.TestCase):
    def test_analysis_reuses_equal_text_invalidates_edits_and_bounds_retention(self):
        attempt_census._cached_controller_analysis.cache_clear()
        self.addCleanup(attempt_census._cached_controller_analysis.cache_clear)
        source = "func current() { openGeneration == generation }"
        edited = source.replace("openGeneration", "viewerActionEpoch")
        with patch.object(attempt_census, "census", wraps=attempt_census.census) as scan:
            original = attempt_census._controller_analysis(source)
            self.assertEqual(attempt_census._controller_analysis(source), original)
            self.assertEqual(scan.call_count, 1)
            changed = attempt_census._controller_analysis(edited)
            self.assertNotEqual(changed[0], original[0])
            self.assertEqual(scan.call_count, 2)
            self.assertEqual(attempt_census._cached_controller_analysis.cache_info().currsize, 1)
            self.assertEqual(attempt_census._controller_analysis(source), original)
            self.assertEqual(scan.call_count, 3)

        # UTF-8 bytes, rather than characters, bound the retained source.
        oversized = "é" * (512 * 1024 + 1)
        with patch.object(attempt_census, "census", return_value=()) as scan, \
                patch.object(attempt_census, "fence_calls", return_value=()), \
                patch.object(attempt_census, "still_current_calls", return_value=()):
            self.assertEqual(attempt_census._controller_analysis(oversized), ((), (), ()))
            self.assertEqual(attempt_census._controller_analysis(oversized), ((), (), ()))
            self.assertEqual(scan.call_count, 2)
            self.assertEqual(attempt_census._cached_controller_analysis.cache_info().currsize, 1)
