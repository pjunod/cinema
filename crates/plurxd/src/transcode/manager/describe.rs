use super::*;

impl TranscodeManager {
    /// Describe a session that already exists, for an idempotent re-create.
    pub(super) async fn recover(&self, session_id: &str) -> Option<StartInfo> {
        if let Some(recovered) = self.vod.recovered_start(session_id).await {
            // An idempotent replay of a VOD create: repeat the persisted
            // answer, field for field, from the session record.
            return Some(StartInfo {
                playlist_url: format!("/api/v1/hls/{}/index.m3u8", recovered.start.session_id),
                session_id: recovered.start.session_id,
                duration_ms: Some(recovered.start.duration_ms),
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                target_height: recovered.target_height,
                kind: recovered.kind,
                encoder: "vod",
                grade: OutputGrade::Sdr,
                vod: true,
                control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            });
        }
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        if session.failed.load(Relaxed) {
            return None;
        }
        let duration_ms = self
            .store
            .get_file(session.file_id)
            .await
            .ok()
            .flatten()
            .and_then(|f| f.duration_ms);
        let encoder = *session.encoder_label.lock().await;
        Some(StartInfo {
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id: session_id.to_owned(),
            duration_ms,
            start_seconds: session.start_seconds,
            media_origin_seconds: session.media_origin_seconds,
            target_height: session.target_height,
            kind: session.kind,
            encoder,
            grade: session.grade,
            vod: session.cached,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }

    /// A numeric setting, or its default when unset or unparseable.
    pub(super) async fn num_setting<T>(&self, key: &str, default: T) -> T
    where
        T: std::str::FromStr + PartialOrd + Default,
    {
        match self.store.get_setting(key).await {
            Ok(Some(v)) => v
                .trim()
                .parse::<T>()
                .ok()
                .filter(|n| *n >= T::default())
                .unwrap_or(default),
            _ => default,
        }
    }

    /// Conservative shape guarantee for a peer whose serving contract may
    /// predate unconditional sliding playlists. Remote start v1 does not
    /// acknowledge the actual shape. Retain the old shared-setting guarantee
    /// for durable takeover recipes rather than claiming this binary's local
    /// behavior for an older worker. Local rolling serving never reads it.
    pub(crate) async fn cluster_playlist_is_typeless(&self) -> bool {
        self.bool_setting(keys::HLS_TYPELESS_SLIDING).await
            || self
                .bool_setting(keys::CLUSTER_SESSION_TAKEOVER_ENABLED)
                .await
    }

    /// A feature switch stored in the ordinary settings table. Only the
    /// literal value `1` enables an experiment; absent, malformed, and every
    /// other value stay on the established path.
    async fn bool_setting(&self, key: &str) -> bool {
        self.store
            .get_setting(key)
            .await
            .ok()
            .flatten()
            .is_some_and(|value| value.trim() == "1")
    }

    /// How an HLS session's input should be paced, given the admin settings
    /// and what this ffmpeg build supports. `for_copy` picks the pre-5.1
    /// degradation (see [`crate::ffmpeg::PacingCaps::resolve`]).
    pub(super) async fn pacing(&self, for_copy: bool) -> Pacing {
        let rate = self
            .num_setting(keys::HLS_READRATE, HLS_READRATE_DEFAULT)
            .await;
        let burst = self
            .num_setting(keys::HLS_BURST_SECS, HLS_BURST_SECS_DEFAULT)
            .await;
        pacing_caps().await.resolve(rate, burst, for_copy)
    }

    /// Hardware slots in use, and the cap. The pair is the diagnostic: "2"
    /// alone says nothing, and a viewer being refused while the count sits at
    /// zero is a very different bug from one being refused at the cap.
    pub async fn hardware_slots(&self) -> (usize, usize) {
        (self.admissions.in_use(), self.max_hw_sessions().await)
    }

    /// How many hardware transcodes this node will run at once.
    pub async fn max_hw_sessions(&self) -> usize {
        // A configured zero means "no hardware transcoding", which is a
        // legitimate thing to want on a box whose GPU is doing something else.
        self.num_setting(keys::MAX_HW_SESSIONS, DEFAULT_MAX_HW_SESSIONS)
            .await
    }

    /// Threads the software-encoder pool may hand out at once: every core
    /// but one unless the admin says otherwise ([`keys::SW_POOL_THREADS`]).
    /// The tests set the key, so the suite asserts policy rather than the
    /// build machine's core count.
    pub async fn software_budget(&self) -> usize {
        self.num_setting(keys::SW_POOL_THREADS, crate::admission::software_budget())
            .await
    }

    /// Choose the encoder given the admin preference setting (empty = auto).
    pub(super) async fn encoder(&self) -> Encoder {
        let prefer = self
            .store
            .get_setting(keys::HWACCEL)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        self.caps.choose(&prefer)
    }

    /// The conservative whole-node projection retained for legacy readers.
    ///
    /// Production planning additionally intersects this with the exact
    /// resolved decode path; this accessor remains for the direct-identity
    /// test seam and startup transition logging.
    pub fn artifact_qualification(&self) -> plurx_core::transcode::ArtifactQualification {
        self.artifact_qualification
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .effective
    }

    /// The answer this node published, exactly as it published it.
    pub fn published_artifact_qualification(&self) -> ArtifactQualificationReadiness {
        self.artifact_qualification
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether newly created producer attempts may use the one-shot
    /// hardware-to-software decoder fallback.
    pub fn automatic_decoder_recovery_enabled(&self) -> bool {
        self.automatic_decoder_recovery.load(Acquire)
    }

    /// Apply the Developer switch immediately. It changes only whether a new
    /// attempt may act on diagnostics; it does not rotate cache identities or
    /// mutate work already in flight.
    pub fn set_automatic_decoder_recovery(&self, enabled: bool) {
        self.automatic_decoder_recovery.store(enabled, Release);
    }

    /// Publish the persisted switch before the listener accepts sessions.
    pub async fn publish_automatic_decoder_recovery(&self) {
        let enabled = match self
            .store
            .get_setting(plurx_core::store::keys::AUTOMATIC_DECODER_RECOVERY)
            .await
        {
            Ok(value) => plurx_core::store::stored_switch(value.as_deref(), false),
            Err(error) => {
                tracing::warn!(target: "plurxd::transcode", %error, "could not read automatic decoder recovery setting; keeping it off");
                false
            }
        };
        self.set_automatic_decoder_recovery(enabled);
        tracing::info!(
            target: "plurxd::transcode",
            enabled, "published automatic decoder recovery setting"
        );
    }

    /// What this node measured, and what it may therefore honour.
    ///
    /// Reads no setting: it answers "could this node do it", which the
    /// settings surface needs in order to explain a refusal *before* an
    /// operator asks for one and gets it.
    pub fn artifact_qualification_readiness(
        &self,
        requested: bool,
    ) -> ArtifactQualificationReadiness {
        let selectable_paths = self.selectable_decode_paths();
        artifact_qualification_readiness(
            requested,
            &self.diagnostic_policy,
            &self.measured_decoders,
            &selectable_paths,
        )
    }

    /// Every `(codec, backend)` the manager can select under its boot-time
    /// encoder capabilities. Failed probes stay in this denominator: their
    /// absence is evidence that whole-node qualification cannot be proven,
    /// not evidence that the route disappeared.
    fn selectable_decode_paths(&self) -> Vec<(String, plurx_core::transcode::DecodeBackend)> {
        use plurx_core::transcode::{DecodeBackend, Encoder};

        let backends = [
            (Encoder::Software, DecodeBackend::Software),
            (Encoder::VideoToolbox, DecodeBackend::VideoToolbox),
            (Encoder::Nvenc, DecodeBackend::Cuda),
            (Encoder::Qsv, DecodeBackend::Qsv),
            (Encoder::Vaapi, DecodeBackend::Vaapi),
        ];
        self.decoders
            .iter()
            .flat_map(|codec| {
                backends
                    .iter()
                    .filter(|(encoder, _)| self.caps.available(*encoder))
                    .map(|(_, backend)| (codec.clone(), *backend))
            })
            .collect()
    }

    /// Read and publish the operator's requested path-scoped policy.
    ///
    /// **Called once, at start.** Not on every write, and that is the whole
    /// design rather than an omission. This value can change the cache key for
    /// every covered path, so moving it on a live node moves those key spaces
    /// under work that is already running: a session that resolved its plan a
    /// second ago publishes into a directory the next lookup will not name, a
    /// resumable production cannot find its own earlier parts and — under the
    /// qualified identity — those parts carry no receipt, so the film it
    /// restarts can never be kept. A control that quietly did that to a busy
    /// node would be a worse failure than the one this effort exists to fix,
    /// because it would be caused by the fix.
    ///
    /// So the request is stored when it is written and read when the node next
    /// starts, which is also how a fleet rolls one out. Each later plan applies
    /// that stable request only to an exact path with one covering contract.
    /// The settings surface reports the stored request and the published
    /// answer as separate facts, so an operator can see that a restart is owed.
    ///
    /// A store that cannot be read is not an excuse to guess, and it is also
    /// not a reason to refuse to start a media server. The node keeps the
    /// identity every deployed node already has and says it could not read the
    /// setting, which is true and is visible on the settings surface.
    pub async fn publish_artifact_qualification(&self) -> ArtifactQualificationReadiness {
        #[cfg(test)]
        self.force_artifact_qualification
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let stored = self
            .store
            .get_setting(plurx_core::store::keys::DECODER_HEALTH_QUALIFIED_ARTIFACTS)
            .await;
        let readiness = match stored {
            Ok(value) => self.artifact_qualification_readiness(value.as_deref() == Some("1")),
            Err(error) => {
                tracing::warn!(
                    target: "plurxd::transcode",
                    %error,
                    "could not read the verified-decode request; keeping the unqualified identity"
                );
                ArtifactQualificationReadiness::unreadable()
            }
        };
        let requested = readiness.requested;
        let previous = self.artifact_qualification();
        *self
            .artifact_qualification
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = readiness.clone();
        if previous != readiness.effective {
            tracing::warn!(
                target: "plurxd::transcode",
                namespace = readiness.effective.namespace(),
                previous = previous.namespace(),
                requested,
                covered = readiness.covered_decoders.len(),
                refusal = readiness.refusal.map(QualificationRefusal::name),
                "the verified-artifact policy changed; uniquely covered decode paths use its identity"
            );
        } else {
            tracing::info!(
                target: "plurxd::transcode",
                namespace = readiness.effective.namespace(),
                requested,
                covered = readiness.covered_decoders.len(),
                refusal = readiness.refusal.map(QualificationRefusal::name),
                "published the effective artifact identity"
            );
        }
        readiness
    }

    /// Publish an identity directly, without per-path contract selection.
    ///
    /// Test-only, and it must stay that way. Every test of the enforcement
    /// behind this control runs on a host no diagnostic contract covers, so
    /// the real publisher would — correctly — refuse them all. This says
    /// "pretend the prerequisites are met"; the real publisher is what decides
    /// whether they are.
    #[cfg(test)]
    pub(crate) fn test_publish_artifact_qualification(
        &self,
        qualification: plurx_core::transcode::ArtifactQualification,
    ) {
        {
            let mut published = self
                .artifact_qualification
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            published.effective = qualification;
            published.requested = qualification.enforces_receipt();
            published.refusal = None;
        }
        self.force_artifact_qualification.store(
            qualification.enforces_receipt(),
            std::sync::atomic::Ordering::Relaxed,
        );
        tracing::info!(
            target: "plurxd::transcode",
            namespace = qualification.namespace(),
            "published the effective artifact identity"
        );
    }

    /// Replace real offline production with deterministic outcomes and retain
    /// the exact recipe reference each attempt received.
    #[cfg(test)]
    pub(crate) fn test_script_offline_production(
        &self,
        outcomes: impl IntoIterator<Item = OfflineProduceOutcome>,
    ) {
        self.offline_produce_script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(outcomes);
    }

    #[cfg(test)]
    pub(crate) fn test_offline_produced_recipes(&self) -> Vec<String> {
        self.offline_produced_recipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn test_fail_next_offline_recovery_begin(&self) {
        self.fail_next_offline_recovery_begin
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many generation manifests this manager has published.
    #[cfg(test)]
    pub(crate) fn test_manifests_published(&self) -> usize {
        self.manifests_published
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn rate_control_snapshot(&self) -> RateControlSnapshot {
        *self
            .rate_control
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The effective value a newly normalized session on `encoder` receives.
    pub fn effective_rate_control(&self, encoder: Encoder) -> EffectiveRateControl {
        self.rate_control_snapshot().effective_for(encoder)
    }

    /// Capture the exact effective mode selected for a new resumable package.
    /// Callers persist this value before queueing any encoder work.
    pub async fn effective_rate_control_for_new_offline_package(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<EffectiveRateControl, String> {
        Ok(self.effective_rate_control(
            self.encoder_for_file(file, crate::process_control::ChildClass::Background)
                .await?,
        ))
    }

    #[cfg(test)]
    pub(crate) fn test_mark_live_waiting(&self) -> crate::admission::LiveWait {
        self.admissions.wait_for_slot()
    }

    /// Encoder threads the software pool has reserved right now.
    #[cfg(test)]
    pub(crate) fn test_software_threads_in_use(&self) -> usize {
        self.admissions.software_in_use()
    }

    #[cfg(test)]
    pub(crate) async fn test_publish_supported_quality(&self, quality: u8) -> Encoder {
        let encoder = self.encoder().await;
        let mut quality_rc = QualityRc::default();
        quality_rc.set_supported(encoder, true);
        self.publish_rate_control(
            RateControlSnapshot {
                requested_mode: Some(RateMode::Quality),
                requested_quality: Some(quality),
                quality_rc,
            },
            encoder,
        );
        encoder
    }

    async fn validate_rate_control_snapshot(
        &self,
        mode: Option<RateMode>,
        quality: Option<u8>,
        policy: RateControlProbePolicy,
    ) -> RateControlValidation {
        let needs_quality_probe = [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ]
        .into_iter()
        .any(|encoder| {
            self.caps.available(encoder)
                && mode.unwrap_or_else(|| encoder.default_rate_mode()) == RateMode::Quality
        });
        if !needs_quality_probe {
            return RateControlValidation::Complete(RateControlSnapshot {
                requested_mode: mode,
                requested_quality: quality,
                quality_rc: self.caps.quality_rc,
            });
        }

        // Runtime validation is background encoder work, not an exception to
        // the background-work contract. Sharing this gate means it cannot run
        // beside an active speculative or offline encode. If an offline job
        // queues after the probe wins the gate, `offline_waiting` below is its
        // cancellation signal and the probe gives the lane back promptly.
        let _background_lane = match policy {
            RateControlProbePolicy::Boot => None,
            RateControlProbePolicy::YieldingBackground => {
                let Ok(guard) = self.background_producer.try_lock() else {
                    return RateControlValidation::Deferred;
                };
                Some(guard)
            }
        };

        let mut quality_rc = QualityRc::default();
        for encoder in [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ] {
            if !self.caps.available(encoder) {
                continue;
            }
            if mode.unwrap_or_else(|| encoder.default_rate_mode()) == RateMode::Bitrate {
                continue;
            }
            let q = quality.unwrap_or_else(|| encoder.default_quality());
            if matches!(policy, RateControlProbePolicy::Boot) {
                quality_rc.set_supported(
                    encoder,
                    transcode::validate_quality_rate_control(
                        &ffmpeg_bin(),
                        encoder,
                        q,
                        self.caps.forced_idr.wanted_by(encoder),
                    )
                    .await,
                );
                continue;
            }
            let should_yield = || {
                self.admissions.live_is_waiting()
                    || self
                        .offline_waiting
                        .load(std::sync::atomic::Ordering::Acquire)
            };
            if should_yield() {
                return RateControlValidation::Deferred;
            }
            let validation = if encoder == Encoder::Software {
                let budget = self.software_budget().await;
                // Even a misconfigured zero-thread budget must account for
                // the process. The software pool's empty-pool exception lets
                // one oversized job run, and a positive weight makes the next
                // live waiter visible so this probe can yield to it.
                let probe_weight = budget.max(1);
                let Some(_permit) =
                    self.admissions
                        .try_admit_software(budget, probe_weight, Priority::Background)
                else {
                    return RateControlValidation::Deferred;
                };
                transcode::validate_quality_rate_control_yielding(
                    &ffmpeg_bin(),
                    encoder,
                    q,
                    self.caps.forced_idr.wanted_by(encoder),
                    should_yield,
                )
                .await
            } else {
                let Some(_slot) = self
                    .admissions
                    .try_acquire(self.max_hw_sessions().await, Priority::Background)
                else {
                    return RateControlValidation::Deferred;
                };
                transcode::validate_quality_rate_control_yielding(
                    &ffmpeg_bin(),
                    encoder,
                    q,
                    self.caps.forced_idr.wanted_by(encoder),
                    should_yield,
                )
                .await
            };
            match validation {
                QualityRateControlValidation::Supported => quality_rc.set_supported(encoder, true),
                QualityRateControlValidation::Refused => quality_rc.set_supported(encoder, false),
                QualityRateControlValidation::Deferred => return RateControlValidation::Deferred,
            }
        }
        RateControlValidation::Complete(RateControlSnapshot {
            requested_mode: mode,
            requested_quality: quality,
            quality_rc,
        })
    }

    pub(super) fn publish_rate_control(&self, snapshot: RateControlSnapshot, selected: Encoder) {
        let effective = snapshot.effective_for(selected);
        *self
            .rate_control
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
        tracing::info!(
            target: "plurxd::transcode",
            requested_mode = snapshot
                .requested_mode
                .map_or("family_default", RateMode::as_str),
            requested_quality = snapshot.requested_quality,
            encoder = selected.label(),
            effective = %effective.snapshot_value(),
            "published validated rate-control snapshot"
        );
    }

    pub(super) async fn requested_rate_control(
        &self,
    ) -> Result<(Option<RateMode>, Option<u8>), plurx_core::error::StoreError> {
        let (raw_mode, raw_quality) = self
            .store
            .get_setting_pair(keys::TRANSCODE_RATE_MODE, keys::TRANSCODE_QUALITY)
            .await?;
        let (mode, quality, corrupt) =
            normalize_rate_control_request(raw_mode.as_deref(), raw_quality.as_deref());
        if corrupt {
            tracing::warn!(
                target: "plurxd::transcode",
                rate_mode = raw_mode.as_deref().unwrap_or_default(),
                quality = raw_quality.as_deref().unwrap_or_default(),
                "invalid durable rate-control pair — using bitrate"
            );
        }
        Ok((mode, quality))
    }

    async fn refresh_rate_control_locked(
        &self,
    ) -> Result<Option<RateControlSnapshot>, plurx_core::error::StoreError> {
        loop {
            let (mode, quality) = self.requested_rate_control().await?;
            let current = self.rate_control_snapshot();
            if current.requested_mode == mode && current.requested_quality == quality {
                return Ok(Some(current));
            }
            let candidate = match self
                .validate_rate_control_snapshot(
                    mode,
                    quality,
                    RateControlProbePolicy::YieldingBackground,
                )
                .await
            {
                RateControlValidation::Complete(snapshot) => snapshot,
                RateControlValidation::Deferred => return Ok(None),
            };
            // A different node may have committed another complete pair while
            // this node exercised its driver. Publish only a still-current
            // request; otherwise validate the newer pair instead.
            if self.requested_rate_control().await? != (mode, quality) {
                continue;
            }
            let selected = self.encoder().await;
            self.publish_rate_control(candidate, selected);
            return Ok(Some(candidate));
        }
    }

    /// Refresh this node's validated effective state from the replicated
    /// requested pair. Each voter owns different hardware, so requested values
    /// replicate while validation remains deliberately node-local.
    pub(super) async fn refresh_rate_control(
        &self,
    ) -> Result<Option<RateControlSnapshot>, plurx_core::error::StoreError> {
        let _serial = self.rate_control_update.lock().await;
        self.refresh_rate_control_locked().await
    }

    async fn rate_control_refresh_loop_inner(self: Arc<Self>, stop_after_first: bool) {
        let start = tokio::time::Instant::now() + RATE_CONTROL_REFRESH;
        let mut interval = tokio::time::interval_at(start, RATE_CONTROL_REFRESH);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match self.refresh_rate_control().await {
                Ok(Some(_)) => {}
                Ok(None) => tracing::debug!(
                    target: "plurxd::transcode",
                    "rate-control refresh deferred for live/offline work or occupied encoder capacity"
                ),
                Err(error) => {
                    tracing::warn!(
                        target: "plurxd::transcode",
                        %error, "could not refresh replicated rate-control settings"
                    )
                }
            }
            if stop_after_first {
                return;
            }
        }
    }

    /// Keep the in-memory hot-path snapshot within the plan's two-second TTL.
    /// Store reads and behavioral probes stay entirely off session creation;
    /// live/producer paths only copy the already-published snapshot.
    pub async fn rate_control_refresh_loop(self: Arc<Self>) {
        self.rate_control_refresh_loop_inner(false).await;
    }

    #[cfg(test)]
    pub(super) async fn rate_control_refresh_once(self: Arc<Self>) {
        self.rate_control_refresh_loop_inner(true).await;
    }

    /// Load the durable request at boot, exercise the real production args,
    /// and publish only the effective result. Invalid legacy/corrupt values
    /// fall back to bitrate rather than preventing the server from starting.
    pub async fn initialize_rate_control(&self) -> Result<(), plurx_core::error::StoreError> {
        let _serial = self.rate_control_update.lock().await;
        let (mode, quality) = self.requested_rate_control().await?;
        let RateControlValidation::Complete(snapshot) = self
            .validate_rate_control_snapshot(mode, quality, RateControlProbePolicy::Boot)
            .await
        else {
            unreachable!("boot rate-control validation never defers")
        };
        if self.requested_rate_control().await? == (mode, quality) {
            let selected = self.encoder().await;
            self.publish_rate_control(snapshot, selected);
        } else {
            // Startup settings changed during the probe. The runtime refresher
            // will validate the new complete pair under background admission.
            tracing::info!(
                target: "plurxd::transcode",
                "rate-control settings changed during boot validation; deferring refresh"
            );
        }
        Ok(())
    }

    /// Validate and durably apply one complete requested setting pair.
    /// Sessions keep the old effective snapshot until every probe and both
    /// writes succeed, then all new sessions see the new one at once.
    pub async fn apply_rate_control_settings(
        &self,
        mode: RateMode,
        quality: Option<u8>,
    ) -> Result<(), ApplyRateControlError> {
        let _serial = self.rate_control_update.lock().await;
        let snapshot = match self
            .validate_rate_control_snapshot(
                Some(mode),
                quality,
                RateControlProbePolicy::YieldingBackground,
            )
            .await
        {
            RateControlValidation::Complete(snapshot) => snapshot,
            RateControlValidation::Deferred => return Err(ApplyRateControlError::Busy),
        };
        let stored_quality = quality.map(|value| value.to_string()).unwrap_or_default();
        self.store
            .put_settings(&[
                (keys::TRANSCODE_QUALITY, stored_quality.as_str()),
                (keys::TRANSCODE_RATE_MODE, mode.as_str()),
            ])
            .await?;
        if self.requested_rate_control().await? == (Some(mode), quality) {
            let selected = self.encoder().await;
            self.publish_rate_control(snapshot, selected);
        } else {
            // A concurrent writer on another voter won after our transaction.
            // Bring this process to the replicated winner before returning
            // when a safe probe window exists. If it does not, the write has
            // already completed and must not be mislabeled as the pre-write
            // Busy/409 case; the background loop will retry while the HTTP
            // response reports the replicated winner's requested pair.
            if self.refresh_rate_control_locked().await?.is_none() {
                tracing::info!(
                    target: "plurxd::transcode",
                    "concurrent rate-control winner will be validated by the background refresher"
                );
            }
        }
        Ok(())
    }

    /// The rung "Auto" means, given what this server can actually encode with.
    ///
    /// Auto was 720p for everything, which was the right answer when every
    /// transcode was software: 1080p on x264 is a session that cannot hold
    /// realtime on a NUC, and a stream that stutters at 1080p is worse than one
    /// that plays at 720p. A hardware encoder changes the arithmetic: a known
    /// SDR source follows its own resolution through the validated hardware
    /// path, while an unprobed source keeps the 1080p fallback.
    ///
    /// Link quality is not guessed here. A node-local network prior may step
    /// the result down after this capability choice, but the absence of a
    /// prior is not evidence that a 4K route cannot carry 20 Mb/s. Never
    /// upscales: a 480p source transcodes at 480p.
    ///
    /// The server decides this, not the player, because only the server knows
    /// which encoder won — the player learns that from the response it gets
    /// back *after* the height has been chosen (PERF-PLAN §4.7).
    pub async fn auto_height(
        &self,
        source_height: Option<i64>,
        prior: Option<&plurx_core::domain::NetworkPrior>,
    ) -> i64 {
        // Software's ceiling is lower; the shape is the same. It used to
        // return its ceiling unconditionally, which never *upscaled* — the
        // filter caps at the source — but advertised 720p bitrate and
        // response metadata for a 480p stream (review §3.1). The rung is a
        // promise about the output; it follows the source on both encoders.
        let encoder = self.encoder().await;
        let max = if encoder == Encoder::Software {
            AUTO_SOFTWARE_HEIGHT
        } else {
            MAX_HEIGHT
        };
        let fallback = if encoder == Encoder::Software {
            AUTO_SOFTWARE_HEIGHT
        } else {
            AUTO_HARDWARE_PROBED_HEIGHT
        };
        let current = source_height
            .filter(|h| *h > 0)
            .unwrap_or(fallback)
            .clamp(MIN_HEIGHT, max);
        auto_height_from_prior(current, source_height, prior, unix_ms())
    }

    #[cfg(test)]
    pub async fn auto_height_for_file(
        &self,
        file: Option<&plurx_core::domain::MediaFile>,
        prior: Option<&plurx_core::domain::NetworkPrior>,
    ) -> i64 {
        self.auto_height_for_request(file, prior, false).await
    }

    /// Grade-aware Auto. A web client that asked for HDR10 leaves its first
    /// height unset so this node, which alone knows the boot-proved encoder,
    /// can start at the highest measured rung.
    pub async fn auto_height_for_request(
        &self,
        file: Option<&plurx_core::domain::MediaFile>,
        prior: Option<&plurx_core::domain::NetworkPrior>,
        hdr10_requested: bool,
    ) -> i64 {
        let Some(file) = file else {
            return self.auto_height(None, prior).await;
        };
        let source_height = file.height;
        // Follow the encoder and filter graph this exact file can reach. This
        // used to apply only to Dolby Vision reshapes, leaving every ordinary
        // 4K SDR hardware transcode under a global 1080p policy cap.
        let max = self
            .capability_height_ceiling_for_request(Some(file), hdr10_requested)
            .await;
        let current = source_height
            .filter(|height| *height > 0)
            .unwrap_or(max.min(AUTO_HARDWARE_PROBED_HEIGHT))
            .clamp(MIN_HEIGHT, max);
        auto_height_from_prior(current, source_height, prior, unix_ms())
    }

    /// The highest rung this node can actually sustain for this file, before
    /// any network prior is applied.
    ///
    /// This exists because the advertised ladder was built from the source
    /// height alone while Auto was capped by the pipeline — so on a
    /// software-only node, or for a Dolby Vision source that must go through
    /// the software reshape, the response promised a 1080p rung the server
    /// would never serve. The web ABR controller believed it: it started at
    /// the 720p Auto answer, measured a JIT delivery estimate far above the
    /// 1080p bar, upgraded on schedule, got `session_failed`, took the
    /// emergency downgrade back to 720p, and repeated on a ~2 minute period.
    /// A ladder that lists a rung is a promise that the rung can be served.
    ///
    /// Deliberately excludes `auto_height_from_prior`: a network prior is a
    /// judgement about one link at one moment, and hiding rungs on that basis
    /// would keep a recovered link from ever being offered them again. This
    /// is the capability ceiling only.
    #[cfg(test)]
    pub async fn capability_height_ceiling(
        &self,
        file: Option<&plurx_core::domain::MediaFile>,
    ) -> i64 {
        self.capability_height_ceiling_for_request(file, false)
            .await
    }

    pub async fn capability_height_ceiling_for_request(
        &self,
        file: Option<&plurx_core::domain::MediaFile>,
        hdr10_requested: bool,
    ) -> i64 {
        // The exact Profile-5 → HDR10 → QSV chain was measured above realtime
        // at both 1080p and 2160p. This branch is intentionally narrower than
        // generic "hardware HDR": it requires the selected QSV family and the
        // boot proof of its Main10 upload/encode graph.
        let hdr10_renderer_proved = match file.and_then(plurx_core::playback::hdr_route) {
            Some(plurx_core::playback::HdrRoute::DolbyVisionRpu) => self.dovi_passthrough,
            Some(plurx_core::playback::HdrRoute::Passthrough) => {
                self.hdr10_passthrough && self.hdr10_passthrough_qsv
            }
            None => false,
        };
        if hdr10_requested
            && hdr10_renderer_proved
            && self.dovi_passthrough_qsv
            && self.encoder().await == Encoder::Qsv
        {
            if let Some(file) = file {
                if hdr10_rung_fits(file, HDR10_4K_HEIGHT, Encoder::Qsv) {
                    return HDR10_4K_HEIGHT;
                }
                if hdr10_rung_fits(file, HDR10_HEIGHT, Encoder::Qsv) {
                    return HDR10_HEIGHT;
                }
            }
        }
        // A Dolby Vision reshape is capped by whichever encoder it can
        // actually reach — hardware when the pairing is proved, software
        // otherwise — not unconditionally by the software rung.
        let encoder = match file {
            Some(file) if Self::needs_dovi_reshape(file) == Ok(true) => self
                .encoder_for_file(file, crate::process_control::ChildClass::Background)
                .await
                .unwrap_or(Encoder::Software),
            _ => self.encoder().await,
        };
        capability_height_for_encoder(file, encoder)
    }

    /// The admin's playback language preferences (Settings → Playback
    /// defaults), falling back to English/English/Auto.
    pub async fn lang_prefs(&self) -> plurx_core::tracks::LangPrefs {
        self.try_lang_prefs().await.unwrap_or_default()
    }

    pub(super) async fn try_lang_prefs(
        &self,
    ) -> Result<plurx_core::tracks::LangPrefs, plurx_core::error::StoreError> {
        let mut prefs = plurx_core::tracks::LangPrefs::default();
        if let Some(v) = self.store.get_setting(keys::AUDIO_LANG).await? {
            if !v.trim().is_empty() {
                prefs.audio_lang = v.trim().to_owned();
            }
        }
        if let Some(v) = self.store.get_setting(keys::SUB_LANG).await? {
            if !v.trim().is_empty() {
                prefs.sub_lang = v.trim().to_owned();
            }
        }
        if let Some(v) = self.store.get_setting(keys::SUB_MODE).await? {
            prefs.sub_mode = plurx_core::tracks::SubMode::parse(v.trim());
        }
        Ok(prefs)
    }

    /// Kill any session belonging to the same player instance.
    ///
    /// A seek is a *new* session: the client asks for a playlist starting at the
    /// new position and abandons the old one without telling anyone. Nothing in
    /// the protocol says the old session is finished, so it was left to the idle
    /// reaper — 60s idle, noticed by a 15s ticker, so up to ~75 seconds of a
    /// second ffmpeg still writing segments nobody will fetch. Scrub along a
    /// timeline and those stack: ten seeks in a minute is ten encoders (or ten
    /// remuxes reading the source flat-out), all competing for the same CPU,
    /// GPU, disk and link. That is enough on its own to starve a Wi-Fi client
    /// badly enough to cost it its DHCP lease.
    ///
    /// The key is the *player instance*, not the viewer. It used to be
    /// (viewer, file), which made two devices signed in to one account fight
    /// over the same film — each new session killing the other's — and
    /// automatic quality restarts would have turned that from a rare
    /// annoyance into a loop. A player instance restarts its own stream all
    /// the time and never anyone else's, which is exactly the scope wanted.
    async fn reap_superseded_until(
        &self,
        deadline: Option<tokio::time::Instant>,
        supersession_user: &str,
        playback_id: &str,
    ) -> Result<(), String> {
        let mut doomed: Vec<(String, Arc<Session>)> = {
            let sessions = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, self.sessions.lock())
                    .await
                    .map_err(|_| replacement_deadline_error())?,
                None => self.sessions.lock().await,
            };
            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                return Err(replacement_deadline_error());
            }
            sessions
                .iter()
                .filter(|(_, s)| {
                    s.supersession_user == supersession_user && s.playback_id == playback_id
                })
                .map(|(id, session)| (id.clone(), Arc::clone(session)))
                .collect()
        };
        // Stable ordering is not a correctness dependency, but makes the
        // first irreversible rolling victim deterministic for diagnostics and
        // for cancellation regressions that pause exactly that boundary.
        doomed.sort_by(|left, right| left.0.cmp(&right.0));
        // Transfer the complete cross-presentation victim set before entering
        // VOD or actor lifecycle waits. Cancellation of this create request
        // cannot strand a VOD-only or first-rolling-only partial sweep.
        let (settled, result) = tokio::sync::oneshot::channel();
        let vod = Arc::clone(&self.vod);
        let sessions = Arc::clone(&self.sessions);
        let retired_presentations = Arc::clone(&self.retired_presentations);
        let active_session_count = Arc::clone(&self.active_session_count);
        let store = Arc::clone(&self.store);
        let recent_marker_ambiguities = Arc::clone(&self.recent_marker_ambiguities);
        let supersession_user = supersession_user.to_owned();
        let owned_playback_id = playback_id.to_owned();
        tokio::spawn(async move {
            let outcome = own_supersession_convergence(
                vod,
                sessions,
                retired_presentations,
                active_session_count,
                store,
                recent_marker_ambiguities,
                doomed,
                deadline,
                supersession_user,
                owned_playback_id,
            )
            .await;
            let _ = settled.send(outcome);
        });
        let removed = result.await.unwrap_or_else(|_| {
            Err("supersession convergence owner exited before settlement".to_owned())
        })?;
        for (session_id, _session) in removed {
            tracing::info!(
                target: "plurxd::transcode",
                session = %session_log_id(&session_id),
                playback = %session_log_id(playback_id),
                "reaped superseded transcode session (this player started a new one)"
            );
        }
        Ok(())
    }

    /// Bound the retained legacy/process-local supersession sweep. Cluster
    /// replacements skip this break-before-make path and keep their
    /// predecessor serving until the later durable activation CAS succeeds.
    pub(super) async fn reap_superseded_before(
        &self,
        deadline: Option<tokio::time::Instant>,
        supersession_user: &str,
        playback_id: &str,
    ) -> Result<(), String> {
        self.reap_superseded_until(deadline, supersession_user, playback_id)
            .await
    }
}
