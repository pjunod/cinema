use super::*;
use crate::process::observation::ProcessIdentity;

fn baseline(at: Instant, pid: u32, start_marker: u64, cpu_ns: u64, epoch: u64) -> Baseline {
    Baseline {
        at,
        counters: ProcessCounters {
            identity: ProcessIdentity { pid, start_marker },
            cpu_ns,
            resident_bytes: Metric::Known(4096),
        },
        control_epoch: epoch,
    }
}

#[test]
fn stale_and_cached_observations_cannot_form_a_cost_window() {
    let start = Instant::now();
    let before = baseline(start, 7, 11, 100, 2);
    assert_eq!(
        classify_window(before, baseline(start, 7, 11, 200, 2), start),
        WindowDecision::Stale
    );
    let stale = baseline(start + Duration::from_secs(1), 7, 11, 200, 2);
    assert_eq!(
        classify_window(before, stale, start + MAX_WINDOW + Duration::from_secs(2)),
        WindowDecision::Stale
    );
}

#[test]
fn identity_control_and_regressed_counters_rebase_without_a_sample() {
    let start = Instant::now();
    let before = baseline(start, 7, 11, 100, 2);
    let after = start + Duration::from_secs(2);
    assert_eq!(
        classify_window(before, baseline(after, 8, 12, 300, 2), after),
        WindowDecision::Rebase
    );
    assert_eq!(
        classify_window(before, baseline(after, 7, 11, 300, 3), after),
        WindowDecision::Rebase
    );
    assert_eq!(
        classify_window(before, baseline(after, 7, 11, 99, 2), after),
        WindowDecision::Rebase
    );
    assert_eq!(
        classify_window(before, baseline(after, 7, 11, 300, 2), after + MAX_WINDOW),
        WindowDecision::Record(Duration::from_secs(2))
    );
    let long_gap = start + MAX_WINDOW + Duration::from_secs(1);
    assert_eq!(
        classify_window(before, baseline(long_gap, 7, 11, 300, 2), long_gap),
        WindowDecision::Rebase
    );
}

#[test]
fn only_fresh_monotonic_windows_accumulate_or_record() {
    let start = Instant::now();
    let before = baseline(start, 7, 11, 100, 2);
    assert_eq!(
        classify_window(
            before,
            baseline(start + MIN_WINDOW / 2, 7, 11, 200, 2),
            start + MIN_WINDOW / 2,
        ),
        WindowDecision::Accumulate
    );
    assert!(matches!(
        classify_window(
            before,
            baseline(start + MIN_WINDOW, 7, 11, 200, 2),
            start + MIN_WINDOW,
        ),
        WindowDecision::Record(_)
    ));
}
