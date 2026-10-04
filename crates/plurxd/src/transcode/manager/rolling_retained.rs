//! Local attachment of independently qualified process-private rolling output.
use super::*;

impl TranscodeManager {
    pub(super) async fn begin_rolling_retention(
        &self,
        production: Option<&Arc<crate::rolling_provenance::RollingProduction>>,
        duration_ms: Option<i64>,
        source_bytes: i64,
    ) -> Option<Arc<crate::vodserve::retained::RollingCollection>> {
        let duration_ms = u64::try_from(duration_ms?).ok()?;
        if duration_ms == 0 {
            return None;
        }
        // No new timer. Each actual publication and completion checks this
        // finite deadline; the ordinary session/collector owns cleanup.
        let lifetime_ms = duration_ms.checked_mul(4)?.checked_add(300_000)?;
        if lifetime_ms > 24 * 60 * 60 * 1000 {
            return None;
        }
        let budget = self.rolling_retained_budget().await?;
        // Reserve the same bounded allocation the complete-output VOD path
        // queues with (twice the source plus 64 MiB generation headroom), not
        // the whole remaining budget: a collection lives as long as its
        // session, and an unbounded hold starves preparations and any second
        // collection. A capture that outgrows it refuses retention.
        let estimate = u64::try_from(source_bytes)
            .ok()?
            .checked_mul(2)?
            .checked_add(64 * 1024 * 1024)?
            .min(budget);
        self.vod
            .begin_rolling_collection(
                Arc::clone(production?),
                budget,
                estimate,
                Instant::now().checked_add(Duration::from_millis(lifetime_ms))?,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)] // exact current owner and resolved presentation
    pub(super) async fn attach_rolling_retained(
        &self,
        production: &Arc<crate::rolling_provenance::RollingProduction>,
        frozen: FrozenHlsPresentation,
        kind: SessionKind,
        contract: Option<&plurx_core::transcode::PresentationContract>,
        audio: Option<plurx_core::playback::audio::AudioDelivery>,
        grade: OutputGrade,
        height: i64,
        user_name: &str,
        supersession_user: &str,
        playback_id: &str,
        item_title: &str,
        automatic: bool,
    ) -> Option<StartInfo> {
        let artifact = self.vod.acquire_rolling_output(production)?;
        let manifest = artifact.manifest()?;
        let mut context = frozen.context;
        context.bandwidth = Some(artifact.bandwidth()?);
        let frozen = FrozenHlsPresentation::from_rolling_artifact(
            frozen.file,
            context,
            &kind,
            contract,
            &artifact,
        );
        let file = frozen.file.clone();
        let control =
            crate::playback_control::RollingControlHandle::spawn("retained-rolling-attach");
        let failed = Arc::new(AtomicBool::new(false));
        control
            .bind_response_publication_contract(
                frozen.contract_fingerprint.clone(),
                Arc::clone(&failed),
            )
            .await
            .ok()?;
        let session_id = uuid::Uuid::new_v4().to_string();
        let method = if matches!(kind, SessionKind::Copy { .. }) {
            crate::delivery::Method::HlsCopy
        } else {
            crate::delivery::Method::Transcode
        };
        let session = Arc::new(Session {
            dir: artifact.directory.clone(),
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: Some(frozen),
            rolling_provenance: None,
            rolling_collection: None,
            rolling_artifact: Some(Arc::clone(&artifact)),
            copy_output_measurement: std::sync::Mutex::new(None),
            actor_managed_response_publication: true,
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
            retirement_context: Some(self.rolling_retirement_context()),
            cache_integrity_cleanup_started: AtomicBool::new(false),
            child: Mutex::new(None),
            child_transition: Mutex::new(()),
            replacing_child: AtomicBool::new(false),
            terminal_response_pending: Arc::new(AtomicBool::new(false)),
            terminal_control: std::sync::Mutex::new(None),
            hooks: crate::seam_hooks::HookSlot::new(&NoopSessionHooks),
            cached: true,
            _cache_reader: None,
            subtitle_handle: None,
            #[cfg(windows)]
            source_handle: None,
            #[cfg(windows)]
            output_handle: None,
            cache_manifest: Some(manifest),
            cache_location: None,
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id: file.id,
            item_id: file.item_id,
            item_title: item_title.to_owned(),
            user_name: user_name.to_owned(),
            supersession_user: supersession_user.to_owned(),
            playback_id: playback_id.to_owned(),
            recovery: None,
            automatic,
            kind,
            audio_delivery: audio.clone(),
            method,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            grade,
            target_height: height,
            tone_map_peak_nits: None,
            tone_map_peak_source: None,
            encoder_label: Mutex::new("retained"),
            started_unix: unix_ms() / 1000,
            failed,
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(true),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: None,
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope: 0,
            upload: None,
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::new()),
            class: std::sync::Mutex::new(String::new()),
            hw_slot: std::sync::Mutex::new(None),
            sw_permit: std::sync::Mutex::new(None),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::for_method(method.metric_label()),
            http_waits: HttpWaitLedger::default(),
            readrate: 0.0,
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover: None,
            first_slide_logged: AtomicBool::new(false),
        });
        // Source/engine equality is checked after actor await and immediately
        // before the real attachment. Received rates/locator never enter here.
        if !artifact.current_for_attachment(production).await {
            return None;
        }
        self.register_session(&session_id, session, 0).await.ok()?;
        if !artifact.current_for_attachment(production).await {
            self.stop_session(&session_id, "source_changed").await;
            return None;
        }
        Some(StartInfo {
            retained_output: None,
            audio_delivery: audio,
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: file.duration_ms,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            target_height: height,
            kind,
            encoder: "retained",
            grade,
            vod: true,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }
}
