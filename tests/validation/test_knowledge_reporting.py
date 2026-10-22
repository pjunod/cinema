"""Synthetic, source-free coverage for numeric study reporting."""
from types import SimpleNamespace

import unittest

from validation.knowledge.evaluation_reporting import DocumentScore, documentation_analysis, token_summary


def make_batch(*, missing_usage=(), failed=()):
    planned = []
    runs = []
    for case_index in range(12):
        case = f"case-{case_index:02d}"
        cohort = f"cohort-{case_index // 3}"
        for arm in ("A", "B", "C"):
            for repeat in (1, 2, 3):
                identity = f"{case}:{arm}:{repeat}"
                spec = SimpleNamespace(id=identity, case_id=case, cohort=cohort, arm=arm,
                                       repeat=repeat, case_type="investigation")
                planned.append(spec)
                runs.append(SimpleNamespace(
                    spec=spec,
                    status="failed" if identity in failed else "completed",
                    total_tokens=None if identity in missing_usage else 100 + case_index,
                    answer="private answer that must not appear",
                    tools=("private tool output",),
                    edits=("private source edit",),
                    error="private provider error",
                ))
    return SimpleNamespace(planned=tuple(planned), runs=tuple(runs))


def paired_scores(planned, expected_by_case, *, disagree_run=None, omit=()):
    scores = []
    for spec in planned:
        if spec.id in omit:
            continue
        expected = expected_by_case[spec.case_id]
        found = min(1, expected)
        digest = f"{1:064x}"
        scores.append(DocumentScore("rater-a", spec.id, expected, found, 0, digest))
        other = f"{2:064x}" if spec.id == disagree_run else digest
        scores.append(DocumentScore("rater-b", spec.id, expected, found, 0, other))
    return scores


class NumericReportingTests(unittest.TestCase):
    def test_empty_denominator_is_neither_recall_nor_success(self):

        observed = make_batch()
        expected = {spec.case_id: 0 for spec in observed.planned}
        scores = paired_scores(observed.planned, expected)

        report = documentation_analysis(observed, scores, expected, bootstrap_samples=10)

        first_case = report["per_case"]["case-00"]["arms"]["A"]
        assert first_case["recall"] is None
        assert first_case["complete_doc_success"] is None
        assert report["per_arm"]["A"]["micro_recall"] is None
        assert report["per_arm"]["A"]["zero_denominator_cases"] == sorted(expected)


    def test_equal_counts_with_different_judgment_digests_disagree(self):
        observed = make_batch()
        expected = {spec.case_id: 2 for spec in observed.planned}
        run_id = observed.planned[0].id

        report = documentation_analysis(
            observed, paired_scores(observed.planned, expected, disagree_run=run_id), expected,
            bootstrap_samples=10,
        )

        assert report["disagreements"] == [run_id]
        assert report["per_case"]["case-00"]["arms"]["A"]["scored_runs"] == 2
        assert report["comparisons"]["A-B"]["paired_difference_interval"] is None
        assert report["comparisons"]["A-B"]["point_estimate_coverage"]["complete"] is False


    def test_partial_scores_and_failed_runs_stay_visible_without_interval(self):
        failed = "case-00:A:1"
        missing = "case-00:A:2"
        observed = make_batch(failed=(failed,))
        expected = {spec.case_id: 1 for spec in observed.planned}

        report = documentation_analysis(
            observed, paired_scores(observed.planned, expected, omit=(missing,)), expected,
            bootstrap_samples=10,
        )

        assert report["run_statuses"][failed] == "failed"
        assert missing in report["unscored_runs"]
        assert report["comparisons"]["A-B"]["paired_difference_interval"] is None
        serialized = repr(report)
        assert "private answer" not in serialized
        assert "private tool output" not in serialized
        assert "private source edit" not in serialized
        assert "private provider error" not in serialized


    def test_unknown_usage_produces_none_total_and_reports_failed_status(self):
        identity = "case-00:A:1"
        observed = make_batch(missing_usage=(identity,), failed=(identity,))

        report = token_summary(observed)

        arm = report["per_arm"]["A"]
        assert arm["total_tokens"] is None
        assert arm["known_usage_runs"] == 35
        assert arm["missing_usage_runs"] == 1
        assert arm["status_counts"]["failed"] == 1
        assert report["comparisons"]["A-B"]["label"].startswith("descriptive")
        assert "private answer" not in repr(report)


    def test_score_rejects_invalid_counts_and_digest(self):
        with self.assertRaises(ValueError):
            DocumentScore("rater", "run", 1, 2, 0, f"{1:064x}")
        with self.assertRaises(ValueError):
            DocumentScore("rater", "run", 1, 1, 0, "not-a-digest")

    def test_empty_cases_do_not_disable_nonempty_case_uncertainty(self):
        observed = make_batch()
        expected = {spec.case_id: (0 if spec.case_id == 'case-00' else 1)
                    for spec in observed.planned}
        report = documentation_analysis(observed, paired_scores(observed.planned, expected),
                                        expected, bootstrap_samples=10)
        comparison = report['comparisons']['A-B']
        self.assertEqual(comparison['paired_difference_interval'], (0, 0))
        self.assertEqual(comparison['point_estimate_coverage']['eligible_nonempty_cases'], 11)
        self.assertTrue(comparison['point_estimate_coverage']['complete'])

    def test_duplicate_log_refuses_instead_of_overwriting(self):
        observed = make_batch()
        observed.runs += (observed.runs[0],)
        with self.assertRaises(ValueError):
            token_summary(observed)
