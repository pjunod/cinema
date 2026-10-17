use super::*;

/// Startup never waits on or refuses for clock evidence: missing
/// coverage is reported and the activation admission still proceeds.
#[cfg(feature = "hiqlite-store")]
#[test]
fn startup_clock_evidence_is_advisory_and_never_blocks_activation() {
    use crate::cluster::clock::{ClockDecision, ClockRefusal, ClusterClockGuard};
    let standalone = ClusterClockGuard::new(false);
    assert_eq!(startup_clock_advisory(&standalone, "observation"), None);

    // A replicated guard with an unreachable member has no coverage.
    let guard = std::sync::Arc::new(ClusterClockGuard::new(true));
    assert_eq!(
        startup_clock_advisory(&guard, "observation"),
        Some(ClockRefusal::Unknown)
    );
    let admission = guard
        .acquire_owned_for(ClockDecision::MembershipChange)
        .expect("startup activation is not refused for clock evidence");
    assert_eq!(admission.revalidate(), Ok(()));
    assert!(guard.prometheus().contains(
        "plurx_cluster_clock_advisory_refusals_total{decision=\"membership_change\",cause=\"unknown\"} 2\n"
    ));
}

#[tokio::test(start_paused = true)]
async fn immediate_watermark_errors_cannot_starve_due_startup_log() {
    let mut next_wait_log = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut due_logs = 0;
    for _ in 0..100 {
        due_logs += usize::from(startup_wait_log_is_due(&mut next_wait_log));
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    due_logs += usize::from(startup_wait_log_is_due(&mut next_wait_log));

    assert_eq!(
        due_logs, 1,
        "the due ten-second log must survive fast errors"
    );
    assert!(!startup_wait_log_is_due(&mut next_wait_log));
}
