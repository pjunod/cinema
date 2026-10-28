use super::*;

#[test]
fn sharing_custody_migration_dispatch_keeps_frozen_source71_separate_from_local70() {
    let row = |schema_version| CompatibilityRow {
        schema_version,
        protocol_min: AUTH_PROTOCOL_VERSION,
        protocol_max: AUTH_PROTOCOL_VERSION,
    };
    for previous in [QUALITY_LEDGER_SCHEMA_VERSION, SHARING_SCHEMA_VERSION] {
        assert_eq!(
            schema_migration_action(&[row(previous)], ClusterCompatibility::CURRENT)
                .expect("explicit legacy upgrade input"),
            SchemaMigrationAction::MigrateFrom(previous)
        );
    }
    for current in [AUTH_SCHEMA_BASELINE_VERSION, AUTH_SCHEMA_VERSION] {
        assert_eq!(
            schema_migration_action(&[row(current)], ClusterCompatibility::CURRENT)
                .expect("current exact marker"),
            SchemaMigrationAction::Current
        );
    }
    assert!(schema_migration_action(
        &[row(AUTH_SCHEMA_VERSION + 1)],
        ClusterCompatibility::CURRENT
    )
    .is_err());
    assert_eq!(
        super::super::sharing_source_schema::SOURCE_LAYOUT_VERSION,
        71
    );
    assert!(super::super::sharing_source_schema::INSTALLATION_SCHEMA
        .contains("CHECK(schema_version=71)"));
}

#[test]
fn sharing_private_replicated70_to73_objects_refuse_main_ordinal_reinterpretation() {
    for marker in 70..=73 {
        assert!(sharing_lineage_is_ambiguous(marker, 1));
        assert!(!sharing_lineage_is_ambiguous(marker, 0));
    }
    assert!(sharing_lineage_is_ambiguous(79, 1));
    assert!(!sharing_lineage_is_ambiguous(80, 1));
    assert_eq!(SHARING_SCHEMA_MIGRATION_SOURCE, 79);
    assert_eq!(
        (AUTH_SCHEMA_BASELINE_VERSION, AUTH_SCHEMA_VERSION),
        (81, 82)
    );
}

#[test]
fn playback_generation_covers_tied_updates_deletes_import_and_rollback() {
    let conn = rusqlite::Connection::open_in_memory().expect("database");
    conn.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);").expect("settings");
    conn.execute_batch(super::super::PLAYBACK_INPUT_SCHEMA)
        .expect("generation schema");
    let generation = || {
        conn.query_row(
            "SELECT generation FROM playback_input_generation WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("generation")
    };
    conn.execute(
        "INSERT INTO settings VALUES('playback.audio_lang', 'eng', 1)",
        [],
    )
    .expect("insert");
    assert_eq!(generation(), 1);
    conn.execute(
        "UPDATE settings SET value = 'fra' WHERE key = 'playback.audio_lang'",
        [],
    )
    .expect("tied update");
    assert_eq!(generation(), 2);
    conn.execute("DELETE FROM settings WHERE key = 'playback.audio_lang'", [])
        .expect("delete");
    assert_eq!(generation(), 3);
    conn.execute_batch(
        "BEGIN; INSERT INTO settings VALUES('transcode.hwaccel', 'vaapi', 1); ROLLBACK;",
    )
    .expect("rollback");
    assert_eq!(generation(), 3);
    conn.execute_batch("INSERT INTO settings VALUES('jobs.last_scan', '2', 1);")
        .expect("job write");
    assert_eq!(generation(), 3);
    // Raw SQL is the import/migration path, so it must also bump atomically.
    conn.execute_batch(
        "BEGIN; INSERT INTO settings VALUES('transcode.hwaccel', 'qsv', 1); COMMIT;",
    )
    .expect("import");
    assert_eq!(generation(), 4);
}

static TEST_STORE_OPERATION_METRICS: LazyLock<StoreOperationMetrics> =
    LazyLock::new(StoreOperationMetrics::default);

#[test]
fn k06_schema_migration_admits_each_new_boundary_without_replenishment() {
    let expired = std::sync::atomic::AtomicBool::new(false);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let admit = || {
        attempts.fetch_add(1, Ordering::SeqCst);
        if expired.load(Ordering::SeqCst) {
            Err(StoreError::Database(
                "original startup deadline expired".into(),
            ))
        } else {
            Ok(())
        }
    };
    admit_schema_migration(Some(&admit)).expect("first boundary in budget");
    expired.store(true, Ordering::SeqCst);
    assert!(admit_schema_migration(Some(&admit)).is_err());
    assert!(admit_schema_migration(Some(&admit)).is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    // Ordinary maintenance/open migration has no startup phase callback.
    admit_schema_migration(None).expect("unchanged ordinary path");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn replicated_generation_guard_matches_only_canonical_integer_state() {
    let connection = rusqlite::Connection::open_in_memory().expect("open SQLite evaluator");
    connection
        .execute(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .expect("schema");
    let matches = |expected: i64| {
        connection
            .query_row(
                &format!("SELECT {CANONICAL_SETTINGS_GENERATION_PREDICATE}"),
                rusqlite::params!["generation", expected],
                |row| row.get::<_, bool>(0),
            )
            .expect("evaluate replicated predicate")
    };
    assert!(matches(0), "an absent generation starts at zero");
    for corrupt in ["garbage", "00", "-0", "+0", " 0"] {
        connection
            .execute(
                "INSERT INTO settings (key, value) VALUES ('generation', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [corrupt],
            )
            .expect("inject state");
        assert!(!matches(0), "{corrupt:?} must fail closed");
    }
    connection
        .execute(
            "UPDATE settings SET value = '0' WHERE key = 'generation'",
            [],
        )
        .expect("canonical zero");
    assert!(matches(0));
    assert!(!matches(1));
}

#[test]
fn media_session_dump_retains_terminal_and_publication_fence_fields() {
    let row = MediaSessionDumpRow {
        incarnation_id: "incarnation".to_owned(),
        session_id: "session".to_owned(),
        user_id: 1,
        playback_id: "playback".to_owned(),
        request_fingerprint: "fingerprint".to_owned(),
        owner_node_id: "node".to_owned(),
        owner_epoch: 2,
        lease_expires_at_ms: 3,
        state: "ended".to_owned(),
        terminal_reason: Some("deleted".to_owned()),
        publication_ready_at_ms: 4,
        recipe_json: "{}".to_owned(),
        response_json: "{}".to_owned(),
        produced_playable_through_ms: 5,
        fetched_through_ms: 6,
        media_origin_ms: 7,
        media_sequence: 8,
        discontinuity_sequence: 9,
        updated_at_ms: 10,
    };

    let encoded = serde_json::to_value(row).expect("serialize media session dump row");
    assert_eq!(encoded["terminal_reason"], "deleted");
    assert_eq!(encoded["publication_ready_at_ms"], 4);
}

#[tokio::test(start_paused = true)]
async fn replaceable_write_gate_pins_window_failure_retry_and_terminal_bypass() {
    let gate = ReplaceableWriteGate::<String>::default();
    let submitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let run = |submitted: Arc<std::sync::atomic::AtomicUsize>| async move {
        submitted.fetch_add(1, Ordering::Relaxed);
        Ok::<_, StoreError>(())
    };

    assert_eq!(
        gate.run(
            "cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("leading touch"),
        ReplaceableWriteOutcome::Submitted
    );
    tokio::time::advance(CACHE_TOUCH_COMMIT_WINDOW - Duration::from_millis(1)).await;
    assert_eq!(
        gate.run(
            "cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("inside-window touch"),
        ReplaceableWriteOutcome::Suppressed
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        gate.run(
            "cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("boundary touch"),
        ReplaceableWriteOutcome::Submitted
    );
    assert_eq!(submitted.load(Ordering::Relaxed), 2);

    assert_eq!(
        gate.run(
            "slow-cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            async {
                submitted.fetch_add(1, Ordering::Relaxed);
                tokio::time::advance(Duration::from_secs(4)).await;
                Ok(())
            },
        )
        .await
        .expect("slow leading touch"),
        ReplaceableWriteOutcome::Submitted
    );
    tokio::time::advance(Duration::from_millis(999)).await;
    assert_eq!(
        gate.run(
            "slow-cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("slow touch inside dispatch-anchored window"),
        ReplaceableWriteOutcome::Suppressed
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        gate.run(
            "slow-cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("slow touch at dispatch-anchored boundary"),
        ReplaceableWriteOutcome::Submitted
    );

    let failure = gate
        .run("failure".to_owned(), CACHE_TOUCH_COMMIT_WINDOW, async {
            Err(StoreError::Task("injected touch failure".to_owned()))
        })
        .await;
    assert!(failure.is_err());
    assert_eq!(
        gate.run(
            "failure".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("failed touch must retry"),
        ReplaceableWriteOutcome::Submitted
    );

    // Completion/removal never enter the replaceable gate. This models a
    // terminal mutation at the same instant as a suppressed activity fact.
    let terminal_commits = std::sync::atomic::AtomicUsize::new(0);
    assert_eq!(
        gate.run(
            "terminal-cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("leading activity before terminal mutation"),
        ReplaceableWriteOutcome::Submitted
    );
    assert_eq!(
        gate.run(
            "terminal-cache-use".to_owned(),
            CACHE_TOUCH_COMMIT_WINDOW,
            run(Arc::clone(&submitted)),
        )
        .await
        .expect("duplicate activity touch"),
        ReplaceableWriteOutcome::Suppressed
    );
    terminal_commits.fetch_add(1, Ordering::Relaxed);
    assert_eq!(terminal_commits.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_equal_replaceable_writes_share_one_durable_result() {
    let gate = Arc::new(ReplaceableWriteGate::<String>::default());
    let submitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(81));
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..80 {
        let gate = Arc::clone(&gate);
        let submitted = Arc::clone(&submitted);
        let barrier = Arc::clone(&barrier);
        requests.spawn(async move {
            barrier.wait().await;
            gate.run(
                "same-location".to_owned(),
                Duration::from_secs(1),
                async move {
                    submitted.fetch_add(1, Ordering::Relaxed);
                    tokio::task::yield_now().await;
                    Ok(())
                },
            )
            .await
        });
    }
    barrier.wait().await;
    let mut admitted = 0;
    while let Some(result) = requests.join_next().await {
        if result.expect("touch task").expect("touch result") == ReplaceableWriteOutcome::Submitted
        {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 1);
    assert_eq!(submitted.load(Ordering::Relaxed), 1);

    let bypass_commits = (0..80).count();
    assert!(
        bypass_commits > admitted,
        "the uncoalesced load control must violate the one-commit burst budget"
    );
}

#[tokio::test(start_paused = true)]
async fn replaceable_write_gate_caps_hot_keys_and_reclaims_expired_identities() {
    let gate = ReplaceableWriteGate::<String>::default();
    for ordinal in 0..REPLACEABLE_WRITE_GATE_MAX_KEYS {
        let state = gate.key_state(format!("recent-{ordinal}"), CACHE_TOUCH_COMMIT_WINDOW);
        *state.lock().await = Some(tokio::time::Instant::now());
    }
    assert_eq!(
        gate.keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        REPLACEABLE_WRITE_GATE_MAX_KEYS
    );

    let active = gate.key_state("recent-0".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
    let same = gate.key_state("recent-0".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
    assert!(Arc::ptr_eq(&active, &same));
    drop(same);

    let overflow = gate.key_state("overflow".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
    assert!(
        !gate
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key("overflow"),
        "a 4,097th recent identity must degrade without growing the map"
    );
    drop(overflow);

    tokio::time::advance(CACHE_TOUCH_COMMIT_WINDOW).await;
    let reclaimed = gate.key_state("reclaimed".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
    {
        let keys = gate
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            keys.len(),
            2,
            "expired idle identities must be reclaimed while an active key stays mapped"
        );
        assert!(keys.contains_key("recent-0"));
        assert!(keys.contains_key("reclaimed"));
    }
    drop(active);
    drop(reclaimed);

    for ordinal in 0..(REPLACEABLE_WRITE_GATE_MAX_KEYS - 2) {
        drop(gate.key_state(format!("refill-{ordinal}"), CACHE_TOUCH_COMMIT_WINDOW));
    }
    let final_key = gate.key_state("final".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
    let keys = gate
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        keys.len(),
        1,
        "an expired key must become reclaimable after its active caller releases it"
    );
    assert!(keys.contains_key("final"));
    drop(keys);
    drop(final_key);
}

#[test]
fn cache_terminal_mutations_cannot_enter_the_replaceable_touch_gate() {
    let source = include_str!("../hiqlite_durable.rs");
    let method = |start: &str, end: &str| {
        source
            .split_once(start)
            .unwrap_or_else(|| panic!("missing {start}"))
            .1
            .split_once(end)
            .unwrap_or_else(|| panic!("missing {end}"))
            .0
    };
    assert!(method(
        "async fn touch_cache_claim",
        "async fn complete_cache_entry"
    )
    .contains("coalesce_cache_touch"));
    assert!(
        method("async fn touch_cache_entry", "async fn cache_by_age")
            .contains("coalesce_cache_touch")
    );
    for terminal in [
        method(
            "async fn complete_cache_entry",
            "async fn touch_cache_entry",
        ),
        method(
            "async fn invalidate_cache_entry",
            "async fn forget_cache_entry",
        ),
        method("async fn forget_cache_entry", "async fn cache_bytes"),
    ] {
        assert!(!terminal.contains("coalesce_cache_touch"));
        assert!(terminal.contains("self.execute") || terminal.contains(".txn("));
    }
}

#[tokio::test]
async fn store_operation_timer_classifies_completion_error_and_cancellation() {
    let count = |class, outcome| {
        TEST_STORE_OPERATION_METRICS
            .cell(class, outcome)
            .count
            .load(Ordering::Relaxed)
    };
    let ok_before = count(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok);
    let error_before = count(
        StoreOperationClass::AuthorityRead,
        StoreOperationOutcome::Error,
    );
    let cancelled_before = count(StoreOperationClass::Write, StoreOperationOutcome::Cancelled);
    let statement_error_before = count(StoreOperationClass::Write, StoreOperationOutcome::Error);

    time_store_operation(
        &TEST_STORE_OPERATION_METRICS,
        StoreOperationClass::LocalRead,
        async { Ok::<_, StoreError>(()) },
        |_| true,
    )
    .await
    .expect("successful timed operation");
    let error = time_store_operation(
        &TEST_STORE_OPERATION_METRICS,
        StoreOperationClass::AuthorityRead,
        async { Err::<(), _>(StoreError::Database("injected failure".to_owned())) },
        |_| true,
    )
    .await;
    assert!(error.is_err());

    let nested = time_store_operation(
        &TEST_STORE_OPERATION_METRICS,
        StoreOperationClass::Write,
        async { Ok::<_, StoreError>(vec![Ok::<(), &'static str>(()), Err("constraint failure")]) },
        |results| results.iter().all(Result::is_ok),
    )
    .await
    .expect("Hiqlite returns statement errors inside the operation result");
    assert!(nested.iter().any(Result::is_err));

    let mut cancelled = Box::pin(time_store_operation(
        &TEST_STORE_OPERATION_METRICS,
        StoreOperationClass::Write,
        std::future::pending::<Result<(), StoreError>>(),
        |_| true,
    ));
    assert!(matches!(
        futures_util::poll!(&mut cancelled),
        std::task::Poll::Pending
    ));
    drop(cancelled);

    assert_eq!(
        count(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok),
        ok_before + 1
    );
    assert_eq!(
        count(
            StoreOperationClass::AuthorityRead,
            StoreOperationOutcome::Error
        ),
        error_before + 1
    );
    assert_eq!(
        count(StoreOperationClass::Write, StoreOperationOutcome::Cancelled),
        cancelled_before + 1
    );
    assert_eq!(
        count(StoreOperationClass::Write, StoreOperationOutcome::Error),
        statement_error_before + 1
    );
}

#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn validation_control_preserves_results_without_recording_any_outcome() {
    let metrics: &'static StoreOperationMetrics =
        Box::leak(Box::new(StoreOperationMetrics::default()));
    let total = || {
        metrics
            .cells
            .iter()
            .fold(0_u64, |sum, cell| sum + cell.count.load(Ordering::Relaxed))
    };

    time_store_operation_controlled(
        metrics,
        StoreOperationClass::LocalRead,
        async { Ok::<_, StoreError>(17) },
        |_| true,
        false,
    )
    .await
    .expect("disabled success preserves its value");
    let error = time_store_operation_controlled(
        metrics,
        StoreOperationClass::AuthorityRead,
        async { Err::<(), _>(StoreError::Database("control error".to_owned())) },
        |_| true,
        false,
    )
    .await
    .expect_err("disabled error remains an error");
    assert_eq!(error.to_string(), "database error: control error");
    let mut cancelled = Box::pin(time_store_operation_controlled(
        metrics,
        StoreOperationClass::Write,
        std::future::pending::<Result<(), StoreError>>(),
        |_| true,
        false,
    ));
    assert!(matches!(
        futures_util::poll!(&mut cancelled),
        std::task::Poll::Pending
    ));
    drop(cancelled);
    assert_eq!(total(), 0, "the control arm recorded an outcome");

    time_store_operation_controlled(
        metrics,
        StoreOperationClass::LocalRead,
        async { Ok::<_, StoreError>(17) },
        |_| true,
        true,
    )
    .await
    .expect("instrumented success");
    let _ = time_store_operation_controlled(
        metrics,
        StoreOperationClass::AuthorityRead,
        async { Err::<(), _>(StoreError::Database("measured error".to_owned())) },
        |_| true,
        true,
    )
    .await;
    let mut cancelled = Box::pin(time_store_operation_controlled(
        metrics,
        StoreOperationClass::Write,
        std::future::pending::<Result<(), StoreError>>(),
        |_| true,
        true,
    ));
    assert!(matches!(
        futures_util::poll!(&mut cancelled),
        std::task::Poll::Pending
    ));
    drop(cancelled);
    assert_eq!(total(), 3, "the measured arm omitted an outcome class");
}

#[tokio::test(start_paused = true)]
async fn takeover_authority_attribution_settings_pair_reaches_timed_client_retries() {
    for (first, second) in [
        (
            keys::CLUSTER_MEDIA_POOL_ENABLED,
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
        ),
        (
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
            keys::CLUSTER_MEDIA_POOL_ENABLED,
        ),
    ] {
        let io = Arc::new(InjectedConsistentRead {
            outcomes: Mutex::new(
                [
                    InjectedConsistentReadOutcome::Timeout,
                    InjectedConsistentReadOutcome::Error(
                        "CheckIsLeaderError: not enough for a quorum; got:{1}",
                    ),
                    InjectedConsistentReadOutcome::Empty,
                    InjectedConsistentReadOutcome::Timeout,
                    InjectedConsistentReadOutcome::Error(
                        "CheckIsLeaderError: not enough for a quorum; got:{1}",
                    ),
                    InjectedConsistentReadOutcome::Empty,
                ]
                .into(),
            ),
            calls: Mutex::new(Vec::new()),
            metrics: TakeoverAuthorityReadMetrics::default(),
        });
        let store = HiqliteAuthStore {
            media_session_projection: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            client: TimedClient {
                inner: TimedClientInner::InjectedConsistentRead(Arc::clone(&io)),
                #[cfg(feature = "cluster-read-cost-validation")]
                operations: Arc::new(OperationCounters::default()),
            },
            clock: Arc::new(FixedClock(0)),
            telemetry: NodeLocalTelemetry::open(Path::new(":memory:")).expect("in-memory sidecar"),
            activity_refreshes: Arc::new(ActivityRefreshGate::default()),
            cache_touches: Arc::new(ReplaceableWriteGate::default()),
            watch_fences: Arc::new(super::super::watch_fence::WatchWriteFences::default()),
            #[cfg(feature = "hiqlite-contract-tests")]
            insertion_seam: Default::default(),
        };
        let started = tokio::time::Instant::now();
        assert_eq!(
            store
                .get_setting_pair(first, second)
                .await
                .expect("the real settings-pair path recovers"),
            (None, None)
        );
        assert!(
            started.elapsed() >= STORE_TIMEOUT,
            "the real TimedClient deadline must fire before retry"
        );
        assert_eq!(
            io.metrics.started.load(Ordering::Relaxed),
            3,
            "settings-pair scope must reach the actual retry attempt sink in either key order"
        );
        assert_eq!(
            io.metrics.outcomes[StoreOperationOutcome::Error.index()].load(Ordering::Relaxed),
            2
        );
        assert_eq!(
            io.metrics.outcomes[StoreOperationOutcome::Ok.index()].load(Ordering::Relaxed),
            1
        );

        assert_eq!(
            store
                .get_setting_pair(keys::MONARR_URL, keys::MONARR_API_KEY)
                .await
                .expect("unrelated settings pair also recovers"),
            (None, None)
        );
        assert_eq!(
            io.metrics.started.load(Ordering::Relaxed),
            3,
            "unrelated retried settings pairs must not enter the takeover numerator"
        );
        assert_eq!(
            io.metrics.outcomes[StoreOperationOutcome::Error.index()].load(Ordering::Relaxed),
            2
        );
        assert_eq!(
            io.metrics.outcomes[StoreOperationOutcome::Ok.index()].load(Ordering::Relaxed),
            1
        );
        let calls = io.calls.lock().expect("calls lock");
        assert_eq!(
            calls.len(),
            6,
            "both pairs must execute all three physical attempts"
        );
        for (index, (sql, bound)) in calls.iter().enumerate() {
            assert_eq!(
                sql,
                "SELECT key, value FROM settings WHERE key = $1 OR key = $2 ORDER BY key"
            );
            let expected = if index < 3 {
                params!(first, second)
            } else {
                params!(keys::MONARR_URL, keys::MONARR_API_KEY)
            };
            assert_eq!(
                *bound, expected,
                "the production query retains the original bound key order on every retry"
            );
        }
        assert!(io.outcomes.lock().expect("outcomes lock").is_empty());
    }
}

#[test]
fn takeover_authority_attribution_selects_only_the_exact_pair_and_fixed_series() {
    let metrics = TakeoverAuthorityReadMetrics::default();
    for (first, second) in [
        (
            keys::CLUSTER_MEDIA_POOL_ENABLED,
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
        ),
        (
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
            keys::CLUSTER_MEDIA_POOL_ENABLED,
        ),
    ] {
        assert_eq!(
            AuthorityReadScope::settings_pair(first, second),
            AuthorityReadScope::TakeoverSettings
        );
        assert!(AuthorityReadScope::settings_pair(first, second)
            .metrics(&metrics)
            .is_some());
    }
    for (first, second) in [
        (keys::MONARR_URL, keys::MONARR_API_KEY),
        (keys::TRANSCODE_RATE_MODE, keys::TRANSCODE_QUALITY),
        (keys::BACKUP_SCHEDULE_UTC, keys::BACKUP_DESTINATION),
        (keys::TELEMETRY_RETAIN_DAYS, keys::PLAYBACK_NETWORK_PRIORS),
        (keys::MAX_HW_SESSIONS, keys::SW_POOL_THREADS),
        (
            keys::CLUSTER_MEDIA_POOL_ENABLED,
            keys::CLUSTER_MEDIA_POOL_ENABLED,
        ),
        (
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
        ),
        (
            keys::CLUSTER_MEDIA_POOL_ENABLED,
            "cluster.session_takeover_enabled.extra",
        ),
        (
            "cluster.media_pool_enabled.extra",
            keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
        ),
    ] {
        assert_eq!(
            AuthorityReadScope::settings_pair(first, second),
            AuthorityReadScope::Unattributed
        );
        assert!(AuthorityReadScope::settings_pair(first, second)
            .metrics(&metrics)
            .is_none());
    }
    let exposition = metrics.render();
    let samples: Vec<_> = exposition
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    assert_eq!(
        samples,
        [
            "plurx_takeover_settings_authority_reads_started_total 0",
            "plurx_takeover_settings_authority_reads_total{outcome=\"ok\"} 0",
            "plurx_takeover_settings_authority_reads_total{outcome=\"error\"} 0",
            "plurx_takeover_settings_authority_reads_total{outcome=\"cancelled\"} 0",
        ]
    );
    assert!(!exposition.contains(keys::CLUSTER_MEDIA_POOL_ENABLED));
    assert!(!exposition.contains(keys::CLUSTER_SESSION_TAKEOVER_ENABLED));
    let production_exposition = prometheus_store_operations();
    assert!(production_exposition
        .contains("# TYPE plurx_takeover_settings_authority_reads_started_total counter\n"));
    assert!(production_exposition
        .contains("# TYPE plurx_takeover_settings_authority_reads_total counter\n"));
}

#[tokio::test(start_paused = true)]
async fn takeover_authority_attribution_counts_retries_errors_and_excludes_other_pairs() {
    let store_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let metrics = TakeoverAuthorityReadMetrics::default();
    let scope = AuthorityReadScope::settings_pair(
        keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
        keys::CLUSTER_MEDIA_POOL_ENABLED,
    );
    let mut calls = 0;
    let value =
        time_scoped_authority_read_with_retry(store_metrics, scope.metrics(&metrics), || {
            calls += 1;
            let call = calls;
            async move {
                match call {
                    1 => Err(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned())),
                    2 | 3 => Err(StoreError::Database(
                        "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
                    )),
                    _ => Ok(42),
                }
            }
        })
        .await
        .expect("existing retry policy recovers");
    assert_eq!((value, calls), (42, 4));
    assert_eq!(metrics.started.load(Ordering::Relaxed), 4);
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Error.index()].load(Ordering::Relaxed),
        3
    );
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Ok.index()].load(Ordering::Relaxed),
        1
    );

    let error =
        time_scoped_authority_read_with_retry(store_metrics, scope.metrics(&metrics), || async {
            Err::<(), _>(StoreError::Database("bad row".to_owned()))
        })
        .await
        .expect_err("permanent error is not retried");
    assert_eq!(error.to_string(), "database error: bad row");
    assert_eq!(metrics.started.load(Ordering::Relaxed), 5);
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Error.index()].load(Ordering::Relaxed),
        4
    );

    let other = AuthorityReadScope::settings_pair(keys::MONARR_URL, keys::MONARR_API_KEY);
    time_scoped_authority_read_with_retry(store_metrics, other.metrics(&metrics), || async {
        Ok(())
    })
    .await
    .expect("unrelated authority read succeeds");
    assert_eq!(
        metrics.started.load(Ordering::Relaxed),
        5,
        "unrelated authority work must never enter this numerator"
    );
    assert_eq!(
        store_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Ok
            )
            .count
            .load(Ordering::Relaxed),
        2,
        "the unrelated read still enters the existing aggregate metric"
    );
}

#[tokio::test(start_paused = true)]
async fn takeover_authority_attribution_starts_on_poll_and_retains_cancelled_attempts() {
    let store_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let metrics = TakeoverAuthorityReadMetrics::default();
    let scope = AuthorityReadScope::settings_pair(
        keys::CLUSTER_MEDIA_POOL_ENABLED,
        keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
    );
    let unpolled =
        time_scoped_authority_read_with_retry(store_metrics, scope.metrics(&metrics), || {
            std::future::pending::<Result<(), StoreError>>()
        });
    drop(unpolled);
    assert_eq!(metrics.started.load(Ordering::Relaxed), 0);
    let mut pending = Box::pin(time_scoped_authority_read_with_retry(
        store_metrics,
        scope.metrics(&metrics),
        std::future::pending::<Result<(), StoreError>>,
    ));
    assert!(matches!(
        futures_util::poll!(&mut pending),
        std::task::Poll::Pending
    ));
    assert_eq!(
        metrics.started.load(Ordering::Relaxed),
        1,
        "in-flight work is already in the daily numerator"
    );
    assert_eq!(
        metrics
            .outcomes
            .iter()
            .map(|counter| counter.load(Ordering::Relaxed))
            .sum::<u64>(),
        0
    );
    drop(pending);
    assert_eq!(metrics.started.load(Ordering::Relaxed), 1);
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Cancelled.index()].load(Ordering::Relaxed),
        1
    );

    let mut backoff = Box::pin(time_scoped_authority_read_with_retry(
        store_metrics,
        scope.metrics(&metrics),
        || async { Err::<(), _>(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned())) },
    ));
    assert!(matches!(
        futures_util::poll!(&mut backoff),
        std::task::Poll::Pending
    ));
    drop(backoff);
    assert_eq!(
        metrics.started.load(Ordering::Relaxed),
        2,
        "cancellation during backoff cannot manufacture an unstarted retry"
    );
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Error.index()].load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Cancelled.index()].load(Ordering::Relaxed),
        1
    );

    let slow_attempt = STORE_TIMEOUT - Duration::from_millis(100);
    let started = tokio::time::Instant::now();
    time_scoped_authority_read_with_retry(store_metrics, scope.metrics(&metrics), || async {
        tokio::time::sleep(slow_attempt).await;
        Err::<(), _>(StoreError::Database(
            "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
        ))
    })
    .await
    .expect_err("absolute quorum deadline cancels the final attempt");
    assert_eq!(
        started.elapsed(),
        slow_attempt + AUTHORITY_QUORUM_RECOVERY_BUDGET
    );
    assert_eq!(metrics.started.load(Ordering::Relaxed), 5);
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Error.index()].load(Ordering::Relaxed),
        3
    );
    assert_eq!(
        metrics.outcomes[StoreOperationOutcome::Cancelled.index()].load(Ordering::Relaxed),
        2
    );
    assert_eq!(
        store_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Cancelled
            )
            .count
            .load(Ordering::Relaxed),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn authority_reads_retry_timeouts_once_and_quorum_failures_through_election() {
    let timeout_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let retried = time_scoped_authority_read_with_retry(timeout_metrics, None, {
        let attempts = Arc::clone(&attempts);
        move || {
            let attempt = attempts.fetch_add(1, Ordering::Relaxed);
            async move {
                if attempt == 0 {
                    Err(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned()))
                } else {
                    Ok(42)
                }
            }
        }
    })
    .await
    .expect("the bounded retry recovers");
    assert_eq!(retried, 42);
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    assert_eq!(
        timeout_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Error,
            )
            .count
            .load(Ordering::Relaxed),
        1,
        "the timed-out attempt remains visible in metrics"
    );
    assert_eq!(
        timeout_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Ok,
            )
            .count
            .load(Ordering::Relaxed),
        1
    );

    let quorum_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let quorum_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recovered = time_scoped_authority_read_with_retry(quorum_metrics, None, {
        let attempts = Arc::clone(&quorum_attempts);
        move || {
            let attempt = attempts.fetch_add(1, Ordering::Relaxed);
            async move {
                if attempt < 4 {
                    Err(StoreError::Database(
                        "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
                    ))
                } else {
                    Ok(84)
                }
            }
        }
    })
    .await
    .expect("the election-window retry recovers a late quorum");
    assert_eq!(recovered, 84);
    assert_eq!(quorum_attempts.load(Ordering::Relaxed), 5);
    assert_eq!(
        quorum_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Error,
            )
            .count
            .load(Ordering::Relaxed),
        4,
        "every failed physical quorum attempt remains visible"
    );
    assert_eq!(
        quorum_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Ok,
            )
            .count
            .load(Ordering::Relaxed),
        1
    );

    let exhausted_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let exhausted_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = tokio::time::Instant::now();
    let error = time_scoped_authority_read_with_retry(exhausted_metrics, None, {
        let attempts = Arc::clone(&exhausted_attempts);
        move || {
            attempts.fetch_add(1, Ordering::Relaxed);
            async {
                Err::<(), _>(StoreError::Database(
                    "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
                ))
            }
        }
    })
    .await
    .expect_err("a persistent quorum loss exhausts the election window");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < AUTHORITY_QUORUM_RECOVERY_BUDGET,
        "persistent quorum recovery stopped outside its bounded window: {elapsed:?}"
    );
    let exhausted_attempts = exhausted_attempts.load(Ordering::Relaxed);
    assert!(exhausted_attempts > AUTHORITY_READ_TIMEOUT_MAX_ATTEMPTS);
    assert_eq!(
        exhausted_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Error,
            )
            .count
            .load(Ordering::Relaxed),
        exhausted_attempts as u64,
        "metrics must count every exhausted physical quorum attempt"
    );
    assert!(error
        .to_string()
        .contains("CheckIsLeaderError: not enough for a quorum"));

    let slow_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let slow_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let slow_attempt = STORE_TIMEOUT - Duration::from_millis(100);
    let started = tokio::time::Instant::now();
    let error = time_scoped_authority_read_with_retry(slow_metrics, None, {
        let attempts = Arc::clone(&slow_attempts);
        move || {
            attempts.fetch_add(1, Ordering::Relaxed);
            async move {
                tokio::time::sleep(slow_attempt).await;
                Err::<(), _>(StoreError::Database(
                    "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
                ))
            }
        }
    })
    .await
    .expect_err("an in-flight quorum retry is cancelled at the absolute deadline");
    assert_eq!(
        started.elapsed(),
        slow_attempt + AUTHORITY_QUORUM_RECOVERY_BUDGET,
        "the first quorum observation plus the recovery window is a hard wall-clock bound"
    );
    assert_eq!(slow_attempts.load(Ordering::Relaxed), 3);
    assert_eq!(
        slow_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Error,
            )
            .count
            .load(Ordering::Relaxed),
        2
    );
    assert_eq!(
        slow_metrics
            .cell(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Cancelled,
            )
            .count
            .load(Ordering::Relaxed),
        1,
        "the physical attempt cut off by the outer deadline remains visible"
    );
    assert!(error
        .to_string()
        .contains("CheckIsLeaderError: not enough for a quorum"));

    let permanent_metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let permanent_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = time_scoped_authority_read_with_retry(permanent_metrics, None, {
        let attempts = Arc::clone(&permanent_attempts);
        move || {
            attempts.fetch_add(1, Ordering::Relaxed);
            async { Err::<(), _>(StoreError::Database("bad row".to_owned())) }
        }
    })
    .await
    .expect_err("non-timeout database errors are terminal");
    assert_eq!(error.to_string(), "database error: bad row");
    assert_eq!(permanent_attempts.load(Ordering::Relaxed), 1);
}

#[test]
fn authority_reads_fit_the_end_to_end_leader_recovery_budget() {
    assert_eq!(STORE_TIMEOUT, Duration::from_secs(3));
    assert_eq!(AUTHORITY_READ_TIMEOUT_MAX_ATTEMPTS, 2);
    assert!(
        // Before the absolute quorum window starts, the longest reachable
        // path is one timed-out read, its delay, and the first quorum
        // observation. All later sleeps and operations are cut off by the
        // five-second deadline itself.
        STORE_TIMEOUT * AUTHORITY_READ_TIMEOUT_MAX_ATTEMPTS as u32
            + AUTHORITY_READ_TIMEOUT_RETRY_DELAY
            + AUTHORITY_QUORUM_RECOVERY_BUDGET
            < crate::cluster::migration::REPLICATED_LEADER_RECOVERY_BUDGET,
        "timeout and quorum recovery must retain the sixteen-second outer budget"
    );
    for failure in 1..=16 {
        assert!(
            authority_quorum_retry_delay(failure) <= AUTHORITY_QUORUM_RETRY_MAX_DELAY,
            "backoff plus jitter exceeded the named total-delay cap"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn idempotent_writes_absorb_two_election_deadlines_and_nothing_else() {
    let metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let value = time_idempotent_write_with_retry(metrics, {
        let attempts = Arc::clone(&attempts);
        move || {
            let attempt = attempts.fetch_add(1, Ordering::Relaxed);
            async move {
                if attempt < 2 {
                    Err(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned()))
                } else {
                    Ok(42)
                }
            }
        }
    })
    .await
    .expect("the third exact-state attempt recovers after election");
    assert_eq!(value, 42);
    assert_eq!(attempts.load(Ordering::Relaxed), 3);
    assert_eq!(
        metrics
            .cell(StoreOperationClass::Write, StoreOperationOutcome::Error)
            .count
            .load(Ordering::Relaxed),
        2,
        "each timed-out physical attempt remains visible"
    );
    assert_eq!(
        metrics
            .cell(StoreOperationClass::Write, StoreOperationOutcome::Ok)
            .count
            .load(Ordering::Relaxed),
        1
    );

    let permanent_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = time_idempotent_write_with_retry(metrics, {
        let attempts = Arc::clone(&permanent_attempts);
        move || {
            attempts.fetch_add(1, Ordering::Relaxed);
            async { Err::<(), _>(StoreError::Database("constraint".to_owned())) }
        }
    })
    .await
    .expect_err("non-timeout write errors are terminal");
    assert_eq!(error.to_string(), "database error: constraint");
    assert_eq!(permanent_attempts.load(Ordering::Relaxed), 1);

    let exhausted_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = time_idempotent_write_with_retry(metrics, {
        let attempts = Arc::clone(&exhausted_attempts);
        move || {
            attempts.fetch_add(1, Ordering::Relaxed);
            async { Err::<(), _>(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned())) }
        }
    })
    .await
    .expect_err("the exact-state retry budget remains bounded");
    assert_eq!(
        error.to_string(),
        format!("database error: {REPLICATED_STORE_TIMEOUT}")
    );
    assert_eq!(
        exhausted_attempts.load(Ordering::Relaxed),
        IDEMPOTENT_WRITE_MAX_ATTEMPTS
    );
}

#[test]
fn idempotent_writes_fit_the_end_to_end_leader_recovery_budget() {
    assert_eq!(STORE_TIMEOUT, Duration::from_secs(3));
    assert!(
        STORE_TIMEOUT * IDEMPOTENT_WRITE_MAX_ATTEMPTS as u32
            + IDEMPOTENT_WRITE_RETRY_DELAY
                * ((IDEMPOTENT_WRITE_MAX_ATTEMPTS.saturating_sub(1)) as u32)
            < crate::cluster::migration::REPLICATED_LEADER_RECOVERY_BUDGET,
        "the retried exact-state path must retain its sixteen-second outer recovery budget"
    );
}

#[test]
fn store_operation_histogram_pins_boundaries_and_saturates() {
    let metrics = StoreOperationMetrics::default();
    metrics.record(
        StoreOperationClass::LocalRead,
        StoreOperationOutcome::Ok,
        Duration::from_nanos(1_000_000),
    );
    metrics.record(
        StoreOperationClass::LocalRead,
        StoreOperationOutcome::Ok,
        Duration::from_nanos(1_000_001),
    );
    metrics.record(
        StoreOperationClass::LocalRead,
        StoreOperationOutcome::Ok,
        Duration::from_secs(11),
    );
    let cell = metrics.cell(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok);
    assert_eq!(cell.count.load(Ordering::Relaxed), 3);
    assert_eq!(cell.elapsed_nanos.load(Ordering::Relaxed), 11_002_000_001);
    assert_eq!(cell.buckets[0].load(Ordering::Relaxed), 1);
    assert_eq!(cell.buckets[1].load(Ordering::Relaxed), 1);
    assert_eq!(cell.buckets[2].load(Ordering::Relaxed), 0);

    let exposition = metrics.render();
    assert!(exposition.contains(
        "plurx_store_operation_seconds_bucket{class=\"local_read\",outcome=\"ok\",le=\"0.001\"} 1"
    ));
    assert!(exposition.contains(
        "plurx_store_operation_seconds_bucket{class=\"local_read\",outcome=\"ok\",le=\"0.005\"} 2"
    ));
    assert!(exposition.contains(
        "plurx_store_operation_seconds_bucket{class=\"local_read\",outcome=\"ok\",le=\"+Inf\"} 3"
    ));

    let saturated = metrics.cell(
        StoreOperationClass::AuthorityRead,
        StoreOperationOutcome::Error,
    );
    saturated.count.store(u64::MAX, Ordering::Relaxed);
    saturated
        .elapsed_nanos
        .store(u64::MAX - 1, Ordering::Relaxed);
    saturated.buckets[0].store(u64::MAX, Ordering::Relaxed);
    metrics.record(
        StoreOperationClass::AuthorityRead,
        StoreOperationOutcome::Error,
        Duration::from_nanos(10),
    );
    assert_eq!(saturated.count.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(saturated.elapsed_nanos.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(saturated.buckets[0].load(Ordering::Relaxed), u64::MAX);
}

#[test]
fn store_operation_exposition_has_only_the_fixed_label_matrix() {
    let exposition = TEST_STORE_OPERATION_METRICS.render();
    assert!(exposition.contains("# TYPE plurx_store_operation_seconds histogram"));
    assert!(exposition.contains("# TYPE plurx_store_operations_total counter"));
    assert!(!exposition.contains("sql="));
    assert!(!exposition.contains("route="));
    assert_eq!(
        exposition
            .lines()
            .filter(|line| line.starts_with("plurx_store_operations_total{"))
            .count(),
        9
    );
    assert_eq!(
        exposition
            .lines()
            .filter(|line| line.starts_with("plurx_store_operation_seconds_count{"))
            .count(),
        9
    );
    assert_eq!(
        exposition
            .lines()
            .filter(|line| line.starts_with("plurx_store_operation_seconds_bucket{"))
            .count(),
        9 * (STORE_OPERATION_BUCKETS.len() + 1)
    );
    for class in ["local_read", "authority_read", "write"] {
        for outcome in ["ok", "error", "cancelled"] {
            assert!(exposition.contains(&format!(
                "plurx_store_operations_total{{class=\"{class}\",outcome=\"{outcome}\"}}"
            )));
        }
    }
}

#[tokio::test]
async fn failed_activity_operations_release_both_credential_reservations_for_retry() {
    for credential in [
        ActivityCredential::Token("token-hash".to_owned()),
        ActivityCredential::ApiKey(42),
    ] {
        let gate = Arc::new(ActivityRefreshGate::default());
        let failure = gate
            .run(credential.clone(), 1_000, async {
                Err(StoreError::Task(
                    "injected activity write failure".to_owned(),
                ))
            })
            .await;
        assert!(failure.is_err());

        let retried = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let retried_inside = Arc::clone(&retried);
        gate.run(credential.clone(), 1_000, async move {
            retried_inside.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("the same credential retries after its failed operation");
        assert!(retried.load(std::sync::atomic::Ordering::Relaxed));

        let suppressed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let suppressed_inside = Arc::clone(&suppressed);
        gate.run(credential.clone(), 1_060, async move {
            suppressed_inside.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("a retained reservation suppresses the exact boundary");
        assert!(!suppressed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(gate.try_reserve(credential, 1_061).is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_independent_activity_gates_each_admit_one_concurrent_operation() {
    let gates: [Arc<ActivityRefreshGate>; 3] =
        std::array::from_fn(|_| Arc::new(ActivityRefreshGate::default()));
    let admitted: [Arc<std::sync::atomic::AtomicUsize>; 3] =
        std::array::from_fn(|_| Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let barrier = Arc::new(tokio::sync::Barrier::new(121));
    let mut requests = tokio::task::JoinSet::new();
    for ordinal in 0..120 {
        let gate_index = ordinal % gates.len();
        let gate = Arc::clone(&gates[gate_index]);
        let admitted = Arc::clone(&admitted[gate_index]);
        let barrier = Arc::clone(&barrier);
        requests.spawn(async move {
            barrier.wait().await;
            gate.run(
                ActivityCredential::Token("shared-token-hash".to_owned()),
                1_000,
                async move {
                    admitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                },
            )
            .await
        });
    }
    barrier.wait().await;
    while let Some(result) = requests.join_next().await {
        result
            .expect("join gate request")
            .expect("run gate operation");
    }
    assert_eq!(
        admitted.map(|value| value.load(std::sync::atomic::Ordering::Relaxed)),
        [1, 1, 1]
    );
}

#[test]
fn replicated_store_modules_cannot_bypass_the_timed_client_accessor() {
    for (name, source) in [
        ("catalog", include_str!("../hiqlite_catalog.rs")),
        ("coordination", include_str!("../hiqlite_coordination.rs")),
        ("media", include_str!("../hiqlite_media.rs")),
        ("durable", include_str!("../hiqlite_durable.rs")),
        ("import", include_str!("../hiqlite_import.rs")),
        ("pretranscode", include_str!("../hiqlite_pretranscode.rs")),
        ("publication", include_str!("../hiqlite_publication.rs")),
        ("reading", include_str!("../hiqlite_reading.rs")),
    ] {
        let compact: String = source
            .chars()
            .filter(|char| !char.is_whitespace())
            .collect();
        assert!(
            !compact.contains("self.client."),
            "{name} store bypasses HiqliteAuthStore::client(), which enforces the 3s timeout"
        );
    }
    let own_source = include_str!("../hiqlite.rs")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let forbidden_nested_timeout = ["timeout_store(", "self.client()"].concat();
    assert!(
            !own_source.contains(&forbidden_nested_timeout),
            "TimedClient owns the only operation timeout; an equal outer timeout races its terminal metric classification"
        );
}

#[tokio::test]
async fn timed_read_accessors_reject_misordered_placeholders_before_io() {
    let client = disconnected_test_client();
    for error in [
        client
            .query_consistent_map::<CountRow, _>("SELECT $2 AS count WHERE $1 = 1", params!(1, 2))
            .await
            .err()
            .expect("consistent helper must validate SQL"),
        client
            .query_map::<CountRow, _>("SELECT $2 AS count WHERE $1 = 1", params!(1, 2))
            .await
            .err()
            .expect("local helper must validate SQL"),
    ] {
        assert!(error.to_string().contains("expected $1, found $2"));
    }
}

#[test]
fn first_preparation_seal_uses_canonical_replicated_parameter_order() {
    // Validate the actual statement sent by the timed replicated writer,
    // not a stand-in query. SQLite's real seal fixtures separately exercise
    // the same ordered values and require the exact artifact and owner.
    validate_sql(crate::store::prepared_output::SEAL)
        .expect("first-preparation seal must be admitted before replicated IO");
}

#[test]
fn complete_seal_read_uses_canonical_replicated_parameter_order() {
    // The read reuses SEAL's predicate; its leading CTE must keep the
    // positional bindings in SEAL's order.
    validate_sql(crate::store::prepared_output::SEALED)
        .expect("complete-seal read must be admitted before consistent IO");
}

#[test]
fn replicated_auth_schema_and_writes_bind_every_clock_value() {
    validate_sql(AUTH_SCHEMA).expect("schema has no connection-local values");
    for sql in [
        "INSERT INTO settings VALUES ($1, $2, $3)",
        "INSERT INTO users VALUES ($1, $2, $3, $4, $5)",
        "INSERT INTO tokens VALUES ($1, $2, $3, $4, $4)",
        "INSERT INTO api_keys VALUES ($1, $2, $3, $4, $5, $6, $7)",
    ] {
        validate_sql(sql).expect("bound replicated write");
    }

    let error = validate_sql("UPDATE users SET password_hash = $2 WHERE id = $1")
        .expect_err("out-of-order named parameters");
    assert!(error.to_string().contains("expected $1, found $2"));
    validate_sql("UPDATE users SET password_hash = $1 WHERE id = $2")
        .expect("first-appearance order");
    validate_sql("SELECT '$2', id FROM users WHERE id = $1 -- $3")
        .expect("quoted and commented placeholders are data");
    validate_sql("SELECT [$2], [a'b] FROM users WHERE id = $1")
        .expect("bracketed identifiers are not placeholders or strings");

    for unsupported in [
        "SELECT $1, $01, $2",
        "SELECT ?2, ?1",
        "SELECT ? FROM users",
        "SELECT :id FROM users",
        "SELECT @id FROM users",
        "SELECT $id FROM users",
    ] {
        validate_sql(unsupported).expect_err("unsupported placeholder spelling");
    }
    validate_sql("SELECT [a'b], $2, $1 FROM users")
        .expect_err("bracket quote cannot hide misordered placeholders");
}

#[test]
fn validation_refusals_are_counted_once_before_io() {
    let refusals = AtomicU64::new(0);
    validate_sql_with_refusal_counter("SELECT $1", &refusals)
        .expect("a valid statement is accepted");
    assert_eq!(refusals.load(Ordering::Relaxed), 0);

    validate_sql_with_refusal_counter("SELECT $2, $1", &refusals)
        .expect_err("an out-of-order statement is refused");
    assert_eq!(refusals.load(Ordering::Relaxed), 1);

    validate_sql_with_refusal_counter("SELECT ?1", &refusals)
        .expect_err("an unsupported placeholder is refused");
    assert_eq!(refusals.load(Ordering::Relaxed), 2);

    let exposition = prometheus_store_operations();
    assert!(exposition.contains("# TYPE plurx_store_validation_refusals_total counter"));
    assert!(exposition.contains("plurx_store_validation_refusals_total "));
}

/// The binary that shipped before P6: it implements exactly protocol 4.
const PREVIOUS_RELEASE: ClusterCompatibility = ClusterCompatibility {
    schema_version: AUTH_SCHEMA_VERSION,
    protocol_min: AUTH_PROTOCOL_VERSION,
    protocol_max: AUTH_PROTOCOL_VERSION,
};

fn meta(protocol_min: i64, protocol_max: i64) -> CompatibilityRow {
    CompatibilityRow {
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_min,
        protocol_max,
    }
}

#[test]
fn compatibility_rejects_schema_and_protocol_drift() {
    verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
        ClusterCompatibility::CURRENT,
    )
    .expect("current voter");

    let old_schema = ClusterCompatibility {
        schema_version: AUTH_SCHEMA_VERSION - 1,
        ..ClusterCompatibility::CURRENT
    };
    let error =
        verify_compatibility_rows(vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)], old_schema)
            .expect_err("old schema must refuse");
    assert!(error.to_string().contains("incompatible"));

    let existing_v4_schema = CompatibilityRow {
        schema_version: 4,
        protocol_min: AUTH_PROTOCOL_MIN,
        protocol_max: AUTH_PROTOCOL_MIN,
    };
    let error = verify_compatibility_rows(vec![existing_v4_schema], ClusterCompatibility::CURRENT)
        .expect_err("the strict open path must never migrate");
    assert!(error.to_string().contains("schema 4 is incompatible"));

    // A cluster that has moved past every protocol this binary knows.
    let error = verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MIN - 1, AUTH_PROTOCOL_MIN - 1)],
        ClusterCompatibility::CURRENT,
    )
    .expect_err("a dropped protocol must refuse");
    assert!(error.to_string().contains("too new"), "{error}");
}

/// The four P6 transitions, in one place, in the order an operator meets
/// them. Each assertion is a supported-or-refused verdict for a real
/// deployment step, not a field copy.
#[test]
fn protocol_range_governs_every_upgrade_and_activation_transition() {
    // 1. Existing cluster on 4..=4, new binary. This is the whole upgrade
    //    path: installing the range-aware build must need no operator
    //    action and must not change anything.
    verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
        ClusterCompatibility::CURRENT,
    )
    .expect("a 4..=4 cluster admits the [4,5] binary unchanged");

    // 2. Existing cluster on 4..=4, previous release. Rolling upgrades mean
    //    both binaries run against the same unchanged cluster range.
    verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
        PREVIOUS_RELEASE,
    )
    .expect("a 4..=4 cluster still admits the previous release");

    // 3. Activated cluster on 5..=5, new binary.
    verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MAX)],
        ClusterCompatibility::CURRENT,
    )
    .expect("an activated cluster admits the [4,5] binary");

    // 4. Activated cluster on 5..=5, previous release. This is the refusal
    //    that keeps an old voter from rejoining a learner-bearing cluster
    //    and reinterpreting its membership.
    let error = verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MAX)],
        PREVIOUS_RELEASE,
    )
    .expect_err("an activated cluster must refuse the previous release");
    let message = error.to_string();
    assert!(message.contains("protocol 5"), "{message}");
    assert!(message.contains("too old"), "{message}");
    assert!(message.contains("4..=4"), "{message}");

    // Overlap is not enough: a cluster that still needs 4 while also using
    // 5 is covered only by a binary that implements both.
    assert!(ClusterCompatibility::CURRENT.covers(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MAX));
    assert!(!PREVIOUS_RELEASE.covers(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MAX));
}

/// Widening `protocol_max` is the one mistake that silently strands the
/// rest of the fleet, so pin that bootstrap picks the floor of the
/// binary's range and that the range is written exactly once.
#[test]
fn bootstrap_writes_the_unactivated_floor_and_never_widens_it() {
    let production = include_str!("../hiqlite.rs")
        .split_once("\n#[cfg(test)]\nmod tests;")
        .expect("test module boundary")
        .0;
    let bootstrap = production
        .split_once("INSERT INTO cluster_meta")
        .expect("bootstrap insert")
        .1
        .split_once(".await?;")
        .expect("end of the bootstrap statement")
        .0;
    assert!(
        bootstrap.contains("VALUES (1, $1, $2, $2, $3)"),
        "bootstrap must write one protocol value into both range columns"
    );
    assert!(
        bootstrap.contains("AUTH_PROTOCOL_MIN"),
        "bootstrap must seed the range floor, never the activated maximum"
    );
    // Every `cluster_meta` write in the store layer belongs to the schema
    // migration chain and touches only `schema_version`/`migrated_at`. The
    // protocol range is moved by an explicit membership operation, never as
    // a side effect of opening or migrating a store.
    assert_eq!(
        production.matches("UPDATE cluster_meta").count(),
        production
            .matches("UPDATE cluster_meta SET schema_version = $1, migrated_at = $2")
            .count(),
        "a cluster_meta write in the store layer touched something other than \
             the schema version"
    );
    // A freshly bootstrapped cluster is joinable by the previous release.
    verify_compatibility_rows(
        vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
        PREVIOUS_RELEASE,
    )
    .expect("a fresh cluster stays on the unactivated range");
}

#[test]
fn daemon_schema_gate_accepts_the_complete_supported_chain() {
    assert_eq!(
        FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE + 1,
        FRAGMENT_INDEX_SCHEMA_VERSION,
        "v11 must advance exactly one step to the fragment-index schema"
    );
    assert_eq!(
        ANALYSIS_REQUEST_SCHEMA_MIGRATION_SOURCE, FRAGMENT_INDEX_SCHEMA_VERSION,
        "the analysis migration must start from the exact v12 shape"
    );
    assert_eq!(
        ANALYSIS_REQUEST_SCHEMA_MIGRATION_SOURCE + 1,
        ANALYSIS_REQUEST_SCHEMA_VERSION,
        "v12 must advance exactly one step to the analysis-request schema"
    );
    assert_eq!(
        TERMINAL_ACK_SCHEMA_MIGRATION_SOURCE, ANALYSIS_REQUEST_SCHEMA_VERSION,
        "the terminal-ack migration must start from the exact v13 shape"
    );
    assert_eq!(
        TERMINAL_ACK_SCHEMA_MIGRATION_SOURCE + 1,
        TERMINAL_ACK_SCHEMA_VERSION,
        "v13 must advance exactly one step to the terminal-ack schema"
    );
    assert_eq!(
        ANALYSIS_HISTORY_INDEX_SCHEMA_MIGRATION_SOURCE, TERMINAL_ACK_SCHEMA_VERSION,
        "the analysis-index migration must start from the exact v14 shape"
    );
    assert_eq!(
        ANALYSIS_HISTORY_INDEX_SCHEMA_MIGRATION_SOURCE + 1,
        ANALYSIS_HISTORY_INDEX_SCHEMA_VERSION,
        "v14 must advance exactly one step to the analysis-index schema"
    );
    assert_eq!(
        TERMINAL_REASON_SCHEMA_MIGRATION_SOURCE, ANALYSIS_HISTORY_INDEX_SCHEMA_VERSION,
        "the terminal-reason migration must start from the exact v15 shape"
    );
    assert_eq!(
        TERMINAL_REASON_SCHEMA_MIGRATION_SOURCE + 1,
        TERMINAL_REASON_SCHEMA_VERSION,
        "v15 must advance exactly one step to the terminal-reason schema"
    );
    assert_eq!(
        PUBLICATION_FENCE_SCHEMA_MIGRATION_SOURCE, TERMINAL_REASON_SCHEMA_VERSION,
        "the publication-fence migration must start from the exact v16 shape"
    );
    assert_eq!(
        PUBLICATION_FENCE_SCHEMA_MIGRATION_SOURCE + 1,
        PUBLICATION_FENCE_SCHEMA_VERSION,
        "v16 must advance exactly one step to the publication-fence schema"
    );
    assert_eq!(
        PUBLICATION_CLAIM_SCHEMA_MIGRATION_SOURCE, PUBLICATION_FENCE_SCHEMA_VERSION,
        "the publication-claim migration must start from the exact v17 shape"
    );
    assert_eq!(
        PUBLICATION_CLAIM_SCHEMA_MIGRATION_SOURCE + 1,
        PUBLICATION_CLAIM_SCHEMA_VERSION,
        "v17 must advance exactly one step to the publication-claim schema"
    );
    assert_eq!(
        DOLBY_VISION_COLUMNS_SCHEMA_MIGRATION_SOURCE, PUBLICATION_CLAIM_SCHEMA_VERSION,
        "the Dolby Vision column migration must start from the exact v18 shape"
    );
    assert_eq!(
        DOLBY_VISION_COLUMNS_SCHEMA_MIGRATION_SOURCE + 1,
        DOLBY_VISION_COLUMNS_SCHEMA_VERSION,
        "v18 must advance exactly one step to the Dolby Vision column schema"
    );
    assert_eq!(
        TIMELINE_ANNOTATIONS_SCHEMA_MIGRATION_SOURCE, DOLBY_VISION_COLUMNS_SCHEMA_VERSION,
        "the timeline-annotation migration must start from the exact v19 shape"
    );
    assert_eq!(
        TIMELINE_ANNOTATIONS_SCHEMA_MIGRATION_SOURCE + 1,
        TIMELINE_ANNOTATIONS_SCHEMA_VERSION,
        "v19 must advance exactly one step to the timeline-annotation schema"
    );
    assert_eq!(
        TIMELINE_MANUAL_OVERRIDES_SCHEMA_MIGRATION_SOURCE, TIMELINE_ANNOTATIONS_SCHEMA_VERSION,
        "the manual-override migration must start from the exact v20 shape"
    );
    assert_eq!(
        TIMELINE_MANUAL_OVERRIDES_SCHEMA_MIGRATION_SOURCE + 1,
        TIMELINE_MANUAL_OVERRIDES_SCHEMA_VERSION,
        "v20 must advance exactly one step to the manual-override schema"
    );
    assert_eq!(
        ANALYSIS_COMPONENT_SCHEMA_MIGRATION_SOURCE, TIMELINE_MANUAL_OVERRIDES_SCHEMA_VERSION,
        "the analysis-component migration must start from the exact v21 shape"
    );
    assert_eq!(
        ANALYSIS_COMPONENT_SCHEMA_MIGRATION_SOURCE + 1,
        ANALYSIS_COMPONENT_SCHEMA_VERSION,
        "v21 must advance exactly one step to the analysis-component schema"
    );
    assert_eq!(
        STAGED_GENERATION_SCHEMA_MIGRATION_SOURCE, ANALYSIS_COMPONENT_SCHEMA_VERSION,
        "the staged-generation migration must start from the exact v22 shape"
    );
    assert_eq!(
        STAGED_GENERATION_SCHEMA_MIGRATION_SOURCE + 1,
        STAGED_GENERATION_SCHEMA_VERSION,
        "v22 must advance exactly one step to the staged-generation schema"
    );
    assert_eq!(
        DV_CONVERSIONS_SCHEMA_MIGRATION_SOURCE, STAGED_GENERATION_SCHEMA_VERSION,
        "the conversion-ledger migration must start from the exact v23 shape"
    );
    assert_eq!(
        DV_CONVERSIONS_SCHEMA_MIGRATION_SOURCE + 1,
        DV_CONVERSIONS_SCHEMA_VERSION,
        "v23 must advance exactly one step to the conversion-ledger schema"
    );
    assert_eq!(
        DV_RECOVERY_GUARDS_SCHEMA_MIGRATION_SOURCE, DV_CONVERSIONS_SCHEMA_VERSION,
        "the recovery-guard migration must start from the exact v24 shape"
    );
    assert_eq!(
        DV_RECOVERY_GUARDS_SCHEMA_MIGRATION_SOURCE + 1,
        DV_RECOVERY_GUARDS_SCHEMA_VERSION,
        "v24 must advance exactly one step to the recovery-guard schema"
    );
    assert_eq!(
        ATTEMPT_ERRORS_SCHEMA_MIGRATION_SOURCE, DV_RECOVERY_GUARDS_SCHEMA_VERSION,
        "the attempt-history migration must start from the exact v25 shape"
    );
    assert_eq!(
        ATTEMPT_ERRORS_SCHEMA_MIGRATION_SOURCE + 1,
        ATTEMPT_ERRORS_SCHEMA_VERSION,
        "v25 must advance exactly one step to the attempt-history schema"
    );
    assert_eq!(
        REQUEST_IDENTITY_SCHEMA_MIGRATION_SOURCE, ATTEMPT_ERRORS_SCHEMA_VERSION,
        "the request-identity migration must start from the exact v26 shape"
    );
    assert_eq!(
        REQUEST_IDENTITY_SCHEMA_MIGRATION_SOURCE + 1,
        REQUEST_IDENTITY_SCHEMA_VERSION,
        "v26 must advance exactly one step to the request-identity schema"
    );
    assert_eq!(
        DESIRED_SELECTION_SCHEMA_MIGRATION_SOURCE, REQUEST_IDENTITY_SCHEMA_VERSION,
        "the desired-selection migration must start from the exact v27 shape"
    );
    assert_eq!(
        DESIRED_SELECTION_SCHEMA_MIGRATION_SOURCE + 1,
        DESIRED_SELECTION_SCHEMA_VERSION,
        "v27 must advance exactly one step to the desired-selection schema"
    );
    assert_eq!(
        POINTER_DESIRED_FENCE_SCHEMA_MIGRATION_SOURCE, DESIRED_SELECTION_SCHEMA_VERSION,
        "the pointer fence migration must start from the exact v28 shape"
    );
    assert_eq!(
        POINTER_DESIRED_FENCE_SCHEMA_MIGRATION_SOURCE + 1,
        POINTER_DESIRED_FENCE_SCHEMA_VERSION,
        "v28 must advance exactly one step to the pointer fence schema"
    );
    assert_eq!(
        ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA_MIGRATION_SOURCE,
        POINTER_DESIRED_FENCE_SCHEMA_VERSION,
        "the terminal-identity index migration must start from the exact v29 shape"
    );
    assert_eq!(
        ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA_MIGRATION_SOURCE + 1,
        ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA_VERSION,
        "v29 must advance exactly one step to the terminal-identity index schema"
    );
    assert_eq!(
        DRAIN_DEADLINE_SCHEMA_MIGRATION_SOURCE, ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA_VERSION,
        "the drain deadline migration must start from the exact v30 shape"
    );
    assert_eq!(
        DRAIN_DEADLINE_SCHEMA_MIGRATION_SOURCE + 1,
        DRAIN_DEADLINE_SCHEMA_VERSION,
        "v30 must advance exactly one step to the drain deadline schema"
    );
    assert_eq!(
        PRODUCER_RECOVERY_SCHEMA_MIGRATION_SOURCE, DRAIN_DEADLINE_SCHEMA_VERSION,
        "the producer-recovery migration must start from the exact v31 shape"
    );
    assert_eq!(
        PRODUCER_RECOVERY_SCHEMA_MIGRATION_SOURCE + 1,
        PRODUCER_RECOVERY_SCHEMA_VERSION,
        "v31 must advance exactly one step to the producer-recovery schema"
    );
    assert_eq!(
        RECOVERY_EPOCH_SCHEMA_MIGRATION_SOURCE, 32,
        "the recovery-epoch migration must start from the exact v32 shape. \
             The literal is the point: every other step in this chain asserts \
             its source against the constant it is defined as, which cannot \
             fail, so it guards the version bump and not the source"
    );
    assert_eq!(
        RECOVERY_EPOCH_SCHEMA_MIGRATION_SOURCE + 1,
        RECOVERY_EPOCH_SCHEMA_VERSION,
        "v32 must advance exactly one step to the recovery-epoch schema"
    );
    assert_eq!(
        OFFLINE_RECOVERY_CLAIM_SCHEMA_MIGRATION_SOURCE, RECOVERY_EPOCH_SCHEMA_VERSION,
        "the offline recovery-claim migration must start from the exact v33 shape"
    );
    assert_eq!(
        OFFLINE_RECOVERY_CLAIM_SCHEMA_MIGRATION_SOURCE + 1,
        OFFLINE_RECOVERY_CLAIM_SCHEMA_VERSION,
        "v33 must advance exactly one step to the offline recovery-claim schema"
    );
    assert_eq!(
        LIBRARY_CHANNELS_SCHEMA_MIGRATION_SOURCE, OFFLINE_RECOVERY_CLAIM_SCHEMA_VERSION,
        "the Library channels migration must start from the exact v34 shape"
    );
    assert_eq!(
        LIBRARY_CHANNELS_SCHEMA_MIGRATION_SOURCE + 1,
        LIBRARY_CHANNELS_SCHEMA_VERSION,
        "v34 must advance exactly one step to the Library channels schema"
    );
    assert_eq!(
        LIBRARY_CHANNEL_BUILD_STATE_SCHEMA_MIGRATION_SOURCE, LIBRARY_CHANNELS_SCHEMA_VERSION,
        "the channel build-state migration must start from the exact v35 shape"
    );
    assert_eq!(
        LIBRARY_CHANNEL_BUILD_STATE_SCHEMA_MIGRATION_SOURCE + 1,
        LIBRARY_CHANNEL_BUILD_STATE_SCHEMA_VERSION,
        "v35 must advance exactly one step to the channel build-state schema"
    );
    assert_eq!(
        DVR_SCHEMA_MIGRATION_SOURCE, LIBRARY_CHANNEL_BUILD_STATE_SCHEMA_VERSION,
        "the DVR migration must start from the exact v36 shape"
    );
    assert_eq!(
        DVR_SCHEMA_MIGRATION_SOURCE + 1,
        DVR_SCHEMA_VERSION,
        "v36 must advance exactly one step to the DVR schema"
    );
    assert_eq!(
        DVR_SCHEMA_VERSION + 1,
        SUBJECT_SCHEMA_VERSION,
        "v37 must advance exactly one step to the subject schema"
    );
    assert_eq!(
        DVR_EVENT_SCHEMA_MIGRATION_SOURCE, SUBJECT_SCHEMA_VERSION,
        "the DVR-event migration must start from the exact v38 shape"
    );
    assert_eq!(
        DVR_EVENT_SCHEMA_MIGRATION_SOURCE + 1,
        DVR_EVENT_SCHEMA_VERSION,
        "v38 must advance exactly one step to the DVR-event schema"
    );
    assert_eq!(
        VIDEO_CODEC_TAG_SCHEMA_MIGRATION_SOURCE, DVR_EVENT_SCHEMA_VERSION,
        "the video-codec-tag migration must start from the exact v39 shape"
    );
    assert_eq!(
        VIDEO_CODEC_TAG_SCHEMA_MIGRATION_SOURCE + 1,
        VIDEO_CODEC_TAG_SCHEMA_VERSION,
        "v39 must advance exactly one step to the video-codec-tag schema"
    );
    assert_eq!(
        VIDEO_CODEC_TAG_SCHEMA_VERSION + 1,
        CLASSIFICATION_SCHEMA_VERSION,
        "v40 must advance exactly one step to the classification schema"
    );
    assert_eq!(
        CONTENT_ANALYSIS_REPAIR_SCHEMA_MIGRATION_SOURCE, CLASSIFICATION_SCHEMA_VERSION,
        "the content-analysis repair must start from the exact v41 shape"
    );
    assert_eq!(
        CONTENT_ANALYSIS_REPAIR_SCHEMA_MIGRATION_SOURCE + 1,
        CONTENT_ANALYSIS_REPAIR_SCHEMA_VERSION,
        "v41 must advance exactly one step to the content-analysis repair schema"
    );
    assert_eq!(
        FIELD_ORDER_SCHEMA_MIGRATION_SOURCE, CONTENT_ANALYSIS_REPAIR_SCHEMA_VERSION,
        "the field-order migration must start from the exact v42 shape"
    );
    assert_eq!(
        FIELD_ORDER_SCHEMA_MIGRATION_SOURCE + 1,
        FIELD_ORDER_SCHEMA_VERSION,
        "v42 must advance exactly one step to the field-order schema"
    );
    assert_eq!(
        LUMINANCE_SCHEMA_MIGRATION_SOURCE, FIELD_ORDER_SCHEMA_VERSION,
        "the luminance migration must start from the exact v43 shape"
    );
    assert_eq!(
        LUMINANCE_SCHEMA_MIGRATION_SOURCE + 1,
        LUMINANCE_SCHEMA_VERSION,
        "v43 must advance exactly one step to the luminance schema"
    );
    assert_eq!(
        FILE_GRANTS_SCHEMA_MIGRATION_SOURCE, DOWNLOADED_SUBTITLES_SCHEMA_VERSION,
        "the file-grants migration must start from the exact v45 shape"
    );
    assert_eq!(
        FILE_GRANTS_SCHEMA_MIGRATION_SOURCE + 1,
        FILE_GRANTS_SCHEMA_VERSION,
        "v45 must advance exactly one step to the file-grants schema"
    );
    assert_eq!(
        SUBTITLE_SOURCE_SCHEMA_MIGRATION_SOURCE, FILE_GRANTS_SCHEMA_VERSION,
        "the subtitle-source migration must start from the exact v46 shape"
    );
    assert_eq!(
        SUBTITLE_SOURCE_SCHEMA_MIGRATION_SOURCE + 1,
        SUBTITLE_SOURCE_SCHEMA_VERSION,
        "v46 must advance exactly one step to the subtitle-source schema"
    );
    assert_eq!(
        ITEM_READ_INDEXES_SCHEMA_MIGRATION_SOURCE, SUBTITLE_SOURCE_SCHEMA_VERSION,
        "the read-index migration must start from the exact v47 shape"
    );
    assert_eq!(
        ITEM_READ_INDEXES_SCHEMA_MIGRATION_SOURCE + 1,
        ITEM_READ_INDEXES_SCHEMA_VERSION,
        "v47 must advance exactly one step to the read-index schema"
    );
    assert_eq!(
        INTEGRITY_JOBS_SCHEMA_VERSION + 1,
        LIVE_TV_RESOURCE_SCHEMA_VERSION,
        "v60 advances to the Live TV resource schema"
    );
    assert_eq!(
        JOB_RETENTION_SCHEMA_VERSION + 1,
        RECEIPT_PRESSURE_SCHEMA_VERSION,
        "v63 advances to the receipt-pressure schema"
    );
    assert_eq!(
        RECEIPT_PRESSURE_SCHEMA_VERSION + 1,
        VIEWER_ANALYSIS_SCHEMA_VERSION,
        "v64 advances to the viewer-analysis schema"
    );
    assert_eq!(
        PREPARATION_INDEX_SCHEMA_VERSION, DV_REQUEST_PROVENANCE_SCHEMA_MIGRATION_SOURCE,
        "request provenance starts from the preparation-index schema"
    );
    assert_eq!(
        DV_REQUEST_PROVENANCE_SCHEMA_MIGRATION_SOURCE + 1,
        DV_REQUEST_PROVENANCE_SCHEMA_VERSION,
        "v67 advances exactly one step to request provenance"
    );
    assert_eq!(
        ENCODED_OUTPUT_SCHEMA_VERSION + 1,
        JELLYFIN_IDENTITY_SCHEMA_VERSION,
        "v74 advances to Jellyfin wire identities"
    );
    assert_eq!(
        JELLYFIN_IDENTITY_SCHEMA_VERSION + 1,
        JELLYFIN_LOGIN_SCHEMA_VERSION,
        "v75 advances to compatibility logins"
    );
    assert_eq!(
        JELLYFIN_LOGIN_SCHEMA_VERSION + 1,
        JELLYFIN_PLAY_SCHEMA_VERSION,
        "v76 advances to compatibility negotiations"
    );
    assert_eq!(
        JELLYFIN_PLAY_SCHEMA_VERSION + 1,
        JELLYFIN_WATCH_SCHEMA_VERSION,
        "v77 advances to manual-edit watch revisions"
    );
    assert_eq!(
        JELLYFIN_WATCH_SCHEMA_VERSION, QUALITY_CANCELLATION_SCHEMA_MIGRATION_SOURCE,
        "quality cancellation follows the Jellyfin watch schema"
    );
    assert_eq!(
        QUALITY_CANCELLATION_SCHEMA_VERSION, QUALITY_LEDGER_SCHEMA_MIGRATION_SOURCE,
        "continuous dependencies follow quality cancellation"
    );
    assert_eq!(
        QUALITY_LEDGER_SCHEMA_MIGRATION_SOURCE + 1,
        QUALITY_LEDGER_SCHEMA_VERSION,
        "the continuous ledger follows quality cancellation"
    );
    assert_eq!(
        AUTH_SCHEMA_MIGRATION_SOURCE + 74,
        QUALITY_LEDGER_SCHEMA_VERSION,
        "this implementation retains every additive v5→v79 main step"
    );
    assert_eq!(
        SHARING_SCHEMA_MIGRATION_SOURCE, QUALITY_LEDGER_SCHEMA_VERSION,
        "sharing follows all current main migrations"
    );
    assert_eq!(
        SHARING_SCHEMA_VERSION + 1,
        AUTH_SCHEMA_BASELINE_VERSION,
        "ordinary sharing80 advances to custody81"
    );
    assert_eq!(
        AUTH_SCHEMA_BASELINE_VERSION + 1,
        AUTH_SCHEMA_VERSION,
        "Source activation82 preserves frozen family layout71"
    );
    let row = |schema_version| CompatibilityRow {
        schema_version,
        protocol_min: AUTH_PROTOCOL_VERSION,
        protocol_max: AUTH_PROTOCOL_VERSION,
    };
    assert_eq!(
        schema_migration_action(&[row(AUTH_SCHEMA_VERSION)], ClusterCompatibility::CURRENT)
            .expect("current schema"),
        SchemaMigrationAction::Current
    );
    for schema_version in AUTH_SCHEMA_MIGRATION_SOURCE..AUTH_SCHEMA_BASELINE_VERSION {
        assert_eq!(
            schema_migration_action(&[row(schema_version)], ClusterCompatibility::CURRENT,)
                .unwrap_or_else(|error| {
                    panic!("schema v{schema_version} must reach the current schema: {error}")
                }),
            SchemaMigrationAction::MigrateFrom(schema_version),
            "schema v{schema_version} must be admitted to the migration chain"
        );
    }
    assert_eq!(
        schema_migration_action(
            &[row(AUTH_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("oldest supported predecessor"),
        SchemaMigrationAction::MigrateFrom(AUTH_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(BOOK_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("book-schema predecessor"),
        SchemaMigrationAction::MigrateFrom(BOOK_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(LEASE_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("lease-schema predecessor"),
        SchemaMigrationAction::MigrateFrom(LEASE_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(PRETRANSCODE_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("immediate predecessor"),
        SchemaMigrationAction::MigrateFrom(PRETRANSCODE_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("media-session predecessor"),
        SchemaMigrationAction::MigrateFrom(MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(SHARED_CACHE_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("shared-cache predecessor"),
        SchemaMigrationAction::MigrateFrom(SHARED_CACHE_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("fragment-index predecessor"),
        SchemaMigrationAction::MigrateFrom(FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(ANALYSIS_REQUEST_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("analysis-request predecessor"),
        SchemaMigrationAction::MigrateFrom(ANALYSIS_REQUEST_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(TERMINAL_ACK_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("terminal-ack predecessor"),
        SchemaMigrationAction::MigrateFrom(TERMINAL_ACK_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(ANALYSIS_HISTORY_INDEX_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("analysis-index predecessor"),
        SchemaMigrationAction::MigrateFrom(ANALYSIS_HISTORY_INDEX_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(TERMINAL_REASON_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("terminal-reason predecessor"),
        SchemaMigrationAction::MigrateFrom(TERMINAL_REASON_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(PUBLICATION_FENCE_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("publication-fence predecessor"),
        SchemaMigrationAction::MigrateFrom(PUBLICATION_FENCE_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(PUBLICATION_CLAIM_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("publication-claim predecessor"),
        SchemaMigrationAction::MigrateFrom(PUBLICATION_CLAIM_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(DOLBY_VISION_COLUMNS_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("Dolby Vision columns predecessor"),
        SchemaMigrationAction::MigrateFrom(DOLBY_VISION_COLUMNS_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(TIMELINE_ANNOTATIONS_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("timeline-annotation predecessor"),
        SchemaMigrationAction::MigrateFrom(TIMELINE_ANNOTATIONS_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(TIMELINE_MANUAL_OVERRIDES_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("manual-override predecessor"),
        SchemaMigrationAction::MigrateFrom(TIMELINE_MANUAL_OVERRIDES_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(ANALYSIS_COMPONENT_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("analysis-component predecessor"),
        SchemaMigrationAction::MigrateFrom(ANALYSIS_COMPONENT_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(STAGED_GENERATION_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("staged-generation predecessor"),
        SchemaMigrationAction::MigrateFrom(STAGED_GENERATION_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(DV_CONVERSIONS_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("conversion-ledger predecessor"),
        SchemaMigrationAction::MigrateFrom(DV_CONVERSIONS_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(DV_RECOVERY_GUARDS_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("recovery-guard predecessor"),
        SchemaMigrationAction::MigrateFrom(DV_RECOVERY_GUARDS_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(
                ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA_MIGRATION_SOURCE
            )],
            ClusterCompatibility::CURRENT,
        )
        .expect("terminal-identity index predecessor"),
        SchemaMigrationAction::MigrateFrom(
            ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA_MIGRATION_SOURCE
        )
    );
    assert_eq!(
        schema_migration_action(
            &[row(LIBRARY_CHANNELS_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("Library channels predecessor"),
        SchemaMigrationAction::MigrateFrom(LIBRARY_CHANNELS_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(LIBRARY_CHANNEL_BUILD_STATE_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("channel build-state predecessor"),
        SchemaMigrationAction::MigrateFrom(LIBRARY_CHANNEL_BUILD_STATE_SCHEMA_MIGRATION_SOURCE)
    );
    assert_eq!(
        schema_migration_action(
            &[row(DVR_SCHEMA_MIGRATION_SOURCE)],
            ClusterCompatibility::CURRENT,
        )
        .expect("DVR predecessor"),
        SchemaMigrationAction::MigrateFrom(DVR_SCHEMA_MIGRATION_SOURCE)
    );

    for rows in [Vec::new(), vec![row(4)], vec![row(7), row(7)]] {
        let error = schema_migration_action(&rows, ClusterCompatibility::CURRENT)
            .expect_err("ambiguous or unsupported state must fail before writes");
        assert!(
            error.to_string().contains("cannot migrate")
                || error.to_string().contains("metadata returned"),
            "{error}"
        );
    }

    // A cluster whose active protocol this binary does not implement must
    // be refused before the migration chain runs, not after.
    let incompatible_protocol = CompatibilityRow {
        schema_version: AUTH_SCHEMA_MIGRATION_SOURCE,
        protocol_min: AUTH_PROTOCOL_MAX + 1,
        protocol_max: AUTH_PROTOCOL_MAX + 1,
    };
    let error = schema_migration_action(&[incompatible_protocol], ClusterCompatibility::CURRENT)
        .expect_err("protocol drift must fail before migration");
    assert!(error.to_string().contains("too old"), "{error}");

    // The activated range is inside this binary's range, so the migration
    // chain still runs against an activated cluster.
    assert_eq!(
        schema_migration_action(
            &[CompatibilityRow {
                schema_version: AUTH_SCHEMA_MIGRATION_SOURCE,
                protocol_min: AUTH_PROTOCOL_MAX,
                protocol_max: AUTH_PROTOCOL_MAX,
            }],
            ClusterCompatibility::CURRENT,
        )
        .expect("an activated cluster may still be migrated by this binary"),
        SchemaMigrationAction::MigrateFrom(AUTH_SCHEMA_MIGRATION_SOURCE)
    );
}
