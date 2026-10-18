use super::*;

/// A dropped output-enqueue hand-off is counted by reason on /metrics; an
/// unknown reason is not folded into a real one. The counter is node-wide,
/// so the test compares before and after.
#[test]
fn output_enqueue_drops_are_counted_by_reason() {
    let sample = |reason: &str| -> u64 {
        let prefix =
            format!("plurx_transcode_output_enqueue_dropped_total{{reason=\"{reason}\"}} ");
        prometheus()
            .lines()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
            .and_then(|value| value.trim().parse().ok())
            .expect("the drop counter is exposed")
    };
    let full = sample("queue_full");
    record_output_enqueue_drop("queue_full");
    record_output_enqueue_drop("not_a_reason");
    assert!(sample("queue_full") > full);
    let _ = sample("worker_stopped");
    assert!(!prometheus().contains("reason=\"not_a_reason\""));
    assert!(prometheus().contains("# TYPE plurx_transcode_output_enqueue_dropped_total counter"));
}

/// C-08 M5 row 4: the start-outcome families render every enumerated
/// label pair and nothing else, and the production exposition carries
/// them.
#[test]
fn start_outcome_families_render_exactly_their_enumerated_labels() {
    let rendered = render_start_outcomes_for_test(|counters| {
        counters.outcomes[label_index(Some("remux"), &METHODS)][2].fetch_add(3, Ordering::Relaxed);
        counters.unpaired[0].fetch_add(1, Ordering::Relaxed);
    });
    let series = rendered
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(
        series.len(),
        METHODS.len() * START_OUTCOMES.len() + START_UNPAIRED.len()
    );
    for method in METHODS {
        for outcome in START_OUTCOMES {
            let prefix =
                format!("plurx_start_outcomes_total{{method=\"{method}\",outcome=\"{outcome}\"}} ");
            assert!(
                series.iter().any(|line| line.starts_with(&prefix)),
                "missing {prefix}"
            );
        }
    }
    assert!(series.contains(&"plurx_start_outcomes_total{method=\"remux\",outcome=\"failed\"} 3"));
    assert!(series.contains(&"plurx_start_outcomes_unpaired_total{outcome=\"ok\"} 1"));
    assert!(series.contains(&"plurx_start_outcomes_unpaired_total{outcome=\"failed\"} 0"));
    let exposition = prometheus();
    assert!(exposition.contains("# TYPE plurx_start_outcomes_total counter"));
    assert!(exposition.contains("# TYPE plurx_start_outcomes_unpaired_total counter"));
}

#[test]
fn subtitle_source_metrics_render_bounded_labels_and_bytes() {
    let metrics = SubtitleSourceMetrics::new();
    metrics.record(SubtitleSourceMetric::LookupHydrated);
    metrics.record(SubtitleSourceMetric::RequestBackground);
    metrics.record(SubtitleSourceMetric::VerdictMalformed);
    metrics.record(SubtitleSourceMetric::JobBytes(123));
    metrics.record(SubtitleSourceMetric::HydrationFetchedBytes(17));
    metrics.record(SubtitleSourceMetric::ForegroundSelfClaim);
    metrics.record(SubtitleSourceMetric::Repair);
    let rendered = metrics.render();
    assert!(rendered.contains("plurx_subtitle_source_lookups_total{outcome=\"hydrated\"} 1"));
    assert!(rendered.contains("plurx_subtitle_source_requests_total{trigger=\"background\"} 1"));
    assert!(rendered.contains("plurx_subtitle_source_verdicts_total{verdict=\"malformed\"} 1"));
    assert!(rendered.contains("plurx_subtitle_source_job_bytes_total 123"));
    assert!(
        rendered.contains("plurx_subtitle_source_hydration_bytes_total{direction=\"fetched\"} 17")
    );
    assert!(rendered.contains("plurx_subtitle_source_foreground_self_claims_total 1"));
    assert!(rendered.contains("plurx_subtitle_source_repairs_total 1"));
    assert!(!rendered.contains("node_id="));
    assert!(!rendered.contains("file_id="));
}

fn metric_value(rendered: &str, prefix: &str) -> u64 {
    rendered
        .lines()
        .find_map(|line| {
            line.strip_prefix(prefix)
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn metrics_are_recorded_with_retention_off() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
    store
        .put_setting(keys::TELEMETRY_RETAIN_DAYS, "0")
        .await
        .expect("disable retention");
    initialize(Arc::clone(&store)).await.expect("seed settings");
    let prefix = "plurx_ttff_ms_count{method=\"remux\",client=\"other\"} ";
    let before = metric_value(&prometheus(), prefix);

    emit(
        Arc::clone(&store),
        PlaybackEvent {
            event: "ttff".into(),
            method: Some("remux".into()),
            ms: Some(321),
            ..PlaybackEvent::default()
        },
    );
    tokio::time::sleep(Duration::from_millis(250)).await;

    assert_eq!(metric_value(&prometheus(), prefix), before + 1);
    assert!(store
        .playback_events(&plurx_core::domain::PlaybackEventQuery {
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("retained events")
        .is_empty());
}

fn job(event: &str, session_id: &str, level: Option<&str>) -> Job {
    let event = PlaybackEvent {
        event: event.to_owned(),
        session_id: Some(session_id.to_owned()),
        level: level.map(str::to_owned),
        ..Default::default()
    };
    Job {
        class: classify(&event),
        event,
        network: None,
    }
}

#[test]
fn durable_outcomes_and_error_levels_are_terminal() {
    for event in [
        "control_terminal",
        "control_retry_resource",
        "control_hold_withheld",
        "control_action_suppressed",
    ] {
        assert_eq!(classify(&job(event, "s", None).event), EventClass::Terminal);
    }
    assert_eq!(
        classify(&job("future_outcome", "s", Some("error")).event),
        EventClass::Terminal
    );
}

fn isolated_metrics() -> &'static QueueMetrics {
    Box::leak(Box::new(QueueMetrics::new()))
}

/// Drive the public segment handler, not only telemetry admission. A fresh
/// process contains fixture tasks and the real keyed sink registration;
/// neither the production registry nor another test's metrics are reset.
#[test]
fn hls_segment_delivery_stays_bounded_under_slow_and_failed_writer() {
    const CHILD: &str = "PLURX_C06_HANDLER_TEST_CHILD";
    const TEST: &str =
        "telemetry::tests::hls_segment_delivery_stays_bounded_under_slow_and_failed_writer";
    if std::env::var_os(CHILD).is_none() {
        struct OwnedChild(Option<std::process::Child>);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if let Some(child) = self.0.as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let mut owned = OwnedChild(Some(
            std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("isolated handler fixture"),
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(35);
        loop {
            let child = owned.0.as_mut().expect("owned child");
            if child.try_wait().expect("poll child").is_some() {
                let output = owned
                    .0
                    .take()
                    .expect("finished child")
                    .wait_with_output()
                    .expect("collect child");
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success(),
                    "fixture failed: {stdout}\n{stderr}"
                );
                assert!(
                    stdout.contains("C06 handler assertions completed"),
                    "exact fixture did not complete: {stdout}"
                );
                print!("{stdout}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "handler child exceeded 35s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        return;
    }

    struct ControlledWriter {
        block_next: AtomicBool,
        entered: Notify,
        release: Notify,
    }
    impl WriterStore for ControlledWriter {
        fn write_batch<'a>(
            &'a self,
            events: &'a [PlaybackEvent],
            _observations: &'a [NetworkPriorObservation],
        ) -> BoxFuture<'a, Result<u64, StoreError>> {
            Box::pin(async move {
                assert!(events.len() <= BATCH, "writer clone batch bound");
                if self.block_next.swap(false, Ordering::AcqRel) {
                    self.entered.notify_one();
                    tokio::time::timeout(Duration::from_secs(15), self.release.notified())
                        .await
                        .expect("controlled writer must be released");
                    return Err(StoreError::Task("injected node-local writer error".into()));
                }
                Ok(events.len() as u64)
            })
        }
        fn setting_pair(&self) -> BoxFuture<'_, Result<SettingPair, StoreError>> {
            Box::pin(async { Ok((Some("30".into()), Some("0".into()))) })
        }
    }

    async fn samples(fixture: &crate::transcode::HlsDeliveryFixture, bytes: &[u8]) -> Vec<u128> {
        use axum::extract::{Path, State};
        let mut elapsed = Vec::with_capacity(8);
        for _ in 0..8 {
            let started = std::time::Instant::now();
            tokio::time::timeout(Duration::from_millis(500), async {
                let response = crate::http::hls::segment(
                    State(fixture.state.clone()),
                    Path(("c06-pressure".to_owned(), "seg00001.m4s".to_owned())),
                    axum::http::HeaderMap::new(),
                )
                .await
                .expect("actual segment handler");
                assert_eq!(response.status(), axum::http::StatusCode::OK);
                let body = axum::body::to_bytes(response.into_body(), bytes.len() + 1)
                    .await
                    .expect("fully drain actual body");
                assert_eq!(body.as_ref(), bytes);
            })
            .await
            .expect("delivery must not await the blocked writer");
            elapsed.push(started.elapsed().as_micros());
        }
        elapsed
    }

    tokio::runtime::Builder::new_current_thread().enable_all().build()
            .expect("runtime").block_on(async {
                tokio::time::timeout(Duration::from_secs(25), async {
                    let dir = crate::test_tempdir().expect("fixture directory");
                    let fixture = crate::transcode::HlsDeliveryFixture::publish_without_process(
                        dir.path(), "c06-pressure").await;
                    fixture.make_segment_window_servable().await;
                    let bytes = vec![7_u8; 12 * 1024];
                    tokio::fs::write(dir.path().join("seg00001.m4s"), &bytes)
                        .await.expect("segment bytes");
                    let metrics = isolated_metrics();
                    let writer = Arc::new(ControlledWriter {
                        block_next: AtomicBool::new(false), entered: Notify::new(), release: Notify::new(),
                    });
                    let sink = install_test_sink(&fixture.store, writer.clone(), metrics);
                    let queue_capacity = sink.queue.locked().jobs.capacity();
                    assert_eq!(queue_capacity, QUEUE);
                    let _warm = samples(&fixture, &bytes).await;
                    let baseline = samples(&fixture, &bytes).await;

                    writer.block_next.store(true, Ordering::Release);
                    emit(Arc::clone(&fixture.store), PlaybackEvent {
                        event: "producer_pass".into(), session_id: Some("seed".into()),
                        ..PlaybackEvent::default()
                    });
                    tokio::time::timeout(Duration::from_secs(2), writer.entered.notified())
                        .await.expect("prove writer entered before pressure");
                    let inflight_capacity = sink.inflight.lock().expect("inflight mutex").as_ref()
                        .expect("writer owns inflight batch").capacity();
                    assert!(inflight_capacity <= BATCH);
                    // Distinct, bounded ordinary samples model concurrent playback
                    // telemetry pressure; they are not handler-raised terminals.
                    for index in 0..5_000 {
                        emit(Arc::clone(&fixture.store), PlaybackEvent {
                            event: "producer_pass".into(),
                            session_id: Some(format!("pressure-{index:04}")),
                            ..PlaybackEvent::default()
                        });
                    }
                    {
                    let queue = sink.queue.locked();
                    assert!(queue.jobs.len() <= QUEUE);
                    assert_eq!(queue.jobs.capacity(), queue_capacity);
                    assert!(queue.jobs.iter().all(|job| {
                        job.event.session_id.as_ref().is_some_and(|id| id.len() <= 13)
                            && job.event.extra.is_none() && job.network.is_none()
                            && serde_json::to_vec(&job.event).expect("fixture event JSON").len() <= 512
                    }), "known bounded fixture payloads");
                    }
                    assert!(metrics.dropped_queue_full.load(Ordering::Relaxed) > 0);
                    let stressed = samples(&fixture, &bytes).await;
                    assert!(sink.inflight.lock().expect("inflight mutex").is_some(), "writer still blocked");
                    assert_eq!(metrics.written_error.load(Ordering::Relaxed), 0);
                    writer.release.notify_one();
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while metrics.written_error.load(Ordering::Relaxed) == 0
                            || sink.queue.len() != 0 || sink.inflight.lock().expect("inflight mutex").is_some() {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("count error and recover the bounded queue");
                    assert!(metrics.written_ok.load(Ordering::Relaxed) > 0);
                    let recovered = samples(&fixture, &bytes).await;
                    let mut base_sorted = baseline.clone(); base_sorted.sort_unstable();
                    let mut stress_sorted = stressed.clone(); stress_sorted.sort_unstable();
                    let noise = base_sorted[7] - base_sorted[0];
                    let median_delta = stress_sorted[4].saturating_sub(base_sorted[4]);
                    // Report actual distribution/noise; do not turn host scheduling
                    // noise into a flaky correctness assertion or claim fleet latency.
                    println!("C06 wall_us baseline={baseline:?} blocked={stressed:?} recovered={recovered:?}; baseline_span_us={noise}; median_increase_us={median_delta}; within_observed_span={}", median_delta <= noise);
                    println!("C06 capacities queue={queue_capacity} inflight={inflight_capacity}; dropped={} error={} written={}", metrics.dropped_queue_full.load(Ordering::Relaxed), metrics.written_error.load(Ordering::Relaxed), metrics.written_ok.load(Ordering::Relaxed));
                    drain_sink(&sink).await;
                    assert!(sink.queue.is_closed());
                    assert_eq!(sink.queue.len(), 0);
                    assert!(sink.inflight.lock().expect("inflight mutex").is_none());
                    println!("C06 handler assertions completed");
                }).await.expect("bounded complete handler scenario");
            });
}

/// The reserve, exercised on the queue itself rather than on a predicate.
///
/// A queue holding `QUEUE - TERMINAL_RESERVE` non-terminals refuses the
/// next one and still accepts `TERMINAL_RESERVE` terminals, which is what
/// "128 slots reserved" has to mean.
#[test]
fn the_reserve_is_what_the_queue_does_not_what_a_predicate_says() {
    let queue = JobQueue::new(isolated_metrics());
    for index in 0..QUEUE - TERMINAL_RESERVE {
        assert_eq!(
            queue.push(job("producer_pass", &format!("s{index}"), None)),
            Admission::Queued
        );
    }
    assert_eq!(
        queue.push(job("producer_pass", "overflow", None)),
        Admission::Refused
    );
    assert_eq!(
        queue.push(job("stall", "overflow", None)),
        Admission::Refused,
        "lifecycle is non-terminal and stops at the same ceiling"
    );
    for index in 0..TERMINAL_RESERVE {
        assert_eq!(
            queue.push(job("control_terminal", &format!("t{index}"), None)),
            Admission::Queued
        );
    }
    assert_eq!(queue.len(), QUEUE);
}

/// The case a reserve alone does not cover.
///
/// With 896 samples and 128 terminals the queue is full, and a reserve
/// that only refuses non-terminals refuses the next terminal too -- with
/// 896 samples still queued in front of it. A terminal is the statement
/// an operator reads after a restart; a periodic sample is not. So the
/// terminal displaces the oldest sample, occupancy stays at `QUEUE`, and
/// the only queue that can turn a terminal away is one already full of
/// terminals.
#[test]
fn a_terminal_is_never_discarded_while_a_sample_is_queued() {
    let metrics = isolated_metrics();
    let queue = JobQueue::new(metrics);
    for index in 0..QUEUE - TERMINAL_RESERVE {
        assert_eq!(
            queue.push(job("producer_pass", &format!("s{index}"), None)),
            Admission::Queued
        );
    }
    for index in 0..TERMINAL_RESERVE {
        assert_eq!(
            queue.push(job("control_terminal", &format!("t{index}"), None)),
            Admission::Queued
        );
    }
    assert_eq!(queue.len(), QUEUE);

    assert_eq!(
        queue.push(job("control_terminal", "late", None)),
        Admission::Displaced,
        "a full queue must make room for a terminal, not refuse it"
    );
    assert_eq!(queue.len(), QUEUE, "and the memory bound is unchanged");
    let drained = queue.take_batch(QUEUE);
    assert_eq!(
        drained
            .iter()
            .filter(|queued| queued.event.session_id.as_deref() == Some("late"))
            .count(),
        1,
        "the late terminal is in the queue, not in the drop counter"
    );
    assert_eq!(
        drained
            .iter()
            .filter(|queued| queued.class == EventClass::Sample)
            .count(),
        QUEUE - TERMINAL_RESERVE - 1,
        "exactly one sample was displaced"
    );
    assert!(
        !drained
            .iter()
            .any(|queued| queued.event.session_id.as_deref() == Some("s0")),
        "and it was the oldest one"
    );
}

/// A queue of nothing but terminals is the only one that refuses a
/// terminal, and it still never exceeds `QUEUE`.
#[test]
fn a_queue_of_terminals_refuses_a_terminal_and_stays_bounded() {
    let queue = JobQueue::new(isolated_metrics());
    for index in 0..QUEUE {
        assert_eq!(
            queue.push(job("control_terminal", &format!("t{index}"), None)),
            Admission::Queued
        );
    }
    assert_eq!(
        queue.push(job("control_terminal", "late", None)),
        Admission::Refused
    );
    assert_eq!(queue.len(), QUEUE);
}

/// Admission is atomic with the enqueue it authorises.
///
/// Eight threads racing to fill the queue must admit EXACTLY
/// `QUEUE - TERMINAL_RESERVE` non-terminals between them. A depth
/// snapshot taken before a separate send cannot promise that: every
/// racing producer can read the same sub-reserve depth and every one of
/// them can then send, which is how the reserved slots get spent by the
/// work they are reserved from.
#[test]
fn concurrent_producers_cannot_overrun_the_reserve() {
    let queue = Arc::new(JobQueue::new(isolated_metrics()));
    let admitted = Arc::new(AtomicU64::new(0));
    std::thread::scope(|scope| {
        for thread in 0..8 {
            let queue = Arc::clone(&queue);
            let admitted = Arc::clone(&admitted);
            scope.spawn(move || {
                for index in 0..4_000 {
                    if queue.push(job("producer_pass", &format!("s{thread}-{index}"), None))
                        == Admission::Queued
                    {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    assert_eq!(
        admitted.load(Ordering::Relaxed) as usize,
        QUEUE - TERMINAL_RESERVE,
        "non-terminal admissions must stop exactly at the reserve boundary"
    );
    assert_eq!(queue.len(), QUEUE - TERMINAL_RESERVE);
    for index in 0..TERMINAL_RESERVE {
        assert_eq!(
            queue.push(job("control_terminal", &format!("t{index}"), None)),
            Admission::Queued,
            "the reserve survived the race intact"
        );
    }
}

#[test]
fn only_consecutive_samples_for_the_same_session_and_name_coalesce() {
    let before = QUEUE_METRICS.dropped_coalesced.load(Ordering::Relaxed);
    let mut jobs = vec![
        job("producer_pass", "one", None),
        job("producer_pass", "one", None),
        job("stall", "one", None),
        job("producer_pass", "one", None),
        job("producer_pass", "two", None),
        job("control_terminal", "one", None),
        job("control_terminal", "one", None),
    ];

    coalesce_samples(&mut jobs, &QUEUE_METRICS);

    assert_eq!(jobs.len(), 6);
    assert_eq!(
        QUEUE_METRICS.dropped_coalesced.load(Ordering::Relaxed),
        before + 1
    );
    assert_eq!(
        jobs.iter()
            .filter(|job| job.class == EventClass::Terminal)
            .count(),
        2
    );
}

#[tokio::test]
async fn shutdown_drain_returns_inside_its_bound() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
    initialize(Arc::clone(&store)).await.expect("seed settings");
    for index in 0..16 {
        emit(
            Arc::clone(&store),
            PlaybackEvent {
                event: "producer_pass".into(),
                session_id: Some(format!("session-{index}")),
                ..Default::default()
            },
        );
    }
    let started = Instant::now();
    drain_for_shutdown(&store).await;
    assert!(started.elapsed() <= DRAIN + Duration::from_millis(250));
}

#[tokio::test]
async fn settings_seed_precedes_the_first_event_and_local_changes_invalidate() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
    store
        .put_setting(keys::TELEMETRY_RETAIN_DAYS, "0")
        .await
        .expect("disable retention");
    store
        .put_setting(keys::PLAYBACK_NETWORK_PRIORS, "1")
        .await
        .expect("enable priors");

    initialize(Arc::clone(&store)).await.expect("seed settings");
    let sink = sink_for(Arc::clone(&store));
    let seeded = *sink.settings.read().expect("settings");
    assert!(!seeded.retain);
    assert!(seeded.priors);
    assert!(seeded.read_at.is_some());

    store
        .put_setting(keys::TELEMETRY_RETAIN_DAYS, "30")
        .await
        .expect("enable retention");
    invalidate_settings(&store);
    assert!(sink.settings.read().expect("settings").read_at.is_none());
    sink.refresh_settings(&DurableWriterStore(Arc::clone(&store)), false)
        .await
        .expect("refresh invalidated settings");
    assert!(sink.settings.read().expect("settings").retain);
}

#[tokio::test]
async fn a_fresh_cache_skips_replicated_setting_reads_for_its_window() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
    initialize(Arc::clone(&store)).await.expect("seed settings");
    let sink = sink_for(Arc::clone(&store));
    let first_read = sink.settings.read().expect("settings").read_at;

    sink.refresh_settings(&DurableWriterStore(Arc::clone(&store)), false)
        .await
        .expect("cached refresh");

    assert_eq!(sink.settings.read().expect("settings").read_at, first_read);
}

#[test]
fn playback_metrics_render_bounded_labels_and_counts() {
    let metrics = PlaybackMetrics::new();
    metrics.record(&PlaybackEvent {
        event: "ttff".into(),
        method: Some("remux".into()),
        ms: Some(684),
        ..PlaybackEvent::default()
    });
    metrics.record(&PlaybackEvent {
        event: "stall".into(),
        detail: Some("supply".into()),
        ..PlaybackEvent::default()
    });
    metrics.record(&PlaybackEvent {
        event: "stall_recovery".into(),
        detail: Some("attempt:transcode".into()),
        ..PlaybackEvent::default()
    });
    metrics.record(&PlaybackEvent {
        event: "stall_recovery".into(),
        detail: Some("recovered".into()),
        ..PlaybackEvent::default()
    });
    metrics.record(&PlaybackEvent {
        event: "suspend".into(),
        hold_reason: Some("time".into()),
        ..PlaybackEvent::default()
    });
    metrics.record(&PlaybackEvent {
        event: "resume".into(),
        ms: Some(1_500),
        ..PlaybackEvent::default()
    });
    for (event, detail) in [
        ("marker_offer", Some("intro")),
        ("marker_manual_skip", None),
        ("marker_automatic_skip", None),
        ("marker_seek_back", None),
        ("marker_prewarm", Some("hit")),
        ("marker_prewarm", Some("miss")),
    ] {
        metrics.record(&PlaybackEvent {
            event: event.to_owned(),
            detail: detail.map(str::to_owned),
            ..PlaybackEvent::default()
        });
    }
    metrics.record(&PlaybackEvent {
        event: "marker_prewarm".to_owned(),
        detail: Some("miss".to_owned()),
        method: Some("direct_play".to_owned()),
        ..PlaybackEvent::default()
    });
    let text = metrics.render();
    assert!(text.contains("plurx_ttff_ms_count{method=\"remux\",client=\"other\"} 1"));
    assert!(text.contains("plurx_ttff_ms_bucket{method=\"remux\",client=\"other\",le=\"1000\"} 1"));
    assert!(text.contains("plurx_stalls_total{kind=\"supply\"} 1"));
    assert!(text.contains("plurx_stall_recoveries_total{outcome=\"attempt\"} 1"));
    assert!(text.contains("plurx_stall_recoveries_total{outcome=\"recovered\"} 1"));
    assert!(text.contains("plurx_suspends_total{reason=\"time\"} 1"));
    assert!(text.contains("plurx_suspended_seconds_total 1.500"));
    assert!(text.contains("plurx_playback_marker_actions_total{action=\"offer\"} 1"));
    assert!(text.contains("plurx_playback_marker_actions_total{action=\"manual_skip\"} 1"));
    assert!(text.contains("plurx_playback_marker_actions_total{action=\"automatic_skip\"} 1"));
    assert!(text.contains("plurx_playback_marker_actions_total{action=\"undo_seek_back\"} 1"));
    assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"hit\"} 1"));
    assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"miss\"} 2"));
    assert!(text.contains("plurx_playback_marker_prewarm_hit_ratio 0.333333"));
    assert!(!text.contains("title="));
    assert!(!text.contains("user="));
    assert!(!text.contains("path="));
}

/// Row 1: the client class is a label, it cannot carry anything outside
/// the fixed vocabulary, and a start between 30 s and two minutes lands
/// in a real bucket instead of `+Inf`.
#[test]
fn ttff_is_labelled_by_client_class_and_has_buckets_above_thirty_seconds() {
    let metrics = PlaybackMetrics::new();
    let ttff = |ms| PlaybackEvent {
        event: "ttff".into(),
        method: Some("transcode".into()),
        ms: Some(ms),
        ..PlaybackEvent::default()
    };
    metrics.record_from(&ttff(45_000), Some("safari"));
    metrics.record_from(&ttff(110_000), Some("safari"));
    metrics.record_from(&ttff(700), Some("Mozilla/5.0 (X11) <script>"));
    let text = metrics.render();
    let line = |labels: &str| {
        text.lines()
            .find(|line| line.starts_with(&format!("plurx_ttff_ms_bucket{{{labels}}} ")))
            .unwrap_or_else(|| panic!("no bucket {labels}"))
            .rsplit(' ')
            .next()
            .expect("value")
            .to_owned()
    };
    assert_eq!(
        line(r#"method="transcode",client="safari",le="30000""#),
        "0"
    );
    assert_eq!(
        line(r#"method="transcode",client="safari",le="60000""#),
        "1"
    );
    assert_eq!(
        line(r#"method="transcode",client="safari",le="120000""#),
        "2"
    );
    assert_eq!(line(r#"method="transcode",client="safari",le="+Inf""#), "2");
    assert!(text.contains(r#"plurx_ttff_ms_count{method="transcode",client="other"} 1"#));
    assert!(!text.contains("script"));
    assert!(!text.contains("Mozilla"));
}

fn label_values(text: &str, family: &str, label: &str) -> std::collections::BTreeSet<String> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter(|line| {
            line.split(['{', ' ']).next().is_some_and(|name| {
                name == family
                    || name
                        .strip_prefix(family)
                        .is_some_and(|rest| ["_bucket", "_sum", "_count"].contains(&rest))
            })
        })
        .map(|line| {
            let labels = line
                .split_once('{')
                .and_then(|(_, rest)| rest.split_once('}'))
                .map(|(labels, _)| labels)
                .unwrap_or_else(|| panic!("{family} rendered without labels: {line}"));
            labels
                .split(',')
                .find_map(|pair| pair.strip_prefix(&format!("{label}=\"")))
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or_else(|| panic!("{family} line without {label}: {line}"))
                .to_owned()
        })
        .collect()
}

/// The release-evidence families render with exactly their enumerated
/// label values, and no others, even after events carrying values outside
/// every vocabulary.
#[test]
fn release_evidence_families_render_exactly_their_enumerated_labels() {
    let metrics = PlaybackMetrics::new();
    for event in ["seek_resumed", "seek_abandoned", "stall"] {
        metrics.record_from(
            &PlaybackEvent {
                event: event.into(),
                method: Some("/mnt/media/Some Title (2024).mkv".into()),
                detail: Some("session=abc123".into()),
                ms: Some(900),
                ..PlaybackEvent::default()
            },
            Some("user-agent-string"),
        );
    }
    let ledger = WatchLedger::default();
    let start = Instant::now();
    record_progress_beat_into(&ledger, &metrics, 1, 2, 0, Some("file_id=9"), start);
    record_progress_beat_into(
        &ledger,
        &metrics,
        1,
        2,
        5_000,
        Some("file_id=9"),
        start + Duration::from_secs(5),
    );
    metrics.record_admission_wait(AdmissionPool::EncodePermit, Duration::from_millis(40));
    metrics.delivered_bytes[label_index(Some("token=secret"), &METHODS)]
        .fetch_add(1, Ordering::Relaxed);
    let text = metrics.render();
    let set = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<std::collections::BTreeSet<_>>()
    };
    for family in [
        "plurx_seek_to_picture_ms",
        "plurx_seeks_total",
        "plurx_watched_seconds_total",
        "plurx_delivered_bytes_total",
    ] {
        assert_eq!(
            label_values(&text, family, "method"),
            set(&METHODS),
            "{family}"
        );
    }
    assert_eq!(
        label_values(&text, "plurx_stalled_seconds_total", "kind"),
        set(&STALL_KINDS)
    );
    assert_eq!(
        label_values(&text, "plurx_admission_wait_seconds", "pool"),
        set(&ADMISSION_POOLS)
    );
    assert_eq!(
        label_values(&text, "plurx_ttff_ms", "client"),
        set(&CLIENT_CLASSES)
    );
    for forbidden in [
        "/mnt",
        "session=",
        "file_id",
        "token",
        "user-agent",
        "abc123",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} leaked into the exposition"
        );
    }
    // The hostile values folded into the fixed fallbacks.
    assert!(text.contains(r#"plurx_seeks_total{method="unknown"} 2"#));
    assert!(text.contains(r#"plurx_seek_to_picture_ms_count{method="unknown"} 1"#));
    assert!(text.contains(r#"plurx_seek_to_picture_ms_bucket{method="unknown",le="1000"} 1"#));
    assert!(text.contains(r#"plurx_stalled_seconds_total{kind="other"} 0.900"#));
    assert!(text.contains(r#"plurx_watched_seconds_total{method="unknown"} 5.000"#));
    assert!(text.contains(r#"plurx_delivered_bytes_total{method="unknown"} 1"#));
    assert!(
        text.contains(r#"plurx_admission_wait_seconds_bucket{pool="encode_permit",le="0.05"} 1"#)
    );
    assert!(text.contains(r#"plurx_admission_wait_seconds_count{pool="encode_permit"} 1"#));
}

/// Row 3's numerator and denominator move together on one synthetic
/// session: ten minutes of five-second beats and three stalls, one of
/// which the stall kind vocabulary does not know.
#[test]
fn stalled_and_watched_seconds_move_together_on_a_synthetic_session() {
    let metrics = PlaybackMetrics::new();
    let ledger = WatchLedger::default();
    let start = Instant::now();
    for beat in 0..=120u64 {
        record_progress_beat_into(
            &ledger,
            &metrics,
            7,
            70,
            i64::try_from(beat * 5_000).expect("position"),
            Some("remux"),
            start + Duration::from_secs(beat * 5),
        );
    }
    for (detail, ms) in [
        ("supply", 1_200),
        ("decode", 800),
        ("state=recovered", 3_000),
    ] {
        metrics.record_from(
            &PlaybackEvent {
                event: "stall".into(),
                method: Some("remux".into()),
                detail: Some(detail.into()),
                ms: Some(ms),
                ..PlaybackEvent::default()
            },
            None,
        );
    }
    let text = metrics.render();
    assert!(text.contains(r#"plurx_watched_seconds_total{method="remux"} 600.000"#));
    assert!(text.contains(r#"plurx_stalled_seconds_total{kind="supply"} 1.200"#));
    assert!(text.contains(r#"plurx_stalled_seconds_total{kind="decode"} 0.800"#));
    assert!(text.contains(r#"plurx_stalled_seconds_total{kind="other"} 3.000"#));
    assert!(text.contains(r#"plurx_stalls_total{kind="supply"} 1"#));
    // Row 8's numerator is pinned where it is fed: the direct-play
    // handler (`http::tests::a_direct_play_get_credits_exactly_its_body_to_delivered_bytes`),
    // `meter::tests` and one assertion per meter construction site.
}

/// The web reports a long wait while the picture is still frozen, at its
/// eight-second mark, and the rest when the wait ends (`stall_end`, `ms`
/// being only the time after the first report). Row 3 credits both, so a
/// 90 s stall is 90 s, not 8; and it is still one stall.
#[test]
fn a_stall_reported_while_frozen_is_credited_its_whole_length_once() {
    let metrics = PlaybackMetrics::new();
    for (event, ms) in [("stall", Some(8_000)), ("stall_end", Some(82_000))] {
        metrics.record_from(
            &PlaybackEvent {
                event: event.into(),
                method: Some("remux".into()),
                detail: Some("supply-persistent".into()),
                ms,
                ..PlaybackEvent::default()
            },
            None,
        );
    }
    // A close with nothing after the report adds nothing.
    metrics.record_from(
        &PlaybackEvent {
            event: "stall_end".into(),
            detail: Some("decode-persistent".into()),
            ..PlaybackEvent::default()
        },
        None,
    );
    let text = metrics.render();
    assert!(
        text.contains(r#"plurx_stalled_seconds_total{kind="supply"} 90.000"#),
        "{text}"
    );
    assert!(text.contains(r#"plurx_stalls_total{kind="supply"} 1"#));
    assert!(text.contains(r#"plurx_stalls_total{kind="decode"} 0"#));
    assert!(text.contains(r#"plurx_stalled_seconds_total{kind="decode"} 0.000"#));
    assert_eq!(
        classify(&PlaybackEvent {
            event: "stall_end".into(),
            ..PlaybackEvent::default()
        }),
        EventClass::Lifecycle,
        "the close of a stall is kept like the stall, never sampled away"
    );
}

/// A viewer whose beats alternate between two nodes (no cookie holds them
/// to one) is credited wall time once, not once per node: each node
/// takes the durable row another node wrote as the previous beat. And a
/// node alone, with its own coalesced writes in the row, still uses its
/// own exact baseline.
#[test]
fn beats_alternating_between_nodes_are_credited_once_cluster_wide() {
    use plurx_core::domain::WatchState;
    let base = Instant::now();
    // Half a second past a whole second, so the row's truncated
    // `updated_at` is exercised.
    let wall0: i64 = 1_790_000_000_500;
    let nodes = [WatchLedger::default(), WatchLedger::default()];
    let mut row: Option<WatchState> = None;
    let mut credited = 0;
    for beat in 0..=60i64 {
        let seconds = beat * 10;
        let position = 100_000 + seconds * 1_000;
        let wall = wall0 + seconds * 1_000;
        let node = &nodes[usize::try_from(beat % 2).expect("node")];
        credited += node.beat_after(
            1,
            2,
            position,
            base + Duration::from_secs(u64::try_from(seconds).expect("secs")),
            wall,
            row.as_ref(),
        );
        // Every beat commits: each node last wrote twenty seconds ago.
        row = Some(WatchState {
            position_ms: position,
            duration_ms: Some(7_200_000),
            watched: false,
            updated_at: wall / 1_000,
        });
    }
    assert_eq!(credited, 600_000, "ten minutes on two nodes is ten minutes");

    // Paused, alternating: nothing.
    let mut paused = 0;
    for beat in 61..=70i64 {
        let seconds = beat * 10;
        let wall = wall0 + seconds * 1_000;
        paused += nodes[usize::try_from(beat % 2).expect("node")].beat_after(
            1,
            2,
            700_000,
            base + Duration::from_secs(u64::try_from(seconds).expect("secs")),
            wall,
            row.as_ref(),
        );
        row = Some(WatchState {
            position_ms: 700_000,
            duration_ms: Some(7_200_000),
            watched: false,
            updated_at: wall / 1_000,
        });
    }
    assert_eq!(paused, 0);

    // One node, five-second beats, a ten-second commit window: the row
    // holds this node's own earlier beats, never a newer one, and the
    // credit is the local, exact one.
    let alone = WatchLedger::default();
    let mut own_row: Option<WatchState> = None;
    let mut credited = 0;
    for beat in 0..=24i64 {
        let seconds = beat * 5;
        let position = seconds * 1_000;
        let wall = wall0 + seconds * 1_000;
        credited += alone.beat_after(
            3,
            4,
            position,
            base + Duration::from_secs(u64::try_from(seconds).expect("secs")),
            wall,
            own_row.as_ref(),
        );
        if beat % 2 == 0 {
            own_row = Some(WatchState {
                position_ms: position,
                duration_ms: None,
                watched: false,
                updated_at: wall / 1_000,
            });
        }
    }
    assert_eq!(credited, 120_000, "two minutes alone is two minutes");
}

/// Everything the ledger must not count: a paused player, a seek, a
/// rewind, a beat after a long silence, and a viewer arriving when the
/// ledger is full of live ones. And the one thing it must bound: a
/// player at 2x is credited wall time, not media time.
#[test]
fn watched_time_credits_only_plausible_forward_play_and_never_more_than_wall_time() {
    let base = Instant::now();
    let t = |seconds: u64| base + Duration::from_secs(seconds);
    let ledger = WatchLedger::default();
    assert_eq!(
        ledger.beat(1, 1, 10_000, t(0)),
        0,
        "a first beat has no baseline"
    );
    assert_eq!(ledger.beat(1, 1, 15_000, t(5)), 5_000, "ordinary play");
    assert_eq!(ledger.beat(1, 1, 15_000, t(10)), 0, "paused");
    assert_eq!(ledger.beat(1, 1, 615_000, t(15)), 0, "a ten-minute seek");
    assert_eq!(ledger.beat(1, 1, 600_000, t(20)), 0, "a rewind");
    assert_eq!(
        ledger.beat(1, 1, 610_000, t(25)),
        5_000,
        "2x speed earns wall time"
    );
    assert_eq!(
        ledger.beat(1, 1, 612_000, t(30)),
        2_000,
        "part stalled earns the advance"
    );
    assert_eq!(
        ledger.beat(1, 1, 750_000, t(160)),
        0,
        "after a silence longer than the gap"
    );
    assert_eq!(
        ledger.beat(1, 1, 755_000, t(165)),
        5_000,
        "and the baseline restarts"
    );
    assert_eq!(
        ledger.beat(2, 1, 0, t(165)),
        0,
        "another viewer is another key"
    );

    let full = WatchLedger::default();
    for key in 0..WATCH_LEDGER_CAP {
        full.beat(i64::try_from(key).expect("key"), 1, 0, t(0));
    }
    assert_eq!(full.beat(-1, 1, 0, t(1)), 0);
    assert_eq!(
        full.beat(-1, 1, 5_000, t(6)),
        0,
        "a viewer past the cap is not tracked"
    );
    assert_eq!(full.beats.lock().expect("ledger").len(), WATCH_LEDGER_CAP);
    // Once the tracked viewers go quiet past the gap, room is made.
    assert_eq!(full.beat(-1, 1, 0, t(200)), 0);
    assert_eq!(full.beat(-1, 1, 5_000, t(205)), 5_000);
    assert_eq!(full.beats.lock().expect("ledger").len(), 1);
}

#[test]
fn the_prewarm_ratio_stays_absent_until_a_hit_is_observed() {
    // This is the fleet's real state. Prewarming a marker destination is
    // unbuilt, so every callsite on every client reports a miss and does
    // so truthfully -- and a published quotient would sit at 0.000000
    // forever, which is indistinguishable from a feature that is failing.
    // The counters still carry every miss, so the suppression withholds a
    // claim rather than data.
    let metrics = PlaybackMetrics::new();
    for _ in 0..3 {
        metrics.record(&PlaybackEvent {
            event: "marker_prewarm".into(),
            detail: Some("miss".into()),
            ..PlaybackEvent::default()
        });
    }
    let text = metrics.render();
    assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"miss\"} 3"));
    assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"hit\"} 0"));
    assert!(!text.contains("plurx_playback_marker_prewarm_hit_ratio"));

    // A quiet node is the other reading the single value conflated: no
    // marker skip since boot is not a rate of zero either.
    let idle = PlaybackMetrics::new();
    assert!(!idle
        .render()
        .contains("plurx_playback_marker_prewarm_hit_ratio"));

    // The first hit is what makes the gauge mean something, and a genuine
    // zero rate publishes normally once one exists.
    metrics.record(&PlaybackEvent {
        event: "marker_prewarm".into(),
        detail: Some("hit".into()),
        ..PlaybackEvent::default()
    });
    assert!(metrics
        .render()
        .contains("plurx_playback_marker_prewarm_hit_ratio 0.250000"));
}

#[test]
fn prior_observation_uses_conservative_throughput_and_supply_stalls_only() {
    let network = NetworkIdentity {
        client_class: "chrome".to_owned(),
        network_fingerprint: "192.0.2.0/24".to_owned(),
        credential_generation: Some(CredentialGeneration::from("test-gen".to_owned())),
        user_id: Some(42),
    };
    let event = PlaybackEvent {
        at_unix_ms: 123,
        user_id: Some(4),
        event: "stall".to_owned(),
        height: Some(720),
        bandwidth_kbps: Some(8_000),
        delivered_bps: Some(5_000_000),
        detail: Some("supply:empty".to_owned()),
        ..PlaybackEvent::default()
    };
    let observation = prior_observation(&event, Some(&network)).expect("observation");
    assert_eq!(observation.throughput_kbps, Some(5_000));
    assert_eq!(observation.starved_rung_height, Some(720));
    assert_eq!(observation.observed_at_ms, 123);

    let mut decode = event;
    decode.detail = Some("decode:late_frames".to_owned());
    assert!(prior_observation(&decode, Some(&network)).is_none());

    decode.height = None;
    assert!(prior_observation(&decode, Some(&network)).is_none());
}

#[test]
fn typed_auto_causes_do_not_turn_decode_hold_or_authority_into_network_pressure() {
    let network = NetworkIdentity {
        client_class: "android".into(),
        network_fingerprint: "192.0.2.0/24".into(),
        credential_generation: Some(CredentialGeneration::from("test-gen".to_owned())),
        user_id: Some(42),
    };
    for cause in ["link", "decode", "hold", "authority", "unknown"] {
        let event = PlaybackEvent {
            event: "stall".into(),
            height: Some(720),
            bandwidth_kbps: Some(100),
            runway_ds: Some(0),
            detail: Some(format!("{cause}:buffering")),
            ..PlaybackEvent::default()
        };
        let observation = prior_observation(&event, Some(&network));
        if cause == "link" {
            assert_eq!(observation.expect(cause).starved_rung_height, Some(720));
        } else {
            assert!(observation.is_none(), "{cause} is not network evidence");
        }
    }
}

#[test]
fn encode_cpu_pressure_never_updates_network_prior() {
    let network = NetworkIdentity {
        client_class: "android".into(),
        network_fingerprint: "192.0.2.0/24".into(),
        credential_generation: Some(CredentialGeneration::from("test-gen".to_owned())),
        user_id: Some(42),
    };
    let event = PlaybackEvent {
        event: "stall".into(),
        height: Some(1440),
        bandwidth_kbps: Some(100),
        delivered_bps: Some(100_000),
        runway_ds: Some(0),
        detail: Some("encode:adaptive_reopen".into()),
        ..PlaybackEvent::default()
    };
    assert!(prior_observation(&event, Some(&network)).is_none());
}

#[test]
fn a05_legacy_link_meter_never_claims_completed_transfer_provenance() {
    let network = NetworkIdentity {
        client_class: "android".into(),
        network_fingerprint: "192.0.2.0/24".into(),
        credential_generation: Some(CredentialGeneration::from("test-gen".to_owned())),
        user_id: Some(42),
    };
    for cause in ["link", "supply", "encode", "decode", "hold", "authority"] {
        let event = PlaybackEvent {
            at_unix_ms: 123_000,
            event: "stall".into(),
            height: Some(1080),
            bandwidth_kbps: Some(100),
            delivered_bps: Some(100_000),
            runway_ds: Some(0),
            detail: Some(format!("{cause}:buffering")),
            ..PlaybackEvent::default()
        };
        let observation = prior_observation(&event, Some(&network));
        if matches!(cause, "link" | "supply") {
            let observation = observation.expect("legacy prior remains available");
            assert_eq!(observation.starved_rung_height, Some(1080));
            assert!(observation.measured_link.is_none());
        } else {
            assert!(observation.is_none(), "{cause}");
        }
    }
}

/// How the writer called its Store, recorded call by call.
#[derive(Default)]
struct WriterCalls {
    /// `(events, observations)` per `write_batch` call.
    batches: Vec<(usize, usize)>,
}

/// A `WriterStore` that records its calls and can be told to sleep or to
/// panic on the first of them.
struct ScriptedWriterStore {
    calls: Arc<Mutex<WriterCalls>>,
    priors_enabled: bool,
    sleep: Option<Duration>,
    panic_first: AtomicBool,
}

impl ScriptedWriterStore {
    fn new(priors_enabled: bool) -> Self {
        Self {
            calls: Arc::new(Mutex::new(WriterCalls::default())),
            priors_enabled,
            sleep: None,
            panic_first: AtomicBool::new(false),
        }
    }

    fn sleeping(duration: Duration) -> Self {
        Self {
            sleep: Some(duration),
            ..Self::new(false)
        }
    }

    fn panicking_once() -> Self {
        let store = Self::new(false);
        store.panic_first.store(true, Ordering::Release);
        store
    }

    fn batches(&self) -> Vec<(usize, usize)> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .batches
            .clone()
    }
}

impl WriterStore for ScriptedWriterStore {
    fn write_batch<'a>(
        &'a self,
        events: &'a [PlaybackEvent],
        observations: &'a [NetworkPriorObservation],
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .batches
            .push((events.len(), observations.len()));
        Box::pin(async move {
            if self.panic_first.swap(false, Ordering::AcqRel) {
                panic!("injected node-local store panic");
            }
            if let Some(sleep) = self.sleep {
                tokio::time::sleep(sleep).await;
            }
            Ok(events.len() as u64)
        })
    }

    fn setting_pair(&self) -> BoxFuture<'_, Result<SettingPair, StoreError>> {
        let priors = self.priors_enabled;
        Box::pin(async move {
            Ok((
                Some("30".to_owned()),
                Some(if priors { "1" } else { "0" }.to_owned()),
            ))
        })
    }
}

fn network_identity() -> NetworkIdentity {
    NetworkIdentity {
        client_class: "chrome".to_owned(),
        network_fingerprint: "192.0.2.0/24".to_owned(),
        credential_generation: Some(CredentialGeneration::from("c06-gen".to_owned())),
        user_id: Some(42),
    }
}

fn measured_job(session: &str) -> Job {
    let event = PlaybackEvent {
        event: "producer_pass".to_owned(),
        session_id: Some(session.to_owned()),
        bandwidth_kbps: Some(9_000),
        delivered_bps: Some(6_000_000),
        ..PlaybackEvent::default()
    };
    Job {
        class: classify(&event),
        event,
        network: Some(network_identity()),
    }
}

async fn wait_for(mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    done()
}

/// One batch is one Store call even with the network-prior opt-in on.
///
/// The batch is queued before the writer starts so the batch boundary is
/// the test's, not the scheduler's. Folding the priors event by event --
/// one `observe_network_prior` per job after the batched insert -- would
/// make this 65 calls, and on the hiqlite sidecar 65 acquisitions of the
/// one mutex that telemetry shares with fragment-index work.
#[tokio::test]
async fn a_batch_with_priors_on_is_one_store_call_not_one_per_event() {
    let metrics = isolated_metrics();
    let sink = new_sink(metrics);
    for index in 0..BATCH {
        assert_eq!(
            sink.queue.push(measured_job(&format!("session-{index}"))),
            Admission::Queued
        );
    }
    let store = Arc::new(ScriptedWriterStore::new(true));
    let observer = Arc::clone(&store);
    tokio::spawn(supervise_writer(store, Arc::clone(&sink)));

    assert!(
        wait_for(|| !observer.batches().is_empty()).await,
        "the writer never ran"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        observer.batches(),
        vec![(BATCH, BATCH)],
        "64 events and their 64 prior observations must cross the Store \
             boundary once, not once plus one per event"
    );
}

/// Retention off and priors on stays one call carrying only the priors:
/// the switches are independent, the lease is shared.
#[tokio::test]
async fn the_two_opt_ins_stay_independent_inside_the_shared_batch() {
    let metrics = isolated_metrics();
    let sink = new_sink(metrics);
    sink.settings
        .write()
        .expect("settings")
        .clone_from(&EffectiveSettings {
            retain: false,
            priors: true,
            read_at: Some(Instant::now()),
        });
    for index in 0..8 {
        sink.queue.push(measured_job(&format!("session-{index}")));
    }
    // A cached read this fresh is never refreshed, so the sink keeps the
    // retention-off, priors-on pair the operator set.
    let store = Arc::new(ScriptedWriterStore::new(true));
    let observer = Arc::clone(&store);
    tokio::spawn(supervise_writer(store, Arc::clone(&sink)));

    assert!(wait_for(|| !observer.batches().is_empty()).await);
    assert_eq!(observer.batches(), vec![(0, 8)]);
}

/// A store future that panics destroys the batch the writer took out of
/// the queue. Whatever else happens, that loss is counted.
#[tokio::test]
async fn a_panicking_store_accounts_for_the_batch_it_destroys() {
    let metrics = isolated_metrics();
    let sink = new_sink(metrics);
    for index in 0..8 {
        sink.queue.push(measured_job(&format!("doomed-{index}")));
    }
    let store = Arc::new(ScriptedWriterStore::panicking_once());
    let observer = Arc::clone(&store);
    tokio::spawn(supervise_writer(store, Arc::clone(&sink)));

    assert!(
        wait_for(|| metrics.dropped_writer_panic.load(Ordering::Relaxed) == 8).await,
        "a batch lost to a panicking store must raise a drop reason, not vanish; \
             saw {}",
        metrics.dropped_writer_panic.load(Ordering::Relaxed)
    );
    assert_eq!(metrics.written_ok.load(Ordering::Relaxed), 0);

    // And the writer is back: the restart is not a silent stop either.
    for index in 0..4 {
        sink.queue.push(measured_job(&format!("after-{index}")));
    }
    assert!(
        wait_for(|| metrics.written_ok.load(Ordering::Relaxed) == 4).await,
        "the supervisor did not restart the writer"
    );
    assert_eq!(observer.batches().len(), 2);
}

/// The shutdown bound transfers ownership; it does not guess from
/// occupancy.
///
/// A writer wedged inside a Store call still holds the batch it took. The
/// remainder is therefore the queue AND that batch, counted once, by
/// taking both away from the writer -- so a writer that returns after the
/// bound cannot count them again as written.
#[tokio::test]
async fn a_wedged_writer_hands_shutdown_its_remainder_exactly_once() {
    let metrics = isolated_metrics();
    let sink = new_sink(metrics);
    for index in 0..QUEUE - TERMINAL_RESERVE {
        sink.queue.push(measured_job(&format!("sample-{index}")));
    }
    for index in 0..TERMINAL_RESERVE {
        sink.queue
            .push(job("control_terminal", &format!("terminal-{index}"), None));
    }
    assert_eq!(sink.queue.len(), QUEUE);

    let store = Arc::new(ScriptedWriterStore::sleeping(Duration::from_secs(4)));
    let observer = Arc::clone(&store);
    tokio::spawn(supervise_writer(store, Arc::clone(&sink)));
    assert!(
        wait_for(|| !observer.batches().is_empty()).await,
        "the writer never took a batch"
    );
    assert_eq!(sink.queue.len(), QUEUE - BATCH);

    drain_sink(&sink).await;
    assert_eq!(
        metrics.dropped_shutdown.load(Ordering::Relaxed),
        QUEUE as u64,
        "the queue remainder AND the wedged writer's in-flight batch are \
             the remainder; counting sender occupancy alone loses the batch"
    );

    // The store finally returns. The batch it was holding has already
    // been accounted for, so nothing is counted a second time.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(
        metrics.dropped_shutdown.load(Ordering::Relaxed),
        QUEUE as u64
    );
    assert_eq!(
        metrics.written_ok.load(Ordering::Relaxed),
        0,
        "a batch reported dropped must not also be reported written"
    );
}

/// `emit` returns on the caller's thread whatever the store is doing, and
/// the queue it feeds stays bounded.
#[tokio::test]
async fn emit_stays_synchronous_under_a_wedged_writer_and_the_queue_stays_bounded() {
    let metrics = isolated_metrics();
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
    let sink = install_test_sink(
        &store,
        Arc::new(ScriptedWriterStore::sleeping(Duration::from_secs(30))),
        metrics,
    );

    let started = Instant::now();
    for index in 0..5_000 {
        emit(
            Arc::clone(&store),
            PlaybackEvent {
                event: "producer_pass".into(),
                session_id: Some(format!("session-{index}")),
                ..PlaybackEvent::default()
            },
        );
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "5,000 emits took {elapsed:?} against a store that sleeps 30s"
    );
    assert!(
        sink.queue.len() <= QUEUE,
        "queue depth {} exceeded its bound",
        sink.queue.len()
    );
    assert!(
        metrics.dropped_queue_full.load(Ordering::Relaxed) > 0,
        "a full queue must count what it refuses"
    );
    assert_eq!(metrics.written_ok.load(Ordering::Relaxed), 0);

    // And the reserve is still there for the outcome that matters.
    emit(
        Arc::clone(&store),
        PlaybackEvent {
            event: "control_terminal".into(),
            session_id: Some("terminal".into()),
            ..PlaybackEvent::default()
        },
    );
    assert_eq!(
        metrics.enqueued[EventClass::Terminal.index()].load(Ordering::Relaxed),
        1,
        "a terminal was refused by a queue full of samples"
    );
}

#[test]
fn a_store_outliving_its_writer_runtime_cannot_reuse_a_stale_sink() {
    let runtime = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    };
    let old_runtime = runtime();
    let (old_store, old_sink) = old_runtime.block_on(async {
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("old store"));
        let sink = ensure_sink(Arc::clone(&store));
        (store, sink)
    });
    let old_key = store_key(&old_store);
    drop(old_runtime);
    let identity = Arc::downgrade(&old_store);
    drop(old_store);
    assert!(
        identity.upgrade().is_none(),
        "registry must not retain the Store"
    );
    runtime().block_on(async {
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("new store"));
        let new_sink = ensure_sink(Arc::clone(&store));
        assert!(
            !Arc::ptr_eq(&old_sink, &new_sink),
            "a new Store needs a live writer"
        );
        assert!(
            !SINKS.lock().expect("registry").contains_key(&old_key),
            "expired store registrations must be pruned"
        );
        emit(
            Arc::clone(&store),
            PlaybackEvent {
                event: "session_end".into(),
                ..Default::default()
            },
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        event: Some("session_end".into()),
                        limit: 10,
                        ..Default::default()
                    })
                    .await
                    .expect("events");
                if !rows.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the replacement writer must persist events");
        drain_for_shutdown(&store).await;
    });
}

/// `emit` against a Store boot never registered must build the sink and
/// return.
///
/// It runs on its own thread with a timeout because the failure it guards
/// is a deadlock, and a deadlock does not fail a test -- it hangs the
/// whole binary, which is how this reached a review as a passing branch.
#[test]
fn an_unregistered_store_registers_its_sink_without_relocking() {
    // The watchdog covers registry registration, not schema migration or
    // runtime construction. Those fixtures can be slow under suite load
    // without emit relocking its registry or failing to make progress.
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        runtime.block_on(async move {
            emit(
                Arc::clone(&store),
                PlaybackEvent {
                    event: "ttff".into(),
                    ..PlaybackEvent::default()
                },
            );
            let registered = SINKS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&store_key(&store));
            let _ = sender.send(registered);
        });
    });
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(10)),
        Ok(true),
        "emit against an unregistered Store must build its sink and return"
    );
}

#[test]
fn every_drop_reason_is_rendered_with_a_fixed_label() {
    let metrics = QueueMetrics::new();
    let rendered = metrics.render();
    for reason in [
        "queue_full",
        "coalesced",
        "writer_degraded",
        "writer_panic",
        "shutdown",
    ] {
        assert!(
            rendered.contains(&format!(
                "plurx_telemetry_dropped_total{{reason=\"{reason}\"}}"
            )),
            "missing drop reason {reason}"
        );
    }
    assert!(!rendered.contains("session"));
}
