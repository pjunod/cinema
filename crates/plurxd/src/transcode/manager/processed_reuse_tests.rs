use super::*;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
use plurx_core::playback::audio::AudioClaim;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};

use plurx_core::transcode::dv_processing::*;
use std::sync::atomic::AtomicBool;

// Test-only, one-shot pauses keyed by the unique successor id. No production
// polling, global policy or mutable behavior is installed in release builds.
#[derive(Default)]
struct PreparationPause {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

type PauseMap = std::collections::HashMap<(String, &'static str), Arc<PreparationPause>>;
static PAUSES: std::sync::OnceLock<std::sync::Mutex<PauseMap>> = std::sync::OnceLock::new();

impl TranscodeManager {
    pub(super) async fn processed_preparation_test_boundary(session: &str, boundary: &'static str) {
        let pause = PAUSES
            .get_or_init(Default::default)
            .lock()
            .expect("preparation pauses")
            .remove(&(session.to_owned(), boundary));
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }
}

fn pause_at(session: &str, boundary: &'static str) -> Arc<PreparationPause> {
    let pause = Arc::new(PreparationPause::default());
    assert!(PAUSES
        .get_or_init(Default::default)
        .lock()
        .expect("preparation pauses")
        .insert((session.to_owned(), boundary), Arc::clone(&pause))
        .is_none());
    pause
}

async fn prepare(f: &Fixture, session: &str, json: &str) -> bool {
    f.manager
        .vod_prepare_first_before(
            json,
            session,
            f.user_id,
            PreparedVodOwner {
                node_id: "node".into(),
                source_authority: None,
            },
            f.manager.session_adoption_token(session).expect("adoption"),
            Some(1),
            Instant::now() + Duration::from_secs(10),
        )
        .await
}

fn spawn_prepare(f: &Fixture, session: &str, json: &str) -> tokio::task::JoinHandle<bool> {
    let manager = Arc::clone(&f.manager);
    let session = session.to_owned();
    let json = json.to_owned();
    let user_id = f.user_id;
    tokio::spawn(async move {
        manager
            .vod_prepare_first_before(
                &json,
                &session,
                user_id,
                PreparedVodOwner {
                    node_id: "node".into(),
                    source_authority: None,
                },
                manager.session_adoption_token(&session).expect("adoption"),
                Some(1),
                Instant::now() + Duration::from_secs(10),
            )
            .await
    })
}

async fn wait_for_cleanup(f: &Fixture, session: &str, owner: &crate::vodserve::ResponseOwner) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.manager.vod.response_owner_is_live(session, owner).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successor-only cleanup");
    assert!(f.manager.vod.hls_facts(&f.session_id).await.is_some());
    assert_eq!(f.encoding.admissions.in_use(), 1);
    assert_eq!(f.encoding.admissions.software_in_use(), 15);
}

#[tokio::test]
async fn staged_processed_cancellation_at_attachment_facts_and_seal_releases_only_successor() {
    for boundary in ["attached", "facts", "before-seal", "sealed"] {
        let f = fixture(DvDestination::Hdr10).await;
        let (_, session, json) = stage(&f).await;
        let pause = pause_at(&session, boundary);
        let work = spawn_prepare(&f, &session, &json);
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .expect(boundary);
        let owner = f
            .manager
            .vod
            .hls_facts(&session)
            .await
            .expect("attached")
            .response_owner;
        work.abort();
        assert!(work.await.expect_err("cancelled").is_cancelled());
        wait_for_cleanup(&f, &session, &owner).await;
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
    }
}

#[tokio::test]
async fn staged_processed_epoch_motion_after_attachment_refuses_frozen_seal() {
    let f = fixture(DvDestination::Profile81).await;
    let (_, session, json) = stage(&f).await;
    let pause = pause_at(&session, "before-seal");
    let work = spawn_prepare(&f, &session, &json);
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .expect("at seal");
    let owner = f
        .manager
        .vod
        .hls_facts(&session)
        .await
        .expect("attached")
        .response_owner;
    let now = crate::media_sessions::unix_ms() + 120_000;
    let adopted = f
        .manager
        .store
        .claim_media_session_takeover(&plurx_core::domain::MediaSessionTakeover {
            incarnation_id: f.remote.incarnation_id.clone(),
            expected_owner_node_id: "node".into(),
            expected_owner_epoch: 1,
            next_owner_node_id: "survivor".into(),
            now_ms: now,
            lease_expires_at_ms: now + 60_000,
        })
        .await
        .expect("takeover")
        .expect("new epoch");
    assert_eq!(adopted.owner_epoch, 2);
    pause.release.notify_one();
    assert!(
        !work.await.expect("preparation"),
        "must not refresh the admitted epoch"
    );
    wait_for_cleanup(&f, &session, &owner).await;
    f.manager
        .vod
        .end(&f.session_id, crate::vodserve::Terminal::Deleted)
        .await;
}

// Byte-bound lifecycle controls, not Dolby pixel/reference-accuracy evidence.
// The real completed-window validator checks the generated HEVC/PQ stream;
// the exclusive worker claim prevents the fixture renderer from ever running.
struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<TranscodeManager>,
    remote: crate::media_sessions::RemoteStartRequest,
    encoding: Arc<crate::vodencode::Encoding>,
    worker: Option<crate::vodencode::EncodeWorkerPermit>,
    receipt: DvProcessingReceipt,
    payloads: Vec<DvDigest>,
    user_id: i64,
    session_id: String,
}

fn expectation(f: &Fixture, incarnation: &str, session: &str) -> StagedProcessedPredecessor {
    StagedProcessedPredecessor {
        session_id: f.session_id.clone(),
        incarnation_id: f.remote.incarnation_id.clone(),
        owner_node_id: "node".into(),
        owner_epoch: 1,
        successor_session_id: session.to_owned(),
        successor_incarnation_id: incarnation.to_owned(),
    }
}

#[tokio::test]
async fn staged_processed_root_refuses_current_input_and_owner_drift_without_spending_capacity() {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let f = fixture(destination).await;
        let (incarnation, session, json) = stage(&f).await;
        let expected = expectation(&f, &incarnation, &session);
        let original: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&json).expect("wire");
        let mut positive = original.clone();
        drop(
            f.manager
                .restore_staged_processed_root(
                    &mut positive,
                    &expected,
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .expect("exact positive control"),
        );
        for mutation in [
            "height",
            "destination",
            "caps",
            "audio-offset",
            "audio-delivery",
            "audio-claim",
            "audio-track",
            "source-size",
            "file",
            "missing-destination",
            "previous-session",
            "geometry",
            "codec",
            "digest",
            "playback",
            "principal",
            "request-id",
        ] {
            let mut changed = original.clone();
            match mutation {
                "height" => changed.request.kind = SessionKind::Transcode { height: 32 },
                "destination" => {
                    changed.request.dv_processing_destination =
                        Some(if destination == DvDestination::Hdr10 {
                            DvDestination::Profile81
                        } else {
                            DvDestination::Hdr10
                        })
                }
                "caps" => {
                    changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .caps
                        .video[0]
                        .max_height = Some(32)
                }
                "audio-offset" => changed.request.audio_offset_ms = 1,
                "audio-delivery" => changed.request.audio_delivery = None,
                "audio-claim" => changed.request.audio_claim = None,
                "audio-track" => changed.request.audio_index = Some(987),
                "source-size" => changed.source_size += 1,
                "file" => changed.request.file_id += 1,
                "missing-destination" => changed.request.dv_processing_destination = None,
                "previous-session" => {
                    changed.request.previous_session_id = Some(f.session_id.clone())
                }
                "geometry" => {
                    changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .candidate
                        .width += 2
                }
                "codec" => {
                    changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .candidate
                        .planned_codec = Some("h264".into())
                }
                "digest" => {
                    let candidate = &mut changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .candidate;
                    candidate.recipe_digest = [92; 32];
                    candidate.id = CandidateId::for_recipe_digest(candidate.recipe_digest);
                    changed.candidate_id = Some(candidate.id);
                }
                "playback" => changed.request.playback_id = "another-play".into(),
                "principal" => {
                    changed.principal =
                        plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                            user_id: f.user_id + 1,
                        }
                }
                "request-id" => changed.request.request_id = Some(uuid::Uuid::new_v4().to_string()),
                _ => unreachable!(),
            }
            assert!(
                f.manager
                    .restore_staged_processed_root(
                        &mut changed,
                        &expected,
                        Instant::now() + Duration::from_secs(5)
                    )
                    .await
                    .is_err(),
                "{mutation}"
            );
            assert!(
                f.manager.vod.hls_facts(&session).await.is_none(),
                "refusal installed no successor"
            );
            assert_eq!(f.encoding.admissions.in_use(), 1);
            assert_eq!(f.encoding.admissions.software_in_use(), 15);
        }
        for (node, epoch) in [("node", 2), ("remote", 1)] {
            let mut changed = original.clone();
            let mut wrong = expectation(&f, &incarnation, &session);
            wrong.owner_node_id = node.into();
            wrong.owner_epoch = epoch;
            assert!(f
                .manager
                .restore_staged_processed_root(
                    &mut changed,
                    &wrong,
                    Instant::now() + Duration::from_secs(5)
                )
                .await
                .is_err());
        }
        assert!(
            f.manager.vod.hls_facts(&f.session_id).await.is_some(),
            "incumbent remains usable"
        );
        f.manager
            .store
            .put_setting(keys::SW_POOL_THREADS, "14")
            .await
            .expect("planning drift");
        let mut changed = original.clone();
        assert!(
            f.manager
                .restore_staged_processed_root(
                    &mut changed,
                    &expected,
                    Instant::now() + Duration::from_secs(5)
                )
                .await
                .is_err(),
            "a new planning generation cannot borrow the old graph"
        );
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
    }
}

#[tokio::test]
async fn first_processed_attachment_arms_exact_owner_cleanup_before_returning() {
    let f = fixture(DvDestination::Hdr10).await;
    let (incarnation, session, json) = stage(&f).await;
    let cleanup = FirstPreparationCleanup {
        vod: Arc::clone(&f.manager.vod),
        session_id: session.clone(),
        owner: Arc::new(std::sync::Mutex::new(None)),
    };
    assert!(
        f.manager
            .resurrect_vod_from_recipe_before(
                &json,
                &session,
                f.user_id,
                f.manager
                    .session_adoption_token(&session)
                    .expect("adoption"),
                Instant::now() + Duration::from_secs(5),
                VodRecipeAttachment::FirstPreparation {
                    owner: PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    bootstrap: false,
                    processed_predecessor: Some(expectation(&f, &incarnation, &session)),
                    attachment_owner: Arc::clone(&cleanup.owner)
                }
            )
            .await
    );
    let owner = cleanup
        .owner
        .lock()
        .expect("sink")
        .clone()
        .expect("armed by synchronous registry commit");
    drop(cleanup);
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.manager.vod.response_owner_is_live(&session, &owner).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled successor cleaned up");
    assert!(
        f.manager.vod.hls_facts(&f.session_id).await.is_some(),
        "cleanup never retires incumbent"
    );
    assert_eq!(f.encoding.admissions.in_use(), 1);
    assert_eq!(f.encoding.admissions.software_in_use(), 15);
    f.manager
        .vod
        .end(&f.session_id, crate::vodserve::Terminal::Deleted)
        .await;
}

fn digest(bytes: &[u8]) -> DvDigest {
    DvDigest::new(hex::encode(Sha256::digest(bytes))).expect("bounded digest")
}

async fn fixture(destination: DvDestination) -> Fixture {
    use std::os::unix::fs::MetadataExt;
    let root = crate::test_tempdir().expect("processed lifecycle");
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    let user = store
        .create_user("processed", "hash", false)
        .await
        .expect("user");
    let library = store
        .create_library(&NewLibrary {
            name: "processed".into(),
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
            title: "lifecycle control".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = root.path().join("source.mp4");
    std::fs::write(
        &path,
        include_bytes!("../../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4"),
    )
    .expect("source bytes");
    let metadata = std::fs::metadata(&path).expect("metadata");
    let probe = serde_json::json!({"format":{"duration":"2.0"},"streams":[{
        "index":0,"codec_type":"video","codec_name":"hevc","profile":"Main 10",
        "width":64,"height":64,"sample_aspect_ratio":"1:1","pix_fmt":"yuv420p10le",
        "avg_frame_rate":"24/1","r_frame_rate":"24/1","field_order":"progressive",
        "color_transfer":"smpte2084","color_primaries":"bt2020","color_space":"bt2020nc",
        "side_data_list":[{"side_data_type":"DOVI configuration record","dv_profile":7,
            "dv_level":6,"rpu_present_flag":1,"el_present_flag":1,"bl_present_flag":1,
            "dv_bl_signal_compatibility_id":1}]
    }]});
    let file_id = store
        .upsert_file(
            item,
            path.to_str().expect("path"),
            metadata.len() as i64,
            metadata.mtime(),
            &plurx_core::scan::probe::parse_probe_json(&probe),
        )
        .await
        .expect("file");
    for (key, value) in [
        (keys::DV_HDR_PROCESSING, "1"),
        (keys::DV_FEL_REENCODE, "1"),
        (keys::DV_CONVERT, "1"),
        (keys::SW_POOL_THREADS, "15"),
        (keys::MAX_HW_SESSIONS, "1"),
    ] {
        store.put_setting(key, value).await.expect("policy");
    }
    let manager = Arc::new(
        TranscodeManager::new(
            store.clone(),
            root.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_cache(
            root.path().join("cache"),
            crate::ffmpeg::ffmpeg_build().await,
            "node".into(),
        ),
    );
    let snapshot = store
        .playback_planning_snapshot(file_id, &QUALITY_PLANNING_KEYS)
        .await
        .expect("planning")
        .expect("file");
    let file = &snapshot.file;
    assert_eq!(file.dolby_vision.profile, Some(7));
    let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
        "v":2,"video":[{"codec":"hevc","profiles":["main10"],"dv_profiles":[8],"present":["hdr10","pq"],"max_width":64,"max_height":64}],
        "containers":["mp4"],"transports":["hls"],"hls_hevc_sample_entries":["hvc1"],"dv_transport":"hls",
        "audio":["aac"],"audio_sinks":[{"codec":"aac","max_channels":2,"sample_rates_hz":[48000]}]
    })).expect("caps");
    let query = crate::media_pool::QualityCatalogRequest {
        file_id,
        source_size: file.size,
        source_mtime: file.mtime,
        caps: caps.clone(),
        audio_index: None,
        audio_offset_ms: 0,
        audio_claim: AudioClaim::from_caps(&caps).expect("claim"),
        audio_delivery: None,
        subtitle_burn: None,
        presentation: Presentation::Vod,
        copy_contract: Some(if destination == DvDestination::Profile81 {
            (false, true, true)
        } else {
            (false, false, false)
        }),
    };
    let projected = TranscodeManager::processed_catalog_query(&snapshot, &query)
        .expect("canonical encoded audio");
    let options = TranscodeOptions {
        target_height: 64,
        pipeline: Pipeline::Hdr10Passthrough,
        video_bitrate_kbps: 300,
        audio: projected.audio_delivery.clone(),
        ..Default::default()
    };
    let facts =
        TranscodeManager::quality_facts_from_snapshot(&snapshot).expect("fresh decode facts");
    let plan = manager
        .resolve_movie_plan_with_facts(
            file,
            &options,
            Encoder::Software,
            &facts,
            &AttemptRestrictions::none(),
        )
        .expect("PQ encoder plan");
    let raster = (
        plan.output_contract().effective_width().expect("width"),
        plan.output_contract().effective_height().expect("height"),
    );
    let output = root.path().join("completed.mp4");
    let status = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=black:size=64x64:rate=24",
            "-frames:v",
            "3",
            "-an",
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p10le",
            "-preset",
            "ultrafast",
            "-x265-params",
            "pools=none:frame-threads=1:bframes=0:keyint=3:colorprim=9:transfer=16:colormatrix=9",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "smpte2084",
            "-colorspace",
            "bt2020nc",
            "-video_track_timescale",
            "24",
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
        ])
        .arg(&output)
        .kill_on_drop(true)
        .output()
        .await
        .expect("encode control window");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let mut raw = dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(include_bytes!(
        "../../../../plurx-core/tests/fixtures/dv-runtime/frame-000.nal"
    ))
    .expect("authored control RPU");
    let dm = raw.vdr_dm_data.as_mut().expect("metadata");
    dm.remove_metadata_level(10);
    dm.remove_metadata_level(255);
    for curve in &mut raw.rpu_data_mapping.as_mut().expect("mapping").curves {
        let polynomial = curve.polynomial.as_mut().expect("affine control");
        polynomial.poly_coef_int[0][0] = 0;
        polynomial.poly_coef_int[0][1] = 1;
        polynomial.poly_coef[0][0] = 0;
        polynomial.poly_coef[0][1] = 0;
    }
    raw.modified = true;
    let rpus = vec![raw.write_hevc_unspec62_nalu().expect("bounded control RPU"); 3];
    let keys: Vec<_> = (0..3)
        .map(|ordinal| DvFrameKey {
            absolute_video_index: 0,
            continuity_epoch: 0,
            pts: DvTimestamp::new(ordinal, 24).expect("clock"),
            display_ordinal: ordinal as u64,
        })
        .collect();
    let bytes = std::fs::read(output).expect("completed bytes");
    let bytes = if destination == DvDestination::Profile81 {
        dv_author_profile81_window(&bytes, raster, 24, 0, 1, &rpus, 6)
            .expect("actual reconstructed P81 authoring")
    } else {
        bytes
    };
    let source = Arc::new(
        crate::fragment_index_cluster::open_source_fence(file, None)
            .await
            .expect("held source"),
    );
    let backend = DvBackendIdentity::new(
        digest(b"lifecycle renderer"),
        digest(b"lifecycle libraries"),
        digest(b"lifecycle parser"),
        digest(b"lifecycle graph"),
        1,
        digest(b"lifecycle environment"),
    )
    .expect("backend control");
    let source_facts = DvSourceFacts::new(
        file.dolby_vision.clone(),
        0,
        plurx_core::transcode::DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(
            std::fs::read(&path).expect("source hash"),
        )))
        .expect("source identity"),
        plurx_core::transcode::DecodeCacheIdentity::from_media_file(file),
        plurx_core::transcode::PlanSourceBinding::DescriptorBound,
        backend.parser_identity().clone(),
        DvRpuState::ValidatedFresh,
        DvElKind::Fel,
        Some(DvSourceCoverage::sampled(0, keys.clone()).expect("coverage")),
        Some(DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7BaseYuv420P10Limited).expect("BL")),
        Some(DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7ElYuv420P10Residual).expect("EL")),
    )
    .expect("source facts");
    let preferences = TranscodeManager::vod_dv_preferences_from_snapshot(&snapshot);
    let selected_plan = dv_plan_completed_window(DvCompletedWindowInput {
        source: source_facts,
        preferences,
        backend: backend.clone(),
        destination,
        encoder: Encoder::Software,
        mode: DvProcessingMode::Fel,
        source_keys: &keys,
        source_rpus: &rpus,
        source_tick: (1, 24),
        grid: plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid"),
        first_global_frame: 0,
        encoded: &bytes,
        raster,
        export_policy: DvExportPolicy::PqBilinearYuv420p10V1 { raster },
        source_trace: digest(b"lifecycle trace"),
        observed_at: 1,
        decoder: plurx_core::transcode::DecodeBackend::Software,
    })
    .expect("real completed-window validation");
    let receipt = dv_receipt_completed_window(
        &selected_plan,
        &keys,
        &[DvDuration::new(1, 24, DvDurationProvenance::DeclaredFixtureInterval).expect("duration");
            3],
        &rpus,
    )
    .expect("checked control receipt");
    let payloads = vec![digest(&bytes)];
    let runtime = Arc::new(crate::vodencode::dv_runtime::RuntimeRecipe {
        tools: Arc::new(crate::vodencode::dv_runtime::ToolSet::lifecycle_fixture(
            backend,
        )),
        source: Arc::clone(&source),
        source_offsets: Arc::new(tokio::sync::Semaphore::new(1)),
        source_tick: (1, 24),
        native_bl: (64, 64),
        source_clock: plurx_core::transcode::VodSourceClock::ReportedOrigin(0),
        preferences,
        destination,
        mode: DvProcessingMode::Fel,
        encoder: Encoder::Software,
        vaapi_hdr_policy: None,
        runtime_cache: root.path().join("runtime"),
        shader_cache: Arc::new(crate::test_tempdir().expect("shader cache")),
        prepared: std::sync::Mutex::new(None),
        failure_episode: digest(b"lifecycle failure"),
        preparation_budget: Duration::from_secs(5),
        report_revision: AtomicU64::new(0),
        published_receipt: std::sync::Mutex::new(None),
    });
    let mut resources = TranscodeResourceEstimate::of(&plan, &Workload::of(file, 64));
    resources.hardware_slot = true;
    resources.cpu_threads = 15;
    let mut encoding = crate::vodencode::Encoding {
        source_object_version: source.object_version().to_owned(),
        source_clock: runtime.source_clock,
        shared_audio: None,
        dv_processing: DvSelection::Selected(Box::new(selected_plan)),
        dv_runtime: Some(runtime),
        plan,
        resources,
        options,
        grid: plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid"),
        reorder_frames: false,
        subtitle: None,
        subtitle_digest: None,
        ffmpeg_build: crate::ffmpeg::ffmpeg_build().await,
        executable: crate::ffmpeg::EncodedExecutable::capture()
            .await
            .expect("executable"),
        engine: crate::ffmpeg::EncodedEngine::capture(None)
            .await
            .expect("engine"),
        admissions: manager.admissions.clone(),
        store: store.clone(),
        speculative: AtomicBool::new(false),
        nonpreemptive_trial: false,
        candidate_recipe: None,
        production_proofs: Arc::new(crate::vodencode::CandidateProductionProofs::default()),
        active_production: std::sync::Mutex::new(
            crate::vodencode::ActiveProductionWindow::default(),
        ),
        queued: std::sync::Mutex::new(None),
        policy_retry: AtomicBool::new(false),
        handoff_wait: AtomicBool::new(false),
        last_refusal: std::sync::Mutex::new(None),
        handoff_claim: std::sync::Mutex::new(None),
        hooks: Box::new(crate::vodencode::NoopEncodingHooks),
    };
    let binding = crate::media_pool::PlanningBinding::from_snapshot(&snapshot);
    let recipe_digest = encoding
        .processed_candidate_digest(file, &binding)
        .expect("actual digest");
    encoding.candidate_recipe = Some(recipe_digest);
    let encoding = Arc::new(encoding);
    let permit = encoding
        .admissions
        .try_admit_bundle(1, 15, &resources, Priority::Live)
        .expect("mixed admission")
        .into();
    let worker = crate::vodencode::EncodePermit::clone(&permit)
        .try_claim_worker()
        .expect("exclusive worker");
    let candidate = QualityCandidate {
        id: CandidateId::for_recipe_digest(recipe_digest),
        recipe_digest,
        route: CandidateRoute::Encode,
        planned_codec: Some("hevc".into()),
        processed_hdr10: true,
        normalized_geometry: false,
        width: raster.0,
        height: raster.1,
        target_height: 64,
        average_bps: None,
        peak_bps: None,
        grade: OutputGrade::Hdr10,
        decoder_compatible: true,
        complete_cache: false,
        sustainable: false,
    };
    let mut context = TranscodeManager::candidate_context(&candidate);
    context.catalog_query = Some(query.clone());
    context.canonical_caps = Some(caps.clone());
    context.planning_binding = Some(binding.clone());
    context.planning_snapshot = Some(Arc::new(snapshot.clone()));
    context.owner_node_id = Some("node".into());
    let incarnation = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut request: SessionRequest = serde_json::from_value(serde_json::json!({
        "file_id":file_id,"playback_id":"processed-lifecycle","automatic":true,
        "kind":{"kind":"transcode","height":64},"start_seconds":0.0,"hdr10":true,"presentation":"vod"
    })).expect("request");
    request.request_id = Some(incarnation.clone());
    request.dv_processing_destination = Some(destination);
    request.audio_index = projected.audio_index;
    request.audio_delivery = projected.audio_delivery;
    request.audio_claim = projected.audio_claim;
    request.candidate_context = Some(Box::new(context));
    let mut remote: crate::media_sessions::RemoteStartRequest = serde_json::from_str(
        &crate::media_sessions::takeover_eligible_route(&session_id, &incarnation).recipe_json,
    )
    .expect("envelope");
    remote.principal =
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: user.id };
    remote.request = request;
    remote.candidate_id = Some(candidate.id);
    remote.candidate_catalog = Some(crate::media_sessions::CandidateCatalogContext {
        caps,
        candidate,
        binding,
    });
    remote.source_size = file.size;
    remote.source_mtime = file.mtime;
    remote.decoder_caps = None;
    remote.retained_output = None;
    remote.retained_output_receiver = Some(1);
    let settings = manager
        .vod_settings(&remote.request)
        .await
        .expect("settings")
        .expect("VOD");
    manager
        .vod
        .try_create(
            crate::vodserve::VodRecipeRequest {
                root_admission: Some(crate::vodserve::PreparedRootAdmission {
                    reuse_scope: manager.processed_hint_key(&snapshot, &query),
                    reused_parent: None,
                    instance_allowance: None,
                    instance_nonce: Some([1; 16]),
                    encoding: Arc::clone(&encoding),
                    permit,
                }),
                request: &remote.request,
                encoding: Some(Arc::clone(&encoding)),
                ..(&remote.request).into()
            },
            file,
            &settings,
            crate::vodserve::VodAttribution {
                user_name: "processed",
                item_title: "control",
                supersession_user: &principal_supersession_scope(&remote.principal),
            },
            session_id.clone(),
        )
        .await
        .expect("actual processed incumbent");
    let now = crate::media_sessions::unix_ms();
    let activation = plurx_core::domain::MediaSessionActivation {
        incarnation_id: incarnation,
        session_id: session_id.clone(),
        principal: remote.principal.clone(),
        playback_id: remote.request.playback_id.clone(),
        recovery_epoch: "processed-epoch".into(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: None,
        request_fingerprint: "a".repeat(64),
        owner_node_id: "node".into(),
        recipe_json: serde_json::to_string(&remote).expect("recipe"),
        response_json: "{}".into(),
        publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 60_000,
        expected_desired_revision: None,
    };
    manager
        .store
        .activate_media_session(&activation)
        .await
        .expect("activate")
        .expect("incumbent route");
    manager
        .store
        .settle_media_session_activation(
            &activation,
            plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now,
        )
        .await
        .expect("publish route")
        .expect("confirmed");
    Fixture {
        _root: root,
        manager,
        remote,
        encoding,
        worker: Some(worker),
        receipt,
        payloads,
        user_id: user.id,
        session_id,
    }
}

async fn stage(f: &Fixture) -> (String, String, String) {
    let incarnation = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    let mut remote = f.remote.clone();
    remote.incarnation_id = incarnation.clone();
    remote.request.request_id = Some(incarnation.clone());
    remote.request.previous_session_id = None;
    let json = serde_json::to_string(&remote).expect("serialize drops all private context");
    let now = crate::media_sessions::unix_ms();
    f.manager
        .store
        .prepare_media_session(&plurx_core::domain::MediaSessionPreparation {
            quality_cancellation_key: None,
            incarnation_id: incarnation.clone(),
            session_id: session.clone(),
            principal: remote.principal,
            playback_id: remote.request.playback_id,
            expected_predecessor_incarnation_id: f.remote.incarnation_id.clone(),
            expected_predecessor_owner_node_id: "node".into(),
            expected_predecessor_owner_epoch: 1,
            request_fingerprint: "b".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: json.clone(),
            response_json: r#"{"prepared_output_capture_pending":true}"#.into(),
            media_origin_ms: 0,
            now_ms: now,
            expected_desired_revision: None,
            deadline_ms: now + 30_000,
        })
        .await
        .expect("stage")
        .expect("reserved");
    (incarnation, session, json)
}

async fn commit_and_retire(f: &mut Fixture, incarnation: &str, session: &str) {
    let now = crate::media_sessions::unix_ms();
    let committed = f
        .manager
        .store
        .commit_media_session_preparation(
            &f.remote.principal,
            &f.remote.request.playback_id,
            &plurx_core::domain::MediaSessionPreparationCommitRequest {
                staged_incarnation_id: incarnation.to_owned(),
                expected_predecessor_owner_node_id: "node".into(),
                expected_predecessor_owner_epoch: 1,
                now_ms: now,
                lease_expires_at_ms: now + 60_000,
                control_receipt: None,
                expected_desired_revision: None,
            },
        )
        .await
        .expect("durable CAS")
        .expect("committed exact predecessor");
    assert_eq!(committed.route.session_id, session);
    let predecessor = committed.predecessor.expect("retired predecessor");
    assert_eq!(predecessor.session_id, f.session_id);
    assert!(
        f.manager
            .vod
            .end(&predecessor.session_id, crate::vodserve::Terminal::Deleted)
            .await
    );
    let published = f
        .manager
        .store
        .complete_media_session_handoff(
            incarnation,
            "node",
            1,
            plurx_core::domain::MediaSessionProjectionCompletion::PredecessorAcknowledged,
            now,
        )
        .await
        .expect("acknowledged retirement")
        .expect("current successor");
    assert_eq!(published.publication_ready_at_ms, 0);
    f.remote = serde_json::from_str(&committed.route.recipe_json).expect("sealed wire recipe");
    assert!(
        f.remote.retained_output.is_none(),
        "ephemeral root is not a disk artifact"
    );
    f.session_id = session.to_owned();
    assert_eq!(f.encoding.admissions.in_use(), 1);
    assert_eq!(f.encoding.admissions.software_in_use(), 15);
}

#[tokio::test]
async fn staged_processed_second_successor_survives_normal_commit_and_parent_retirement() {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let mut f = fixture(destination).await;
        for _ in 0..2 {
            let (incarnation, session, json) = stage(&f).await;
            assert!(prepare(&f, &session, &json).await);
            commit_and_retire(&mut f, &incarnation, &session).await;
            let facts = f
                .manager
                .vod
                .hls_facts(&session)
                .await
                .expect("committed HLS facts");
            let encoding = facts.encoding.expect("actual processed encoding");
            assert!(Arc::ptr_eq(&encoding, &f.encoding));
            assert_eq!(
                encoding.preserves_processed_dv(),
                destination == DvDestination::Profile81
            );
            assert_eq!(encoding.processed_fel(), Some(true));
            // A selected descriptor is not report authority. Only the checked
            // control window's publication to this actual owner awards HDR10-E.
            assert!(f
                .manager
                .vod
                .dv_effective_report(&session, &incarnation)
                .await
                .is_none());
            f.manager
                .vod
                .test_publish_processed_control(&session, f.receipt.clone(), &f.payloads)
                .await;
            let report = f
                .manager
                .vod
                .dv_effective_report(&session, &incarnation)
                .await;
            if destination == DvDestination::Hdr10 {
                let report = report.expect("actual-owner-bound control publication");
                assert!(report.hdr10_enhanced && report.fel_contributed);
                assert_eq!(report.generation, incarnation);
            } else {
                assert!(
                    report.is_none(),
                    "P81 reports its actual DV carrier, never HDR10-E"
                );
            }
            let (_, root) = f
                .manager
                .vod
                .capture_staged_processed_root(
                    &SessionRequest {
                        request_id: Some("future-incarnation".into()),
                        ..f.remote.request.clone()
                    },
                    &StagedProcessedPredecessor {
                        session_id: session.clone(),
                        incarnation_id: incarnation.clone(),
                        owner_node_id: "node".into(),
                        owner_epoch: 1,
                        successor_session_id: "future-session".into(),
                        successor_incarnation_id: "future-incarnation".into(),
                    },
                    &principal_supersession_scope(&f.remote.principal),
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .expect("new seed survives original-parent retirement");
            assert!(Arc::ptr_eq(&root.encoding, &f.encoding));
            assert!(root.permit.clone().try_claim_worker().is_none());
            assert!(
                f.encoding
                    .admissions
                    .try_admit_bundle(1, 15, &f.encoding.resources, Priority::Live)
                    .is_none(),
                "an unrelated fresh session cannot borrow incumbent credits"
            );
        }
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
        assert_eq!(f.encoding.admissions.software_in_use(), 15);
        drop(f.worker.take());
        assert_eq!(f.encoding.admissions.in_use(), 0);
        assert_eq!(f.encoding.admissions.software_in_use(), 0);
    }
}

#[tokio::test]
async fn staged_processed_roots_reconstruct_both_destinations_without_serialized_authority_or_speed(
) {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let mut f = fixture(destination).await;
        let (incarnation, session, json) = stage(&f).await;
        let mut serialized: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&json).expect("wire");
        assert!(serialized.request.candidate_context.is_none());
        assert!(
            f.manager
                .restore_candidate_context(&mut serialized)
                .await
                .is_err(),
            "ordinary restore still refuses identical JSON"
        );
        assert!(
            !f.manager
                .vod_prepare_first_before(
                    &json,
                    &session,
                    f.user_id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    f.manager
                        .session_adoption_token(&session)
                        .expect("adoption"),
                    None,
                    Instant::now() + Duration::from_secs(5)
                )
                .await,
            "no admitted epoch grants no root authority"
        );
        assert!(f.manager.vod.hls_facts(&session).await.is_none());
        assert!(
            f.manager
                .vod_prepare_first_before(
                    &json,
                    &session,
                    f.user_id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    f.manager
                        .session_adoption_token(&session)
                        .expect("adoption"),
                    Some(1),
                    Instant::now() + Duration::from_secs(5)
                )
                .await,
            "same root prepares despite sustainable=false"
        );
        let expected = StagedProcessedPredecessor {
            session_id: session.clone(),
            incarnation_id: incarnation.clone(),
            owner_node_id: "node".into(),
            owner_epoch: 1,
            successor_session_id: uuid::Uuid::new_v4().to_string(),
            successor_incarnation_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut next = f.remote.request.clone();
        next.request_id = Some(expected.successor_incarnation_id.clone());
        next.previous_session_id = None;
        let (_, root) = f
            .manager
            .vod
            .capture_staged_processed_root(
                &next,
                &expected,
                &principal_supersession_scope(&f.remote.principal),
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("successor retained its own provenance");
        assert!(Arc::ptr_eq(&root.encoding, &f.encoding));
        assert!(root.permit.clone().try_claim_worker().is_none());
        drop(root);
        assert_eq!(f.encoding.admissions.in_use(), 1);
        assert_eq!(f.encoding.admissions.software_in_use(), 15);
        let owner = f
            .manager
            .vod
            .hls_facts(&session)
            .await
            .expect("actual successor")
            .response_owner;
        assert!(
            f.manager
                .vod_prepare_first_before(
                    &json,
                    &session,
                    f.user_id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    f.manager.session_adoption_token(&session).expect("replay"),
                    Some(1),
                    Instant::now() + Duration::from_secs(5)
                )
                .await
        );
        assert!(
            f.manager.vod.response_owner_is_live(&session, &owner).await,
            "replay did not recapture/replace"
        );
        assert!(
            f.manager
                .vod
                .end(&f.session_id, crate::vodserve::Terminal::Deleted)
                .await
        );
        assert_eq!(f.encoding.admissions.software_in_use(), 15);
        assert!(
            f.manager
                .vod
                .end(&session, crate::vodserve::Terminal::Deleted)
                .await
        );
        assert_eq!(
            f.encoding.admissions.software_in_use(),
            15,
            "exclusive worker retains credit"
        );
        assert!(
            !prepare(&f, &session, &json).await,
            "completed capture without a local graph cannot mint authority again"
        );
        drop(f.worker.take());
        assert_eq!(f.encoding.admissions.in_use(), 0);
        assert_eq!(f.encoding.admissions.software_in_use(), 0);
    }
}
