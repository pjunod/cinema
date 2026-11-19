//! Shared cluster timing policy. Consumer aliases retain their existing APIs.
//!
//! Durations are the deployed values, not a new failover promise. In particular,
//! the 16 s leader-recovery budget exceeds the normal 12 s media lease; the
//! measured C4 decision must resolve that mismatch before either is tuned.
//! See `docs/cluster/CLUSTER-TIMING-BUILD.md` for ownership and qualification.

/// Raft sender and election configuration; rolling-update floors are measured policy.
pub mod raft {
    use std::time::Duration;

    pub const HIQLITE_HEARTBEAT_INTERVAL_MS: u64 = 800;
    pub const HIQLITE_ELECTION_TIMEOUT_MIN_MS: u64 = HIQLITE_HEARTBEAT_INTERVAL_MS * 3;
    pub const HIQLITE_ELECTION_TIMEOUT_MAX_MS: u64 = HIQLITE_HEARTBEAT_INTERVAL_MS * 5;
    pub const REPLICATED_LEADER_RECOVERY_BUDGET: Duration = Duration::from_secs(16);
}

/// Replica observations, proof freshness and local serving grace are distinct bounds.
pub mod serving {
    use std::time::Duration;

    pub const PASSIVE_METRICS_REFRESH: Duration = Duration::from_secs(5);
    pub const PASSIVE_METRICS_FRESHNESS_SECS: u64 = 15;
    pub const QUORUM_WATERMARK_REFRESH: Duration = Duration::from_millis(500);
    pub const QUORUM_WATERMARK_TIMEOUT: Duration = Duration::from_millis(750);
    pub const QUORUM_WATERMARK_LEASE: Duration = Duration::from_secs(1);
    pub const SERVING_FENCE_POLL: Duration = Duration::from_millis(25);
    pub const SERVING_FENCE_SESSION_GRACE: Duration = Duration::from_secs(5);
}

/// One attempt, bounded retries and end-to-end recovery keep separate owners.
pub mod store {
    use std::time::Duration;

    pub const STORE_TIMEOUT: Duration = Duration::from_secs(3);
    pub const AUTHORITY_READ_TIMEOUT_RETRY_DELAY: Duration = Duration::from_millis(100);
    pub const AUTHORITY_READ_TIMEOUT_MAX_ATTEMPTS: usize = 2;
    pub const AUTHORITY_QUORUM_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(100);
    pub const AUTHORITY_QUORUM_RETRY_MAX_BACKOFF: Duration = Duration::from_millis(800);
    pub const AUTHORITY_QUORUM_RETRY_JITTER_RANGE_MILLIS: u8 = 100;
    pub const AUTHORITY_QUORUM_RETRY_MAX_DELAY: Duration = Duration::from_millis(
        AUTHORITY_QUORUM_RETRY_MAX_BACKOFF.as_millis() as u64
            + AUTHORITY_QUORUM_RETRY_JITTER_RANGE_MILLIS as u64
            - 1,
    );
    pub const AUTHORITY_QUORUM_RECOVERY_BUDGET: Duration = Duration::from_secs(5);
    pub const IDEMPOTENT_WRITE_RETRY_DELAY: Duration = Duration::from_millis(100);
    pub const IDEMPOTENT_WRITE_MAX_ATTEMPTS: usize = 5;
}

/// Membership observations, liveness, pool and planned handoff bounds.
pub mod membership {
    use std::time::Duration;

    pub const NODE_REACHABLE_WINDOW_MS: i64 = HEARTBEAT_INTERVAL.as_millis() as i64 * 3;
    pub const PROTOCOL_CHANGE_ABSENCE_WINDOW_MS: i64 = HEARTBEAT_INTERVAL.as_millis() as i64 * 12;
    pub const ARTWORK_LEADER_QUORUM_FRESH_MS: u64 = 1_000;
    pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
    pub const HEARTBEAT_COALESCE_WINDOW: Duration = Duration::from_millis(250);
    pub const PROBE_WAIT: Duration = Duration::from_secs(15);
    pub const PROBE_POLL: Duration = Duration::from_millis(500);
    pub const PROBE_REQUEST_TTL: i64 = 300;
    pub const TRANSFER_ACTIVE_SECONDS: i64 = 60;
    pub const SURVIVOR_LEADER_WAIT: Duration = Duration::from_secs(8);
    pub const SHUTDOWN_HANDOFF_WAIT: Duration =
        SURVIVOR_LEADER_WAIT.saturating_add(Duration::from_secs(2));
    pub const PROPOSAL_HOLD_WAIT: Duration = Duration::from_secs(2);
    pub const PROPOSAL_HOLD_ACQUIRE: Duration = Duration::from_millis(500);
    pub const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
    pub const PEER_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
    pub const SOURCE_MEMBER_OBSERVATION_MAX_AGE_MS: i64 = 5_000;
    pub const SHARING_FLOOR_ADVISORY_READ_BOUND: Duration = Duration::from_secs(1);
}

/// Authenticated observation and enforcement-refresh bounds; no cached authority.
pub mod clock {
    use std::time::Duration;

    pub const CLOCK_OFFSET_REFUSAL_MS: i64 = 2_000;
    pub const CLOCK_OBSERVATION_MAX_AGE: Duration = Duration::from_secs(25);
    pub const CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS: i64 = 250;
    pub const REMOVAL_REACHABILITY_STABILIZATION: Duration = Duration::from_secs(30);
    pub const CLOCK_PROBE_MAX_RTT_MS: i64 = 2_000;
    pub const CLOCK_RTT_QUANTIZATION_FLOOR_US: i64 = 1_000;
    pub const CLOCK_PROBE_INTERVAL: Duration = Duration::from_secs(10);
    pub const CLOCK_ROSTER_BUDGET: Duration = Duration::from_secs(2);
    pub const CLOCK_ENFORCEMENT_REFRESH: Duration = Duration::from_secs(10);
    pub const CLOCK_ADVISORY_MAX_HOLD: Duration = Duration::from_secs(120);
}

/// Cluster-wide job lease lifetime, renewal and exact-release retry policy.
pub mod jobs {
    use std::time::Duration;

    pub const MIN_LEASE_TTL: Duration = Duration::from_secs(1);
    pub const MAX_LEASE_TTL: Duration = Duration::from_secs(5 * 60);
    pub const JOB_LEASE_TTL: Duration = Duration::from_secs(JOB_LEASE_HEARTBEAT.as_secs() * 3);
    pub const JOB_LEASE_HEARTBEAT: Duration = Duration::from_secs(30);
    pub const RELEASE_ATTEMPTS: usize = 5;
    pub const RELEASE_RETRY_DELAY: Duration = Duration::from_millis(100);
}

/// Media ownership and relay envelopes; no lease is refreshed by a cached read.
pub mod media {
    use std::time::Duration;

    use crate::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS;

    pub const START_DEADLINE: Duration = Duration::from_secs(50);
    pub const OWNER_ASSIGNMENT_DEADLINE: Duration = super::store::STORE_TIMEOUT;
    pub const ACTIVATION_STORE_DEADLINE: Duration = super::store::STORE_TIMEOUT;
    pub const ABORT_DEADLINE: Duration = Duration::from_secs(5);
    pub const MAX_ADMITTED_MEDIA_BODY_LIFETIME: Duration = Duration::from_secs(300);
    pub const MAX_ADMITTED_MEDIA_PREHEADER_LIFETIME: Duration = Duration::from_secs(
        RELAY_PLAYLIST_MAX_LIFETIME.as_secs() + RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE.as_secs(),
    );
    pub const TERMINAL_PROJECTION_SAFETY_WINDOW: Duration =
        Duration::from_millis(MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS as u64);
    pub const MEDIA_BODY_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);
    pub const LEASE_INTERVAL: Duration = Duration::from_secs(3);
    pub const LEASE_TTL_MS: i64 = LEASE_INTERVAL.as_millis() as i64 * 4;
    pub const TAKEOVER_CLAIM_LEASE_TTL_MS: i64 = LEASE_TTL_MS * 2;
    pub const REMOTE_ACTIVATION_CONFIRMATION_WINDOW: Duration = Duration::from_secs(
        START_DEADLINE.as_secs()
            + OWNER_ASSIGNMENT_DEADLINE.as_secs()
            + (LEASE_TTL_MS as u64 / 1_000)
            + 10,
    );
    pub const RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE: Duration = Duration::from_secs(2);
    pub const RELAY_PLAYLIST_MAX_LIFETIME: Duration = Duration::from_secs(60);
    pub const RELAY_SHORT_MAX_LIFETIME: Duration = Duration::from_secs(5);
    pub const RELAY_SEGMENT_MAX_LIFETIME: Duration = Duration::from_secs(35);
    pub const ROUTE_QUERY_DEADLINE: Duration = Duration::from_secs(3);
    pub const LEASE_RENEWAL_DEADLINE: Duration = Duration::from_secs(4);
    pub const LEASE_RENEWAL_MIN_REMAINING_MS: i64 = 4_000;
    pub const STALE_SETTLEMENT_DEADLINE: Duration = Duration::from_secs(4);
    pub const STALE_SETTLEMENT_MIN_RUNWAY_MS: i64 = 4_000;
    pub const STALE_SETTLEMENT_RETRY_BACKOFF: Duration = Duration::from_secs(30);
    pub const STALE_SETTLEMENT_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
    pub const RELEASE_RECONCILIATION_RETRY_BACKOFF: Duration = Duration::from_secs(1);
    pub const RELEASE_RECONCILIATION_MAX_BACKOFF: Duration = Duration::from_secs(30);
    pub const TAKEOVER_INTERVAL: Duration = Duration::from_secs(2);
    pub const TAKEOVER_DEADLINE: Duration = Duration::from_secs(8);
    pub const TAKEOVER_OVERLAP_MARGIN_MS: i64 = 2_000;
    pub const TAKEOVER_CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
    pub const TAKEOVER_RECONCILIATION_STORE_DEADLINE: Duration = Duration::from_secs(1);
    pub const TAKEOVER_RECONCILIATION_RETRY_BACKOFF: Duration = Duration::from_millis(250);
    pub const TAKEOVER_PUBLICATION_MIN_RUNWAY: Duration = Duration::from_secs(
        TAKEOVER_RECONCILIATION_STORE_DEADLINE.as_secs()
            + LEASE_INTERVAL.as_secs()
            + LEASE_RENEWAL_DEADLINE.as_secs()
            + (LEASE_RENEWAL_MIN_REMAINING_MS as u64 / 1_000)
            + 1,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn shorter(short: Duration, long: Duration, reason: &str) {
        assert!(
            short < long,
            "{reason}: {short:?} must be shorter than {long:?}"
        );
    }

    #[test]
    fn heartbeat_and_proof_expiry_precede_the_supported_election_floors() {
        // The previous deployed floor still constrains rolling upgrades.
        let previous_release_floor = Duration::from_millis(1_500);
        let maximum_idle_poll = Duration::from_millis(raft::HIQLITE_HEARTBEAT_INTERVAL_MS * 3 / 2);
        shorter(
            maximum_idle_poll,
            previous_release_floor,
            "an idle heartbeat must precede an old voter's election",
        );
        shorter(
            serving::QUORUM_WATERMARK_LEASE,
            Duration::from_millis(raft::HIQLITE_ELECTION_TIMEOUT_MIN_MS),
            "cached replica authority must expire before a successor can be elected",
        );
        shorter(
            serving::QUORUM_WATERMARK_REFRESH,
            serving::QUORUM_WATERMARK_LEASE,
            "the sampler must refresh before a proof's fixed expiry",
        );
        shorter(
            serving::QUORUM_WATERMARK_TIMEOUT,
            serving::QUORUM_WATERMARK_LEASE,
            "one sample attempt must remain bounded by the proof lease",
        );
    }

    #[test]
    fn exact_state_retry_envelope_fits_the_accepted_recovery_budget() {
        let attempts = u32::try_from(store::IDEMPOTENT_WRITE_MAX_ATTEMPTS)
            .expect("the configured retry count must fit a duration multiplier");
        let envelope = store::STORE_TIMEOUT * attempts
            + store::IDEMPOTENT_WRITE_RETRY_DELAY * attempts.saturating_sub(1);
        shorter(
            envelope,
            raft::REPLICATED_LEADER_RECOVERY_BUDGET,
            "all exact-state attempts and inter-attempt delays must fit recovery",
        );
        shorter(
            store::AUTHORITY_QUORUM_RETRY_MAX_DELAY,
            store::AUTHORITY_QUORUM_RECOVERY_BUDGET,
            "a capped quorum retry delay must leave room for another attempt",
        );
    }

    #[test]
    fn takeover_claim_outlives_publication_and_first_ordinary_renewal() {
        let claim = Duration::from_millis(media::TAKEOVER_CLAIM_LEASE_TTL_MS as u64);
        shorter(media::TAKEOVER_DEADLINE + media::TAKEOVER_PUBLICATION_MIN_RUNWAY, claim, "the one-shot takeover claim must cover acquisition plus publication and renewal runway");
        let ordinary = Duration::from_millis(media::LEASE_TTL_MS as u64);
        shorter(
            media::LEASE_INTERVAL + media::LEASE_RENEWAL_DEADLINE,
            ordinary,
            "ordinary inventory and renewal must finish within the normal lease",
        );
        shorter(
            media::START_DEADLINE + media::OWNER_ASSIGNMENT_DEADLINE + ordinary,
            media::REMOTE_ACTIVATION_CONFIRMATION_WINDOW,
            "the worker's fixed confirmation lease must cover ingress work",
        );
        shorter(
            media::MAX_ADMITTED_MEDIA_PREHEADER_LIFETIME + media::MAX_ADMITTED_MEDIA_BODY_LIFETIME,
            media::TERMINAL_PROJECTION_SAFETY_WINDOW,
            "terminal projection must outlive every predecessor response envelope",
        );
    }

    #[test]
    fn liveness_windows_leave_room_for_refresh_and_shutdown_release() {
        shorter(
            membership::HEARTBEAT_COALESCE_WINDOW,
            membership::HEARTBEAT_INTERVAL,
            "coalescing must not swallow the ordinary heartbeat cadence",
        );
        shorter(
            membership::HEARTBEAT_INTERVAL,
            Duration::from_millis(membership::NODE_REACHABLE_WINDOW_MS as u64),
            "one missed heartbeat must not immediately mark a peer unreachable",
        );
        shorter(
            membership::PROPOSAL_HOLD_ACQUIRE,
            membership::PROPOSAL_HOLD_WAIT,
            "hold acquisition must leave time to hand off",
        );
        shorter(
            membership::PROPOSAL_HOLD_WAIT,
            membership::SHUTDOWN_HANDOFF_WAIT,
            "the local proposal hold is inside the shutdown deadline",
        );
        shorter(
            jobs::JOB_LEASE_HEARTBEAT
                + store::STORE_TIMEOUT * jobs::RELEASE_ATTEMPTS as u32
                + jobs::RELEASE_RETRY_DELAY * jobs::RELEASE_ATTEMPTS.saturating_sub(1) as u32,
            jobs::JOB_LEASE_TTL,
            "a renewal tick plus the exact-release envelope must fit the job lease",
        );
        shorter(
            clock::CLOCK_PROBE_INTERVAL
                + Duration::from_millis(clock::CLOCK_PROBE_MAX_RTT_MS as u64),
            clock::CLOCK_OBSERVATION_MAX_AGE,
            "a normal probe cycle must fit the maximum observation age",
        );
    }
}
