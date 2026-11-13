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

/// First publication is separate from steady reserve. Native HLS must admit
/// its fixed advertised target before attachment; the audited hls.js path can
/// replenish a shorter first reload window. This contract is frozen at create,
/// so a retry or producer replacement cannot shorten native admission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RollingStartupPolicy {
    #[cfg(test)]
    Conservative,
    #[default]
    NativePresentationWindow,
    FirstReloadWindow,
    #[cfg(test)]
    FirstCompleteSegment,
    #[cfg(test)]
    WebFixedHlsV1,
}

/// The usable forward media a growing presentation must have committed from
/// its requested position before its first playlist is answered: A1-N's one
/// rule ([`RollingStartupPolicy::for_transport`]), as rolling publication,
/// the shared Source range and a local range all apply it. At the fixed
/// 16-second target and rate 1 that is 48 seconds for native, missing and
/// unknown transports and one target for exact `hlsjs`.
pub(crate) fn startup_admission_window_ms(transport: Option<&str>, rate: f64) -> u64 {
    RollingStartupPolicy::for_transport(transport)
        .bootstrap_ms(rate)
        .max(1) as u64
}

impl RollingStartupPolicy {
    /// Only the audited hls.js path has evidence for the short reload
    /// window; native, omitted and future transports get the window derived
    /// from the same fixed presentation target.
    pub(super) fn for_transport(transport: Option<&str>) -> Self {
        match transport {
            Some("hlsjs") => Self::FirstReloadWindow,
            _ => Self::NativePresentationWindow,
        }
    }

    pub(super) fn uses_small_bootstrap(self) -> bool {
        match self {
            #[cfg(test)]
            Self::Conservative => false,
            Self::NativePresentationWindow => false,
            Self::FirstReloadWindow => true,
            #[cfg(test)]
            Self::FirstCompleteSegment => true,
            #[cfg(test)]
            Self::WebFixedHlsV1 => true,
        }
    }

    pub(super) fn bootstrap_ms(self, rate: f64) -> i64 {
        match self {
            #[cfg(test)]
            Self::Conservative => rolling_initial_runway_ms(rate),
            Self::NativePresentationWindow => rolling_initial_runway_ms(rate),
            Self::FirstReloadWindow => {
                ((ROLLING_PUBLICATION_TARGET.as_millis() as f64 * rate).ceil() as i64).clamp(
                    ROLLING_PUBLICATION_TARGET.as_millis() as i64,
                    ROLLING_RESERVE_MAX_MS,
                )
            }
            #[cfg(test)]
            Self::FirstCompleteSegment => {
                ((f64::from(plurx_core::transcode::SEGMENT_SECONDS) * 1_000.0 * rate).ceil() as i64)
                    .clamp(2_000, ROLLING_RESERVE_MAX_MS)
            }
            #[cfg(test)]
            Self::WebFixedHlsV1 => {
                ((32_000.0 * rate).ceil() as i64).clamp(32_000, ROLLING_RESERVE_MAX_MS)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RollingReservePhase {
    #[default]
    Prepublication,
    AwaitingPresentation,
    ActiveLowReserve,
    Steady,
}

#[derive(Default)]
pub(super) struct RollingPublicationClock {
    pub(super) startup_policy: RollingStartupPolicy,
    startup_policy_bound: bool,
    pub(super) reserve_phase: RollingReservePhase,
    pub(super) served: Option<ServedPlaylistSnapshot>,
    pub(super) staged_attempt: Option<u64>,
    /// Actual monotonic observation of the writer's first gated inventory.
    /// Diagnostic only; never used to grant publication or reset a deadline.
    pub(super) first_staged_at: Option<Instant>,
    pub(super) staged_last_segment: Option<i64>,
    pub(super) staged_end_ms: Option<i64>,
    pub(super) next_publish_at: Option<Instant>,
    pub(super) hard_deadline: Option<Instant>,
    /// Logical prefix removal requested by the download-frontier retention
    /// policy. It takes effect only in the next segment-bearing snapshot.
    pub(super) retention_first_segment: Option<i64>,
    pub(super) budget_attempt: Option<u64>,
    pub(super) budget_anchor_sequence: Option<u64>,
    pub(super) allowed_end_ms: Option<i64>,
    pub(super) carried_surplus_ms: i64,
    pub(super) demand_observation_age_ms: Option<i64>,
    /// When the current run of "the next segment is only early" cycles began.
    /// Cleared by the next publication; bounds how long publication may wait
    /// for a viewer who is not consuming.
    pub(super) deferred_since: Option<Instant>,
}

impl RollingPublicationClock {
    #[cfg(test)]
    pub(super) fn historical_control_for_test() -> Self {
        Self {
            startup_policy: RollingStartupPolicy::Conservative,
            ..Self::default()
        }
    }

    pub(super) fn bind_startup_transport(&mut self, transport: Option<&str>) {
        // A ready playlist is not native decoder admission. Safari can fetch
        // every object in a short target-16 window yet stay at HAVE_METADATA
        // until startup expires. Only the audited hls.js path has evidence for
        // the short reload window; native, omitted and future transports need
        // a window derived from that same fixed presentation target. Keep the
        // producer's paid steady reserve and all media validation unchanged.
        if !self.startup_policy_bound && self.served.is_none() {
            self.startup_policy = RollingStartupPolicy::for_transport(transport);
            self.startup_policy_bound = true;
        }
    }

    #[cfg(test)]
    pub(super) fn bind_startup_candidate_for_test(&mut self, policy: RollingStartupPolicy) {
        if !self.startup_policy_bound && self.served.is_none() {
            self.startup_policy = policy;
            self.startup_policy_bound = true;
        }
    }

    pub(super) fn clear_attempt(&mut self) {
        let policy = self.startup_policy;
        let bound = self.startup_policy_bound;
        *self = Self {
            startup_policy: policy,
            startup_policy_bound: bound,
            ..Self::default()
        };
    }

    pub(super) fn update_reserve_phase(
        &mut self,
        lease: Option<&crate::playback_control::RollingLeaseSnapshot>,
        budget: &RollingPublicationBudget,
    ) {
        self.reserve_phase = match self.served.as_ref() {
            None => RollingReservePhase::Prepublication,
            Some(_)
                if !lease.is_some_and(|lease| {
                    lease.startup.phase == crate::playback_control::RollingStartupPhase::Presented
                }) =>
            {
                RollingReservePhase::AwaitingPresentation
            }
            Some(served) if served.end_ms >= budget.desired_end_ms => RollingReservePhase::Steady,
            Some(_) => RollingReservePhase::ActiveLowReserve,
        };
    }

    pub(super) fn first_ready_end_ms(
        &self,
        budget: &RollingPublicationBudget,
        lease: Option<&crate::playback_control::RollingLeaseSnapshot>,
    ) -> i64 {
        let rate = rolling_playback_rate(lease.and_then(|lease| lease.demand.as_ref()));
        budget
            .consumed_end_ms
            .saturating_add(self.startup_policy.bootstrap_ms(rate))
    }

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
            self.reserve_phase = RollingReservePhase::Prepublication;
            self.served = None;
            self.first_staged_at = None;
            self.staged_last_segment = None;
            self.staged_end_ms = None;
            self.next_publish_at = None;
            self.hard_deadline = None;
            self.retention_first_segment = None;
            self.budget_attempt = None;
            self.budget_anchor_sequence = None;
            self.allowed_end_ms = None;
            self.carried_surplus_ms = 0;
            self.demand_observation_age_ms = None;
            self.deferred_since = None;
        }
        self.staged_attempt = Some(producer_attempt);
    }

    pub(super) fn publication_budget_at(
        &mut self,
        now: Instant,
        producer_attempt: u64,
        lease: Option<&crate::playback_control::RollingLeaseSnapshot>,
        media_origin_ms: i64,
        requested_position_ms: i64,
    ) -> RollingPublicationBudget {
        self.budget_attempt = Some(producer_attempt);
        let initial_consumed_ms = requested_position_ms.saturating_sub(media_origin_ms).max(0);
        let published = self
            .served
            .as_ref()
            .is_some_and(|served| served.producer_attempt == producer_attempt);
        let explicit = lease.filter(|lease| {
            lease.mode == crate::playback_control::RollingLeaseMode::Explicit
                && lease.demand.is_some()
                && lease.accepted_demand_sequence.is_some()
        });
        let budget = if let Some(lease) = explicit {
            let demand = lease.demand.as_ref().expect("explicit demand checked");
            // Rendering telemetry is not permission to extrapolate a playhead
            // before this attachment has actually presented. The actor's
            // admission requires advancing, exact-attempt post-publication
            // observations; its latest observation then owns elapsed time.
            let observation_age = if published
                && lease.startup.phase == crate::playback_control::RollingStartupPhase::Presented
            {
                lease.demand_observation_age.unwrap_or_default()
            } else {
                Duration::ZERO
            };
            let mut ends =
                rolling_explicit_publication_ends(demand, observation_age, media_origin_ms);
            if !published {
                ends.consumed_end_ms = initial_consumed_ms;
                ends.consumed_absolute_ms = media_origin_ms.saturating_add(initial_consumed_ms);
                ends.desired_end_ms = initial_consumed_ms.saturating_add(
                    rolling_initial_runway_ms(rolling_playback_rate(Some(demand))),
                );
                ends.allowed_end_ms = ends.desired_end_ms.saturating_add(ROLLING_SEGMENT_MAX_MS);
            }
            RollingPublicationBudget {
                demand_sequence: lease.accepted_demand_sequence,
                consumed_end_ms: ends.consumed_end_ms,
                desired_end_ms: ends.desired_end_ms,
                allowed_end_ms: ends.allowed_end_ms,
                protected_position_ms: ends
                    .consumed_absolute_ms
                    .saturating_sub(ROLLING_PUBLICATION_GUARD_MS)
                    .saturating_sub(ROLLING_BACK_BUFFER_MS)
                    .max(media_origin_ms)
                    .saturating_sub(media_origin_ms),
                observation_age_ms: Some(
                    i64::try_from(observation_age.as_millis()).unwrap_or(i64::MAX),
                ),
            }
        } else {
            // A legacy client supplies no playhead. Its conservative clock
            // starts with actual media delivery, never writer inventory or
            // mere playlist availability. The actor resets this evidence on
            // replacement and rejects predecessor commits.
            let delivered_at = lease
                .filter(|lease| published && lease.delivery.producer_attempt == producer_attempt)
                .and_then(|lease| lease.delivery.first_media_committed_at);
            let elapsed_ms = delivered_at.map_or(0, |at| {
                i64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(i64::MAX)
            });
            let consumed_end_ms = initial_consumed_ms.saturating_add(elapsed_ms);
            let desired_end_ms = consumed_end_ms.saturating_add(ROLLING_INITIAL_RUNWAY_MS);
            let fetched_end_ms = lease
                .map_or(0, |lease| lease.delivery.fetched_end_ms.max(0))
                .max(initial_consumed_ms);
            // The create's frozen position authorizes native bootstrap even
            // before any media can be fetched. Pay one bounded complete cut
            // past the admission floor: rounding down would publish an unsafe
            // short window (or wait forever at a non-segment boundary). Once
            // published, the ordinary fetch frontier owns the allowance again.
            let allowed_end_ms = if !published
                && self.startup_policy == RollingStartupPolicy::NativePresentationWindow
            {
                desired_end_ms.saturating_add(ROLLING_SEGMENT_MAX_MS)
            } else {
                desired_end_ms
                    .saturating_add(ROLLING_SEGMENT_MAX_MS)
                    .min(fetched_end_ms.saturating_add(ROLLING_INITIAL_RUNWAY_MS))
            };
            RollingPublicationBudget {
                demand_sequence: None,
                consumed_end_ms,
                desired_end_ms,
                allowed_end_ms,
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

/// The explicit publication allowance for one accepted demand observation,
/// relative to the media origin. The publication clock and the flow
/// controller both read it here so their frontiers cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RollingExplicitPublicationEnds {
    consumed_absolute_ms: i64,
    pub(super) consumed_end_ms: i64,
    pub(super) desired_end_ms: i64,
    pub(super) allowed_end_ms: i64,
}

pub(super) fn rolling_explicit_publication_ends(
    demand: &crate::playback_control::PlaybackDemandSnapshot,
    observation_age: Duration,
    media_origin_ms: i64,
) -> RollingExplicitPublicationEnds {
    let consumed_absolute_ms =
        crate::playback_control::rolling_estimated_position_ms(demand, observation_age);
    let consumed_end_ms = consumed_absolute_ms.saturating_sub(media_origin_ms).max(0);
    let desired_end_ms = consumed_end_ms.saturating_add(rolling_initial_runway_ms(
        rolling_playback_rate(Some(demand)),
    ));
    RollingExplicitPublicationEnds {
        consumed_absolute_ms,
        consumed_end_ms,
        desired_end_ms,
        allowed_end_ms: desired_end_ms.saturating_add(ROLLING_SEGMENT_MAX_MS),
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
