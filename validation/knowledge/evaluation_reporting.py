"""Gold-free numeric summaries for independently adjudicated document scores."""
from __future__ import annotations

import random
import re
import statistics
from collections import Counter, defaultdict
from dataclasses import dataclass
from typing import Mapping, Sequence

from .evaluation import BatchLog


ARMS = ("A", "B", "C")
REPEATS = (1, 2, 3)
CASE_COUNT = 12
RUN_COUNT = CASE_COUNT * len(ARMS) * len(REPEATS)


@dataclass(frozen=True)
class DocumentScore:
    scorer_id: str
    run_id: str
    expected_count: int
    found_count: int
    unsupported_claims: int
    judgment_digest: str | None

    def __post_init__(self) -> None:
        if not isinstance(self.scorer_id, str) or not self.scorer_id or not isinstance(self.run_id, str) or not self.run_id:
            raise ValueError("scorer and run identities are required")
        for value in (self.expected_count, self.found_count, self.unsupported_claims):
            if type(value) is not int or value < 0:
                raise ValueError("document score counts must be nonnegative integers")
        if self.found_count > self.expected_count:
            raise ValueError("found count exceeds expected count")
        if self.judgment_digest is not None and (not isinstance(self.judgment_digest, str)
                or not re.fullmatch(r"[0-9a-f]{64}", self.judgment_digest)):
            raise ValueError("judgment digest must be a lowercase SHA-256 hex digest")


def _batch_layout(batch: BatchLog) -> tuple[dict[str, object], dict[str, object], dict[str, str]]:
    planned_list = batch.planned
    if len(planned_list) != RUN_COUNT:
        raise ValueError(f"expected {RUN_COUNT} planned runs")
    planned = {run.id: run for run in planned_list}
    if len(planned) != RUN_COUNT:
        raise ValueError("planned run identities must be unique")
    cases = {run.case_id for run in planned_list}
    if len(cases) != CASE_COUNT:
        raise ValueError(f"expected {CASE_COUNT} cases")
    cohort_by_case: dict[str, str] = {}
    cells: set[tuple[str, str, int]] = set()
    for run in planned_list:
        if run.arm not in ARMS or run.repeat not in REPEATS:
            raise ValueError("planned runs must use arms A/B/C and repeats 1/2/3")
        cell = (run.case_id, run.arm, run.repeat)
        if cell in cells:
            raise ValueError("duplicate case/arm/repeat cell")
        cells.add(cell)
        previous = cohort_by_case.setdefault(run.case_id, run.cohort)
        if previous != run.cohort:
            raise ValueError("case cohort changed between planned runs")
    if len(cells) != RUN_COUNT:
        raise ValueError("planned schedule is incomplete")
    runs_list = batch.runs
    runs = {run.spec.id: run for run in runs_list}
    if len(runs_list) != RUN_COUNT or len(runs) != RUN_COUNT or runs.keys() != planned.keys():
        raise ValueError("run logs must match every planned identity exactly once")
    if any(run.spec != planned[identity] for identity, run in runs.items()):
        raise ValueError("run log identity drift")
    return planned, runs, cohort_by_case


def _score_signature(score: DocumentScore) -> tuple[object, ...]:
    return (score.expected_count, score.found_count, score.unsupported_claims, score.judgment_digest)


def _percentile_interval(values: Sequence[float]) -> tuple[float, float] | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[int((len(ordered) - 1) * .025)], ordered[int((len(ordered) - 1) * .975)]


def documentation_analysis(
    batch: BatchLog,
    scores: Sequence[DocumentScore],
    expected_by_case: Mapping[str, int],
    bootstrap_samples: int = 2000,
    bootstrap_seed: int = 20261021,
) -> dict[str, object]:
    """Summarize numeric document judgments without reading or returning source text."""
    if type(bootstrap_samples) is not int or bootstrap_samples <= 0:
        raise ValueError("bootstrap_samples must be a positive integer")
    if type(bootstrap_seed) is not int:
        raise ValueError("bootstrap_seed must be an integer")
    planned, runs, cohort_by_case = _batch_layout(batch)
    cases = sorted(cohort_by_case)
    if set(expected_by_case) != set(cases):
        raise ValueError("sealed expected counts must cover exactly the planned cases")
    for value in expected_by_case.values():
        if type(value) is not int or value < 0:
            raise ValueError("sealed expected counts must be nonnegative integers")

    grouped: dict[str, list[DocumentScore]] = defaultdict(list)
    scorers: set[str] = set()
    for score in scores:
        if not isinstance(score, DocumentScore):
            raise ValueError("scores must be DocumentScore values")
        if score.run_id not in planned:
            raise ValueError("score references an unknown run")
        if score.expected_count != expected_by_case[planned[score.run_id].case_id]:
            raise ValueError("score expected count differs from sealed case count")
        grouped[score.run_id].append(score)
        scorers.add(score.scorer_id)
    if len(scorers) > 2:
        raise ValueError("at most two distinct scorers are allowed")

    consensus: dict[str, DocumentScore] = {}
    disagreements: list[str] = []
    unscored: list[str] = []
    for run_id in planned:
        pair = grouped.get(run_id, [])
        if len(pair) > 2 or len({score.scorer_id for score in pair}) != len(pair):
            raise ValueError("each run accepts at most two distinct scorers")
        if len(pair) != 2 or any(score.judgment_digest is None for score in pair):
            unscored.append(run_id)
        elif _score_signature(pair[0]) != _score_signature(pair[1]):
            disagreements.append(run_id)
        else:
            consensus[run_id] = pair[0]

    per_case: dict[str, dict[str, object]] = {}
    case_recall: dict[str, dict[str, float | None]] = {arm: {} for arm in ARMS}
    arm_counts: dict[str, dict[str, int]] = {
        arm: {"expected_document_units": 0, "scored_expected_units": 0, "found_document_units": 0,
              "unsupported_claims": 0, "planned_runs": 0, "completed_runs": 0, "scored_runs": 0}
        for arm in ARMS
    }
    for case in cases:
        per_case[case] = {"cohort": cohort_by_case[case], "arms": {}}
        expected = expected_by_case[case]
        for arm in ARMS:
            selected = sorted((run for run in planned.values()
                               if run.case_id == case and run.arm == arm), key=lambda run: run.repeat)
            completed = sum(runs[run.id].status == "completed" for run in selected)
            judgments = [consensus[run.id] for run in selected if run.id in consensus]
            scored_expected = sum(score.expected_count for score in judgments)
            found = sum(score.found_count for score in judgments)
            unsupported = sum(score.unsupported_claims for score in judgments)
            recall = found / scored_expected if scored_expected else None
            nonempty = [score for score in judgments if score.expected_count > 0]
            successes = sum(score.found_count == score.expected_count and score.unsupported_claims == 0
                            for score in nonempty)
            success = successes / len(nonempty) if nonempty else None
            arm_counts[arm]["expected_document_units"] += expected * len(selected)
            arm_counts[arm]["scored_expected_units"] += scored_expected
            arm_counts[arm]["found_document_units"] += found
            arm_counts[arm]["unsupported_claims"] += unsupported
            arm_counts[arm]["planned_runs"] += len(selected)
            arm_counts[arm]["completed_runs"] += completed
            arm_counts[arm]["scored_runs"] += len(judgments)
            case_recall[arm][case] = recall
            per_case[case]["arms"][arm] = {
                "planned_runs": len(selected), "completed_runs": completed, "scored_runs": len(judgments),
                "expected_document_units": expected * len(selected),
                "scored_expected_units": scored_expected, "found_document_units": found,
                "unsupported_claims": unsupported, "recall": recall,
                "nonempty_denominator_runs": sum(expected > 0 for _ in selected),
                "scored_nonempty_denominator_runs": len(nonempty),
                "zero_denominator_runs": sum(expected == 0 for _ in selected),
                "zero_denominator_case": expected == 0,
                "complete_doc_success": None if expected == 0 else {
                    "successes": successes, "scored_nonempty_runs": len(nonempty), "rate": success,
                },
            }

    arm_report: dict[str, dict[str, object]] = {}
    for arm in ARMS:
        counts = arm_counts[arm]
        valid = [value for value in case_recall[arm].values() if value is not None]
        denom = counts["scored_expected_units"]
        arm_report[arm] = {
            **counts,
            "micro_recall": counts["found_document_units"] / denom if denom else None,
            "macro_case_recall": statistics.mean(valid) if valid else None,
            "macro_case_coverage": {"cases_with_recall": len(valid), "planned_cases": CASE_COUNT},
            "nonempty_denominator_cases": [case for case in cases if expected_by_case[case] > 0],
            "zero_denominator_cases": [case for case in cases if expected_by_case[case] == 0],
        }

    all_completed = all(run.status == "completed" for run in runs.values())
    full_agreement = len(consensus) == RUN_COUNT
    full_evidence = all_completed and full_agreement
    eligible = [case for case in cases if expected_by_case[case] > 0]
    cohorts = sorted(set(cohort_by_case[case] for case in eligible))
    rng = random.Random(bootstrap_seed)
    comparisons: dict[str, dict[str, object]] = {}
    for before, after in (("A", "B"), ("B", "C")):
        paired = [case for case in cases
                  if case_recall[before][case] is not None and case_recall[after][case] is not None]
        differences = [case_recall[before][case] - case_recall[after][case] for case in paired]
        interval = None
        if full_evidence and paired and set(paired) == set(eligible):
            clusters = [[case for case in eligible if cohort_by_case[case] == cohort] for cohort in cohorts]
            samples = []
            for _ in range(bootstrap_samples):
                sample = [case for cluster in clusters for case in rng.choices(cluster, k=len(cluster))]
                samples.append(statistics.mean(case_recall[before][case] - case_recall[after][case]
                                               for case in sample))
            interval = _percentile_interval(samples)
        comparisons[f"{before}-{after}"] = {
            "before": before, "after": after,
            "paired_macro_case_recall_difference": statistics.mean(differences) if differences else None,
            "difference_direction": "before minus after recall; negative favors after",
            "point_estimate_coverage": {"paired_cases": len(paired), "planned_cases": CASE_COUNT,
                                         "eligible_nonempty_cases": len(eligible),
                                         "complete": bool(eligible) and set(paired) == set(eligible) and full_evidence},
            "paired_difference_interval": interval,
            "interval_method": "case-clustered cohort-stratified percentile bootstrap" if interval else None,
        }

    return {
        "planned_runs": RUN_COUNT,
        "completed_runs": sum(run.status == "completed" for run in runs.values()),
        "all_runs_completed": all_completed,
        "full_two_scorer_agreement": full_agreement,
        "unscored_runs": unscored,
        "disagreements": disagreements,
        "run_statuses": {run_id: runs[run_id].status for run_id in planned},
        "per_case": per_case,
        "per_arm": arm_report,
        "comparisons": comparisons,
        "uncertainty": {"samples": bootstrap_samples, "seed": bootstrap_seed},
    }


def token_summary(batch: BatchLog) -> dict[str, object]:
    """Report all-run usage descriptively; missing usage never becomes zero."""
    planned, runs, _ = _batch_layout(batch)
    per_arm: dict[str, dict[str, object]] = {}
    medians: dict[str, float | None] = {}
    for arm in ARMS:
        selected = [runs[run_id] for run_id, spec in planned.items() if spec.arm == arm]
        known = [run.total_tokens for run in selected if run.total_tokens is not None]
        missing = len(selected) - len(known)
        statuses = dict(sorted(Counter(run.status for run in selected).items()))
        total = sum(known) if not missing else None
        median = statistics.median(known) if known else None
        medians[arm] = median
        per_arm[arm] = {
            "planned_runs": len(selected), "known_usage_runs": len(known), "missing_usage_runs": missing,
            "status_counts": statuses, "known_usage_token_total": sum(known), "total_tokens": total,
            "median_tokens_known_runs": median,
            "mean_tokens_known_runs": statistics.mean(known) if known else None,
        }
    comparisons = {}
    for before, after in (("A", "B"), ("B", "C")):
        base, changed = medians[before], medians[after]
        comparisons[f"{before}-{after}"] = {
            "before_median": base, "after_median": changed,
            "relative_median_change": None if base in (None, 0) or changed is None else (changed - base) / base,
            "label": "descriptive all-run median usage; not a correctness comparison",
        }
    return {"per_arm": per_arm, "comparisons": comparisons}
