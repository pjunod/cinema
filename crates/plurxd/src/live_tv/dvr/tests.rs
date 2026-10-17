use super::*;
use std::sync::atomic::AtomicBool;

async fn oversized_scheduler_fixture() -> (
    Arc<LiveTvManager>,
    LiveTvConfig,
    DvrConfig,
    i64,
    i64,
    tempfile::TempDir,
) {
    use plurx_core::store::{SqliteStore, UserStore as _};
    use plurx_core::transcode::{EncoderCaps, Pipeline};

    let root = tempfile::tempdir().expect("temporary scheduler root");
    let root_path = root.path();
    let sqlite = Arc::new(SqliteStore::open_in_memory().expect("scheduler store"));
    let user = sqlite
        .create_user("scheduler", "test-only", true)
        .await
        .expect("scheduler user");
    let store: Arc<dyn plurx_core::store::Store> = sqlite;
    let transcode = Arc::new(crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        root_path.join("finite"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let manager = LiveTvManager::new(
        Arc::clone(&store),
        Arc::new(super::super::SystemInfo::default()),
        transcode,
        crate::serving_fence::ServingAuthority::always_ready(),
        "node-a".to_owned(),
        root_path.join("live-tv"),
        root_path.join("guide"),
    );
    let now = 1_800_000_000;
    let target_start = now + 13 * 86_400;
    let target_title = "late programme the public clip drops";
    let guide = super::super::LiveTvGuide {
        source: "hdhomerun".to_owned(),
        freshness: super::super::GuideFreshness::Fresh,
        age_seconds: 0,
        fetched_at: Some(now),
        window: GuideWindow {
            start: now,
            end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
        },
        refresh_error: None,
        next_refresh_at: None,
        matched_channels: 64,
        lineup_channels: 64,
        channels: (0..64)
            .map(|channel| super::super::LiveTvGuideChannel {
                id: format!("{channel}.1"),
                guide_number: format!("{channel}.1"),
                affiliate: None,
                image_url: None,
                programmes: (0..700)
                    .map(|slot| {
                        let start = now + i64::from(slot) * 1_800;
                        super::super::LiveTvProgramme {
                            start,
                            end: start + 1_800,
                            title: if channel == 63 && start == target_start {
                                target_title.to_owned()
                            } else {
                                format!(
                                    "ordinary programme {channel:02}-{slot:03} {}",
                                    "x".repeat(180)
                                )
                            },
                            episode_title: None,
                            episode: None,
                            synopsis: None,
                            image_url: None,
                            original_air_date: None,
                            series_id: None,
                            programme_id: None,
                            is_new: None,
                            filters: Vec::new(),
                        }
                    })
                    .collect(),
            })
            .collect(),
    };
    manager.guide_cache.store(7, 1, &Ok(guide)).await;

    let mut live_tv = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
    live_tv.enabled = true;
    live_tv.owner_node_id = "node-a".to_owned();
    live_tv.guide_source = super::super::GuideSource::HdHomeRun;
    live_tv.guide_hours = 336;
    live_tv.generation = 7;
    let dvr = DvrConfig {
        enabled: true,
        root: root_path.join("dvr").to_string_lossy().into_owned(),
        free_floor_gb: 0,
        tuner_reserve: 0,
        pad_start_s: 0,
        pad_end_s: 0,
        reminder_lead_s: 0,
        webhook_url: String::new(),
    };
    store
        .put_dvr_rule(&plurx_core::dvr::DvrRule {
            id: "late-rule".to_owned(),
            owner_user_id: user.id,
            priority: 0,
            name: "late rule".to_owned(),
            match_mode: plurx_core::dvr::DvrMatchMode::Title,
            match_value: target_title.to_owned(),
            channel_id: Some("63.1".to_owned()),
            new_only: false,
            keep_mode: DvrKeepMode::All,
            keep_value: 0,
            pad_start_s: 0,
            pad_end_s: 0,
            enabled: true,
            created_at_ms: now * 1_000,
            updated_at_ms: now * 1_000,
        })
        .await
        .expect("store late rule");
    (manager, live_tv, dvr, now, target_start, root)
}

#[tokio::test]
async fn the_scheduler_sees_an_airing_the_public_clip_drops() {
    let (manager, live_tv, dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
    manager
        .dvr_expand(&live_tv, &dvr, now)
        .await
        .expect("expand late rule");
    let row = manager
        .store
        .get_dvr_recording_for_airing("63.1", target_start)
        .await
        .expect("read scheduled row")
        .expect("late programme scheduled from full view");
    assert_eq!(row.title, "late programme the public clip drops");

    let public = manager
        .local_guide(
            &live_tv,
            GuideWindow {
                start: now,
                end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
            },
        )
        .await;
    assert!(
        public.channels.iter().all(|channel| channel
            .programmes
            .iter()
            .all(|programme| programme.title != row.title)),
        "the fixture must prove the scheduler used data omitted by the 2 MiB response"
    );
}

#[tokio::test]
async fn already_scheduled_late_rows_survive_reconciliation_of_an_oversized_guide() {
    let (manager, live_tv, dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
    manager
        .dvr_expand(&live_tv, &dvr, now)
        .await
        .expect("expand late rule");
    manager
        .dvr_reconcile(&live_tv, live_tv.generation, now)
        .await
        .expect("reconcile late row");
    let row = manager
        .store
        .get_dvr_recording_for_airing("63.1", target_start)
        .await
        .expect("read reconciled row")
        .expect("late row retained");
    assert_eq!(row.state, DvrState::Scheduled);
}

#[tokio::test]
async fn a_late_reminder_is_not_moved_when_only_the_full_guide_contains_it() {
    let (manager, live_tv, _dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
    use plurx_core::store::keys;
    manager
        .store
        .put_settings(&[
            (keys::LIVE_TV_ENABLED, "1"),
            (keys::LIVE_TV_OWNER_NODE_ID, "node-a"),
            (keys::LIVE_TV_GUIDE_SOURCE, "hdhomerun"),
            (keys::LIVE_TV_GUIDE_HOURS, "336"),
            (keys::LIVE_TV_CONFIG_GENERATION, "7"),
        ])
        .await
        .expect("configure the reminder guide consumer");
    let owner = manager
        .store
        .list_dvr_rules()
        .await
        .expect("read fixture rule")
        .into_iter()
        .next()
        .expect("fixture rule owner")
        .owner_user_id;
    let reminder = plurx_core::dvr::DvrReminder {
        id: "late-reminder".to_owned(),
        user_id: owner,
        channel_id: "63.1".to_owned(),
        guide_number: "63.1".to_owned(),
        airing_start: target_start,
        airing_end: target_start + 1_800,
        title: "late programme the public clip drops".to_owned(),
        lead_s: 0,
        state: plurx_core::dvr::DvrReminderState::Armed,
        fired_at_ms: None,
        acked_at_ms: None,
        created_at_ms: now.saturating_mul(1_000),
        updated_at_ms: now.saturating_mul(1_000),
    };
    assert!(manager
        .store
        .put_dvr_reminder(&reminder)
        .await
        .expect("store late reminder"));

    let (events, _queue) = super::super::webhook::channel();
    reminder_tick_at(&manager, &events, now)
        .await
        .expect("reconcile reminder against full guide");

    let rows = manager
        .store
        .list_dvr_reminders(owner, None)
        .await
        .expect("read reconciled reminder");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, plurx_core::dvr::DvrReminderState::Armed);
    assert_eq!(
        live_tv.generation, 7,
        "fixture and persisted cache generation agree"
    );
}

#[derive(Clone, Default)]
struct MemoryWriter {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl tokio::io::AsyncWrite for MemoryWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl SinkWriter for MemoryWriter {
    fn sync_all<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

struct FailingWriter;

impl tokio::io::AsyncWrite for FailingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::other("injected disk failure")))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl SinkWriter for FailingWriter {
    fn sync_all<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

fn test_sink(
    id: &str,
    writer: Box<dyn SinkWriter>,
    delivered: Arc<AtomicU64>,
    metrics: Arc<LiveTvMetrics>,
) -> Arc<DvrSink> {
    test_sink_at(
        id,
        PathBuf::from(format!("/unused/{id}")),
        writer,
        delivered,
        metrics,
    )
}

fn test_sink_at(
    id: &str,
    base: PathBuf,
    writer: Box<dyn SinkWriter>,
    delivered: Arc<AtomicU64>,
    metrics: Arc<LiveTvMetrics>,
) -> Arc<DvrSink> {
    test_sink_attempt_at(id, 1, base, writer, delivered, metrics)
}

fn test_sink_attempt_at(
    id: &str,
    attempt: i64,
    base: PathBuf,
    writer: Box<dyn SinkWriter>,
    delivered: Arc<AtomicU64>,
    metrics: Arc<LiveTvMetrics>,
) -> Arc<DvrSink> {
    let now = unix_seconds();
    let sink = Arc::new(DvrSink {
        recording_id: id.to_owned(),
        channel_id: "7.1".to_owned(),
        airing_start: now,
        title: id.to_owned(),
        attempt,
        window: (now - 60, now + 60),
        base,
        writer: std::sync::Mutex::new(None),
        writer_task: std::sync::Mutex::new(None),
        bytes: AtomicU64::new(0),
        prior_attempt_bytes: None,
        observation: std::sync::Mutex::new(DvrSinkObservation::new(attempt)),
        metrics,
        cancel: CancellationToken::new(),
    });
    start_sink_writer(&sink, writer, delivered);
    sink
}

fn test_transport(
    sinks: Vec<Arc<DvrSink>>,
    _delivered: Arc<AtomicU64>,
    serving_generation: u64,
) -> Arc<DvrTransport> {
    test_transport_with(
        sinks,
        serving_generation,
        Arc::new(LiveTvMetrics::default()),
        0,
    )
}

fn test_channel() -> LiveTvChannel {
    LiveTvChannel {
        id: "7.1".to_owned(),
        guide_number: "7.1".to_owned(),
        guide_name: "Test".to_owned(),
        favorite: false,
        drm: false,
        support: super::super::LiveTvChannelSupport::Ready,
        hd: Some(true),
        video_codec: None,
        audio_codec: None,
        source_format: None,
    }
}

fn test_transport_with(
    sinks: Vec<Arc<DvrSink>>,
    serving_generation: u64,
    metrics: Arc<LiveTvMetrics>,
    seats: usize,
) -> Arc<DvrTransport> {
    let transport = DvrTransport::new(TransportInit {
        channel: test_channel(),
        generation: 1,
        owner_serving_generation: serving_generation,
        device_id: "fixture-device".to_owned(),
        address: std::net::Ipv4Addr::new(10, 42, 1, 20),
        origin: TransportOrigin::Recording,
        scratch: PathBuf::from("/unused/transport"),
        metrics,
        seats,
    });
    transport
        .sinks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(sinks);
    transport
}

fn tuner_input(
    chunks: Vec<bytes::Bytes>,
    pulled_at: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
) -> LiveTunerInput {
    use futures_util::StreamExt as _;

    let prefix = chunks[0].clone();
    let queued = chunks.get(1).cloned();
    let remainder = chunks.into_iter().skip(2).collect::<Vec<_>>();
    let stream = futures_util::stream::unfold(
        (remainder, 0usize, pulled_at),
        |(chunks, index, pulled_at)| async move {
            if index >= chunks.len() {
                return None;
            }
            tokio::task::yield_now().await;
            pulled_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(tokio::time::Instant::now());
            Some((
                Ok::<_, reqwest::Error>(chunks[index].clone()),
                (chunks, index + 1, pulled_at),
            ))
        },
    )
    .boxed();
    LiveTunerInput {
        prefix,
        queued,
        remainder: stream,
    }
}

#[tokio::test(start_paused = true)]
async fn a_slow_sink_never_delays_the_tuner_reader_or_its_sibling() {
    let metrics = Arc::new(LiveTvMetrics::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let fast_writer = MemoryWriter::default();
    let fast_bytes = Arc::clone(&fast_writer.bytes);
    let fast = test_sink(
        "fast",
        Box::new(fast_writer),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let (stalled_writer, stalled_reader) = tokio::io::duplex(1);
    let slow = test_sink(
        "slow",
        Box::new(stalled_writer),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let slow_queue_bytes = slow
        .writer
        .lock()
        .expect("slow writer queue")
        .as_ref()
        .expect("slow writer")
        .queued_bytes
        .clone();
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport(
        vec![Arc::clone(&fast), Arc::clone(&slow)],
        delivered,
        serving.admit().expect("serving generation"),
    );
    let chunk_count = 160usize;
    let chunk_bytes = 64 * 1024usize;
    let chunks = (0..chunk_count)
        .map(|index| bytes::Bytes::from(vec![(index % 251) as u8; chunk_bytes]))
        .collect::<Vec<_>>();
    let pulled_at = Arc::new(std::sync::Mutex::new(Vec::new()));

    // Virtual time measures waiting on the blocked sink without charging
    // unrelated runner CPU scheduling to the fanout. A blocking sink still
    // advances the timer and violates the latency bound.
    let started = tokio::time::Instant::now();
    let result = pump_tuner_fanout(
        tuner_input(chunks, Arc::clone(&pulled_at)),
        serving,
        transport,
        None,
    )
    .await;
    assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(slow.cancel.is_cancelled());
    assert!(!fast.cancel.is_cancelled());
    assert!(slow_queue_bytes.load(Ordering::Acquire) <= DVR_SINK_QUEUE_BYTES);
    assert_eq!(
        slow.observation
            .lock()
            .expect("slow observation")
            .reason_code
            .as_deref(),
        Some("disk_write_backlog")
    );
    let max_gap = {
        let pulls = pulled_at.lock().expect("pull timings");
        pulls
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .max()
            .unwrap_or_default()
    };
    assert!(max_gap < std::time::Duration::from_millis(100));

    assert_eq!(settle_sink(&fast).await, SinkStopResult::Settled);
    assert_eq!(
        fast_bytes.lock().expect("fast bytes").len(),
        chunk_count * chunk_bytes
    );
    drop(stalled_reader);
    assert_eq!(settle_sink(&slow).await, SinkStopResult::Settled);
    assert!(metrics
        .dvr_sink_failures_prometheus()
        .contains("reason=\"disk_write_backlog\"} 1"));
}

#[tokio::test]
async fn a_failing_sink_records_one_named_interruption() {
    let metrics = Arc::new(LiveTvMetrics::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let sink = test_sink(
        "failing",
        Box::new(FailingWriter),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport(
        vec![Arc::clone(&sink)],
        delivered,
        serving.admit().expect("serving generation"),
    );
    let pulled_at = Arc::new(std::sync::Mutex::new(Vec::new()));
    let result = pump_tuner_fanout(
        tuner_input(
            vec![
                bytes::Bytes::from_static(b"first"),
                bytes::Bytes::from_static(b"second"),
                bytes::Bytes::from_static(b"third"),
            ],
            pulled_at,
        ),
        serving,
        transport,
        None,
    )
    .await;
    assert!(
        result.is_ok(),
        "a sink-local disk failure must not become a tuner failure"
    );
    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
    let observation = sink.observation.lock().expect("failure observation");
    assert_eq!(
        observation.reason_code.as_deref(),
        Some("disk_write_failed")
    );
    assert_eq!(
        observation
            .pending_events
            .iter()
            .filter(|event| event.kind == "capture_interrupted")
            .count(),
        1
    );
    drop(observation);
    assert!(metrics
        .dvr_sink_failures_prometheus()
        .contains("reason=\"disk_write_failed\"} 1"));
}

#[tokio::test]
async fn recovery_preserves_one_durable_sink_failure_without_worker_lost() {
    let (manager, live_tv, dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
    manager
        .dvr_expand(&live_tv, &dvr, now)
        .await
        .expect("materialize recovery row");
    let row = manager
        .store
        .get_dvr_recording_for_airing("63.1", target_start)
        .await
        .expect("read recovery row")
        .expect("recovery row");
    assert!(manager
        .transition(
            &row.id,
            &[DvrState::Scheduled],
            DvrState::Recording,
            Some("test capture started"),
            DvrStatePatch::Started {
                attempt: 1,
                tuner_owner_node_id: manager.node_id.clone(),
                started_at_ms: now.saturating_mul(1_000),
                late_start_s: 0,
                path: "/unused/recovery.ts".to_owned(),
            },
            None,
        )
        .await
        .expect("claim recovery row"));

    let metrics = Arc::new(LiveTvMetrics::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let failing = test_sink_attempt_at(
        &row.id,
        1,
        PathBuf::from(format!("/unused/{}", row.id)),
        Box::new(FailingWriter),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let sibling = test_sink(
        "sibling-keeps-transport",
        Box::new(MemoryWriter::default()),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport(
        vec![Arc::clone(&failing), Arc::clone(&sibling)],
        delivered,
        serving.admit().expect("serving generation"),
    );
    manager
        .registry
        .lock()
        .expect("registry")
        .transports
        .insert("7.1".to_owned(), Arc::clone(&transport));
    let _ = pump_tuner_fanout(
        tuner_input(
            vec![
                bytes::Bytes::from_static(b"first"),
                bytes::Bytes::from_static(b"second"),
                bytes::Bytes::from_static(b"third"),
            ],
            Arc::new(std::sync::Mutex::new(Vec::new())),
        ),
        serving,
        transport,
        None,
    )
    .await;
    assert!(failing.cancel.is_cancelled());

    manager
        .drain_dvr_observation_events()
        .await
        .expect("persist precise sink failure");
    let (events, _queue) = super::super::webhook::channel();
    manager
        .dvr_recover(&live_tv, &dvr, &events, live_tv.generation, now)
        .await
        .expect("recover failed sink");

    let history = manager
        .store
        .list_dvr_events(&row.id, None, None, 100)
        .await
        .expect("read recovery history");
    let interruptions = history
        .rows
        .iter()
        .filter(|event| event.kind == "capture_interrupted")
        .collect::<Vec<_>>();
    assert_eq!(interruptions.len(), 1);
    assert_eq!(
        interruptions[0].reason_code.as_deref(),
        Some("disk_write_failed")
    );
    assert!(metrics
        .dvr_sink_failures_prometheus()
        .contains("reason=\"disk_write_failed\"} 1"));
    assert_eq!(settle_sink(&failing).await, SinkStopResult::Settled);
    assert_eq!(settle_sink(&sibling).await, SinkStopResult::Settled);
}

#[tokio::test]
async fn two_overlapping_recordings_get_identical_bytes_in_their_overlap() {
    let metrics = Arc::new(LiveTvMetrics::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let left_writer = MemoryWriter::default();
    let left_bytes = Arc::clone(&left_writer.bytes);
    let right_writer = MemoryWriter::default();
    let right_bytes = Arc::clone(&right_writer.bytes);
    let left = test_sink(
        "left",
        Box::new(left_writer),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let right = test_sink(
        "right",
        Box::new(right_writer),
        Arc::clone(&delivered),
        metrics,
    );
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport(
        vec![Arc::clone(&left), Arc::clone(&right)],
        delivered,
        serving.admit().expect("serving generation"),
    );
    let chunks = vec![
        bytes::Bytes::from_static(b"overlap-a"),
        bytes::Bytes::from_static(b"overlap-b"),
        bytes::Bytes::from_static(b"overlap-c"),
    ];
    let expected = chunks.concat();
    let result = pump_tuner_fanout(
        tuner_input(chunks, Arc::new(std::sync::Mutex::new(Vec::new()))),
        serving,
        transport,
        None,
    )
    .await;
    assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
    assert_eq!(settle_sink(&left).await, SinkStopResult::Settled);
    assert_eq!(settle_sink(&right).await, SinkStopResult::Settled);
    assert_eq!(*left_bytes.lock().expect("left bytes"), expected);
    assert_eq!(*right_bytes.lock().expect("right bytes"), expected);
}

// ---- shared transport (plan L-03 M1) ------------------------------------

fn transport_test_manager(root: &std::path::Path) -> Arc<LiveTvManager> {
    use plurx_core::store::SqliteStore;
    use plurx_core::transcode::{EncoderCaps, Pipeline};

    let store: Arc<dyn plurx_core::store::Store> =
        Arc::new(SqliteStore::open_in_memory().expect("transport store"));
    let transcode = Arc::new(crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        root.join("finite"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    LiveTvManager::new(
        store,
        Arc::new(super::super::SystemInfo::default()),
        transcode,
        crate::serving_fence::ServingAuthority::always_ready(),
        "node-a".to_owned(),
        root.join("live-tv"),
        root.join("guide"),
    )
}

/// A viewer attached the way a session attaches: a seat reserved, the
/// consumer pushed, the seat given back.
fn attach_test_viewer(
    transport: &Arc<DvrTransport>,
    name: &str,
) -> (Arc<ViewerConsumer>, ViewerFeed, CancellationToken) {
    let cancel = CancellationToken::new();
    let (viewer, feed) = ViewerConsumer::new(name.to_owned(), cancel.clone());
    assert!(transport.reserve_seat());
    assert!(transport.attach_viewer(Arc::clone(&viewer)));
    transport.release_seat();
    (viewer, feed, cancel)
}

fn seat_for(channel_id: &str) -> super::super::TransportSeat<'_> {
    super::super::TransportSeat {
        channel_id,
        device_id: "fixture-device",
        address: std::net::Ipv4Addr::new(10, 42, 1, 20),
        running_floor: 0,
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// A tuner that never stops sending, and says when its response is gone.
fn endless_tuner(dropped: Arc<AtomicBool>) -> LiveTunerInput {
    use futures_util::StreamExt as _;

    let packet = bytes::Bytes::from_static(&[0x47; 188]);
    let chunk = packet.clone();
    let stream = futures_util::stream::unfold(DropFlag(dropped), move |guard| {
        let chunk = chunk.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            Some((Ok::<_, reqwest::Error>(chunk), guard))
        }
    })
    .boxed();
    LiveTunerInput {
        prefix: packet,
        queued: None,
        remainder: stream,
    }
}

fn register(manager: &LiveTvManager, key: &str, transport: &Arc<DvrTransport>) {
    manager
        .registry
        .lock()
        .expect("registry")
        .transports
        .insert(key.to_owned(), Arc::clone(transport));
}

#[tokio::test]
async fn a_viewer_joins_a_recording_transport_without_a_slot() {
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let metrics = Arc::new(LiveTvMetrics::default());
    let sink = test_sink(
        "recording",
        Box::new(MemoryWriter::default()),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&metrics),
    );
    let recording = test_transport_with(vec![Arc::clone(&sink)], 0, metrics, 0);
    register(&manager, "7.1", &recording);
    let now = tokio::time::Instant::now();
    let decide = |channel: &str| {
        manager
            .registry
            .lock()
            .expect("registry")
            .viewer_admission(&seat_for(channel), 7, now, 1)
    };

    match decide("7.1") {
        super::super::ViewerAdmission::Join(joined) => {
            assert!(Arc::ptr_eq(&joined, &recording));
        }
        _ => panic!("a viewer on the recorded channel takes no tuner of its own"),
    }
    assert!(
        matches!(
            decide("4.1"),
            super::super::ViewerAdmission::Refuse(LiveTvError::Capacity(_))
        ),
        "another channel needs the one tuner the recording holds"
    );
    let holders = manager.transport_holders();
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].channel_id, "7.1");
    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
}

#[tokio::test]
async fn a_second_start_joins_a_transport_whose_opener_has_not_attached_yet() {
    // §2.4 D2: the opener reserves its transport in the admission's own
    // lock hold, so a second start on the channel in the window before
    // the opener's FFmpeg exists joins it. It never opens a second GET
    // and never replaces the first in the registry.
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let opening = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
    register(&manager, "7.1", &opening);
    let decision = manager.registry.lock().expect("registry").viewer_admission(
        &seat_for("7.1"),
        8,
        tokio::time::Instant::now(),
        2,
    );
    match decision {
        super::super::ViewerAdmission::Join(joined) => assert!(Arc::ptr_eq(&joined, &opening)),
        _ => panic!("a reserved transport is joined, not replaced"),
    }
}

#[tokio::test]
async fn a_join_crosses_a_settings_generation_but_not_a_device_or_serving_change() {
    // The D7 decision: a viewer at configuration generation 4 joins a
    // transport opened at 3, because the bytes do not depend on it. The
    // device (id and address) and the serving generation are what the
    // bytes do depend on, and neither may differ. Stale source facts do
    // not refuse a join: the joiner re-probes before it plans.
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let transport = DvrTransport::new(TransportInit {
        channel: test_channel(),
        generation: 3,
        owner_serving_generation: 0,
        device_id: "fixture-device".to_owned(),
        address: std::net::Ipv4Addr::new(10, 42, 1, 20),
        origin: TransportOrigin::Viewer,
        scratch: PathBuf::from("/unused/transport"),
        metrics: Arc::new(LiveTvMetrics::default()),
        seats: 0,
    });
    let (_viewer, _feed, _cancel) = attach_test_viewer(&transport, "watching");
    register(&manager, "7.1", &transport);
    let now = tokio::time::Instant::now();
    let decide = |seat: super::super::TransportSeat<'_>| {
        manager
            .registry
            .lock()
            .expect("registry")
            .viewer_admission(&seat, 7, now, 2)
    };
    assert!(matches!(
        decide(seat_for("7.1")),
        super::super::ViewerAdmission::Join(_)
    ));
    let mut other_address = seat_for("7.1");
    other_address.address = std::net::Ipv4Addr::new(10, 42, 1, 21);
    let mut other_device = seat_for("7.1");
    other_device.device_id = "another-device";
    // A transport whose loss of serving authority outlasted the grace
    // (its generation is below the running floor) is never joined.
    let mut other_authority = seat_for("7.1");
    other_authority.running_floor = 1;
    for seat in [other_address, other_device, other_authority] {
        assert!(
            matches!(
                decide(seat),
                super::super::ViewerAdmission::Refuse(LiveTvError::Capacity(_))
            ),
            "a different tuner is never joined, and never opened beside a live one"
        );
    }
    transport.mark_source_stale();
    assert!(
        matches!(
            decide(seat_for("7.1")),
            super::super::ViewerAdmission::Join(_)
        ),
        "one viewer's format change does not lock the channel for the rest of the recording"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_ready_tuner_burst_does_not_evict_a_reading_viewer() {
    let chunk_count = VIEWER_QUEUE_CHUNKS * 3;
    let chunk = bytes::Bytes::from_static(&[0x47; 188]);
    assert!(chunk_count * chunk.len() < VIEWER_QUEUE_BYTES as usize);
    assert_ready_tuner_burst_is_fair(chunk_count, chunk).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_ready_large_chunk_burst_does_not_evict_a_reading_viewer() {
    let chunk = bytes::Bytes::from(vec![0x47; 64 * 1024]);
    let chunk_count = VIEWER_QUEUE_BYTES as usize / chunk.len() + 1;
    assert!(chunk_count < VIEWER_QUEUE_CHUNKS);
    assert_ready_tuner_burst_is_fair(chunk_count, chunk).await;
}

async fn assert_ready_tuner_burst_is_fair(chunk_count: usize, chunk: bytes::Bytes) {
    use futures_util::StreamExt as _;

    let metrics = Arc::new(LiveTvMetrics::default());
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport_with(
        Vec::new(),
        serving.admit().expect("serving generation"),
        metrics,
        0,
    );
    let (reading, mut feed, _reading_cancel) = attach_test_viewer(&transport, "reading");
    let (stalled, _stalled_feed, _stalled_cancel) = attach_test_viewer(&transport, "stalled");
    let drained = tokio::spawn(async move {
        let mut total = 0;
        while let Some(bytes) = feed.rx.recv().await {
            feed.queued_bytes
                .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
            total += bytes.len();
        }
        total
    });
    let expected_bytes = chunk_count * chunk.len();
    // Unlike tuner_input, this stream adds no test-only scheduling
    // opportunity. Buffered HTTP body chunks can all be ready at once.
    let input = LiveTunerInput {
        prefix: bytes::Bytes::new(),
        queued: None,
        remainder: futures_util::stream::iter(
            (0..chunk_count).map(move |_| Ok::<_, reqwest::Error>(chunk.clone())),
        )
        .boxed(),
    };
    let result = pump_tuner_fanout(input, serving, Arc::clone(&transport), None).await;
    transport.finish(&result);
    let received = drained.await.expect("drain task");
    assert_eq!(
        reading.evicted(),
        None,
        "a runnable reader must get CPU before its chunk queue is called stalled"
    );
    assert_eq!(received, expected_bytes, "the reader receives every byte");
    assert_eq!(
        stalled.evicted(),
        Some("backlog"),
        "fair scheduling must not remove the stalled reader's chunk bound"
    );
    assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
}

#[tokio::test]
async fn a_stalled_viewer_is_evicted_and_its_sibling_continues() {
    let metrics = Arc::new(LiveTvMetrics::default());
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport_with(
        Vec::new(),
        serving.admit().expect("serving generation"),
        Arc::clone(&metrics),
        0,
    );
    let (reading, mut feed, _reading_cancel) = attach_test_viewer(&transport, "reading");
    let (stalled, _stalled_feed, _stalled_cancel) = attach_test_viewer(&transport, "stalled");
    let drained = tokio::spawn(async move {
        let mut total = 0usize;
        while let Some(bytes) = feed.rx.recv().await {
            feed.queued_bytes
                .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
            total += bytes.len();
        }
        total
    });
    let chunk_count = 160usize;
    let chunk_bytes = 64 * 1024usize;
    let chunks = (0..chunk_count)
        .map(|index| bytes::Bytes::from(vec![(index % 251) as u8; chunk_bytes]))
        .collect::<Vec<_>>();
    let pulled_at = Arc::new(std::sync::Mutex::new(Vec::new()));

    let started = std::time::Instant::now();
    let result = pump_tuner_fanout(
        tuner_input(chunks, Arc::clone(&pulled_at)),
        serving,
        Arc::clone(&transport),
        None,
    )
    .await;
    assert!(
        matches!(result, Err(LiveTvError::StreamFailed(_))),
        "the transport ends with its tuner stream, not with an eviction"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(stalled.evicted(), Some("backlog"));
    assert_eq!(reading.evicted(), None);
    assert!(!transport.cancel.is_cancelled());
    let max_gap = {
        let pulls = pulled_at.lock().expect("pull timings");
        pulls
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .max()
            .unwrap_or_default()
    };
    assert!(max_gap < std::time::Duration::from_millis(100));
    transport.finish(&result);
    assert_eq!(
        drained.await.expect("drain task"),
        chunk_count * chunk_bytes,
        "the sibling received every byte the stalled viewer missed"
    );
    let series = super::super::LiveTvMetrics::transports_prometheus(
        &super::super::LiveTvRegistry::default(),
        &metrics.consumer_evictions,
    );
    assert!(
        series.contains(
            "plurx_live_tv_consumer_evictions_total{kind=\"viewer\",reason=\"backlog\"} 1"
        ),
        "{series}"
    );
}

#[tokio::test]
async fn the_last_detach_closes_the_transport_and_releases_the_tuner() {
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport_with(
        Vec::new(),
        serving.admit().expect("serving generation"),
        Arc::new(LiveTvMetrics::default()),
        0,
    );
    let (_first, _first_feed, first) = attach_test_viewer(&transport, "first");
    let (_second, _second_feed, second) = attach_test_viewer(&transport, "second");
    register(&manager, "7.1", &transport);
    let dropped = Arc::new(AtomicBool::new(false));
    let worker = tokio::spawn(pump_tuner_fanout(
        endless_tuner(Arc::clone(&dropped)),
        serving,
        Arc::clone(&transport),
        None,
    ));
    assert!(
        transport.open_until() >= unix_seconds(),
        "a live viewer wants bytes now"
    );

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    first.cancel();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(
        !dropped.load(Ordering::Acquire),
        "one viewer left; the other still watches"
    );
    assert!(!worker.is_finished());

    second.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), worker)
        .await
        .expect("the last detach ends the reader promptly")
        .expect("reader task");
    assert!(result.is_ok());
    assert!(
        dropped.load(Ordering::Acquire),
        "the reader owned the tuner response, so its end is the tuner's release"
    );
    assert!(
        !transport.reserve_seat(),
        "a retired transport admits nobody, however late they arrive"
    );
    manager.close_transport_arc(Arc::clone(&transport)).await;
    assert!(transport.is_closed());
    assert_eq!(manager.registry.lock().expect("registry").held(), 0);
}

#[tokio::test]
async fn a_recording_keeps_the_transport_open_past_the_last_viewer() {
    let metrics = Arc::new(LiveTvMetrics::default());
    let serving = super::super::LiveTvAuthority::always_ready();
    let writer = MemoryWriter::default();
    let written = Arc::clone(&writer.bytes);
    let sink = test_sink(
        "recording",
        Box::new(writer),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&metrics),
    );
    let transport = test_transport_with(
        vec![Arc::clone(&sink)],
        serving.admit().expect("serving generation"),
        metrics,
        0,
    );
    let (_viewer, _feed, viewer) = attach_test_viewer(&transport, "viewer");
    let dropped = Arc::new(AtomicBool::new(false));
    let worker = tokio::spawn(pump_tuner_fanout(
        endless_tuner(Arc::clone(&dropped)),
        serving,
        Arc::clone(&transport),
        None,
    ));
    viewer.cancel();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !worker.is_finished(),
        "the recording's window is still open"
    );
    assert!(!dropped.load(Ordering::Acquire));
    assert_eq!(transport.open_until(), sink.window.1);

    sink.cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), worker)
        .await
        .expect("the window's end closes the transport")
        .expect("reader task");
    assert!(result.is_ok());
    assert!(dropped.load(Ordering::Acquire));
    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
    assert!(!written.lock().expect("written").is_empty());
}

#[tokio::test]
async fn closing_a_shared_transport_ends_every_viewer_and_joins_its_reader() {
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = DvrTransport::new(TransportInit {
        channel: test_channel(),
        generation: 3,
        owner_serving_generation: serving.admit().expect("serving generation"),
        device_id: "fixture-device".to_owned(),
        address: std::net::Ipv4Addr::new(10, 42, 1, 20),
        origin: TransportOrigin::Viewer,
        scratch: root.path().join("transport-probe"),
        metrics: Arc::new(LiveTvMetrics::default()),
        seats: 0,
    });
    let (_a, mut a_feed, _a_cancel) = attach_test_viewer(&transport, "a");
    let (_b, mut b_feed, _b_cancel) = attach_test_viewer(&transport, "b");
    register(&manager, "7.1", &transport);
    let dropped = Arc::new(AtomicBool::new(false));
    let worker = tokio::spawn(pump_tuner_fanout(
        endless_tuner(Arc::clone(&dropped)),
        serving,
        Arc::clone(&transport),
        None,
    ));
    *transport.worker.lock().expect("worker slot") = Some(worker);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    manager.close_all_transports().await;
    assert!(transport.is_closed());
    assert_eq!(manager.registry.lock().expect("registry").held(), 0);
    assert!(
        dropped.load(Ordering::Acquire),
        "closing joined the tuner's reader"
    );
    for feed in [&mut a_feed, &mut b_feed] {
        let ended = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while feed.rx.recv().await.is_some() {}
        })
        .await;
        assert!(ended.is_ok(), "every viewer's feed ends with the transport");
    }
}

#[tokio::test]
async fn a_recording_joining_a_viewer_transport_respects_the_reserve() {
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let metrics = Arc::new(LiveTvMetrics::default());
    let viewers = test_transport_with(Vec::new(), 0, Arc::clone(&metrics), 0);
    let (_viewer, _feed, _cancel) = attach_test_viewer(&viewers, "viewer");
    register(&manager, "2.1", &viewers);
    {
        let registry = manager.registry.lock().expect("registry");
        assert!(
            registry.may_add_recording(2, 1),
            "no recording yet: one may join the viewers' transport"
        );
    }
    let sink = test_sink(
        "recording",
        Box::new(MemoryWriter::default()),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&metrics),
    );
    let recording = test_transport_with(vec![Arc::clone(&sink)], 0, metrics, 0);
    register(&manager, "4.1", &recording);
    {
        let registry = manager.registry.lock().expect("registry");
        assert!(
            !registry.may_add_recording(2, 1),
            "a second recording transport on two tuners with one reserved is refused, \
                 even though joining needs no tuner"
        );
        assert!(!registry.may_open_transport(2, 1));
    }
    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
}

#[tokio::test]
async fn the_recording_tick_leaves_a_transport_only_viewers_hold() {
    // §2.4 D3: both closers keyed on sinks, so the DVR tick closed every
    // transport without a recording — including one people are watching.
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let transport = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 0);
    let (_viewer, _feed, viewer) = attach_test_viewer(&transport, "viewer");
    transport.publish_source(1, Ok(LiveSourceFacts::default()));
    register(&manager, "7.1", &transport);

    manager.close_finished_transports().await;
    manager.close_recordings().await;
    assert!(!transport.cancel.is_cancelled());
    assert_eq!(manager.registry.lock().expect("registry").held(), 1);
    assert!(
        manager.transport_holders().is_empty(),
        "nothing a client could stop"
    );
    let watchable = manager.watchable_channels();
    assert_eq!(
        watchable.len(),
        1,
        "but a channel a refused viewer could join"
    );
    assert_eq!(watchable[0].channel_id, "7.1");
    let metrics = manager.metrics.prometheus();
    assert!(
        metrics.contains("plurx_live_tv_transports 1\n"),
        "{metrics}"
    );
    assert!(
        metrics.contains("plurx_live_tv_transport_consumers{kind=\"viewer\"} 1\n"),
        "{metrics}"
    );

    viewer.cancel();
    manager.close_finished_transports().await;
    assert!(transport.is_closed());
    assert_eq!(manager.registry.lock().expect("registry").held(), 0);
}

#[tokio::test]
async fn a_viewer_waiting_on_a_transport_gets_its_probe_or_its_error() {
    // §2.4 D1: the transport's probe is what every viewer plans from and
    // what every sidecar records. A tuner the device refused reaches the
    // viewer as the device's own error.
    let refused = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
    let waiting = {
        let refused = Arc::clone(&refused);
        tokio::spawn(async move {
            refused
                .source_for_viewer(
                    &CancellationToken::new(),
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    refused.finish(&Err(LiveTvError::TunerUnavailable("no tuner".into())));
    assert!(matches!(
        waiting.await.expect("waiter"),
        Err(LiveTvError::TunerUnavailable(_))
    ));

    let probed = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
    let facts = LiveSourceFacts {
        video_codec: Some("mpeg2video".into()),
        width: Some(1920),
        height: Some(1080),
        ..LiveSourceFacts::default()
    };
    probed.publish_source(1, Ok(facts.clone()));
    let seen = probed
        .source_for_viewer(
            &CancellationToken::new(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .await
        .expect("probed facts");
    assert_eq!(seen, facts);
    assert_eq!(
        probed.source.lock().expect("source").clone(),
        Some(facts),
        "the facts a recording's sidecar reads are the same ones"
    );
}

/// A recording writer that keeps nothing, for tests that do not read the
/// recorded bytes.
struct DiscardWriter;

impl tokio::io::AsyncWrite for DiscardWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl SinkWriter for DiscardWriter {
    fn sync_all<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// A tuner sending 16 KiB a millisecond, so a re-probe's sample reaches
/// `SOURCE_PREFIX_BYTES` in well under `SOURCE_PREFIX_TIME`.
fn busy_tuner(dropped: Arc<AtomicBool>) -> LiveTunerInput {
    use futures_util::StreamExt as _;

    let chunk = bytes::Bytes::from(vec![0x47_u8; 16 * 1024]);
    let prefix = chunk.clone();
    let stream = futures_util::stream::unfold(DropFlag(dropped), move |guard| {
        let chunk = chunk.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            Some((Ok::<_, reqwest::Error>(chunk), guard))
        }
    })
    .boxed();
    LiveTunerInput {
        prefix,
        queued: None,
        remainder: stream,
    }
}

/// An FFprobe that prints `<root>/probe.json`, and fails while that file
/// is absent, so a test decides what each probe sees.
#[cfg(unix)]
fn scripted_ffprobe(root: &std::path::Path) -> Arc<super::super::SystemInfo> {
    let script = root.join("fake-ffprobe");
    let answer = root.join("probe.json");
    crate::write_test_executable(
        &script,
        format!(
            "#!/bin/sh\n[ -f '{0}' ] || exit 1\nexec /bin/cat '{0}'\n",
            answer.display()
        ),
        0o755,
    );
    Arc::new(super::super::SystemInfo {
        ffprobe: script.to_string_lossy().into_owned(),
        ..super::super::SystemInfo::default()
    })
}

fn probe_answer(height: u16) -> String {
    let width = if height >= 720 { 1920 } else { 720 };
    format!(
        r#"{{"streams":[{{"codec_type":"video","codec_name":"mpeg2video","width":{width},"height":{height},"field_order":"tt"}},{{"codec_type":"audio","codec_name":"ac3","channels":2}}]}}"#
    )
}

async fn viewer_facts(transport: &DvrTransport) -> Result<LiveSourceFacts, LiveTvError> {
    transport
        .source_for_viewer(
            &CancellationToken::new(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        )
        .await
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_or_stale_probe_is_reprobed_for_the_next_viewer_of_a_recording() {
    // Review of #482, finding 1, built from the reviewer's reproduction: a
    // recording's transport whose opening probe failed, with a tuner
    // free. Every later start joined it and failed at once on the stored
    // failure, and the channel was offered as "watch instead". Now each
    // viewer that finds failed, stale or old facts has the running
    // fan-out re-probe the live edge; a failed re-probe fails that viewer
    // alone; the channel is offered only while its format is known; and a
    // format change (D5) is re-probed instead of refusing every viewer
    // until the recording ends. The recording is never cut.
    let root = tempfile::tempdir().expect("root");
    let manager = transport_test_manager(root.path());
    let metrics = Arc::new(LiveTvMetrics::default());
    let serving = super::super::LiveTvAuthority::always_ready();
    let scratch = root.path().join("transport");
    std::fs::create_dir_all(&scratch).expect("transport scratch");
    let transport = DvrTransport::new(TransportInit {
        channel: test_channel(),
        generation: 1,
        owner_serving_generation: serving.admit().expect("serving generation"),
        device_id: "fixture-device".to_owned(),
        address: std::net::Ipv4Addr::new(10, 42, 1, 20),
        origin: TransportOrigin::Recording,
        scratch,
        metrics: Arc::clone(&metrics),
        seats: 0,
    });
    let sink = test_sink(
        "recording",
        Box::new(DiscardWriter),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&metrics),
    );
    transport
        .sinks
        .lock()
        .expect("sinks")
        .push(Arc::clone(&sink));
    register(&manager, "7.1", &transport);
    let dropped = Arc::new(AtomicBool::new(false));
    let worker = tokio::spawn(pump_tuner_fanout(
        busy_tuner(Arc::clone(&dropped)),
        serving,
        Arc::clone(&transport),
        Some(scripted_ffprobe(root.path())),
    ));
    transport.publish_source(
        1,
        Err(LiveTvError::StartupTimeout(
            "source_probe_incomplete: FFprobe exceeded its two-second budget".into(),
        )),
    );
    assert!(
        manager.watchable_channels().is_empty(),
        "a channel whose format is unknown is not offered as watch-instead"
    );
    let join = || {
        let decision = manager.registry.lock().expect("registry").viewer_admission(
            &seat_for("7.1"),
            7,
            tokio::time::Instant::now(),
            2,
        );
        match decision {
            super::super::ViewerAdmission::Join(joined) => {
                assert!(Arc::ptr_eq(&joined, &transport));
            }
            _ => panic!("a viewer joins the recording's transport"),
        }
    };

    // The first viewer's re-probe fails as well: it gets that probe's own
    // error, not the stored timeout.
    join();
    let failed = viewer_facts(&transport).await;
    assert!(
        matches!(failed, Err(LiveTvError::InvalidResponse(_))),
        "{failed:?}"
    );

    // The next viewer asks again, and this time the probe succeeds.
    std::fs::write(root.path().join("probe.json"), probe_answer(1080)).expect("probe answer");
    join();
    let facts = viewer_facts(&transport)
        .await
        .expect("a re-probe of the live edge");
    assert_eq!(facts.height, Some(1080));
    assert_eq!(
        manager.watchable_channels().len(),
        1,
        "a known format is offered"
    );

    // Fresh facts are shared without probing again.
    std::fs::remove_file(root.path().join("probe.json")).expect("probe answer removed");
    join();
    let shared = std::time::Instant::now();
    assert_eq!(
        viewer_facts(&transport).await.expect("fresh facts").height,
        Some(1080)
    );
    assert!(shared.elapsed() < std::time::Duration::from_millis(100));

    // D5: one viewer saw the format change. The channel is neither
    // offered nor locked: the next viewer re-probes and plans from what
    // the broadcast now is.
    transport.mark_source_stale();
    assert!(manager.watchable_channels().is_empty());
    std::fs::write(root.path().join("probe.json"), probe_answer(480)).expect("probe answer");
    join();
    assert_eq!(
        viewer_facts(&transport)
            .await
            .expect("a re-probe after the change")
            .height,
        Some(480)
    );
    assert_eq!(manager.watchable_channels().len(), 1);

    assert!(!transport.cancel.is_cancelled());
    assert!(!sink.cancel.is_cancelled(), "the recording was never cut");
    assert!(!dropped.load(Ordering::Acquire));
    transport.cancel.cancel();
    assert!(worker.await.expect("fan-out").is_ok());
    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
}

#[tokio::test]
async fn the_opener_of_a_full_rate_channel_takes_its_whole_prefix() {
    // Found while building the test for review finding 5. The held
    // prefix is about three seconds of the multiplex (7 MB at 19.4
    // Mbit/s) and went to viewers against a 4 MiB bound, so every opener
    // of a full-rate channel was evicted on its first chunk, however fast
    // its FFmpeg read. A reading opener takes the prefix and the live
    // bytes behind it, and a viewer that stops reading is still evicted.
    let metrics = Arc::new(LiveTvMetrics::default());
    let serving = super::super::LiveTvAuthority::always_ready();
    let transport = test_transport_with(
        Vec::new(),
        serving.admit().expect("serving generation"),
        Arc::clone(&metrics),
        0,
    );
    let (opener, mut feed, _opener_cancel) = attach_test_viewer(&transport, "opener");
    let (stalled, _stalled_feed, _stalled_cancel) = attach_test_viewer(&transport, "stalled");
    let prefix_bytes = 7 * 1024 * 1024;
    let live_chunks = 96;
    let chunk_bytes = 64 * 1024;
    let chunks = std::iter::once(bytes::Bytes::from(vec![0x47_u8; prefix_bytes]))
        .chain((0..live_chunks).map(|_| bytes::Bytes::from(vec![0x47_u8; chunk_bytes])))
        .collect::<Vec<_>>();
    let reader = tokio::spawn(async move {
        let mut total = 0usize;
        while let Some(bytes) = feed.rx.recv().await {
            feed.queued_bytes
                .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
            total += bytes.len();
        }
        total
    });
    let result = pump_tuner_fanout(
        tuner_input(chunks, Arc::new(std::sync::Mutex::new(Vec::new()))),
        serving,
        Arc::clone(&transport),
        None,
    )
    .await;
    assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
    assert_eq!(opener.evicted(), None, "the opener is fed its prefix");
    assert_eq!(
        stalled.evicted(),
        Some("backlog"),
        "the bound still holds past the prefix"
    );
    transport.finish(&result);
    assert_eq!(
        reader.await.expect("reader"),
        prefix_bytes + live_chunks * chunk_bytes
    );
}

#[test]
fn retirement_is_atomic_against_a_consumer_attaching() {
    // Review of #482, finding 3. A consumer attaches by pushing itself
    // under its list's lock and then giving its seat back. The retirement
    // read the lists and swapped the seat count separately, so an attach
    // and release landing between the two retired a transport that had
    // just gained a viewer — and the fan-out then returned, cutting it.
    // Race the two, many times over, and count retirements with a live
    // viewer attached.
    let rounds = 20_000;
    let mut retired_under_a_viewer = 0;
    for round in 0..rounds {
        let transport = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
        let start = Arc::new(std::sync::Barrier::new(2));
        let attached = Arc::new(AtomicBool::new(false));
        let attacher = {
            let transport = Arc::clone(&transport);
            let start = Arc::clone(&start);
            let attached = Arc::clone(&attached);
            std::thread::spawn(move || {
                let (viewer, feed) =
                    ViewerConsumer::new(format!("viewer-{round}"), CancellationToken::new());
                start.wait();
                assert!(transport.attach_viewer(viewer));
                transport.release_seat();
                attached.store(true, Ordering::Release);
                feed
            })
        };
        start.wait();
        let mut retired = false;
        loop {
            let done = attached.load(Ordering::Acquire);
            if transport.try_retire() {
                retired = true;
                break;
            }
            if done {
                break;
            }
        }
        let _feed = attacher.join().expect("attacher");
        if retired && !transport.live_viewers().is_empty() {
            retired_under_a_viewer += 1;
        }
    }
    assert_eq!(
            retired_under_a_viewer, 0,
            "a transport was retired with a viewer attached in {retired_under_a_viewer} of {rounds} rounds"
        );
}

/// A leader restart costs the owner its serving authority for a second
/// or two. Inside the session grace the transport keeps feeding every
/// recording: a replacement owner cannot claim the capture while this
/// owner's ledger lease is valid, so stopping would only cut a hole.
#[tokio::test]
async fn a_transport_keeps_writing_through_a_loss_inside_the_grace() {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let metrics = Arc::new(LiveTvMetrics::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let left_writer = MemoryWriter::default();
    let left_bytes = Arc::clone(&left_writer.bytes);
    let right_writer = MemoryWriter::default();
    let right_bytes = Arc::clone(&right_writer.bytes);
    let left = test_sink(
        "graced-left",
        Box::new(left_writer),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let right = test_sink(
        "graced-right",
        Box::new(right_writer),
        Arc::clone(&delivered),
        metrics,
    );
    let fence =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let serving = super::super::LiveTvAuthority::new(fence.authority());
    let generation = serving.admit().expect("serving generation");
    let transport = test_transport(
        vec![Arc::clone(&left), Arc::clone(&right)],
        delivered,
        generation,
    );
    // Lost, and the grace is not spent: the floor stays where it was.
    fence.validation_set_ready(false).await;
    let chunks = vec![
        bytes::Bytes::from_static(b"kept-a"),
        bytes::Bytes::from_static(b"kept-b"),
    ];
    let expected = chunks.concat();
    let result = pump_tuner_fanout(
        tuner_input(chunks, Arc::new(std::sync::Mutex::new(Vec::new()))),
        serving,
        transport,
        None,
    )
    .await;
    assert!(
        matches!(result, Err(LiveTvError::StreamFailed(_))),
        "the pump runs to the end of its input, not to a fence: {result:?}"
    );
    assert!(!left.cancel.is_cancelled(), "no recording is cancelled");
    assert_eq!(settle_sink(&left).await, SinkStopResult::Settled);
    assert_eq!(settle_sink(&right).await, SinkStopResult::Settled);
    assert_eq!(*left_bytes.lock().expect("left bytes"), expected);
    assert_eq!(*right_bytes.lock().expect("right bytes"), expected);
}

#[tokio::test]
async fn a_fenced_transport_cancels_every_sink_before_exiting() {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let metrics = Arc::new(LiveTvMetrics::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let left_writer = MemoryWriter::default();
    let left_bytes = Arc::clone(&left_writer.bytes);
    let right_writer = MemoryWriter::default();
    let right_bytes = Arc::clone(&right_writer.bytes);
    let left = test_sink(
        "fenced-left",
        Box::new(left_writer),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let right = test_sink(
        "fenced-right",
        Box::new(right_writer),
        Arc::clone(&delivered),
        metrics,
    );
    let fence =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let serving = super::super::LiveTvAuthority::new(fence.authority());
    let generation = serving.admit().expect("serving generation");
    let transport = test_transport(
        vec![Arc::clone(&left), Arc::clone(&right)],
        delivered,
        generation,
    );
    fence.validation_set_ready(false).await;
    // The loss outlasted the session grace: the authority's loop raises
    // the running floor past this transport's generation.
    assert!(serving.end_running_before(fence.authority().state().loss_generation));

    let result = pump_tuner_fanout(
        tuner_input(
            vec![
                bytes::Bytes::from_static(b"must-not-land"),
                bytes::Bytes::from_static(b"nor-this"),
            ],
            Arc::new(std::sync::Mutex::new(Vec::new())),
        ),
        serving,
        transport,
        None,
    )
    .await;
    assert!(matches!(result, Err(LiveTvError::OwnerUnavailable(_))));
    assert!(left.cancel.is_cancelled());
    assert!(right.cancel.is_cancelled());
    assert_eq!(settle_sink(&left).await, SinkStopResult::Settled);
    assert_eq!(settle_sink(&right).await, SinkStopResult::Settled);
    assert!(left_bytes.lock().expect("left bytes").is_empty());
    assert!(right_bytes.lock().expect("right bytes").is_empty());
}

#[tokio::test(start_paused = true)]
async fn stop_waits_for_the_writer_and_never_concatenates_an_unsettled_attempt() {
    let root = tempfile::tempdir().expect("temporary recording root");
    let base = root.path().join("stalled-recording");
    tokio::fs::write(attempt_path(&base, 1), b"settling")
        .await
        .expect("attempt fixture");
    let (writer, reader) = tokio::io::duplex(1);
    let sink = test_sink_at(
        "settling",
        base.clone(),
        Box::new(writer),
        Arc::new(AtomicU64::new(0)),
        Arc::new(LiveTvMetrics::default()),
    );
    let queue = sink
        .writer
        .lock()
        .expect("writer queue")
        .as_ref()
        .expect("active writer")
        .clone();
    assert!(reserve_sink_queue_bytes(&queue.queued_bytes, 64 * 1024));
    queue
        .tx
        .try_send(bytes::Bytes::from(vec![0_u8; 64 * 1024]))
        .expect("queue stalled write");
    tokio::task::yield_now().await;

    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settling);
    assert_eq!(
        sink.observation.lock().expect("settling observation").phase,
        "settling"
    );
    assert!(attempt_path(&base, 1).exists());
    assert!(!final_path(&base).exists());

    drop(reader);
    assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
}

#[tokio::test(start_paused = true)]
async fn finalization_fences_a_later_retry_and_joins_every_attempt_before_assembly() {
    let (manager, _live_tv, _dvr, _now, _target_start, root) = oversized_scheduler_fixture().await;
    let base = root.path().join("retried-recording");
    tokio::fs::write(attempt_path(&base, 1), b"attempt-one")
        .await
        .expect("first attempt fixture");
    tokio::fs::write(attempt_path(&base, 2), b"attempt-two")
        .await
        .expect("second attempt fixture");
    let delivered = Arc::new(AtomicU64::new(0));
    let metrics = Arc::new(LiveTvMetrics::default());
    let first = test_sink_attempt_at(
        "retried",
        1,
        base.clone(),
        Box::new(MemoryWriter::default()),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    assert_eq!(settle_sink(&first).await, SinkStopResult::Settled);
    let sibling = test_sink(
        "sibling",
        Box::new(MemoryWriter::default()),
        Arc::clone(&delivered),
        Arc::clone(&metrics),
    );
    let (blocked_writer, blocked_reader) = tokio::io::duplex(1);
    let second = test_sink_attempt_at(
        "retried",
        2,
        base.clone(),
        Box::new(blocked_writer),
        Arc::clone(&delivered),
        metrics,
    );
    let queue = second
        .writer
        .lock()
        .expect("second writer queue")
        .as_ref()
        .expect("active second writer")
        .clone();
    assert!(reserve_sink_queue_bytes(&queue.queued_bytes, 64 * 1024));
    queue
        .tx
        .try_send(bytes::Bytes::from(vec![0_u8; 64 * 1024]))
        .expect("queue blocked retry write");
    tokio::task::yield_now().await;
    let transport = test_transport(
        vec![
            Arc::clone(&first),
            Arc::clone(&sibling),
            Arc::clone(&second),
        ],
        delivered,
        crate::serving_fence::ServingAuthority::always_ready()
            .admit()
            .expect("serving generation"),
    );
    manager
        .registry
        .lock()
        .expect("registry")
        .transports
        .insert("7.1".to_owned(), transport);

    assert!(!manager.begin_finishing("retried", 1));
    assert_eq!(
        manager.stop_sink("retried", 1).await,
        SinkStopResult::Settling,
        "a stale row may neither cancel nor assemble the later attempt"
    );
    assert!(!second.cancel.is_cancelled());
    assert!(!final_path(&base).exists());

    assert!(manager.begin_finishing("retried", 2));
    assert_eq!(
        manager.stop_sink("retried", 2).await,
        SinkStopResult::Settling,
        "the current attempt stays unassemblable while its writer lives"
    );
    assert!(attempt_path(&base, 1).exists());
    assert!(attempt_path(&base, 2).exists());
    assert!(!final_path(&base).exists());

    drop(blocked_reader);
    assert_eq!(
        manager.stop_sink("retried", 2).await,
        SinkStopResult::Settled
    );
    assert_eq!(
        concatenate_attempts(&base, 2)
            .await
            .expect("assemble settled attempts"),
        b"attempt-oneattempt-two".len() as u64
    );
    assert_eq!(
        tokio::fs::read(final_path(&base))
            .await
            .expect("final recording"),
        b"attempt-oneattempt-two"
    );
    assert!(!attempt_path(&base, 1).exists());
    assert!(!attempt_path(&base, 2).exists());
    assert_eq!(settle_sink(&sibling).await, SinkStopResult::Settled);
}

#[test]
fn queue_byte_reservations_never_exceed_the_bound() {
    let queued = AtomicU64::new(0);
    for _ in 0..DVR_SINK_QUEUE_CHUNKS * 2 {
        let _ = reserve_sink_queue_bytes(&queued, 64 * 1024);
        assert!(queued.load(Ordering::Acquire) <= DVR_SINK_QUEUE_BYTES);
    }
    assert_eq!(queued.load(Ordering::Acquire), DVR_SINK_QUEUE_BYTES);
    assert!(!reserve_sink_queue_bytes(&queued, 1));
}

#[test]
fn write_rate_uses_successful_samples_from_one_five_second_window() {
    let mut sample = DvrSinkObservation::new(1);
    let start = std::time::Instant::now();
    sample.successful_write(start, 1_000, 100);
    assert_eq!(sample.write_bps(), None, "one write is not a rate");
    sample.successful_write(start + std::time::Duration::from_secs(1), 2_000, 1_100);
    assert_eq!(sample.write_bps(), Some(1_000));
    sample.successful_write(start + std::time::Duration::from_secs(7), 8_000, 2_100);
    assert_eq!(
        sample.write_bps(),
        None,
        "a new attempt window never reuses a sample older than five seconds"
    );
    assert_eq!(sample.first_write_at_ms, Some(1_000));
}

#[test]
fn finishing_is_latched_before_late_writes_can_relabel_the_sink() {
    let mut sample = DvrSinkObservation::new(2);
    assert_eq!(sample.phase, "reconnecting");
    sample.begin_finishing(2_000);
    sample.successful_write(std::time::Instant::now(), 2_001, 512);
    assert_eq!(sample.phase, "finishing");
    assert_eq!(
        sample.pending_events.front().map(|event| event.kind),
        Some("finishing")
    );

    sample.settling();
    sample.successful_write(std::time::Instant::now(), 2_002, 1_024);
    assert_eq!(sample.phase, "settling");
}

fn base(root: &str, title: &str, start: i64, number: &str, id: &str) -> PathBuf {
    PathBuf::from(root)
        .join(recording_folder(title))
        .join(recording_basename(title, start, number, id))
}

/// The bug this test exists for: `Path::with_extension` replaces
/// everything after the *last* dot, and a recording's basename ends
/// `… - 7.1 - abcdef01`. Building paths that way truncated at the dot
/// inside the channel number, throwing away the sub-channel and the id —
/// which are exactly what makes the name unique. Two sub-channels showing
/// a programme called `News` at six o'clock collided on one file.
#[test]
fn a_channel_number_is_not_mistaken_for_a_file_extension() {
    let seven_one = base("/20t/dvr", "News", 1_789_000_800, "7.1", "aaaaaaaa1111");
    let seven_two = base("/20t/dvr", "News", 1_789_000_800, "7.2", "bbbbbbbb2222");

    assert!(
        final_path(&seven_one)
            .to_string_lossy()
            .ends_with("7.1 - aaaaaaaa.ts"),
        "the sub-channel and the id survive: {}",
        final_path(&seven_one).display()
    );
    assert_ne!(
        final_path(&seven_one),
        final_path(&seven_two),
        "two sub-channels, one programme title, one instant"
    );
    assert_ne!(attempt_path(&seven_one, 1), attempt_path(&seven_two, 1));
    assert_ne!(sidecar_path(&seven_one), sidecar_path(&seven_two));
    assert_eq!(
        attempt_path(&seven_one, 2),
        PathBuf::from(format!("{}.a2.part", seven_one.display())),
        "an attempt is a suffix on the whole basename, never an extension swap"
    );
}

/// Every path a recording owns is the same basename plus one suffix, so
/// the sidecar is always found beside the file the row names.
#[test]
fn a_recordings_paths_are_all_siblings_of_one_basename() {
    let base = base(
        "/20t/dvr",
        "Kitchen Table",
        1_789_000_800,
        "7.1",
        "abcdef012345",
    );
    let stem = base.to_string_lossy().into_owned();
    assert_eq!(final_path(&base).to_string_lossy(), format!("{stem}.ts"));
    assert_eq!(
        sidecar_path(&base).to_string_lossy(),
        format!("{stem}.json")
    );
    assert_eq!(
        attempt_path(&base, 1).to_string_lossy(),
        format!("{stem}.a1.part")
    );
}

/// A single attempt is renamed rather than copied, and several are joined
/// in order. MPEG-TS packets concatenate, so the join is a byte copy; the
/// discontinuity is a gap the row and the sidecar name rather than hide.
#[tokio::test]
async fn attempts_are_joined_in_order_and_their_parts_removed() {
    let root = tempfile::tempdir().expect("temp root");
    let base = root
        .path()
        .join("Kitchen Table - 2026-09-10 0040 - 7.1 - abcdef01");

    tokio::fs::write(attempt_path(&base, 1), b"first")
        .await
        .expect("a1");
    let bytes = concatenate_attempts(&base, 1).await.expect("join one");
    assert_eq!(bytes, 5);
    assert_eq!(
        tokio::fs::read(final_path(&base)).await.expect("final"),
        b"first",
        "one attempt is a rename, not a copy"
    );
    assert!(!attempt_path(&base, 1).exists());

    // A second recovery: the final file from the first join is stale and
    // the parts are what count.
    tokio::fs::remove_file(final_path(&base))
        .await
        .expect("reset");
    tokio::fs::write(attempt_path(&base, 1), b"first")
        .await
        .expect("a1");
    tokio::fs::write(attempt_path(&base, 2), b"-second")
        .await
        .expect("a2");
    let bytes = concatenate_attempts(&base, 2).await.expect("join two");
    assert_eq!(bytes, 12);
    assert_eq!(
        tokio::fs::read(final_path(&base)).await.expect("final"),
        b"first-second",
        "attempts join in order, so the recording plays in order"
    );
    assert!(!attempt_path(&base, 1).exists());
    assert!(!attempt_path(&base, 2).exists());
}

/// A capture that never opened a file must not be reported as a file.
#[tokio::test]
async fn a_capture_that_wrote_nothing_joins_to_nothing() {
    let root = tempfile::tempdir().expect("temp root");
    let base = root
        .path()
        .join("Nothing - 2026-09-10 0040 - 7.1 - abcdef01");
    assert_eq!(
        concatenate_attempts(&base, 3).await.expect("join none"),
        0,
        "zero bytes is what makes `finish` call it failed rather than done"
    );
    assert!(!final_path(&base).exists());
}

/// The sidecar is what the library scan reads, so it has to carry the
/// facts the guide supplied — and say plainly when a capture is partial.
#[tokio::test]
async fn a_sidecar_says_what_the_capture_actually_got() {
    let root = tempfile::tempdir().expect("temp root");
    let base = root
        .path()
        .join("Kitchen Table - 2026-09-10 0040 - 7.1 - abcdef01");
    let mut row = crate::http::dvr::recording_from_programme(
        &crate::http::dvr::ResolvedChannel {
            id: "7.1".into(),
            guide_number: "7.1".into(),
            name: "WABC".into(),
        },
        &super::super::LiveTvProgramme {
            start: 1_789_000_800,
            end: 1_789_002_600,
            title: "Kitchen Table".into(),
            episode_title: Some("The Chase".into()),
            episode: Some("S3E14".into()),
            synopsis: Some("A synopsis.".into()),
            image_url: None,
            original_air_date: Some("2026-09-10".into()),
            series_id: Some("EP01234567".into()),
            programme_id: Some("EP012345670023".into()),
            is_new: Some(true),
            filters: Vec::new(),
        },
        plurx_core::dvr::DvrOrigin::Rule,
        Some("rule-a".into()),
        Some(1),
        60,
        120,
        0,
    );
    row.attempt = 2;

    write_sidecar(
        &base,
        &row,
        &None,
        DvrState::Partial,
        2_048,
        41,
        1_789_002_700,
    )
    .await
    .expect("sidecar");
    let parsed = plurx_core::scan::recordings::read_sidecar(&final_path(&base))
        .expect("the scan must be able to read what the engine wrote");
    let programme = parsed.programme.expect("programme");
    assert_eq!(programme.title.as_deref(), Some("Kitchen Table"));
    assert_eq!(programme.episode.as_deref(), Some("S3E14"));
    assert_eq!(parsed.state.as_deref(), Some("partial"));
    assert_eq!(parsed.gap_s, Some(41));
    assert_eq!(parsed.recording_id.as_deref(), Some(row.id.as_str()));
}

/// The webhook queue drops its *oldest* entry. A recording that just
/// started is the event someone is waiting on; an hour-old reminder
/// nobody could deliver is not.
#[test]
fn a_full_webhook_queue_drops_the_oldest_event() {
    let (sink, queue) = super::super::webhook::channel();
    let event = |id: usize| super::super::webhook::DvrEvent::RecordingFailed {
        recording_id: format!("rec-{id}"),
        title: "Kitchen Table".into(),
        reason: "test".into(),
    };
    for index in 0..plurx_core::dvr::DVR_WEBHOOK_QUEUE + 3 {
        sink.enqueue(event(index));
    }
    let mut seen = Vec::new();
    while let Some(super::super::webhook::DvrEvent::RecordingFailed { recording_id, .. }) =
        queue.take()
    {
        seen.push(recording_id);
    }
    assert_eq!(seen.len(), plurx_core::dvr::DVR_WEBHOOK_QUEUE);
    assert_eq!(
        seen.first().map(String::as_str),
        Some("rec-3"),
        "the three oldest went, not the three newest"
    );
}
