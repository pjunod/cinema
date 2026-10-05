//! Node-local clock evidence and typed consumer admission. No HTTP or Store calls.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CLOCK_OFFSET_REFUSAL_MS: i64 = 2_000;
pub const CLOCK_OBSERVATION_MAX_AGE: Duration = Duration::from_secs(25);
pub const CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS: i64 = 250;
#[cfg(feature = "hiqlite-store")]
const REMOVAL_REACHABILITY_STABILIZATION: Duration = Duration::from_secs(30);

/// Address-free identity of a locally applied Raft membership entry. Log
/// identity is part of coverage even when an ABA change restores the same IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockMembershipIdentity {
    pub local_node: u64,
    pub log: (u64, u64, u64),
    pub members: BTreeSet<u64>,
    pub voters: BTreeSet<u64>,
}

/// A synchronous in-process watch only: implementations must not perform
/// network/Store IO or await. Unknown, stopped and unapplied return None.
pub trait ClockMembershipSource: Send + Sync {
    fn current(&self) -> Option<ClockMembershipIdentity>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerClockOffset {
    Bounded {
        offset_us: i64,
        uncertainty_us: i64,
        observed_at: Instant,
    },
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator-enabled guard. `ClusterClockGuard::new` defaults to
    /// advisory mode, covered by `enforcement_off_admits_and_counts_advisory`.
    fn enforced() -> ClusterClockGuard {
        let guard = ClusterClockGuard::new(true);
        guard.set_enforced(true);
        guard
    }

    fn bounded(now: Instant) -> BTreeMap<String, PeerClockOffset> {
        BTreeMap::from([(
            "peer".into(),
            PeerClockOffset::Bounded {
                offset_us: 0,
                uncertainty_us: 1000,
                observed_at: now,
            },
        )])
    }

    #[test]
    fn local_discontinuity_invalidates_generation() {
        // Before the next probe, during t1/t4, after a decision ticket, and
        // a common-mode peer step all use the same serialized generation fence.
        for stage in [
            "before probe",
            "during exchange",
            "after ticket",
            "common mode",
        ] {
            let guard = enforced();
            let ticket = guard.roster(&["peer".into()]);
            assert!(guard.publish(ticket, bounded(Instant::now())));
            let old = guard.ticket();
            {
                let mut inner = guard.inner.lock().expect("clock state lock");
                let before = Instant::now();
                let current_wall = inner.anchor_wall_ms.expect("fixture anchor") + 15_000;
                ClusterClockGuard::continuity(&mut inner, before, Some(current_wall), before);
                assert_eq!(
                    inner.snapshot.clock_generation,
                    old.clock_generation + 1,
                    "{stage}"
                );
                assert!(matches!(
                    inner.snapshot.state,
                    ClusterClockState::Incomplete { .. }
                ));
                assert_eq!(inner.snapshot.peers["peer"], PeerClockOffset::Unknown);
            }
            assert!(!guard.publish(old, bounded(Instant::now())), "{stage}");
        }
    }

    #[test]
    fn fixed_anchor_and_unrepresentable_wall_invalidate() {
        let guard = enforced();
        let mut inner = guard.inner.lock().expect("clock state lock");
        let mono = inner.anchor_mono;
        let wall = inner.anchor_wall_ms.expect("fixture anchor");
        for delta in [100, 200] {
            ClusterClockGuard::continuity(&mut inner, mono, Some(wall + delta), mono);
            assert_eq!(inner.snapshot.clock_generation, 0);
        }
        ClusterClockGuard::continuity(&mut inner, mono, Some(wall + 300), mono);
        assert_eq!(inner.snapshot.clock_generation, 1);
        ClusterClockGuard::continuity(&mut inner, mono, None, mono);
        assert_eq!(inner.snapshot.clock_generation, 2);
    }

    #[test]
    fn roster_failure_expiry_and_passive_metrics_preserve_unknown() {
        let guard = enforced();
        assert!(matches!(
            guard.snapshot().state,
            ClusterClockState::Incomplete { .. }
        ));
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(ticket, bounded(Instant::now())));
        assert!(guard
            .prometheus()
            .contains("plurx_cluster_clock_offset_seconds{peer=\"peer\"} 0\n"));
        let old = guard.ticket();
        guard.roster_failed();
        assert!(!guard.publish(old, bounded(Instant::now())));
        assert!(!guard
            .prometheus()
            .contains("plurx_cluster_clock_offset_seconds{peer=\"peer\"}"));
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(ticket, bounded(Instant::now() - Duration::from_secs(26))));
        assert!(matches!(
            guard.snapshot().state,
            ClusterClockState::Incomplete { .. }
        ));
        let ticket = guard.roster(&[]);
        assert!(guard.publish(ticket, BTreeMap::new()));
        assert_eq!(guard.snapshot().state, ClusterClockState::NoPeers);
        assert!(!guard.prometheus().contains("peer=\"peer\""));
        assert_eq!(
            ClusterClockGuard::new(false).snapshot().state,
            ClusterClockState::NoPeers
        );
    }

    fn publish_offset(guard: &ClusterClockGuard, offset_us: i64, uncertainty_us: i64) {
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(
            ticket,
            BTreeMap::from([(
                "peer".into(),
                PeerClockOffset::Bounded {
                    offset_us,
                    uncertainty_us,
                    observed_at: Instant::now(),
                },
            )])
        ));
    }

    #[test]
    fn prepared_admission_never_replaces_original_refusal_time_or_guard() {
        let guard = enforced();
        let initially_unknown = guard.acquire();
        assert!(guard.prometheus().contains(
            "plurx_cluster_clock_refusals_total{decision=\"membership_change\",cause=\"unknown\"} 0\n"
        ));
        publish_offset(&guard, 0, 1_000);
        assert_eq!(
            guard
                .admit_for(ClockDecision::MembershipChange, initially_unknown)
                .err(),
            Some(ClockRefusal::Unknown),
            "later recovery must not mint a new pre-await admission"
        );
        let prepared = guard.acquire().expect("bounded entry");
        let original_now = prepared.now_ms();
        let admitted = guard
            .admit_for(ClockDecision::MembershipChange, Ok(prepared))
            .expect("unchanged exact guard");
        assert_eq!(admitted.now_ms(), original_now);
        guard.roster_failed();
        publish_offset(&guard, 0, 1_000);
        assert_eq!(
            guard
                .admit_for(ClockDecision::MembershipChange, Ok(prepared))
                .err(),
            Some(ClockRefusal::GenerationChanged)
        );
        let other = ClusterClockGuard::new(false);
        other.set_enforced(true);
        assert_eq!(
            other
                .admit_for(ClockDecision::MembershipChange, guard.acquire())
                .err(),
            Some(ClockRefusal::GenerationChanged),
            "an equal-looking foreign guard is not authority"
        );
        publish_offset(&guard, 2_500_000, 1_000);
        let originally_unbounded = guard.acquire();
        publish_offset(&guard, 0, 1_000);
        assert_eq!(
            guard
                .admit_for(ClockDecision::MembershipChange, originally_unbounded)
                .err(),
            Some(ClockRefusal::Offset)
        );
        let before = guard.prometheus();
        let unused_idempotent_capture = guard.acquire();
        assert!(unused_idempotent_capture.is_ok());
        assert_eq!(
            guard.prometheus(),
            before,
            "discarded pure capture is not refusal"
        );
        assert!(before.contains(
            "plurx_cluster_clock_refusals_total{decision=\"membership_change\",cause=\"unknown\"} 1\n"
        ));
        assert!(before.contains(
            "plurx_cluster_clock_refusals_total{decision=\"membership_change\",cause=\"generation_changed\"} 1\n"
        ));
        assert!(before.contains(
            "plurx_cluster_clock_refusals_total{decision=\"membership_change\",cause=\"offset\"} 1\n"
        ));
    }

    #[test]
    fn consumer_refusals_count_only_typed_admission_not_policy_or_scrapes() {
        let guard = enforced();
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert_eq!(
            guard.acquire_for(ClockDecision::ExpiryScan).err(),
            Some(ClockRefusal::Unknown)
        );
        publish_offset(&guard, 0, 1_000);
        let acquired = guard
            .acquire_for(ClockDecision::Takeover)
            .expect("safe acquisition");
        let original_now = acquired.now_ms();
        guard.roster_failed();
        assert_eq!(
            guard.revalidate_for(ClockDecision::Takeover, &acquired),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(acquired.now_ms(), original_now);
        let first = guard.prometheus();
        let second = guard.prometheus();
        assert_eq!(first, second, "scraping must not count another refusal");
        assert!(first.contains(
            "plurx_cluster_clock_refusals_total{decision=\"expiry_scan\",cause=\"unknown\"} 1\n"
        ));
        assert!(first.contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"generation_changed\"} 1\n"
        ));
        assert!(first.contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"unknown\"} 0\n"
        ));
        assert_eq!(
            first
                .lines()
                .filter(|line| line.starts_with("plurx_cluster_clock_refusals_total{"))
                .count(),
            12,
            "metric cardinality is exactly three decisions by four causes"
        );
    }

    #[test]
    fn owned_acquisition_keeps_original_guard_time_across_detached_preparation() {
        let guard = Arc::new(ClusterClockGuard::new(false));
        let proof = guard
            .acquire_owned_for(ClockDecision::Takeover)
            .expect("standalone admission");
        let original_now = proof.now_ms();
        drop(guard);
        let (carried_now, result) = std::thread::spawn(move || {
            std::thread::yield_now();
            (proof.now_ms(), proof.revalidate())
        })
        .join()
        .expect("opaque owned proof moves into detached work");
        assert_eq!(carried_now, original_now);
        assert_eq!(result, Ok(()));

        for cause in ["recovered generation", "local/common-mode step", "expiry"] {
            let guard = Arc::new(enforced());
            publish_offset(&guard, 0, 1_000);
            let proof = guard
                .acquire_owned_for(ClockDecision::Takeover)
                .expect("safe bound");
            let original_now = proof.now_ms();
            match cause {
                "recovered generation" => {
                    guard.roster_failed();
                    publish_offset(&guard, 0, 1_000);
                }
                "local/common-mode step" => {
                    let mut inner = guard.inner.lock().expect("clock state lock");
                    let mono = Instant::now();
                    let wall = inner.anchor_wall_ms.expect("fixture anchor") + 15_000;
                    ClusterClockGuard::continuity(&mut inner, mono, Some(wall), mono);
                }
                "expiry" => {
                    let mut inner = guard.inner.lock().expect("clock state lock");
                    inner.snapshot.peers = bounded(Instant::now() - Duration::from_secs(26));
                }
                _ => unreachable!(),
            }
            assert_eq!(
                proof.revalidate(),
                Err(match cause {
                    "local/common-mode step" => ClockRefusal::LocalDiscontinuity,
                    // Expiry is not an identity change: the current policy
                    // re-check refuses it as Unknown coverage.
                    "expiry" => ClockRefusal::Unknown,
                    _ => ClockRefusal::GenerationChanged,
                }),
                "{cause}"
            );
            assert_eq!(proof.now_ms(), original_now, "{cause}");
        }
    }

    #[test]
    fn acquisition_current_missing_peer_invalidates_atomically_but_stale_round_does_not() {
        let guard = enforced();
        publish_offset(&guard, 0, 1_000);
        let safe = guard.acquire().expect("safe prior evidence");
        let round = guard.roster(&["peer".into()]);
        assert!(!guard.publish(round, BTreeMap::new()));
        assert_eq!(
            guard.revalidate(&safe),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));

        let stale = guard.roster(&["peer".into()]);
        publish_offset(&guard, 0, 1_000);
        let newer = guard.acquire().expect("newer safe evidence");
        assert!(!guard.publish(stale, BTreeMap::new()));
        assert_eq!(guard.revalidate(&newer), Ok(()));
        assert!(guard.acquire().is_ok());
    }

    #[test]
    fn acquisition_requires_exact_fresh_roster_and_safe_upper_bound() {
        let standalone = ClusterClockGuard::new(false);
        let ticket = standalone.acquire().expect("standalone proof");
        assert!(ticket.now_ms() > 0);
        assert_eq!(standalone.revalidate(&ticket), Ok(()));

        let guard = enforced();
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        guard.roster(&[]);
        let empty = guard.acquire().expect("proved empty committed roster");
        assert_eq!(guard.revalidate(&empty), Ok(()));
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.roster_observed_at = Some(Instant::now() - Duration::from_secs(26));
        }
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert_eq!(guard.revalidate(&empty), Err(ClockRefusal::Unknown));

        // Exact threshold includes uncertainty, on either side of zero.
        for offset in [1_999_000, -1_999_000] {
            publish_offset(&guard, offset, 1_000);
            assert!(guard.acquire().is_ok());
        }
        for (offset, uncertainty) in [(1_500_000, 601_000), (-2_500_000, 1_000)] {
            publish_offset(&guard, offset, uncertainty);
            assert_eq!(guard.acquire().err(), Some(ClockRefusal::Offset));
        }
        for (offset, uncertainty) in [(i64::MIN, 1_000), (0, -1)] {
            publish_offset(&guard, offset, uncertainty);
            assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        }
        publish_offset(&guard, 0, 1_000);
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.snapshot.peers = bounded(Instant::now() - Duration::from_secs(26));
        }
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        publish_offset(&guard, 0, 1_000);
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.snapshot.peers = bounded(Instant::now() + Duration::from_secs(1));
        }
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        guard.roster_failed();
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
    }

    #[test]
    fn acquisition_ticket_is_guard_bound_and_rechecks_both_generations() {
        let guard = enforced();
        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe initial ticket");
        let original_now = ticket.now_ms();
        let other = enforced();
        publish_offset(&other, 0, 1_000);
        assert_eq!(
            other.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.revalidate(&ticket), Ok(()));
        assert_eq!(ticket.now_ms(), original_now);

        // An ordinary completed round (the prober's ~10 s cadence) is not
        // decision evidence: the same ticket stays valid while the policy
        // still admits, so a long startup/takeover tail is not refused.
        publish_offset(&guard, 0, 1_000);
        assert_eq!(guard.revalidate(&ticket), Ok(()));
        let ticket = guard.acquire().expect("new safe ticket");
        guard.roster(&["peer".into(), "new-peer".into()]);
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));

        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe before failed round");
        let round = guard.roster(&["peer".into()]);
        assert!(guard.publish(
            round,
            BTreeMap::from([("peer".into(), PeerClockOffset::Unknown)])
        ));
        assert_eq!(guard.revalidate(&ticket), Err(ClockRefusal::Unknown));
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
    }

    #[test]
    fn completed_rounds_keep_tickets_but_policy_is_rechecked() {
        let guard = Arc::new(enforced());
        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe ticket");
        let owned = guard
            .acquire_owned_for(ClockDecision::Takeover)
            .expect("safe owned ticket");
        for _ in 0..5 {
            publish_offset(&guard, 250_000, 1_000);
        }
        assert_eq!(guard.revalidate(&ticket), Ok(()));
        assert_eq!(owned.revalidate(), Ok(()));
        publish_offset(&guard, 2_500_000, 1_000);
        assert_eq!(guard.revalidate(&ticket), Err(ClockRefusal::Offset));
        assert_eq!(owned.revalidate(), Err(ClockRefusal::Offset));
        publish_offset(&guard, 0, 1_000);
        assert_eq!(
            guard.revalidate(&ticket),
            Ok(()),
            "same identity, safe again"
        );
        guard.roster_failed();
        publish_offset(&guard, 0, 1_000);
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged),
            "roster failure is decision evidence"
        );
    }

    #[test]
    fn enforcement_off_admits_and_counts_advisory() {
        let guard = Arc::new(ClusterClockGuard::new(true));
        assert!(!guard.is_enforced(), "default is advisory");
        assert_eq!(guard.check_evidence(), Err(ClockRefusal::Unknown));
        let pure = guard.acquire().expect("uncounted capture admits while off");
        assert_eq!(guard.revalidate(&pure), Ok(()));
        let takeover = guard
            .acquire_for(ClockDecision::Takeover)
            .expect("takeover is never blocked while off");
        let owned = guard
            .acquire_owned_for(ClockDecision::ExpiryScan)
            .expect("expiry is never blocked while off");
        let membership = guard
            .admit_for(ClockDecision::MembershipChange, Err(ClockRefusal::Unknown))
            .expect("an originally refused capture is admitted while off");
        assert!(membership.now_ms() > 0);
        guard.roster_failed();
        assert_eq!(
            guard.revalidate_for(ClockDecision::Takeover, &takeover),
            Ok(())
        );
        assert_eq!(owned.revalidate(), Ok(()));
        let metrics = guard.prometheus();
        assert!(metrics.contains("plurx_cluster_clock_enforced 0\n"));
        assert!(
            metrics
                .lines()
                .filter(|line| line.starts_with("plurx_cluster_clock_refusals_total{"))
                .all(|line| line.ends_with(" 0")),
            "nothing is actually refused while off"
        );
        for line in [
            "plurx_cluster_clock_advisory_refusals_total{decision=\"takeover\",cause=\"unknown\"} 1\n",
            "plurx_cluster_clock_advisory_refusals_total{decision=\"takeover\",cause=\"generation_changed\"} 1\n",
            "plurx_cluster_clock_advisory_refusals_total{decision=\"expiry_scan\",cause=\"unknown\"} 1\n",
            "plurx_cluster_clock_advisory_refusals_total{decision=\"expiry_scan\",cause=\"generation_changed\"} 1\n",
            "plurx_cluster_clock_advisory_refusals_total{decision=\"membership_change\",cause=\"unknown\"} 1\n",
        ] {
            assert!(metrics.contains(line), "{line}");
        }
        assert_eq!(
            metrics
                .lines()
                .filter(|line| line.starts_with("plurx_cluster_clock_advisory_refusals_total{"))
                .count(),
            12,
            "advisory vocabulary is the same closed three by four"
        );

        // Turning it on applies the current policy to tickets minted while off.
        guard.set_enforced(true);
        assert_eq!(
            guard.revalidate_for(ClockDecision::Takeover, &takeover),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(
            guard.acquire_for(ClockDecision::Takeover).err(),
            Some(ClockRefusal::Unknown)
        );
        publish_offset(&guard, 0, 1_000);
        assert!(guard.acquire_for(ClockDecision::Takeover).is_ok());
        assert!(guard
            .prometheus()
            .contains("plurx_cluster_clock_enforced 1\n"));
    }

    #[test]
    fn acquisition_refuses_common_mode_step_and_unrepresentable_wall() {
        let guard = enforced();
        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe before common-mode step");
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            // Relative peer offsets still say zero. Only synchronous local
            // continuity at the actual policy read can invalidate this proof.
            *inner.anchor_wall_ms.as_mut().expect("fixture anchor") += 15_000;
        }
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::LocalDiscontinuity)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, 0, 1_000);
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.anchor_wall_ms = None;
        }
        assert_eq!(
            guard.acquire().err(),
            Some(ClockRefusal::LocalDiscontinuity)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
    }

    #[test]
    fn readiness_counts_completed_positive_rounds_and_resets_on_invalid_evidence() {
        let guard = enforced();
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, 2_500_000, 1_000);
        for _ in 0..10 {
            assert!(
                !guard.snapshot().readiness.is_unbounded(),
                "polls are not rounds"
            );
        }
        let invalid = guard.roster(&["peer".into()]);
        assert!(!guard.publish(invalid, BTreeMap::new()));
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, -2_500_000, 1_000);
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, -2_500_000, 1_000);
        assert!(guard.snapshot().readiness.is_unbounded());
        for reset in [
            "safe",
            "unknown",
            "failure",
            "expiry",
            "roster",
            "continuity",
        ] {
            match reset {
                "safe" => publish_offset(&guard, 0, 1_000),
                "unknown" => {
                    let round = guard.roster(&["peer".into()]);
                    assert!(guard.publish(
                        round,
                        BTreeMap::from([("peer".into(), PeerClockOffset::Unknown)])
                    ));
                }
                "failure" => guard.roster_failed(),
                "expiry" => {
                    let mut inner = guard.inner.lock().expect("clock state lock");
                    inner.snapshot.peers = bounded(Instant::now() - Duration::from_secs(26));
                }
                "roster" => {
                    guard.roster(&[]);
                }
                "continuity" => {
                    let mut inner = guard.inner.lock().expect("clock state lock");
                    inner.anchor_wall_ms = None;
                    drop(inner);
                    assert_eq!(
                        guard.acquire().err(),
                        Some(ClockRefusal::LocalDiscontinuity)
                    );
                }
                _ => unreachable!("listed fixture reset"),
            }
            assert!(!guard.snapshot().readiness.is_unbounded(), "{reset}");
            publish_offset(&guard, 2_500_000, 1_000);
            assert!(
                !guard.snapshot().readiness.is_unbounded(),
                "first round after {reset}"
            );
            publish_offset(&guard, 2_500_000, 1_000);
            assert!(
                guard.snapshot().readiness.is_unbounded(),
                "second round after {reset}"
            );
        }
        let metrics = guard.prometheus();
        assert!(metrics.contains("measurement-only emits zero"));
        assert!(metrics.contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"offset\"} 0"
        ));
    }

    #[test]
    #[cfg(feature = "hiqlite-store")]
    fn k06_removal_directory_pins_uuid_raft_origin_and_original_time() {
        struct Source(ClockMembershipIdentity);
        impl ClockMembershipSource for Source {
            fn current(&self) -> Option<ClockMembershipIdentity> {
                Some(self.0.clone())
            }
        }
        let identity = ClockMembershipIdentity {
            local_node: 1,
            log: (2, 1, 7),
            members: BTreeSet::from([1, 2, 3]),
            voters: BTreeSet::from([1, 2]),
        };
        let guard = ClusterClockGuard::with_membership_source(Arc::new(Source(identity.clone())));
        guard.set_enforced(true);
        let target = "00000000-0000-0000-0000-000000000002";
        let survivor = "00000000-0000-0000-0000-000000000003";
        let mut roster = super::super::membership::ClockPeerRoster {
            membership: Some(identity.clone()),
            peers: vec![
                super::super::membership::ActivityPeer {
                    node_id: target.into(),
                    raft_id: 2,
                    http_base: Some("http://target:80/".into()),
                    reachable: false,
                },
                super::super::membership::ActivityPeer {
                    node_id: survivor.into(),
                    raft_id: 3,
                    http_base: Some("https://survivor:443".into()),
                    reachable: true,
                },
            ],
        };
        let ids = vec![target.into(), survivor.into()];
        guard
            .roster_for_membership(&ids, Some(&identity))
            .expect("generic roster");
        assert!(matches!(
            guard.capture_removal_raft(2),
            Err(ClockRefusal::Unknown)
        ));
        let round = guard
            .roster_for_peer_directory(&roster)
            .expect("exact directory");
        assert!(guard.publish(
            round,
            BTreeMap::from([
                (target.into(), PeerClockOffset::Unknown),
                (
                    survivor.into(),
                    PeerClockOffset::Bounded {
                        offset_us: 0,
                        uncertainty_us: 1,
                        observed_at: Instant::now()
                    }
                ),
            ])
        ));
        let capture = guard
            .capture_removal_raft(2)
            .expect("target may remain Unknown");
        let original_time = capture.now_ms();
        assert!(guard
            .revalidate_removal_directory(&capture, &roster, target, 2, "local")
            .is_ok());
        roster.peers[0].http_base = Some("http://target".into());
        assert!(guard
            .revalidate_removal_directory(&capture, &roster, target, 2, "local")
            .is_ok());
        roster.peers[0].raft_id = 3;
        roster.peers[1].raft_id = 2;
        assert_eq!(
            guard.revalidate_removal_directory(&capture, &roster, target, 2, "local"),
            Err(ClockRefusal::GenerationChanged)
        );
        roster.peers[0].raft_id = 2;
        roster.peers[1].raft_id = 3;
        roster.peers[1].http_base = Some("https://different-survivor".into());
        assert_eq!(
            guard.revalidate_removal_directory(&capture, &roster, target, 2, "local"),
            Err(ClockRefusal::GenerationChanged)
        );
        guard
            .roster_for_peer_directory(&roster)
            .expect("new directory");
        assert_eq!(
            guard.revalidate_removal_capture(&capture),
            Err(ClockRefusal::GenerationChanged)
        );
        assert!(guard
            .snapshot()
            .peers
            .values()
            .all(|peer| matches!(peer, PeerClockOffset::Unknown)));
        assert_eq!(capture.now_ms(), original_time);
    }

    #[test]
    fn applied_membership_watch_invalidates_aba_proofs_without_a_probe_tick() {
        struct Source(Mutex<Option<ClockMembershipIdentity>>);
        impl ClockMembershipSource for Source {
            fn current(&self) -> Option<ClockMembershipIdentity> {
                self.0.lock().expect("fixture watch").clone()
            }
        }
        let source = Arc::new(Source(Mutex::new(None)));
        let guard = ClusterClockGuard::with_membership_source(source.clone());
        guard.set_enforced(true);
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        let unknown_empty = guard.roster(&[]);
        assert!(!guard.publish(unknown_empty, BTreeMap::new()));
        let identity = ClockMembershipIdentity {
            local_node: 1,
            log: (2, 1, 7),
            members: BTreeSet::from([1, 2]),
            voters: BTreeSet::from([1, 2]),
        };
        *source.0.lock().expect("fixture watch") = Some(identity.clone());
        assert_eq!(
            guard.roster_for_membership(&[], Some(&identity)),
            Err(ClockRefusal::Unknown),
            "remote coverage cannot be silently shortened"
        );
        let roster = ["peer".into()];
        let round = guard
            .roster_for_membership(&roster, Some(&identity))
            .expect("mapped applied roster");
        assert!(guard.publish(round, bounded(Instant::now())));
        let original = guard.acquire().expect("original bounded proof");
        let original_time = original.now_ms();
        let pending = guard
            .roster_for_membership(&roster, Some(&identity))
            .expect("original pending round");
        let newer = ClockMembershipIdentity {
            log: (3, 2, 9),
            ..identity.clone()
        };
        *source.0.lock().expect("fixture watch") = Some(newer.clone());
        // No prober ran: the actual consumer's local check catches raw ABA.
        assert_eq!(
            guard.revalidate(&original),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(original.now_ms(), original_time);
        assert!(!guard.publish(pending, bounded(Instant::now())));
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert_eq!(
            guard.roster_for_membership(&roster, Some(&identity)),
            Err(ClockRefusal::GenerationChanged)
        );
        let round = guard
            .roster_for_membership(&roster, Some(&newer))
            .expect("new mapped applied roster");
        assert!(guard.publish(round, bounded(Instant::now())));
        let accepted = guard.acquire().expect("new bounded proof");
        let learner = ClockMembershipIdentity {
            voters: BTreeSet::from([1]),
            ..newer.clone()
        };
        *source.0.lock().expect("fixture watch") = Some(learner);
        assert_eq!(
            guard.revalidate(&accepted),
            Err(ClockRefusal::GenerationChanged)
        );
        *source.0.lock().expect("fixture watch") = None;
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert!(guard.roster_for_membership(&[], None).is_err());
        let singleton = ClockMembershipIdentity {
            log: (4, 1, 11),
            members: BTreeSet::from([1]),
            voters: BTreeSet::from([1]),
            ..identity
        };
        *source.0.lock().expect("fixture watch") = Some(singleton.clone());
        let empty = guard
            .roster_for_membership(&[], Some(&singleton))
            .expect("proved applied singleton");
        assert!(guard.publish(empty, BTreeMap::new()));
        assert!(guard.acquire().is_ok());
        assert_eq!(
            guard.revalidate(&original),
            Err(ClockRefusal::GenerationChanged)
        );
    }

    #[cfg(feature = "hiqlite-store")]
    struct FixedMembership(ClockMembershipIdentity);

    #[cfg(feature = "hiqlite-store")]
    impl ClockMembershipSource for FixedMembership {
        fn current(&self) -> Option<ClockMembershipIdentity> {
            Some(self.0.clone())
        }
    }

    #[cfg(feature = "hiqlite-store")]
    fn peer_uuid(raft_id: u64) -> String {
        format!("00000000-0000-0000-0000-{raft_id:012}")
    }

    /// An enforced guard bound to an applied membership with the given local
    /// node, members and voters, and an exact directory of every remote member.
    #[cfg(feature = "hiqlite-store")]
    fn membership_guard(
        local_node: u64,
        members: &[u64],
        voters: &[u64],
    ) -> (ClusterClockGuard, super::super::membership::ClockPeerRoster) {
        let identity = ClockMembershipIdentity {
            local_node,
            log: (2, 1, 7),
            members: members.iter().copied().collect(),
            voters: voters.iter().copied().collect(),
        };
        let guard =
            ClusterClockGuard::with_membership_source(Arc::new(FixedMembership(identity.clone())));
        guard.set_enforced(true);
        let roster = super::super::membership::ClockPeerRoster {
            membership: Some(identity),
            peers: members
                .iter()
                .filter(|raft_id| **raft_id != local_node)
                .map(|raft_id| super::super::membership::ActivityPeer {
                    node_id: peer_uuid(*raft_id),
                    raft_id: *raft_id,
                    http_base: Some(format!("https://node{raft_id}:443")),
                    reachable: true,
                })
                .collect(),
        };
        (guard, roster)
    }

    /// Publish one complete round: the listed raft ids get the given offset,
    /// every other remote member is Unknown.
    #[cfg(feature = "hiqlite-store")]
    fn publish_round(
        guard: &ClusterClockGuard,
        roster: &super::super::membership::ClockPeerRoster,
        bounded: &[(u64, i64)],
    ) {
        let round = guard
            .roster_for_peer_directory(roster)
            .expect("exact directory");
        let observations = roster
            .peers
            .iter()
            .map(|peer| {
                let observation = bounded
                    .iter()
                    .find(|(raft_id, _)| *raft_id == peer.raft_id)
                    .map_or(PeerClockOffset::Unknown, |(_, offset_us)| {
                        PeerClockOffset::Bounded {
                            offset_us: *offset_us,
                            uncertainty_us: 1_000,
                            observed_at: Instant::now(),
                        }
                    });
                (peer.node_id.clone(), observation)
            })
            .collect();
        assert!(guard.publish(round, observations));
    }

    /// K-06 learner rule. A stopped learner (non-voting member) has no
    /// observation; it must not refuse takeover, the expiry scan or a
    /// membership change on any node while every voter is bounded. An
    /// unobserved voter still refuses, a MEASURED learner beyond the bound
    /// still refuses (it may own delegated media leases those decisions
    /// spend), and promoting a learner requires that learner's own bound.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn k06_stopped_learner_admits_when_voters_bounded_but_unobserved_voter_refuses() {
        let (guard, roster) = membership_guard(1, &[1, 2, 3, 4], &[1, 2, 3]);
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        let snapshot = guard.snapshot();
        assert_eq!(
            snapshot.state,
            ClusterClockState::Bounded {
                worst_abs_upper_us: 1_000
            }
        );
        assert_eq!(snapshot.unobserved_learners, 1);
        assert!(guard.acquire_for(ClockDecision::Takeover).is_ok());
        assert!(guard.acquire_for(ClockDecision::ExpiryScan).is_ok());
        assert!(guard
            .admit_for(ClockDecision::MembershipChange, guard.acquire())
            .is_ok());
        let metrics = guard.prometheus();
        assert!(metrics.contains("plurx_cluster_clock_unobserved_learners 1\n"));
        assert!(metrics.contains(&format!(
            "plurx_cluster_clock_observation_state{{peer=\"{}\",state=\"unknown\"}} 1\n",
            peer_uuid(4)
        )));
        assert!(
            metrics
                .lines()
                .filter(|line| line.starts_with("plurx_cluster_clock_refusals_total{"))
                .all(|line| line.ends_with(" 0")),
            "nothing refused: {metrics}"
        );

        // Promoting the unobserved learner makes it a voter: refused.
        assert_eq!(
            guard.admit_promotion_for(4, guard.acquire()).err(),
            Some(ClockRefusal::Unknown)
        );

        // An unobserved VOTER is still Incomplete, whatever the learner says.
        publish_round(&guard, &roster, &[(2, 0), (4, 0)]);
        assert_eq!(
            guard.snapshot().state,
            ClusterClockState::Incomplete {
                unknown_peers: 1,
                worst_abs_upper_us: 1_000
            }
        );
        assert_eq!(guard.snapshot().unobserved_learners, 0);
        assert_eq!(
            guard.acquire_for(ClockDecision::Takeover).err(),
            Some(ClockRefusal::Unknown)
        );
        assert_eq!(
            guard
                .admit_for(ClockDecision::MembershipChange, guard.acquire())
                .err(),
            Some(ClockRefusal::Unknown)
        );

        // A measured learner beyond the bound refuses like any member.
        publish_round(&guard, &roster, &[(2, 0), (3, 0), (4, 2_500_000)]);
        assert_eq!(
            guard.acquire_for(ClockDecision::Takeover).err(),
            Some(ClockRefusal::Offset)
        );

        // A bounded learner may be promoted; the ticket is re-checked against
        // the same target before submission.
        publish_round(&guard, &roster, &[(2, 0), (3, 0), (4, 0)]);
        let mut promotion = guard
            .admit_promotion_for(4, guard.acquire())
            .expect("bounded learner promotion");
        assert_eq!(guard.revalidate_promotion_for(4, &mut promotion), Ok(()));
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        assert!(guard.acquire().is_ok(), "the learner alone is excused");
        assert_eq!(
            guard.revalidate_promotion_for(4, &mut promotion),
            Err(ClockRefusal::Unknown),
            "but not as a promotion target"
        );
        assert_eq!(
            guard.admit_promotion_for(9, guard.acquire()).err(),
            Some(ClockRefusal::Unknown),
            "an unmapped target is never bounded"
        );
    }

    /// The local node may itself be a learner (it runs takeover for its own
    /// delegated sessions). Every voter is still its peer and must be
    /// bounded; another unobserved learner is excused; and promoting ITSELF
    /// needs only its own continuity, which the ticket already proves.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn k06_local_learner_requires_every_voter_and_excuses_other_learners() {
        let (guard, roster) = membership_guard(4, &[1, 2, 3, 4, 5], &[1, 2, 3]);
        publish_round(&guard, &roster, &[(1, 0), (2, 0), (3, 0)]);
        assert_eq!(guard.snapshot().unobserved_learners, 1);
        assert!(guard.acquire_for(ClockDecision::Takeover).is_ok());
        assert!(guard.admit_promotion_for(4, guard.acquire()).is_ok());
        assert_eq!(
            guard.admit_promotion_for(5, guard.acquire()).err(),
            Some(ClockRefusal::Unknown)
        );
        publish_round(&guard, &roster, &[(1, 0), (3, 0)]);
        assert_eq!(
            guard.acquire_for(ClockDecision::Takeover).err(),
            Some(ClockRefusal::Unknown),
            "an unobserved voter refuses even a learner's own decisions"
        );
        assert_eq!(
            guard.admit_promotion_for(4, guard.acquire()).err(),
            Some(ClockRefusal::Unknown)
        );
    }

    /// The learner role is proved only from the exact applied directory bound
    /// to the membership watch. Without it every peer counts as a voter, so
    /// the exemption can never be reached by a directory-less roster.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn k06_learner_role_needs_the_exact_applied_directory() {
        let (guard, roster) = membership_guard(1, &[1, 2, 3], &[1, 2]);
        publish_round(&guard, &roster, &[(2, 0)]);
        let inner = guard.inner.lock().expect("clock state lock");
        assert!(ClusterClockGuard::is_learner_peer(
            inner.membership.as_ref(),
            inner.peer_directory.as_ref(),
            &peer_uuid(3)
        ));
        assert!(!ClusterClockGuard::is_learner_peer(
            inner.membership.as_ref(),
            inner.peer_directory.as_ref(),
            &peer_uuid(2)
        ));
        assert!(
            !ClusterClockGuard::is_learner_peer(inner.membership.as_ref(), None, &peer_uuid(3)),
            "no exact directory: every peer is treated as a voter"
        );
        drop(inner);
        // A generic (directory-less) roster cannot prove a role: Unknown refuses.
        let generic = enforced();
        let ticket = generic.roster(&[peer_uuid(3)]);
        assert!(generic.publish(
            ticket,
            BTreeMap::from([(peer_uuid(3), PeerClockOffset::Unknown)])
        ));
        assert_eq!(generic.snapshot().unobserved_learners, 0);
        assert_eq!(generic.acquire().err(), Some(ClockRefusal::Unknown));

        // The production shape with the directory missing: the applied
        // membership watch IS bound (and names peer 3 a learner), but the
        // round was installed without the exact peer directory. The role is
        // then unproved and peer 3 counts as a voter.
        let identity = ClockMembershipIdentity {
            local_node: 1,
            log: (2, 1, 7),
            members: BTreeSet::from([1, 2, 3]),
            voters: BTreeSet::from([1, 2]),
        };
        let sourced =
            ClusterClockGuard::with_membership_source(Arc::new(FixedMembership(identity.clone())));
        sourced.set_enforced(true);
        let round = sourced
            .roster_for_membership(&[peer_uuid(2), peer_uuid(3)], Some(&identity))
            .expect("applied membership without a directory");
        assert!(sourced.publish(
            round,
            BTreeMap::from([
                (
                    peer_uuid(2),
                    PeerClockOffset::Bounded {
                        offset_us: 0,
                        uncertainty_us: 1_000,
                        observed_at: Instant::now(),
                    },
                ),
                (peer_uuid(3), PeerClockOffset::Unknown),
            ])
        ));
        {
            let inner = sourced.inner.lock().expect("clock state lock");
            assert!(inner.membership.is_some() && inner.peer_directory.is_none());
            assert!(!ClusterClockGuard::is_learner_peer(
                inner.membership.as_ref(),
                inner.peer_directory.as_ref(),
                &peer_uuid(3)
            ));
        }
        assert_eq!(sourced.snapshot().unobserved_learners, 0);
        assert_eq!(
            sourced.snapshot().state,
            ClusterClockState::Incomplete {
                unknown_peers: 1,
                worst_abs_upper_us: 1_000
            }
        );
        assert_eq!(sourced.acquire().err(), Some(ClockRefusal::Unknown));
        assert_eq!(
            sourced.acquire_for(ClockDecision::Takeover).err(),
            Some(ClockRefusal::Unknown)
        );
    }

    fn counted(guard: &ClusterClockGuard, family: &str, decision: &str, cause: &str) -> u64 {
        let prefix = format!("{family}{{decision=\"{decision}\",cause=\"{cause}\"}} ");
        guard
            .prometheus()
            .lines()
            .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::to_owned))
            .expect("closed metric vocabulary")
            .parse()
            .expect("counter value")
    }

    /// K-06 review P1. Coverage excuses an unobserved learner, but a learner
    /// owns delegated media sessions whose lease expiry its own clock wrote.
    /// Takeover and the expiry scan therefore need the route's OWNER bounded:
    /// an unobserved learner's routes are not contested (enforced) until it is
    /// measured within the bound or leaves the roster, while a voter's routes
    /// are. Advisory mode contests both and counts the owner refusal.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn k06_lease_owner_must_be_bounded_even_when_coverage_excuses_it() {
        let refusals = "plurx_cluster_clock_refusals_total";
        let advisory = "plurx_cluster_clock_advisory_refusals_total";
        let (guard, roster) = membership_guard(1, &[1, 2, 3, 4], &[1, 2, 3]);
        let guard = Arc::new(guard);
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        let learner = peer_uuid(4);
        let voter = peer_uuid(2);

        // The page as a whole is admitted: every voter is bounded.
        let page = guard
            .acquire_for(ClockDecision::ExpiryScan)
            .expect("voters bounded");
        assert_eq!(
            guard.admit_owner_for(ClockDecision::ExpiryScan, &page, &learner),
            Err(ClockRefusal::Unknown),
            "an unobserved learner's lease is not expired"
        );
        assert_eq!(
            guard.admit_owner_for(ClockDecision::ExpiryScan, &page, &voter),
            Ok(())
        );
        assert_eq!(
            guard.admit_owner_for(ClockDecision::ExpiryScan, &page, "not-a-member"),
            Ok(()),
            "an owner outside the proved roster is this node or a removed member"
        );
        assert_eq!(
            guard
                .acquire_owned_for_owner(ClockDecision::Takeover, &learner)
                .err(),
            Some(ClockRefusal::Unknown),
            "an unobserved learner's route is not taken over"
        );
        let voter_takeover = guard
            .acquire_owned_for_owner(ClockDecision::Takeover, &voter)
            .expect("bounded voter owner");
        assert_eq!(voter_takeover.revalidate(), Ok(()));
        assert_eq!(counted(&guard, refusals, "expiry_scan", "unknown"), 1);
        assert_eq!(counted(&guard, refusals, "takeover", "unknown"), 1);

        // Measured within the bound, the learner's routes are contestable;
        // the owner is re-checked on every revalidation of the takeover.
        publish_round(&guard, &roster, &[(2, 0), (3, 0), (4, 0)]);
        let learner_takeover = guard
            .acquire_owned_for_owner(ClockDecision::Takeover, &learner)
            .expect("bounded learner owner");
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        assert_eq!(
            learner_takeover.revalidate(),
            Err(ClockRefusal::Unknown),
            "the owner went unobserved before submission"
        );
        assert_eq!(voter_takeover.revalidate(), Ok(()));
        // A measured learner above the bound refuses coverage itself.
        publish_round(&guard, &roster, &[(2, 0), (3, 0), (4, 2_500_000)]);
        assert_eq!(
            guard.acquire_for(ClockDecision::ExpiryScan).err(),
            Some(ClockRefusal::Offset)
        );

        // An unproved roster proves nothing about absence.
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        let page = guard
            .acquire_for(ClockDecision::ExpiryScan)
            .expect("bounded");
        guard.roster_failed();
        assert_eq!(
            guard.admit_owner_for(ClockDecision::ExpiryScan, &page, "not-a-member"),
            Err(ClockRefusal::Unknown)
        );

        // Advisory: contested as before, the refusal counted.
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        guard.set_enforced(false);
        let page = guard
            .acquire_for(ClockDecision::ExpiryScan)
            .expect("bounded");
        let before = counted(&guard, advisory, "expiry_scan", "unknown");
        assert_eq!(
            guard.admit_owner_for(ClockDecision::ExpiryScan, &page, &learner),
            Ok(())
        );
        assert_eq!(
            counted(&guard, advisory, "expiry_scan", "unknown"),
            before + 1
        );
        assert!(guard
            .acquire_owned_for_owner(ClockDecision::Takeover, &learner)
            .is_ok());
        assert_eq!(counted(&guard, advisory, "takeover", "unknown"), 1);

        // Once the learner leaves the committed roster, its routes are
        // contestable: its renewals are fenced by its removal.
        let (removed, roster) = membership_guard(1, &[1, 2, 3], &[1, 2, 3]);
        publish_round(&removed, &roster, &[(2, 0), (3, 0)]);
        let page = removed
            .acquire_for(ClockDecision::ExpiryScan)
            .expect("bounded");
        assert_eq!(
            removed.admit_owner_for(ClockDecision::ExpiryScan, &page, &learner),
            Ok(())
        );
    }

    /// K-06 review P3(b). One promotion attempt counts at most one advisory
    /// refusal, however many pre-submission re-checks it crosses; enforced,
    /// the first refusal is counted and ends the attempt.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn k06_one_promotion_attempt_counts_one_advisory_refusal() {
        let advisory = "plurx_cluster_clock_advisory_refusals_total";
        let refusals = "plurx_cluster_clock_refusals_total";
        let (guard, roster) = membership_guard(1, &[1, 2, 3, 4], &[1, 2, 3]);
        guard.set_enforced(false);
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        // Refused at admission: counted there, never again for this attempt.
        let mut attempt = guard
            .admit_promotion_for(4, guard.acquire())
            .expect("advisory admits");
        assert_eq!(counted(&guard, advisory, "membership_change", "unknown"), 1);
        assert_eq!(guard.revalidate_promotion_for(4, &mut attempt), Ok(()));
        assert_eq!(guard.revalidate_promotion_for(4, &mut attempt), Ok(()));
        assert_eq!(counted(&guard, advisory, "membership_change", "unknown"), 1);

        // Admitted cleanly, then refused at both re-checks: counted once.
        publish_round(&guard, &roster, &[(2, 0), (3, 0), (4, 0)]);
        let mut attempt = guard
            .admit_promotion_for(4, guard.acquire())
            .expect("bounded learner");
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        assert_eq!(guard.revalidate_promotion_for(4, &mut attempt), Ok(()));
        assert_eq!(guard.revalidate_promotion_for(4, &mut attempt), Ok(()));
        assert_eq!(counted(&guard, advisory, "membership_change", "unknown"), 2);

        // Enforced, the refusal is counted and ends the attempt.
        guard.set_enforced(true);
        publish_round(&guard, &roster, &[(2, 0), (3, 0), (4, 0)]);
        let mut attempt = guard
            .admit_promotion_for(4, guard.acquire())
            .expect("bounded learner");
        publish_round(&guard, &roster, &[(2, 0), (3, 0)]);
        assert_eq!(
            guard.revalidate_promotion_for(4, &mut attempt),
            Err(ClockRefusal::Unknown)
        );
        assert_eq!(counted(&guard, refusals, "membership_change", "unknown"), 1);
        assert_eq!(counted(&guard, advisory, "membership_change", "unknown"), 2);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterClockState {
    NoPeers,
    Bounded {
        worst_abs_upper_us: i64,
    },
    Incomplete {
        unknown_peers: usize,
        worst_abs_upper_us: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDecisionTicket {
    pub clock_generation: u64,
    pub state_generation: u64,
    pub now_ms: i64,
}

/// Typed local refusal; it never establishes a submitted proposal's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockRefusal {
    #[error("clock coverage is incomplete or expired")]
    Unknown,
    #[error("clock offset upper bound exceeds the safety limit")]
    Offset,
    #[error("local wall clock continuity changed")]
    LocalDiscontinuity,
    #[error("clock decision evidence changed")]
    GenerationChanged,
}

/// Closed metric vocabulary: no target, session, or caller-supplied labels.
#[derive(Clone, Copy, Debug)]
pub enum ClockDecision {
    Takeover,
    MembershipChange,
    ExpiryScan,
}

impl ClockDecision {
    const ALL: [Self; 3] = [Self::Takeover, Self::MembershipChange, Self::ExpiryScan];

    const fn index(self) -> usize {
        match self {
            Self::Takeover => 0,
            Self::MembershipChange => 1,
            Self::ExpiryScan => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Takeover => "takeover",
            Self::MembershipChange => "membership_change",
            Self::ExpiryScan => "expiry_scan",
        }
    }
}

impl ClockRefusal {
    const ALL: [Self; 4] = [
        Self::Offset,
        Self::Unknown,
        Self::LocalDiscontinuity,
        Self::GenerationChanged,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Offset => 0,
            Self::Unknown => 1,
            Self::LocalDiscontinuity => 2,
            Self::GenerationChanged => 3,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Offset => "offset",
            Self::Unknown => "unknown",
            Self::LocalDiscontinuity => "local_discontinuity",
            Self::GenerationChanged => "generation_changed",
        }
    }
}

/// An acquisition proof belongs to this exact shared guard, not another node's
/// equal-looking generations. Unlike a measurement ticket it cannot be forged.
///
/// Its admission generation changes only on evidence that matters to the
/// decision (local wall discontinuity, membership/roster identity change or
/// roster failure). An ordinary completed measurement round does not
/// invalidate it; revalidation re-checks the current policy instead.
#[derive(Clone, Copy)]
pub struct ClockAcquisitionTicket<'guard> {
    guard: &'guard ClusterClockGuard,
    decision: ClockDecisionTicket,
    admission_generation: u64,
    /// Set once this attempt's advisory refusal has been counted, so a later
    /// re-check of the same attempt does not count it again.
    advisory_counted: bool,
}

/// Original node-local observation for a reduction, not admission by itself.
/// Only an exact durable removal fence can qualify exclusion of its target.
#[cfg(feature = "hiqlite-store")]
pub struct ClockRemovalCapture<'guard> {
    guard: &'guard ClusterClockGuard,
    decision: ClockDecisionTicket,
    admission_generation: u64,
    /// None only for an advisory capture minted while enforcement is off and
    /// coverage was unavailable; strict checks then refuse, and are softened.
    membership: Option<ClockMembershipIdentity>,
    peers: BTreeMap<String, PeerClockOffset>,
    directory: Option<BTreeMap<String, (u64, String)>>,
    target: RemovalTarget,
    captured_at: Instant,
    reachability_stable: bool,
}

#[cfg(feature = "hiqlite-store")]
enum RemovalTarget {
    Node(String),
    Raft(u64),
}

#[cfg(feature = "hiqlite-store")]
impl ClockRemovalCapture<'_> {
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.decision.now_ms
    }

    /// A wall-age comparison is unavailable for the entire original operation
    /// when it began inside a post-step stabilization window. Waiting cannot
    /// renew this capture into a new reachability observation.
    #[must_use]
    pub(crate) fn permits_wall_reachability(&self) -> bool {
        self.reachability_stable
    }

    pub(crate) fn remaining_removal_budget(&self) -> Option<Duration> {
        Duration::from_secs(15)
            .checked_sub(Instant::now().checked_duration_since(self.captured_at)?)
            .filter(|remaining| !remaining.is_zero())
    }

    fn matches_target(&self, node_id: &str, raft_id: u64) -> bool {
        match &self.target {
            RemovalTarget::Node(expected) => expected == node_id,
            RemovalTarget::Raft(expected) => *expected == raft_id,
        }
    }
}

impl ClockAcquisitionTicket<'_> {
    /// Bind this original value to the caller's expiry query/proposal. A later
    /// revalidation never replaces it with a fresh wall reading.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.decision.now_ms
    }
}

/// A detached preparation task carries the same opaque proof and original
/// caller time. Owning the exact guard does not mint a new decision or let a
/// caller reconstruct authority from public measurement generations.
pub struct OwnedClockAcquisitionTicket {
    guard: Arc<ClusterClockGuard>,
    decision: ClockDecisionTicket,
    admission_generation: u64,
    consumer: ClockDecision,
    /// The node whose lease this decision spends, re-checked on every
    /// revalidation (see [`ClusterClockGuard::acquire_owned_for_owner`]).
    owner: Option<String>,
}

impl OwnedClockAcquisitionTicket {
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.decision.now_ms
    }

    /// Only before initial submission. An already submitted, commit-unknown
    /// proposal must continue its exact reconciliation without this admission.
    pub fn revalidate(&self) -> Result<(), ClockRefusal> {
        let ticket = ClockAcquisitionTicket {
            guard: &self.guard,
            decision: self.decision,
            admission_generation: self.admission_generation,
            advisory_counted: false,
        };
        let strict = self.guard.revalidate_strict_with(&ticket, |inner| {
            self.owner.as_deref().map_or(Ok(()), |owner| {
                ClusterClockGuard::owner_policy(inner, owner)
            })
        });
        self.guard.decide(Some(self.consumer), strict)
    }
}

/// Passive facts from completed observation rounds, not readiness polls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockReadiness {
    consecutive_violating_rounds: u8,
}

impl ClockReadiness {
    #[must_use]
    pub fn is_unbounded(self) -> bool {
        self.consecutive_violating_rounds >= 2
    }
}

#[derive(Clone, Debug)]
pub struct ClockSnapshot {
    pub state: ClusterClockState,
    pub peers: BTreeMap<String, PeerClockOffset>,
    /// Committed non-voting learners in `peers` with no usable observation.
    /// They are reported here and in the per-peer state gauge, but they do
    /// not make `state` Incomplete: a learner holds no vote, so a stopped or
    /// unanswering one must not refuse every node's guarded decisions. A
    /// learner that IS measured still contributes its upper bound, and a
    /// promotion of a learner still requires that learner to be bounded.
    pub unobserved_learners: usize,
    pub clock_generation: u64,
    pub state_generation: u64,
    pub discontinuities: u64,
    pub unknown_rounds: u64,
    pub readiness: ClockReadiness,
}

struct ClockInner {
    snapshot: ClockSnapshot,
    /// Bumped only by decision-relevant evidence: local discontinuity,
    /// membership/roster identity change, or roster failure. Separate from
    /// the per-round `state_generation` that measurement publication uses.
    admission_generation: u64,
    roster_proved: bool,
    anchor_wall_ms: Option<i64>,
    anchor_mono: Instant,
    standalone: bool,
    roster_observed_at: Option<Instant>,
    membership: Option<ClockMembershipIdentity>,
    last_discontinuity: Option<Instant>,
    peer_directory: Option<BTreeMap<String, (u64, String)>>,
}

pub struct ClusterClockGuard {
    inner: Mutex<ClockInner>,
    membership_source: Option<Arc<dyn ClockMembershipSource>>,
    authority_reads: AtomicU64,
    /// Operator switch (Developer settings), default off. While off, every
    /// admission entry point admits and only records what it WOULD refuse.
    enforced: AtomicBool,
    refusals: [[AtomicU64; 4]; 3],
    advisory_refusals: [[AtomicU64; 4]; 3],
}

fn wall_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
}

impl ClusterClockGuard {
    #[must_use]
    pub fn new(replicated: bool) -> Self {
        let mono = Instant::now();
        Self {
            membership_source: None,
            authority_reads: AtomicU64::new(0),
            enforced: AtomicBool::new(false),
            refusals: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            advisory_refusals: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            inner: Mutex::new(ClockInner {
                admission_generation: 0,
                snapshot: ClockSnapshot {
                    state: if replicated {
                        ClusterClockState::Incomplete {
                            unknown_peers: 1,
                            worst_abs_upper_us: 0,
                        }
                    } else {
                        ClusterClockState::NoPeers
                    },
                    peers: BTreeMap::new(),
                    unobserved_learners: 0,
                    clock_generation: 0,
                    state_generation: 0,
                    discontinuities: 0,
                    unknown_rounds: 0,
                    readiness: ClockReadiness::default(),
                },
                roster_proved: !replicated,
                anchor_wall_ms: wall_ms(),
                anchor_mono: mono,
                standalone: !replicated,
                roster_observed_at: None,
                membership: None,
                last_discontinuity: None,
                peer_directory: None,
            }),
        }
    }

    /// Share this exact watch-backed guard with all node-local consumers.
    /// A closed or newly changed watch immediately invalidates prior coverage;
    /// it never establishes an empty roster from an absent directory.
    #[must_use]
    pub fn with_membership_source(source: Arc<dyn ClockMembershipSource>) -> Self {
        let mut guard = Self::new(true);
        guard.membership_source = Some(source);
        guard
    }

    fn refresh_membership(&self, inner: &mut ClockInner) {
        let Some(source) = &self.membership_source else {
            return;
        };
        let current = source.current();
        if current != inner.membership || (current.is_none() && inner.roster_proved) {
            inner.membership = current;
            inner.roster_proved = false;
            inner.peer_directory = None;
            inner.roster_observed_at = None;
            inner.snapshot.state_generation += 1;
            inner.admission_generation += 1;
            inner.snapshot.readiness = ClockReadiness::default();
            for peer in inner.snapshot.peers.values_mut() {
                *peer = PeerClockOffset::Unknown;
            }
            Self::recompute(inner);
        }
    }

    /// Apply the operator's enforcement setting. Off (the default) makes
    /// every admission entry point admit; refusals it would have made are
    /// counted in `plurx_cluster_clock_advisory_refusals_total` instead.
    pub fn set_enforced(&self, enforced: bool) {
        self.enforced.store(enforced, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_enforced(&self) -> bool {
        self.enforced.load(Ordering::SeqCst)
    }

    /// Pure, uncounted inspection of the current evidence against the
    /// admission policy, regardless of enforcement. This mints nothing and is
    /// what diagnostics and observation barriers use; consumers use
    /// `acquire`/`acquire_for`/`admit_for`.
    pub fn check_evidence(&self) -> Result<(), ClockRefusal> {
        self.capture().1
    }

    /// Apply enforcement to a strict result. Only actual consumer entry
    /// points pass a decision; pure inspection stays uncounted.
    fn decide(
        &self,
        decision: Option<ClockDecision>,
        strict: Result<(), ClockRefusal>,
    ) -> Result<(), ClockRefusal> {
        let Err(cause) = strict else {
            return Ok(());
        };
        let enforced = self.is_enforced();
        if let Some(decision) = decision {
            let counters = if enforced {
                &self.refusals
            } else {
                &self.advisory_refusals
            };
            counters[decision.index()][cause.index()].fetch_add(1, Ordering::Relaxed);
        }
        if enforced {
            Err(cause)
        } else {
            Ok(())
        }
    }

    /// One serialized continuity/state read: the ticket for the current
    /// generations plus whether the strict policy admits it.
    fn capture(&self) -> (ClockAcquisitionTicket<'_>, Result<(), ClockRefusal>) {
        self.capture_with(|_| Ok(()))
    }

    /// `capture` plus a decision-specific check read under the SAME lock.
    fn capture_with(
        &self,
        also: impl FnOnce(&ClockInner) -> Result<(), ClockRefusal>,
    ) -> (ClockAcquisitionTicket<'_>, Result<(), ClockRefusal>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let generation = inner.snapshot.clock_generation;
        let before = Instant::now();
        let decision = Self::continuity(&mut inner, before, wall_ms(), Instant::now());
        let strict = if decision.clock_generation == generation {
            Self::acquisition_policy(&inner).and_then(|()| also(&inner))
        } else {
            Err(ClockRefusal::LocalDiscontinuity)
        };
        (
            ClockAcquisitionTicket {
                guard: self,
                decision,
                admission_generation: inner.admission_generation,
                advisory_counted: false,
            },
            strict,
        )
    }

    /// Synchronously check fixed wall/monotonic continuity, serialized with evidence.
    #[must_use]
    pub fn ticket(&self) -> ClockDecisionTicket {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let before = Instant::now();
        let wall = wall_ms();
        let after = Instant::now();
        Self::continuity(&mut inner, before, wall, after)
    }

    /// Prepare a pure acquisition proof from one serialized continuity/state
    /// read. Future consumers must revalidate immediately before submission.
    /// This does not submit, count a production refusal, or cancel an existing
    /// commit-unknown proposal.
    /// While enforcement is off this always admits (uncounted).
    pub fn acquire(&self) -> Result<ClockAcquisitionTicket<'_>, ClockRefusal> {
        let (ticket, strict) = self.capture();
        self.decide(None, strict)?;
        Ok(ticket)
    }

    /// Capture before the manager resolves this UUID through any awaited read.
    /// Unknown target evidence is retained, never silently called NoPeers.
    #[cfg(feature = "hiqlite-store")]
    pub fn capture_removal_node(
        &self,
        node_id: &str,
    ) -> Result<ClockRemovalCapture<'_>, ClockRefusal> {
        if node_id.is_empty() || node_id.len() > 256 {
            return Err(ClockRefusal::Unknown);
        }
        self.capture_removal(RemovalTarget::Node(node_id.to_owned()))
    }

    /// Receiver-local capture; the untrusted requested id grants no exclusion.
    #[cfg(feature = "hiqlite-store")]
    pub fn capture_removal_raft(
        &self,
        raft_id: u64,
    ) -> Result<ClockRemovalCapture<'_>, ClockRefusal> {
        self.capture_removal(RemovalTarget::Raft(raft_id))
    }

    #[cfg(feature = "hiqlite-store")]
    fn capture_removal(
        &self,
        target: RemovalTarget,
    ) -> Result<ClockRemovalCapture<'_>, ClockRefusal> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let generation = inner.snapshot.clock_generation;
        let before = Instant::now();
        let decision = Self::continuity(&mut inner, before, wall_ms(), Instant::now());
        let membership = inner.membership.clone();
        let directory = inner.peer_directory.clone();
        let strict = (|| {
            if decision.clock_generation != generation {
                return Err(ClockRefusal::LocalDiscontinuity);
            }
            let membership = membership.as_ref().ok_or(ClockRefusal::Unknown)?;
            directory.as_ref().ok_or(ClockRefusal::Unknown)?;
            if !inner.roster_proved
                || membership.members.len() > 64
                || !membership.members.contains(&membership.local_node)
                || inner.snapshot.peers.len() != membership.members.len().saturating_sub(1)
            {
                return Err(ClockRefusal::Unknown);
            }
            Ok(())
        })();
        // Uncounted: the counted consumer boundary is `admit_fenced_removal`.
        self.decide(None, strict)?;
        let reachability_stable = inner.last_discontinuity.is_none_or(|changed| {
            before
                .checked_duration_since(changed)
                .is_some_and(|age| age >= REMOVAL_REACHABILITY_STABILIZATION)
        });
        Ok(ClockRemovalCapture {
            guard: self,
            decision,
            admission_generation: inner.admission_generation,
            membership,
            peers: inner.snapshot.peers.clone(),
            directory,
            target,
            captured_at: before,
            reachability_stable,
        })
    }

    /// While enforcement is off this always admits (uncounted).
    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn revalidate_removal_capture(
        &self,
        captured: &ClockRemovalCapture<'_>,
    ) -> Result<(), ClockRefusal> {
        self.decide(None, self.revalidate_removal_capture_strict(captured))
    }

    #[cfg(feature = "hiqlite-store")]
    fn revalidate_removal_capture_strict(
        &self,
        captured: &ClockRemovalCapture<'_>,
    ) -> Result<(), ClockRefusal> {
        if !std::ptr::eq(self, captured.guard) {
            return Err(ClockRefusal::GenerationChanged);
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let now = Instant::now();
        let current = Self::continuity(&mut inner, now, wall_ms(), Instant::now());
        if current.clock_generation != captured.decision.clock_generation {
            return Err(ClockRefusal::LocalDiscontinuity);
        }
        if inner.admission_generation != captured.admission_generation
            || captured.membership.is_none()
            || captured.directory.is_none()
            || inner.membership != captured.membership
            || inner.peer_directory != captured.directory
        {
            return Err(ClockRefusal::GenerationChanged);
        }
        if !inner.roster_proved
            || !now
                .checked_duration_since(captured.captured_at)
                .is_some_and(|age| age <= CLOCK_OBSERVATION_MAX_AGE)
        {
            return Err(ClockRefusal::Unknown);
        }
        Ok(())
    }

    /// Final local redemption of an original observation and actual fenced
    /// target. Called only before a new proposal, never for its reconciliation.
    #[cfg(feature = "hiqlite-store")]
    pub(super) fn admit_fenced_removal(
        &self,
        captured: &ClockRemovalCapture<'_>,
        fence: &super::membership::AppliedRemovalFence,
    ) -> Result<(), ClockRefusal> {
        let reference = fence.reference();
        // Target identity is not clock evidence: a fence for another target
        // is never redeemable, enforced or not.
        if !captured.matches_target(&reference.target_node_id, reference.target_raft_id) {
            return Err(ClockRefusal::GenerationChanged);
        }
        let result = (|| {
            self.revalidate_removal_capture_strict(captured)?;
            if captured.remaining_removal_budget().is_none() {
                return Err(ClockRefusal::Unknown);
            }
            let membership = captured
                .membership
                .as_ref()
                .ok_or(ClockRefusal::GenerationChanged)?;
            if !membership.members.contains(&reference.target_raft_id)
                || (reference.target_raft_id != membership.local_node
                    && !captured.peers.contains_key(&reference.target_node_id))
            {
                return Err(ClockRefusal::GenerationChanged);
            }
            if fence.relies_on_wall_reachability() && !captured.permits_wall_reachability() {
                return Err(ClockRefusal::LocalDiscontinuity);
            }
            let now = Instant::now();
            for (node_id, observation) in &captured.peers {
                if node_id == &reference.target_node_id {
                    continue;
                }
                let fresh = match observation {
                    PeerClockOffset::Bounded { observed_at, .. } => now
                        .checked_duration_since(*observed_at)
                        .is_some_and(|age| age <= CLOCK_OBSERVATION_MAX_AGE),
                    PeerClockOffset::Unknown => false,
                };
                let Some(upper) = Self::upper_bound(observation).filter(|_| fresh) else {
                    // The coverage rule: an unobserved surviving learner
                    // holds no vote and does not refuse the reduction. An
                    // unrelated unobserved VOTER is never excused.
                    if Self::is_learner_peer(
                        captured.membership.as_ref(),
                        captured.directory.as_ref(),
                        node_id,
                    ) {
                        continue;
                    }
                    return Err(ClockRefusal::Unknown);
                };
                if upper > CLOCK_OFFSET_REFUSAL_MS * 1_000 {
                    return Err(ClockRefusal::Offset);
                }
            }
            Ok(())
        })();
        self.decide(Some(ClockDecision::MembershipChange), result)
    }

    /// Whether this original capture may use a wall-age `last_seen_at`
    /// comparison as authoritative-unreachable target evidence. Inside the
    /// post-step stabilization window the strict answer is
    /// `LocalDiscontinuity`; while enforcement is off it admits, exactly as
    /// every other consumer does. Uncounted: the counted boundary is
    /// `admit_fenced_removal`, which re-checks this same fact, so an advisory
    /// admission is counted there once rather than once per poll.
    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn admit_wall_reachability(
        &self,
        captured: &ClockRemovalCapture<'_>,
    ) -> Result<(), ClockRefusal> {
        self.decide(None, Self::wall_reachability_strict(captured))
    }

    /// Counted final refusal for an original removal budget that ended while
    /// the only available target evidence was a wall-age comparison the
    /// capture may not use. Admits and counts nothing, advisory or not, if
    /// enforcement was turned off meanwhile or the capture is stable: the
    /// operation then fails as the plain deadline it is, and an advisory
    /// refusal for it would count a decision nobody admitted.
    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn refuse_unstable_wall_reachability(
        &self,
        captured: &ClockRemovalCapture<'_>,
    ) -> Result<(), ClockRefusal> {
        match Self::wall_reachability_strict(captured) {
            Err(cause) if self.is_enforced() => {
                self.refusals[ClockDecision::MembershipChange.index()][cause.index()]
                    .fetch_add(1, Ordering::Relaxed);
                Err(cause)
            }
            _ => Ok(()),
        }
    }

    #[cfg(feature = "hiqlite-store")]
    fn wall_reachability_strict(captured: &ClockRemovalCapture<'_>) -> Result<(), ClockRefusal> {
        if captured.permits_wall_reachability() {
            Ok(())
        } else {
            Err(ClockRefusal::LocalDiscontinuity)
        }
    }

    /// Close both generation races after awaited preparation. This is a local
    /// synchronous check only; it cannot establish whether a submitted CAS won.
    /// While enforcement is off this always admits (uncounted).
    pub fn revalidate(&self, ticket: &ClockAcquisitionTicket<'_>) -> Result<(), ClockRefusal> {
        self.decide(None, self.revalidate_strict(ticket))
    }

    /// Decision-relevant evidence only: the exact guard, unchanged local
    /// continuity, unchanged admission generation, and the CURRENT policy.
    /// A completed measurement round alone never invalidates the ticket.
    fn revalidate_strict(&self, ticket: &ClockAcquisitionTicket<'_>) -> Result<(), ClockRefusal> {
        self.revalidate_strict_with(ticket, |_| Ok(()))
    }

    /// `revalidate_strict` plus a decision-specific check read under the SAME
    /// serialized state, so the two cannot describe different generations.
    fn revalidate_strict_with(
        &self,
        ticket: &ClockAcquisitionTicket<'_>,
        also: impl FnOnce(&ClockInner) -> Result<(), ClockRefusal>,
    ) -> Result<(), ClockRefusal> {
        if !std::ptr::eq(self, ticket.guard) {
            return Err(ClockRefusal::GenerationChanged);
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let before = Instant::now();
        let current = Self::continuity(&mut inner, before, wall_ms(), Instant::now());
        if current.clock_generation != ticket.decision.clock_generation {
            return Err(ClockRefusal::LocalDiscontinuity);
        }
        if inner.admission_generation != ticket.admission_generation {
            return Err(ClockRefusal::GenerationChanged);
        }
        Self::acquisition_policy(&inner)?;
        also(&inner)
    }

    /// A learner's clock is excused from coverage only while it holds no
    /// vote. Promotion makes it a voter, so the promoted node itself must
    /// have a fresh bounded observation (or be this node, whose continuity
    /// the ticket already proves). An unmapped target is Unknown.
    fn promotion_target_policy(inner: &ClockInner, target_raft: u64) -> Result<(), ClockRefusal> {
        let membership = inner.membership.as_ref().ok_or(ClockRefusal::Unknown)?;
        if target_raft == membership.local_node {
            return Ok(());
        }
        let directory = inner.peer_directory.as_ref().ok_or(ClockRefusal::Unknown)?;
        let (node_id, _) = directory
            .iter()
            .find(|(_, (raft_id, _))| *raft_id == target_raft)
            .ok_or(ClockRefusal::Unknown)?;
        // `continuity` has already expired stale samples to Unknown.
        let upper = inner
            .snapshot
            .peers
            .get(node_id)
            .and_then(Self::upper_bound)
            .ok_or(ClockRefusal::Unknown)?;
        if upper > CLOCK_OFFSET_REFUSAL_MS * 1_000 {
            return Err(ClockRefusal::Offset);
        }
        Ok(())
    }

    /// `admit_for(MembershipChange, ..)` for promoting one learner to voter:
    /// the ordinary policy plus a bounded observation of that learner, read
    /// under one lock. Same original-refusal and advisory semantics.
    pub fn admit_promotion_for<'guard>(
        &'guard self,
        target_raft: u64,
        prepared: Result<ClockAcquisitionTicket<'guard>, ClockRefusal>,
    ) -> Result<ClockAcquisitionTicket<'guard>, ClockRefusal> {
        let strict = match &prepared {
            Ok(ticket) => self.revalidate_strict_with(ticket, |inner| {
                Self::promotion_target_policy(inner, target_raft)
            }),
            Err(cause) => Err(*cause),
        };
        self.admit_decided(ClockDecision::MembershipChange, prepared, strict)
    }

    /// Re-check before submitting a promotion, counting a real refusal.
    /// One promotion attempt counts at most one ADVISORY refusal: the ticket
    /// remembers that its admission (or an earlier re-check) already counted
    /// one, so the attempt's later re-checks admit without counting again.
    /// An enforced refusal is always counted; it ends the attempt.
    pub fn revalidate_promotion_for(
        &self,
        target_raft: u64,
        ticket: &mut ClockAcquisitionTicket<'_>,
    ) -> Result<(), ClockRefusal> {
        let strict = self.revalidate_strict_with(ticket, |inner| {
            Self::promotion_target_policy(inner, target_raft)
        });
        if strict.is_err() && ticket.advisory_counted && !self.is_enforced() {
            return Ok(());
        }
        let refused = strict.is_err();
        let result = self.decide(Some(ClockDecision::MembershipChange), strict);
        ticket.advisory_counted |= refused;
        result
    }

    /// Actual consumer entry only; pure policy inspection remains uncounted.
    /// Consume a proof captured before awaited identity/preflight reads. An
    /// idempotent already-committed operation can discard that pure result;
    /// new authority must not replace an original refusal with a fresh proof.
    /// While enforcement is off an originally refused capture is replaced by
    /// a current ticket (counted as an advisory refusal), as if unguarded.
    pub fn admit_for<'guard>(
        &'guard self,
        decision: ClockDecision,
        prepared: Result<ClockAcquisitionTicket<'guard>, ClockRefusal>,
    ) -> Result<ClockAcquisitionTicket<'guard>, ClockRefusal> {
        let strict = match &prepared {
            Ok(ticket) => self.revalidate_strict(ticket),
            Err(cause) => Err(*cause),
        };
        self.admit_decided(decision, prepared, strict)
    }

    fn admit_decided<'guard>(
        &'guard self,
        decision: ClockDecision,
        prepared: Result<ClockAcquisitionTicket<'guard>, ClockRefusal>,
        strict: Result<(), ClockRefusal>,
    ) -> Result<ClockAcquisitionTicket<'guard>, ClockRefusal> {
        let admitted = strict.is_ok();
        self.decide(Some(decision), strict)?;
        match prepared {
            Ok(ticket) if admitted => Ok(ticket),
            // Advisory admission: bind the operation to current generations
            // so one stale capture is not re-counted at every later check,
            // and remember that this attempt's advisory refusal is counted.
            _ => Ok(ClockAcquisitionTicket {
                advisory_counted: true,
                ..self.capture().0
            }),
        }
    }

    /// Actual consumer entry only; pure policy inspection remains uncounted.
    pub fn acquire_for(
        &self,
        decision: ClockDecision,
    ) -> Result<ClockAcquisitionTicket<'_>, ClockRefusal> {
        let (ticket, strict) = self.capture();
        self.decide(Some(decision), strict)?;
        Ok(ticket)
    }

    pub fn acquire_owned_for(
        self: &Arc<Self>,
        consumer: ClockDecision,
    ) -> Result<OwnedClockAcquisitionTicket, ClockRefusal> {
        let ticket = self.acquire_for(consumer)?;
        Ok(OwnedClockAcquisitionTicket {
            guard: Arc::clone(self),
            decision: ticket.decision,
            admission_generation: ticket.admission_generation,
            consumer,
            owner: None,
        })
    }

    /// `acquire_owned_for` for a decision that spends one node's lease
    /// (takeover of a route that `owner_node_id` holds). Besides the ordinary
    /// policy, the owner itself must be bounded: see [`Self::owner_policy`].
    /// The owner is re-checked by every `revalidate` of the returned ticket.
    pub fn acquire_owned_for_owner(
        self: &Arc<Self>,
        consumer: ClockDecision,
        owner_node_id: &str,
    ) -> Result<OwnedClockAcquisitionTicket, ClockRefusal> {
        let (ticket, strict) = self.capture_with(|inner| Self::owner_policy(inner, owner_node_id));
        self.decide(Some(consumer), strict)?;
        Ok(OwnedClockAcquisitionTicket {
            guard: Arc::clone(self),
            decision: ticket.decision,
            admission_generation: ticket.admission_generation,
            consumer,
            owner: Some(owner_node_id.to_owned()),
        })
    }

    /// Per-route owner check for a decision already admitted as a whole (the
    /// expiry scan's page): may this caller spend the lease `owner_node_id`
    /// holds? Reads the CURRENT evidence, so a membership change or an owner
    /// that has since gone unobserved refuses. Counted per call under
    /// `decision`; advisory mode admits and counts, exactly like the page.
    pub fn admit_owner_for(
        &self,
        decision: ClockDecision,
        ticket: &ClockAcquisitionTicket<'_>,
        owner_node_id: &str,
    ) -> Result<(), ClockRefusal> {
        let strict = if std::ptr::eq(self, ticket.guard) {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.refresh_membership(&mut inner);
            Self::expire(&mut inner, Instant::now());
            Self::recompute(&mut inner);
            Self::owner_policy(&inner, owner_node_id)
        } else {
            Err(ClockRefusal::GenerationChanged)
        };
        self.decide(Some(decision), strict)
    }

    /// A lease's expiry was written by its owner's clock, so a decision that
    /// spends it (takeover, the expiry scan) needs the OWNER bounded, not just
    /// coverage. Coverage excuses an unobserved learner because it holds no
    /// vote, but a learner owns delegated media sessions: an unobserved (or
    /// out-of-bound) owner is never contested, until it is measured within
    /// the bound or leaves the committed roster. An owner absent from the
    /// proved roster is this node or not a member of the membership this node
    /// applied (a removed node, whose renewals the removal fence refuses).
    /// An unproved roster refuses: absence then proves nothing.
    fn owner_policy(inner: &ClockInner, owner_node_id: &str) -> Result<(), ClockRefusal> {
        if !inner.roster_proved {
            return Err(ClockRefusal::Unknown);
        }
        let Some(observation) = inner.snapshot.peers.get(owner_node_id) else {
            return Ok(());
        };
        match Self::upper_bound(observation) {
            None => Err(ClockRefusal::Unknown),
            Some(upper) if upper > CLOCK_OFFSET_REFUSAL_MS * 1_000 => Err(ClockRefusal::Offset),
            Some(_) => Ok(()),
        }
    }

    /// Preserve the original caller time across awaits, counting only a real
    /// refusal to submit. Never use this to abandon a commit-unknown proposal.
    pub fn revalidate_for(
        &self,
        decision: ClockDecision,
        ticket: &ClockAcquisitionTicket<'_>,
    ) -> Result<(), ClockRefusal> {
        self.decide(Some(decision), self.revalidate_strict(ticket))
    }

    fn acquisition_policy(inner: &ClockInner) -> Result<(), ClockRefusal> {
        match inner.snapshot.state {
            ClusterClockState::NoPeers if inner.roster_proved => Ok(()),
            ClusterClockState::Bounded { worst_abs_upper_us }
                if worst_abs_upper_us <= CLOCK_OFFSET_REFUSAL_MS * 1_000 =>
            {
                Ok(())
            }
            ClusterClockState::Bounded { .. } => Err(ClockRefusal::Offset),
            _ => Err(ClockRefusal::Unknown),
        }
    }

    fn continuity(
        inner: &mut ClockInner,
        before: Instant,
        wall: Option<i64>,
        after: Instant,
    ) -> ClockDecisionTicket {
        let valid = inner
            .anchor_wall_ms
            .zip(wall)
            .is_some_and(|(anchor, current)| {
                let delta = i128::from(current) - i128::from(anchor);
                let lower = before
                    .saturating_duration_since(inner.anchor_mono)
                    .as_millis() as i128
                    - i128::from(CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS);
                let upper = after
                    .saturating_duration_since(inner.anchor_mono)
                    .as_millis() as i128
                    + i128::from(CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS);
                (lower..=upper).contains(&delta)
            });
        if !valid {
            inner.last_discontinuity = Some(after);
            inner.snapshot.clock_generation += 1;
            inner.snapshot.state_generation += 1;
            inner.admission_generation += 1;
            inner.snapshot.discontinuities += 1;
            for peer in inner.snapshot.peers.values_mut() {
                *peer = PeerClockOffset::Unknown;
            }
            inner.roster_proved = false;
            inner.roster_observed_at = None;
            inner.snapshot.readiness = ClockReadiness::default();
            inner.anchor_wall_ms = wall;
            inner.anchor_mono = before;
        }
        Self::expire(inner, after);
        Self::recompute(inner);
        ClockDecisionTicket {
            clock_generation: inner.snapshot.clock_generation,
            state_generation: inner.snapshot.state_generation,
            now_ms: wall.unwrap_or_default(),
        }
    }

    fn expire(inner: &mut ClockInner, now: Instant) {
        let mut changed = false;
        if !inner.standalone
            && inner.roster_proved
            && !inner.roster_observed_at.is_some_and(|observed| {
                now.checked_duration_since(observed)
                    .is_some_and(|age| age <= CLOCK_OBSERVATION_MAX_AGE)
            })
        {
            inner.roster_proved = false;
            changed = true;
        }
        for peer in inner.snapshot.peers.values_mut() {
            if matches!(peer, PeerClockOffset::Bounded { observed_at, .. } if !now.checked_duration_since(*observed_at).is_some_and(|age| age <= CLOCK_OBSERVATION_MAX_AGE))
            {
                *peer = PeerClockOffset::Unknown;
                changed = true;
            }
        }
        if changed {
            inner.snapshot.state_generation += 1;
            inner.snapshot.readiness = ClockReadiness::default();
        }
    }

    /// A committed non-voting learner: its exact applied-directory Raft id is
    /// a member and not a voter. Without an exact peer directory bound to the
    /// applied membership watch the role cannot be proved, so every peer is
    /// treated as a voter and fails closed.
    fn is_learner_peer(
        membership: Option<&ClockMembershipIdentity>,
        directory: Option<&BTreeMap<String, (u64, String)>>,
        node_id: &str,
    ) -> bool {
        let (Some(membership), Some(directory)) = (membership, directory) else {
            return false;
        };
        directory.get(node_id).is_some_and(|(raft_id, _)| {
            membership.members.contains(raft_id) && !membership.voters.contains(raft_id)
        })
    }

    /// `|offset| + uncertainty` of a usable observation; None is Unknown.
    fn upper_bound(observation: &PeerClockOffset) -> Option<i64> {
        match observation {
            PeerClockOffset::Unknown => None,
            PeerClockOffset::Bounded {
                offset_us,
                uncertainty_us,
                ..
            } => offset_us
                .checked_abs()
                .and_then(|value| value.checked_add(*uncertainty_us))
                .filter(|_| *uncertainty_us >= 0),
        }
    }

    fn recompute(inner: &mut ClockInner) {
        let mut unknown = 0;
        let mut unobserved_learners = 0;
        let mut worst = 0;
        for (node_id, peer) in &inner.snapshot.peers {
            match Self::upper_bound(peer) {
                Some(value) => worst = worst.max(value),
                // An unobserved learner is reported, not coverage: it holds
                // no vote, and a measured learner still counts in `worst`.
                None if Self::is_learner_peer(
                    inner.membership.as_ref(),
                    inner.peer_directory.as_ref(),
                    node_id,
                ) =>
                {
                    unobserved_learners += 1;
                }
                None => unknown += 1,
            }
        }
        inner.snapshot.unobserved_learners = unobserved_learners;
        inner.snapshot.state = if !inner.roster_proved || unknown > 0 {
            ClusterClockState::Incomplete {
                unknown_peers: unknown.max(usize::from(!inner.roster_proved)),
                worst_abs_upper_us: worst,
            }
        } else if inner.snapshot.peers.is_empty() {
            ClusterClockState::NoPeers
        } else {
            ClusterClockState::Bounded {
                worst_abs_upper_us: worst,
            }
        };
    }

    /// Install the exact committed roster before starting fanout. New members are Unknown.
    pub fn roster(&self, peers: &[String]) -> ClockDecisionTicket {
        self.roster_for_membership(peers, None).unwrap_or_else(|_| {
            self.roster_failed();
            self.ticket()
        })
    }

    /// UUID mapping must have been resolved against this exact applied watch
    /// entry, including an after-await comparison by the membership adapter.
    pub fn roster_for_membership(
        &self,
        peers: &[String],
        membership: Option<&ClockMembershipIdentity>,
    ) -> Result<ClockDecisionTicket, ClockRefusal> {
        self.roster_with_directory(peers, membership, None)
    }

    /// Bind a complete measured round to the exact applied peer directory.
    /// A changed address/UUID/Raft mapping discards old samples, not their age.
    #[cfg(feature = "hiqlite-store")]
    pub fn roster_for_peer_directory(
        &self,
        roster: &super::membership::ClockPeerRoster,
    ) -> Result<ClockDecisionTicket, ClockRefusal> {
        let directory = Self::peer_directory(roster).inspect_err(|_| self.roster_failed())?;
        let peers: Vec<_> = directory.keys().cloned().collect();
        self.roster_with_directory(&peers, roster.membership.as_ref(), Some(directory))
    }

    #[cfg(feature = "hiqlite-store")]
    fn peer_directory(
        roster: &super::membership::ClockPeerRoster,
    ) -> Result<BTreeMap<String, (u64, String)>, ClockRefusal> {
        let membership = roster.membership.as_ref().ok_or(ClockRefusal::Unknown)?;
        if membership.members.len() > 64
            || !membership.members.contains(&membership.local_node)
            || roster.peers.len() != membership.members.len().saturating_sub(1)
        {
            return Err(ClockRefusal::Unknown);
        }
        let mut directory = BTreeMap::new();
        let mut raft_ids = BTreeSet::new();
        for peer in &roster.peers {
            let origin = peer
                .http_base
                .as_deref()
                .filter(|value| value.len() <= 2048)
                .and_then(super::membership::normalize_internal_http_base)
                .ok_or(ClockRefusal::Unknown)?;
            if peer.node_id.len() != 36
                || uuid::Uuid::parse_str(&peer.node_id)
                    .ok()
                    .is_none_or(|id| id.to_string() != peer.node_id)
                || peer.raft_id == membership.local_node
                || !membership.members.contains(&peer.raft_id)
                || !raft_ids.insert(peer.raft_id)
                || directory
                    .insert(peer.node_id.clone(), (peer.raft_id, origin))
                    .is_some()
            {
                return Err(ClockRefusal::Unknown);
            }
        }
        Ok(directory)
    }

    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn revalidate_removal_directory(
        &self,
        captured: &ClockRemovalCapture<'_>,
        roster: &super::membership::ClockPeerRoster,
        target_node: &str,
        target_raft: u64,
        local_node: &str,
    ) -> Result<(), ClockRefusal> {
        let strict = (|| {
            let (Some(membership), Some(directory)) =
                (captured.membership.as_ref(), captured.directory.as_ref())
            else {
                return Err(ClockRefusal::GenerationChanged);
            };
            if roster.membership.as_ref() != Some(membership)
                || Self::peer_directory(roster)? != *directory
                || if target_raft == membership.local_node {
                    target_node != local_node
                } else {
                    directory.get(target_node).map(|entry| entry.0) != Some(target_raft)
                }
            {
                return Err(ClockRefusal::GenerationChanged);
            }
            self.revalidate_removal_capture_strict(captured)
        })();
        self.decide(None, strict)
    }

    fn roster_with_directory(
        &self,
        peers: &[String],
        membership: Option<&ClockMembershipIdentity>,
        directory: Option<BTreeMap<String, (u64, String)>>,
    ) -> Result<ClockDecisionTicket, ClockRefusal> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let before = Instant::now();
        let wall = wall_ms();
        let ticket = Self::continuity(&mut inner, before, wall, Instant::now());
        if self.membership_source.is_some()
            && (inner.membership.is_none() || membership != inner.membership.as_ref())
        {
            // A caller cannot prove coverage with a bare empty UUID directory
            // or refresh evidence for an older membership after an ABA change.
            return Err(ClockRefusal::GenerationChanged);
        }
        if self.membership_source.is_some()
            && membership.is_some_and(|identity| {
                !identity.members.contains(&identity.local_node)
                    || !identity.voters.is_subset(&identity.members)
                    || peers.len() != identity.members.len().saturating_sub(1)
            })
        {
            return Err(ClockRefusal::Unknown);
        }
        let directory_changed = inner.peer_directory != directory;
        let changed = !inner.roster_proved
            || directory_changed
            || inner.snapshot.peers.keys().ne(peers.iter());
        if changed {
            if directory_changed {
                inner.snapshot.peers.clear();
            }
            inner.peer_directory = directory;
            inner.snapshot.peers.retain(|id, _| peers.contains(id));
            for id in peers {
                inner
                    .snapshot
                    .peers
                    .entry(id.clone())
                    .or_insert(PeerClockOffset::Unknown);
            }
            inner.roster_proved = true;
            inner.snapshot.state_generation += 1;
            inner.admission_generation += 1;
            inner.snapshot.readiness = ClockReadiness::default();
        }
        inner.roster_observed_at = Some(Instant::now());
        Self::recompute(&mut inner);
        Ok(ClockDecisionTicket {
            state_generation: inner.snapshot.state_generation,
            ..ticket
        })
    }

    /// A round publishes atomically only if continuity and roster remained identical.
    pub fn publish(
        &self,
        ticket: ClockDecisionTicket,
        peers: BTreeMap<String, PeerClockOffset>,
    ) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        let before = Instant::now();
        let wall = wall_ms();
        let current = Self::continuity(&mut inner, before, wall, Instant::now());
        if !inner.roster_proved
            || current.clock_generation != ticket.clock_generation
            || current.state_generation != ticket.state_generation
            || inner.snapshot.clock_generation != ticket.clock_generation
            || inner.snapshot.state_generation != ticket.state_generation
        {
            // An obsolete round has no authority to invalidate newer evidence.
            return false;
        }
        if inner.snapshot.peers.keys().ne(peers.keys()) {
            // A failed current round must invalidate proofs before releasing the lock.
            inner.roster_proved = false;
            inner.roster_observed_at = None;
            for peer in inner.snapshot.peers.values_mut() {
                *peer = PeerClockOffset::Unknown;
            }
            inner.snapshot.state_generation += 1;
            inner.admission_generation += 1;
            inner.snapshot.unknown_rounds += 1;
            inner.snapshot.readiness = ClockReadiness::default();
            Self::recompute(&mut inner);
            return false;
        }
        let failed = peers
            .values()
            .any(|peer| matches!(peer, PeerClockOffset::Unknown));
        inner.snapshot.peers = peers;
        if failed {
            inner.snapshot.unknown_rounds += 1;
        }
        // Every completed round advances the measurement generation. It does
        // not touch the admission generation: a ticket is re-checked against
        // the current policy, so a newly failed peer still refuses (enforced).
        inner.snapshot.state_generation += 1;
        Self::expire(&mut inner, Instant::now());
        Self::recompute(&mut inner);
        inner.snapshot.readiness.consecutive_violating_rounds = match inner.snapshot.state {
            ClusterClockState::Bounded { worst_abs_upper_us }
                if worst_abs_upper_us > CLOCK_OFFSET_REFUSAL_MS * 1_000 =>
            {
                inner
                    .snapshot
                    .readiness
                    .consecutive_violating_rounds
                    .saturating_add(1)
                    .min(2)
            }
            _ => 0,
        };
        true
    }

    pub fn roster_failed(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.roster_proved = false;
        inner.roster_observed_at = None;
        inner.peer_directory = None;
        inner.snapshot.readiness = ClockReadiness::default();
        for peer in inner.snapshot.peers.values_mut() {
            *peer = PeerClockOffset::Unknown;
        }
        inner.snapshot.state_generation += 1;
        inner.admission_generation += 1;
        inner.snapshot.unknown_rounds += 1;
        Self::recompute(&mut inner);
    }

    #[must_use]
    pub fn snapshot(&self) -> ClockSnapshot {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_membership(&mut inner);
        Self::expire(&mut inner, Instant::now());
        Self::recompute(&mut inner);
        inner.snapshot.clone()
    }

    /// Shift the fixed continuity anchor so the next serialized read observes
    /// a local wall-clock step, as the in-module fixtures do directly.
    #[cfg(all(test, feature = "hiqlite-store"))]
    pub(crate) fn simulate_wall_step_for_test(&self, step_ms: i64) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(anchor) = inner.anchor_wall_ms.as_mut() {
            *anchor += step_ms;
        }
    }

    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn record_authority_read(&self) {
        self.authority_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Passive scrape: no continuity sampling or Store/network IO. At most
    /// an in-process Raft watch read invalidates obsolete membership coverage.
    #[must_use]
    pub fn prometheus(&self) -> String {
        let snapshot = self.snapshot();
        let mut out = String::from("# HELP plurx_cluster_clock_observation_state Per-peer offset observation state.\n# TYPE plurx_cluster_clock_observation_state gauge\n# HELP plurx_cluster_clock_offset_seconds Measured clock offset to a committed peer.\n# TYPE plurx_cluster_clock_offset_seconds gauge\n# HELP plurx_cluster_clock_offset_uncertainty_seconds Filtered network half-delay plus quantization.\n# TYPE plurx_cluster_clock_offset_uncertainty_seconds gauge\n");
        for (id, observation) in &snapshot.peers {
            let peer = id
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            let bounded = matches!(observation, PeerClockOffset::Bounded { .. });
            out.push_str(&format!("plurx_cluster_clock_observation_state{{peer=\"{peer}\",state=\"bounded\"}} {}\nplurx_cluster_clock_observation_state{{peer=\"{peer}\",state=\"unknown\"}} {}\n", u8::from(bounded), u8::from(!bounded)));
            if let PeerClockOffset::Bounded {
                offset_us,
                uncertainty_us,
                ..
            } = observation
            {
                out.push_str(&format!("plurx_cluster_clock_offset_seconds{{peer=\"{peer}\"}} {}\nplurx_cluster_clock_offset_uncertainty_seconds{{peer=\"{peer}\"}} {}\n", *offset_us as f64 / 1_000_000.0, *uncertainty_us as f64 / 1_000_000.0));
            }
        }
        out.push_str(&format!("# HELP plurx_cluster_clock_unobserved_learners Committed non-voting learners with no usable observation; reported, not required for coverage.\n# TYPE plurx_cluster_clock_unobserved_learners gauge\nplurx_cluster_clock_unobserved_learners {}\n", snapshot.unobserved_learners));
        out.push_str(&format!("# HELP plurx_cluster_clock_discontinuities_total Local wall/monotonic discontinuities observed.\n# TYPE plurx_cluster_clock_discontinuities_total counter\nplurx_cluster_clock_discontinuities_total {}\n# HELP plurx_cluster_clock_unknown_rounds_total Completed failed or incomplete observation rounds.\n# TYPE plurx_cluster_clock_unknown_rounds_total counter\nplurx_cluster_clock_unknown_rounds_total {}\n# HELP plurx_cluster_clock_authority_reads_total Consistent authority reads attributable to inbound clock requests.\n# TYPE plurx_cluster_clock_authority_reads_total counter\nplurx_cluster_clock_authority_reads_total {}\n# HELP plurx_cluster_clock_refusals_total Decisions refused because the clock could not be bounded (measurement-only emits zero).\n# TYPE plurx_cluster_clock_refusals_total counter\n", snapshot.discontinuities, snapshot.unknown_rounds, self.authority_reads.load(Ordering::Relaxed)));
        for decision in ClockDecision::ALL {
            for cause in ClockRefusal::ALL {
                let count = self.refusals[decision.index()][cause.index()].load(Ordering::Relaxed);
                let decision = decision.label();
                let cause = cause.label();
                out.push_str(&format!("plurx_cluster_clock_refusals_total{{decision=\"{decision}\",cause=\"{cause}\"}} {count}\n"));
            }
        }
        out.push_str("# HELP plurx_cluster_clock_advisory_refusals_total Decisions that enforcement would have refused while it was off (admitted anyway).\n# TYPE plurx_cluster_clock_advisory_refusals_total counter\n");
        for decision in ClockDecision::ALL {
            for cause in ClockRefusal::ALL {
                let count =
                    self.advisory_refusals[decision.index()][cause.index()].load(Ordering::Relaxed);
                let decision = decision.label();
                let cause = cause.label();
                out.push_str(&format!("plurx_cluster_clock_advisory_refusals_total{{decision=\"{decision}\",cause=\"{cause}\"}} {count}\n"));
            }
        }
        out.push_str(&format!("# HELP plurx_cluster_clock_enforced Whether the cluster clock guard refuses decisions (1) or is advisory only (0).\n# TYPE plurx_cluster_clock_enforced gauge\nplurx_cluster_clock_enforced {}\n", u8::from(self.is_enforced())));
        out
    }
}
