use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Review 76, P3-1: a QSV or VA-API readiness failure names its cause, not
/// libva's start-up banner, which is printed first.
#[test]
fn a_failed_readiness_graph_reports_its_cause_not_the_libva_banner() {
    let stderr = "libva info: VA-API version 1.24.0\n\
                      libva info: Trying to open /usr/lib/jellyfin-ffmpeg/lib/dri/iHD_drv_video.so\n\
                      libva info: Found init function __vaDriverInit_1_24\n\
                      libva info: va_openDriver() returns 0\n\
                      [h264_qsv @ 0x1] Error initializing an internal MFX session: unsupported (-3)\n\
                      Error while opening encoder - maybe incorrect parameters\n\
                      Conversion failed!\n\
                      Exiting with status 1\n";
    assert_eq!(
        super::graph_failure_summary(stderr),
        "[h264_qsv @ 0x1] Error initializing an internal MFX session: unsupported (-3) \
             Error while opening encoder - maybe incorrect parameters Conversion failed!"
    );
    // A banner and nothing else is still something to report.
    assert_eq!(
        super::graph_failure_summary("libva info: VA-API version 1.24.0\n"),
        "libva info: VA-API version 1.24.0"
    );
}

/// Review 76, P2-1: class and budget travel together. A viewer keeps the
/// two-second budget and the realtime class; the self-test gets the
/// readiness budget and the background class.
#[test]
fn source_probe_work_pairs_each_class_with_its_budget() {
    assert_eq!(
        super::SourceProbeWork::VIEWER,
        super::SourceProbeWork {
            class: crate::process_control::ChildClass::Realtime,
            budget: Duration::from_secs(2),
        }
    );
    assert_eq!(
        super::SourceProbeWork::SELF_TEST,
        super::SourceProbeWork {
            class: crate::process_control::ChildClass::Background,
            budget: super::READINESS_PROBE_TIME,
        }
    );
}

/// The counterpart of the caption module's slow-probe test: the same
/// three-second stub is past a viewer's budget, so that test measures a
/// probe a viewer would have abandoned.
#[cfg(unix)]
#[tokio::test]
async fn a_viewer_source_probe_still_gives_up_after_two_seconds() {
    let root = crate::test_tempdir().expect("slow probe root");
    let answer = root.path().join("probe.json");
    std::fs::write(
            &answer,
            r#"{"streams":[{"codec_type":"video","codec_name":"mpeg2video","width":1920,"height":1080},{"codec_type":"audio","codec_name":"ac3","channels":2}]}"#,
        )
        .expect("probe answer");
    let script = root.path().join("slow-ffprobe");
    crate::write_test_executable(
        &script,
        format!("#!/bin/sh\nsleep 3\nexec /bin/cat '{}'\n", answer.display()),
        0o755,
    );
    let system = SystemInfo {
        ffprobe: script.to_string_lossy().into_owned(),
        ..SystemInfo::default()
    };
    let facts = super::probe_live_source(
        &system,
        root.path(),
        b"prefix",
        super::SourceProbeWork::VIEWER,
    )
    .await;
    assert!(
        matches!(facts, Err(LiveTvError::StartupTimeout(_))),
        "{facts:?}"
    );
}

use super::guide::LiveTvProgramme;
use super::*;

/// How long a test waits on an event before calling it a hang. It is not
/// a latency promise: a loaded runner can take seconds to schedule the
/// fixture processes these tests drive, and the tests assert on what
/// happens, not on how quickly.
const LIVE_TV_TEST_HANG_GUARD: Duration = Duration::from_secs(60);

async fn serve_once(response: Vec<u8>) -> (reqwest::Url, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake HDHomeRun");
    let address = listener.local_addr().expect("fake address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept fake request");
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).await.expect("read fake request");
        let _ = stream.write_all(&response).await;
    });
    (
        reqwest::Url::parse(&format!("http://{address}/document.json")).expect("fake URL"),
        server,
    )
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("test client")
}

fn test_manager(root: &Path) -> Arc<LiveTvManager> {
    test_manager_with_system(root, SystemInfo::default())
}

fn test_manager_with_system(root: &Path, system: SystemInfo) -> Arc<LiveTvManager> {
    test_manager_with(
        root,
        system,
        crate::serving_fence::ServingAuthority::always_ready(),
    )
}

fn test_manager_with(
    root: &Path,
    system: SystemInfo,
    serving: crate::serving_fence::ServingAuthority,
) -> Arc<LiveTvManager> {
    use plurx_core::store::SqliteStore;
    use plurx_core::transcode::{EncoderCaps, Pipeline};

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let transcode = Arc::new(crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        root.join("finite"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    LiveTvManager::new(
        store,
        Arc::new(system),
        transcode,
        serving,
        "node-a".into(),
        root.join("live-tv"),
        root.join("live-tv-guide"),
    )
}

/// Live TV's authority with its real loop over a fence the test drives.
fn live_tv_authority_under_test() -> (
    crate::serving_fence::ServingFence,
    LiveTvAuthority,
    tokio::task::JoinHandle<()>,
) {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let fence =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let authority = LiveTvAuthority::new(fence.authority());
    let task = tokio::spawn(authority.clone().serving_fence_loop(fence.subscribe()));
    (fence, authority, task)
}

async fn wait_until_not_running(authority: &LiveTvAuthority, generation: u64, why: &str) {
    tokio::time::timeout(
        crate::serving_fence::SERVING_FENCE_SESSION_GRACE + Duration::from_secs(10),
        async {
            while authority.running(generation) {
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap_or_else(|_| panic!("{why}"));
}

/// The 2026-10-04 shape on the Live TV owner: a leader restart costs it
/// its serving authority for a second or two. Running sessions,
/// transports and recordings must outlive a loss that recovers inside
/// the grace, although the generation they were admitted under is gone.
/// Shutting the fence down still ends them.
#[tokio::test]
async fn live_tv_running_work_survives_a_brief_serving_loss() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (fence, authority, task) = live_tv_authority_under_test();
    let admitted = authority.admit().expect("authority at start");
    fence.validation_set_ready(false).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(grace / 4).await;
    fence.validation_set_ready(true).await;
    tokio::time::sleep(grace + grace / 2).await;
    assert!(
        authority.running(admitted),
        "a loss inside the grace must not end running Live TV"
    );
    assert!(
        !fence.authority().is_current(admitted),
        "admissions still see the generation move"
    );
    drop(fence);
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the loop exits with its fence")
        .expect("Live TV fence loop");
    assert!(!authority.running(admitted), "shutdown ends running work");
}

/// The fence's safety property: a loss that outlasts the grace ends
/// running Live TV, and not before the grace; work admitted after
/// authority returns runs.
#[tokio::test]
async fn live_tv_running_work_ends_after_a_sustained_serving_loss() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (fence, authority, _task) = live_tv_authority_under_test();
    let admitted = authority.admit().expect("authority at start");
    let lost_at = std::time::Instant::now();
    fence.validation_set_ready(false).await;
    wait_until_not_running(
        &authority,
        admitted,
        "a sustained loss must end running Live TV",
    )
    .await;
    assert!(
        lost_at.elapsed() >= grace,
        "ended after {:?}, before the {grace:?} grace",
        lost_at.elapsed()
    );
    fence.validation_set_ready(true).await;
    let readmitted = authority.admit().expect("authority is back");
    assert!(
        authority.running(readmitted),
        "new work runs once authority returns"
    );
}

/// Recovery, then further losses before the quorum has been stable for a
/// grace: one budget, so a flapping quorum still ends running work.
#[tokio::test]
async fn live_tv_losses_after_a_recovery_share_one_grace() {
    let slice = crate::serving_fence::SERVING_FENCE_SESSION_GRACE / 3;
    let (fence, authority, _task) = live_tv_authority_under_test();
    let admitted = authority.admit().expect("authority at start");
    for _ in 0..4 {
        fence.validation_set_ready(false).await;
        tokio::time::sleep(slice).await;
        fence.validation_set_ready(true).await;
        tokio::task::yield_now().await;
    }
    wait_until_not_running(
        &authority,
        admitted,
        "losses summing past the grace must end running Live TV",
    )
    .await;
}

/// A loss and recovery published before the loop runs leave only a new
/// generation with authority restored. Running work continues; a
/// sustained loss after it still ends it, and ends work admitted under the
/// new generation too.
#[tokio::test]
async fn live_tv_running_work_survives_a_generation_bump_with_authority_back() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (fence, authority, _task) = live_tv_authority_under_test();
    let admitted = authority.admit().expect("authority at start");
    // Let the loop take generation zero as resolved, so the coalesced
    // loss and recovery below reach it as a bare generation bump.
    tokio::task::yield_now().await;
    fence.validation_set_ready(false).await;
    fence.validation_set_ready(true).await;
    tokio::time::sleep(grace + grace / 2).await;
    assert!(
        authority.running(admitted),
        "a generation bump alone ends nothing"
    );
    let later = authority.admit().expect("authority is back");
    assert!(later > admitted && authority.running(later));
    fence.validation_set_ready(false).await;
    wait_until_not_running(&authority, later, "a sustained loss ends later work").await;
    assert!(!authority.running(admitted), "and the earlier work with it");
}

/// Shutdown while authority is lost does not wait out the grace.
#[tokio::test]
async fn live_tv_running_work_ends_when_the_fence_closes_during_a_loss() {
    let grace = crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    let (fence, authority, task) = live_tv_authority_under_test();
    let admitted = authority.admit().expect("authority at start");
    fence.validation_set_ready(false).await;
    tokio::task::yield_now().await;
    drop(fence);
    tokio::time::timeout(grace / 2, task)
        .await
        .expect("a closed fence ends the loop without waiting for the grace")
        .expect("Live TV fence loop");
    assert!(!authority.running(admitted));
}

/// The running session's own fence, through a loss inside the grace and
/// one that outlasts it: kept and answered retryably first, ended after.
#[tokio::test]
async fn a_loss_inside_the_grace_keeps_the_session_and_refuses_requests_retryably() {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let root = crate::test_tempdir().expect("scratch root");
    let fence =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let manager = test_manager_with(root.path(), SystemInfo::default(), fence.authority());
    seed_test_config(&manager).await;
    let session = test_session(root.path().join("live-tv-serving-grace"), 1);
    manager
        .resource_start(&session.request, &session.device_id, 4)
        .await
        .expect("fixture admission");
    manager.observe_fence().await;
    ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect("a healthy session passes its fence");

    fence.validation_set_ready(false).await;
    ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect("a loss inside the grace must not end a running session");
    assert!(
        matches!(
            manager.resource_session_fence(&session).await,
            Err(LiveTvError::OwnerUnavailable(_))
        ),
        "a request during the loss is refused retryably, not as an expired capability"
    );

    fence.validation_set_ready(true).await;
    ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect("authority back: the session continues past the generation bump");
    manager
        .resource_session_fence(&session)
        .await
        .expect("requests are served again");

    fence.validation_set_ready(false).await;
    assert!(
        manager
            .authority()
            .end_running_before(fence.authority().state().loss_generation),
        "the grace ran out: the authority's loop raises the running floor"
    );
    assert!(matches!(
        ensure_session_fence_from_observation(&manager, &session).await,
        Err(LiveTvError::OwnerUnavailable(_))
    ));
    assert!(matches!(
        manager.resource_session_fence(&session).await,
        Err(LiveTvError::CapabilityExpired(_))
    ));
}

fn test_encode_delivery(height: u16) -> LiveDeliveryPlan {
    LiveDeliveryPlan {
        source: LiveSourceFacts {
            video_codec: Some("mpeg2video".into()),
            width: Some(1920),
            height: Some(1080),
            audio_codec: Some("ac3".into()),
            audio_channels: Some(2),
            ..LiveSourceFacts::default()
        },
        output: crate::live_tv_delivery::LiveDeliveryOutput {
            container: "mpegts".into(),
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            width: height.saturating_mul(16) / 9,
            height,
            bit_depth: Some(8),
            frame_rate: None,
            hdr: None,
            audio_channels: 2,
        },
        video_action: LiveTrackAction::Encode,
        audio_action: LiveTrackAction::Encode,
        audio_track: 0,
        packaging: LivePackaging::Mpegts,
        reasons: vec![crate::live_tv_delivery::LiveDeliveryReason {
            code: "test_fixture".into(),
            explanation: "exercise the encoded live path".into(),
        }],
        deinterlace: false,
        deinterlace_output: None,
        max_bitrate_bps: None,
    }
}

fn test_session(directory: PathBuf, generation: i64) -> Arc<LiveTvSession> {
    let now = tokio::time::Instant::now();
    Arc::new(LiveTvSession {
        capability: format!(
            "ltv1.{}.{}",
            base64url_encode(b"node-a"),
            uuid::Uuid::new_v4()
        ),
        activation_token: uuid::Uuid::new_v4().to_string(),
        request: LiveTvStartRequest {
            expected_owner_node_id: "node-a".into(),
            source_node_id: "node-a".into(),
            user_id: 1,
            user_name: "viewer".into(),
            // `.simple()`, because that is the only spelling on the wire:
            // 32 lower-case hex, whether a client minted it or the ingress
            // did. A fixture carrying a hyphenated UUID is a fixture that
            // cannot reproduce a real start, and that is how the owner's
            // version-4 rule survived being wrong.
            request_id: uuid::Uuid::new_v4().simple().to_string(),
            channel_id: "7.1".into(),
            config_generation: generation,
            source_serving_generation: 0,
            playback: None,
        },
        channel: LiveTvChannel {
            id: "7.1".into(),
            guide_number: "7.1".into(),
            guide_name: "Test".into(),
            favorite: false,
            drm: false,
            support: LiveTvChannelSupport::Ready,
            hd: None,
            video_codec: None,
            audio_codec: None,
            source_format: None,
        },
        device_id: "fixture-device".into(),
        output: StdMutex::new(LiveTvOutput {
            container: "hls".into(),
            video: "h264".into(),
            audio: "aac".into(),
            height: 720,
        }),
        delivery: StdMutex::new(None),
        device_ipv4: None,
        owner_serving_generation: 0,
        started: now,
        directory,
        cancel: CancellationToken::new(),
        changed: tokio::sync::Notify::new(),
        startup_waiters: AtomicUsize::new(0),
        worker: StdMutex::new(None),
        resource_admission: Arc::new(tokio::sync::Semaphore::new(LOCAL_RESOURCE_CONCURRENCY)),
        process: tokio::sync::Mutex::new(None),
        decoder_unavailable: Arc::new(AtomicBool::new(false)),
        source_format_changed: Arc::new(AtomicBool::new(false)),
        encoder_diagnostic: Arc::new(StdMutex::new(None)),
        source_format: Arc::new(StdMutex::new(None)),
        source_format_expires_at: Arc::new(AtomicI64::new(0)),
        tuner_bytes: Arc::new(AtomicU64::new(0)),
        codec_qualification_counted: AtomicBool::new(false),
        state: StdMutex::new(LiveTvSessionState {
            phase: LiveTvSessionPhase::Active,
            startup: None,
            activated: true,
            provisional_at: Some(now),
            last_touch: now,
            last_progress: now,
            newest_listed: None,
            media_sequence: 0,
            publication: None,
            encoder: "software".into(),
            error: None,
            terminal_error: None,
            cleanup: None,
        }),
        signal_cache: tokio::sync::Mutex::new(None),
        transport: StdMutex::new(None),
        seat_reserved: AtomicBool::new(false),
        consumer: StdMutex::new(None),
    })
}

/// Put a session in the registry under a public identity, and give it the
/// fake worker `cancel_and_wait` waits for: on cancellation it retires the
/// session the way `run_live_session` would.
async fn register_test_session(
    manager: &Arc<LiveTvManager>,
    user_id: i64,
    request_id: &str,
    phase: LiveTvSessionPhase,
    last_touch: tokio::time::Instant,
) -> Arc<LiveTvSession> {
    register_test_session_at(manager, user_id, request_id, phase, last_touch, 0).await
}

/// Inject legacy duplicates explicitly: current request keys stay stable
/// across ingress generations, while recovery must still close old duplicates.
async fn register_test_session_at(
    manager: &Arc<LiveTvManager>,
    user_id: i64,
    request_id: &str,
    phase: LiveTvSessionPhase,
    last_touch: tokio::time::Instant,
    serving_generation: u64,
) -> Arc<LiveTvSession> {
    let session = test_session(
        manager.scratch_root.join(uuid::Uuid::new_v4().to_string()),
        1,
    );
    // `test_session` builds an owned request; rewrite the identity fields
    // through a fresh Arc so the registry key is the one under test.
    let mut request = session.request.clone();
    request.user_id = user_id;
    request.request_id = request_id.to_owned();
    request.source_serving_generation = serving_generation;
    let session = Arc::new(LiveTvSession {
        request,
        state: StdMutex::new(LiveTvSessionState {
            phase,
            startup: None,
            activated: phase == LiveTvSessionPhase::Active,
            provisional_at: Some(last_touch),
            last_touch,
            last_progress: last_touch,
            newest_listed: None,
            media_sequence: 0,
            publication: None,
            encoder: "software".into(),
            error: None,
            terminal_error: None,
            cleanup: None,
        }),
        capability: session.capability.clone(),
        activation_token: session.activation_token.clone(),
        channel: session.channel.clone(),
        device_id: session.device_id.clone(),
        output: StdMutex::new(session.output()),
        delivery: StdMutex::new(None),
        device_ipv4: None,
        owner_serving_generation: 0,
        started: last_touch,
        directory: session.directory.clone(),
        cancel: CancellationToken::new(),
        changed: tokio::sync::Notify::new(),
        startup_waiters: AtomicUsize::new(0),
        worker: StdMutex::new(None),
        process: tokio::sync::Mutex::new(None),
        decoder_unavailable: Arc::new(AtomicBool::new(false)),
        source_format_changed: Arc::new(AtomicBool::new(false)),
        encoder_diagnostic: Arc::new(StdMutex::new(None)),
        source_format: Arc::new(StdMutex::new(None)),
        source_format_expires_at: Arc::new(AtomicI64::new(0)),
        tuner_bytes: Arc::new(AtomicU64::new(0)),
        codec_qualification_counted: AtomicBool::new(false),
        resource_admission: Arc::new(tokio::sync::Semaphore::new(LOCAL_RESOURCE_CONCURRENCY)),
        signal_cache: tokio::sync::Mutex::new(None),
        transport: StdMutex::new(None),
        seat_reserved: AtomicBool::new(false),
        consumer: StdMutex::new(None),
    });
    {
        let mut registry = manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = LiveTvRequestKey::from(&session.request);
        if let Some(previous) = registry.requests.remove(&key) {
            registry.requests.insert(
                LiveTvRequestKey {
                    source_node_id: "legacy-ingress".into(),
                    source_serving_generation: serving_generation,
                    ..key.clone()
                },
                previous,
            );
        }
        registry.requests.insert(key, session.capability.clone());
        registry
            .sessions
            .insert(session.capability.clone(), Arc::clone(&session));
    }
    manager
        .store
        .put_settings(&[("live_tv.enabled", "1"), ("live_tv.config_generation", "1")])
        .await
        .expect("fixture generation");
    manager
        .resource_start(&session.request, &session.device_id, 4)
        .await
        .expect("fixture admission");
    let worker_manager = Arc::clone(manager);
    let worker_session = Arc::clone(&session);
    tokio::spawn(async move {
        worker_session.cancel.cancelled().await;
        worker_session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cleanup = Some(Ok(()));
        worker_manager.retire_session(&worker_session);
        worker_session.changed.notify_waiters();
    });
    session
}

#[tokio::test]
async fn refused_replay_does_not_terminalize_the_original_claim() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let session = register_test_session(
        &manager,
        7,
        &hex_request_id(40),
        LiveTvSessionPhase::Active,
        tokio::time::Instant::now(),
    )
    .await;
    let mut changed = session.request.clone();
    changed.channel_id = "different-channel".into();
    assert!(manager.start_local(changed).await.is_err());
    manager
        .resource_session_fence(&session)
        .await
        .expect("original remains authorized");
    assert!(!session.cancel.is_cancelled());
}

fn hex_request_id(seed: u8) -> String {
    format!("{seed:02x}").repeat(16)
}

#[tokio::test]
async fn a_retired_request_id_can_never_start_again() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let id = hex_request_id(1);
    let mut request = test_session(root.path().join("x"), 1).request.clone();
    request.user_id = 7;
    request.request_id = id.clone();

    {
        let mut registry = manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            registry.request_session(&request).expect("fresh").is_none(),
            "an id nobody has seen is simply new"
        );
        registry.retire(7, id.clone(), tokio::time::Instant::now());
        assert!(matches!(
            registry.request_session(&request),
            Err(LiveTvError::Conflict(_))
        ));
        // A different viewer's identical id is a different fact.
        let mut other = request.clone();
        other.user_id = 8;
        assert!(registry
            .request_session(&other)
            .expect("other user")
            .is_none());
    }
}

#[tokio::test]
async fn one_viewer_cannot_fill_the_shared_recovery_history_by_retiring() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let now = tokio::time::Instant::now();
    {
        let mut registry = manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for seed in 0..40u8 {
            registry.retire(
                7,
                hex_request_id(seed),
                now + Duration::from_millis(seed.into()),
            );
        }
        assert_eq!(
            registry
                .retired
                .keys()
                .filter(|(user, _)| *user == 7)
                .count(),
            MAX_RETIRED_PER_USER,
            "a viewer holds at most MAX_RETIRED_PER_USER ids, oldest evicted"
        );
        assert!(
            registry.is_retired(7, &hex_request_id(39)),
            "the newest survives"
        );
        assert!(
            registry.is_retired(7, &hex_request_id(38)),
            "and so does the one before it, even though both were retired \
                 inside one clock tick — insertion order is the tie-break"
        );
        assert!(
            !registry.is_retired(7, &hex_request_id(0)),
            "the oldest does not"
        );
        assert!(
            registry.terminals.is_empty(),
            "retires are not tombstones: they must never consume the shared cap"
        );
    }
}

#[tokio::test]
async fn retire_stops_every_session_one_public_identity_produced() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    tokio::fs::create_dir_all(&manager.scratch_root)
        .await
        .expect("root");
    let id = hex_request_id(2);
    let now = tokio::time::Instant::now();
    // Two legacy sessions for one identity must still be retired together,
    // although current admission no longer duplicates on ingress change.
    let first =
        register_test_session_at(&manager, 7, &id, LiveTvSessionPhase::Active, now, 0).await;
    let second =
        register_test_session_at(&manager, 7, &id, LiveTvSessionPhase::Active, now, 1).await;
    assert_eq!(
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions_for_public_request(7, &id)
            .len(),
        2,
        "one public identity, two registry keys — the race the plan names"
    );

    assert_eq!(
        manager.retire_local(7, &id).await,
        LiveTvRetireOutcome::Stopped
    );
    assert!(first.cancel.is_cancelled());
    assert!(second.cancel.is_cancelled());

    // And a start racing the retire finds the fence, never a fresh slot.
    let mut racing = first.request.clone();
    racing.source_serving_generation = 9;
    assert!(matches!(
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request_session(&racing),
        Err(LiveTvError::Conflict(_))
    ));
}

#[tokio::test]
async fn retire_reports_what_it_actually_did() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    assert_eq!(
        manager.retire_local(7, &hex_request_id(3)).await,
        LiveTvRetireOutcome::Retired,
        "nothing to stop, and now nothing ever will be"
    );
}

#[tokio::test]
async fn resume_hands_back_one_stream_and_cancels_the_rest() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    tokio::fs::create_dir_all(&manager.scratch_root)
        .await
        .expect("root");
    let id = hex_request_id(4);
    let now = tokio::time::Instant::now();

    let stale = register_test_session_at(
        &manager,
        7,
        &id,
        LiveTvSessionPhase::Active,
        now - Duration::from_secs(30),
        0,
    )
    .await;
    let freshest =
        register_test_session_at(&manager, 7, &id, LiveTvSessionPhase::Active, now, 1).await;

    let answer = manager.resume_local(7, &id).await;
    assert_eq!(answer.outcome, LiveTvResumeOutcome::Live);
    assert_eq!(
        answer.session.as_ref().expect("session").session_id,
        freshest.capability,
        "the one the viewer most recently touched"
    );
    assert!(
        stale.cancel.is_cancelled(),
        "one public identity, one stream"
    );
    assert!(!freshest.cancel.is_cancelled());
}

#[tokio::test]
async fn resume_answers_pending_ended_and_retired_apart() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    tokio::fs::create_dir_all(&manager.scratch_root)
        .await
        .expect("root");
    let now = tokio::time::Instant::now();

    let pending_id = hex_request_id(5);
    let provisional = register_test_session(
        &manager,
        7,
        &pending_id,
        LiveTvSessionPhase::Provisional,
        now,
    )
    .await;
    let answer = manager.resume_local(7, &pending_id).await;
    assert_eq!(
        answer.outcome,
        LiveTvResumeOutcome::Pending,
        "an activation in flight from an ingress is that ingress's to finish"
    );
    assert!(answer.session.is_none());
    assert!(
        !provisional.cancel.is_cancelled(),
        "resume never cancels the session it is waiting on"
    );

    let ended_id = hex_request_id(6);
    let ended =
        register_test_session(&manager, 7, &ended_id, LiveTvSessionPhase::Active, now).await;
    ended.cancel.cancel();
    manager.retire_session(&ended);
    assert_eq!(
        manager.resume_local(7, &ended_id).await.outcome,
        LiveTvResumeOutcome::Ended
    );

    let unknown_id = hex_request_id(7);
    assert_eq!(
        manager.resume_local(7, &unknown_id).await.outcome,
        LiveTvResumeOutcome::Retired
    );
    assert!(
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_retired(7, &unknown_id),
        "an id the owner has never heard of is retired on the spot, so a \
             racing start cannot revive it"
    );
}

#[tokio::test]
async fn a_full_owner_evicts_this_viewers_stray_and_nobody_elses() {
    // The five capacity fixtures the plan names, each decided by the very
    // function the start path calls — not by a copy of its predicate.
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let now = tokio::time::Instant::now();
    let idle = now - STRAY_EVICTION_IDLE - Duration::from_secs(1);

    let mine_idle = register_test_session_at(
        &manager,
        7,
        &hex_request_id(20),
        LiveTvSessionPhase::Active,
        idle,
        0,
    )
    .await;
    let mine_fresh = register_test_session_at(
        &manager,
        7,
        &hex_request_id(21),
        LiveTvSessionPhase::Active,
        now,
        1,
    )
    .await;
    let mine_starting = register_test_session_at(
        &manager,
        7,
        &hex_request_id(22),
        LiveTvSessionPhase::Starting,
        idle,
        2,
    )
    .await;
    let mine_provisional = register_test_session_at(
        &manager,
        7,
        &hex_request_id(23),
        LiveTvSessionPhase::Provisional,
        idle,
        3,
    )
    .await;
    let theirs_idle = register_test_session_at(
        &manager,
        8,
        &hex_request_id(24),
        LiveTvSessionPhase::Active,
        idle,
        4,
    )
    .await;
    // Slots are transports now: each of these holds a tuner of its own,
    // so evicting any one of them would free one.
    for session in [
        &mine_idle,
        &mine_fresh,
        &mine_starting,
        &mine_provisional,
        &theirs_idle,
    ] {
        give_own_transport(&manager, session);
    }

    let chosen = manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .stray_to_evict(7, now)
        .expect("this viewer has a stray");
    assert_eq!(
        chosen.capability, mine_idle.capability,
        "the one that has missed three heartbeats, and only that one"
    );
    for other in [&mine_fresh, &mine_starting, &mine_provisional, &theirs_idle] {
        assert_ne!(chosen.capability, other.capability);
    }

    // Another viewer has nothing to evict here, however idle this
    // viewer's sessions are: their start is refused, which is correct.
    assert!(
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stray_to_evict(9, now)
            .is_none(),
        "a viewer's own stray must not refuse them; it is not a licence to \
             take someone else's tuner"
    );

    // With the stray gone, nothing is left to take: a viewer being
    // actively watched, a start in flight and a session awaiting
    // activation are all refused rather than cancelled.
    mine_idle.cancel.cancel();
    assert!(
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stray_to_evict(7, now)
            .is_none(),
        "a fresh active session, a start and a provisional are never \
             evicted for idleness \u{2014} their own timeouts bound them"
    );

    assert!(
        STRAY_EVICTION_IDLE < CAPABILITY_IDLE_TIMEOUT,
        "a viewer must be able to reclaim their own slot before the owner \
             reaps it, or the eviction never fires"
    );
}

/// Give a registered session a tuner connection only it feeds, as its
/// admission would have: a transport registered under its own key, this
/// viewer attached.
fn give_own_transport(
    manager: &Arc<LiveTvManager>,
    session: &Arc<LiveTvSession>,
) -> Arc<dvr::DvrTransport> {
    let transport = test_viewer_transport(manager, session);
    manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .transports
        .insert(session.capability.clone(), Arc::clone(&transport));
    transport
}

/// A viewer transport for `session`'s channel with that session attached.
fn test_viewer_transport(
    manager: &Arc<LiveTvManager>,
    session: &Arc<LiveTvSession>,
) -> Arc<dvr::DvrTransport> {
    let transport = dvr::DvrTransport::new(dvr::TransportInit {
        channel: session.channel.clone(),
        generation: session.request.config_generation,
        owner_serving_generation: 0,
        device_id: session.device_id.clone(),
        address: Ipv4Addr::new(10, 42, 1, 20),
        origin: dvr::TransportOrigin::Viewer,
        scratch: manager.transport_scratch(),
        metrics: Arc::clone(&manager.metrics),
        seats: 1,
    });
    join_test_transport(session, &transport);
    transport
}

fn join_test_transport(session: &Arc<LiveTvSession>, transport: &Arc<dvr::DvrTransport>) {
    if !session.seat_reserved.load(Ordering::Acquire) {
        session.hold_seat_on(Arc::clone(transport));
    }
    let (consumer, _feed) =
        dvr::ViewerConsumer::new(session.capability.clone(), session.cancel.clone());
    session
        .attach_consumer(consumer)
        .expect("a live transport takes the viewer");
}

fn test_seat(channel_id: &str) -> TransportSeat<'_> {
    TransportSeat {
        channel_id,
        device_id: "fixture-device",
        address: Ipv4Addr::new(10, 42, 1, 20),
        running_floor: 0,
    }
}

/// Plan L-03 §5.1 `stray_eviction_only_when_a_new_transport_is_needed`,
/// with §2.4 D8: a start on the channel the stray is watching joins it and
/// leaves the stray alone; a start that needs a new tuner at capacity
/// evicts the stray only when that frees a tuner.
#[tokio::test]
async fn stray_eviction_only_when_a_new_transport_is_needed() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let now = tokio::time::Instant::now();
    let idle = now - STRAY_EVICTION_IDLE - Duration::from_secs(1);
    let stray = register_test_session_at(
        &manager,
        7,
        &hex_request_id(40),
        LiveTvSessionPhase::Active,
        idle,
        0,
    )
    .await;
    let transport = test_viewer_transport(&manager, &stray);
    manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .transports
        .insert(stray.channel.id.clone(), Arc::clone(&transport));

    let decide = |channel: &str| {
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .viewer_admission(&test_seat(channel), 7, now, 1)
    };
    match decide(&stray.channel.id) {
        ViewerAdmission::Join(joined) => assert!(Arc::ptr_eq(&joined, &transport)),
        _ => panic!("a start on the stray's own channel joins its transport"),
    }
    assert!(!stray.cancel.is_cancelled(), "and leaves the stray alone");
    match decide("4.1") {
        ViewerAdmission::EvictStray(evicted, freed) => {
            assert_eq!(evicted.capability, stray.capability);
            assert!(Arc::ptr_eq(&freed, &transport));
        }
        _ => panic!("a new channel at capacity evicts the viewer's own stray"),
    }

    // Someone else joins the stray's transport: cancelling the stray
    // would now free nothing, so the new channel is refused instead.
    let sibling = register_test_session_at(
        &manager,
        8,
        &hex_request_id(41),
        LiveTvSessionPhase::Active,
        now,
        1,
    )
    .await;
    sibling.hold_seat_on(Arc::clone(&transport));
    assert!(transport.reserve_seat());
    join_test_transport(&sibling, &transport);
    assert!(matches!(
        decide("4.1"),
        ViewerAdmission::Refuse(LiveTvError::Capacity(_))
    ));
}

#[tokio::test]
async fn a_retired_id_evicts_a_session_in_any_phase() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let id = hex_request_id(25);
    let now = tokio::time::Instant::now();
    // Freshly touched and only provisional: neither half of the idleness
    // rule can reach it.
    let provisional =
        register_test_session_at(&manager, 7, &id, LiveTvSessionPhase::Provisional, now, 0).await;
    give_own_transport(&manager, &provisional);

    assert!(
        manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stray_to_evict(7, now)
            .is_none(),
        "nothing to evict before the viewer says the id is spent"
    );

    manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retire(7, id.clone(), now);

    let chosen = manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .stray_to_evict(7, now)
        .expect("a retired id is evictable in any phase");
    assert_eq!(chosen.capability, provisional.capability);
}

#[tokio::test]
async fn a_retired_id_stops_being_retired_when_its_minute_is_up() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let id = hex_request_id(26);
    let now = tokio::time::Instant::now();
    manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retire(7, id.clone(), now);
    assert!(manager
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_retired(7, &id));

    tokio::time::pause();
    tokio::time::advance(RETIRED_TTL + Duration::from_secs(1)).await;
    assert!(
        !manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_retired(7, &id),
        "the expiry is checked on read, not left to whenever a prune \
             happens to run \u{2014} an idle owner would otherwise answer \
             \"retired\" for hours"
    );
}

#[tokio::test]
async fn a_started_session_reports_its_state_without_handing_back_a_capability() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let id = hex_request_id(9);
    let now = tokio::time::Instant::now();
    assert_eq!(manager.start_state_local(7, &id).state, "unknown");
    let session = register_test_session(&manager, 7, &id, LiveTvSessionPhase::Active, now).await;
    assert_eq!(manager.start_state_local(7, &id).state, "active");
    session.cancel.cancel();
    manager.retire_session(&session);
    let ended = manager.start_state_local(7, &id);
    assert_eq!(ended.state, "ended");
    assert_eq!(ended.code.as_deref(), Some("capability_expired"));
    let json = serde_json::to_string(&ended).expect("encode");
    assert!(
        !json.contains(&session.capability),
        "a status read is never a handle: {json}"
    );
}

#[tokio::test]
async fn a_clean_release_is_counted_apart_from_a_failure() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let now = tokio::time::Instant::now();
    let released = register_test_session(
        &manager,
        7,
        &hex_request_id(10),
        LiveTvSessionPhase::Active,
        now,
    )
    .await;
    released.cancel.cancel();
    manager.retire_session(&released);

    let failed = register_test_session(
        &manager,
        7,
        &hex_request_id(11),
        LiveTvSessionPhase::Active,
        now,
    )
    .await;
    failed
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .terminal_error = Some(LiveTvError::StreamFailed("the tuner stopped".into()));
    failed.cancel.cancel();
    manager.retire_session(&failed);

    let prometheus = manager.metrics.session_ends_prometheus();
    assert!(
        prometheus.contains("plurx_live_tv_session_ends_total{reason=\"released\"} 1"),
        "a stop, a drain and an idle reap are releases, not failures: {prometheus}"
    );
    assert!(
        prometheus.contains("plurx_live_tv_session_ends_total{reason=\"stream_failed\"} 1"),
        "and a real failure is visible on its own line: {prometheus}"
    );
}

async fn seed_test_config(manager: &LiveTvManager) {
    manager
        .store
        .put_settings(&[
            (keys::LIVE_TV_ENABLED, "1"),
            (keys::LIVE_TV_DEVICE_IPV4, "10.42.1.20"),
            (keys::LIVE_TV_OWNER_NODE_ID, "node-a"),
            (keys::LIVE_TV_MAX_SESSIONS, "2"),
            (keys::LIVE_TV_OUTPUT_HEIGHT, "720"),
            (keys::LIVE_TV_CONFIG_GENERATION, "1"),
        ])
        .await
        .expect("seed live-TV settings");
}

fn settings_reads(manager: &LiveTvManager, site: SettingsReadSite) -> (u64, u64) {
    (
        manager.metrics.settings_reads[site as usize][0].load(Ordering::Acquire),
        manager.metrics.settings_reads[site as usize][1].load(Ordering::Acquire),
    )
}

fn all_settings_reads(manager: &LiveTvManager) -> (u64, u64) {
    SettingsReadSite::ALL
        .into_iter()
        .map(|site| settings_reads(manager, site))
        .fold((0, 0), |(ok, failed), (o, f)| (ok + o, failed + f))
}

fn fence_observations(manager: &LiveTvManager) -> (u64, u64, u64) {
    (
        manager.metrics.fence_observations[0].load(Ordering::Acquire),
        manager.metrics.fence_observations[1].load(Ordering::Acquire),
        manager.metrics.fence_observations[2].load(Ordering::Acquire),
    )
}

#[tokio::test]
async fn the_per_second_fence_costs_one_settings_read_per_node_not_one_per_session() {
    // Two sessions each took the fence four times a second and each of
    // those took a leader-consistent read of the replicated settings. The
    // reading is now taken once per node per second and both sessions
    // validate against it, so the read rate stops scaling with viewers.
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let first = test_session(root.path().join("live-tv-a"), 1);
    let second = test_session(root.path().join("live-tv-b"), 1);
    manager
        .resource_start(&first.request, &first.device_id, 4)
        .await
        .expect("fixture admission");
    manager
        .resource_start(&second.request, &second.device_id, 4)
        .await
        .expect("fixture admission");

    manager.observe_fence().await;
    let (ok_before, failed_before) = all_settings_reads(&manager);
    assert_eq!((ok_before, failed_before), (1, 0));

    tokio::time::pause();
    for _ in 0..40 {
        ensure_session_fence_from_observation(&manager, &first)
            .await
            .expect("the first session validates against the observation");
        ensure_session_fence_from_observation(&manager, &second)
            .await
            .expect("and so does the second");
    }
    assert_eq!(
        all_settings_reads(&manager),
        (ok_before, failed_before),
        "ten seconds of two sessions' fence ticks must read the store not once"
    );
    assert_eq!(fence_observations(&manager).0, 80);

    // And the observer's own read is still one read, so the node's rate is
    // the loop's rate rather than zero.
    manager.observe_fence().await;
    assert_eq!(
        settings_reads(&manager, SettingsReadSite::Observer),
        (ok_before + 1, failed_before)
    );
}

#[tokio::test]
async fn a_read_that_keeps_failing_is_graced_then_ends_the_session_naming_the_settings_store() {
    // A leader failover costs ~6.1 s of failed consistent reads at worst.
    // Ending healthy streams for that is what L2 is about; continuing
    // indefinitely would let a tuner that has been disabled or moved on
    // another node keep streaming. The grace is the bound between the two.
    //
    // Every observer tick after t=0 really does take a read, and every one
    // of those reads really fails: the failure arm of `observe_fence` is
    // what is under test, not the absence of a read.
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let session = test_session(root.path().join("live-tv-graced"), 1);
    manager
        .resource_start(&session.request, &session.device_id, 4)
        .await
        .expect("fixture admission");

    manager.observe_fence().await;
    let proved = manager
        .fence
        .observed()
        .expect("the healthy read at t=0 is published");
    assert_eq!(settings_reads(&manager, SettingsReadSite::Observer), (1, 0));

    tokio::time::pause();
    manager.test_fail_settings_reads(true);
    for second in 1..=9 {
        tokio::time::advance(FENCE_OBSERVATION_INTERVAL).await;
        manager.observe_fence().await;
        let kept = manager
            .fence
            .observed()
            .expect("a failed read keeps the last observation rather than clearing it");
        assert!(
            Arc::ptr_eq(&kept, &proved),
            "a failed read at t={second}s must leave the t=0 observation in place, \
                 neither clearing it nor re-publishing it as new"
        );
    }
    assert_eq!(
        settings_reads(&manager, SettingsReadSite::Observer),
        (1, 9),
        "every tick took a read and every one of them failed"
    );
    ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect("nine seconds of failed reads must not end a healthy stream");
    assert_eq!(fence_observations(&manager), (0, 1, 0));

    for _ in 0..2 {
        tokio::time::advance(FENCE_OBSERVATION_INTERVAL).await;
        manager.observe_fence().await;
    }
    assert!(Arc::ptr_eq(
        &manager.fence.observed().expect("still the t=0 observation"),
        &proved
    ));
    assert_eq!(
        settings_reads(&manager, SettingsReadSite::Observer),
        (1, 11)
    );
    let error = ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect_err("past the grace bound the session ends");
    assert_eq!(error.code(), "owner_unavailable");
    let message = error.to_string();
    assert!(
        message.contains("replicated settings"),
        "the sentence must name the settings store: {message}"
    );
    assert!(
        !message.contains("HDHomeRun") && !message.contains("session server"),
        "and must not read as a tuner problem, which is what the old \
             device_unavailable did: {message}"
    );
    assert_eq!(fence_observations(&manager), (0, 1, 1));

    // The first read that succeeds again replaces the observation; nothing
    // about the outage is latched.
    manager.test_fail_settings_reads(false);
    manager.observe_fence().await;
    assert!(!Arc::ptr_eq(
        &manager.fence.observed().expect("a fresh observation"),
        &proved
    ));
    ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect("a recovered read is fresh again");
    assert_eq!(fence_observations(&manager), (1, 1, 1));
}

#[tokio::test]
async fn settings_reads_are_counted_by_the_site_that_took_them() {
    // The session fence's cost is the observer's 1 Hz read plus the two
    // direct fences per start. Every Live TV API handler also reads
    // settings, so an unlabelled total says nothing about the fence.
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let session = test_session(root.path().join("live-tv-sites"), 1);

    manager.config().await.expect("a handler read");
    manager.config().await.expect("another handler read");
    manager.observe_fence().await;
    ensure_session_fence(&manager, &session)
        .await
        .expect("a direct fence read");
    manager.test_fail_settings_reads(true);
    manager.observe_fence().await;

    assert_eq!(settings_reads(&manager, SettingsReadSite::Request), (2, 0));
    assert_eq!(settings_reads(&manager, SettingsReadSite::Observer), (1, 1));
    assert_eq!(settings_reads(&manager, SettingsReadSite::Fence), (1, 0));
    let prometheus = manager.metrics.settings_reads_prometheus();
    for line in [
        "plurx_live_tv_settings_reads_total{site=\"observer\",outcome=\"ok\"} 1\n",
        "plurx_live_tv_settings_reads_total{site=\"observer\",outcome=\"failed\"} 1\n",
        "plurx_live_tv_settings_reads_total{site=\"fence\",outcome=\"ok\"} 1\n",
        "plurx_live_tv_settings_reads_total{site=\"fence\",outcome=\"failed\"} 0\n",
        "plurx_live_tv_settings_reads_total{site=\"request\",outcome=\"ok\"} 2\n",
        "plurx_live_tv_settings_reads_total{site=\"request\",outcome=\"failed\"} 0\n",
    ] {
        assert!(
            prometheus.contains(line),
            "missing {line:?} in {prometheus}"
        );
    }
}

/// Plan L-02 §3.1: the settings observer reads only while a transport is
/// held. It used to take its leader-consistent read of the whole settings
/// table once a second on every node, live or not, to keep two gauges and
/// the guide cache's generation current. Those now ride on change
/// signals, so an idle node takes no consistent read, its gauges keep what
/// it last proved, and they blank only when that proof is lost.
/// A proved caption service is advertised only to a player that decodes
/// it. hls.js has no CEA-708 decoder and folds `SERVICE1` onto the text
/// track it made for `CC1`, so the web client was shown a phantom
/// "English 708" in place of CC1's label.
#[test]
fn advertised_caption_services_follow_the_players_decoders() {
    let delivery = test_encode_delivery(720);
    let source = delivery.source.clone();
    let proof = caption_probe::CaptionProof {
        encoder: Encoder::Software.label().to_owned(),
        packaging: delivery.packaging,
        deinterlace: delivery.deinterlace_output,
        source_height: source.height.expect("fixture source height"),
        output_height: delivery.output.height,
        services: vec!["CC1", "SERVICE1"],
    };
    let advertised = |decoders: Option<&[String]>| {
        advertise_proven_captions(
            std::slice::from_ref(&proof),
            &source,
            delivery.clone(),
            Some(Encoder::Software.label()),
            decoders,
        )
        .reasons
        .into_iter()
        .find(|reason| reason.code == "captions_advertised")
        .map(|reason| reason.explanation)
    };
    assert_eq!(
        advertised(None).as_deref(),
        Some("CC1,SERVICE1"),
        "a player that makes no claim is offered what it always was"
    );
    let request: crate::live_tv_delivery::LivePlaybackRequest = serde_json::from_value(
        serde_json::json!({"v": 1, "caps": {"v": 2, "caption_decoders": ["cea608"]}}),
    )
    .expect("the web's live envelope");
    let hls_js = request.caption_decoders().expect("the web's claim");
    assert_eq!(advertised(Some(&hls_js)).as_deref(), Some("CC1"));
    let native = ["cea608".to_owned(), "cea708".to_owned()];
    assert_eq!(advertised(Some(&native)).as_deref(), Some("CC1,SERVICE1"));
    assert_eq!(advertised(Some(&[])), None);
}

#[tokio::test]
async fn the_settings_observer_reads_only_while_a_transport_is_held() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    tokio::time::pause();
    let observer_reads =
        |manager: &LiveTvManager| settings_reads(manager, SettingsReadSite::Observer).0;
    let enabled_gauge = |manager: &LiveTvManager| {
        let prometheus = manager.metrics.prometheus();
        prometheus
            .lines()
            .find_map(|line| line.strip_prefix("plurx_live_tv_enabled "))
            .expect("the enabled gauge")
            .to_owned()
    };
    let (applied, changes) = tokio::sync::watch::channel(0u64);
    manager.set_settings_changes(SettingsChangeSource::Test(changes));
    let shutdown = CancellationToken::new();
    let observer = tokio::spawn(Arc::clone(&manager).settings_observer_loop(shutdown.clone()));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(observer_reads(&manager), 1, "one observation at boot");

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        observer_reads(&manager),
        1,
        "a node with nothing live took a consistent settings read"
    );
    assert_eq!(
        enabled_gauge(&manager),
        "1",
        "a parked observer's proof keeps the gauge"
    );

    // A replicated write that leaves the Live TV generation alone costs a
    // replica read, not a consistent one.
    applied.send_replace(1);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(observer_reads(&manager), 1);
    // A save on another node bumps the generation: one consistent read.
    manager
        .store
        .put_settings(&[(keys::LIVE_TV_CONFIG_GENERATION, "2")])
        .await
        .expect("a save elsewhere");
    applied.send_replace(2);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(observer_reads(&manager), 2);
    assert_eq!(
        manager
            .fence
            .observed()
            .expect("an observation")
            .config
            .generation,
        2
    );

    // A transport opens (`start_local` and the DVR wake the observer where
    // they insert one): once a second while it is held.
    let session = test_session(root.path().join("live-tv-observed"), 2);
    let transport = give_own_transport(&manager, &session);
    manager.wake_settings_observer();
    tokio::time::sleep(Duration::from_millis(10_500)).await;
    let live = observer_reads(&manager) - 2;
    assert!((9..=11).contains(&live), "{live} reads in ten live seconds");

    // Released: the reads stop within one interval.
    manager
        .registry
        .lock()
        .expect("registry")
        .transports
        .clear();
    drop(transport);
    tokio::time::sleep(FENCE_OBSERVATION_INTERVAL + Duration::from_millis(500)).await;
    let released = observer_reads(&manager);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(observer_reads(&manager), released);
    assert_eq!(enabled_gauge(&manager), "1");

    // The change source closes: no later save could be seen, so the
    // parked reading is no longer proved and the gauge ages out.
    drop(applied);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(enabled_gauge(&manager), "NaN");
    assert_eq!(observer_reads(&manager), released);

    shutdown.cancel();
    observer.await.expect("the observer loop");
}

/// A failed boot read proves nothing, so the observer must not park on it:
/// parked, nothing but an unrelated replicated write would make it read
/// again, and the gauges would stay unproved until then. It keeps reading
/// at the running cadence until a read succeeds, and only then parks.
#[tokio::test]
async fn a_failed_boot_read_keeps_the_settings_observer_reading_until_one_succeeds() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    tokio::time::pause();
    let enabled_gauge = |manager: &LiveTvManager| {
        let prometheus = manager.metrics.prometheus();
        prometheus
            .lines()
            .find_map(|line| line.strip_prefix("plurx_live_tv_enabled "))
            .expect("the enabled gauge")
            .to_owned()
    };
    let (_applied, changes) = tokio::sync::watch::channel(0u64);
    manager.set_settings_changes(SettingsChangeSource::Test(changes));
    manager.test_fail_settings_reads(true);
    let shutdown = CancellationToken::new();
    let observer = tokio::spawn(Arc::clone(&manager).settings_observer_loop(shutdown.clone()));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        settings_reads(&manager, SettingsReadSite::Observer),
        (0, 1),
        "the boot read failed"
    );

    // Unproved: one read an interval, not parked and not back to back.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (ok, failed) = settings_reads(&manager, SettingsReadSite::Observer);
    assert_eq!(ok, 0);
    assert!(
        (5..=7).contains(&failed),
        "{failed} reads in five unproved seconds"
    );
    assert_eq!(enabled_gauge(&manager), "NaN");

    // The store answers again: the next read proves the settings, and the
    // observer parks on that proof.
    manager.test_fail_settings_reads(false);
    tokio::time::sleep(FENCE_OBSERVATION_INTERVAL + Duration::from_millis(500)).await;
    let proved = settings_reads(&manager, SettingsReadSite::Observer);
    assert_eq!(proved.0, 1, "the first read after recovery succeeded");
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        settings_reads(&manager, SettingsReadSite::Observer),
        proved,
        "a proved observer with nothing live parked"
    );
    assert_eq!(enabled_gauge(&manager), "1");

    shutdown.cancel();
    observer.await.expect("the observer loop");
}

#[tokio::test]
async fn a_session_with_no_observation_at_all_is_refused_rather_than_admitted() {
    // "Unknown, continue" is the failure mode this whole fence exists to
    // prevent. A node that has never managed a read has proved nothing.
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let session = test_session(root.path().join("live-tv-unread"), 1);
    let error = ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect_err("no observation is not permission to keep streaming");
    assert_eq!(error.code(), "owner_unavailable");
    assert!(error.to_string().contains("replicated settings"));
    assert_eq!(fence_observations(&manager), (0, 0, 1));
}

#[tokio::test]
async fn a_fresh_observation_that_fails_validation_ends_the_session_exactly_as_before() {
    // The three `validate_start_config` checks are not weakened by moving
    // where the config comes from: a change another node saved reaches
    // every session through the next observation, with the same error
    // codes the clients already branch on.
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let session = test_session(root.path().join("live-tv-conflict"), 1);
    manager
        .resource_start(&session.request, &session.device_id, 4)
        .await
        .expect("fixture admission");

    manager.observe_fence().await;
    ensure_session_fence_from_observation(&manager, &session)
        .await
        .expect("the seeded configuration admits this session");

    manager
        .store
        .put_settings(&[(keys::LIVE_TV_CONFIG_GENERATION, "2")])
        .await
        .expect("another node saves a configuration change");
    manager.observe_fence().await;
    assert_eq!(
        ensure_session_fence_from_observation(&manager, &session)
            .await
            .expect_err("a generation change ends the session")
            .code(),
        "settings_conflict"
    );

    manager
        .store
        .put_settings(&[
            (keys::LIVE_TV_CONFIG_GENERATION, "1"),
            (keys::LIVE_TV_ENABLED, "0"),
        ])
        .await
        .expect("and a disable");
    manager.observe_fence().await;
    assert_eq!(
        ensure_session_fence_from_observation(&manager, &session)
            .await
            .expect_err("a disable ends the session")
            .code(),
        "live_tv_disabled"
    );
}

#[tokio::test]
async fn the_start_and_publication_fences_read_the_store_rather_than_the_observation() {
    // A stale-but-in-grace observation must never admit a *first* segment:
    // before publication the cost of failing closed is a retry, and after
    // it is the stream the client is already watching. So those two fences
    // keep their direct read, and only the per-second loop fence uses the
    // shared observation.
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let session = test_session(root.path().join("live-tv-publish"), 1);
    manager.observe_fence().await;

    tokio::time::pause();
    tokio::time::advance(FENCE_GRACE_MAX_AGE + Duration::from_secs(1)).await;
    assert!(
        ensure_session_fence_from_observation(&manager, &session)
            .await
            .is_err(),
        "the loop fence is past its grace bound"
    );

    let before = settings_reads(&manager, SettingsReadSite::Fence);
    ensure_session_fence(&manager, &session)
        .await
        .expect("the direct fence reads the store and finds it healthy");
    assert_eq!(
        settings_reads(&manager, SettingsReadSite::Fence),
        (before.0 + 1, before.1),
        "the start and publication fences must spend a read of their own"
    );
    // With the observer parked that read is the only recent one, so it is
    // what the session's first running fence validates against.
    manager
        .fence
        .validated()
        .expect("the start's own read is published while nothing fresher is");
}

#[test]
fn the_fence_bounds_are_sized_against_the_reads_they_stand_for() {
    // Fresh is one observation interval plus one `STORE_TIMEOUT`: the
    // longest a single successful consistent read may take.
    assert_eq!(
        FENCE_OBSERVATION_FRESH_MAX_AGE,
        FENCE_OBSERVATION_INTERVAL + Duration::from_secs(3)
    );
    // The grace must outlast one whole authority-read retry budget — two
    // 3 s attempts 100 ms apart — or a leader failover ends healthy
    // streams, which is the failure L2 names.
    let authority_read_budget = Duration::from_secs(3) * 2 + Duration::from_millis(100);
    assert!(FENCE_GRACE_MAX_AGE > authority_read_budget);
    // And it must be a bound rather than an open end: a session may not
    // outlive the last thing this node proved by more than this.
    assert_eq!(FENCE_GRACE_MAX_AGE, Duration::from_secs(10));
    assert!(FENCE_OBSERVATION_FRESH_MAX_AGE < FENCE_GRACE_MAX_AGE);
}

#[tokio::test]
async fn live_tv_sweeper_preserves_registered_scratch_and_removes_only_orphans() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    tokio::fs::create_dir_all(&manager.scratch_root)
        .await
        .expect("Live TV root");
    let active_path = manager.scratch_root.join("live-tv-active");
    tokio::fs::create_dir(&active_path)
        .await
        .expect("active scratch");
    tokio::fs::write(active_path.join("index.m3u8"), b"active")
        .await
        .expect("active playlist");
    let session = test_session(active_path.clone(), 1);
    manager
        .registry
        .lock()
        .expect("registry")
        .sessions
        .insert(session.capability.clone(), session);
    let orphan = manager.scratch_root.join("live-tv-orphan");
    tokio::fs::create_dir(&orphan)
        .await
        .expect("orphan scratch");

    assert_eq!(manager.sweep_orphan_scratch().await, 1);
    assert!(active_path.join("index.m3u8").is_file());
    assert!(!orphan.exists());
}

#[tokio::test]
async fn shutdown_acknowledges_only_after_registry_and_scratch_cleanup() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    tokio::fs::create_dir_all(&manager.scratch_root)
        .await
        .expect("Live TV root");
    let path = manager.scratch_root.join("live-tv-drain");
    tokio::fs::create_dir(&path).await.expect("session scratch");
    let session = test_session(path.clone(), 1);
    manager
        .registry
        .lock()
        .expect("registry")
        .sessions
        .insert(session.capability.clone(), Arc::clone(&session));
    let task_manager = Arc::clone(&manager);
    let task_session = Arc::clone(&session);
    tokio::spawn(async move {
        task_session.cancel.cancelled().await;
        tokio::fs::remove_dir_all(&task_session.directory)
            .await
            .expect("physical scratch cleanup");
        task_manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .remove(&task_session.capability);
        task_session.changed.notify_waiters();
    });

    assert_eq!(manager.shutdown().await.expect("confirmed drain"), 1);
    assert!(!path.exists());
    assert!(manager.activities().is_empty());
}

#[tokio::test]
async fn activation_keeps_its_token_and_generation_fences_across_ingress_changes() {
    let root = crate::test_tempdir().expect("activation root");
    let manager = test_manager(root.path());
    seed_test_config(&manager).await;
    let session = test_session(manager.scratch_root.join("live-tv-activation"), 1);
    manager
        .resource_start(&session.request, &session.device_id, 4)
        .await
        .expect("fixture admission");

    {
        let mut state = session.state.lock().expect("session state");
        state.phase = LiveTvSessionPhase::Provisional;
        state.activated = false;
    }
    manager
        .registry
        .lock()
        .expect("registry")
        .sessions
        .insert(session.capability.clone(), Arc::clone(&session));
    let mut request = LiveTvActivateRequest {
        expected_owner_node_id: "node-a".into(),
        capability: session.capability.clone(),
        activation_token: session.activation_token.clone(),
        config_generation: 1,
        source_serving_generation: 0,
    };

    let original_token = request.activation_token.clone();
    request.activation_token = "wrong-token".into();
    assert!(matches!(
        manager.activate_local(&request, "node-b").await,
        Err(LiveTvError::Conflict(_))
    ));
    request.activation_token = original_token;
    request.config_generation = 2;
    assert!(matches!(
        manager.activate_local(&request, "node-b").await,
        Err(LiveTvError::Conflict(_))
    ));
    request.config_generation = 1;
    request.source_serving_generation = 1;
    assert!(
        manager
            .activate_local(&request, "node-b")
            .await
            .expect("valid activation through another ingress")
            .live
    );

    let mut other_generation = session.request.clone();
    other_generation.source_serving_generation = 1;
    assert_eq!(
        LiveTvRequestKey::from(&session.request),
        LiveTvRequestKey::from(&other_generation),
        "a replay keeps its identity across ingress serving generations"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_producer_answers_only_once_two_segments_are_listed() {
    use std::sync::atomic::AtomicUsize;

    let root = crate::test_tempdir().expect("lifecycle root");
    let ffmpeg = root.path().join("fake-ffmpeg");
    crate::write_test_executable(
        &ffmpeg,
        r#"#!/bin/sh
for output do playlist="$output"; done
directory=${playlist%/*}
root="${0%/*}/live-tv"
printf 'transport-stream' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$directory/segment-000001.ts" > /dev/null
printf '#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1.000000,\nsegment-000001.ts\n' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$playlist" > /dev/null
while [ ! -f "$root/release-second" ]; do sleep 0.01; done
printf 'transport-stream-two' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$directory/segment-000002.ts" > /dev/null
printf '#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1.000000,\nsegment-000001.ts\n#EXTINF:1.000000,\nsegment-000002.ts\n' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$playlist" > /dev/null
exec /bin/cat >/dev/null
"#,
        0o755,
    );
    let ffprobe = root.path().join("fake-ffprobe");
    crate::write_test_executable(
        &ffprobe,
        r#"#!/bin/sh
printf '%s' '{"streams":[{"codec_type":"video","codec_name":"mpeg2video","width":1920,"height":1080,"field_order":"progressive"},{"codec_type":"audio","codec_name":"ac3","channels":2}]}'
"#,
        0o755,
    );
    let system = SystemInfo {
        ffmpeg: ffmpeg.to_string_lossy().into_owned(),
        ffprobe: ffprobe.to_string_lossy().into_owned(),
        ..SystemInfo::default()
    };
    let mut manager = test_manager_with_system(root.path(), system);
    seed_test_config(&manager).await;

    let get_count = Arc::new(AtomicUsize::new(0));
    let observed_get_count = Arc::clone(&get_count);
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake tuner");
    let address = listener.local_addr().expect("fake tuner address");
    // Test-only transport injection, not a production URL override: run
    // the actual pinned-address discovery/lineup/start implementation via
    // a loopback HTTP proxy fixture.
    Arc::get_mut(&mut manager)
        .expect("unique test manager")
        .client = Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(format!("http://{address}")).expect("fixture proxy"))
        .build()
        .expect("fixture client"));
    let tuner = tokio::spawn(async move {
        let mut tuner_stream = None;
        for expected_path in ["/discover.json", "/lineup.json", "/auto/v7.1"] {
            let (mut stream, _) = listener.accept().await.expect("accept tuner request");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).await.expect("read tuner GET");
            assert!(String::from_utf8_lossy(&request[..read]).contains(expected_path));
            let document = match expected_path {
                "/discover.json" => Some(
                    r#"{"FriendlyName":"Fixture","ModelNumber":"HDFX-4K","FirmwareVersion":"fixture","DeviceID":"10ABCDEF","TunerCount":4}"#,
                ),
                "/lineup.json" => Some(
                    r#"[{"GuideNumber":"7.1","GuideName":"Test","URL":"http://ignored.local:5004/auto/v7.1"}]"#,
                ),
                _ => None,
            };
            if let Some(document) = document {
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes()).await.expect("fixture document");
            } else {
                observed_get_count.fetch_add(1, Ordering::Relaxed);
                tuner_stream = Some(stream);
            }
        }
        let mut stream = tuner_stream.expect("one tuner stream");
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("write tuner headers");
        loop {
            if stream.write_all(b"4\r\ndata\r\n").await.is_err() {
                let _ = closed_tx.send(());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    // The session's per-second fence validates against the node's 1 Hz
    // settings observation; run that loop as production does (and as
    // `start_path_fixture` does), because this test now drives the start
    // for as long as it waits and the session's timers run with it.
    let observed = Arc::downgrade(&manager);
    let observer = tokio::spawn(async move {
        while let Some(manager) = observed.upgrade() {
            manager.observe_fence().await;
            drop(manager);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let request = test_session(root.path().join("unused"), 1).request.clone();
    let mut start = Box::pin(manager.start_local(request.clone()));
    // Drive the start while watching its session: it must still be
    // unanswered when the producer has listed exactly one segment. The
    // start used to be polled for a fixed 700 ms and then left alone while
    // the registry was read, so a runner too loaded to register the
    // session inside that window found no session at all. The wait is
    // bounded only against a hang.
    let first_publication = async {
        loop {
            let observed = {
                let registry = manager.registry.lock().expect("registry");
                registry.sessions.values().next().map(|session| {
                    let state = session.state.lock().expect("session state");
                    assert!(state.startup.is_none());
                    state
                        .publication
                        .as_ref()
                        .map(|publication| publication.listed)
                })
            };
            if observed == Some(Some(1)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        biased;
        answered = &mut start => panic!(
            "one listed segment must not answer the start (answered ok: {})",
            answered.is_ok()
        ),
        () = first_publication => {}
        () = tokio::time::sleep(LIVE_TV_TEST_HANG_GUARD) => {
            panic!("producer never published its first listed segment")
        }
    }
    // And one listed segment keeps it unanswered while it is driven on.
    assert!(
        tokio::time::timeout(Duration::from_millis(700), &mut start)
            .await
            .is_err(),
        "one listed segment must not answer the start"
    );
    tokio::fs::write(manager.scratch_root.join("release-second"), b"release")
        .await
        .expect("release second segment");
    let provisional = tokio::time::timeout(LIVE_TV_TEST_HANG_GUARD, &mut start)
        .await
        .expect("second segment publication deadline")
        .expect("first HLS publication");
    drop(start);
    let recovered = manager
        .start_local(request.clone())
        .await
        .expect("same request recovers without GET");
    assert_eq!(recovered.capability, provisional.capability);
    assert_eq!(recovered.outcome, LiveTvStartOutcome::Recovered);
    let session = manager
        .session(&provisional.capability)
        .expect("registered session");
    assert_eq!(provisional.outcome, LiveTvStartOutcome::Created);
    let activated = manager
        .activate_local(
            &LiveTvActivateRequest {
                expected_owner_node_id: "node-a".into(),
                capability: provisional.capability.clone(),
                activation_token: provisional.activation_token,
                config_generation: 1,
                source_serving_generation: 0,
            },
            "node-a",
        )
        .await
        .expect("activate live TV");
    assert!(activated.live);

    let playlist = manager
        .resource_local(LiveTvResourceRequest::Playlist {
            capability: provisional.capability.clone(),
        })
        .await
        .expect("serve playlist");
    let playlist = axum::body::to_bytes(playlist.into_body(), MAX_PLAYLIST_BYTES as usize)
        .await
        .expect("playlist bytes");
    assert!(playlist.ends_with(b"segment-000002.ts\n"));
    let segment = manager
        .resource_local(LiveTvResourceRequest::Segment {
            capability: provisional.capability.clone(),
            sequence: 1,
        })
        .await
        .expect("serve segment");
    let segment = axum::body::to_bytes(segment.into_body(), MAX_SEGMENT_BYTES as usize)
        .await
        .expect("segment bytes");
    assert_eq!(&segment[..], b"transport-stream");

    manager
        .stop_local(&provisional.capability)
        .await
        .expect("confirmed stop");
    assert!(
        matches!(
            manager.start_local(request).await,
            Err(LiveTvError::CapabilityExpired(_))
        ),
        "a delayed replay must not open another tuner after cleanup"
    );
    tokio::time::timeout(Duration::from_secs(2), closed_rx)
        .await
        .expect("tuner socket close deadline")
        .expect("tuner socket close signal");
    assert_eq!(get_count.load(Ordering::Relaxed), 1);
    assert!(manager.activities().is_empty());
    assert!(!session.directory.exists());
    tuner.await.expect("fake tuner");
    observer.abort();
}

/// A loopback HDHomeRun for start-path tests. It answers discovery and
/// the lineup, and streams every tuner GET 16 KiB a millisecond until the
/// client goes away, counting the GETs. With `lineup_gate` above one it
/// holds lineup answers until that many are waiting (or a second has
/// passed), so concurrent starts reach admission together.
struct FixtureTuner {
    gets: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
    /// The node's 1 Hz settings observer, when a manager is attached.
    observer: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for FixtureTuner {
    fn drop(&mut self) {
        self.server.abort();
        if let Some(observer) = &self.observer {
            observer.abort();
        }
    }
}

async fn fixture_tuner(lineup_gate: usize) -> (reqwest::Client, FixtureTuner) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture tuner");
    let address = listener.local_addr().expect("fixture tuner address");
    // Test-only transport injection, as in the producer test above: the
    // real pinned-address discovery, lineup and tuner GET, through a
    // loopback proxy.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(format!("http://{address}")).expect("fixture proxy"))
        .build()
        .expect("fixture client");
    let gets = Arc::new(AtomicUsize::new(0));
    let lineups = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(tokio::sync::Notify::new());
    let server = {
        let gets = Arc::clone(&gets);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let gets = Arc::clone(&gets);
                let lineups = Arc::clone(&lineups);
                let released = Arc::clone(&released);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 2048];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let line = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    let document = if line.contains("/discover.json") {
                        Some(
                            r#"{"FriendlyName":"Fixture","ModelNumber":"HDFX-4K","FirmwareVersion":"fixture","DeviceID":"10ABCDEF","TunerCount":4}"#,
                        )
                    } else if line.contains("/lineup.json") {
                        let release = released.notified();
                        tokio::pin!(release);
                        release.as_mut().enable();
                        if lineups.fetch_add(1, Ordering::SeqCst) + 1 >= lineup_gate {
                            released.notify_waiters();
                        } else {
                            let _ = tokio::time::timeout(Duration::from_secs(1), release).await;
                        }
                        Some(
                            r#"[{"GuideNumber":"7.1","GuideName":"Test","URL":"http://ignored.local:5004/auto/v7.1"}]"#,
                        )
                    } else {
                        None
                    };
                    if let Some(document) = document {
                        let _ = stream
                                .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes())
                                .await;
                        return;
                    }
                    if !line.contains("/auto/v") {
                        let _ = stream
                                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .await;
                        return;
                    }
                    gets.fetch_add(1, Ordering::SeqCst);
                    if stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nTransfer-Encoding: chunked\r\n\r\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                    // Chunk `n` of a GET is 16 KiB of the byte `n % 251`,
                    // so a capture of what an FFmpeg was fed shows any
                    // gap or repeat as a break in the sequence.
                    let mut sent = 0_usize;
                    loop {
                        let chunk = vec![(sent % 251) as u8; 16 * 1024];
                        let frame = [
                            format!("{:x}\r\n", chunk.len()).into_bytes(),
                            chunk,
                            b"\r\n".to_vec(),
                        ]
                        .concat();
                        if stream.write_all(&frame).await.is_err() {
                            break;
                        }
                        sent += 1;
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                });
            }
        })
    };
    (
        client,
        FixtureTuner {
            gets,
            server,
            observer: None,
        },
    )
}

/// A manager wired to a fixture tuner, a scripted FFmpeg (`ffmpeg` is the
/// script's body) and an FFprobe that prints `<root>/probe.json`, fails
/// while it is absent, and first sleeps a second while `<root>/probe.slow`
/// exists.
#[cfg(unix)]
async fn start_path_fixture(
    root: &Path,
    ffmpeg: &str,
    lineup_gate: usize,
) -> (Arc<LiveTvManager>, FixtureTuner) {
    start_path_fixture_for_node(root, ffmpeg, lineup_gate, "node-a").await
}

#[cfg(unix)]
async fn start_path_fixture_for_node(
    root: &Path,
    ffmpeg: &str,
    lineup_gate: usize,
    node: &str,
) -> (Arc<LiveTvManager>, FixtureTuner) {
    let ffmpeg_path = root.join("fake-ffmpeg");
    crate::write_test_executable(&ffmpeg_path, format!("#!/bin/sh\n{ffmpeg}\n"), 0o755);
    let ffprobe_path = root.join("fake-ffprobe");
    crate::write_test_executable(&ffprobe_path, format!(
                "#!/bin/sh\n[ -f '{slow}' ] && /bin/sleep 1\n[ -f '{answer}' ] || exit 1\n/bin/cat '{answer}'\nstatus=$?\n/bin/date +%s%N >> '{finished}'\nexit $status\n",
                slow = root.join("probe.slow").display(),
                answer = root.join("probe.json").display(),
                finished = root.join("probe.finished").display(),
            ), 0o755);
    let system = SystemInfo {
        ffmpeg: ffmpeg_path.to_string_lossy().into_owned(),
        ffprobe: ffprobe_path.to_string_lossy().into_owned(),
        ..SystemInfo::default()
    };
    let mut manager = test_manager_with_system(root, system);
    Arc::get_mut(&mut manager).expect("unique manager").node_id = node.into();
    seed_test_config(&manager).await;
    let (client, mut tuner) = fixture_tuner(lineup_gate).await;
    Arc::get_mut(&mut manager)
        .expect("unique test manager")
        .client = Ok(client);
    // Every session's per-second fence validates against the node's
    // 1 Hz settings observation; run that loop as production does, so a
    // session in these tests ends for its own reason and not the fence's.
    let observed = Arc::downgrade(&manager);
    tuner.observer = Some(tokio::spawn(async move {
        while let Some(manager) = observed.upgrade() {
            manager.observe_fence().await;
            drop(manager);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }));
    (manager, tuner)
}

const PROBED_480: &str = r#"{"streams":[{"codec_type":"video","codec_name":"mpeg2video","width":720,"height":480,"field_order":"tt"},{"codec_type":"audio","codec_name":"ac3","channels":2}]}"#;
const PROBED_H264_180: &str = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":320,"height":180,"field_order":"progressive"},{"codec_type":"audio","codec_name":"aac","channels":2}]}"#;

/// A scripted FFmpeg for the start-path tests: it logs `<pid> <ns>` to
/// `<root>/ffmpeg.spawns`, publishes two listed segments at once, and
/// then either discards what it is fed or keeps it in `<root>/stdin-<pid>`.
fn publishing_ffmpeg(root: &Path, keep_input: bool) -> String {
    let sink = if keep_input {
        format!("'{}/stdin-'$$", root.display())
    } else {
        "/dev/null".to_owned()
    };
    format!(
        r#"echo "$$ $(/bin/date +%s%N)" >> '{spawns}'
for output do playlist="$output"; done
directory=${{playlist%/*}}
printf 'transport-stream' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$directory/segment-000001.ts" > /dev/null
printf 'transport-stream-two' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$directory/segment-000002.ts" > /dev/null
printf '#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1.000000,\nsegment-000001.ts\n#EXTINF:1.000000,\nsegment-000002.ts\n' | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$playlist" > /dev/null
exec /bin/cat > {sink}"#,
        spawns = root.join("ffmpeg.spawns").display(),
    )
}

/// `(pid, unix nanoseconds)` for every FFmpeg the fixture started, in order.
fn ffmpeg_spawns(root: &Path) -> Vec<(u32, u128)> {
    std::fs::read_to_string(root.join("ffmpeg.spawns"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let (pid, at) = line.split_once(' ').expect("pid and time");
            (pid.parse().expect("pid"), at.parse().expect("nanoseconds"))
        })
        .collect()
}

/// Unix nanoseconds at which each fixture probe finished, in order.
fn probes_finished(root: &Path) -> Vec<u128> {
    std::fs::read_to_string(root.join("probe.finished"))
        .unwrap_or_default()
        .lines()
        .map(|line| line.parse().expect("nanoseconds"))
        .collect()
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_nanos()
}

fn start_plans(manager: &LiveTvManager) -> [u64; 4] {
    std::array::from_fn(|index| manager.metrics.start_plans[index].load(Ordering::Acquire))
}

/// Start once cold, stop, and wait for the transport to close: the
/// channel's facts are then cached, as they are after any real watch.
async fn watch_once_cold(manager: &Arc<LiveTvManager>) -> LiveTvProvisional {
    let provisional = tokio::time::timeout(
        Duration::from_secs(10),
        manager.start_local(fixture_request(1)),
    )
    .await
    .expect("cold start deadline")
    .expect("cold start");
    manager
        .stop_local(&provisional.capability)
        .await
        .expect("stop the cold start");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !manager
        .registry
        .lock()
        .expect("registry")
        .transports
        .is_empty()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cold transport never closed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    provisional
}

/// The one session registered, once its inventory is publishable.
async fn publishable_session(manager: &LiveTvManager) -> Arc<LiveTvSession> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .values()
            .next()
            .cloned();
        if let Some(session) = session {
            let listed = session
                .state
                .lock()
                .expect("state")
                .publication
                .as_ref()
                .map_or(0, |publication| publication.listed);
            if listed >= STARTUP_LISTED_SEGMENTS {
                return session;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no publishable inventory"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Plan L-02 §5.3, the cold half: with nothing cached the start is
/// today's sequence — the opening probe finishes before FFmpeg is
/// spawned — and it leaves the probed facts for the next opener.
#[cfg(unix)]
#[tokio::test]
async fn a_cold_start_probes_before_it_spawns_and_caches_the_facts() {
    let root = crate::test_tempdir().expect("root");
    let (manager, tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    let key = SourceFormatKey {
        generation: 1,
        device_id: "10ABCDEF".into(),
        channel_id: "7.1".into(),
    };
    assert!(manager.warm_source_facts(&key).is_none());

    let provisional = watch_once_cold(&manager).await;
    let spawns = ffmpeg_spawns(root.path());
    let probes = probes_finished(root.path());
    assert_eq!(spawns.len(), 1);
    assert_eq!(probes.len(), 1);
    assert!(
        probes[0] < spawns[0].1,
        "a cold start spawns only after its probe"
    );
    assert_eq!(tuner.gets.load(Ordering::SeqCst), 1);
    assert_eq!(start_plans(&manager), [1, 0, 0, 0]);
    let cached = manager
        .warm_source_facts(&key)
        .expect("facts cached for the next opener");
    assert_eq!(cached.video_codec.as_deref(), Some("mpeg2video"));
    assert_eq!(
        provisional
            .delivery
            .as_ref()
            .map(|delivery| &delivery.source.video_codec),
        Some(&cached.video_codec)
    );
}

/// Plan L-02 §5.3: a warm opener spawns FFmpeg without waiting for the
/// prefix and its probe, and still publishes nothing until this tune's
/// probe has re-proven the plan — although its inventory was publishable
/// long before.
#[cfg(unix)]
#[tokio::test]
async fn a_warm_start_spawns_before_the_prefix_ceiling_and_publishes_only_after_agreement() {
    let root = crate::test_tempdir().expect("root");
    let (manager, tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    let cold = watch_once_cold(&manager).await;

    // Slow the verification probe so the window is wide and visible.
    std::fs::write(root.path().join("probe.slow"), b"").expect("slow probe");
    let requested = unix_nanos();
    let start = spawn_start(&manager, fixture_request(2));
    let session = publishable_session(&manager).await;
    assert_eq!(
        probes_finished(root.path()).len(),
        1,
        "the verification probe has not finished yet"
    );
    assert!(
        session.state.lock().expect("state").startup.is_none(),
        "a publishable inventory must not be published before the probe agrees"
    );
    assert!(
        session.tuner_bytes.load(Ordering::Acquire) > 0,
        "the warm FFmpeg is fed while its verification probe is still running"
    );
    let warm = tokio::time::timeout(Duration::from_secs(10), start)
        .await
        .expect("warm start deadline")
        .expect("start task")
        .expect("warm start");
    let answered = unix_nanos();

    let spawns = ffmpeg_spawns(root.path());
    let probes = probes_finished(root.path());
    assert_eq!(
        spawns.len(),
        2,
        "one FFmpeg for the cold start, one for the warm"
    );
    assert_eq!(probes.len(), 2);
    let warm_spawn = spawns[1].1;
    assert!(
        warm_spawn - requested < SOURCE_PREFIX_TIME.as_nanos(),
        "a warm start spawns before the prefix ceiling"
    );
    assert!(
        warm_spawn < probes[1],
        "…and before its verification probe finished"
    );
    assert!(
        probes[1] <= answered,
        "the answer came only after the probe"
    );
    assert_eq!(
        start_plans(&manager),
        [1, 0, 1, 0],
        "cold, then warm_agreed"
    );
    assert_eq!(tuner.gets.load(Ordering::SeqCst), 2);
    assert_eq!(
        warm.delivery, cold.delivery,
        "the same channel, the same plan"
    );
    manager.shutdown().await.expect("shutdown");
}

/// Plan L-02 §5.3: the cache says one thing and the mux another. The
/// FFmpeg started from the cache is killed and reaped, the start restarts
/// from the probe within the same start budget, and only the probed plan
/// is ever published.
#[cfg(unix)]
#[tokio::test]
async fn a_warm_start_that_disagrees_restarts_cold_and_never_publishes_the_wrong_plan() {
    let root = crate::test_tempdir().expect("root");
    let (manager, tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    watch_once_cold(&manager).await;

    // The broadcast changed format since the facts were cached.
    std::fs::write(root.path().join("probe.json"), PROBED_H264_180).expect("new format");
    let requested = tokio::time::Instant::now();
    let provisional = tokio::time::timeout(
        STARTUP_FEEDING_TIMEOUT,
        manager.start_local(fixture_request(2)),
    )
    .await
    .expect("the restarted start stays inside the start budget")
    .expect("restarted start");
    assert!(requested.elapsed() < STARTUP_FEEDING_TIMEOUT);

    let delivery = provisional.delivery.expect("delivery");
    assert_eq!(delivery.source.video_codec.as_deref(), Some("h264"));
    assert_eq!(delivery.source.height, Some(180));
    assert!(
        !delivery.deinterlace,
        "nothing of the cached interlaced plan survives"
    );
    let spawns = ffmpeg_spawns(root.path());
    assert_eq!(spawns.len(), 3, "cold, warm, and the restart");
    assert!(
        !process_exists(spawns[1].0),
        "the FFmpeg started from the wrong facts was killed and reaped"
    );
    assert_eq!(
        start_plans(&manager),
        [1, 0, 0, 1],
        "cold, then warm_disagreed"
    );
    assert_eq!(
        tuner.gets.load(Ordering::SeqCst),
        2,
        "the restart kept its tuner"
    );
    let key = SourceFormatKey {
        generation: 1,
        device_id: "10ABCDEF".into(),
        channel_id: "7.1".into(),
    };
    assert_eq!(
        manager
            .warm_source_facts(&key)
            .and_then(|facts| facts.video_codec),
        Some("h264".into()),
        "the cache now holds what this tune probed"
    );
    manager.shutdown().await.expect("shutdown");
}

/// The restart keeps its tuner however long the wrong FFmpeg takes to
/// reap. The seam holds the start after that reap until the fan-out has
/// closed the aborted pump's feed and run on for half a second: with no
/// live viewer on it, the transport retires at its next chunk unless the
/// restart already holds a seat. It used to reserve that seat only after
/// the reap, so on a loaded runner this start failed with "the shared
/// tuner connection closed".
#[cfg(unix)]
#[tokio::test]
async fn a_warm_restart_holds_its_tuner_seat_through_a_slow_reap() {
    let root = crate::test_tempdir().expect("root");
    let (manager, tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    watch_once_cold(&manager).await;

    std::fs::write(root.path().join("probe.json"), PROBED_H264_180).expect("new format");
    manager
        .linger_after_warm_reap
        .store(true, Ordering::Release);
    let provisional = tokio::time::timeout(
        STARTUP_FEEDING_TIMEOUT,
        manager.start_local(fixture_request(2)),
    )
    .await
    .expect("the restarted start stays inside the start budget")
    .expect("the restart kept a live tuner connection");
    let delivery = provisional.delivery.expect("delivery");
    assert_eq!(delivery.source.video_codec.as_deref(), Some("h264"));
    assert_eq!(
        start_plans(&manager),
        [1, 0, 0, 1],
        "cold, then warm_disagreed"
    );
    assert_eq!(
        tuner.gets.load(Ordering::SeqCst),
        2,
        "the restart kept its tuner"
    );
    manager.shutdown().await.expect("shutdown");
}

/// Plan L-02 §5.3: the warm FFmpeg is fed from the tuner's first byte,
/// and every byte after it, once.
#[cfg(unix)]
#[tokio::test]
async fn every_tuner_byte_reaches_ffmpeg_exactly_once_on_a_warm_start() {
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), true), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    watch_once_cold(&manager).await;
    let warm = tokio::time::timeout(
        Duration::from_secs(10),
        manager.start_local(fixture_request(2)),
    )
    .await
    .expect("warm start deadline")
    .expect("warm start");
    assert_eq!(start_plans(&manager), [1, 0, 1, 0]);
    manager.stop_local(&warm.capability).await.expect("stop");

    let spawns = ffmpeg_spawns(root.path());
    let fed = std::fs::read(root.path().join(format!("stdin-{}", spawns[1].0)))
        .expect("the warm FFmpeg's input");
    assert!(
        fed.len() > 16 * 1024,
        "the warm FFmpeg was fed ({} bytes)",
        fed.len()
    );
    for (offset, byte) in fed.iter().enumerate() {
        let expected = ((offset / (16 * 1024)) % 251) as u8;
        assert_eq!(
            *byte, expected,
            "byte {offset} of the warm FFmpeg's input is not the tuner's byte {offset}: \
                 a gap or a repeat"
        );
    }
}

/// Plan L-02 §5.4: a scratch scan that never returns — a `stat` on a
/// stalled mount — is polled on later ticks rather than replaced, so
/// exactly one is ever in flight, and the tick keeps running the timers
/// that end the session. A published session has no start waiter to time
/// it out: awaited inline, as it used to be, the hung scan stopped the
/// tick itself, and with it the producer-progress timer that is the only
/// thing that ends a session whose producer has stopped.
#[cfg(unix)]
#[tokio::test]
async fn a_hung_inventory_does_not_stack_and_the_progress_timer_still_ends_the_session() {
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    let provisional = tokio::time::timeout(
        Duration::from_secs(10),
        manager.start_local(fixture_request(1)),
    )
    .await
    .expect("start deadline")
    .expect("published start");
    let session = manager
        .session(&provisional.capability)
        .expect("registered session");
    let before = manager.scratch_scans_started.load(Ordering::Acquire);
    manager.hang_scratch_scans.store(true, Ordering::Release);

    let deadline =
        tokio::time::Instant::now() + PRODUCER_PROGRESS_TIMEOUT + Duration::from_secs(10);
    while manager
        .registry
        .lock()
        .expect("registry")
        .sessions
        .contains_key(&provisional.capability)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a hung scan must not stop the session's own timers"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let terminal = session.state.lock().expect("state").terminal_error.clone();
    assert!(
        matches!(&terminal, Some(LiveTvError::StreamFailed(message)) if message.contains("stopped advancing")),
        "{terminal:?}"
    );
    assert_eq!(
        manager.scratch_scans_started.load(Ordering::Acquire) - before,
        1,
        "one scan in flight from the moment the filesystem hung, never a second"
    );
}

/// A scripted FFmpeg that keeps producing until its scratch is removed: a
/// new one-second segment every 300 ms, a three-segment window, and the
/// one deletion-lag segment kept behind it.
fn advancing_ffmpeg() -> String {
    r#"for output do playlist="$output"; done
directory=${playlist%/*}
(
n=1
while :; do
  printf 'segment-%d' "$n" | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$directory/$(printf 'segment-%06d.ts' "$n")" > /dev/null
  first=1
  [ "$n" -gt 2 ] && first=$((n - 2))
  {
    printf '#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:%d\n' "$first"
    listed=$first
    while [ "$listed" -le "$n" ]; do
      printf '#EXTINF:1.000000,\nsegment-%06d.ts\n' "$listed"
      listed=$((listed + 1))
    done
  } | /usr/bin/curl --noproxy '*' -fsS -X PUT --data-binary @- "$playlist" > /dev/null
  stale=$((first - 2))
  [ "$stale" -ge 1 ] && /usr/bin/curl --noproxy '*' -fsS -X DELETE "$directory/$(printf 'segment-%06d.ts' "$stale")" > /dev/null
  n=$((n + 1))
  /bin/sleep 0.3
done
) &
exec /bin/cat > /dev/null"#
        .to_owned()
}

/// One playlist GET, bounded: before plan L-03 every GET took the
/// session's consistent ledger read first.
async fn served_playlist(manager: &LiveTvManager, capability: &str) -> bytes::Bytes {
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        manager.resource_local(LiveTvResourceRequest::Playlist {
            capability: capability.to_owned(),
        }),
    )
    .await
    .expect("a playlist GET must not wait on the Store")
    .expect("serve playlist");
    axum::body::to_bytes(response.into_body(), MAX_PLAYLIST_BYTES as usize)
        .await
        .expect("playlist bytes")
}

/// An active session from the advancing FFmpeg whose running ledger read
/// has started and hangs (plan L-03): the manager, the tuner, the
/// capability, the session, and the count of ledger reads started once the
/// hung one had.
#[cfg(unix)]
async fn session_with_a_hung_ledger_read(
    root: &Path,
) -> (
    Arc<LiveTvManager>,
    FixtureTuner,
    String,
    Arc<LiveTvSession>,
    u64,
) {
    let (manager, tuner) = start_path_fixture(root, &advancing_ffmpeg(), 1).await;
    std::fs::write(root.join("probe.json"), PROBED_480).expect("probe answer");
    let request = fixture_request(1);
    let provisional = tokio::time::timeout(
        LIVE_TV_TEST_HANG_GUARD,
        manager.start_local(request.clone()),
    )
    .await
    .expect("start deadline")
    .expect("published start");
    manager
        .activate_local(
            &LiveTvActivateRequest {
                expected_owner_node_id: "node-a".into(),
                capability: provisional.capability.clone(),
                activation_token: provisional.activation_token.clone(),
                config_generation: 1,
                source_serving_generation: 0,
            },
            "node-a",
        )
        .await
        .expect("activate live TV");
    let session = manager
        .session(&provisional.capability)
        .expect("registered session");
    let before = manager.session_fence_reads_started.load(Ordering::Acquire);
    manager.hold_session_fence_reads.send_replace(true);
    let deadline = tokio::time::Instant::now() + LIVE_TV_TEST_HANG_GUARD;
    while manager.session_fence_reads_started.load(Ordering::Acquire) == before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the running session never checked its ledger"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let held = manager.session_fence_reads_started.load(Ordering::Acquire);
    (manager, tuner, provisional.capability, session, held)
}

/// Wait for the tuner's lease to renew (the ingest's revision moves on
/// each five-second renewal). A settings or ledger change that lands just
/// after one has five seconds before that lease sees it too and closes the
/// transport, which would end the session for the lease's reason first.
async fn await_ingest_renewal(manager: &LiveTvManager) {
    let ingest_revision = || async {
        manager
            .resource_snapshot(0, "")
            .await
            .expect("ledger snapshot")
            .records
            .into_iter()
            .find_map(|record| match record {
                plurx_core::live_tv_resource::Record::Ingest(ingest) => Some(ingest.revision),
                _ => None,
            })
    };
    let deadline = tokio::time::Instant::now() + LIVE_TV_TEST_HANG_GUARD;
    let renewed_from = ingest_revision().await;
    while ingest_revision().await == renewed_from {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the tuner lease never renewed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the session is gone from the registry, at most `within`,
/// and return how it ended.
async fn session_ended_within(
    manager: &LiveTvManager,
    capability: &str,
    session: &LiveTvSession,
    within: Duration,
    why: &str,
) -> Option<LiveTvError> {
    let deadline = tokio::time::Instant::now() + within;
    while manager
        .registry
        .lock()
        .expect("registry")
        .sessions
        .contains_key(capability)
    {
        assert!(tokio::time::Instant::now() < deadline, "{why}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    session.state.lock().expect("state").terminal_error.clone()
}

/// Plan L-03: a session's ledger read that does not return — a consistent
/// read held across a slow leader change — must not stop its publication.
/// Awaited on the tick, as it used to be, it stopped the scratch scans: the
/// playlist froze while FFmpeg kept producing until the player declared it
/// stuck, and every playlist GET waited on the same read. The ledger read
/// now runs beside the tick, one at a time, and when it finally answers
/// that the cluster no longer authorizes the attempt the session still
/// ends.
#[cfg(unix)]
#[tokio::test]
async fn a_hung_ledger_read_keeps_the_playlist_advancing_and_its_refusal_still_ends_the_session() {
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner, capability, session, held) =
        session_with_a_hung_ledger_read(root.path()).await;
    let sequence_held = session.state.lock().expect("state").media_sequence;
    let playlist_held = served_playlist(&manager, &capability).await;

    // Several fence intervals pass with the read hung.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let sequence = session.state.lock().expect("state").media_sequence;
    assert!(
        sequence >= sequence_held + 3,
        "the publication stopped advancing while the ledger read hung: \
             {sequence_held} -> {sequence}"
    );
    assert_ne!(
        served_playlist(&manager, &capability).await,
        playlist_held,
        "a playlist GET during the hung read served the frozen playlist"
    );
    assert_eq!(
        manager.session_fence_reads_started.load(Ordering::Acquire),
        held,
        "one ledger read in flight from the moment the read hung, never a second"
    );
    assert!(!session.cancel.is_cancelled());

    // The ledger alone stops authorizing the attempt: its start is retired,
    // which no settings observation sees. The hung read, once it returns,
    // is what has to end the session.
    await_ingest_renewal(&manager).await;
    manager
        .resource_retire(session.request.user_id, &session.request.request_id)
        .await
        .expect("the ledger retires the start");
    manager.hold_session_fence_reads.send_replace(false);
    let terminal = session_ended_within(
        &manager,
        &capability,
        &session,
        LIVE_TV_TEST_HANG_GUARD,
        "the hung read's refusal must still end the session",
    )
    .await;
    assert!(
        matches!(&terminal, Some(LiveTvError::CapabilityExpired(message)) if message.contains("no longer authorizes")),
        "{terminal:?}"
    );
}

/// While the ledger read hangs, the rest of the running fence must not
/// hang with it: a settings change the node's observer reads (here another
/// node's save moving the generation) still ends the session on the next
/// fence tick, inside the observation's designed grace and before the
/// ingest lease would have, and without the hung read ever returning.
#[cfg(unix)]
#[tokio::test]
async fn a_settings_change_ends_a_session_whose_ledger_read_is_hung() {
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner, capability, session, held) =
        session_with_a_hung_ledger_read(root.path()).await;
    await_ingest_renewal(&manager).await;
    manager
        .store
        .put_settings(&[(keys::LIVE_TV_CONFIG_GENERATION, "2")])
        .await
        .expect("a settings save elsewhere");
    // One observer interval to read the save and one fence tick to act on
    // it; the lease's next renewal is five seconds out.
    let terminal = session_ended_within(
        &manager,
        &capability,
        &session,
        2 * FENCE_OBSERVATION_INTERVAL + Duration::from_millis(1_500),
        "a generation change the observer read did not end a session whose ledger read hung",
    )
    .await;
    assert_eq!(
        terminal.as_ref().map(LiveTvError::code),
        Some("settings_conflict"),
        "{terminal:?}"
    );
    assert_eq!(
        manager.session_fence_reads_started.load(Ordering::Acquire),
        held,
        "the session ended on the observation, not on a second ledger read"
    );
    assert!(*manager.hold_session_fence_reads.borrow());
}

#[test]
fn native_live_upload_requires_exact_sdr_source_facts() {
    let system = SystemInfo {
        ffmpeg: "/task/ffmpeg".into(),
        ..SystemInfo::default()
    };
    let mut delivery = test_encode_delivery(720);
    delivery.source = LiveSourceFacts {
        video_codec: Some("h264".into()),
        width: Some(1920),
        height: Some(1080),
        bit_depth: Some(8),
        pixel_format: Some("yuv420p".into()),
        field_order: Some("progressive".into()),
        frame_rate: Some(crate::live_tv_delivery::LiveRational {
            num: 30000,
            den: 1001,
        }),
        sample_aspect_ratio: Some("1:1".into()),
        rotation: Some(0),
        color_range: Some("tv".into()),
        color_primaries: Some("bt709".into()),
        color_transfer: Some("bt709".into()),
        color_space: Some("bt709".into()),
        hdr: Some("sdr".into()),
        ..LiveSourceFacts::default()
    };
    delivery.deinterlace_output = None;
    let plan = LiveTvTranscodePlan::new(&system, delivery, Some(Encoder::VideoToolbox), None)
        .expect("valid isolated live upload fixture");
    assert_eq!(
        live_macos_upload_graph(&plan),
        Some(MacosProcessingGraph::LiveSdrUploadScale)
    );
    for change in 0..7 {
        let mut rejected = plan.clone();
        match change {
            0 => rejected.delivery.source.rotation = None,
            1 => rejected.delivery.source.color_range = None,
            2 => rejected.delivery.source.sample_aspect_ratio = Some("4:3".into()),
            3 => rejected.delivery.source.color_transfer = Some("smpte2084".into()),
            4 => rejected.delivery.source.width = Some(1919),
            5 => rejected.delivery.source.field_order = Some("unknown".into()),
            _ => rejected.encoder = Some(Encoder::Software),
        }
        assert_eq!(live_macos_upload_graph(&rejected), None, "case {change}");
    }
    let mut interlaced = plan.clone();
    interlaced.delivery.source.field_order = Some("bb".into());
    interlaced.delivery.deinterlace_output = Some(LiveDeinterlaceOutput::Field);
    assert_eq!(
        live_macos_upload_graph(&interlaced),
        Some(MacosProcessingGraph::LiveSdrUploadBwdifField)
    );
    interlaced.delivery.source.video_codec = Some("hevc".into());
    assert_eq!(live_macos_upload_graph(&interlaced), None);
}

#[cfg(unix)]
#[tokio::test]
async fn changed_live_upload_executable_is_rejected_before_launch() {
    use std::os::unix::fs::PermissionsExt;
    let root = crate::test_tempdir().expect("valid isolated live upload fixture");
    let executable_path = root.path().join("ffmpeg");
    std::fs::write(&executable_path, "#!/bin/sh\nexit 0\n")
        .expect("valid isolated live upload fixture");
    std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))
        .expect("valid isolated live upload fixture");
    let executable = crate::ffmpeg::EncodedExecutable::capture_program(
        executable_path
            .to_str()
            .expect("valid isolated live upload fixture"),
    )
    .await
    .expect("valid isolated live upload fixture");
    let identity = MacosProcessingIdentity::new(
        executable.digest.clone(),
        "1".repeat(64),
        "2".repeat(64),
        "3".repeat(64),
        "test-build".into(),
        "arm64".into(),
        "test-hardware".into(),
    )
    .expect("valid isolated live upload fixture");
    let system = SystemInfo {
        ffmpeg: "/unrelated/configured/ffmpeg".into(),
        ..SystemInfo::default()
    };
    let mut plan = LiveTvTranscodePlan::new(
        &system,
        test_encode_delivery(720),
        Some(Encoder::VideoToolbox),
        None,
    )
    .expect("valid isolated live upload fixture");
    plan.macos_upload = Some(LiveMacosUpload {
        graph: MacosProcessingGraph::LiveSdrUploadScale,
        identity,
        executable,
    });
    plan.verify_macos_upload()
        .await
        .expect("valid isolated live upload fixture");
    let command = live_ffmpeg_command(&system, &plan, root.path())
        .expect("valid isolated live upload fixture");
    assert_eq!(
        command.as_std().get_program(),
        std::fs::canonicalize(&executable_path)
            .expect("valid isolated live upload fixture")
            .as_os_str()
    );
    let args = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(args.windows(2).any(|pair| pair == ["-hwaccel", "none"]));
    assert!(args
        .iter()
        .any(|arg| arg.contains("hwupload") && arg.contains("scale_vt")));
    std::fs::write(&executable_path, "#!/bin/sh\nexit 17\n")
        .expect("valid isolated live upload fixture");
    assert!(plan.verify_macos_upload().await.is_err());
}

#[test]
fn native_source_reconfiguration_uses_existing_format_change_owner() {
    assert!(live_source_parameters_changed(
        "reconfiguring filter graph because video parameters changed to nv12(tv, bt709), 1920x1080"
    ));
    assert!(live_source_parameters_changed("new audio stream"));
    assert!(!live_source_parameters_changed(
        "decoding requested, but no decoder found for: ac4"
    ));
    assert!(!live_source_parameters_changed(
        "could not reconfigure filter graph"
    ));
}

#[test]
fn source_rotation_probe_does_not_turn_malformed_metadata_into_upright_video() {
    assert_eq!(live_probe_rotation(&serde_json::json!({})), Some(0));
    assert_eq!(
        live_probe_rotation(
            &serde_json::json!({"side_data_list": [{"side_data_type": "Display Matrix", "rotation": 90}]})
        ),
        Some(90)
    );
    assert_eq!(
        live_probe_rotation(
            &serde_json::json!({"side_data_list": [{"side_data_type": "Display Matrix"}]})
        ),
        None
    );
    assert_eq!(
        live_probe_rotation(&serde_json::json!({"tags": {"rotate": "invalid"}})),
        None
    );
}

/// Plan L-02 §5.3: warm facts are exactly this generation, device and
/// channel, and only until their expiry.
#[tokio::test(start_paused = true)]
async fn an_expired_or_foreign_generation_entry_is_cold() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let key = SourceFormatKey {
        generation: 3,
        device_id: "10ABCDEF".into(),
        channel_id: "7.1".into(),
    };
    let facts = LiveSourceFacts {
        video_codec: Some("mpeg2video".into()),
        height: Some(480),
        ..LiveSourceFacts::default()
    };
    manager.record_source_facts(key.clone(), "7.1", facts.clone());
    assert_eq!(manager.warm_source_facts(&key), Some(facts));
    for foreign in [
        SourceFormatKey {
            generation: 4,
            ..key.clone()
        },
        SourceFormatKey {
            device_id: "10FEDCBA".into(),
            ..key.clone()
        },
        SourceFormatKey {
            channel_id: "7.2".into(),
            ..key.clone()
        },
    ] {
        assert!(manager.warm_source_facts(&foreign).is_none(), "{foreign:?}");
    }
    tokio::time::advance(SOURCE_FORMAT_TTL + Duration::from_secs(1)).await;
    assert!(
        manager.warm_source_facts(&key).is_none(),
        "past its expiry the entry is cold"
    );
    manager.record_source_facts(key.clone(), "7.1", LiveSourceFacts::default());
    manager.forget_source_facts(&key);
    assert!(manager.warm_source_facts(&key).is_none());
}

/// Plan L-02 §5.3: agreement is an identical delivery and an identical
/// FFmpeg command, never a field-by-field guess. Every field the planner
/// or the command reads flips it; a field neither reads does not.
#[test]
fn warm_agreement_is_an_identical_delivery_and_command() {
    let system = SystemInfo {
        ffmpeg: "/usr/bin/ffmpeg".into(),
        ..SystemInfo::default()
    };
    let config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    let policy = live_quality_policy(&config, false);
    let support = live_execution_support(&system);
    let directory = Path::new("/scratch/live-tv-agreement");
    let base = LiveSourceFacts {
        video_codec: Some("mpeg2video".into()),
        width: Some(1920),
        height: Some(1080),
        field_order: Some("tt".into()),
        frame_rate: Some(crate::live_tv_delivery::LiveRational {
            num: 30000,
            den: 1001,
        }),
        audio_codec: Some("ac3".into()),
        audio_sample_rate: Some(48_000),
        audio_channels: Some(6),
        audio_tracks: vec![
            crate::live_tv_delivery::LiveSourceAudioTrack {
                index: 0,
                id: Some(0x31),
                codec: Some("ac3".into()),
                sample_rate: Some(48_000),
                channels: Some(6),
                layout: Some("5.1(side)".into()),
                language: Some("eng".into()),
                described: false,
            },
            crate::live_tv_delivery::LiveSourceAudioTrack {
                index: 1,
                id: Some(0x34),
                codec: Some("ac3".into()),
                sample_rate: Some(48_000),
                channels: Some(2),
                layout: Some("stereo".into()),
                language: Some("spa".into()),
                described: false,
            },
        ],
        ..LiveSourceFacts::default()
    };
    let delivery = crate::live_tv_delivery::resolve_live_delivery(&base, None, &policy, &support)
        .expect("base plan");
    let running = LiveTvTranscodePlan::new(&system, delivery, Some(Encoder::Software), Some(4))
        .expect("running plan");
    let agrees = |probed: &LiveSourceFacts| {
        warm_plan_agrees(
            &system, &running, &base, probed, None, &policy, &support, directory,
        )
        .is_some()
    };
    assert!(agrees(&base), "a tune that probes the same facts agrees");

    type Mutation = Box<dyn Fn(&mut LiveSourceFacts)>;
    let mutations: [(&str, Mutation); 6] = [
        (
            "video codec",
            Box::new(|f| f.video_codec = Some("h264".into())),
        ),
        ("height", Box::new(|f| f.height = Some(720))),
        ("width", Box::new(|f| f.width = Some(1280))),
        (
            "field order",
            Box::new(|f| f.field_order = Some("progressive".into())),
        ),
        ("hdr", Box::new(|f| f.hdr = Some("hdr10".into()))),
        (
            "selected audio stream id",
            Box::new(|f| f.audio_tracks[0].id = Some(0x44)),
        ),
    ];
    for (field, mutate) in mutations {
        let mut probed = base.clone();
        mutate(&mut probed);
        assert!(!agrees(&probed), "a changed {field} must disagree");
    }

    let mut unread = base.clone();
    unread.audio_tracks[1].language = Some("fra".into());
    assert!(
        agrees(&unread),
        "a field neither the planner nor the command reads agrees"
    );
    // The planner reads the audio codec and channel count, but the legacy
    // profile re-encodes every source to stereo AAC: the command is byte
    // for byte the same, so the FFmpeg already running is the right one
    // and it agrees. A field list would have restarted this start.
    let mut same_command = base.clone();
    same_command.audio_codec = Some("eac3".into());
    same_command.audio_channels = Some(2);
    same_command.audio_tracks[0].codec = Some("eac3".into());
    same_command.audio_tracks[0].channels = Some(2);
    same_command.audio_tracks[0].layout = Some("stereo".into());
    assert!(
        agrees(&same_command),
        "an identical command agrees whatever produced it"
    );
}

/// PR #496 review finding 1. A warm opening's fan-out collects the opening
/// sample, and it used to begin only once a consumer attached. A joiner
/// cannot attach before that sample's probe, so if the warm opener ended
/// before attaching — here it is stopped while it still waits for an
/// encoder slot — nothing ever attached, nothing was probed, and the
/// joiner hung to its start deadline and failed with "the tuner sent no
/// data … no signal". The joiner must start once the opener is gone and
/// admission is free, planning from the probe of the opening sample.
#[cfg(unix)]
#[tokio::test]
async fn a_joiner_of_a_warm_opening_starts_when_the_opener_leaves_before_attaching() {
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    let cold = watch_once_cold(&manager).await;
    let out_h = cold
        .delivery
        .as_ref()
        .map(|delivery| delivery.output.height)
        .unwrap_or(480);
    // Hold every software encoder admission, so the warm opener waits for
    // one after it has opened the transport.
    let mut held = Vec::new();
    while let Ok(admission) = manager
        .transcode
        .admit_live_tv(480, "mpeg2video", None, out_h, Duration::ZERO)
        .await
    {
        held.push(admission);
        assert!(held.len() <= 64, "the encoder pool is bounded");
    }
    let before = start_plans(&manager);
    let started = tokio::time::Instant::now();
    let opener = spawn_start(&manager, fixture_request(2));
    admitted_transports(&manager, 1).await;
    let joiner = spawn_start(&manager, fixture_request(3));
    let transports = admitted_transports(&manager, 2).await;
    assert!(
        Arc::ptr_eq(&transports[0], &transports[1]),
        "the second start joined the warm opening"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let opener_capability = manager
        .registry
        .lock()
        .expect("registry")
        .sessions
        .values()
        .find(|session| session.request.user_id == 2)
        .map(|session| session.capability.clone())
        .expect("the opener is registered");
    manager
        .stop_local(&opener_capability)
        .await
        .expect("stop the opener");
    assert!(
        opener.await.expect("opener task").is_err(),
        "the opener ended before it attached"
    );
    drop(held);
    let joined = tokio::time::timeout(Duration::from_secs(25), joiner)
        .await
        .expect("the joiner answers")
        .expect("joiner task");
    assert!(
        joined.is_ok(),
        "the joiner must start once the opener is gone and admission is free, after {:?}: {:?}",
        started.elapsed(),
        joined.err()
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the joiner waited out its start budget: {:?}",
        started.elapsed()
    );
    let after = start_plans(&manager);
    assert_eq!(
        after[1] - before[1],
        1,
        "the joiner planned from the opening sample's probe: {before:?} -> {after:?}"
    );
    manager.shutdown().await.expect("shutdown");
}

/// The recovery history through the real start path: a start is refused
/// with "recovery history is full" while `MAX_TERMINAL_TOMBSTONES`
/// unexpired tombstones fill it, and admitted once they have expired —
/// the start's own `request_session` prunes them in the same lock hold as
/// the check, so nothing else has to (PR #496 review finding 3).
#[cfg(unix)]
#[tokio::test]
async fn a_full_recovery_history_refuses_a_start_until_its_tombstones_expire() {
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner) =
        start_path_fixture(root.path(), &publishing_ffmpeg(root.path(), false), 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    let ended = fixture_request(99);
    {
        let mut registry = manager.registry.lock().expect("registry");
        for index in 0..MAX_TERMINAL_TOMBSTONES {
            registry.terminals.insert(
                format!("ended-{index}"),
                LiveTvTerminalTombstone {
                    error: LiveTvError::CapabilityExpired("ended".into()),
                    request: ended.clone(),
                    expires_at: tokio::time::Instant::now() + TERMINAL_TOMBSTONE_TTL,
                },
            );
        }
    }
    match manager.start_local(fixture_request(1)).await {
        Err(LiveTvError::Capacity(message)) => assert!(
            message.contains("recovery history is full"),
            "refused for the wrong reason: {message}"
        ),
        other => panic!("a full recovery history must refuse the start: {other:?}"),
    }
    {
        let mut registry = manager.registry.lock().expect("registry");
        let lapsed = tokio::time::Instant::now() - Duration::from_millis(1);
        for tombstone in registry.terminals.values_mut() {
            tombstone.expires_at = lapsed;
        }
    }
    watch_once_cold(&manager).await;
    assert!(
        manager
            .registry
            .lock()
            .expect("registry")
            .terminals
            .keys()
            .all(|capability| !capability.starts_with("ended-")),
        "the start pruned the expired tombstones"
    );
    manager.shutdown().await.expect("shutdown");
}

fn fixture_request(user_id: i64) -> LiveTvStartRequest {
    let mut request = test_session(PathBuf::from("/unused"), 1).request.clone();
    request.user_id = user_id;
    request
}

fn spawn_start(
    manager: &Arc<LiveTvManager>,
    request: LiveTvStartRequest,
) -> tokio::task::JoinHandle<Result<LiveTvProvisional, LiveTvError>> {
    let manager = Arc::clone(manager);
    tokio::spawn(async move { manager.start_local(request).await })
}

/// Wait until `count` sessions are registered and each has its transport.
async fn admitted_transports(manager: &LiveTvManager, count: usize) -> Vec<Arc<dvr::DvrTransport>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let transports = {
            let registry = manager.registry.lock().expect("registry");
            registry
                .sessions
                .values()
                .filter_map(|session| session.transport())
                .collect::<Vec<_>>()
        };
        if transports.len() == count {
            return transports;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} admitted starts expected, saw {}",
            transports.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn two_concurrent_starts_on_one_channel_open_one_transport_with_one_get() {
    // Review of #482, finding 4: D2 on the production start path. Two
    // starts for one channel reach admission together (the fixture holds
    // both lineup answers until both are asked). Deciding in one lock hold
    // and inserting in another lets both decide "open": the second insert
    // replaces the first, whose worker still holds a tuner GET that
    // nothing counts or can close.
    let root = crate::test_tempdir().expect("root");
    let (manager, tuner) = start_path_fixture(root.path(), "exec /bin/cat >/dev/null", 2).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");

    let first = spawn_start(&manager, fixture_request(1));
    let second = spawn_start(&manager, fixture_request(2));
    let transports = admitted_transports(&manager, 2).await;
    assert!(
        Arc::ptr_eq(&transports[0], &transports[1]),
        "one start opened the transport and the other joined it"
    );
    assert_eq!(manager.registry.lock().expect("registry").held(), 1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(tuner.gets.load(Ordering::SeqCst), 1, "one tuner GET");

    manager.shutdown().await.expect("shutdown");
    let _ = first.await;
    let _ = second.await;
    assert_eq!(tuner.gets.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_probe_ends_a_viewers_only_transport_and_the_next_start_probes_again() {
    // Review of #482, finding 1, the transport no recording holds: D1 as
    // designed. Its probe failing ends it with the probe's error, so the
    // viewer who joined while it was probing fails promptly with that
    // error instead of waiting out its start deadline for a re-probe the
    // unattached transport would never take, and the next start opens a
    // fresh GET and probes again.
    let root = crate::test_tempdir().expect("root");
    let (manager, tuner) = start_path_fixture(root.path(), "exec /bin/cat >/dev/null", 2).await;
    std::fs::write(root.path().join("probe.slow"), b"").expect("slow probe");

    let started = tokio::time::Instant::now();
    let first = spawn_start(&manager, fixture_request(1));
    let second = spawn_start(&manager, fixture_request(2));
    let transports = admitted_transports(&manager, 2).await;
    assert!(Arc::ptr_eq(&transports[0], &transports[1]));
    for start in [first, second] {
        let result = tokio::time::timeout(Duration::from_secs(8), start)
            .await
            .expect("both starts answer with the probe's failure")
            .expect("start task");
        assert!(
            matches!(result, Err(LiveTvError::InvalidResponse(_))),
            "{result:?}"
        );
    }
    assert!(started.elapsed() < STARTUP_TIMEOUT);
    let closed = tokio::time::Instant::now() + Duration::from_secs(3);
    while manager.registry.lock().expect("registry").held() != 0 {
        assert!(
            tokio::time::Instant::now() < closed,
            "the failed transport closes"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    std::fs::remove_file(root.path().join("probe.slow")).expect("fast probe");
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");
    let third = spawn_start(&manager, fixture_request(3));
    let planned = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let delivery = {
            let registry = manager.registry.lock().expect("registry");
            registry
                .sessions
                .values()
                .next()
                .and_then(|session| session.delivery())
        };
        if delivery.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < planned,
            "the next start plans"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        tuner.gets.load(Ordering::SeqCst),
        2,
        "a fresh GET, probed again"
    );
    manager.shutdown().await.expect("shutdown");
    let _ = third.await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_viewer_whose_input_contradicts_its_plan_ends_and_marks_the_facts_stale() {
    // Review of #482, finding 2. The plan (height, deinterlace, encode
    // admission) was made from 480-line facts; the input FFmpeg opened
    // is 1080 lines. That is the joiner who planned from a recording's
    // opening probe after the broadcast switched. The viewer ends with
    // `source_format_changed` and the transport's facts are stale, so the
    // retry re-probes.
    let root = crate::test_tempdir().expect("root");
    let ffmpeg = r#"printf 'Input #0, mpegts, from pipe:0:\n  Stream #0:0[0x31]: Video: mpeg2video (Main), yuv420p(tv, top first), 1920x1080 [SAR 1:1 DAR 16:9], 29.97 fps\n  Stream #0:1[0x34]: Audio: ac3, 48000 Hz, stereo, fltp, 192 kb/s\nStream mapping:\n' >&2
exec /bin/cat >/dev/null"#;
    let (manager, _tuner) = start_path_fixture(root.path(), ffmpeg, 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");

    let start = spawn_start(&manager, fixture_request(1));
    let transport = admitted_transports(&manager, 1).await.remove(0);
    let result = tokio::time::timeout(Duration::from_secs(8), start)
        .await
        .expect("the start answers once the input is seen")
        .expect("start task");
    assert!(
        matches!(result, Err(LiveTvError::SourceFormatChanged(_))),
        "{result:?}"
    );
    assert!(transport.source_stale());
    manager.shutdown().await.expect("shutdown");
}

#[test]
fn only_a_stated_difference_in_size_or_channels_contradicts_a_plan() {
    let planned = LiveSourceFacts {
        width: Some(720),
        height: Some(480),
        audio_channels: Some(2),
        field_order: Some("tt".into()),
        ..LiveSourceFacts::default()
    };
    let input = |width, height, channels, scan: Option<&str>| LiveTvSourceFormat {
        video_width: width,
        video_height: height,
        scan: scan.map(str::to_owned),
        audio_channels: channels,
        audio_layout: None,
        observed_at: 0,
    };
    assert!(!input_contradicts_plan(
        &planned,
        &input(Some(720), Some(480), Some(2), Some("interlaced"))
    ));
    assert!(!input_contradicts_plan(
        &planned,
        &input(None, None, None, Some("progressive"))
    ));
    assert!(input_contradicts_plan(
        &planned,
        &input(Some(1920), Some(1080), Some(2), None)
    ));
    assert!(input_contradicts_plan(
        &planned,
        &input(Some(720), Some(480), Some(6), None)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn an_evicted_viewer_ends_on_the_next_tick_with_the_eviction_reason() {
    // Review of #482, finding 5 (plan §3.2, §5.1). This FFmpeg never
    // reads its stdin: the pump blocks on its write and the viewer's
    // queue fills, so the transport evicts it. The session must end on
    // its next tick saying it stopped consuming — not `TUNER_READ_TIMEOUT`
    // later with "FFmpeg input stalled", holding its FFmpeg and encode
    // permit meanwhile.
    let root = crate::test_tempdir().expect("root");
    let (manager, _tuner) = start_path_fixture(root.path(), "exec /bin/sleep 60", 1).await;
    std::fs::write(root.path().join("probe.json"), PROBED_480).expect("probe answer");

    let started = tokio::time::Instant::now();
    let start = spawn_start(&manager, fixture_request(1));
    let result = tokio::time::timeout(Duration::from_secs(10), start)
        .await
        .expect("the evicted start answers well before the write timeout")
        .expect("start task");
    match result {
        Err(LiveTvError::StreamFailed(message)) => {
            assert!(message.contains("stopped consuming"), "{message}");
        }
        other => panic!("expected the eviction, got {other:?}"),
    }
    assert!(started.elapsed() < TUNER_READ_TIMEOUT / 2);
    manager.shutdown().await.expect("shutdown");
}

#[test]
fn live_filter_deinterlaces_interlaced_frames_only() {
    let filter = live_video_filter(
        Encoder::Software,
        1080,
        720,
        Some(LiveDeinterlaceOutput::Field),
    );
    assert!(filter.contains("bwdif=mode=send_field:parity=auto:deint=interlaced"));
    assert!(filter.ends_with("scale=-2:720,format=yuv420p"));
    let frame = live_video_filter(
        Encoder::Software,
        1080,
        720,
        Some(LiveDeinterlaceOutput::Frame),
    );
    assert!(frame.contains("bwdif=mode=send_frame:parity=auto:deint=interlaced"));
}

#[test]
fn live_bitrate_uses_reported_output_cadence_and_respects_the_cap() {
    let mut plan = test_encode_delivery(1080);
    plan.output.frame_rate = Some(crate::live_tv_delivery::LiveRational {
        num: 60_000,
        den: 1_001,
    });
    assert_eq!(live_video_bitrate_kbps(&plan), 15_984);
    plan.output.frame_rate = Some(crate::live_tv_delivery::LiveRational {
        num: 30_000,
        den: 1_001,
    });
    assert_eq!(live_video_bitrate_kbps(&plan), 7_992);
    plan.max_bitrate_bps = Some(6_000_000);
    assert_eq!(live_video_bitrate_kbps(&plan), 6_000);
}

#[tokio::test]
async fn live_tv_unpolled_segment_releases_queue_and_permit_at_deadline() {
    let root = crate::test_tempdir().expect("response root");
    let path = root.path().join("segment-000001.ts");
    tokio::fs::write(&path, vec![7; LOCAL_RESOURCE_CHUNK_BYTES * 8])
        .await
        .expect("segment");
    let file = open_bounded_segment(&path, (LOCAL_RESOURCE_CHUNK_BYTES * 8) as u64)
        .await
        .expect("bounded file");
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&slots)
        .try_acquire_owned()
        .expect("response slot");
    let parent = CancellationToken::new();
    tokio::time::pause();
    let body = bounded_segment_body(
        file,
        (LOCAL_RESOURCE_CHUNK_BYTES * 8) as u64,
        parent.clone(),
        permit,
    );
    tokio::task::yield_now().await;
    assert_eq!(slots.available_permits(), 0);
    tokio::time::advance(LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT + Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        slots.available_permits(),
        1,
        "unpolled Body must not retain a slot"
    );
    assert!(
        !parent.is_cancelled(),
        "one response timeout must not stop the session"
    );
    assert!(axum::body::to_bytes(body, MAX_SEGMENT_BYTES as usize)
        .await
        .is_err());
}

#[tokio::test]
async fn live_tv_dropped_segment_releases_response_without_stopping_session() {
    let root = crate::test_tempdir().expect("response root");
    let path = root.path().join("segment-000001.ts");
    tokio::fs::write(&path, b"segment").await.expect("segment");
    let file = open_bounded_segment(&path, 7).await.expect("bounded file");
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&slots)
        .try_acquire_owned()
        .expect("response slot");
    let parent = CancellationToken::new();
    drop(bounded_segment_body(file, 7, parent.clone(), permit));
    tokio::task::yield_now().await;
    assert_eq!(slots.available_permits(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn live_tv_startup_cancels_only_when_final_waiter_disappears() {
    let root = crate::test_tempdir().expect("waiter root");
    let session = test_session(root.path().join("session"), 1);
    let first = tokio::spawn(wait_for_startup(Arc::clone(&session), false));
    let second = tokio::spawn(wait_for_startup(Arc::clone(&session), true));
    tokio::task::yield_now().await;
    assert_eq!(session.startup_waiters.load(Ordering::Acquire), 2);
    first.abort();
    let _ = first.await;
    assert!(!session.cancel.is_cancelled());
    second.abort();
    let _ = second.await;
    assert!(session.cancel.is_cancelled());
}

#[tokio::test]
async fn live_tv_shutdown_fences_a_real_start_paused_before_insertion() {
    {
        let root = crate::test_tempdir().expect("start race root");
        let mut manager = test_manager(root.path());
        seed_test_config(&manager).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        Arc::get_mut(&mut manager).expect("unique manager").client = Ok(reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{address}")).expect("fixture proxy"))
            .build()
            .expect("fixture client"));
        let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let fixture = tokio::spawn(async move {
            let (mut discover, _) = listener.accept().await.expect("discover request");
            let mut request = [0; 2048];
            assert!(discover.read(&mut request).await.expect("discover headers") > 0);
            let document = r#"{"FriendlyName":"Fixture","ModelNumber":"HDFX-4K","FirmwareVersion":"fixture","DeviceID":"10ABCDEF","TunerCount":4}"#;
            discover.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes()).await.expect("discover response");
            drop(discover);
            let (mut lineup, _) = listener.accept().await.expect("lineup request");
            assert!(lineup.read(&mut request).await.expect("lineup headers") > 0);
            arrived_tx.send(()).expect("lineup paused");
            release_rx.await.expect("resume lineup");
            let document = r#"[{"GuideNumber":"7.1","GuideName":"Test","URL":"http://ignored.local:5004/auto/v7.1"}]"#;
            lineup.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes()).await.expect("lineup response");
            drop(lineup);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "a fenced delayed start must not open a tuner GET"
            );
        });
        let request = test_session(root.path().join("unused"), 1).request.clone();
        let starting_manager = Arc::clone(&manager);
        let starting = tokio::spawn(async move { starting_manager.start_local(request).await });
        tokio::time::timeout(Duration::from_secs(2), arrived_rx)
            .await
            .expect("lineup arrival deadline")
            .expect("lineup arrival");
        assert_eq!(manager.shutdown().await.expect("empty shutdown"), 0);
        release_tx.send(()).expect("release lineup");
        assert!(matches!(
            starting.await.expect("start task"),
            Err(LiveTvError::Conflict(_))
        ));
        assert!(manager.activities().is_empty());
        assert_eq!(manager.metrics.starts_created.load(Ordering::Acquire), 0);
        fixture.await.expect("fixture task");
    }
}

#[tokio::test]
async fn live_tv_real_video_only_source_cannot_publish_the_aac_profile() {
    plurx_core::testfixtures::require_ffmpeg();
    let root = crate::test_tempdir().expect("audio-required root");
    let ffmpeg = plurx_core::testfixtures::ffmpeg();
    let source = tokio::process::Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=25",
            "-t",
            "0.5",
            "-an",
            "-c:v",
            "mpeg2video",
            "-f",
            "mpegts",
            "pipe:1",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .expect("generate video-only MPEG-TS");
    assert!(
        source.status.success(),
        "{}",
        String::from_utf8_lossy(&source.stderr)
    );
    let system = SystemInfo {
        ffmpeg,
        ..SystemInfo::default()
    };
    let plan = LiveTvTranscodePlan::new(
        &system,
        test_encode_delivery(720),
        Some(Encoder::Software),
        Some(2),
    )
    .expect("prepare live-TV plan");
    let (mut child, _child_job) =
        spawn_live_ffmpeg(&system, &plan, root.path()).expect("start exact production graph");
    let detected = Arc::new(AtomicBool::new(false));
    let stderr = tokio::spawn(capture_live_stderr(
        child.stderr.take().expect("stderr"),
        Arc::clone(&detected),
        Arc::new(StdMutex::new(None)),
        Arc::new(StdMutex::new(None)),
        Arc::new(AtomicI64::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(StdMutex::new(None)),
    ));
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(&source.stdout)
        .await
        .expect("one video-only source");
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("FFmpeg exit deadline")
        .expect("FFmpeg exit");
    stderr.await.expect("bounded stderr collection");
    assert!(
        !status.success(),
        "the H.264/AAC profile must require audio"
    );
    assert!(
        !root.path().join("index.m3u8").exists(),
        "no successful HLS publication without audio"
    );
    assert!(matches!(
        classify_live_source_error(
            Err(LiveTvError::StreamFailed("child exited".into())),
            detected.load(Ordering::Acquire),
            None,
        ),
        Err(LiveTvError::CodecUnsupported(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn a_fast_source_change_exit_is_classified_after_the_owned_stderr_reader_settles() {
    let root = crate::test_tempdir().expect("source-change root");
    let session = test_session(root.path().join("session"), 1);
    tokio::fs::create_dir_all(&session.directory)
        .await
        .expect("scratch");
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "exit 178"]);
    let (mut child, job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::realtime("Live source-change test child"),
    )
    .expect("child");
    assert!(!child.wait().await.expect("already exited").success());
    let (release, waiting) = tokio::sync::oneshot::channel();
    let captured = Arc::clone(&session);
    let stderr = tokio::spawn(async move {
        waiting.await.expect("release pending stderr");
        let initial =
            std::io::Cursor::new(b"Input #0, mpegts, from pipe:0:\nStream mapping:\n".to_vec());
        let changed = std::io::Cursor::new(b"Reconfiguring filter graph because video parameters changed to yuv420p(tv, bt709), 416x234\n".to_vec());
        capture_live_stderr(
            tokio::io::AsyncReadExt::chain(initial, changed),
            Arc::clone(&captured.decoder_unavailable),
            Arc::clone(&captured.encoder_diagnostic),
            Arc::clone(&captured.source_format),
            Arc::clone(&captured.source_format_expires_at),
            Arc::clone(&captured.source_format_changed),
            Arc::new(StdMutex::new(None)),
        )
        .await;
    });
    *session.process.lock().await = Some(LiveTvProcess {
        child,
        sink: None,
        _job: job,
        _admission: None,
        stderr: Some(stderr),
    });
    assert!(!session.source_format_changed.load(Ordering::Acquire));
    let cleanup = cleanup_session(&session, false);
    tokio::pin!(cleanup);
    tokio::select! {
        biased;
        result = &mut cleanup => panic!("cleanup returned before stderr settled: {result:?}"),
        () = tokio::task::yield_now() => {}
    }
    release.send(()).expect("reader is owned by cleanup");
    cleanup.await.expect("existing bounded cleanup");
    assert!(session.process.lock().await.is_none());
    assert!(matches!(
        classify_live_terminal_error(
            Err(LiveTvError::StreamFailed("child exited with178".into())),
            false,
            None,
            session.source_format_changed.load(Ordering::Acquire),
        ),
        Err(LiveTvError::SourceFormatChanged(_))
    ));
}

#[test]
fn confirmed_source_change_preserves_classified_decoder_encoder_and_owner_errors() {
    assert!(matches!(
        classify_live_terminal_error(
            Err(LiveTvError::StreamFailed("exit".into())),
            true,
            None,
            true,
        ),
        Err(LiveTvError::CodecUnsupported(_))
    ));
    let encoded = classify_live_terminal_error(
        Err(LiveTvError::StreamFailed("exit".into())),
        false,
        Some("known encoder cause"),
        true,
    )
    .expect_err("encoder cause");
    assert_eq!(encoded.code(), "stream_failed");
    assert!(encoded.to_string().contains("known encoder cause"));
    for error in [
        LiveTvError::OwnerUnavailable("fenced".into()),
        LiveTvError::CapabilityExpired("stopped".into()),
        LiveTvError::StartupTimeout("deadline".into()),
    ] {
        let expected = error.code();
        assert_eq!(
            classify_live_terminal_error(Err(error), false, None, true)
                .expect_err("retain owner reason")
                .code(),
            expected
        );
    }
    assert!(classify_live_terminal_error(Ok(()), false, None, true).is_ok());
    assert_eq!(
        classify_live_terminal_error(
            Err(LiveTvError::StreamFailed("ordinary EOF".into())),
            false,
            None,
            false,
        )
        .expect_err("unchanged EOF")
        .code(),
        "stream_failed"
    );
}

#[tokio::test]
async fn live_caption_stderr_fact_enriches_stream_failure_without_claiming_decoder_failure() {
    for variant in ["type", "size"] {
        for split in [false, true] {
            let (mut reader, mut writer) = tokio::io::simplex(4096);
            let decoder = Arc::new(AtomicBool::new(false));
            let diagnostic = Arc::new(StdMutex::new(None));
            let changed = Arc::new(AtomicBool::new(false));
            let mut bytes = b"Input #0, http://secret.invalid/token\nStream mapping:\n".to_vec();
            if split {
                bytes.resize(1010, b'x');
            }
            bytes.extend_from_slice(
                format!("Unexpected end of SEI NAL Unit parsing {variant}.\n").as_bytes(),
            );
            // Repetition and later unknown errors must not duplicate or
            // replace the first recognized static cause.
            bytes.extend_from_slice(
                b"Unexpected end of SEI NAL Unit parsing size.\nprivate-path: unknown failure\n",
            );
            let feed = async {
                writer.write_all(&bytes).await.expect("diagnostic input");
                writer.shutdown().await.expect("stderr EOF");
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(
                    feed,
                    capture_live_stderr(
                        &mut reader,
                        Arc::clone(&decoder),
                        Arc::clone(&diagnostic),
                        Arc::new(StdMutex::new(None)),
                        Arc::new(AtomicI64::new(0)),
                        changed,
                        Arc::new(StdMutex::new(None)),
                    )
                );
            })
            .await
            .expect("bounded stderr collection");
            assert!(
                !decoder.load(Ordering::Acquire),
                "an encoder failure is not missing decode support"
            );
            let fact = *diagnostic.lock().expect("diagnostic lock");
            assert_eq!(
                fact,
                Some("VideoToolbox failed while inserting A/53 captions into H.264 SEI data")
            );
            let error = classify_live_source_error(
                Err(LiveTvError::StreamFailed(
                    "live-TV FFmpeg exited with 183".into(),
                )),
                decoder.load(Ordering::Acquire),
                fact,
            )
            .expect_err("stream failure");
            assert_eq!(error.code(), "stream_failed");
            let message = error.to_string();
            assert_eq!(message.matches("A/53").count(), 1);
            assert!(!message.contains("secret.invalid"));
            assert!(!message.contains("private-path"));
            assert!(classify_live_source_error(Ok(()), false, fact).is_ok());
            assert!(matches!(
                classify_live_source_error(
                    Err(LiveTvError::CapabilityExpired("idle".into())),
                    false,
                    fact
                ),
                Err(LiveTvError::CapabilityExpired(_))
            ));
        }
    }
}

#[tokio::test]
async fn live_caption_stderr_ignores_unrelated_errors_and_preserves_the_first_fact() {
    for initial in [None, Some("already recorded encoder cause")] {
        let (mut reader, mut writer) = tokio::io::simplex(128);
        let decoder = Arc::new(AtomicBool::new(false));
        let diagnostic = Arc::new(StdMutex::new(initial));
        let bytes: &[u8] = if initial.is_some() {
            b"Unexpected end of SEI NAL Unit parsing type."
        } else {
            b"Input #0, mpegts\nStream mapping:\nError opening output: no space left on device"
        };
        let feed = async {
            writer.write_all(bytes).await.expect("stderr");
            writer.shutdown().await.expect("stderr EOF");
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                feed,
                capture_live_stderr(
                    &mut reader,
                    Arc::clone(&decoder),
                    Arc::clone(&diagnostic),
                    Arc::new(StdMutex::new(None)),
                    Arc::new(AtomicI64::new(0)),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(StdMutex::new(None)),
                )
            );
        })
        .await
        .expect("bounded stderr collection");
        assert_eq!(*diagnostic.lock().expect("diagnostic lock"), initial);
        assert!(!decoder.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn live_tv_decoder_diagnostic_capture_is_bounded_and_specific() {
    for (diagnostic, unsupported) in [
        ("Decoding requested, but no decoder found for: ac4", true),
        ("Decoder (codec ac4) not found for input stream #0:1", true),
        ("Stream map '0:a:0' matches no streams.", true),
        ("Stream map '' matches no streams.", true),
        ("Error opening output: no space left on device", false),
        ("Invalid data found when processing input", false),
    ] {
        let (mut writer, reader) = tokio::io::duplex(128);
        let detected = Arc::new(AtomicBool::new(false));
        let capture = tokio::spawn(capture_live_stderr(
            reader,
            Arc::clone(&detected),
            Arc::new(StdMutex::new(None)),
            Arc::new(StdMutex::new(None)),
            Arc::new(AtomicI64::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(StdMutex::new(None)),
        ));
        writer
            .write_all(&vec![b'x'; 32 * 1024])
            .await
            .expect("long bounded preamble");
        for byte in diagnostic.as_bytes() {
            writer.write_all(&[*byte]).await.expect("split diagnostic");
            tokio::task::yield_now().await;
        }
        drop(writer);
        capture.await.expect("stderr capture");
        assert_eq!(
            detected.load(Ordering::Acquire),
            unsupported,
            "{diagnostic}"
        );
    }
}

#[tokio::test]
async fn initial_stream_discovery_does_not_report_a_format_change() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let changed = Arc::new(AtomicBool::new(false));
    let capture = tokio::spawn(capture_live_stderr(
        reader,
        Arc::new(AtomicBool::new(false)),
        Arc::new(StdMutex::new(None)),
        Arc::new(StdMutex::new(None)),
        Arc::new(AtomicI64::new(0)),
        Arc::clone(&changed),
        Arc::new(StdMutex::new(None)),
    ));
    writer
            .write_all(
                b"Input #0, mpegts\nNew video stream 0:0 at pos:0\nStream mapping:\n  Stream #0:0 -> #0:0\n",
            )
            .await
            .expect("initial descriptor");
    drop(writer);
    capture.await.expect("stderr capture");
    assert!(!changed.load(Ordering::Acquire));
}

#[test]
fn live_tv_source_format_reads_only_the_selected_input_description() {
    let interlaced = br#"
Input #0, mpegts, from 'pipe:0':
  Stream #0:0[0x31]: Video: mpeg2video (Main), yuv420p(tv, top first), 1920x1080 [SAR 1:1 DAR 16:9], 29.97 fps
  Stream #0:1[0x34]: Audio: ac3, 48000 Hz, stereo, fltp, 192 kb/s
Stream mapping:
  Stream #0:0 -> #0:0 (mpeg2video -> h264)
Output #0, hls, to 'index.m3u8':
  Stream #0:0: Video: h264, 3840x2160p
"#;
    let observed = parse_live_source_format(interlaced, 1_788_998_400).expect("input source facts");
    assert_eq!(observed.video_width, Some(1920));
    assert_eq!(observed.video_height, Some(1080));
    assert_eq!(observed.scan.as_deref(), Some("interlaced"));
    assert_eq!(observed.audio_channels, Some(2));
    assert_eq!(observed.audio_layout.as_deref(), Some("stereo"));

    let ultra_hd = br#"
Input #0, mpegts, from 'pipe:0':
  Stream #0:0: Video: hevc (Main 10), yuv420p10le(progressive), 3840x2160p, 59.94 fps
  Stream #0:1: Audio: eac3, 48000 Hz, 5.1(side), fltp
Output #0, hls, to 'index.m3u8':
  Stream #0:0: Video: h264, 1280x720
"#;
    let observed = parse_live_source_format(ultra_hd, 1_788_998_401)
        .expect("hardware-path input source facts");
    assert_eq!(observed.video_width, Some(3840));
    assert_eq!(observed.video_height, Some(2160));
    assert_eq!(observed.scan.as_deref(), Some("progressive"));
    assert_eq!(observed.audio_channels, Some(6));
    assert_eq!(observed.audio_layout.as_deref(), Some("5.1"));
}

#[test]
fn live_tv_source_format_rejects_bad_optional_fields_independently() {
    let channel: LiveTvChannel = serde_json::from_value(serde_json::json!({
        "id": "2.1",
        "guide_number": "2.1",
        "guide_name": "WXYZ",
        "favorite": false,
        "drm": false,
        "support": "ready",
        "source_format": {
            "video_width": -1,
            "video_height": 2160,
            "scan": "invented",
            "audio_channels": 99,
            "audio_layout": "5.1",
            "observed_at": 1788998400
        }
    }))
    .expect("one invalid optional fact must not reject a channel");
    let observed = channel.source_format.expect("valid observation envelope");
    assert_eq!(observed.video_width, None);
    assert_eq!(observed.video_height, Some(2160));
    assert_eq!(observed.scan, None);
    assert_eq!(observed.audio_channels, None);
    assert_eq!(observed.audio_layout.as_deref(), Some("5.1"));
}

#[test]
fn live_tv_session_never_revives_an_expired_lineup_observation() {
    let format = LiveTvSourceFormat {
        video_width: Some(1920),
        video_height: Some(1080),
        scan: Some("interlaced".into()),
        audio_channels: Some(2),
        audio_layout: Some("stereo".into()),
        observed_at: unix_seconds() - 60,
    };
    let mut session = test_session(PathBuf::from("unused"), 1);
    Arc::get_mut(&mut session)
        .expect("unshared test session")
        .channel
        .source_format = Some(format.clone());

    assert_eq!(session.channel_with_source_format().source_format, None);

    *session.source_format.lock().expect("source format") = Some(format.clone());
    session
        .source_format_expires_at
        .store(unix_seconds() + 60, Ordering::Release);
    assert_eq!(
        session.channel_with_source_format().source_format,
        Some(format)
    );
}

/// Register `session` as a start would, and run its real worker. With no
/// Live TV settings in the store the start fence refuses at once, so the
/// worker goes straight to the cleanup and retirement under test.
async fn run_session_to_its_end(manager: &Arc<LiveTvManager>, session: &Arc<LiveTvSession>) {
    {
        let mut registry = manager.registry.lock().expect("registry");
        registry.requests.insert(
            LiveTvRequestKey::from(&session.request),
            session.capability.clone(),
        );
        registry
            .sessions
            .insert(session.capability.clone(), Arc::clone(session));
    }
    run_live_session(
        Arc::downgrade(manager),
        Arc::clone(session),
        guide_config(session.request.config_generation),
        LiveStartKind::Cold,
    )
    .await;
}

fn orphan_gauges(manager: &LiveTvManager) -> (u64, u64) {
    (
        manager.metrics.orphans[0].load(Ordering::Acquire),
        manager.metrics.orphans[1].load(Ordering::Acquire),
    )
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // Signal 0 probes without delivering anything. A reaped child is gone;
    // an unreaped one would still answer, which is the point.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Plan L-02 §3.4 / §5.4. The kernel has not confirmed FFmpeg's exit by
/// the cleanup deadline. The session must still leave the registry — it
/// used to stay there for ever, counted against the recovery history — but
/// the child, its encoder admission and its directory stay owned, by the
/// orphan record, until they are confirmed gone; and a drain still waits
/// for them.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_cleanup_retires_the_session_and_keeps_the_child_until_reaped() {
    let root = crate::test_tempdir().expect("orphan root");
    let manager = test_manager(root.path());
    let session = test_session(manager.scratch_root.join("live-tv-orphan-child"), 1);
    tokio::fs::create_dir_all(&session.directory)
        .await
        .expect("session scratch");
    tokio::fs::write(session.directory.join("index.m3u8"), b"#EXTM3U\n")
        .await
        .expect("scratch content");
    let idle_threads = manager.transcode.test_software_threads_in_use();
    let admission = manager
        .transcode
        .admit_live_tv(1080, "h264", None, 720, Duration::ZERO)
        .await
        .expect("software Live TV admission");
    let admitted_threads = manager.transcode.test_software_threads_in_use();
    assert!(
        admitted_threads > idle_threads,
        "the fixture admission holds threads"
    );
    let mut command = tokio::process::Command::new("sleep");
    command.arg("30");
    let (child, job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::realtime("Live TV test child"),
    )
    .expect("child");
    let pid = child.id().expect("child pid");
    *session.process.lock().await = Some(LiveTvProcess {
        child,
        sink: None,
        _job: job,
        _admission: Some(admission),
        stderr: None,
    });
    manager.withhold_reaps.store(true, Ordering::Release);

    run_session_to_its_end(&manager, &session).await;

    assert!(
        matches!(session.state.lock().expect("state").cleanup, Some(Err(_))),
        "the cleanup did fail"
    );
    {
        let registry = manager.registry.lock().expect("registry");
        assert!(
            !registry.sessions.contains_key(&session.capability),
            "a failed cleanup must still retire the session"
        );
        assert!(registry.requests.is_empty());
        assert_eq!(registry.held(), 0);
    }
    assert!(manager.activities().is_empty());
    assert_eq!(orphan_gauges(&manager), (1, 0));
    assert!(process_exists(pid), "the child is not abandoned");
    assert_eq!(
        manager.transcode.test_software_threads_in_use(),
        admitted_threads,
        "the encoder admission stays with the unconfirmed child"
    );
    assert_eq!(
        manager.sweep_orphan_scratch().await,
        0,
        "the sweeper must not remove a directory a live child may write into"
    );
    assert!(session.directory.exists());
    assert!(
        manager
            .cancel_and_wait(Vec::new(), OrphanScope::All)
            .await
            .is_err(),
        "a drain acknowledges disappearance, so it waits for the orphan too"
    );

    manager.withhold_reaps.store(false, Ordering::Release);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while orphan_gauges(&manager) != (0, 0) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the orphan was never confirmed gone"
        );
        manager.retry_orphans().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !process_exists(pid),
        "the retry killed and reaped the child"
    );
    assert_eq!(
        manager.transcode.test_software_threads_in_use(),
        idle_threads,
        "the admission is released with the reaped child"
    );
    assert!(!session.directory.exists());
    assert!(!manager
        .scratch_claims
        .lock()
        .expect("claims")
        .contains(&session.directory));
    assert_eq!(
        manager
            .cancel_and_wait(Vec::new(), OrphanScope::All)
            .await
            .expect("drain after reap"),
        0
    );
}

/// The scratch half of the same handoff: the directory could not be
/// removed. The session leaves at once; a drain waits for the directory
/// and acknowledges once it is gone.
#[tokio::test(start_paused = true)]
async fn a_failed_scratch_cleanup_retires_the_session_and_a_drain_waits_for_the_directory() {
    let root = crate::test_tempdir().expect("cleanup root");
    let manager = test_manager(root.path());
    let path = root.path().join("not-a-directory");
    tokio::fs::write(&path, b"cannot remove as directory")
        .await
        .expect("cleanup obstruction");
    let session = test_session(path.clone(), 1);
    run_session_to_its_end(&manager, &session).await;
    assert!(manager.activities().is_empty(), "the session left at once");
    assert_eq!(orphan_gauges(&manager), (0, 1));
    assert!(
        manager
            .cancel_and_wait(Vec::new(), OrphanScope::All)
            .await
            .is_err(),
        "failed physical cleanup must not acknowledge disappearance"
    );
    tokio::fs::remove_file(&path)
        .await
        .expect("clear fixture obstruction");
    assert_eq!(
        manager
            .cancel_and_wait(Vec::new(), OrphanScope::All)
            .await
            .expect("retry cleanup"),
        0
    );
    assert_eq!(orphan_gauges(&manager), (0, 0));
    assert!(
        manager
            .session_for_request(&LiveTvRequestKey::from(&session.request), &session.request)
            .is_err(),
        "a replay finds the tombstone, never a session to start again"
    );
}

/// PR #496 review finding 2. An orphan's directory removal that never
/// returns — `remove_dir_all` on a stalled mount — was awaited while the
/// retry pass held the orphan list. The owner's next pass then held it
/// for as long as the mount stalled, so the next failed cleanup could not
/// hand over (its session stayed registered) and every drain waited for
/// the list far past `SESSION_DRAIN_TIMEOUT`. The removal now runs on its
/// own task: the owner's pass returns, a failed cleanup still retires its
/// session, and a drain that wants the stuck orphan answers at its
/// deadline.
#[tokio::test(start_paused = true)]
async fn a_hung_orphan_removal_holds_up_no_handoff_and_no_drain_past_its_deadline() {
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    manager.hang_orphan_removals.store(true, Ordering::Release);
    let first_path = root.path().join("stuck-first");
    tokio::fs::write(&first_path, b"not a directory")
        .await
        .expect("cleanup obstruction");
    let first = test_session(first_path, 1);
    run_session_to_its_end(&manager, &first).await;
    assert_eq!(orphan_gauges(&manager), (0, 1));

    // The owner's retry loop, as `scratch_sweep_loop` runs it.
    let owner = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            loop {
                manager.retry_orphans().await;
                tokio::time::sleep(ORPHAN_RETRY).await;
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    tokio::time::timeout(Duration::from_secs(1), manager.retry_orphans())
        .await
        .expect("a retry pass does not wait on the filesystem");

    let second_path = root.path().join("stuck-second");
    tokio::fs::write(&second_path, b"not a directory")
        .await
        .expect("cleanup obstruction");
    let second = test_session(second_path, 1);
    tokio::time::timeout(
        Duration::from_secs(30),
        run_session_to_its_end(&manager, &second),
    )
    .await
    .expect("a failed cleanup hands over while another removal hangs");
    assert!(
        !manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .contains_key(&second.capability),
        "the second session is retired"
    );
    assert_eq!(orphan_gauges(&manager), (0, 2));

    let asked = tokio::time::Instant::now();
    let drained = tokio::time::timeout(
        SESSION_DRAIN_TIMEOUT + Duration::from_secs(1),
        manager.cancel_and_wait(Vec::new(), OrphanScope::All),
    )
    .await
    .expect("a drain answers by its own deadline");
    assert!(
        drained.is_err(),
        "the stuck orphans are not confirmed gone, so the drain does not acknowledge"
    );
    assert!(
        asked.elapsed() <= SESSION_DRAIN_TIMEOUT + Duration::from_millis(100),
        "the drain overran its deadline: {:?}",
        asked.elapsed()
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            manager.cancel_and_wait(Vec::new(), OrphanScope::Sessions)
        )
        .await
        .expect("a drain that wants no orphan is not held by one")
        .expect("nothing to drain"),
        0
    );
    owner.abort();
}

/// §5.4: failed cleanups used to stay in the registry for ever, counted in
/// `sessions + terminals`, so 256 of them refused every start. Now each is
/// retired and leaves only a one-minute tombstone like any other end, and
/// the orphan owner is bounded. That a start is refused only while
/// unexpired tombstones fill the history is pinned through the real start
/// path by `a_full_recovery_history_refuses_a_start_until_its_tombstones_expire`.
#[tokio::test(start_paused = true)]
async fn two_hundred_failed_cleanups_do_not_refuse_starts() {
    let root = crate::test_tempdir().expect("cleanup root");
    let manager = test_manager(root.path());
    for index in 0..300 {
        let path = root.path().join(format!("obstruction-{index}"));
        tokio::fs::write(&path, b"not a directory")
            .await
            .expect("cleanup obstruction");
        let session = test_session(path, 1);
        run_session_to_its_end(&manager, &session).await;
    }
    let orphans = manager.orphans.lock().await.len();
    assert_eq!(orphans, ORPHAN_MAX, "scratch-only orphans are bounded");
    assert_eq!(orphan_gauges(&manager), (0, ORPHAN_MAX as u64));
    assert!(
        manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .is_empty(),
        "every failed cleanup retired its session"
    );
}

/// Plan §3.4 lookup text: the code is `capability_expired` (410, retry
/// now) whatever the sentence, and the sentence claims a restart only
/// when the previous process was seen recently enough to have served it.
#[tokio::test]
async fn an_unknown_capability_is_unavailable_unless_a_recent_incarnation_proves_a_restart() {
    fn unknown_capability_answer(manager: &LiveTvManager) -> String {
        let capability = format!(
            "ltv1.{}.{}",
            base64url_encode(b"node-a"),
            uuid::Uuid::new_v4()
        );
        match manager.session(&capability) {
            Err(error @ LiveTvError::CapabilityExpired(_)) => {
                assert_eq!(error.code(), "capability_expired");
                error.to_string()
            }
            Err(other) => panic!("wrong error for an unknown capability: {other:?}"),
            Ok(_) => panic!("an unknown capability was accepted"),
        }
    }
    fn age_incarnation(root: &Path, seconds: i64) {
        let path = root.join("live-tv-guide").join(INCARNATION_FILE);
        let mut persisted: PersistedIncarnation =
            serde_json::from_slice(&std::fs::read(&path).expect("incarnation file"))
                .expect("incarnation json");
        persisted.last_seen_at = unix_seconds() - seconds;
        std::fs::write(&path, serde_json::to_vec(&persisted).expect("json"))
            .expect("aged incarnation");
    }

    let root = crate::test_tempdir().expect("incarnation root");
    let first = test_manager(root.path());
    let answer = unknown_capability_answer(&first);
    assert!(answer.contains("not available"), "{answer}");
    assert!(
        !answer.contains("expired") && !answer.contains("restarted"),
        "{answer}"
    );
    drop(first);

    age_incarnation(root.path(), 20);
    let restarted = test_manager(root.path());
    let answer = unknown_capability_answer(&restarted);
    assert!(answer.contains("restarted"), "{answer}");
    drop(restarted);

    age_incarnation(root.path(), 600);
    let long_after = test_manager(root.path());
    let answer = unknown_capability_answer(&long_after);
    assert!(answer.contains("not available"), "{answer}");
}

#[tokio::test(start_paused = true)]
async fn the_incarnation_is_touched_only_while_something_is_live() {
    let root = crate::test_tempdir().expect("incarnation root");
    let incarnation = OwnerIncarnation::adopt(Some(root.path().join(INCARNATION_FILE)));
    tokio::time::advance(INCARNATION_TOUCH).await;
    assert!(
        !incarnation.touch_due(0),
        "nothing live: nothing to explain later"
    );
    assert!(incarnation.touch_due(1));
    assert!(!incarnation.touch_due(1), "at most once per interval");
    tokio::time::advance(INCARNATION_TOUCH).await;
    assert!(incarnation.touch_due(2));
    let persisted: PersistedIncarnation = serde_json::from_slice(
        &std::fs::read(root.path().join(INCARNATION_FILE)).expect("written at adoption"),
    )
    .expect("incarnation json");
    assert_eq!(persisted.version, INCARNATION_VERSION);
}

#[tokio::test(start_paused = true)]
async fn live_tv_metrics_expire_proofs_and_ignore_older_configuration() {
    let root = crate::test_tempdir().expect("metrics root");
    let manager = test_manager(root.path());
    let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    config.generation = 2;
    config.enabled = true;
    manager.observe_config(&config);
    manager
        .metrics
        .projection
        .lock()
        .expect("projection")
        .device = Some((2, tokio::time::Instant::now(), 8, 2));
    assert!(manager
        .metrics
        .prometheus()
        .contains("plurx_live_tv_device_ready 1\n"));
    let mut older = config.clone();
    older.generation = 1;
    older.enabled = false;
    manager.observe_config(&older);
    assert!(manager
        .metrics
        .prometheus()
        .contains("plurx_live_tv_enabled 1\n"));
    tokio::time::advance(Duration::from_secs(4)).await;
    let expired = manager.metrics.prometheus();
    assert!(expired.contains("plurx_live_tv_enabled NaN\n"));
    assert!(expired.contains("plurx_live_tv_device_ready 0\n"));
    manager.observe_config(&config);
    assert!(manager
        .metrics
        .prometheus()
        .contains("plurx_live_tv_device_ready 1\n"));
    tokio::time::advance(SNAPSHOT_TTL).await;
    manager.observe_config(&config);
    assert!(manager
        .metrics
        .prometheus()
        .contains("plurx_live_tv_device_ready 0\n"));
}

#[test]
fn provisional_lease_starts_when_first_media_is_published() {
    let root = crate::test_temp_path(format!("live-tv-{}", uuid::Uuid::new_v4()));
    let session = test_session(root, 1);
    let published_at = tokio::time::Instant::now();
    let mut state = session.state.lock().expect("state");
    state.activated = false;
    state.provisional_at = Some(published_at);
    assert!(!provisional_expired(
        &state,
        published_at + PROVISIONAL_TIMEOUT - Duration::from_millis(1)
    ));
    assert!(provisional_expired(
        &state,
        published_at + PROVISIONAL_TIMEOUT
    ));
}

#[test]
fn device_address_is_a_private_canonical_literal() {
    assert_eq!(
        parse_device_ipv4("10.42.4.20").expect("private"),
        Some(Ipv4Addr::new(10, 42, 4, 20))
    );
    for refused in [
        "127.0.0.1",
        "169.254.169.254",
        "8.8.8.8",
        "224.0.0.1",
        "255.255.255.255",
        "hdhomerun.local",
        "10.42.004.020",
    ] {
        assert!(parse_device_ipv4(refused).is_err(), "{refused}");
    }
}

#[test]
fn lineup_drops_bad_rows_and_fails_closed_on_duplicates_and_urls() {
    let rows = serde_json::from_str::<Vec<serde_json::Value>>(
        r#"[
              {"GuideNumber":"7.1","GuideName":"WABC","Tags":"favorite","HD":1,
               "VideoCodec":"HEVC","AudioCodec":"AC4"},
              {"GuideNumber":"oops/../","GuideName":"bad"},
              {"GuideNumber":"8.1","GuideName":42},
              "not-an-object",
              {"GuideNumber":"10","GuideName":"Ten","Tags":"DRM"}
            ]"#,
    )
    .expect("lineup");
    let channels = validate_lineup(rows).expect("validated");
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].guide_number, "7.1");
    assert!(channels[0].favorite);
    assert_eq!(channels[0].hd, Some(true));
    assert_eq!(channels[0].video_codec.as_deref(), Some("HEVC"));
    assert_eq!(channels[0].audio_codec.as_deref(), Some("AC4"));
    assert_eq!(channels[1].support, LiveTvChannelSupport::DrmUnsupported);

    let duplicate = serde_json::from_str::<Vec<serde_json::Value>>(
        r#"[
              {"GuideNumber":"7.1","GuideName":"A"},
              {"GuideNumber":"7.1","GuideName":"B"}
            ]"#,
    )
    .expect("duplicate");
    assert!(validate_lineup(duplicate).is_err());

    let hostile = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[{"GuideNumber":"7.1","GuideName":"A","URL":"http://169.254.169.254/latest/meta-data?x=1"}]"#,
        )
        .expect("hostile");
    assert!(validate_lineup(hostile).is_err());
}

#[test]
fn tuner_status_reports_only_the_busy_matching_channels_bounded_signal() {
    let rows = serde_json::from_str::<Vec<serde_json::Value>>(
        r#"[
              {"Resource":"tuner0","VctNumber":"7.1","TargetIP":"",
               "SignalStrengthPercent":99,"SignalQualityPercent":99,"SymbolQualityPercent":99},
              {"Resource":"tuner1","VctNumber":"9.1","TargetIP":"10.42.4.2",
               "SignalStrengthPercent":100,"SignalQualityPercent":100,"SymbolQualityPercent":100},
              {"Resource":"tuner2","VctNumber":"7.1","TargetIP":"172.18.0.2",
               "SignalStrengthPercent":96,"SignalQualityPercent":89,"SymbolQualityPercent":100}
            ]"#,
    )
    .expect("status rows");
    assert_eq!(
        tuner_signal_for_channel(rows, "7.1"),
        Some(LiveTvSignalStatus {
            strength_percent: Some(96),
            quality_percent: Some(89),
            symbol_quality_percent: Some(100),
        })
    );

    let invalid = serde_json::from_str::<Vec<serde_json::Value>>(
        r#"[{"Resource":"tuner0","VctNumber":"7.1","TargetIP":"10.42.4.2",
                  "SignalStrengthPercent":101,"SignalQualityPercent":999}]"#,
    )
    .expect("invalid percentages");
    assert_eq!(tuner_signal_for_channel(invalid, "7.1"), None);
}

/// A 10-bit broadcast has to reach the encoder as 8-bit.
///
/// Found on a real HDHomeRun FLEX 4K: every ATSC 3.0 channel is HEVC Main
/// 10, the live chain was `bwdif,scale` with no pixel-format conversion,
/// and the software encoder pins `-profile:v high`. x264 answers "high
/// profile doesn't support a bit depth of 10" and FFmpeg exits before it
/// publishes anything, so those channels were listed playable and never
/// started.
///
/// The conversion must happen before a hardware upload: VAAPI's suffix
/// already does that, while QSV needs the same nv12 conversion inserted
/// ahead of its qsv-surface upload.
/// The probe budget is a measurement, so it does not get to drift back.
///
/// A byte budget spent on a stream arriving at the broadcaster's rate is a
/// time budget, and the old 8 MiB was about 23 s of wall time on a
/// 2.8 Mbps mux -- longer than the entire startup budget. The first sweep
/// established 512 KiB as the smallest exercised byte bound; the later
/// 1080p sweep paired it with a one-second stream-time bound and published
/// both ATSC 1.0 and AC-4 ATSC 3.0 successfully.
#[test]
fn the_live_probe_budget_is_the_measured_one() {
    let args = live_probe_args();
    assert_eq!(
        args,
        ["-probesize", "524288", "-analyzeduration", "1000000"],
        "the live probe budget was measured, not guessed: {args:?}"
    );
    let probesize: u64 = args[1].parse().expect("probesize is a byte count");
    assert!(
        probesize <= 2 * 1024 * 1024,
        "every byte above the measured 2 MiB is startup latency on a live \
             tuner, not spare detection budget"
    );
    assert!(
        probesize >= 512 * 1024,
        "512 KiB was the smallest value measured; below it nothing was \
             won and detection has less to work with"
    );
}

#[test]
fn live_caption_forwarding_is_disabled_only_for_videotoolbox_encoding() {
    let system = SystemInfo {
        ffmpeg: "/fixture/ffmpeg".into(),
        ..SystemInfo::default()
    };
    for input in [LiveTvFfmpegInput::Tuner, LiveTvFfmpegInput::GraphProbe] {
        for encoder in [
            Encoder::VideoToolbox,
            Encoder::Software,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::Nvenc,
        ] {
            let plan =
                LiveTvTranscodePlan::new(&system, test_encode_delivery(720), Some(encoder), None)
                    .expect("encode plan");
            let command =
                live_ffmpeg_command_for_input(&system, &plan, Path::new("/fixture/live"), input)
                    .expect("encode command");
            let args: Vec<_> = command.as_std().get_args().collect();
            let caption_args: Vec<_> = args.windows(2).filter(|pair| pair[0] == "-a53cc").collect();
            if encoder == Encoder::VideoToolbox {
                assert_eq!(caption_args.len(), 1);
                assert_eq!(caption_args[0][1], "0");
            } else {
                assert!(caption_args.is_empty(), "leave {encoder:?} unchanged");
            }
        }
    }
}

#[test]
fn live_caption_forwarding_leaves_video_copy_routes_unchanged() {
    let system = SystemInfo {
        ffmpeg: "/fixture/ffmpeg".into(),
        ..SystemInfo::default()
    };
    for audio_action in [LiveTrackAction::Copy, LiveTrackAction::Encode] {
        let mut delivery = test_encode_delivery(720);
        delivery.video_action = LiveTrackAction::Copy;
        delivery.audio_action = audio_action;
        let plan = LiveTvTranscodePlan::new(&system, delivery, None, None).expect("copy plan");
        let command =
            live_ffmpeg_command(&system, &plan, Path::new("/fixture/live")).expect("copy command");
        let args: Vec<_> = command.as_std().get_args().collect();
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-c:v" && pair[1] == "copy"));
        assert!(!args.iter().any(|arg| *arg == "-a53cc"));
    }
}

/// Live TV has no pre-body codec facts, so its plan freezes the explicit
/// software-auto input contract instead of reusing a movie cache plan.
#[test]
fn live_tv_software_hls_argument_baseline_is_stable() {
    let system = SystemInfo {
        ffmpeg: "/fixture/ffmpeg".to_owned(),
        ..SystemInfo::default()
    };
    let plan = LiveTvTranscodePlan::new(
        &system,
        test_encode_delivery(720),
        Some(Encoder::Software),
        Some(2),
    )
    .expect("prepare live-TV plan");
    let command = live_ffmpeg_command(&system, &plan, Path::new("/fixture/live"))
        .expect("build live-TV FFmpeg command");
    let allowed_environment = [
        "LC_ALL",
        "PATH",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "LIBVA_DRIVER_NAME",
        "LIBVA_DRIVERS_PATH",
        "VDPAU_DRIVER",
        "CUDA_VISIBLE_DEVICES",
        "NVIDIA_VISIBLE_DEVICES",
        "NVIDIA_DRIVER_CAPABILITIES",
        "CUDA_CACHE_DISABLE",
        "MESA_SHADER_CACHE_DISABLE",
    ];
    assert!(command
        .as_std()
        .get_envs()
        .all(|(name, _)| allowed_environment
            .iter()
            .any(|allowed| name == std::ffi::OsStr::new(allowed))));
    for (key, value) in [
        ("CUDA_CACHE_DISABLE", "1"),
        ("MESA_SHADER_CACHE_DISABLE", "true"),
    ] {
        assert!(
            command.as_std().get_envs().any(|(name, supplied)| {
                name == std::ffi::OsStr::new(key) && supplied == Some(std::ffi::OsStr::new(value))
            }),
            "{key} must preserve the runtime cache contract"
        );
    }
    let actual = command
        .as_std()
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let expected = [
        "-hide_banner",
        "-loglevel",
        "info",
        "-nostdin",
        "-y",
        "-hwaccel",
        "none",
        "-fflags",
        "+genpts+discardcorrupt",
        "-probesize",
        "524288",
        "-analyzeduration",
        "1000000",
        "-i",
        "pipe:0",
        "-map",
        "0:v:0",
        "-map",
        "0:a:0",
        "-sn",
        "-dn",
        "-vf",
        "scale=-2:720,format=yuv420p",
        "-force_key_frames",
        "expr:gte(t,n_forced*1)",
        "-g",
        "120",
        "-keyint_min",
        "1",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-b:v",
        "4000k",
        "-maxrate",
        "6000k",
        "-bufsize",
        "8000k",
        "-profile:v",
        "high",
        "-threads",
        "2",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-af",
        "aformat=channel_layouts=stereo|mono",
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_list_size",
        "24",
        "-hls_delete_threshold",
        "4",
        "-hls_flags",
        "delete_segments+temp_file+independent_segments+omit_endlist",
        "-hls_segment_filename",
        "/fixture/live/segment-%06d.ts",
        "/fixture/live/index.m3u8",
    ];
    assert_eq!(actual, expected);
    assert!(
        actual
            .windows(2)
            .any(|arguments| arguments == ["-hwaccel", "none"]),
        "the live-TV plan must state its software decode contract"
    );
}

/// The cadence must stay uniform after startup so a player attached near
/// the edge never catches a segment-duration jump.
#[test]
fn live_hls_cuts_uniform_one_second_segments_and_keeps_a_24_entry_window() {
    let value_after = |flag: &str| {
        LIVE_HLS_OUTPUT_ARGS
            .windows(2)
            .find_map(|pair| (pair[0] == flag).then_some(pair[1]))
            .unwrap_or_else(|| panic!("missing {flag} from live HLS arguments"))
    };

    assert!(!LIVE_HLS_OUTPUT_ARGS.contains(&"-hls_init_time"));
    assert_eq!(value_after("-hls_time"), "1");
    assert_eq!(value_after("-hls_list_size"), "24");
    assert_eq!(
        value_after("-hls_delete_threshold")
            .parse::<usize>()
            .expect("numeric threshold"),
        HLS_DELETE_THRESHOLD
    );
    assert_eq!(MAX_DELETION_LAG_SEGMENTS, HLS_DELETE_THRESHOLD + 1);

    let system = SystemInfo {
        ffmpeg: "/fixture/ffmpeg".to_owned(),
        ..SystemInfo::default()
    };
    let plan = LiveTvTranscodePlan::new(
        &system,
        test_encode_delivery(720),
        Some(Encoder::Software),
        Some(2),
    )
    .expect("prepare live-TV plan");
    let command = live_ffmpeg_command(&system, &plan, Path::new("/fixture/live"))
        .expect("build live-TV command");
    let environment = command
        .as_std()
        .get_envs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        environment.get(std::ffi::OsStr::new("CUDA_CACHE_DISABLE")),
        Some(&Some(std::ffi::OsStr::new("1")))
    );
    assert_eq!(
        environment.get(std::ffi::OsStr::new("MESA_SHADER_CACHE_DISABLE")),
        Some(&Some(std::ffi::OsStr::new("true")))
    );
    let arguments = command
        .as_std()
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(arguments
        .windows(2)
        .any(|pair| pair == ["-force_key_frames", "expr:gte(t,n_forced*1)"]));
}

#[tokio::test]
async fn the_graph_probe_publishes_the_production_cadence() {
    let temp = crate::test_tempdir().expect("graph probe scratch");
    let system = SystemInfo {
        ffmpeg: "ffmpeg".to_owned(),
        ..SystemInfo::default()
    };
    run_graph_probe(Encoder::Software, &system, 720, temp.path())
        .await
        .expect("production graph probe");
    let manifest = tokio::fs::read_to_string(temp.path().join("index.m3u8"))
        .await
        .expect("graph probe playlist");
    assert!(manifest.contains("#EXT-X-TARGETDURATION:1"));
    assert!(
        manifest
            .lines()
            .filter(|line| line.starts_with("#EXTINF:1."))
            .count()
            >= 3
    );
    assert!(!manifest.contains("#EXT-X-ENDLIST"));
}

#[test]
fn the_live_chain_delivers_eight_bit_to_an_eight_bit_profile() {
    for encoder in [Encoder::Software, Encoder::Nvenc] {
        let filter = live_video_filter(encoder, 1080, 720, None);
        assert!(
            filter.ends_with(",format=yuv420p"),
            "{encoder:?} pins an 8-bit profile and gets whatever the \
                 broadcaster sent: {filter}"
        );
    }
    for encoder in [Encoder::Vaapi, Encoder::Qsv] {
        let filter = live_video_filter(encoder, 1080, 720, None);
        let suffix = encoder
            .filter_suffix()
            .expect("a hardware upload path pins its own format");
        if encoder == Encoder::Qsv {
            assert!(
                filter.ends_with(&format!("format=nv12,{suffix}")),
                "QSV must convert 10-bit ATSC 3.0 to the 8-bit H.264 contract: {filter}"
            );
        } else {
            assert!(
                filter.ends_with(suffix),
                "{encoder:?} already uploads in its own format; a trailing \
                     system-memory conversion would break the upload: {filter}"
            );
        }
        assert!(
            !filter.ends_with(",format=yuv420p"),
            "{encoder:?} must not get the software conversion: {filter}"
        );
    }
}

/// A slow channel and an absent one get different budgets.
///
/// This is the exact defect a real HDHomeRun found. An ATSC 3.0 channel
/// published its first segment at 18.1 s while the ATSC 1.0 channel on the
/// same device and host took 7.04 s, so a flat 15 s budget refused a
/// channel the lineup had already called playable. Raising the constant for
/// everyone would have been the wrong fix: two ATSC 3.0 channels on that
/// antenna deliver no bytes at all, and those should still fail promptly.
///
/// Every row below is a decision the unfixed implementation got wrong or
/// right for the wrong reason: it compared `now` to one deadline and never
/// looked at the tuner at all.
#[test]
fn a_tuner_that_is_feeding_earns_longer_than_one_that_is_silent() {
    let started = tokio::time::Instant::now();
    let at = |seconds: u64| started + Duration::from_secs(seconds);

    // Silent: prompt, and the message says the channel may have no signal
    // rather than blaming the segment that never came.
    assert!(startup_overdue(started, at(14), 0, 0).is_none());
    let silent = startup_overdue(started, at(15), 0, 0).expect("silent tuner is overdue at 15s");
    assert!(
        silent.contains("no data"),
        "a silent tuner must say so, not blame a missing segment: {silent}"
    );

    // Feeding: the 18.1 s real measurement is INSIDE the budget. This is
    // the assertion the unfixed implementation fails.
    assert!(
        startup_overdue(started, at(16), 1, 0).is_none(),
        "one byte from the tuner must buy more than the silent budget"
    );
    assert!(
        startup_overdue(started, at(19), 4_000_000, 0).is_none(),
        "18.1s is what a real ATSC 3.0 channel needed; refusing it is the defect"
    );

    // But feeding is not forever, and the eventual refusal is specific
    // enough to tell a stalled producer from an absent channel.
    let stalled = startup_overdue(started, at(30), 4_000_000, 0)
        .expect("a feeding tuner that never publishes is overdue eventually");
    assert!(
        stalled.contains("4000000") && stalled.contains("no complete live segment"),
        "a stalled producer must name what arrived: {stalled}"
    );

    // The two budgets are ordered, whatever the constants become.
    assert!(
        STARTUP_FEEDING_TIMEOUT > STARTUP_TIMEOUT,
        "a feeding tuner must never get less time than a silent one"
    );
}

#[test]
fn startup_overdue_says_how_many_segments_it_has() {
    let started = tokio::time::Instant::now();
    let overdue = started + STARTUP_FEEDING_TIMEOUT;
    let none =
        startup_overdue(started, overdue, 4_000_000, 0).expect("zero-segment start is overdue");
    assert!(none.contains("no complete live segment"));
    let one =
        startup_overdue(started, overdue, 4_000_000, 1).expect("one-segment start is overdue");
    assert!(one.contains("1 live segment(s)"));
    assert!(one.contains("2 a player needs"));
}

#[test]
fn flex_lineup_combines_current_markers_and_fails_closed_on_unknown_drm() {
    let rows = serde_json::from_str::<Vec<serde_json::Value>>(
        r#"[
              {"GuideNumber":"2.1","GuideName":"Protected","DRM":1},
              {"GuideNumber":"4.1","GuideName":"Clear","DRM":0},
              {"GuideNumber":"5.1","GuideName":"Favorite","Favorite":1},
              {"GuideNumber":"6.1","GuideName":"Legacy","Tags":"favorite, DRM","DRM":0},
              {"GuideNumber":"7.1","GuideName":"Unknown","DRM":"unknown"},
              {"GuideNumber":"8.1","GuideName":"Null","DRM":null}
            ]"#,
    )
    .expect("FLEX lineup");
    let channels = validate_lineup(rows).expect("validated FLEX lineup");
    assert_eq!(channels.len(), 6);
    assert!(channels[0].drm);
    assert_eq!(channels[0].support, LiveTvChannelSupport::DrmUnsupported);
    assert!(!channels[1].drm);
    assert_eq!(channels[1].support, LiveTvChannelSupport::Ready);
    assert!(channels[2].favorite);
    assert!(channels[3].favorite);
    assert!(channels[3].drm, "legacy DRM must combine with DRM:0");
    assert!(channels[4].drm, "an unknown DRM marker must fail closed");
    assert!(channels[5].drm, "a null DRM marker must fail closed");
}

fn cache_test_snapshot(generation: i64) -> LiveTvSnapshot {
    LiveTvSnapshot {
        generation,
        device: LiveTvDevice {
            device_id: "test-device".to_owned(),
            friendly_name: "HDHomeRun FLEX 4K".to_owned(),
            model_number: "HDFX-4K".to_owned(),
            firmware_version: "test".to_owned(),
            tuner_count: 4,
        },
        channels: Vec::new(),
        freshness: SnapshotFreshness::Fresh,
        age_seconds: 0,
        last_success_at: 1,
        refresh_error: None,
        ffmpeg_graph_ready: false,
        ffmpeg_graph_message: "not probed".to_owned(),
        start_protocols: vec![1, 2, 3, 5],
    }
}

async fn forced_refresh_burst(
    cache: Arc<SnapshotCache>,
    outcome: Result<LiveTvSnapshot, LiveTvError>,
) -> Vec<Result<LiveTvSnapshot, LiveTvError>> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    for _ in 0..MAX_FORCED_REFRESH_CALLERS {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let release = Arc::clone(&release);
        let entered_tx = entered_tx.clone();
        let outcome = outcome.clone();
        tasks.push(tokio::spawn(async move {
            cache
                .get_or_refresh_inner(
                    7,
                    true,
                    move || entered_tx.send(()).expect("entry observer"),
                    move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let permit = release.acquire_owned().await.expect("release semaphore");
                        permit.forget();
                        outcome
                    },
                )
                .await
        }));
    }
    drop(entered_tx);
    // Each notification occurs after the caller recorded its request time
    // and acquired one bounded permit, but before it tries the cache lock.
    // Releasing the leader now therefore makes every other task a follower
    // by construction rather than by a wall-clock scheduling assumption.
    for _ in 0..MAX_FORCED_REFRESH_CALLERS {
        entered_rx.recv().await.expect("admitted caller");
    }
    release.add_permits(1);
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.expect("refresh task"));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one forced burst must perform exactly one device refresh"
    );
    assert_eq!(
        cache.forced_admission.available_permits(),
        MAX_FORCED_REFRESH_CALLERS,
        "every forced admission must be restored"
    );
    results
}

#[tokio::test]
async fn concurrent_forced_refreshes_perform_one_bounded_device_operation() {
    let cache = Arc::new(SnapshotCache::default());
    let results = forced_refresh_burst(Arc::clone(&cache), Ok(cache_test_snapshot(7))).await;
    for result in results {
        assert_eq!(result.expect("snapshot").generation, 7);
    }

    let held = (0..MAX_FORCED_REFRESH_CALLERS)
        .map(|_| {
            cache
                .forced_admission
                .try_acquire()
                .expect("bounded permit")
        })
        .collect::<Vec<_>>();
    assert!(
        cache.forced_admission.try_acquire().is_err(),
        "forced-refresh admission must have a fixed ceiling"
    );
    drop(held);
}

#[tokio::test]
async fn concurrent_forced_refreshes_share_failure_and_stale_fallback() {
    let cache = Arc::new(SnapshotCache::default());
    let failures = forced_refresh_burst(
        Arc::clone(&cache),
        Err(LiveTvError::DeviceUnavailable("device offline".to_owned())),
    )
    .await;
    for failure in failures {
        assert_eq!(
            failure.expect_err("offline tuner must fail").to_string(),
            "device offline"
        );
    }

    let cache = Arc::new(SnapshotCache::default());
    cache
        .get_or_refresh(7, false, || async { Ok(cache_test_snapshot(7)) })
        .await
        .expect("prime fresh snapshot");
    let stale = forced_refresh_burst(
        cache,
        Err(LiveTvError::DeviceUnavailable("device offline".to_owned())),
    )
    .await;
    for snapshot in stale {
        let snapshot = snapshot.expect("stale fallback");
        assert_eq!(snapshot.freshness, SnapshotFreshness::Stale);
        assert_eq!(snapshot.refresh_error.as_deref(), Some("device offline"));
    }
}

#[test]
fn discovery_accepts_the_documented_acronym_field_names() {
    let document = serde_json::from_str::<DiscoverDocument>(
        r#"{
              "FriendlyName":"HDHomeRun FLEX 4K",
              "ModelNumber":"HDFX-4K",
              "FirmwareVersion":"20250623",
              "DeviceID":"10ABCDEF",
              "TunerCount":4,
              "BaseURL":"http://hdhomerun.local:5004",
              "LineupURL":"http://hdhomerun.local:5004/lineup.json"
            }"#,
    )
    .expect("discovery document");
    let device = document.validated().expect("validated discovery");
    assert_eq!(device.device_id, "10ABCDEF");
    assert_eq!(device.tuner_count, 4);
    assert_eq!(
        document.lineup_url.as_deref(),
        Some("http://hdhomerun.local:5004/lineup.json")
    );
}

#[test]
fn advertised_hostname_is_ignored_but_path_port_and_scheme_are_strict() {
    let allowed = validate_advertised_url(
        "http://hdhomerun.local:5004/auto/v7.1",
        &["/auto/v7.1"],
        &[80, 5004],
    )
    .expect("safe routing hint");
    assert_eq!(allowed.host_str(), Some("hdhomerun.local"));
    assert_eq!(
        lineup_url(
            Ipv4Addr::new(10, 42, 4, 20),
            Some("http://never-resolve.invalid:5004/lineup.json"),
        )
        .expect("pinned lineup")
        .as_str(),
        "http://10.42.4.20:5004/lineup.json"
    );
    for refused in [
        "https://hdhomerun.local:5004/auto/v7.1",
        "http://user@hdhomerun.local:5004/auto/v7.1",
        "http://hdhomerun.local:8080/auto/v7.1",
        "http://hdhomerun.local:5004/auto/v8.1",
        "http://hdhomerun.local:5004/auto/v7.1?redirect=x",
    ] {
        assert!(
            validate_advertised_url(refused, &["/auto/v7.1"], &[80, 5004]).is_err(),
            "{refused}"
        );
    }
}

#[test]
fn config_defaults_disabled_without_a_compile_time_gate() {
    let config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    assert!(!config.enabled);
    assert_eq!(config.owner_node_id, "node-a");
    assert_eq!(config.max_sessions, 2);
    assert_eq!(config.output_height, 720);
    assert_eq!(config.generation, 0);
}

#[tokio::test]
async fn device_documents_refuse_redirects_and_declared_oversize_bodies() {
    let (redirect, redirect_server) = serve_once(
            b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        )
        .await;
    let redirected = fetch_json::<serde_json::Value>(&test_client(), redirect).await;
    redirect_server.await.expect("redirect server");
    assert!(
        matches!(redirected, Err(LiveTvError::DeviceUnavailable(message)) if message.contains("HTTP 302"))
    );

    let (oversized, oversized_server) = serve_once(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_DOCUMENT_BYTES + 1
            )
            .into_bytes(),
        )
        .await;
    let result = fetch_json::<serde_json::Value>(&test_client(), oversized).await;
    oversized_server.await.expect("oversize server");
    assert!(matches!(
        result,
        Err(LiveTvError::InvalidResponse(message)) if message.contains("too large")
    ));
}

#[tokio::test]
async fn device_documents_bound_chunked_bodies_without_content_length() {
    let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            MAX_DOCUMENT_BYTES + 1
        )
        .into_bytes();
    response.resize(response.len() + MAX_DOCUMENT_BYTES + 1, b'x');
    response.extend_from_slice(b"\r\n0\r\n\r\n");
    let (url, server) = serve_once(response).await;
    let result = fetch_json::<serde_json::Value>(&test_client(), url).await;
    server.await.expect("chunked server");
    assert!(matches!(
        result,
        Err(LiveTvError::InvalidResponse(message)) if message.contains("too large")
    ));
}

#[tokio::test(start_paused = true)]
async fn device_document_deadline_covers_a_stalled_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake HDHomeRun");
    let address = listener.local_addr().expect("fake address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept fake request");
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).await.expect("read fake request");
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("write headers");
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let url = reqwest::Url::parse(&format!("http://{address}/document.json")).expect("fake URL");
    let fetch =
        tokio::spawn(async move { fetch_json::<serde_json::Value>(&test_client(), url).await });
    tokio::task::yield_now().await;
    tokio::time::advance(DOCUMENT_TIMEOUT + Duration::from_secs(1)).await;
    let result = fetch.await.expect("fetch task");
    assert!(matches!(
        result,
        Err(LiveTvError::DeviceUnavailable(message)) if message.contains("timed out")
    ));
    server.abort();
}

fn live_playlist(first: u64, count: usize) -> Vec<u8> {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n\
             #EXT-X-MEDIA-SEQUENCE:{first}\n"
    );
    for sequence in first..first + count as u64 {
        playlist.push_str(&format!("#EXTINF:1.000000,\nsegment-{sequence:06}.ts\n"));
    }
    playlist.into_bytes()
}

#[test]
fn live_capabilities_are_canonical_and_owner_bound() {
    let owner = "node-a/with punctuation";
    let capability = format!(
        "ltv1.{}.{}",
        base64url_encode(owner.as_bytes()),
        "d03bb893-a723-43c8-994d-8cb86d9e337a"
    );
    assert_eq!(capability_owner(&capability).expect("owner"), owner);
    validate_capability_owner(&capability, owner).expect("matching owner");
    assert!(validate_capability_owner(&capability, "node-b").is_err());

    for refused in [
        "ltv1.bm9kZS1h.d03bb893-a723-43c8-994d-8cb86d9e337a.extra",
        "ltv1.bm9kZS1h=.d03bb893-a723-43c8-994d-8cb86d9e337a",
        "ltv1.bm9kZS1h.not-a-uuid",
        "other.bm9kZS1h.d03bb893-a723-43c8-994d-8cb86d9e337a",
    ] {
        assert!(capability_owner(refused).is_err(), "{refused}");
    }
}

#[test]
fn live_playlist_accepts_only_the_closed_numeric_inventory() {
    let parsed =
        parse_playlist_bytes(&live_playlist(41, MAX_LISTED_SEGMENTS)).expect("bounded playlist");
    assert_eq!(parsed.media_sequence, 41);
    assert_eq!(parsed.target_duration, 1);
    assert_eq!(parsed.listed_seconds, 24.0);
    assert_eq!(parsed.segments.len(), MAX_LISTED_SEGMENTS);

    for refused in [
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4,\n../secret.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\nsegment-000001.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:bad\n#EXTINF:4,\nsegment-000001.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4,\nsegment-000001.ts\n#EXT-X-ENDLIST\n"
                .as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4,\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1,\nsegment-000001.ts\n"
                .as_slice(),
            b"#EXTM3U\n#EXTINF:4,\nsegment-000001.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:0\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1.0,\nsegment-000001.ts\n"
                .as_slice(),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:nan,\nsegment-000001.ts\n"
                .as_slice(),
        ] {
            assert!(parse_playlist_bytes(refused).is_err());
        }
    let titled = b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1.0,title\nsegment-000001.ts\n";
    assert!(parse_playlist_bytes(titled).is_ok());
    assert!(parse_playlist_bytes(&live_playlist(1, MAX_LISTED_SEGMENTS + 1)).is_err());
}

#[test]
fn the_start_is_answered_at_the_second_listed_segment() {
    let inventory =
        |listed: usize, listed_seconds: f64, target_duration: u64, files: usize| ScratchInventory {
            playlist: Vec::new(),
            media_sequence: 10,
            listed,
            listed_seconds,
            target_duration,
            init: None,
            segments: (0..files)
                .map(|offset| {
                    let sequence = 10 + offset as u64;
                    (sequence, (format!("segment-{sequence:06}.ts"), 1))
                })
                .collect(),
        };

    assert!(!startup_publishable(&inventory(1, 1.0, 1, 2)));
    assert!(startup_publishable(&inventory(2, 2.0, 1, 2)));
    assert!(!startup_publishable(&inventory(2, 4.0, 3, 2)));
    assert!(startup_publishable(&inventory(3, 7.0, 3, 3)));

    let temp = crate::test_tempdir().expect("startup verdict root");
    let session = test_session(temp.path().join("startup-verdict"), 1);
    let mut state = session.state.lock().expect("session state");
    state.publication = Some(inventory(2, 2.0, 1, 2));
    assert!(
        startup_waiter_verdict(
            &state,
            session.started,
            session.started + STARTUP_FEEDING_TIMEOUT + Duration::from_secs(2),
            4_000_000,
        )
        .is_none(),
        "a publishable inventory awaiting its serving fence is not overdue"
    );
    state.publication = Some(inventory(1, 1.0, 1, 1));
    assert!(matches!(
        startup_waiter_verdict(
            &state,
            session.started,
            session.started + STARTUP_FEEDING_TIMEOUT + Duration::from_secs(2),
            4_000_000,
        ),
        Some(Err(LiveTvError::StartupTimeout(_)))
    ));
}

#[tokio::test]
async fn live_tv_qualification_counts_the_first_encoded_publication_once() {
    let temp = crate::test_tempdir().expect("qualification root");
    let manager = test_manager(temp.path());
    let copy_session = test_session(temp.path().join("copy"), 1);

    assert_eq!(
        manager.transcode.codec_qualification_encoder_count(
            Encoder::Software,
            plurx_core::transcode::OutputGrade::Sdr,
        ),
        0
    );
    copy_session.record_codec_qualification_publication(&manager.transcode, None);
    copy_session.record_codec_qualification_publication(&manager.transcode, None);
    assert_eq!(
        manager.transcode.codec_qualification_encoder_count(
            Encoder::Software,
            plurx_core::transcode::OutputGrade::Sdr,
        ),
        0,
        "a copy route without video admission must not count as encoding"
    );

    let encoded_session = test_session(temp.path().join("encoded"), 1);
    let admission = manager
        .transcode
        .admit_live_tv(1080, "h264", None, 720, Duration::ZERO)
        .await
        .expect("software Live TV admission");
    encoded_session.record_codec_qualification_publication(&manager.transcode, Some(&admission));
    encoded_session.record_codec_qualification_publication(&manager.transcode, Some(&admission));
    assert_eq!(
        manager.transcode.codec_qualification_encoder_count(
            Encoder::Software,
            plurx_core::transcode::OutputGrade::Sdr,
        ),
        1,
        "a worker retry at the same publication boundary double-counted the session"
    );
}

#[test]
fn progress_is_the_newest_listed_segment_not_the_window_sliding() {
    let temp = crate::test_tempdir().expect("session root");
    let session = test_session(temp.path().join("progress"), 1);
    let base = tokio::time::Instant::now();
    let mut state = session.state.lock().expect("session state");
    state.last_progress = base;
    state.newest_listed = None;
    state.publication = None;

    for tick in 1_u64..=40 {
        let listed = usize::try_from(tick.min(MAX_LISTED_SEGMENTS as u64)).expect("listed count");
        let media_sequence = tick.saturating_sub(MAX_LISTED_SEGMENTS as u64);
        let now = base + Duration::from_secs(tick);
        record_producer_inventory(
            &mut state,
            ScratchInventory {
                playlist: Vec::new(),
                media_sequence,
                listed,
                listed_seconds: listed as f64,
                target_duration: 1,
                init: None,
                segments: HashMap::new(),
            },
            now,
        );
        assert_eq!(state.newest_listed, Some(tick - 1));
        assert_eq!(state.last_progress, now);
        assert!(!producer_progress_overdue(true, &state, now));
    }

    assert!(!producer_progress_overdue(
        true,
        &state,
        state.last_progress + PRODUCER_PROGRESS_TIMEOUT - Duration::from_millis(1)
    ));
    assert!(producer_progress_overdue(
        true,
        &state,
        state.last_progress + PRODUCER_PROGRESS_TIMEOUT
    ));
}

#[tokio::test]
async fn live_scratch_keeps_playing_during_http_uploads_and_pending_playlist() {
    for extension in ["ts", "m4s"] {
        let temp = crate::test_tempdir().expect("scratch root");
        let sink = crate::scratch_put::PutSink::bind_live_tv(temp.path().to_owned())
            .expect("live upload sink");
        let base = sink.base_url(0);
        let client = reqwest::Client::new();
        for sequence in 10..=12 {
            client
                .put(format!("{base}/segment-{sequence:06}.{extension}"))
                .body("transport")
                .send()
                .await
                .expect("segment upload")
                .error_for_status()
                .expect("segment committed");
        }
        let playlist = |first| {
            let text = String::from_utf8(live_playlist(first, 3)).expect("playlist");
            if extension == "m4s" {
                text.replace(".ts", ".m4s").replace(
                    "#EXT-X-VERSION:3",
                    "#EXT-X-VERSION:7\n#EXT-X-MAP:URI=\"init.mp4\"",
                )
            } else {
                text
            }
        };
        if extension == "m4s" {
            client
                .put(format!("{base}/init.mp4"))
                .body("init")
                .send()
                .await
                .expect("init upload")
                .error_for_status()
                .expect("init committed");
        }
        client
            .put(format!("{base}/index.m3u8"))
            .body(playlist(10))
            .send()
            .await
            .expect("playlist upload")
            .error_for_status()
            .expect("published");

        // Hold a real segment upload open while the next playlist arrives.
        // The playlist remains pending until this segment commits, so the
        // scanner must tolerate both receiver temporaries without publishing
        // either unfinished resource.
        let url = reqwest::Url::parse(&base).expect("upload URL");
        let mut segment = tokio::net::TcpStream::connect((
            url.host_str().expect("host"),
            url.port().expect("port"),
        ))
        .await
        .expect("connect");
        segment.write_all(format!(
            "PUT {}/segment-000013.{extension} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 8\r\n\r\npart",
            url.path()).as_bytes()).await.expect("partial segment");
        let pending = client
            .put(format!("{base}/index.m3u8"))
            .body(playlist(11))
            .send()
            .await
            .expect("next playlist");
        assert_eq!(pending.status(), reqwest::StatusCode::ACCEPTED);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut entries = tokio::fs::read_dir(temp.path()).await.expect("directory");
                let mut uploads = 0;
                while let Some(entry) = entries.next_entry().await.expect("entry") {
                    if entry.file_name().to_string_lossy().ends_with(".tmp") {
                        uploads += 1;
                    }
                }
                if uploads == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("two overlapping uploads");
        for _ in 0..3 {
            let inventory = inspect_scratch(temp.path())
                .await
                .expect("live scan")
                .expect("published window");
            assert_eq!(inventory.media_sequence, 10);
            assert_eq!(inventory.newest_listed(), Some(12));
            assert_eq!(inventory.segments.len(), 3);
        }
        segment.write_all(b"done").await.expect("finish segment");
        let mut response = Vec::new();
        segment
            .read_to_end(&mut response)
            .await
            .expect("upload response");
        assert!(response.starts_with(b"HTTP/1.1 201"));
        let inventory = inspect_scratch(temp.path())
            .await
            .expect("completed scan")
            .expect("new published window");
        assert_eq!(inventory.media_sequence, 11);
        assert_eq!(inventory.newest_listed(), Some(13));
        assert_eq!(inventory.segments.len(), 4);
        assert!(sink.failure().is_none());
    }
}

#[tokio::test]
async fn live_scratch_upload_temporaries_keep_name_count_and_byte_limits() {
    let temp = crate::test_tempdir().expect("scratch root");
    for name in [
        "index.m3u8.0.tmp",
        "init.mp4.1.tmp",
        "segment-000013.ts.2.tmp",
        "segment-000014.m4s.18446744073709551615.tmp",
    ] {
        std::fs::write(temp.path().join(name), b"upload").expect("temporary");
    }
    assert!(scan_scratch_directory(temp.path())
        .expect("valid uploads")
        .is_some());
    for name in [
        "notes.txt.0.tmp",
        "index.m3u8..tmp",
        "init.mp4.-1.tmp",
        "segment-000013.ts.no.tmp",
        "segment-000013.ts.18446744073709551616.tmp",
        "segment-000013.ts.1.2.tmp",
        "segment-x.ts.1.tmp",
    ] {
        let path = temp.path().join(name);
        std::fs::write(&path, b"stray").expect("invalid temporary");
        assert!(
            scan_scratch_directory(temp.path())
                .err()
                .expect("refused")
                .to_string()
                .contains("unexpected filename"),
            "{name}"
        );
        std::fs::remove_file(path).expect("remove invalid temporary");
    }
    let large = temp.path().join("segment-000015.ts.3.tmp");
    let file = std::fs::File::create(&large).expect("temporary");
    file.set_len(MAX_SESSION_BYTES + 1).expect("sparse upload");
    assert!(scan_scratch_directory(temp.path())
        .err()
        .expect("oversized upload")
        .to_string()
        .contains("128 MiB"));
    drop(file);
    std::fs::remove_file(&large).expect("remove oversized upload");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("index.m3u8.0.tmp", &large).expect("temporary symlink");
        assert!(scan_scratch_directory(temp.path())
            .err()
            .expect("symlink refused")
            .to_string()
            .contains("non-regular"));
        std::fs::remove_file(&large).expect("remove symlink");
    }
    for request in 4..=MAX_UPLOAD_TEMPORARIES {
        std::fs::write(
            temp.path().join(format!("segment-000015.ts.{request}.tmp")),
            b"x",
        )
        .expect("upload temporary");
    }
    assert!(scan_scratch_directory(temp.path())
        .err()
        .expect("excess uploads")
        .to_string()
        .contains("too many temporary"));
}

#[tokio::test]
async fn live_scratch_inventory_enforces_list_deletion_and_temp_budgets() {
    let temp = crate::test_tempdir().expect("scratch root");
    let directory = temp.path();
    tokio::fs::write(directory.join("index.m3u8"), live_playlist(10, 3))
        .await
        .expect("playlist");
    for sequence in 10..=12 {
        tokio::fs::write(
            directory.join(format!("segment-{sequence:06}.ts")),
            b"transport",
        )
        .await
        .expect("segment");
    }
    let inventory = inspect_scratch(directory)
        .await
        .expect("scratch")
        .expect("published");
    assert_eq!(inventory.media_sequence, 10);
    assert_eq!(inventory.listed, 3);
    assert_eq!(inventory.newest_listed(), Some(12));
    assert_eq!(inventory.segments.len(), 3);

    for sequence in (5..=9).rev() {
        tokio::fs::write(directory.join(format!("segment-{sequence:06}.ts")), b"lag")
            .await
            .expect("permitted deletion lag");
        assert!(inspect_scratch(directory).await.is_ok());
    }
    tokio::fs::write(directory.join("segment-000004.ts"), b"excess lag")
        .await
        .expect("sixth deletion lag");
    assert!(inspect_scratch(directory).await.is_err());
}

/// Plan L-02 §5.4 / correction 13: the scan moved into one blocking walk
/// and kept every predicate. Each case starts from a valid published
/// window and breaks exactly one rule.
#[cfg(unix)]
#[tokio::test]
async fn inspect_scratch_keeps_every_check_in_one_blocking_scan() {
    async fn valid_window(directory: &Path) {
        tokio::fs::create_dir_all(directory).await.expect("scratch");
        tokio::fs::write(directory.join("index.m3u8"), live_playlist(10, 3))
            .await
            .expect("playlist");
        for sequence in 10..=12 {
            tokio::fs::write(
                directory.join(format!("segment-{sequence:06}.ts")),
                b"transport",
            )
            .await
            .expect("segment");
        }
    }
    let temp = crate::test_tempdir().expect("scratch root");
    let baseline = temp.path().join("baseline");
    valid_window(&baseline).await;
    assert!(inspect_scratch(&baseline).await.expect("valid").is_some());

    let cases: [(&str, &str); 7] = [
        ("symlink", "non-regular"),
        ("subdirectory", "non-regular"),
        ("growing temporary segment", "128 MiB"),
        ("oversized playlist", "playlist exceeded"),
        ("unexpected name", "unexpected filename"),
        ("two temporary files", "too many temporary"),
        ("deletion lag", "segment inventory budget"),
    ];
    for (case, message) in cases {
        let directory = temp.path().join(case.replace(' ', "-"));
        valid_window(&directory).await;
        match case {
            "symlink" => std::os::unix::fs::symlink(&baseline, directory.join("segment-000013.ts"))
                .expect("symlink"),
            "subdirectory" => {
                std::fs::create_dir(directory.join("segment-000013.ts")).expect("subdirectory")
            }
            "growing temporary segment" => {
                // A temporary segment grows before the playlist changes:
                // the playlist is untouched and still names three small
                // segments, and the total alone must refuse it.
                let file = std::fs::File::create(directory.join("segment-000013.ts.tmp"))
                    .expect("temporary segment");
                file.set_len(200 * 1024 * 1024).expect("sparse 200 MiB");
            }
            "oversized playlist" => std::fs::write(
                directory.join("index.m3u8"),
                vec![b'#'; MAX_PLAYLIST_BYTES as usize + 1],
            )
            .expect("oversized playlist"),
            "unexpected name" => {
                std::fs::write(directory.join("notes.txt"), b"x").expect("stray file")
            }
            "two temporary files" => {
                std::fs::write(directory.join("segment-000013.ts.tmp"), b"x").expect("tmp");
                std::fs::write(directory.join("index.m3u8.tmp"), b"x").expect("tmp");
            }
            "deletion lag" => {
                for sequence in 4..=9 {
                    std::fs::write(directory.join(format!("segment-{sequence:06}.ts")), b"lag")
                        .expect("lagging segment");
                }
            }
            _ => unreachable!(),
        }
        match inspect_scratch(&directory).await {
            Err(error) => assert!(
                error.to_string().contains(message),
                "{case}: {error} should mention {message}"
            ),
            Ok(inventory) => panic!("{case} was accepted: {:?}", inventory.map(|i| i.listed)),
        }
    }
}

#[tokio::test]
async fn the_rename_before_rewrite_instant_is_not_a_failure() {
    let temp = crate::test_tempdir().expect("scratch root");
    let directory = temp.path();
    tokio::fs::write(directory.join("index.m3u8"), live_playlist(10, 3))
        .await
        .expect("playlist");
    for sequence in 6..=13 {
        tokio::fs::write(
            directory.join(format!("segment-{sequence:06}.ts")),
            b"transport",
        )
        .await
        .expect("segment");
    }
    let inventory = inspect_scratch(directory)
        .await
        .expect("threshold plus rename transient")
        .expect("published inventory");
    assert_eq!(inventory.listed, 3);
    assert_eq!(inventory.segments.len(), 8);

    tokio::fs::write(directory.join("segment-000005.ts"), b"excess")
        .await
        .expect("sixth unlisted segment");
    assert!(inspect_scratch(directory).await.is_err());
}

#[tokio::test]
async fn ten_live_windows_publish_manifest_and_bounded_deletion_lag_atomically() {
    let temp = crate::test_tempdir().expect("scratch root");
    let directory = temp.path();
    for first in 1_u64..=10 {
        let playlist = live_playlist(first, MAX_LISTED_SEGMENTS);
        tokio::fs::write(directory.join("index.m3u8"), &playlist)
            .await
            .expect("playlist");
        for sequence in first - 1..first + MAX_LISTED_SEGMENTS as u64 {
            tokio::fs::write(
                directory.join(format!("segment-{sequence:06}.ts")),
                format!("window-{first}-segment-{sequence}"),
            )
            .await
            .expect("segment");
        }
        if first > 1 {
            let _ = tokio::fs::remove_file(directory.join(format!("segment-{:06}.ts", first - 2)))
                .await;
        }
        let publication = inspect_scratch(directory)
            .await
            .expect("valid scratch")
            .expect("published window");
        assert_eq!(publication.playlist, playlist);
        assert_eq!(publication.media_sequence, first);
        assert_eq!(publication.segments.len(), MAX_LISTED_SEGMENTS + 1);
        assert!(publication.segments.contains_key(&(first - 1)));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn live_scratch_inventory_rejects_symlinks() {
    let temp = crate::test_tempdir().expect("scratch root");
    let directory = temp.path().join("session");
    tokio::fs::create_dir(&directory)
        .await
        .expect("session dir");
    tokio::fs::write(directory.join("index.m3u8"), live_playlist(1, 1))
        .await
        .expect("playlist");
    let outside = temp.path().join("outside.ts");
    tokio::fs::write(&outside, b"transport")
        .await
        .expect("outside segment");
    std::os::unix::fs::symlink(&outside, directory.join("segment-000001.ts"))
        .expect("segment symlink");
    assert!(inspect_scratch(&directory).await.is_err());
}

#[tokio::test]
async fn live_tuner_open_returns_after_headers_without_buffering_the_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake tuner");
    let address = listener.local_addr().expect("fake address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept tuner open");
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).await.expect("read tuner open");
        assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /auto/v7.1 "));
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ndata\r\n",
                )
                .await
                .expect("write stream prefix");
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let url = reqwest::Url::parse(&format!("http://{address}/auto/v7.1")).expect("stream URL");
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        open_tuner_stream(
            &test_client(),
            url,
            tokio::time::Instant::now() + Duration::from_secs(1),
        ),
    )
    .await
    .expect("headers must not wait for the endless body")
    .expect("stream response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    drop(response);
    server.abort();
}

#[tokio::test]
async fn live_tuner_open_does_not_retry_a_failed_get() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake tuner");
    let address = listener.local_addr().expect("fake address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept tuner open");
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).await.expect("read tuner open");
        stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write failure");
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_ok()
    });
    let url = reqwest::Url::parse(&format!("http://{address}/auto/v7.1")).expect("stream URL");
    let result = open_tuner_stream(
        &test_client(),
        url,
        tokio::time::Instant::now() + Duration::from_secs(1),
    )
    .await;
    assert!(matches!(result, Err(LiveTvError::TunerUnavailable(_))));
    assert!(
        !server.await.expect("retry observer"),
        "a tuner GET was retried"
    );
}

// ---- programme guide --------------------------------------------------

fn guide_lineup() -> Vec<LiveTvChannel> {
    vec![
        LiveTvChannel {
            id: "7.1".into(),
            guide_number: "7.1".into(),
            guide_name: "WABC".into(),
            favorite: false,
            drm: false,
            support: LiveTvChannelSupport::Ready,
            hd: None,
            video_codec: None,
            audio_codec: None,
            source_format: None,
        },
        LiveTvChannel {
            id: "4.1".into(),
            guide_number: "4.1".into(),
            guide_name: "WNBC".into(),
            favorite: false,
            drm: false,
            support: LiveTvChannelSupport::Ready,
            hd: None,
            video_codec: None,
            audio_codec: None,
            source_format: None,
        },
    ]
}

fn guide_by_number(lineup: &[LiveTvChannel]) -> BTreeMap<String, &LiveTvChannel> {
    lineup
        .iter()
        .map(|channel| (channel.guide_number.clone(), channel))
        .collect()
}

#[test]
fn hdhomerun_guide_maps_the_documented_shape_and_drops_channels_the_lineup_does_not_carry() {
    let lineup = guide_lineup();
    let body = br#"[
          {"GuideNumber":"7.1","GuideName":"WABC","Affiliate":"ABC",
           "ImageURL":"https://example.invalid/abc.png",
           "Guide":[{"StartTime":1789000800,"EndTime":1789002600,"Title":"City Beat",
                     "EpisodeTitle":"Pier 40","EpisodeNumber":"S03E14","Synopsis":"A rebuild.",
                     "OriginalAirdate":1789000800,"ImageURL":"https://example.invalid/e.png",
                     "Filter":["News"]}]},
          {"GuideNumber":"99.9","GuideName":"Nowhere","Guide":[
                    {"StartTime":1789000800,"EndTime":1789002600,"Title":"Not in the lineup"}]}
        ]"#;
    let channels =
        guide::parse_hdhomerun_guide(body, &guide_by_number(&lineup)).expect("guide document");
    assert_eq!(
        channels.len(),
        1,
        "a guide row without a lineup row is not a channel"
    );
    let channel = &channels[0];
    assert_eq!(channel.id, "7.1");
    assert_eq!(channel.affiliate.as_deref(), Some("ABC"));
    let programme = &channel.programmes[0];
    assert_eq!(programme.title, "City Beat");
    assert_eq!(programme.episode.as_deref(), Some("S3E14"));
    assert_eq!(programme.original_air_date.as_deref(), Some("2026-09-10"));
    assert_eq!(programme.filters, vec!["News".to_owned()]);
}

/// Admission is about occupancy, never about who arrived first. With four
/// tuners and one reserved for viewing, "one viewer and three recordings"
/// fits and "four recordings" does not — whichever order they turned up
/// in. An order-dependent answer would make the same fleet behave
/// differently on two identical evenings.
///
/// Slots are transports (plan L-03 §3.4): the first argument below is the
/// number of transports only viewers hold, however many viewers share
/// each, and the second the number holding a recording.
#[test]
fn capacity_rules_over_transports_do_not_depend_on_who_arrived_first() {
    let may_open = |viewer_transports: usize, recording_transports: usize, max: u8, reserve: u8| {
        LiveTvRegistry::occupancy_admits(
            viewer_transports + recording_transports,
            recording_transports,
            max,
            reserve,
        )
    };

    assert!(may_open(0, 2, 4, 1), "two recordings, a third may open");
    assert!(
        may_open(1, 2, 4, 1),
        "one viewer and two recordings: a third recording still fits four tuners"
    );
    assert!(
        !may_open(0, 3, 4, 1),
        "three recordings already hold the most they may; the reserve is the point"
    );
    assert!(
        !may_open(1, 3, 4, 1),
        "one viewer and three recordings fills the set"
    );
    assert!(
        !may_open(4, 0, 4, 1),
        "four viewers leave nothing, reserve or no reserve"
    );
    assert!(
        !may_open(0, 0, 1, 1),
        "reserving the only tuner stops recording rather than underflowing"
    );
}

#[tokio::test]
async fn a_persisted_guide_survives_a_restart_and_keeps_the_age_it_had() {
    let root = tempfile::tempdir().expect("temp root");
    let config = {
        let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
        config.generation = 7;
        config.guide_source = GuideSource::HdHomeRun;
        config
    };
    let window = GuideWindow {
        start: 0,
        end: i64::MAX / 4,
    };

    let first = GuideCache::with_store(root.path().to_path_buf());
    first
        .store(7, 1, &Ok(guide_with(2, 3, 16)))
        .await
        .then_some(())
        .expect("first publication");
    assert!(
        root.path().join(PERSISTED_GUIDE_FILE).exists(),
        "a successful refresh writes the owner's copy"
    );

    // A second cache over the same directory is what a restart looks like.
    let restarted = GuideCache::with_store(root.path().to_path_buf());
    let served = restarted.read(&config, window.clone()).await;
    assert_eq!(
        served.freshness,
        GuideFreshness::Fresh,
        "a restarted owner serves the grid it had, not `unavailable`"
    );
    assert_eq!(served.channels.len(), 2);

    // A generation change is a different lineup: the copy goes, and so
    // does the file.
    restarted.invalidate().await;
    assert!(!root.path().join(PERSISTED_GUIDE_FILE).exists());
}

#[tokio::test]
async fn an_unavailable_guide_still_says_when_to_ask_again() {
    let cache = GuideCache::default();
    let config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    cache.note_next_refresh(1_789_004_400).await;
    let served = cache
        .read(
            &config,
            GuideWindow {
                start: 0,
                end: 3600,
            },
        )
        .await;
    assert_eq!(served.freshness, GuideFreshness::Unavailable);
    assert_eq!(
        served.next_refresh_at,
        Some(1_789_004_400),
        "a client polls on the owner's clock even when there is nothing to show"
    );
}

#[test]
fn a_settings_change_wakes_the_refresh_loop_and_the_first_observation_does_not() {
    use futures_util::FutureExt as _;

    let cache = GuideCache::default();
    // A `Notify` permit is only observable by polling `notified()`, so
    // each assertion polls once and takes whatever permit is waiting.
    cache.observe_generation(4);
    assert!(
        Box::pin(cache.wake.notified()).now_or_never().is_none(),
        "startup is already running a tick; the first observation is not an event"
    );
    cache.observe_generation(5);
    assert!(
        Box::pin(cache.wake.notified()).now_or_never().is_some(),
        "a settings save must not wait out a twenty-minute sleep"
    );
}

#[test]
fn hdhomerun_rows_carry_series_and_programme_identity_when_the_tier_supplies_it() {
    let lineup = guide_lineup();
    let body = br#"[{"GuideNumber":"7.1","Guide":[
            {"StartTime":1789000800,"EndTime":1789002600,"Title":"City Beat",
             "SeriesID":"EP01234567","ProgramID":"EP012345670023"}]}]"#;
    let channels =
        guide::parse_hdhomerun_guide(body, &guide_by_number(&lineup)).expect("guide document");
    let programme = &channels[0].programmes[0];
    assert_eq!(
        programme.series_id.as_deref(),
        Some("EP01234567"),
        "`SeriesID` needs an explicit serde rename — PascalCase would look for `SeriesId`"
    );
    assert_eq!(programme.programme_id.as_deref(), Some("EP012345670023"));
    assert_eq!(
        programme.is_new, None,
        "HDHomeRun's Filter tags are genres, and never a first-run marker"
    );
}

#[test]
fn a_tier_without_identity_fields_parses_exactly_as_it_did_before() {
    let lineup = guide_lineup();
    let body = br#"[{"GuideNumber":"7.1","Guide":[
            {"StartTime":1789000800,"EndTime":1789002600,"Title":"City Beat"}]}]"#;
    let channels =
        guide::parse_hdhomerun_guide(body, &guide_by_number(&lineup)).expect("guide document");
    let programme = &channels[0].programmes[0];
    assert_eq!(programme.title, "City Beat");
    assert_eq!(programme.series_id, None);
    assert_eq!(programme.programme_id, None);
}

#[test]
fn an_xmltv_dd_progid_yields_both_identities_and_new_marks_a_first_run() {
    let lineup = guide_lineup();
    let body = br#"<tv>
            <channel id="c1"><display-name>7.1</display-name></channel>
            <programme channel="c1" start="20260913120000 +0000" stop="20260913123000 +0000">
              <title>Kitchen Table</title>
              <episode-num system="dd_progid">EP012345670023</episode-num>
              <new/>
            </programme>
          </tv>"#;
    let channels = guide::parse_xmltv(body, &lineup).expect("xmltv document");
    let programme = &channels[0].programmes[0];
    assert_eq!(programme.series_id.as_deref(), Some("EP01234567"));
    assert_eq!(programme.programme_id.as_deref(), Some("EP012345670023"));
    assert_eq!(programme.is_new, Some(true));
    assert_eq!(
        programme.episode, None,
        "dd_progid is an identity, never a season and episode number"
    );
}

#[test]
fn a_channel_holds_a_fortnight_of_rows_rather_than_the_old_two_hundred() {
    let rows = (0..900)
        .map(|slot| LiveTvProgramme {
            start: 1_789_000_000 + slot * 1800,
            end: 1_789_000_000 + (slot + 1) * 1800,
            title: format!("Slot {slot}"),
            episode_title: None,
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: None,
            programme_id: None,
            is_new: None,
            filters: Vec::new(),
        })
        .collect::<Vec<_>>();
    let normalised = guide::normalise_programmes(rows);
    assert_eq!(
        normalised.len(),
        guide::GUIDE_MAX_PROGRAMMES_PER_CHANNEL,
        "a fortnight of half-hour slots is ~670 rows; the old cap of 200 truncated it"
    );
    assert!(normalised.len() > 670);
}

#[test]
fn merging_a_cached_page_with_a_fresher_one_keeps_one_row_and_gains_its_identity() {
    let base = |series: Option<&str>, end: i64| LiveTvProgramme {
        start: 1_789_000_800,
        end,
        title: "Kitchen Table".into(),
        episode_title: None,
        episode: None,
        synopsis: None,
        image_url: None,
        original_air_date: None,
        series_id: series.map(str::to_owned),
        programme_id: None,
        is_new: None,
        filters: Vec::new(),
    };
    let merged = guide::normalise_programmes(vec![
        base(None, 1_789_002_600),
        base(Some("EP1"), 1_789_002_600),
    ]);
    assert_eq!(merged.len(), 1, "the same airing twice is one row");
    assert_eq!(
        merged[0].series_id.as_deref(),
        Some("EP1"),
        "a cached row without identity picks it up from a fresher page"
    );
}

#[test]
fn guide_rows_are_bounded_because_the_guide_host_is_untrusted_input() {
    let lineup = guide_lineup();
    let long = "x".repeat(4096);
    let body = format!(
        r#"[{{"GuideNumber":"7.1","Affiliate":"{long}","ImageURL":"http://example.invalid/i.png",
               "Guide":[
                 {{"StartTime":1789000800,"EndTime":1789002600,"Title":"{long}","Synopsis":"{long}",
                   "EpisodeNumber":"garbage","ImageURL":"http://plain.invalid/x.png",
                   "Filter":["a","b","c","d","e","f","g","h","i","j"]}},
                 {{"StartTime":1789002600,"EndTime":1789002600,"Title":"zero length"}},
                 {{"StartTime":1789002600,"EndTime":1789004400}}
               ]}}]"#
    );
    let channels = guide::parse_hdhomerun_guide(body.as_bytes(), &guide_by_number(&lineup))
        .expect("guide document");
    let channel = &channels[0];
    assert_eq!(channel.affiliate.as_deref().map(str::len), Some(32));
    // Artwork is passed through to clients, so a non-https URL must not be.
    assert_eq!(channel.image_url, None);
    assert_eq!(
        channel.programmes.len(),
        1,
        "a zero-length and a title-less row are dropped"
    );
    let programme = &channel.programmes[0];
    assert_eq!(programme.title.len(), 256);
    assert_eq!(programme.synopsis.as_deref().map(str::len), Some(1024));
    assert_eq!(
        programme.episode, None,
        "an unparseable episode number is dropped, not guessed"
    );
    assert_eq!(programme.image_url, None);
    assert_eq!(programme.filters.len(), 8);
}

#[test]
fn overlapping_guide_rows_are_normalised_so_what_is_on_now_is_never_ambiguous() {
    let rows = vec![
        LiveTvProgramme {
            start: 200,
            end: 400,
            title: "Second".into(),
            episode_title: None,
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: None,
            programme_id: None,
            is_new: None,
            filters: Vec::new(),
        },
        LiveTvProgramme {
            start: 100,
            end: 300,
            title: "First".into(),
            episode_title: None,
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: None,
            programme_id: None,
            is_new: None,
            filters: Vec::new(),
        },
        LiveTvProgramme {
            start: 120,
            end: 260,
            title: "Swallowed".into(),
            episode_title: None,
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: None,
            programme_id: None,
            is_new: None,
            filters: Vec::new(),
        },
    ];
    let normalised = guide::normalise_programmes(rows);
    assert_eq!(
        normalised
            .iter()
            .map(|p| (p.start, p.end, p.title.as_str()))
            .collect::<Vec<_>>(),
        vec![(100, 200, "First"), (200, 400, "Second")],
        "a row entirely inside another disappears, and a genuine overlap \
             shortens the earlier row rather than moving the later row's start \
             — a start is the DVR's identity for an airing"
    );
}

#[test]
fn the_guide_host_allowlist_pins_scheme_port_userinfo_and_host() {
    let approved = |raw: &str| guide::approved_guide_url(&reqwest::Url::parse(raw).expect("url"));
    assert!(approved("https://api.hdhomerun.com/api/guide"));
    assert!(approved("https://my.hdhomerun.com/api/guide.php"));
    assert!(!approved("http://api.hdhomerun.com/api/guide"));
    assert!(!approved("https://api.hdhomerun.com:8443/api/guide"));
    assert!(!approved("https://user:pass@api.hdhomerun.com/api/guide"));
    assert!(!approved(
        "https://api.hdhomerun.com.evil.invalid/api/guide"
    ));
    assert!(!approved("https://10.42.1.20/api/guide"));
}

#[test]
fn a_guide_request_url_carries_the_credential_and_nothing_else_leaves_the_allowlist() {
    let url =
        guide::guide_request_url("secret-auth", Some("7.1"), Some(1789000800)).expect("guide url");
    assert_eq!(url.host_str(), Some("api.hdhomerun.com"));
    let pairs = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<Vec<_>>();
    assert!(pairs.contains(&("DeviceAuth".to_owned(), "secret-auth".to_owned())));
    assert!(pairs.contains(&("Channel".to_owned(), "7.1".to_owned())));
    assert!(pairs.contains(&("Start".to_owned(), "1789000800".to_owned())));
}

#[test]
fn xmltv_matches_by_display_name_then_lcn_then_callsign_and_drops_the_rest() {
    let lineup = guide_lineup();
    let document = br#"<?xml version="1.0"?>
        <tv>
          <channel id="a"><display-name>7.1</display-name></channel>
          <channel id="b"><display-name>NBC New York</display-name><lcn>4.1</lcn></channel>
          <channel id="c"><display-name>Not in the lineup</display-name></channel>
          <programme start="20260907200000 -0400" stop="20260907203000 -0400" channel="a">
            <title>City Beat</title><sub-title>Pier 40</sub-title><desc>A rebuild.</desc>
            <episode-num system="xmltv_ns">2.13.</episode-num>
            <category>News</category><date>20260907</date>
          </programme>
          <programme start="20260907200000 -0400" stop="20260907210000 -0400" channel="b">
            <title>The Long Game</title>
          </programme>
          <programme start="20260907200000 -0400" stop="20260907210000 -0400" channel="c">
            <title>Nobody watches this</title>
          </programme>
        </tv>"#;
    let channels = guide::parse_xmltv(document, &lineup).expect("xmltv");
    assert_eq!(channels.len(), 2, "an unmatched XMLTV channel is dropped");
    let abc = channels.iter().find(|c| c.id == "7.1").expect("7.1");
    assert_eq!(abc.programmes[0].title, "City Beat");
    assert_eq!(abc.programmes[0].episode.as_deref(), Some("S3E14"));
    assert_eq!(
        abc.programmes[0].original_air_date.as_deref(),
        Some("2026-09-07")
    );
    assert_eq!(abc.programmes[0].filters, vec!["News".to_owned()]);
    // 20:00 EDT on 2026-09-07 is 2026-09-08T00:00:00Z.
    assert_eq!(abc.programmes[0].start, 1788825600);
    let nbc = channels.iter().find(|c| c.id == "4.1").expect("4.1");
    assert_eq!(nbc.programmes[0].title, "The Long Game");
}

#[test]
fn an_xmltv_time_without_an_offset_is_an_error_for_that_row_and_never_a_guess() {
    assert_eq!(
        guide::parse_xmltv_time("20260907200000 -0400"),
        Some(1788825600)
    );
    assert_eq!(
        guide::parse_xmltv_time("20260908000000 +0000"),
        Some(1788825600)
    );
    assert_eq!(guide::parse_xmltv_time("20260907200000"), None);
    assert_eq!(guide::parse_xmltv_time("nonsense +0000"), None);
    assert_eq!(guide::parse_xmltv_time("20261307200000 +0000"), None);
}

#[test]
fn xmltv_accepts_a_gzipped_document_by_sniffing_it_and_refuses_a_bomb() {
    use std::io::Write as _;
    let plain = b"<?xml version=\"1.0\"?><tv></tv>";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(plain).expect("gzip");
    let gzipped = encoder.finish().expect("gzip finish");
    assert_eq!(
        guide::decompress_if_gzip(gzipped).expect("inflate"),
        plain.to_vec()
    );
    assert_eq!(
        guide::decompress_if_gzip(plain.to_vec()).expect("passthrough"),
        plain.to_vec()
    );
    let mut bomb = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    bomb.write_all(&vec![b'a'; guide::GUIDE_MAX_DOCUMENT_BYTES + 4096])
        .expect("gzip bomb");
    let bomb = bomb.finish().expect("gzip bomb finish");
    assert!(bomb.len() < 64 * 1024, "the bomb must be small on the wire");
    assert!(
        guide::decompress_if_gzip(bomb).is_err(),
        "a small compressed body must not become an unbounded allocation"
    );
}

#[test]
fn episode_numbers_normalise_to_one_spelling_across_both_sources() {
    assert_eq!(
        guide::normalise_episode(Some("S03E14".into())).as_deref(),
        Some("S3E14")
    );
    assert_eq!(
        guide::normalise_episode(Some("s3e14".into())).as_deref(),
        Some("S3E14")
    );
    assert_eq!(
        guide::normalise_episode(Some("E07".into())).as_deref(),
        Some("E7")
    );
    assert_eq!(guide::normalise_episode(Some("14".into())), None);
    assert_eq!(guide::normalise_episode(Some("SxxEyy".into())), None);
    assert_eq!(guide::xmltv_ns_episode("2.13.").as_deref(), Some("S3E14"));
    assert_eq!(
        guide::xmltv_ns_episode("2.13/26.").as_deref(),
        Some("S3E14")
    );
    assert_eq!(guide::xmltv_ns_episode(".4.").as_deref(), Some("E5"));
    assert_eq!(guide::xmltv_ns_episode("nonsense"), None);
}

fn guide_with(channels: usize, programmes_each: usize, title_bytes: usize) -> LiveTvGuide {
    LiveTvGuide {
        source: "hdhomerun".into(),
        freshness: GuideFreshness::Fresh,
        age_seconds: 0,
        fetched_at: Some(1789000000),
        window: GuideWindow {
            start: 0,
            end: i64::from(u32::MAX),
        },
        refresh_error: None,
        next_refresh_at: None,
        matched_channels: channels,
        lineup_channels: channels,
        channels: (0..channels)
            .map(|index| LiveTvGuideChannel {
                id: format!("{index}.1"),
                guide_number: format!("{index}.1"),
                affiliate: None,
                image_url: None,
                programmes: (0..programmes_each)
                    .map(|slot| LiveTvProgramme {
                        start: (slot as i64) * 1800,
                        end: (slot as i64 + 1) * 1800,
                        title: "t".repeat(title_bytes),
                        episode_title: None,
                        episode: None,
                        synopsis: None,
                        image_url: None,
                        original_air_date: None,
                        series_id: None,
                        programme_id: None,
                        is_new: None,
                        filters: Vec::new(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[test]
fn an_oversized_guide_loses_its_furthest_programmes_and_never_a_channel() {
    let guide = guide_with(512, 40, 200);
    let raw = serde_json::to_vec(&guide).expect("guide json");
    assert!(
        raw.len() > guide::MAX_GUIDE_RESPONSE_BYTES,
        "the fixture must actually exceed the cap ({} bytes)",
        raw.len()
    );
    let clipped = guide.clipped(&GuideWindow {
        start: 0,
        end: i64::from(u32::MAX),
    });
    assert_eq!(
        clipped.channels.len(),
        512,
        "a channel must never be dropped"
    );
    assert!(
        serde_json::to_vec(&clipped).expect("clipped json").len()
            <= guide::MAX_GUIDE_RESPONSE_BYTES
    );
    assert!(
        clipped.total_programmes() < guide.total_programmes(),
        "something has to have been dropped"
    );
}

#[test]
fn clipping_keeps_only_programmes_that_touch_the_window() {
    let guide = guide_with(1, 6, 8);
    let clipped = guide.clipped(&GuideWindow {
        start: 1800,
        end: 5400,
    });
    let spans = clipped.channels[0]
        .programmes
        .iter()
        .map(|p| (p.start, p.end))
        .collect::<Vec<_>>();
    assert_eq!(spans, vec![(1800, 3600), (3600, 5400)]);
    assert_eq!(clipped.window.start, 1800);
}

#[test]
fn programmes_in_matches_the_linear_window_predicate() {
    let guide = guide_with(1, 48, 8);
    let channel = &guide.channels[0];
    for start in (-900..=90_000).step_by(733) {
        let window = GuideWindow {
            start,
            end: start + 7_777,
        };
        let expected = channel
            .programmes
            .iter()
            .filter(|programme| programme.end > window.start && programme.start < window.end)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(channel.programmes_in(&window), expected);
    }
}

#[tokio::test]
async fn a_refresh_replaces_the_guide_arc_without_changing_a_held_view() {
    let cache = GuideCache::default();
    let mut first = guide_with(1, 2, 8);
    first.channels[0].programmes[0].title = "old snapshot".to_owned();
    cache.store(4, 1, &Ok(first)).await;
    let held = cache.view(4).await.expect("first guide view");

    let mut replacement = guide_with(1, 2, 8);
    replacement.channels[0].programmes[0].title = "new snapshot".to_owned();
    cache.store(4, 2, &Ok(replacement)).await;
    let current = cache.view(4).await.expect("replacement guide view");

    assert!(!Arc::ptr_eq(&held.guide, &current.guide));
    assert_eq!(held.guide.channels[0].programmes[0].title, "old snapshot");
    assert_eq!(
        current.guide.channels[0].programmes[0].title,
        "new snapshot"
    );
}

#[tokio::test]
async fn guide_adoption_repairs_unsorted_programmes_before_binary_search() {
    let cache = GuideCache::default();
    let mut guide = guide_with(1, 3, 8);
    guide.channels[0].programmes.swap(0, 2);
    cache.store(4, 1, &Ok(guide)).await;

    let view = cache.view(4).await.expect("normalised guide");
    let starts = view.guide.channels[0]
        .programmes
        .iter()
        .map(|programme| programme.start)
        .collect::<Vec<_>>();
    assert_eq!(starts, vec![0, 1800, 3600]);
    assert_eq!(
        view.guide.channels[0]
            .programmes_in(&GuideWindow {
                start: 1700,
                end: 3700,
            })
            .len(),
        3
    );
}

#[tokio::test]
async fn the_guide_cache_serves_fresh_then_stale_then_nothing_and_a_generation_change_empties_it() {
    tokio::time::pause();
    let cache = GuideCache::default();
    let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    config.generation = 4;
    config.guide_source = GuideSource::HdHomeRun;
    let window = GuideWindow {
        start: 0,
        end: i64::from(u32::MAX),
    };

    assert_eq!(
        cache.read(&config, window.clone()).await.freshness,
        GuideFreshness::Unavailable,
        "an empty cache is unavailable, not an error"
    );

    cache.store(4, 1, &Ok(guide_with(2, 2, 8))).await;
    assert_eq!(
        cache.read(&config, window.clone()).await.freshness,
        GuideFreshness::Fresh
    );

    tokio::time::advance(guide::GUIDE_REFRESH_INTERVAL + Duration::from_secs(1)).await;
    assert_eq!(
        cache.read(&config, window.clone()).await.freshness,
        GuideFreshness::Stale,
        "past one refresh interval the answer is stale, and still served"
    );

    // A refresh that fails keeps the content and reports the reason.
    cache
        .store(
            4,
            2,
            &Err(LiveTvError::DeviceUnavailable("the tuner is asleep".into())),
        )
        .await;
    let after_failure = cache.read(&config, window.clone()).await;
    assert_eq!(after_failure.freshness, GuideFreshness::Stale);
    assert_eq!(
        after_failure.channels.len(),
        2,
        "a failed refresh keeps the cache"
    );
    assert!(after_failure.refresh_error.is_some());

    tokio::time::advance(guide::GUIDE_STALE_TTL).await;
    assert_eq!(
        cache.read(&config, window.clone()).await.freshness,
        GuideFreshness::Unavailable,
        "past the stale window a confidently wrong grid is worse than none"
    );

    cache.store(4, 3, &Ok(guide_with(2, 2, 8))).await;
    config.generation = 5;
    assert_eq!(
        cache.read(&config, window).await.freshness,
        GuideFreshness::Unavailable,
        "a settings generation change invalidates the cache it was matched against"
    );
}

fn guide_config(generation: i64) -> LiveTvConfig {
    let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    config.generation = generation;
    config.guide_source = GuideSource::HdHomeRun;
    config
}

fn whole_window() -> GuideWindow {
    GuideWindow {
        start: 0,
        end: i64::from(u32::MAX),
    }
}

#[tokio::test(start_paused = true)]
async fn a_persisted_guide_survives_a_restart_at_its_real_age() {
    // The clock starts near zero under a paused runtime, so an hour-old
    // copy has no instant to hang itself on. The age must survive that
    // anyway: reporting zero would make a deploy look like a fresh fetch
    // and hold the stale expiry open for six extra hours.
    let dir = tempfile::tempdir().expect("tempdir");
    let age = Duration::from_secs(3600);
    let first = GuideCache::with_store(dir.path().to_path_buf());
    let config = guide_config(4);
    first.store(4, 1, &Ok(guide_with(2, 2, 8))).await;
    let fetched_at = unix_seconds() - age.as_secs() as i64;
    // Rewrite the copy as if it had been written an hour ago; the file is
    // the only thing a new process sees.
    let path = dir.path().join(PERSISTED_GUIDE_FILE);
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    persisted["fetched_at"] = serde_json::json!(fetched_at);
    std::fs::write(&path, serde_json::to_vec(&persisted).expect("encode")).expect("write");

    let restarted = GuideCache::with_store(dir.path().to_path_buf());
    let served = restarted.read(&config, whole_window()).await;
    assert_eq!(
        served.freshness,
        GuideFreshness::Stale,
        "an hour-old copy is served, and says so"
    );
    assert_eq!(served.age_seconds, age.as_secs(), "the real age, not zero");
    assert_eq!(served.fetched_at, Some(fetched_at));
    assert_eq!(served.channels.len(), 2);

    // It expires at the stale window measured from the fetch, not from the
    // adoption: five more hours, not six.
    tokio::time::advance(guide::GUIDE_STALE_TTL - age - Duration::from_secs(1)).await;
    assert_eq!(
        restarted.read(&config, whole_window()).await.freshness,
        GuideFreshness::Stale,
        "one second short of the window it is still served"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        restarted.read(&config, whole_window()).await.freshness,
        GuideFreshness::Unavailable,
        "the window is measured from the fetch, so the adopted copy expires early"
    );
}

#[tokio::test]
async fn a_persisted_guide_past_the_stale_window_or_unreadable_is_not_adopted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(PERSISTED_GUIDE_FILE);
    let config = guide_config(4);

    std::fs::create_dir_all(dir.path()).expect("mkdir");
    std::fs::write(&path, b"{not json at all").expect("write");
    assert_eq!(
        GuideCache::with_store(dir.path().to_path_buf())
            .read(&config, whole_window())
            .await
            .freshness,
        GuideFreshness::Unavailable,
        "garbage on disk starts cold rather than panicking"
    );

    let written = GuideCache::with_store(dir.path().to_path_buf());
    written.store(4, 1, &Ok(guide_with(2, 2, 8))).await;
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    persisted["fetched_at"] =
        serde_json::json!(unix_seconds() - guide::GUIDE_STALE_TTL.as_secs() as i64 - 60);
    std::fs::write(&path, serde_json::to_vec(&persisted).expect("encode")).expect("write");
    assert_eq!(
        GuideCache::with_store(dir.path().to_path_buf())
            .read(&config, whole_window())
            .await
            .freshness,
        GuideFreshness::Unavailable,
        "a copy older than the stale window is not adopted"
    );

    persisted["version"] = serde_json::json!(PERSISTED_GUIDE_VERSION + 1);
    persisted["fetched_at"] = serde_json::json!(unix_seconds());
    std::fs::write(&path, serde_json::to_vec(&persisted).expect("encode")).expect("write");
    assert_eq!(
        GuideCache::with_store(dir.path().to_path_buf())
            .read(&config, whole_window())
            .await
            .freshness,
        GuideFreshness::Unavailable,
        "a version this build does not know is ignored, not migrated"
    );
}

#[tokio::test]
async fn an_invalidated_guide_leaves_nothing_on_disk_for_the_next_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = GuideCache::with_store(dir.path().to_path_buf());
    cache.store(4, 1, &Ok(guide_with(2, 2, 8))).await;
    assert!(dir.path().join(PERSISTED_GUIDE_FILE).exists());
    cache.invalidate().await;
    assert!(
        !dir.path().join(PERSISTED_GUIDE_FILE).exists(),
        "a settings change discards the copy a restart would otherwise adopt"
    );
}

#[tokio::test]
async fn the_guide_document_says_when_the_owner_comes_back() {
    let cache = GuideCache::default();
    let config = guide_config(4);
    let next = unix_seconds() + 60;

    cache.note_next_refresh(next).await;
    let unavailable = cache.read(&config, whole_window()).await;
    assert_eq!(unavailable.freshness, GuideFreshness::Unavailable);
    assert_eq!(
        unavailable.next_refresh_at,
        Some(next),
        "an unavailable answer still says when asking again is worthwhile"
    );

    cache.store(4, 1, &Ok(guide_with(2, 2, 8))).await;
    assert_eq!(
        cache.read(&config, whole_window()).await.next_refresh_at,
        Some(next)
    );
}

#[test]
fn a_skipped_tick_on_the_owner_comes_back_in_a_minute_not_twenty() {
    assert_eq!(
        guide_skip_delay(true),
        guide::GUIDE_COLD_LINEUP_RETRY,
        "an owner that is merely not admitted yet is seconds from admitted"
    );
    assert_eq!(
        guide_skip_delay(false),
        guide::GUIDE_REFRESH_INTERVAL,
        "no source: only a settings save changes that, and a save wakes the loop"
    );
}

#[test]
fn an_ingress_forgets_an_unavailable_answer_quickly_and_a_real_one_by_the_owners_clock() {
    let now = 1_789_000_000;
    let mut unavailable = LiveTvGuide::unavailable(GuideSource::HdHomeRun, whole_window(), None);
    unavailable.next_refresh_at = Some(now + 1200);
    assert_eq!(
        relay_guide_memory(&unavailable, now),
        RELAY_UNAVAILABLE_GUIDE_MEMORY
    );

    let mut fresh = guide_with(1, 1, 8);
    fresh.next_refresh_at = None;
    assert_eq!(relay_guide_memory(&fresh, now), RELAY_GUIDE_MEMORY);

    fresh.next_refresh_at = Some(now + 5);
    assert_eq!(
        relay_guide_memory(&fresh, now),
        Duration::from_secs(5),
        "an ingress never holds an answer past the moment a newer one exists"
    );

    fresh.next_refresh_at = Some(now - 30);
    assert_eq!(
        relay_guide_memory(&fresh, now),
        Duration::ZERO,
        "an owner already past its refresh is asked again immediately"
    );

    fresh.next_refresh_at = Some(now + 100_000);
    assert_eq!(
        relay_guide_memory(&fresh, now),
        RELAY_GUIDE_MEMORY,
        "the fan-out bound still applies to an owner that is far from refreshing"
    );
}

#[tokio::test]
async fn a_settings_generation_change_wakes_the_guide_loop_once() {
    let cache = GuideCache::default();
    // The first observation is construction, not a change.
    cache.observe_generation(4);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), cache.wake.notified())
            .await
            .is_err(),
        "seeing a generation for the first time is not an event"
    );
    cache.observe_generation(4);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), cache.wake.notified())
            .await
            .is_err(),
        "the same generation twice is not an event"
    );
    cache.observe_generation(5);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), cache.wake.notified())
            .await
            .is_ok(),
        "a settings save is the one skip reason that does not resolve itself"
    );
}

#[tokio::test]
async fn a_lineup_that_fills_from_cold_wakes_the_guide_loop_and_a_warm_one_does_not() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let cache = SnapshotCache::with_wake(Arc::clone(&wake));

    cache
        .get_or_refresh(4, true, || async { Ok(cache_test_snapshot(4)) })
        .await
        .expect("cold");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), wake.notified())
            .await
            .is_ok(),
        "the first lineup for a generation is the event the loop was sleeping through"
    );

    cache
        .get_or_refresh(4, true, || async { Ok(cache_test_snapshot(4)) })
        .await
        .expect("warm");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), wake.notified())
            .await
            .is_err(),
        "a lineup that was already there is not a wake"
    );

    cache
        .get_or_refresh(5, true, || async { Ok(cache_test_snapshot(5)) })
        .await
        .expect("new generation");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), wake.notified())
            .await
            .is_ok(),
        "a new generation starts cold again"
    );
}

#[tokio::test]
async fn an_older_guide_completion_cannot_replace_a_newer_publication() {
    let cache = GuideCache::default();
    let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    config.generation = 4;
    config.guide_source = GuideSource::HdHomeRun;
    assert!(cache.store(4, 2, &Ok(guide_with(2, 1, 8))).await);
    assert!(!cache.store(4, 1, &Ok(guide_with(1, 1, 8))).await);

    let served = cache
        .read(
            &config,
            GuideWindow {
                start: 0,
                end: i64::from(u32::MAX),
            },
        )
        .await;
    assert_eq!(served.matched_channels, 2);
}

#[tokio::test]
async fn a_config_change_cancels_the_active_guide_refresh() {
    let cache = GuideCache::default();
    let mut started = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    started.generation = 4;
    let cancelled = cache.begin_refresh(&started);
    let mut changed = started.clone();
    changed.generation = 5;
    cache.cancel_if_config_changed(&changed);
    assert!(cancelled.is_cancelled());
}

#[tokio::test]
async fn abandoned_blocking_guide_work_retains_refresh_admission() {
    let cache = GuideCache::default();
    let permit = Arc::new(
        Arc::clone(&cache.refresh_admission)
            .try_acquire_owned()
            .expect("first guide refresh"),
    );
    let parser_permit = Arc::clone(&permit);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let parser = tokio::task::spawn_blocking(move || {
        let _permit = parser_permit;
        let _ = started_tx.send(());
        release_rx.recv().expect("release parser");
    });
    started_rx.await.expect("parser started");
    drop(permit);
    assert!(Arc::clone(&cache.refresh_admission)
        .try_acquire_owned()
        .is_err());
    release_tx.send(()).expect("stop parser");
    parser.await.expect("parser stopped");
    assert!(Arc::clone(&cache.refresh_admission)
        .try_acquire_owned()
        .is_ok());
}

#[test]
fn guide_settings_are_validated_structurally_and_never_on_whether_the_source_answers() {
    let base = |source: GuideSource, url: &str, hours: u16| {
        let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
        config.guide_source = source;
        config.xmltv_url = url.to_owned();
        config.guide_hours = hours;
        config
    };
    assert!(base(GuideSource::Off, "", 24).validate_guide().is_ok());
    assert!(base(GuideSource::HdHomeRun, "", 24)
        .validate_guide()
        .is_ok());
    assert!(base(GuideSource::Xmltv, "", 24).validate_guide().is_err());
    assert!(base(GuideSource::Xmltv, "https://x.invalid/g.xml", 24)
        .validate_guide()
        .is_ok());
    // An unreachable host is not a validation failure: it is a thing the
    // Developer card says and the operator decides about.
    assert!(base(GuideSource::Xmltv, "http://192.0.2.1/g.xml", 4)
        .validate_guide()
        .is_ok());
    assert!(base(GuideSource::Xmltv, "file:///etc/passwd", 24)
        .validate_guide()
        .is_err());
    assert!(
        base(GuideSource::Xmltv, "https://user:pw@x.invalid/g.xml", 24)
            .validate_guide()
            .is_err()
    );
    assert!(base(GuideSource::HdHomeRun, "", 3)
        .validate_guide()
        .is_err());
    assert!(
        base(GuideSource::HdHomeRun, "", 336)
            .validate_guide()
            .is_ok(),
        "a fortnight is the horizon a series recording rule needs"
    );
    assert!(base(GuideSource::HdHomeRun, "", 337)
        .validate_guide()
        .is_err());
}

#[test]
fn xmltv_text_survives_entities_and_cdata_because_real_grabbers_emit_both() {
    let lineup = guide_lineup();
    let body = br#"<tv>
          <channel id="c1"><display-name>7.1</display-name></channel>
          <programme start="20260907200000 -0400" stop="20260907203000 -0400" channel="c1">
            <title>Tom &amp; Jerry</title>
            <sub-title><![CDATA[Cat & Mouse: "the chase"]]></sub-title>
            <desc>Fast &lt;and&gt; loud &#8212; very.</desc>
            <episode-num system="onscreen">S00E05</episode-num>
          </programme>
        </tv>"#;
    let channels = guide::parse_xmltv(body, &lineup).expect("xmltv document");
    let row = &channels[0].programmes[0];
    assert_eq!(
        row.title, "Tom & Jerry",
        "an entity is its own event; reading only text events truncated the title at it"
    );
    assert_eq!(
        row.episode_title.as_deref(),
        Some("Cat & Mouse: \"the chase\""),
        "CDATA is character data too, and was being dropped entirely"
    );
    assert_eq!(row.synopsis.as_deref(), Some("Fast <and> loud — very."));
    assert_eq!(
        row.episode.as_deref(),
        Some("S0E5"),
        "S00E05 is how every source spells a special; trimming zeros first dropped it"
    );
}

#[test]
fn text_bounds_are_the_bytes_the_contract_states_not_characters() {
    // The contract's limits are byte limits, and the guide host is
    // untrusted: 256 CJK characters is 768 bytes.
    let wide = "\u{6f22}".repeat(400);
    let bounded = guide::bounded_text(Some(wide), 256).expect("some text survives");
    assert!(bounded.len() <= 256, "bounded by bytes, not characters");
    assert!(
        bounded.chars().all(|c| c == '\u{6f22}'),
        "and never split mid-character"
    );
}

#[test]
fn a_guide_far_over_the_cap_is_trimmed_rather_than_emptied() {
    // Headroom that scaled with the overshoot asked to drop nine times the
    // cap's worth of rows and returned an empty grid — the one outcome the
    // clip exists to prevent.
    let huge = guide_with(200, 120, 700);
    let clipped = huge.clipped(&GuideWindow {
        start: 0,
        end: i64::from(u32::MAX),
    });
    let encoded = serde_json::to_vec(&clipped).expect("json").len();
    assert!(encoded <= guide::MAX_GUIDE_RESPONSE_BYTES);
    assert_eq!(
        clipped.channels.len(),
        200,
        "channels are never dropped, only the furthest-out programmes"
    );
    assert!(
        clipped.total_programmes() > 0,
        "a guide well over the cap still answers with a readable grid"
    );
}

#[test]
fn an_xmltv_url_may_reach_the_lan_but_not_this_machine_or_link_local_space() {
    // The LAN is the point: the plan's own acceptance puts the XMLTV
    // document on another node.
    for allowed in [
        "http://10.42.4.7:8080/guide.xml",
        "http://10.1.2.3/xmltv.gz",
        "https://guide.example.com/xmltv.xml",
        "http://[2001:db8::5]/g.xml",
    ] {
        assert!(
            validate_xmltv_url(allowed).is_ok(),
            "{allowed} is a destination an operator legitimately has"
        );
    }
    // Admin of plurx must not widen into reading this node's own
    // unauthenticated internal listeners, or cloud metadata.
    for refused in [
        "http://127.0.0.1:3000/_internal/v1/live-tv/snapshot",
        "http://127.1/x.xml",
        "http://0.0.0.0/x.xml",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/x.xml",
        "http://[::ffff:127.0.0.1]/x.xml",
        "http://[fe80::1]/x.xml",
        "http://255.255.255.255/x.xml",
        "http://239.1.1.1/x.xml",
    ] {
        assert!(
            validate_xmltv_url(refused).is_err(),
            "{refused} points at this machine or at link-local space"
        );
    }
}

#[tokio::test]
async fn a_name_that_resolves_to_loopback_is_refused_at_fetch_time() {
    // The literal check cannot see through a name, so the fetch resolves
    // first and refuses on the answer.
    let url = validate_xmltv_url("http://localhost:8080/guide.xml").expect("a name parses");
    let error = resolve_permitted_addrs(&url)
        .await
        .expect_err("localhost is this machine");
    assert!(matches!(error, LiveTvError::InvalidConfig(_)));

    // A literal needs no resolution and pins nothing.
    let literal = validate_xmltv_url("http://10.42.4.7/guide.xml").expect("a literal parses");
    assert!(resolve_permitted_addrs(&literal)
        .await
        .expect("a permitted literal")
        .is_empty());
}

#[tokio::test]
async fn a_refresh_with_no_cached_lineup_refuses_rather_than_storing_an_empty_guide() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(manager.as_ref()).await;
    let mut config = manager.config().await.expect("config");
    config.guide_source = GuideSource::Xmltv;
    config.xmltv_url = "http://10.42.4.7/guide.xml".to_owned();
    config.owner_node_id = manager.node_id.clone();

    // A good guide is cached and the lineup cache is cold — the state a
    // freshly restarted owner is in before anyone opens Live TV.
    manager
        .guide_cache
        .store(config.generation, 1, &Ok(guide_with(2, 2, 8)))
        .await;

    let error = manager
        .refresh_guide(&config, false)
        .await
        .expect_err("a cold lineup is not something to fetch against");
    assert!(
        format!("{error}").contains("lineup"),
        "the operator is told what is missing, not that the source failed"
    );

    // And crucially the good guide survived: fetching anyway matched
    // nothing and stored the empty result over it.
    let window = GuideWindow {
        start: 0,
        end: i64::from(u32::MAX),
    };
    let served = manager.guide_cache.read(&config, window).await;
    assert_eq!(served.channels.len(), 2);
    assert_eq!(served.matched_channels, 2);
}

#[tokio::test]
async fn a_guide_never_carries_deviceauth_into_anything_that_leaves_the_owner() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(manager.as_ref()).await;
    let mut config = manager.config().await.expect("config");
    config.guide_source = GuideSource::HdHomeRun;

    let secret = "SECRET-DEVICE-AUTH-VALUE";

    // The credential is genuinely in flight on the real request path.
    // Asserting that first is what makes every absence below mean
    // something: a test that only searched a guide the secret was never
    // put into would pass with every credential precaution deleted.
    let request = guide::guide_request_url(secret, Some("7.1"), Some(1_789_000_800))
        .expect("guide request url");
    assert!(
        request.as_str().contains(secret),
        "the credential really is a query parameter of the outbound fetch"
    );

    // And the guide under test is what that credentialed fetch produced,
    // parsed by the shipping parser, rather than a hand-built value.
    let lineup = guide_lineup();
    let body = br#"[
          {"GuideNumber":"7.1","GuideName":"WABC","Affiliate":"ABC",
           "Guide":[{"StartTime":1789000800,"EndTime":1789002600,"Title":"City Beat",
                     "Synopsis":"A rebuild."}]}
        ]"#;
    let channels =
        guide::parse_hdhomerun_guide(body, &guide_by_number(&lineup)).expect("guide document");
    let mut guide = guide_with(0, 0, 0);
    guide.matched_channels = channels.len();
    guide.lineup_channels = lineup.len();
    guide.channels = channels;
    manager
        .guide_cache
        .store(config.generation, 1, &Ok(guide))
        .await;

    let served = manager
        .local_guide(
            &config,
            GuideWindow {
                start: 0,
                end: i64::from(u32::MAX),
            },
        )
        .await;
    let encoded = serde_json::to_string(&served).expect("guide json");
    assert!(!encoded.contains(secret));
    assert!(!encoded.contains("DeviceAuth"));

    // And the credential is not reachable from the settings tuple either:
    // it is read from the device at refresh time and never written.
    let settings = manager.store.settings_snapshot().await.expect("settings");
    assert!(settings
        .values()
        .all(|value| !value.contains(secret) && !value.contains("DeviceAuth")));

    // The relay is the one path that copies a guide off this node, so it
    // must be clean for the same reason — asserted on the value the relay
    // actually holds, not on a copy of it.
    manager
        .remember_relayed_guide(config.generation, served.clone())
        .await;
    let relayed = manager
        .relayed_guide(config.generation)
        .await
        .expect("relayed guide");
    let relayed_json = serde_json::to_string(&relayed).expect("relay json");
    assert!(!relayed_json.contains(secret));
    assert!(!relayed_json.contains("DeviceAuth"));

    // Failure copy is the other way a URL escapes. Every message the fetch
    // path can produce is a fixed sentence, so a failed refresh recorded
    // against this generation says nothing about the request it made.
    manager
        .guide_cache
        .store(
            config.generation,
            2,
            &Err(LiveTvError::DeviceUnavailable(
                "HDHomeRun device request failed".to_owned(),
            )),
        )
        .await;
    let after_failure = manager
        .local_guide(
            &config,
            GuideWindow {
                start: 0,
                end: i64::from(u32::MAX),
            },
        )
        .await;
    let failure_json = serde_json::to_string(&after_failure).expect("failure json");
    assert!(!failure_json.contains(secret));
    assert!(!failure_json.contains("DeviceAuth"));
    assert!(!failure_json.contains("api.hdhomerun.com"));

    // The copy on disk is the newest way a credential could outlive the
    // process, so it is held to the same rule as the served and relayed
    // copies — and it is held to it by reading the actual bytes.
    let on_disk = manager
        .guide_cache
        .persist
        .as_ref()
        .expect("the manager keeps a copy on disk");
    let bytes = std::fs::read(on_disk).expect("persisted guide");
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains(secret), "{on_disk:?}");
    assert!(!text.contains("DeviceAuth"), "{on_disk:?}");
    assert!(!text.contains("api.hdhomerun.com"), "{on_disk:?}");
}

#[tokio::test]
async fn a_session_end_line_names_the_channel_and_nothing_that_opens_a_stream() {
    // The line an operator reads to find out what a tuner was doing. It
    // must be enough to answer that, and never enough to resume anything.
    let root = crate::test_tempdir().expect("root");
    let manager = test_manager(root.path());
    let session = register_test_session(
        &manager,
        7,
        &hex_request_id(30),
        LiveTvSessionPhase::Active,
        tokio::time::Instant::now(),
    )
    .await;
    session.cancel.cancel();

    let rendered = format!(
        "channel={} reason={} duration_s={} tuner_bytes={} user={}",
        session.channel.guide_number,
        "released",
        session.started.elapsed().as_secs(),
        session.tuner_bytes.load(Ordering::Acquire),
        session.request.user_id,
    );
    manager.retire_session(&session);

    assert!(rendered.contains("channel=7.1"));
    assert!(
        !rendered.contains(&session.capability),
        "a capability in an access-controlled log is still a capability: {rendered}"
    );
    assert!(!rendered.contains(&session.activation_token), "{rendered}");
    assert!(!rendered.contains("DeviceAuth"), "{rendered}");
    assert!(!rendered.contains("http"), "{rendered}");
    assert!(
        !rendered.contains(&session.request.request_id),
        "the request id derives a capability through replay and resume: {rendered}"
    );
}

#[tokio::test]
async fn a_refresh_publishes_the_programme_title_the_activity_page_shows() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    let now = unix_seconds();
    let mut guide = guide_with(1, 1, 8);
    guide.channels[0].guide_number = "7.1".into();
    guide.channels[0].programmes[0].start = now - 60;
    guide.channels[0].programmes[0].end = now + 600;
    guide.channels[0].programmes[0].title = "City Beat".into();
    manager.publish_guide_titles(&guide);
    let titles = Arc::clone(
        &manager
            .guide_titles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let row = titles.get("7.1").expect("channel titles");
    assert!(row
        .iter()
        .any(|(start, end, title)| *start <= now && now < *end && title == "City Beat"));
    manager.clear_guide_titles();
    assert!(manager
        .guide_titles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_empty());
}

#[tokio::test]
async fn a_refresh_is_refused_with_no_source_configured() {
    let root = crate::test_tempdir().expect("scratch root");
    let manager = test_manager(root.path());
    seed_test_config(manager.as_ref()).await;
    let config = manager.config().await.expect("config");

    assert!(matches!(
        manager.refresh_guide(&config, false).await,
        Err(LiveTvError::InvalidConfig(_)),
    ));
}

fn start_request_with_id(request_id: &str) -> LiveTvStartRequest {
    LiveTvStartRequest {
        expected_owner_node_id: "node-a".into(),
        source_node_id: "node-a".into(),
        user_id: 1,
        user_name: "viewer".into(),
        request_id: request_id.to_owned(),
        channel_id: "7.1".into(),
        config_generation: 1,
        source_serving_generation: 0,
        playback: None,
    }
}

/// The production failure, as a test. A client mints its request id from
/// sixteen random bytes, so the hex digit a UUID reserves for its version
/// is random too: fifteen ids in sixteen are not version 4. The owner
/// demanded version 4 — which held only while the server minted the id
/// itself — and refused those starts as `invalid_request`, which the web
/// renders as "Live TV could not read that request".
///
/// Every nibble, so this cannot pass by drawing a lucky id.
#[test]
fn a_client_minted_request_id_is_not_a_uuid_and_the_owner_takes_it_anyway() {
    for nibble in 0..16u8 {
        let mut id = String::from("0123456789ab");
        id.push(char::from_digit(u32::from(nibble), 16).expect("hex digit"));
        id.push_str("def0123456789abcdef");
        assert_eq!(id.len(), 32, "the spelling every client produces");
        assert!(
            valid_request_id(&id),
            "the owner must accept the id its own public surface accepted: {id}"
        );
        assert!(
            validate_start_request(&start_request_with_id(&id)).is_ok(),
            "a start whose id is not version 4 is still a start: {id}"
        );
    }
}

/// The id is 128 bits in one spelling, and the owner is the only place
/// that decides it. A hyphenated UUID is not that spelling: no client and
/// no ingress produces one, and a fixture that used one was the reason
/// the version-4 rule survived as long as it did.
#[test]
fn the_owner_refuses_every_request_id_that_is_not_thirty_two_lower_case_hex() {
    for bad in [
        "",
        &"a".repeat(31),
        &"a".repeat(33),
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde-",
        "../../etc/passwd/aaaaaaaaaaaaaaa",
        &uuid::Uuid::new_v4().to_string(),
    ] {
        assert!(
            !valid_request_id(bad),
            "not a request id, and the registry keys a slot by this: {bad:?}"
        );
        assert!(
            validate_start_request(&start_request_with_id(bad)).is_err(),
            "the owner must refuse it too: {bad:?}"
        );
    }
    assert!(
        valid_request_id(&uuid::Uuid::new_v4().simple().to_string()),
        "the id the ingress mints for a client too old to send one"
    );
}

#[test]
fn the_air_date_conversion_is_a_calendar_day_and_rejects_impossible_instants() {
    assert_eq!(
        guide::unix_to_air_date(1789000800).as_deref(),
        Some("2026-09-10")
    );
    assert_eq!(guide::unix_to_air_date(0).as_deref(), Some("1970-01-01"));
    assert_eq!(guide::unix_to_air_date(-1), None);
}

#[tokio::test]
async fn live_tv_http_writer_preserves_ts_and_fmp4_sliding_windows() {
    plurx_core::testfixtures::require_ffmpeg();
    for packaging in [LivePackaging::Mpegts, LivePackaging::Fmp4] {
        let root = crate::test_tempdir().expect("live HTTP output");
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
        let budget =
            crate::playback_cache::PlaybackCacheBudget::new(store, vec![root.path().to_path_buf()]);
        budget.configure_for_test(64 << 20);
        let sink =
            crate::scratch_put::PutSink::bind_live_tv(root.path().to_path_buf()).expect("receiver");
        let system = SystemInfo {
            ffmpeg: plurx_core::testfixtures::ffmpeg(),
            ..SystemInfo::default()
        };
        let mut delivery = test_encode_delivery(180);
        delivery.packaging = packaging;
        if packaging == LivePackaging::Fmp4 {
            delivery.output.container = "fmp4".into();
        }
        let plan = LiveTvTranscodePlan::new(&system, delivery, Some(Encoder::Software), Some(2))
            .expect("plan");
        let command = live_ffmpeg_command_for_output(
            &system,
            &plan,
            root.path(),
            LiveTvFfmpegInput::GraphProbe,
            Some(&sink.base_url(0)),
        )
        .expect("HTTP graph");
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| match argument.to_str() {
                Some("4.25") => std::ffi::OsString::from("36.25"),
                Some("testsrc2=size=1920x1080:rate=30") => {
                    std::ffi::OsString::from("testsrc2=size=320x180:rate=30")
                }
                _ => argument.to_os_string(),
            })
            .collect::<Vec<_>>();
        assert!(arguments
            .windows(2)
            .any(|pair| pair[0] == "-method" && pair[1] == "PUT"));
        assert!(arguments
            .windows(2)
            .any(|pair| pair[0] == "-http_proxy" && pair[1].is_empty()));
        assert!(arguments
            .windows(2)
            .filter(|pair| pair[0] == "-hls_flags")
            .all(|pair| !pair[1].to_string_lossy().contains("temp_file")));
        // Rebuild only the synthetic duration/raster arguments; keep the
        // production command's executable and explicit runtime environment.
        // In particular, env_clear alone loses Homebrew's PATH on macOS.
        let mut execution = tokio::process::Command::new(command.as_std().get_program());
        execution.env_clear();
        for (name, value) in command.as_std().get_envs() {
            if let Some(value) = value {
                execution.env(name, value);
            }
        }
        let output = tokio::time::timeout(
            Duration::from_secs(40),
            execution.args(arguments).kill_on_drop(true).output(),
        )
        .await
        .expect("bounded graph deadline")
        .expect("FFmpeg graph");
        sink.drain().await;
        assert!(
            output.status.success(),
            "{}",
            crate::scratch_put::redact(&String::from_utf8_lossy(&output.stderr))
        );
        assert!(sink.failure().is_none(), "{:?}", sink.failure());
        let inventory = inspect_scratch(root.path())
            .await
            .unwrap_or_else(|error| {
                let names = std::fs::read_dir(root.path())
                    .expect("diagnostic inventory")
                    .map(|entry| entry.expect("diagnostic entry").file_name())
                    .collect::<Vec<_>>();
                panic!(
                    "{packaging:?}: {error:?}; files={names:?}; stderr={}",
                    crate::scratch_put::redact(&String::from_utf8_lossy(&output.stderr))
                );
            })
            .expect("published playlist");
        assert_eq!(inventory.listed, MAX_LISTED_SEGMENTS);
        let mut count = 0;
        let mut entries = tokio::fs::read_dir(root.path())
            .await
            .expect("output directory");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            if parse_segment_name(&entry.file_name().to_string_lossy(), false).is_some() {
                count += 1;
            }
        }
        assert!(
            count <= MAX_LISTED_SEGMENTS + MAX_DELETION_LAG_SEGMENTS,
            "old segments must be deleted through the receiver"
        );
        assert!(
            inventory.media_sequence > 0,
            "playlist must advance beyond startup"
        );
        assert_eq!(inventory.init.is_some(), packaging == LivePackaging::Fmp4);
    }
}

#[tokio::test]
async fn live_tv_delete_defers_response_pins_and_retries_after_playlist_promotion() {
    let root = crate::test_tempdir().expect("live delete");
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    let budget =
        crate::playback_cache::PlaybackCacheBudget::new(store, vec![root.path().to_path_buf()]);
    budget.configure_for_test(64 << 10);
    let sink = crate::scratch_put::PutSink::bind_live_tv(root.path().to_path_buf()).expect("sink");
    let base = sink.base_url(0);
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    for sequence in 0..3 {
        assert!(client
            .put(format!("{base}/segment-{sequence:06}.ts"))
            .body(vec![sequence as u8; 128])
            .send()
            .await
            .expect("upload")
            .status()
            .is_success());
    }
    let playlist = |sequence| {
        format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{sequence}\n#EXTINF:1,\nsegment-{sequence:06}.ts\n")
    };
    assert!(client
        .put(format!("{base}/index.m3u8"))
        .body(playlist(1))
        .send()
        .await
        .expect("playlist")
        .status()
        .is_success());
    let opened = open_bounded_segment(&root.path().join("segment-000000.ts"), 128)
        .await
        .expect("response open");
    let deletion = tokio::time::timeout(
        Duration::from_secs(1),
        client.delete(format!("{base}/segment-000000.ts")).send(),
    )
    .await
    .expect("DELETE must not stall the muxer")
    .expect("delete");
    assert_eq!(deletion.status(), reqwest::StatusCode::ACCEPTED);
    assert!(root.path().join("segment-000000.ts").exists());
    drop(opened);
    assert!(client
        .put(format!("{base}/index.m3u8"))
        .body(playlist(1))
        .send()
        .await
        .expect("retry promotion")
        .status()
        .is_success());
    assert!(!root.path().join("segment-000000.ts").exists());
    // A premature FFmpeg DELETE is remembered, since FFmpeg will not retry.
    assert_eq!(
        client
            .delete(format!("{base}/segment-000001.ts"))
            .send()
            .await
            .expect("early delete")
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    let opened = open_bounded_segment(&root.path().join("segment-000001.ts"), 128)
        .await
        .expect("response open");
    assert!(client
        .put(format!("{base}/index.m3u8"))
        .body(playlist(2))
        .send()
        .await
        .expect("advance")
        .status()
        .is_success());
    assert!(
        root.path().join("segment-000001.ts").exists(),
        "promotion cannot unlink a pinned response"
    );
    drop(opened);
    assert!(client
        .put(format!("{base}/index.m3u8"))
        .body(playlist(2))
        .send()
        .await
        .expect("retry promotion")
        .status()
        .is_success());
    assert!(!root.path().join("segment-000001.ts").exists());
}
