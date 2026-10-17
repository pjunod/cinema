use super::{
    analysis_queue_verdict, AnalysisQueueHealth, AnalysisQueueVerdict, ANALYSIS_DEAD_BACKLOG_FLOOR,
    ANALYSIS_DEAD_CLAIM_FLOOR, ANALYSIS_RATIO_CLAIM_FLOOR,
};

/// `ready`, `attempt_limit`, `claimed`, `claimable`, `running_past_lease`
/// — the whole store side of the verdict, in the order the runbook's
/// table reads them.
fn health(
    ready_24h: i64,
    attempt_limit_24h: i64,
    claimed_24h: i64,
    claimable: i64,
    running_past_lease: i64,
) -> AnalysisQueueHealth {
    AnalysisQueueHealth {
        ready_24h,
        attempt_limit_24h,
        claimed_24h,
        claimable,
        running_past_lease,
        last_ready_at_ms: 0,
    }
}

#[test]
fn a_queue_that_claims_and_never_finishes_is_dead() {
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 25, 0, 0), 25, 25, 0),
        AnalysisQueueVerdict::Dead
    );
    // The fleet numbers that named the outage.
    assert_eq!(
        analysis_queue_verdict(health(0, 84, 396, 1_118, 11), 412, 398, 3),
        AnalysisQueueVerdict::Dead
    );
}

#[test]
fn the_dead_claim_floor_is_the_boundary_it_says_it_is() {
    let just_under = ANALYSIS_DEAD_CLAIM_FLOOR - 1;
    assert_eq!(
        analysis_queue_verdict(health(0, 0, just_under, 0, 0), just_under, 0, 0),
        AnalysisQueueVerdict::Degraded
    );
    assert_eq!(
        analysis_queue_verdict(
            health(0, 0, ANALYSIS_DEAD_CLAIM_FLOOR, 0, 0),
            ANALYSIS_DEAD_CLAIM_FLOOR,
            0,
            0
        ),
        AnalysisQueueVerdict::Dead
    );
}

#[test]
fn a_backlog_nobody_claims_is_dead_rather_than_idle() {
    // The shape a claim-counting rule alone misses: the queue never got
    // as far as a claim, so every counter that counts claims reads zero
    // while the work sits in front of it. Calling this `idle` is exactly
    // the silence this verdict exists to break.
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 0, ANALYSIS_DEAD_BACKLOG_FLOOR, 0), 0, 0, 0),
        AnalysisQueueVerdict::Dead
    );
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 0, ANALYSIS_DEAD_BACKLOG_FLOOR - 1, 0), 0, 0, 0),
        AnalysisQueueVerdict::Degraded
    );
}

#[test]
fn a_queue_that_finishes_most_of_what_it_claims_is_healthy() {
    assert_eq!(
        analysis_queue_verdict(health(20, 0, 22, 4, 0), 25, 2, 0),
        AnalysisQueueVerdict::Healthy
    );
}

#[test]
fn nothing_claimed_and_nothing_built_is_not_a_fault() {
    // A fully indexed library and `vod_index_mins = 0` look the same here,
    // and calling either one dead would be a false alarm every night.
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 0, 0, 0), 0, 0, 0),
        AnalysisQueueVerdict::Idle
    );
    // Still idle when another pipeline is busy: `skip_markers` claims move
    // the shared lifecycle counter, and they are not evidence about this
    // queue in either direction.
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 0, 0, 0), 900, 40, 0),
        AnalysisQueueVerdict::Idle
    );
}

#[test]
fn a_restart_does_not_invent_a_failure_ratio() {
    // The regression this guard exists for: `attempt_limit_24h` outlives
    // the process and the old denominator did not, so any hard failure in
    // the last day made a fresh daemon report `degraded` on its first
    // sample. Both sides are the same window now.
    assert_eq!(
        analysis_queue_verdict(health(60, 1, 61, 0, 0), 0, 0, 0),
        AnalysisQueueVerdict::Healthy
    );
}

#[test]
fn losing_a_quarter_of_its_claims_is_degraded_even_while_producing() {
    assert_eq!(
        analysis_queue_verdict(health(3, 0, 25, 0, 0), 25, 12, 0),
        AnalysisQueueVerdict::Degraded
    );
}

#[test]
fn the_lease_loss_ratio_waits_for_a_sample_worth_dividing() {
    let under = ANALYSIS_RATIO_CLAIM_FLOOR - 1;
    // One preemption out of a handful of claims is a Tuesday, not a fault.
    assert_eq!(
        analysis_queue_verdict(health(5, 0, 6, 0, 0), under, under / 4 + 1, 0),
        AnalysisQueueVerdict::Healthy
    );
    // At the floor the same proportion is worth reporting.
    assert_eq!(
        analysis_queue_verdict(
            health(5, 0, 6, 0, 0),
            ANALYSIS_RATIO_CLAIM_FLOOR,
            ANALYSIS_RATIO_CLAIM_FLOOR / 4,
            0
        ),
        AnalysisQueueVerdict::Degraded
    );
}

#[test]
fn failures_and_a_standing_stale_row_each_degrade_on_their_own() {
    // A tenth of what was claimed exhausting its budget.
    assert_eq!(
        analysis_queue_verdict(health(30, 4, 40, 0, 0), 0, 0, 0),
        AnalysisQueueVerdict::Degraded
    );
    // Just under a tenth is not.
    assert_eq!(
        analysis_queue_verdict(health(30, 4, 41, 0, 0), 0, 0, 0),
        AnalysisQueueVerdict::Healthy
    );
    // One sample with a lapsed row is a sweep that has not run yet.
    assert_eq!(
        analysis_queue_verdict(health(30, 0, 40, 0, 1), 40, 0, 1),
        AnalysisQueueVerdict::Healthy
    );
    // Two in a row is a queue that is not sweeping.
    assert_eq!(
        analysis_queue_verdict(health(30, 0, 40, 0, 1), 40, 0, 2),
        AnalysisQueueVerdict::Degraded
    );
}

#[test]
fn too_few_claims_to_be_sure_is_not_healthy_either() {
    // Something was claimed and nothing finished. Not provable, not fine.
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 3, 0, 0), 3, 3, 0),
        AnalysisQueueVerdict::Degraded
    );
    // And a lapsed row on its own is enough to keep it out of `idle`.
    assert_eq!(
        analysis_queue_verdict(health(0, 0, 0, 0, 1), 0, 0, 0),
        AnalysisQueueVerdict::Degraded
    );
}
