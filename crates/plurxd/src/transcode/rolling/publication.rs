use super::*;

pub(super) struct PreparedSharedCacheRead {
    pub(super) dir: PathBuf,
    pub(super) manifest: Arc<plurx_core::transcode::manifest::GenerationManifest>,
}

#[derive(Clone)]
pub(super) struct RollingTerminalResultReceipt {
    result: tokio::sync::watch::Receiver<
        Option<
            Result<
                crate::playback_control::LocalControlResult,
                crate::playback_control::ControlStateError,
            >,
        >,
    >,
}

impl RollingTerminalResultReceipt {
    pub(super) fn pending() -> (
        Self,
        tokio::sync::watch::Sender<
            Option<
                Result<
                    crate::playback_control::LocalControlResult,
                    crate::playback_control::ControlStateError,
                >,
            >,
        >,
    ) {
        let (sender, result) = tokio::sync::watch::channel(None);
        (Self { result }, sender)
    }

    pub(super) async fn wait(
        &self,
    ) -> Result<
        crate::playback_control::LocalControlResult,
        crate::playback_control::ControlStateError,
    > {
        let mut result = self.result.clone();
        loop {
            if let Some(outcome) = result.borrow().clone() {
                return outcome;
            }
            result
                .changed()
                .await
                .map_err(|_| crate::playback_control::ControlStateError::Unavailable)?;
        }
    }
}

#[derive(Clone)]
pub(super) struct RollingTerminalIdentity {
    generation: String,
    owner_node_id: String,
    owner_epoch: u64,
    client_instance_id: String,
    sequence: u64,
    snapshot: crate::playback_control::PlaybackDemandSnapshot,
}

impl RollingTerminalIdentity {
    pub(super) fn from_request(control: &crate::playback_control::LocalControlRequest<'_>) -> Self {
        Self {
            generation: control.generation.to_owned(),
            owner_node_id: control.owner_node_id.to_owned(),
            owner_epoch: control.owner_epoch,
            client_instance_id: control.client_instance_id.to_owned(),
            sequence: control.sequence,
            snapshot: control.snapshot.clone(),
        }
    }

    pub(super) fn matches(
        &self,
        control: &crate::playback_control::LocalControlRequest<'_>,
    ) -> bool {
        self.generation == control.generation
            && self.owner_node_id == control.owner_node_id
            && self.owner_epoch == control.owner_epoch
            && self.client_instance_id == control.client_instance_id
            && self.sequence == control.sequence
            && self.snapshot == control.snapshot
    }
}

#[derive(Clone)]
pub(super) struct RollingTerminalOperation {
    pub(super) identity: RollingTerminalIdentity,
    pub(super) result: RollingTerminalResultReceipt,
    /// Zero while the actor-owned result is still being prepared. Once the
    /// terminal committer returns, this becomes its exact immutable durable
    /// acknowledgement expiry rather than a separately guessed admission TTL.
    pub(super) expires_at_unix_ms: Arc<AtomicI64>,
    pub(super) exact_commit:
        Arc<std::sync::Mutex<Option<crate::playback_control::TerminalCommitReceipt>>>,
}

pub(super) const ROLLING_PUBLICATION_POLL: Duration = Duration::from_millis(250);
pub(super) const ROLLING_PUBLICATION_TARGET: Duration =
    Duration::from_secs(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS as u64);
pub(super) const ROLLING_PUBLICATION_EARLIEST: Duration =
    Duration::from_secs((plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS / 2) as u64);
pub(super) const ROLLING_PUBLICATION_HARD: Duration = Duration::from_secs(
    (plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS
        + plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS / 2) as u64,
);
pub(super) const ROLLING_INITIAL_RUNWAY_MS: i64 =
    plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS as i64 * 3 * 1_000;
pub(super) const ROLLING_SERVED_WINDOW_MS: i64 = RETENTION_SECS * 1_000;
pub(super) const ROLLING_SEGMENT_MAX_MS: i64 =
    plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS as i64 * 1_000;
pub(super) const ROLLING_PUBLICATION_GUARD_MS: i64 =
    crate::playback_control::NEXT_EXCHANGE_MS as i64 * 2;
pub(super) const ROLLING_BACK_BUFFER_MS: i64 = CLIENT_BACK_BUFFER_SECS * 1_000;
pub(super) const ROLLING_RESERVE_MAX_MS: i64 = ROLLING_SERVED_WINDOW_MS
    - ROLLING_PUBLICATION_GUARD_MS
    - ROLLING_SEGMENT_MAX_MS
    - ROLLING_BACK_BUFFER_MS;

#[derive(Clone)]
pub(super) struct ServedPlaylistSnapshot {
    pub(super) raw: Arc<[u8]>,
    pub(super) producer_attempt: u64,
    pub(super) revision: u64,
    pub(super) last_segment: i64,
    pub(super) first_segment: i64,
    pub(super) end_ms: i64,
    pub(super) duration_ms: i64,
    pub(super) end_list: bool,
    pub(super) available_at: Instant,
}

#[derive(Default)]
pub(super) struct RollingPublicationClock {
    pub(super) served: Option<ServedPlaylistSnapshot>,
    pub(super) staged_attempt: Option<u64>,
    pub(super) staged_last_segment: Option<i64>,
    pub(super) staged_end_ms: Option<i64>,
    pub(super) next_publish_at: Option<Instant>,
    pub(super) hard_deadline: Option<Instant>,
    /// Logical prefix removal requested by the download-frontier retention
    /// policy. It takes effect only in the next segment-bearing snapshot.
    pub(super) retention_first_segment: Option<i64>,
    pub(super) budget_attempt: Option<u64>,
    pub(super) budget_anchor_sequence: Option<u64>,
    legacy_bootstrap_at: Option<Instant>,
    pub(super) allowed_end_ms: Option<i64>,
    pub(super) carried_surplus_ms: i64,
    pub(super) demand_observation_age_ms: Option<i64>,
}

impl RollingPublicationClock {
    pub(super) fn staged_seconds(&self) -> i64 {
        let served = self.served.as_ref().map_or(0, |snapshot| snapshot.end_ms);
        self.staged_end_ms.unwrap_or(served).saturating_sub(served) / 1_000
    }

    pub(super) fn reset_for_attempt(&mut self, producer_attempt: u64) {
        if self
            .staged_attempt
            .is_some_and(|attempt| attempt != producer_attempt)
            || self
                .served
                .as_ref()
                .is_some_and(|snapshot| snapshot.producer_attempt != producer_attempt)
        {
            self.served = None;
            self.staged_last_segment = None;
            self.staged_end_ms = None;
            self.next_publish_at = None;
            self.hard_deadline = None;
            self.retention_first_segment = None;
            self.budget_attempt = None;
            self.budget_anchor_sequence = None;
            self.legacy_bootstrap_at = None;
            self.allowed_end_ms = None;
            self.carried_surplus_ms = 0;
            self.demand_observation_age_ms = None;
        }
        self.staged_attempt = Some(producer_attempt);
    }

    pub(super) fn publication_budget_at(
        &mut self,
        now: Instant,
        producer_attempt: u64,
        lease: Option<&crate::playback_control::RollingLeaseSnapshot>,
        media_origin_ms: i64,
    ) -> RollingPublicationBudget {
        self.budget_attempt = Some(producer_attempt);
        let explicit = lease.filter(|lease| {
            lease.mode == crate::playback_control::RollingLeaseMode::Explicit
                && lease.demand.is_some()
                && lease.accepted_demand_sequence.is_some()
        });
        let budget = if let Some(lease) = explicit {
            self.legacy_bootstrap_at = None;
            let demand = lease.demand.as_ref().expect("explicit demand checked");
            let observation_age = lease.demand_observation_age.unwrap_or_default();
            let consumed_absolute_ms =
                crate::playback_control::rolling_estimated_position_ms(demand, observation_age);
            let consumed_end_ms = consumed_absolute_ms.saturating_sub(media_origin_ms).max(0);
            let reserve_ms = rolling_initial_runway_ms(rolling_playback_rate(Some(demand)));
            RollingPublicationBudget {
                demand_sequence: lease.accepted_demand_sequence,
                consumed_end_ms,
                desired_end_ms: consumed_end_ms.saturating_add(reserve_ms),
                allowed_end_ms: consumed_end_ms
                    .saturating_add(reserve_ms)
                    .saturating_add(ROLLING_SEGMENT_MAX_MS),
                protected_position_ms: consumed_absolute_ms
                    .saturating_sub(ROLLING_PUBLICATION_GUARD_MS)
                    .saturating_sub(ROLLING_BACK_BUFFER_MS)
                    .max(media_origin_ms)
                    .saturating_sub(media_origin_ms),
                observation_age_ms: Some(
                    i64::try_from(observation_age.as_millis()).unwrap_or(i64::MAX),
                ),
            }
        } else {
            let bootstrap_at = *self.legacy_bootstrap_at.get_or_insert(now);
            let consumed_end_ms =
                i64::try_from(now.saturating_duration_since(bootstrap_at).as_millis())
                    .unwrap_or(i64::MAX);
            let desired_end_ms = consumed_end_ms.saturating_add(ROLLING_INITIAL_RUNWAY_MS);
            let fetched_end_ms = lease.map_or(0, |lease| lease.delivery.fetched_end_ms.max(0));
            RollingPublicationBudget {
                demand_sequence: None,
                consumed_end_ms,
                desired_end_ms,
                allowed_end_ms: desired_end_ms
                    .saturating_add(ROLLING_SEGMENT_MAX_MS)
                    .min(fetched_end_ms.saturating_add(ROLLING_INITIAL_RUNWAY_MS)),
                protected_position_ms: consumed_end_ms
                    .saturating_sub(ROLLING_PUBLICATION_GUARD_MS)
                    .saturating_sub(ROLLING_BACK_BUFFER_MS)
                    .max(0),
                observation_age_ms: None,
            }
        };
        self.budget_anchor_sequence = budget.demand_sequence;
        self.allowed_end_ms = Some(budget.allowed_end_ms);
        self.demand_observation_age_ms = budget.observation_age_ms;
        self.carried_surplus_ms = self.served.as_ref().map_or(0, |served| {
            served.end_ms.saturating_sub(budget.desired_end_ms).max(0)
        });
        budget
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RollingPublicationBudget {
    pub(super) demand_sequence: Option<u64>,
    pub(super) consumed_end_ms: i64,
    pub(super) desired_end_ms: i64,
    pub(super) allowed_end_ms: i64,
    pub(super) protected_position_ms: i64,
    pub(super) observation_age_ms: Option<i64>,
}

impl RollingTerminalOperation {
    pub(super) fn expired(&self, now_unix_ms: i64) -> bool {
        if let Some(commit) = self
            .exact_commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            return commit.is_expired();
        }
        let expires_at_unix_ms = self.expires_at_unix_ms.load(Acquire);
        expires_at_unix_ms > 0 && now_unix_ms >= expires_at_unix_ms
    }
}
