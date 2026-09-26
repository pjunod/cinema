use super::*;

impl TranscodeManager {
    pub(super) fn rolling_retirement_context(&self) -> RollingRetirementContext {
        RollingRetirementContext {
            sessions: Arc::downgrade(&self.sessions),
            retired_presentations: Arc::downgrade(&self.retired_presentations),
            active_session_count: Arc::clone(&self.active_session_count),
            store: Arc::clone(&self.store),
            recent_marker_ambiguities: Arc::clone(&self.recent_marker_ambiguities),
        }
    }

    /// `pipeline` is the tone-map graph this node proved at boot — see
    /// [`crate::pipeprobe`]. It is fixed for the manager's life because it is
    /// a fact about the hardware, not a setting; the per-session filtering
    /// that decides whether a given stream may actually use it lives in
    /// [`Pipeline::for_session`].
    pub fn new(
        store: Arc<dyn Store>,
        work_dir: PathBuf,
        caps: EncoderCaps,
        pipeline: Pipeline,
    ) -> Self {
        // Managers without a finished-transcode cache (mainly tests and
        // embedded uses) still get a writable location without reaching
        // outside their scratch root. `with_cache` moves this beside the
        // persistent caches in a normal daemon.
        let runtime_cache = work_dir.join(".runtime-cache");
        let subtitle_cache = work_dir.join(".subtitle-cache");
        if let Err(err) = std::fs::create_dir_all(&runtime_cache) {
            tracing::warn!(
                target: "plurxd::transcode",
                path = %runtime_cache.display(),
                "could not create ffmpeg runtime cache: {err}"
            );
        }
        TranscodeManager {
            vod: crate::vodserve::VodServe::new(work_dir.join("renditions"), Arc::clone(&store)),
            store,
            work_dir,
            runtime_cache,
            subtitle_cache,
            subtitle_membership: None,
            subtitle_jobs: None,
            rate_control: std::sync::RwLock::new(RateControlSnapshot::bitrate(caps.quality_rc)),
            artifact_qualification: std::sync::RwLock::new(ArtifactQualificationReadiness {
                requested: false,
                measured_build: None,
                measured_decoders: Vec::new(),
                covered_decoders: Vec::new(),
                effective: plurx_core::transcode::ArtifactQualification::Unqualified,
                refusal: Some(QualificationRefusal::NotRequested),
            }),
            rate_control_update: Mutex::new(()),
            caps,
            decoders: Vec::new(),
            diagnostic_policy: std::sync::Arc::new(
                crate::decoder_health::DiagnosticPolicy::default(),
            ),
            measured_decoders: plurx_core::transcode::decoder_inventory::MeasuredDecoders::default(
            ),
            automatic_decoder_recovery: AtomicBool::new(false),
            #[cfg(test)]
            manifests_published: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            force_artifact_qualification: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            offline_produce_script: std::sync::Mutex::new(std::collections::VecDeque::new()),
            #[cfg(test)]
            offline_produced_recipes: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            fail_next_offline_recovery_begin: std::sync::atomic::AtomicBool::new(false),
            decode_facts: crate::decode_facts::DecodeFactCache::new(),
            decode_probe_identity: None,
            #[cfg(test)]
            decode_source_final_identity_delay: Duration::ZERO,
            pipeline,
            admissions: Admissions::new(),
            cache: None,
            shared_cache: None,
            scratch_bytes_free: AtomicI64::new(0),
            scratch_sampled_at_unix_ms: AtomicI64::new(0),
            scratch_sample_generation: AtomicU64::new(0),
            scratch_ledger: crate::scratch_ledger::ScratchLedger::new(),
            scratch_cap: Arc::new(AtomicI64::new(HLS_SCRATCH_MAX_BYTES_DEFAULT)),
            scratch_starved: Arc::new(tokio::sync::Notify::new()),
            cache_readers: crate::cachekeep::ActiveCacheReaders::default(),
            cache_offer_verdicts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            cache_offer_verifier: Arc::new(tokio::sync::Semaphore::new(1)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            retired_presentations: Arc::new(Mutex::new(HashMap::new())),
            recent_marker_ambiguities: Arc::new(std::sync::Mutex::new(
                RecentMarkerAmbiguityLedger::default(),
            )),
            terminal_controls: std::sync::Mutex::new(HashMap::new()),
            cluster_replacement_gates: Arc::new(ClusterReplacementGates::default()),
            session_release_gates: Arc::new(SessionReleaseGates::default()),
            serving_authority: crate::serving_fence::ServingAuthority::always_ready(),
            serving_ready: AtomicBool::new(true),
            serving_loss_generation: AtomicU64::new(0),
            active_session_count: Arc::new(AtomicUsize::new(0)),
            codec_qualification: Arc::new(CodecQualificationMetrics::default()),
            requests: std::sync::Mutex::new(HashMap::new()),
            producer: ProducerTuning::default(),
            background_producer: Mutex::new(()),
            offline_waiting: AtomicBool::new(false),
            dv_strippable: false,
            dv_convertible: false,
            dovi_reshape: false,
            dovi_passthrough: false,
            dovi_passthrough_qsv: false,
            hdr10_passthrough: false,
            hdr10_passthrough_qsv: false,
            dovi_proofs: std::sync::Mutex::new(HashMap::new()),
            cached_limits: std::sync::RwLock::new(None),
            playlist_wait_override_ms: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            subtitle_playlist_commit_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            vod_publication_admission_pause: std::sync::Mutex::new(None),
        }
    }

    /// Point the manager at a cache root, and tell it what this node's output
    /// is identified by.
    ///
    /// Separate from the constructor because both pieces come from elsewhere:
    /// the ffmpeg build from the startup probe, the node id from the store.
    /// Chaining keeps every existing call site — and every test that does not
    /// care about caching — unchanged.
    /// Record what the boot probe found out about this daemon's ffmpeg.
    /// Independent of the cache: the capabilities it gates are the daemon's,
    /// not the cache's.
    pub fn with_dv_strippable(mut self, dv_strippable: bool) -> Self {
        self.dv_strippable = dv_strippable;
        self
    }

    pub fn with_dv_convertible(mut self, dv_convertible: bool) -> Self {
        self.dv_convertible = dv_convertible;
        self
    }

    pub fn with_dovi_passthrough(mut self, dovi_passthrough: bool) -> Self {
        self.dovi_passthrough = dovi_passthrough;
        self
    }

    pub fn with_dovi_passthrough_qsv(mut self, proved: bool) -> Self {
        self.dovi_passthrough_qsv = proved;
        self
    }

    pub fn with_hdr10_passthrough(mut self, proved: bool) -> Self {
        self.hdr10_passthrough = proved;
        self
    }

    pub fn with_hdr10_passthrough_qsv(mut self, proved: bool) -> Self {
        self.hdr10_passthrough_qsv = proved;
        self
    }

    /// The tallest frame this node can actually encode HDR10 at, or 0.
    ///
    /// `/decision` needs this and cannot derive it: the renderer proofs answer
    /// "can this graph run", while the rungs are measured per
    /// (height, encoder) pair — 1080p on software or QSV, 2160p on QSV only
    /// (`hdr10_rung_fits`). Without it the negotiation promised HDR10 to every
    /// HDR title on a software-only node, whose Auto ceiling is 720p, and the
    /// session tone-mapped every one of them.
    ///
    /// The Dolby Vision route's own QSV proof is deliberately not consulted
    /// here: this is the ceiling a plain PQ source can reach, and a node that
    /// proved only the Dolby half still cannot encode one.
    pub async fn hdr10_ceiling(&self) -> i64 {
        if !self.hdr10_passthrough {
            return 0;
        }
        match self.encoder().await {
            Encoder::Qsv if self.hdr10_passthrough_qsv => HDR10_4K_HEIGHT,
            Encoder::Qsv | Encoder::Software => HDR10_HEIGHT,
            // No measured Main10 route on this family, and dropping to
            // software x265 costs realtime — see `hdr10_grade_for`.
            _ => 0,
        }
    }

    pub fn with_dovi_reshape(mut self, dovi_reshape: bool) -> Self {
        self.dovi_reshape = dovi_reshape;
        self
    }

    pub fn with_decoders(mut self, decoders: Vec<String>) -> Self {
        self.decoders = decoders;
        self
    }

    /// Install the boot measurement of which decoder this build selects.
    /// The contracts this node resolved for its own FFmpeg.
    #[must_use]
    pub fn with_diagnostic_policy(
        mut self,
        policy: std::sync::Arc<crate::decoder_health::DiagnosticPolicy>,
    ) -> Self {
        self.diagnostic_policy = policy;
        self
    }

    pub fn with_measured_decoders(
        mut self,
        measured: plurx_core::transcode::decoder_inventory::MeasuredDecoders,
    ) -> Self {
        self.measured_decoders = measured;
        self
    }

    pub fn with_decode_probe(
        mut self,
        identity: Option<crate::decode_facts::DecodeProbeIdentity>,
    ) -> Self {
        self.decode_probe_identity = identity;
        self
    }

    #[cfg(test)]
    pub(super) fn with_decode_source_final_identity_delay(mut self, delay: Duration) -> Self {
        self.decode_source_final_identity_delay = delay;
        self
    }

    /// Whether this build can strip a Dolby Vision configuration
    /// (`dovi_rpu`, ffmpeg 7.1+).
    pub fn dv_strippable(&self) -> bool {
        self.dv_strippable
    }

    /// Whether a Profile 7 source on this node can be indexed and served as a
    /// conversion. The fragment indexer asks it: an operator who turned the
    /// conversion off has no converting sessions to serve, and indexing for
    /// them would spend a third full pass over every Profile 7 remux in the
    /// library on a stream nothing can ask for.
    /// Whether this node converts Profile 7 to 8.1 right now.
    ///
    /// The builder value above is what this boot was configured with; the
    /// setting is what an operator has since answered, and it wins. Absent
    /// means on, which is the default the environment variable this replaced
    /// also had: a Profile 7 title reaching a Dolby Vision client as HDR10 is
    /// what the conversion exists to stop, so it should not need enabling.
    ///
    /// A store that cannot be read falls back to the boot value rather than to
    /// a constant. Losing the setting is not a reason to start or stop doing
    /// work on somebody's GPU.
    pub(crate) async fn dv_convert_enabled(&self) -> bool {
        match self
            .store
            .get_setting(plurx_core::store::keys::DV_CONVERT)
            .await
        {
            Ok(stored) => plurx_core::store::stored_switch(stored.as_deref(), true),
            Err(_) => self.dv_convertible,
        }
    }

    #[cfg(test)]
    pub fn with_cache(self, cache_dir: PathBuf, ffmpeg_build: String, node_id: String) -> Self {
        let cache_parent = cache_dir.parent().unwrap_or(cache_dir.as_path()).to_owned();
        self.with_cache_layout(
            cache_dir,
            cache_parent.join("runtime"),
            cache_parent.join("subs"),
            cache_parent.join("renditions"),
            ffmpeg_build,
            node_id,
            None,
        )
    }

    /// Configure every managed cache path explicitly. Startup canonicalizes
    /// and validates these paths before scratch cleanup; deriving siblings
    /// from a canonicalized `transcode` leaf would move them when only that
    /// legacy child is a relocation symlink.
    #[allow(clippy::too_many_arguments)]
    pub fn with_cache_layout(
        mut self,
        cache_dir: PathBuf,
        runtime_cache: PathBuf,
        subtitle_cache: PathBuf,
        rendition_cache: PathBuf,
        ffmpeg_build: String,
        node_id: String,
        cluster_membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Self {
        self.runtime_cache = runtime_cache;
        self.subtitle_cache = subtitle_cache;
        self.subtitle_membership = cluster_membership.clone();
        // Renditions are durable state — admitted ones are the copy cache the
        // plan promises — so they live beside the persistent caches rather
        // than in scratch. Replaced before serving starts, like the caches.
        self.vod = crate::vodserve::VodServe::new_cluster_with_runtime(
            rendition_cache,
            self.runtime_cache.clone(),
            Arc::clone(&self.store),
            node_id.clone(),
            crate::fragment_index_cluster::cache_root(&self.runtime_cache),
            cluster_membership,
        );
        if let Err(err) = std::fs::create_dir_all(&self.runtime_cache) {
            tracing::warn!(
                target: "plurxd::transcode",
                path = %self.runtime_cache.display(),
                "could not create ffmpeg runtime cache: {err}"
            );
        }
        self.cache = Some(CacheConfig {
            dir: cache_dir,
            ffmpeg_build,
            node_id,
        });
        self
    }

    pub(crate) fn with_subtitle_jobs(mut self, jobs: Arc<crate::state::JobManager>) -> Self {
        self.subtitle_jobs = Some(jobs);
        self
    }

    pub(super) fn subtitle_source_access(&self) -> crate::subtitle_source::StoreAccess {
        let access = crate::subtitle_source::StoreAccess::from_setting(
            Arc::clone(&self.store),
            &self.runtime_cache,
        )
        .on_node(self.cache_location().map(|(_, node_id)| node_id));
        let access = match &self.subtitle_membership {
            Some(membership) => access.with_membership(membership.clone()),
            None => access,
        };
        match &self.subtitle_jobs {
            Some(jobs) => access.with_jobs(Arc::clone(jobs)),
            None => access,
        }
    }

    pub fn with_shared_cache(
        mut self,
        shared_cache: Arc<crate::shared_cache::SharedCacheCoordinator>,
    ) -> Self {
        self.shared_cache = Some(shared_cache);
        self
    }

    pub(crate) fn with_serving_authority(
        mut self,
        serving_authority: crate::serving_fence::ServingAuthority,
    ) -> Self {
        self.serving_authority = serving_authority;
        self
    }

    pub fn cache_readers(&self) -> &crate::cachekeep::ActiveCacheReaders {
        &self.cache_readers
    }

    /// Pin a shared-cache-backed worker before its durable route is exposed.
    /// Non-cache and node-local sessions need no distributed pin and succeed
    /// immediately.
    pub(crate) async fn pin_shared_session(
        &self,
        session_id: &str,
        incarnation_id: &str,
        owner_epoch: i64,
        expires_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let session = self.sessions.lock().await.get(session_id).cloned();
        let Some(location) = session
            .as_ref()
            .and_then(|session| session.cache_location.as_ref())
            .filter(|location| location.storage_class == "shared")
        else {
            return Ok(true);
        };
        let Some(generation_id) = location.generation_id.as_ref() else {
            return Ok(false);
        };
        self.store
            .acquire_cache_consumer_pin(
                &CacheConsumerPin {
                    storage_id: location.node_id.clone(),
                    recipe_hash: location.recipe_hash.clone(),
                    generation_id: generation_id.clone(),
                    consumer_kind: CacheConsumerKind::MediaSession,
                    consumer_id: incarnation_id.to_owned(),
                    consumer_epoch: owner_epoch,
                    expires_at_ms,
                },
                crate::media_sessions::unix_ms(),
            )
            .await
    }

    #[cfg(test)]
    pub fn begin_cache_eviction_for_test(&self, recipe: &str) -> impl Drop {
        self.cache_readers
            .begin_eviction(recipe)
            .expect("test eviction claim")
    }

    /// Blocked-GET admission counters for the operator surfaces.
    ///
    /// Read through `self.vod` at call time rather than captured once. This
    /// manager replaces its own `VodServe` on the cluster boot path, so a
    /// handle taken at construction can address a pool nothing serves from —
    /// and an admission counter that quietly stops moving is worse than no
    /// counter, because it reads as a quiet node.
    pub(crate) fn blocked_get_metrics_handle(
        &self,
    ) -> std::sync::Arc<crate::waitpool::BlockedGetMetrics> {
        self.vod.blocked_get_metrics_handle()
    }

    /// Narrow process-metrics handle with no session map or Store access.
    pub(crate) fn metrics_handle(&self) -> TranscodeMetrics {
        TranscodeMetrics {
            active_sessions: Arc::clone(&self.active_session_count),
            active_cache: self.cache_readers.metrics(),
            decode_facts: self.decode_facts.metrics_handle(),
            caps: self.caps.clone(),
            codec_qualification: Arc::clone(&self.codec_qualification),
        }
    }

    /// Record one accepted encoding start after its serving owner is
    /// registered. For rolling this is manager registration, for VOD it is
    /// the reader attachment returned by `try_create`, and Live TV calls it
    /// only after the first publishable scratch inventory crosses its serving
    /// fence. Callers that do not execute a video pipeline (copy/remux) must
    /// not call this method.
    pub(crate) fn record_codec_qualification_session(
        &self,
        encoder: Encoder,
        grade: OutputGrade,
        pipeline: Option<Pipeline>,
    ) {
        self.codec_qualification.record_encoder(encoder, grade);
        if let Some(pipeline) = pipeline {
            self.codec_qualification.record_pipeline(pipeline);
        }
    }

    #[cfg(test)]
    pub(crate) fn codec_qualification_encoder_count(
        &self,
        encoder: Encoder,
        grade: OutputGrade,
    ) -> u64 {
        self.codec_qualification.encoder_sessions[CodecQualificationMetrics::encoder_slot(encoder)]
            [CodecQualificationMetrics::grade_slot(grade)]
        .load(Relaxed)
    }

    #[cfg(test)]
    pub(super) fn codec_qualification_pipeline_count(&self, pipeline: Pipeline) -> u64 {
        self.codec_qualification.pipeline_sessions
            [CodecQualificationMetrics::pipeline_slot(pipeline)]
        .load(Relaxed)
    }

    /// Override [`ProducerTuning`]. Tests only — there is deliberately no
    /// production path that reaches this, so the daemon cannot be configured
    /// into pacing a producer or into a shorter retry by accident.
    #[cfg(test)]
    pub(super) fn with_producer_tuning(mut self, producer: ProducerTuning) -> Self {
        self.producer = producer;
        self
    }

    /// Where finished transcodes live and what this node is called there, or
    /// `None` when no cache root is configured.
    ///
    /// Handed out rather than duplicated in the caller: the housekeeping sweep
    /// and the serving path have to agree about both, and two copies of "the
    /// cache root" is how they come to disagree after somebody makes one
    /// configurable.
    /// Where ffmpeg's own caches go, so a background child gets the same
    /// environment a session's does — an unset `XDG_CACHE_HOME` makes
    /// fontconfig rebuild its cache on every spawn.
    pub fn runtime_cache_dir(&self) -> &std::path::Path {
        self.runtime_cache.as_path()
    }

    pub fn cache_location(&self) -> Option<(&std::path::Path, &str)> {
        self.cache
            .as_ref()
            .map(|c| (c.dir.as_path(), c.node_id.as_str()))
    }

    /// Refresh free cache space outside Tokio's blocking pool.
    ///
    /// Only one OS call exists at a time. If a dead mount never returns, this
    /// loop consumes interval ticks without submitting another call, while
    /// request paths keep using the last completed (or fail-closed zero)
    /// sample only until its maximum age.
    pub(crate) async fn scratch_space_loop(self: Arc<Self>) {
        let Some(cache_dir) = self.cache.as_ref().map(|cache| cache.dir.clone()) else {
            return;
        };
        let mut interval = tokio::time::interval(SCRATCH_SAMPLE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let (sender, mut receiver) = tokio::sync::oneshot::channel();
            let sample_path = cache_dir.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("plurx-scratch-sample".to_owned())
                .spawn(move || {
                    let _ = sender.send(available_cache_scratch_bytes(&sample_path));
                })
            {
                tracing::debug!(
                    target: "plurxd::transcode",
                    %error, "could not start cache scratch sampler"
                );
            }
            loop {
                tokio::select! {
                    sample = &mut receiver => {
                        let sample = sample.ok().flatten().unwrap_or(0).max(0);
                        self.scratch_sample_generation.fetch_add(1, AcqRel);
                        self.scratch_bytes_free.store(sample, Relaxed);
                        self.scratch_sampled_at_unix_ms.store(unix_ms(), Relaxed);
                        self.scratch_sample_generation.fetch_add(1, Release);
                        break;
                    }
                    _ = interval.tick() => {
                        // The outstanding OS call is deliberately left alone.
                        // Do not submit another one until it actually returns.
                    }
                }
            }
        }
    }

    /// Bounded claim filter for the distributed speculative queue.
    pub fn pretranscode_capabilities(&self) -> PretranscodeWorkerCapabilities {
        let mut encoder_families = vec!["software".to_owned()];
        for (available, family) in [
            (self.caps.nvenc, "nvenc"),
            (self.caps.qsv, "qsv"),
            (self.caps.vaapi, "vaapi"),
            (self.caps.videotoolbox, "videotoolbox"),
        ] {
            if available {
                encoder_families.push(family.to_owned());
            }
        }
        PretranscodeWorkerCapabilities {
            version: plurx_core::domain::PretranscodeRequirements::VERSION,
            decoders: self.decoders.clone(),
            encoder_families,
            max_target_height: if self.caps.nvenc
                || self.caps.qsv
                || self.caps.vaapi
                || self.caps.videotoolbox
            {
                MAX_HEIGHT
            } else {
                AUTO_SOFTWARE_HEIGHT
            },
            output_contracts: vec!["hls-mpegts-v1".to_owned()],
            // The selected graph is a boot-proved pipeline (with CPU as the
            // explicit fallback), but an operator can still disable mapping.
            tone_map: self.pipeline.handles(Some("hdr10")) && tone_map_pref() != ToneMap::None,
            output_grades: vec!["sdr".to_owned()],
            scratch_bytes: {
                let (bytes, sampled_at) = read_scratch_sample(
                    &self.scratch_sample_generation,
                    &self.scratch_bytes_free,
                    &self.scratch_sampled_at_unix_ms,
                );
                fresh_scratch_bytes(bytes, sampled_at, unix_ms())
            },
        }
    }

    /// Snapshot media capacity without probing a source mount.
    pub(crate) async fn media_node_runtime(&self) -> MediaNodeRuntime {
        const SCRATCH_TARGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
        let capabilities = self.pretranscode_capabilities();
        let (hardware_slots_used, hardware_slots_max) = self.hardware_slots().await;
        let software_threads_max = self.software_budget().await;
        let software_threads_used = self.admissions.software_in_use();
        let session_pressure_limit = hardware_slots_max
            .saturating_add((software_threads_max / 2).max(1))
            .max(1);
        let mut tone_map_pipelines = if capabilities.tone_map {
            vec![self.pipeline.name().to_owned()]
        } else {
            Vec::new()
        };
        if self.dovi_passthrough {
            tone_map_pipelines.push(Pipeline::DoviPassthrough.name().to_owned());
        }
        // Advertised for the same reason as the Dolby rung: the cluster picks
        // a node for HDR work off this list, and a node that proved only the
        // plain HDR10 encode -- no tone-map graph, no Dolby filter -- would
        // otherwise publish an empty list and be filtered out of the work it
        // is the best answer for.
        if self.hdr10_passthrough {
            tone_map_pipelines.push(Pipeline::Hdr10Passthrough.name().to_owned());
        }
        MediaNodeRuntime {
            scratch_bytes_free: u64::try_from(capabilities.scratch_bytes.max(0)).unwrap_or(0),
            scratch_target_bytes: SCRATCH_TARGET_BYTES,
            active_sessions: self.active_session_count.load(Relaxed),
            session_pressure_limit,
            encoder_families: capabilities.encoder_families,
            max_target_height: capabilities.max_target_height,
            decoders: capabilities.decoders,
            tone_map_pipelines,
            hardware_slots_used,
            hardware_slots_max,
            software_threads_used,
            software_threads_max,
            live_waiting: self.admissions.live_is_waiting(),
            background_active: self.admissions.background_is_active(),
        }
    }

    /// Inspect whether this node could service one request. Offers are
    /// intentionally non-reserving and never stat/open `file.path`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn media_offer_probe(
        &self,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        hdr10: bool,
    ) -> Result<MediaOfferProbe, &'static str> {
        let needs_source_proof =
            Self::needs_dovi_reshape(file).map_err(|_| "unsupported_source")?;
        if needs_source_proof {
            if !self.dovi_reshape {
                return Err("incapable");
            }
            let proof = self
                .dovi_proofs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&Self::dovi_proof_key(file))
                .copied();
            if proof != Some(true) {
                // The authoritative live path may create this proof for the
                // selected node. Diagnostics fan-out must never be what opens
                // every candidate's source mount.
                return Err("source_proof_unavailable");
            }
        }
        let capabilities = self.pretranscode_capabilities();
        let decoder_supported = crate::media_pool::decoder_contract(file).is_some_and(|decoder| {
            capabilities
                .decoders
                .iter()
                .any(|candidate| candidate == decoder || candidate == "*")
        });
        let geometry_supported =
            (MIN_HEIGHT..=capabilities.max_target_height).contains(&target_height);
        // The grade with no burn, read before the tracks are chosen, because
        // it is what decides whether an implicit burn may be chosen at all.
        let base_grade = self.grade_preview(file, hdr10, target_height, None).await;
        let Tracks {
            audio_index,
            subtitle_burn,
        } = self
            .select_tracks(
                file,
                audio_override,
                subtitle_override,
                base_grade == OutputGrade::Hdr10,
            )
            .await;
        let (encoder, grade) = self
            .encoder_and_grade_for(file, hdr10, target_height, subtitle_burn.is_some())
            .await
            .map_err(|_| "incapable")?;
        let target_supported = geometry_supported && (!hdr10 || grade == OutputGrade::Hdr10);
        let opts = self.live_lookup_options(
            self.rate_control_snapshot(),
            encoder,
            file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn,
            None,
            grade,
        );
        // An offer cannot bind or stat the source, and it does not need to:
        // the artifact key is the catalog identity plus stored probe facts,
        // both of which this node already has. `cache_hit` is an *eligibility*
        // input downstream, not a preference — a node holding a complete
        // byte-verified generation whose source is momentarily unreadable is
        // exactly the node that should serve, and answering `false` here would
        // refuse it. Plan resolution failing means we cannot name the artifact,
        // so we claim nothing and fail closed.
        let cache_hit = match self.resolve_movie_plan(file, &opts, encoder).await {
            Ok(plan) => self.verified_cache_hit(&plan).await,
            Err(reason) => {
                tracing::debug!(
                    target: "plurxd::transcode",
                    file_id = file.id, %reason, "offer cannot name an artifact"
                );
                false
            }
        };
        let (hardware_used, hardware_max) = self.hardware_slots().await;
        let software_max = self.software_budget().await;
        let software_used = self.admissions.software_in_use();
        let active_sessions = self.active_session_count.load(Relaxed);
        let session_pressure_limit = hardware_max
            .saturating_add((software_max / 2).max(1))
            .max(1);
        let workload = Workload::of(file, target_height);
        Ok(MediaOfferProbe {
            active_sessions,
            session_pressure_limit,
            scratch_bytes_free: u64::try_from(capabilities.scratch_bytes.max(0)).unwrap_or(0),
            decoder_supported,
            target_supported,
            cache_hit,
            free_hardware_slots: hardware_max.saturating_sub(hardware_used),
            free_software_threads: software_max.saturating_sub(software_used),
            encoder: encoder.family_name().to_owned(),
            pipeline: opts.pipeline.name().to_owned(),
            recent_speed: self
                .admissions
                .recent_speed(&workload.class(encoder.family_name())),
        })
    }

    /// Identity for every mutable input that can change a speculative recipe
    /// without changing the source snapshot or target height.
    #[cfg(test)]
    pub async fn pretranscode_policy_generation(&self) -> String {
        self.pretranscode_policy_snapshot().await.generation
    }

    #[cfg(test)]
    pub async fn pretranscode_policy_snapshot(&self) -> PretranscodePolicySnapshot {
        self.try_pretranscode_policy_snapshot()
            .await
            .unwrap_or_else(|_| {
                let rate_control = self.rate_control_snapshot();
                let prefs = plurx_core::tracks::LangPrefs::default();
                let requested_encoder = String::new();
                PretranscodePolicySnapshot {
                    generation: Self::pretranscode_policy_generation_for(
                        rate_control,
                        &requested_encoder,
                        &prefs,
                    ),
                    requested_encoder,
                    rate_control,
                    prefs,
                }
            })
    }

    pub async fn try_pretranscode_policy_snapshot(
        &self,
    ) -> Result<PretranscodePolicySnapshot, plurx_core::error::StoreError> {
        let rate_control = self.rate_control_snapshot();
        let prefs = self.try_lang_prefs().await?;
        let requested_encoder = self
            .store
            .get_setting(keys::HWACCEL)
            .await?
            .unwrap_or_default();
        let generation =
            Self::pretranscode_policy_generation_for(rate_control, &requested_encoder, &prefs);
        Ok(PretranscodePolicySnapshot {
            generation,
            requested_encoder,
            rate_control,
            prefs,
        })
    }

    pub(super) async fn pretranscode_policy_interruption(
        &self,
        expected: &str,
    ) -> Option<OfflineProduceOutcome> {
        match self.try_pretranscode_policy_snapshot().await {
            Ok(policy) if policy.generation == expected => None,
            Ok(_) => Some(OfflineProduceOutcome::PolicyChanged),
            Err(error) => {
                tracing::warn!(
                    target: "plurxd::transcode",
                    %error, "speculative publication could not verify transcode policy"
                );
                Some(OfflineProduceOutcome::Yielded)
            }
        }
    }

    /// Cluster-stable speculative output geometry.
    ///
    /// Candidate generation must not inherit the scheduler node's local GPU.
    /// Automatic/unknown policy therefore chooses the universally claimable
    /// software rung, while an explicit hardware-family request may queue the
    /// source rung (HDR remains at the broadly proved 1080p tone-map ceiling).
    /// Claiming independently enforces each worker's proved height ceiling.
    pub(super) fn pretranscode_target_height_for(
        file: &plurx_core::domain::MediaFile,
        requested_encoder: &str,
    ) -> i64 {
        let explicit_hardware = matches!(
            requested_encoder,
            "nvenc" | "qsv" | "vaapi" | "videotoolbox"
        );
        let ceiling = if !explicit_hardware {
            AUTO_SOFTWARE_HEIGHT
        } else if file.hdr.is_some() {
            AUTO_HARDWARE_PROBED_HEIGHT
        } else {
            MAX_HEIGHT
        };
        ladder(file.height)
            .into_iter()
            .find(|rung| rung.height <= ceiling)
            .map_or(MIN_HEIGHT, |rung| rung.height)
    }

    /// Speculative dedupe must cover the same mutable policy inputs as track
    /// selection and recipe construction. Normalize language aliases so a
    /// spelling-only settings edit does not create useless replacement work.
    pub(super) fn pretranscode_policy_generation_for(
        snapshot: RateControlSnapshot,
        requested_encoder: &str,
        prefs: &plurx_core::tracks::LangPrefs,
    ) -> String {
        let audio_lang =
            plurx_core::tracks::bcp47_tag(Some(&prefs.audio_lang)).to_ascii_lowercase();
        let sub_lang = plurx_core::tracks::bcp47_tag(Some(&prefs.sub_lang)).to_ascii_lowercase();
        let mut hasher = Sha256::new();
        for value in [
            format!("recipe:{CACHE_RECIPE_VERSION}"),
            "contract:hls-mpegts-v1".to_owned(),
            format!("requested-encoder:{requested_encoder}"),
            // Unset spells exactly what explicit `bitrate` spells. This is a
            // durable dedupe key: it is stored on every speculative queue row
            // and a mismatch is a hard `cancel_job(.., "policy_changed")`, not
            // a yield. Every `Encoder::default_rate_mode()` is Bitrate, so the
            // two requests resolve to the same effective policy and the key
            // must not move because the internal type gained a third state —
            // it would re-queue every speculative row at deploy and flip again
            // mid-boot on every restart, when the manager's initial
            // `RateControlSnapshot::bitrate` is replaced by the absent pair.
            // A PR that flips a family default moves this spelling
            // deliberately, with the artefact §3.4 requires.
            format!(
                "requested:{}",
                snapshot
                    .requested_mode
                    .map_or_else(|| RateMode::Bitrate.as_str(), RateMode::as_str)
            ),
            format!("quality:{:?}", snapshot.requested_quality),
            format!("audio-lang:{audio_lang}"),
            format!("subtitle-lang:{sub_lang}"),
            format!("subtitle-mode:{}", prefs.sub_mode.as_str()),
        ] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        format!("speculative-auto-v2:{}", hex::encode(hasher.finalize()))
    }

    /// Speculative work never reserves a queue row while foreground/offline
    /// encoding already owns or is waiting for this node's capacity. A race
    /// after this observation is still resolved by admission before ffmpeg.
    pub fn pretranscode_worker_idle(&self) -> bool {
        !self.admissions.live_is_waiting()
            && self.admissions.in_use() == 0
            && self.admissions.software_in_use() == 0
            && !self
                .offline_waiting
                .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn subtitle_cache_dir(&self) -> &std::path::Path {
        &self.subtitle_cache
    }
}
