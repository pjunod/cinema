use super::*;

impl VodServe {
    /// Convert an attached prepared VOD rendition to ordinary foreground
    /// admission after its durable pointer commit. A copy rendition has no
    /// encoder and therefore needs no transition.
    pub(crate) async fn promote_prepared_session(&self, session_id: &str) -> bool {
        let rendition = self
            .shared
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.rendition.clone());
        let Some(rendition) = rendition else {
            return false;
        };
        if let Some(encoding) = rendition.recipe.encoding.as_ref() {
            encoding.promote();
        }
        true
    }

    /// A preparation gate for one live session, or `None` when this engine
    /// has no live attachment under that id.
    ///
    /// **`None` does not mean "ask the rolling actor".** This engine tracks
    /// in-flight creates in `preparing_sessions` precisely because absence
    /// from `sessions` is not absence from this engine —
    /// [`VodServe::owns_or_preparing`] exists for that question and is the one
    /// a router must ask. `None` here means only that there is no live
    /// attachment to hold a slot *right now*; a session still being created
    /// will have one shortly. Returning a gate that always answered `false`
    /// would look like a full slot and hide that distinction.
    ///
    /// The liveness read below is not a guarantee — the gate outlives it, and
    /// re-checks on every call, pinned to the exact incarnation seen here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn preparation_gate(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Option<Arc<dyn crate::playback_control::PreparationGate>> {
        let sessions = self.shared.sessions.lock().await;
        let live = sessions
            .get(session_id)
            .filter(|session| session.tombstone.is_none())
            .map(|session| Arc::downgrade(&session.incarnation));
        drop(sessions);
        live.map(|incarnation| {
            Arc::new(VodPreparationGate {
                shared: Arc::clone(&self.shared),
                session_id: session_id.to_owned(),
                incarnation,
            }) as Arc<dyn crate::playback_control::PreparationGate>
        })
    }

    /// `base` is the renditions root directory (created lazily).
    pub fn new(base: PathBuf, store: Arc<dyn Store>) -> Arc<VodServe> {
        let runtime_cache = base
            .parent()
            .unwrap_or(base.as_path())
            .join(".runtime-cache");
        Self::new_configured(base, runtime_cache, store, None, None, None)
    }

    /// Publish a producer-less VOD attachment for HTTP response-finalization
    /// tests. The fixture uses the real session/reader/incarnation machinery;
    /// only materialization and the producer driver are bypassed.
    #[cfg(test)]
    pub(crate) async fn install_http_test_session(
        &self,
        session_id: &str,
        file: MediaFile,
        base: &Path,
    ) {
        use plurx_core::fmp4::CutClass;
        use plurx_core::segplan::IndexRow;

        let mut rows = Vec::new();
        let mut dts = 0_u64;
        for index in 0..240 {
            let duration = if index % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes: 100_000,
                video_bytes: 99_400,
                class: CutClass::CleanIdr,
            });
            dts += duration;
        }
        let source_identity = SourceIdentity::new(
            u64::try_from(file.size).unwrap_or_default(),
            file.mtime,
            "http-test",
        );
        let index = FragmentIndex::new(16_000, rows, "http-test", source_identity);
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let duration_ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: duration_ms,
                audio_ms: duration_ms,
                audio_bits_per_second: 256_000,
            },
        );
        let dir = RenditionDir::new(base.join(format!("http-test-{}", uuid::Uuid::new_v4())));
        dir.create().await.expect("create HTTP VOD test rendition");
        let plan_len = plan.len();
        let rendition = Arc::new(Rendition {
            key: format!("http-test-{}", uuid::Uuid::new_v4()),
            dir,
            recipe: Recipe {
                file: file.clone(),
                audio_index: None,
                aac: true,
                video: CopyVideoOptions::new(false, false),
                source_object_version: None,
                cluster_cache_key: None,
                encoding: None,
            },
            source: None,
            playlist: plan.playlist().into_bytes(),
            timescale: plan.timescale,
            seconds_per_segment: plan.duration_ticks() as f64
                / f64::from(plan.timescale)
                / plan.len() as f64,
            index: Some(index),
            policy,
            working_set_budget: 8 << 30,
            completed_cache_budget: 50 << 30,
            materialize_budget: Duration::from_secs(30),
            manifest: Mutex::new(Manifest::new(plan.clone())),
            plan,
            identity: Mutex::new(IdentityState::default()),
            slot: ProducerSlot::new(),
            readers: Mutex::new(HashMap::new()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            capacity_hold: StdMutex::new(None),
            ahead_hold: AtomicBool::new(false),
            init_notify: Notify::new(),
            wake: Notify::new(),
            #[cfg(test)]
            stopped_poll_armed: Notify::new(),
            #[cfg(test)]
            stopped_poll_fired: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            dormant_since: StdMutex::new(None),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        });

        let lifecycle = self.shared.session_lifecycle(session_id);
        let _lifecycle = lifecycle.lock().await;
        let previous = self
            .shared
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        if let Some(previous) = previous {
            previous.detach_reader(&self.shared.pool, session_id).await;
        }
        rendition.attach_reader(session_id, 0).await;
        self.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(file.clone()),
                playback_id: "http-vod-test".to_owned(),
                user_name: "test".to_owned(),
                item_title: "HTTP VOD fixture".to_owned(),
                started_unix: 1,
                target_height: file.height.unwrap_or(0),
                kind: SessionKind::Copy {
                    aac: true,
                    preserve_dolby_vision: false,
                    convert_dolby_vision: false,
                },
                supersession_user: "[\"user_id\",1]".to_owned(),
                block_budget: Duration::from_secs(1),
                lifecycle: Arc::clone(&lifecycle),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
    }

    #[cfg(test)]
    pub(crate) async fn last_touch_for_test(&self, session_id: &str) -> Option<Instant> {
        let sessions = self.shared.sessions.lock().await;
        let touch = sessions.get(session_id)?.last_touch.lock().ok()?;
        Some(*touch)
    }

    #[cfg(test)]
    pub(crate) fn set_terminal_detach_pause_for_test(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .shared
            .terminal_detach_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    #[cfg(test)]
    pub(crate) async fn has_attached_reader_for_test(&self, session_id: &str) -> bool {
        let rendition = self
            .shared
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        let Some(rendition) = rendition else {
            return false;
        };
        let attached = rendition.readers.lock().await.contains_key(session_id);
        attached
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new_cluster(
        base: PathBuf,
        store: Arc<dyn Store>,
        node_id: String,
        cluster_index_root: PathBuf,
        membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        let runtime_cache = cluster_index_root
            .parent()
            .unwrap_or(cluster_index_root.as_path())
            .to_owned();
        Self::new_cluster_with_runtime(
            base,
            runtime_cache,
            store,
            node_id,
            cluster_index_root,
            membership,
        )
    }

    pub(crate) fn new_cluster_with_runtime(
        base: PathBuf,
        runtime_cache: PathBuf,
        store: Arc<dyn Store>,
        node_id: String,
        cluster_index_root: PathBuf,
        membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        Self::new_configured(
            base,
            runtime_cache,
            store,
            Some(node_id),
            Some(cluster_index_root),
            membership,
        )
    }

    fn new_configured(
        base: PathBuf,
        runtime_cache: PathBuf,
        store: Arc<dyn Store>,
        cluster_node_id: Option<String>,
        cluster_index_root: Option<PathBuf>,
        cluster_membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        let mut encoded_generation_scanner = EncodedGenerationScanner::new(base.clone());
        let obsolete_encoded_generations = encoded_generation_scanner.discover(
            crate::ffmpeg::encoded_process_identity(),
            ENCODED_RECONCILE_BATCH,
            ENCODED_RECONCILE_BATCH,
            &HashSet::new(),
        );
        Arc::new(VodServe {
            shared: Arc::new(Shared {
                base,
                runtime_cache,
                store,
                obsolete_encoded_generations: Mutex::new(VecDeque::from(
                    obsolete_encoded_generations,
                )),
                encoded_generation_scanner: Arc::new(StdMutex::new(encoded_generation_scanner)),
                cluster_node_id,
                cluster_index_root,
                cluster_membership,
                sessions: Mutex::new(HashMap::new()),
                preparing_sessions: StdMutex::new(HashMap::new()),
                session_lifecycles: StdMutex::new(HashMap::new()),
                rendition_builds: StdMutex::new(HashMap::new()),
                renditions: Mutex::new(HashMap::new()),
                head_regeneration_slots: Arc::new(Semaphore::new(HEAD_REGENERATION_CAPACITY)),
                pool: WaitPool::new(DEFAULT_GLOBAL_WAIT_CAP, PER_SESSION_WAIT_CAP),
                working_set: AtomicU64::new(0),
                completed_cache: AtomicU64::new(0),
                terminal_eviction_cursor: AtomicU64::new(0),
                #[cfg(test)]
                terminal_replay_pause: StdMutex::new(None),
                #[cfg(test)]
                control_applied_pause: StdMutex::new(None),
                #[cfg(test)]
                segment_ready_pause: StdMutex::new(None),
                #[cfg(test)]
                terminal_detach_pause: StdMutex::new(None),
                #[cfg(test)]
                rendition_install_pause: StdMutex::new(None),
                #[cfg(test)]
                dormant_purge_pause: StdMutex::new(None),
                #[cfg(test)]
                terminal_route_test_outcomes: StdMutex::new(HashMap::new()),
            }),
        })
    }

    /// None means this node has no source attestation yet. The caller may
    /// still use a local index; only a complete index miss queues analysis.
    pub(super) async fn try_cluster_fragment_index(
        &self,
        file: &MediaFile,
        video: CopyVideoOptions,
    ) -> Result<Option<(FragmentIndex, String, String)>, String> {
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return Err("the fragment-index engine changed; restart is required".to_owned());
        }
        let node_id = self
            .shared
            .cluster_node_id
            .as_deref()
            .ok_or_else(|| "this process has no cluster index identity".to_owned())?;
        let root = self
            .shared
            .cluster_index_root
            .as_deref()
            .ok_or_else(|| "this process has no cluster index cache root".to_owned())?;
        let object_version = crate::fragment_index_cluster::inspect_source(file).await?;
        let Some(observation) = self
            .shared
            .store
            .fragment_index_source(node_id, file.id, &object_version)
            .await
            .map_err(|error| format!("reading source attestation: {error}"))?
        else {
            return Ok(None);
        };
        let engine = crate::ffmpeg::fragment_index_engine_digest().await;
        let pipeline = crate::fragment_index_cluster::pipeline_digest(file, &engine, video);
        let cache_key = plurx_core::store::cluster_fragment_index_key(
            file.id,
            file.size,
            file.mtime,
            &observation.source_sha256,
            &pipeline,
        )
        .ok_or_else(|| "source attestation contained an invalid digest".to_owned())?;
        let now = crate::fragment_index_cluster::unix_ms();
        let repair = plurx_core::store::NewClusterFragmentIndexJob {
            cache_key: cache_key.clone(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256: observation.source_sha256.clone(),
            pipeline_sha256: pipeline.clone(),
            priority: "foreground".to_owned(),
            trigger: "foreground".to_owned(),
            target_node_id: node_id.to_owned(),
            not_before_ms: now,
            created_at_ms: now,
        };
        let artifact = self
            .shared
            .store
            .cluster_fragment_index_artifact(&cache_key)
            .await
            .map_err(|error| format!("reading cluster index catalog: {error}"))?
            .filter(|artifact| {
                artifact.source_size == file.size
                    && artifact.source_sha256 == observation.source_sha256
                    && artifact.pipeline_sha256 == pipeline
            });
        let Some(artifact) = artifact else {
            let queued = self
                .shared
                .store
                .enqueue_cluster_fragment_index(&repair)
                .await
                .map_err(|error| format!("queueing the exact v2 artifact: {error}"))?;
            return Err(if queued {
                "the exact v2 artifact is queued".to_owned()
            } else {
                "the exact v2 artifact is awaiting queue admission".to_owned()
            });
        };
        let index = crate::fragment_index_cluster::hydrate(
            self.shared.store.as_ref(),
            self.shared.cluster_membership.as_ref(),
            node_id,
            root,
            &artifact,
        )
        .await?;
        let Some(index) = index else {
            let repair = repair_job_for_artifact(repair, &artifact);
            // Lost work: without this transition the exact artifact remains
            // permanently complete even though no verified holder can serve it.
            crate::store_result::observe(
                crate::store_result::Operation::RequeueFragmentIndexNoHolder,
                crate::store_result::Discard::LostWork,
                plurx_core::store::requeue_cluster_fragment_index_after_no_holder(
                    self.shared.store.as_ref(),
                    &repair,
                )
                .await,
            );
            return Err("no verified holder could supply the v2 artifact".to_owned());
        };
        Ok(Some((index, object_version, artifact.cache_key)))
    }

    /// Keep VOD lifecycle telemetry on the same node-local event stream as
    /// every legacy presentation. The VOD registry is the only place every
    /// terminal path converges (client release, supersession, admin, revoked,
    /// replacement), so emitting here cannot miss one of those arms.
    pub(super) fn emit_lifecycle(
        &self,
        session_id: &str,
        file_id: i64,
        height: i64,
        kind: SessionKind,
        event: &str,
        reason: Option<&str>,
    ) {
        crate::telemetry::emit(
            Arc::clone(&self.shared.store),
            plurx_core::domain::PlaybackEvent {
                at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                    .unwrap_or(0),
                session_id: Some(session_log_id(session_id)),
                file_id: Some(file_id),
                event: event.to_owned(),
                method: Some(
                    match kind {
                        SessionKind::Copy { .. } => "remux",
                        SessionKind::Transcode { .. } => "transcode",
                    }
                    .to_owned(),
                ),
                encoder: Some("vod".to_owned()),
                height: Some(height),
                suspended: Some(false),
                reason: reason.map(str::to_owned),
                extra: Some(r#"{"presentation":"vod"}"#.to_owned()),
                ..plurx_core::domain::PlaybackEvent::default()
            },
        );
    }

    pub(super) fn emit_marker_prewarm(
        &self,
        session_id: &str,
        file_id: i64,
        kind: SessionKind,
        outcome: MarkerPrewarmOutcome,
    ) {
        let produced_range = outcome.produced_range.map(|range| {
            serde_json::json!({
                "first_entry": range.first,
                "last_entry": range.last,
            })
        });
        crate::telemetry::emit(
            Arc::clone(&self.shared.store),
            plurx_core::domain::PlaybackEvent {
                at_unix_ms: now_ms(),
                session_id: Some(session_log_id(session_id)),
                file_id: Some(file_id),
                event: "marker_prewarm".to_owned(),
                level: Some("info".to_owned()),
                method: Some(
                    match kind {
                        SessionKind::Copy { .. } => "remux",
                        SessionKind::Transcode { .. } => "transcode",
                    }
                    .to_owned(),
                ),
                encoder: Some("vod".to_owned()),
                detail: Some(if outcome.hit { "hit" } else { "miss" }.to_owned()),
                extra: Some(
                    serde_json::json!({
                        "presentation": "vod",
                        "marker": outcome.destination.kind.as_str(),
                        "destination_ms": outcome.destination.end_ms,
                        "destination_entry": outcome.destination.target_entry,
                        "requested_sequence": outcome.requested_sequence,
                        "produced_range": produced_range,
                    })
                    .to_string(),
                ),
                ..plurx_core::domain::PlaybackEvent::default()
            },
        );
    }
}
