use super::*;

/// A live HLS session published into a real [`TranscodeManager`], plus the
/// [`AppState`](crate::state::AppState) in front of it.
///
/// Delivery accounting spans two modules: the tracker and its classifications
/// live here, and the response body that drives them lives in `http::hls`. A
/// test of the tracker alone pins arithmetic, not the correction — so the HTTP
/// layer needs a way to stand a session up without ffmpeg. `Session` stays
/// private; this hands out only the two facts a delivery test reads.
#[cfg(test)]
pub(crate) struct HlsDeliveryFixture {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) state: crate::state::AppState,
    pub(super) session: Arc<Session>,
    _executor_registration: Option<crate::playback_control::RollingProducerExecutorRegistration>,
}

#[cfg(test)]
impl HlsDeliveryFixture {
    /// Install a compatibility serving window without publishing first media
    /// to the control actor. Response-race tests use this narrow state to open
    /// predecessor bytes before an actor-authorized successor attempt begins.
    pub(crate) async fn make_segment_window_servable(&self) {
        let mut index = self.session.segments.lock().await;
        if index.segs.is_empty() {
            for segment in 0..12_i64 {
                index.segs.push(SegmentMeta {
                    index: segment,
                    name: format!("seg{segment:05}.ts"),
                    start_ms: segment * 4_000,
                    end_ms: (segment + 1) * 4_000,
                    bytes: 0,
                    visibility: SegmentVisibility::Advertised,
                });
            }
            index.revision = index.revision.saturating_add(1);
        }
        drop(index);
        let producer_attempt = self.session.control.current_producer_attempt();
        self.session.publication.lock().await.served = Some(ServedPlaylistSnapshot {
            raw: Arc::from(&b"#EXTM3U\n"[..]),
            producer_attempt,
            revision: 1,
            last_segment: 11,
            first_segment: 0,
            end_ms: 48_000,
            duration_ms: 48_000,
            end_list: false,
            available_at: Instant::now(),
        });
    }

    /// Mark this synthetic session as having presented media to its client.
    ///
    /// A fixture session has an empty segment index, which is exactly the
    /// shape of a session that has not started — and a `hold` from a client
    /// that has not started is deliberately ignored, because obeying it is the
    /// startup deadlock (see
    /// `a_starting_client_cannot_hold_a_session_that_has_published_nothing`).
    /// A test about steady-state flow control has to say it is past that point
    /// rather than borrow the startup exemption by accident.
    pub(crate) async fn mark_started(&self) {
        let mut index = self.session.segments.lock().await;
        if index.segs.is_empty() {
            // A published rolling fixture must satisfy the same initial
            // runway as production. Seed the ordinary four-second grid so
            // HTTP tests can resolve any of the early segment names they
            // materialize without borrowing a pre-publication state.
            for segment in 0..16_i64 {
                index.segs.push(SegmentMeta {
                    index: segment,
                    name: format!("seg{segment:05}.ts"),
                    start_ms: segment * 4_000,
                    end_ms: (segment + 1) * 4_000,
                    bytes: 0,
                    visibility: SegmentVisibility::Advertised,
                });
            }
            index.revision = index.revision.saturating_add(1);
        }
        drop(index);
        let producer_attempt = self.session.control.current_producer_attempt();
        let accepted = self
            .session
            .control
            .observe_publication(crate::playback_control::RollingPublicationObservation {
                producer_attempt,
                publication_commit: true,
                demand_sequence: None,
                produced_segment: Some(15),
                produced_end_ms: Some(64_000),
                playlist_ready: true,
                published_segment: Some(11),
                published_end_ms: Some(48_000),
                published_first_segment: Some(0),
                published_start_ms: Some(0),
                media_origin_ms: 0,
                next_media_sequence: 12,
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            })
            .await;
        assert!(accepted, "fixture publication must reach the control actor");
        let mut publication = self.session.publication.lock().await;
        if publication.served.is_none() {
            publication.served = Some(ServedPlaylistSnapshot {
                raw: Arc::from(&b"#EXTM3U\n"[..]),
                producer_attempt,
                revision: 1,
                last_segment: 11,
                first_segment: 0,
                end_ms: 48_000,
                duration_ms: 48_000,
                end_list: false,
                available_at: Instant::now(),
            });
        }
        publication.staged_attempt = Some(producer_attempt);
        publication.staged_last_segment = Some(15);
        publication.staged_end_ms = Some(64_000);
        drop(publication);
        // A started fixture is one whose actor has accepted presentation, not
        // merely one whose producer filled the old publication floor.
        self.session.playlist_published.store(true, Relaxed);
        self.session.control.mark_startup_presented_for_test().await;
    }

    /// Publish a producer-less session under `session_id`, serving whatever
    /// files the caller writes into `dir`.
    pub(crate) async fn publish(dir: &std::path::Path, session_id: &str) -> Self {
        Self::publish_with_takeover(dir, session_id, None, false, false).await
    }

    pub(crate) async fn publish_actor_managed(dir: &std::path::Path, session_id: &str) -> Self {
        Self::publish_with_takeover(dir, session_id, None, false, true).await
    }

    pub(crate) async fn publish_copy_actor_managed(
        dir: &std::path::Path,
        session_id: &str,
    ) -> Self {
        Self::publish_with_takeover(dir, session_id, None, true, true).await
    }

    pub(crate) async fn publish_takeover(
        dir: &std::path::Path,
        session_id: &str,
        incarnation_id: &str,
        owner_epoch: i64,
    ) -> Self {
        Self::publish_with_takeover(
            dir,
            session_id,
            Some(SessionTakeoverStart {
                provisional_session_id: session_id.to_owned(),
                incarnation_id: incarnation_id.to_owned(),
                origin_base_ms: 0,
                frontier_offset_ms: 0,
                media_sequence: owner_epoch
                    .saturating_mul(crate::media_sessions::TAKEOVER_SEQUENCE_STRIDE),
                discontinuity_sequence: owner_epoch.saturating_sub(1),
                owner_epoch,
            }),
            false,
            false,
        )
        .await
    }

    pub(super) async fn publish_with_separate_state_root(
        session_dir: &std::path::Path,
        state_root: &std::path::Path,
        session_id: &str,
    ) -> Self {
        Self::publish_with_takeover_and_state_root(
            session_dir,
            state_root,
            session_id,
            None,
            false,
            false,
        )
        .await
    }

    async fn publish_with_takeover(
        dir: &std::path::Path,
        session_id: &str,
        takeover: Option<SessionTakeoverStart>,
        copy: bool,
        actor_managed: bool,
    ) -> Self {
        Self::publish_with_takeover_and_state_root(
            dir,
            dir,
            session_id,
            takeover,
            copy,
            actor_managed,
        )
        .await
    }

    async fn publish_with_takeover_and_state_root(
        session_dir: &std::path::Path,
        state_root: &std::path::Path,
        session_id: &str,
        takeover: Option<SessionTakeoverStart>,
        copy: bool,
        actor_managed: bool,
    ) -> Self {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "L".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Heat".into(),
                year: Some(1995),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = store
            .upsert_file(
                item,
                "/media/Heat.mkv",
                1,
                1,
                &ProbeResult {
                    duration_ms: Some(6_000_000),
                    ..ProbeResult::default()
                },
            )
            .await
            .expect("file");

        let (control, mut executor_registration) = if actor_managed {
            let (control, registration) = if copy {
                crate::playback_control::RollingControlHandle::spawn_prepublication_producer(
                    "test-copy-start",
                )
            } else {
                crate::playback_control::RollingControlHandle::spawn_prepublication_transcode(
                    "test-transcode-start",
                )
            };
            (control, Some(registration))
        } else {
            (
                crate::playback_control::RollingControlHandle::spawn("test-start"),
                None,
            )
        };
        let mut raw_session = test_session_with_control(session_dir.to_path_buf(), control);
        raw_session.takeover = takeover;
        raw_session.file_id = file_id;
        if copy {
            raw_session.kind = SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            };
            raw_session.method = crate::delivery::Method::HlsCopy;
            raw_session.encoder_label = Mutex::new("copy");
        }
        let frozen_file = store
            .get_file(file_id)
            .await
            .expect("fixture file lookup")
            .expect("fixture file");
        raw_session.frozen_presentation = Some(FrozenHlsPresentation::new(
            frozen_file,
            HlsContext {
                file_id,
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                codecs: "avc1.640034,mp4a.40.2".into(),
                supplemental_codecs: None,
                frame_rate: None,
            },
            &raw_session.kind,
        ));
        if let Some(registration) = executor_registration.as_mut() {
            registration
                .register()
                .await
                .expect("register actor-managed fixture executor");
            let presentation_contract = raw_session
                .frozen_presentation
                .as_ref()
                .expect("fixture presentation")
                .contract_fingerprint
                .clone();
            raw_session
                .control
                .bind_response_publication_contract(
                    presentation_contract.clone(),
                    Arc::clone(&raw_session.failed),
                )
                .await
                .expect("bind actor-managed fixture response contract");
            let policy = if copy {
                crate::playback_control::InitialProducerPolicy::copy_immediate(
                    presentation_contract,
                    PROGRESS_STALL,
                )
            } else {
                crate::playback_control::InitialProducerPolicy::software(
                    presentation_contract,
                    PROGRESS_STALL,
                )
            };
            let attempt = raw_session
                .control
                .begin_initial_producer_attempt(policy)
                .await
                .expect("admit actor-managed fixture producer");
            raw_session.bind_retry_compatibility_attempt(attempt).await;
            raw_session.actor_managed_response_publication = true;
            raw_session.actor_managed_prepublication_process = true;
            raw_session
                .actor_prepublication_producer
                .store(true, Release);
        }
        let session = Arc::new(raw_session);
        let state = crate::state::AppState::new(
            "test".into(),
            Arc::clone(&store),
            crate::state::Dirs {
                artwork: state_root.join("artwork"),
                transcode: state_root.join("transcode"),
                cache: state_root.join("cache"),
                subs: state_root.join("subs"),
                runtime_cache: state_root.join("runtime"),
                renditions: state_root.join("renditions"),
            },
            "test-node".into(),
            EncoderCaps::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        state
            .transcode
            .register_session_for_test(session_id, Arc::clone(&session))
            .await;
        Self {
            store,
            state,
            session,
            _executor_registration: executor_registration,
        }
    }

    pub(crate) async fn hold_actor_managed_producer(&self) {
        let attempt = self.session.control.current_producer_attempt();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(matches!(
            self.session
                .control
                .request_producer_flow_before(attempt, true, deadline)
                .await,
            crate::playback_control::ProducerFlowIntentionOutcome::Issue { hold: true, .. }
        ));
        {
            let transition = self.session.control.lock_producer_transition();
            assert!(self
                .session
                .control
                .reserve_producer_flow_applied(&transition, attempt));
            self.session
                .control
                .finish_producer_flow_applied(&transition, attempt, true, true);
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self
                    .session
                    .control
                    .snapshot()
                    .await
                    .is_some_and(|snapshot| snapshot.producer_control.physical_flow == "held")
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor applies fixture hold");
    }

    /// What this session's delivery meter has recorded — the figure behind
    /// `delivered_bps` in status and in every playback event.
    pub(crate) fn delivered_bytes(&self) -> i64 {
        self.session.delivery.total_bytes()
    }

    /// Seed one closed delivery-rate window without making an HTTP test sleep
    /// for the meter's 1.5-second minimum.
    pub(crate) fn set_delivered_bps_for_test(&self, bits_per_second: i64) {
        self.session.delivery.idle_for_test(2_000);
        self.session
            .delivery
            .note(u64::try_from(bits_per_second / 4).unwrap_or_default());
    }

    pub(crate) fn file_id(&self) -> i64 {
        self.session.file_id
    }

    pub(crate) async fn last_renewal_kind(&self) -> &'static str {
        self.session
            .control
            .snapshot()
            .await
            .expect("fixture control actor")
            .last_renewal_kind
    }

    pub(crate) async fn actor_snapshot(&self) -> crate::playback_control::RollingLeaseSnapshot {
        self.session
            .control
            .snapshot()
            .await
            .expect("fixture control actor")
    }

    pub(crate) fn fetched_segment(&self) -> i64 {
        self.session.high_segment.load(Relaxed)
    }

    pub(crate) async fn actor_delivery(&self) -> crate::playback_control::RollingDeliverySnapshot {
        self.session
            .control
            .snapshot()
            .await
            .expect("fixture control actor")
            .delivery
    }

    pub(crate) async fn wait_for_delivery_projection(
        &self,
        expected_kind: &'static str,
        expected_fetched_segment: Option<i64>,
    ) -> crate::playback_control::RollingDeliverySnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = self
                    .session
                    .control
                    .snapshot()
                    .await
                    .expect("fixture control actor");
                if snapshot.last_renewal_kind == expected_kind
                    && snapshot.delivery.fetched_segment == expected_fetched_segment
                {
                    return snapshot.delivery;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached response completion reached the control actor")
    }

    pub(crate) async fn refresh_frozen_presentation_from_store(&mut self, session_id: &str) {
        let file = self
            .store
            .get_file(self.session.file_id)
            .await
            .expect("fixture file lookup")
            .expect("fixture file");
        let registered = self
            .state
            .transcode
            .sessions
            .lock()
            .await
            .remove(session_id)
            .expect("registered fixture session");
        assert!(Arc::ptr_eq(&registered, &self.session));
        drop(registered);

        let session = Arc::get_mut(&mut self.session)
            .expect("fixture owns the only session reference while rebuilding presentation");
        let context = session
            .frozen_presentation
            .as_ref()
            .expect("frozen fixture presentation")
            .context
            .clone();
        session.frozen_presentation =
            Some(FrozenHlsPresentation::new(file, context, &session.kind));
        self.state
            .transcode
            .register_session_for_test(session_id, Arc::clone(&self.session))
            .await;
    }

    pub(crate) async fn begin_producer_attempt(
        &self,
    ) -> Result<u64, crate::playback_control::ProducerAttemptRejection> {
        let producer_attempt = self.session.control.begin_producer_attempt().await?;
        self.session
            .reset_compatibility_delivery(producer_attempt)
            .await;
        Ok(producer_attempt)
    }

    pub(crate) async fn hold_child_transition(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.session.child_transition.lock().await
    }

    pub(crate) fn pause_response_projection(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .session
            .response_projection_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    pub(crate) fn pause_playlist_publication(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .session
            .playlist_publication_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    pub(crate) fn pause_control_after_acceptance(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .session
            .control_applied_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    pub(crate) async fn worker_is_registered(&self, session_id: &str) -> bool {
        self.state
            .transcode
            .sessions
            .lock()
            .await
            .contains_key(session_id)
    }

    pub(crate) async fn reaper_pass_keeps_worker(&self, session_id: &str) -> bool {
        let Some(session) = self
            .state
            .transcode
            .sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
        else {
            return false;
        };
        matches!(
            self.state
                .transcode
                .session_reap_verdict(session_id.to_owned(), session)
                .await,
            SessionReapVerdict::Live(_, _)
        )
    }

    /// Every `segment_delivery_*` row recorded so far, once at least `want` of
    /// them have landed. Telemetry is written off the request path.
    pub(crate) async fn delivery_events(&self, want: usize) -> Vec<PlaybackEvent> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let events: Vec<_> = self
                    .store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: None,
                        limit: 50,
                    })
                    .await
                    .expect("delivery telemetry query")
                    .into_iter()
                    .filter(|event| event.event.starts_with("segment_delivery_"))
                    .collect();
                if events.len() >= want {
                    return events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("delivery telemetry persisted")
    }

    /// The same read, after giving any pending telemetry a chance to land —
    /// for asserting that a path recorded *nothing*.
    pub(crate) async fn settle(&self) -> Vec<PlaybackEvent> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        self.store
            .playback_events(&plurx_core::domain::PlaybackEventQuery {
                since_ms: None,
                event: None,
                limit: 50,
            })
            .await
            .expect("delivery telemetry query")
            .into_iter()
            .filter(|event| event.event.starts_with("segment_delivery_"))
            .collect()
    }
}

/// A session with no encoder behind it, for exercising the index, the
/// retention window and the pruner without spawning ffmpeg. The child is a
/// real (idle) process because `Session` owns one; nothing here signals it.
#[cfg(test)]
pub(super) fn test_session(dir: PathBuf) -> Session {
    test_session_with_control(
        dir,
        crate::playback_control::RollingControlHandle::spawn("test-start"),
    )
}

#[cfg(test)]
fn test_session_with_control(
    dir: PathBuf,
    control: crate::playback_control::RollingControlHandle,
) -> Session {
    let child = tokio::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn placeholder child");
    Session {
        dir,
        recovery: None,
        response_incarnation: uuid::Uuid::new_v4(),
        frozen_presentation: None,
        actor_managed_response_publication: false,
        actor_managed_prepublication_process: false,
        actor_prepublication_producer: Arc::new(AtomicBool::new(false)),
        response_publication_transition: Mutex::new(()),
        first_media_handoff_applied: AtomicBool::new(false),
        first_media_handoff_notify: tokio::sync::Notify::new(),
        prepublication_cleanup_active: AtomicBool::new(false),
        retirement_cleanup_started: AtomicBool::new(false),
        retirement_cleanup_finished: AtomicBool::new(false),
        retirement_settlement: std::sync::Mutex::new(None),
        scratch_cleanup_started: AtomicBool::new(false),
        retirement_context: None,
        cache_integrity_cleanup_started: AtomicBool::new(false),
        child: Mutex::new(Some(AttemptChild::new(0, child, control.clone(), None))),
        child_transition: Mutex::new(()),
        replacing_child: AtomicBool::new(false),
        replacement_pause: std::sync::Mutex::new(None),
        activity_detail_pause: std::sync::Mutex::new(None),
        control_applied_pause: std::sync::Mutex::new(None),
        terminal_response_pending: Arc::new(AtomicBool::new(false)),
        terminal_control: std::sync::Mutex::new(None),
        flow_completion_pause: std::sync::Mutex::new(None),
        playlist_publication_pause: std::sync::Mutex::new(None),
        producer_install_pause: std::sync::Mutex::new(None),
        refresh_after_read_pause: std::sync::Mutex::new(None),
        path_owner_sample_pause: std::sync::Mutex::new(None),
        retention_delete_pause: std::sync::Mutex::new(None),
        response_projection_pause: std::sync::Mutex::new(None),
        #[cfg(test)]
        first_media_owner_claim_pause: std::sync::Mutex::new(None),
        retirement_started: AtomicBool::new(false),
        #[cfg(test)]
        retirement_cleanup_handoff_pause: std::sync::Mutex::new(None),
        #[cfg(test)]
        scratch_cleanup_pause: std::sync::Mutex::new(None),
        cached: false,
        _cache_reader: None,
        subtitle_handle: None,
        #[cfg(windows)]
        source_handle: None,
        #[cfg(windows)]
        output_handle: None,
        cache_manifest: None,
        cache_location: None,
        control,
        publication: Mutex::new(RollingPublicationClock::default()),
        publication_worker_started: AtomicBool::new(false),
        flow_worker_started: AtomicBool::new(false),
        file_id: 1,
        item_id: 1,
        item_title: "T".into(),
        user_name: "paul".into(),
        supersession_user: serde_json::json!(["username", "paul"]).to_string(),
        playback_id: "pb-test".into(),
        // The steppable shape: server-chosen height, and a kind that agrees
        // with `method` and `target_height` below. A stall reopen bound to
        // this session may move it down the ladder, so a test that wants the
        // sticky case has to say so by overriding `automatic`.
        automatic: true,
        kind: SessionKind::Transcode { height: 720 },
        method: crate::delivery::Method::Transcode,
        start_seconds: 0.0,
        media_origin_seconds: 0.0,
        grade: OutputGrade::Sdr,
        target_height: 720,
        tone_map_peak_nits: None,
        tone_map_peak_source: None,
        encoder_label: Mutex::new("test"),
        started_unix: 0,
        failed: Arc::new(AtomicBool::new(false)),
        failure: std::sync::Mutex::new(None),
        playlist_published: AtomicBool::new(false),
        high_segment: Arc::new(AtomicI64::new(-1)),
        compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
        fetched_end_ms: Arc::new(AtomicI64::new(0)),
        segments: Mutex::new(SegmentIndex::default()),
        ahead_bytes: AtomicI64::new(0),
        live_bytes: Arc::new(AtomicI64::new(0)),
        scratch: None,
        retired_release: Arc::new(RetiredRelease::new()),
        scratch_envelope: 0,
        retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
        retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
        retention_cleanup_active: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(Progress::new()),
        class: std::sync::Mutex::new(String::new()),
        hw_slot: std::sync::Mutex::new(None),
        sw_permit: std::sync::Mutex::new(None),
        sw_delta_permit: std::sync::Mutex::new(None),
        delivery: Meter::new(),
        http_waits: HttpWaitLedger::default(),
        readrate: 0.0,
        suspended: AtomicBool::new(false),
        suspended_at: Mutex::new(None),
        suspend_count: AtomicU64::new(0),
        takeover: None,
        first_slide_logged: AtomicBool::new(false),
    }
}
