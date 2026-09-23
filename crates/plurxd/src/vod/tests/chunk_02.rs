
    /// Liveness is the engine's half, and it answers before the slot is
    /// touched: a session that has ended takes no successor, and one that is
    /// gone from the registry has no gate at all.
    #[tokio::test]
    async fn an_ended_vod_session_takes_no_successor() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let gate = serve.preparation_gate(&session_id).await.expect("gate");

        serve
            .shared
            .sessions
            .lock()
            .await
            .get_mut(&session_id)
            .expect("session")
            .tombstone = Some(Terminal::Deleted);
        assert!(
            !gate
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                    None,
                )
                .await,
            "a tombstoned session takes no successor",
        );
        assert!(
            serve.preparation_gate(&session_id).await.is_none(),
            "and hands out no further gate",
        );

        serve.shared.sessions.lock().await.remove(&session_id);
        assert!(
            !gate
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                    None,
                )
                .await,
            "nor does a gate outliving its session",
        );
        assert!(serve.preparation_gate(&session_id).await.is_none());
    }

    #[tokio::test]
    async fn terminal_cleanup_compacts_the_registry_before_publishing_completion() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        let weak = Arc::downgrade(&rendition);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        drop(rendition);

        assert!(serve.end(&session_id, Terminal::Deleted).await);
        let sessions = serve.shared.sessions.lock().await;
        let terminal = sessions.get(&session_id).expect("compact terminal owner");
        assert!(terminal
            .terminal_cleanup
            .as_ref()
            .is_some_and(|cleanup| cleanup.is_finished()));
        assert!(
            terminal.rendition.is_none(),
            "cleanup completion is not visible before the strong graph is released"
        );
        drop(sessions);
        assert!(
            weak.upgrade().is_none(),
            "the compact 410 owner retains no rendition/media/process graph"
        );
        assert!(matches!(
            serve
                .playlist(&session_id)
                .await
                .expect("compact terminal remains addressable")
                .result,
            Err(VodError::Gone(Terminal::Deleted))
        ));
    }

    #[tokio::test]
    async fn one_rendition_build_gate_does_not_block_other_keys_or_purge_maintenance() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        let key = rendition.key.clone();
        *rendition.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(key.clone(), Arc::clone(&rendition));

        let gate = serve.shared.rendition_build_gate(&key);
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let paused_build = tokio::spawn(async move {
            let _guard = gate.lock_owned().await;
            let _ = held_tx.send(());
            std::future::pending::<()>().await;
        });
        held_rx.await.expect("key A gate held");

        let other = serve.shared.rendition_build_gate("unrelated-key");
        let other_guard = tokio::time::timeout(Duration::from_millis(250), other.lock_owned())
            .await
            .expect("key B must not queue behind key A");
        drop(other_guard);
        let registry =
            tokio::time::timeout(Duration::from_millis(250), serve.shared.renditions.lock())
                .await
                .expect("a paused key build must not own the node-wide registry");
        drop(registry);
        tokio::time::timeout(
            Duration::from_millis(250),
            serve.shared.purge_if_dormant(&key, Duration::ZERO),
        )
        .await
        .expect("maintenance must skip an in-flight key rather than wait");
        assert!(serve.shared.renditions.lock().await.contains_key(&key));

        paused_build.abort();
        assert!(paused_build
            .await
            .expect_err("paused build is cancelled")
            .is_cancelled());
        let released = serve.shared.rendition_build_gate(&key);
        let released_guard =
            tokio::time::timeout(Duration::from_millis(250), released.lock_owned())
                .await
                .expect("cancellation releases exactly key A");
        drop(released_guard);
    }

    #[tokio::test]
    async fn cancelled_real_attach_is_accounted_reusable_and_purgeable() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let source_path = base.path().join("source.mkv");
        tokio::fs::write(&source_path, b"x")
            .await
            .expect("write source fence fixture");

        let index = synthetic_index(240);
        let duration_ms = index_video_ms(&index);
        let identity = SourceIdentity::new(1, 1, "fingerprint");
        let recipe = Recipe {
            file: media_file_at(source_path, duration_ms),
            audio_index: None,
            aac: true,
            video: CopyVideoOptions::new(false, false),
            source_object_version: None,
            cluster_cache_key: None,
            encoding: None,
        };
        let key = rendition_key(&recipe, &identity);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &shipped_policy(index.timescale),
            &track_durations(&index, &recipe, duration_ms),
        );

        // Plant one verifiable member so the real adoption path publishes a
        // non-zero working-set claim before the cancellation point.
        let dir = RenditionDir::new(base.path().join(&key));
        dir.create().await.expect("create adopted rendition");
        let mut planted = Manifest::new(plan);
        dir.materialize(&mut planted, 0, b"adopted-segment", now_ms())
            .await
            .expect("plant adopted segment");
        let adopted_bytes = planted.materialized_bytes();
        dir.write_init(b"fixture-init")
            .await
            .expect("plant adopted init");
        store_identity(
            &dir.path().join(IDENTITY_NAME),
            &InitIdentity {
                muxer_init: "fixture-muxer".to_owned(),
                served_init: "fixture-served".to_owned(),
                promotion: plurx_core::fmp4::PromotionInputs::default(),
            },
        )
        .await
        .expect("plant adopted identity");

        let install_pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .rendition_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&install_pause));
        let pending = {
            let shared = Arc::clone(&serve.shared);
            let key = key.clone();
            let identity = identity.clone();
            let index = index.clone();
            let recipe = recipe.clone();
            let settings = settings();
            tokio::spawn(async move {
                shared
                    .attach_rendition(&key, &identity, Some(index), recipe, duration_ms, &settings)
                    .await
            })
        };
        install_pause.wait().await;

        let installed = serve
            .shared
            .renditions
            .lock()
            .await
            .get(&key)
            .map(Arc::clone)
            .expect("real attach installed the rendition");
        assert_eq!(serve.shared.working_set.load(Relaxed), adopted_bytes);
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(
            serve.shared.renditions.lock().await.contains_key(&key),
            "purge must skip the exact key while attach still owns its gate"
        );

        pending.abort();
        assert!(
            matches!(
                pending.await,
                Err(error) if error.is_cancelled()
            ),
            "attach is cancelled after publication"
        );
        install_pause.wait().await;
        *serve
            .shared
            .rendition_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

        let reused = serve
            .shared
            .attach_rendition(
                &key,
                &identity,
                Some(index),
                recipe,
                duration_ms,
                &settings(),
            )
            .await
            .expect("same-key retry")
            .expect("non-empty rendition");
        assert!(Arc::ptr_eq(&installed, &reused.rendition));
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            adopted_bytes,
            "same-key reuse must not double-count adopted bytes"
        );
        drop(reused);

        tokio::time::sleep(Duration::from_millis(1)).await;
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(!serve.shared.renditions.lock().await.contains_key(&key));
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            0,
            "purge releases the exact installed rendition's byte claim"
        );
    }

    #[tokio::test]
    async fn cancelled_dormant_purge_keeps_key_and_accounting_owned_until_settlement() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"owned-dormant-bytes", now_ms())
                .await
                .expect("materialize dormant member");
        }
        let claimed = rendition.manifest.lock().await.materialized_bytes();
        serve.shared.working_set.fetch_add(claimed, Relaxed);
        tokio::fs::write(rendition.identity_path(), b"identity")
            .await
            .expect("write dormant identity");
        *rendition.dormant_since.lock().expect("dormant lock") =
            Some(Instant::now() - Duration::from_secs(1));
        let key = rendition.key.clone();
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(key.clone(), Arc::clone(&rendition));

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .dormant_purge_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let caller = tokio::spawn({
            let shared = Arc::clone(&serve.shared);
            let key = key.clone();
            async move { shared.purge_if_dormant(&key, Duration::ZERO).await }
        });
        pause.wait().await;
        assert!(!serve.shared.renditions.lock().await.contains_key(&key));
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            claimed,
            "accounting remains owned while detached cleanup is paused"
        );
        caller.abort();
        assert!(caller
            .await
            .expect_err("maintenance caller cancelled")
            .is_cancelled());

        let retry_gate = serve.shared.rendition_build_gate(&key);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), retry_gate.lock_owned())
                .await
                .is_err(),
            "same-key rebuild cannot overlap removed rendition settlement"
        );
        pause.wait().await;
        wait_until(
            "detached dormant settlement",
            Duration::from_secs(2),
            || {
                let serve = Arc::clone(&serve);
                let identity = rendition.identity_path();
                async move {
                    serve.shared.working_set.load(Relaxed) == 0
                        && tokio::fs::metadata(identity).await.is_err()
                }
            },
        )
        .await;
        let retry_gate = serve.shared.rendition_build_gate(&key);
        let retry = tokio::time::timeout(Duration::from_secs(2), retry_gate.lock_owned())
            .await
            .expect("same-key rebuild authority releases after exact settlement");
        drop(retry);
        *serve
            .shared
            .dormant_purge_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_head_regeneration_owner_kills_and_confirms_child_reap() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let regeneration = tokio::spawn(async move {
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .expect("deterministic head-regeneration child");
            let pid = child.id().expect("child pid");
            let owner = HeadChildOwner::new(child);
            let _ = started_tx.send(pid);
            std::future::pending::<()>().await;
            drop(owner);
        });
        let pid = started_rx.await.expect("child started");
        regeneration.abort();
        assert!(regeneration
            .await
            .expect_err("head regeneration is cancelled")
            .is_cancelled());

        wait_until(
            "cancelled head child confirmed reaped",
            Duration::from_secs(2),
            || async {
                let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
                result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            },
        )
        .await;
    }

    #[tokio::test]
    async fn head_regeneration_admission_and_pre_init_buffer_are_hard_bounded() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let slots = Arc::clone(&serve.shared.head_regeneration_slots);
        let all = Arc::clone(&slots)
            .acquire_many_owned(HEAD_REGENERATION_CAPACITY as u32)
            .await
            .expect("reserve every head slot");
        assert!(
            Arc::clone(&slots).try_acquire_owned().is_err(),
            "head-regeneration exhaustion is an immediate typed refusal point"
        );
        drop(all);
        let key_a = Arc::clone(&slots)
            .try_acquire_owned()
            .expect("key A head slot");
        let key_b = Arc::clone(&slots)
            .try_acquire_owned()
            .expect("key B is not serialized behind key A");
        drop((key_a, key_b));

        use tokio::io::AsyncWriteExt as _;
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let write = tokio::spawn(async move {
            let mut oversized = vec![0_u8; 4096];
            oversized[..4].copy_from_slice(&8192_u32.to_be_bytes());
            oversized[4..8].copy_from_slice(b"free");
            writer
                .write_all(&oversized)
                .await
                .expect("write malformed oversized head");
        });
        let error = read_muxer_init_bounded(&mut reader, 4096)
            .await
            .expect_err("an init absent at the exact byte ceiling is refused");
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        write.await.expect("bounded head writer");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_build_waiter_cannot_release_same_key_before_confirmed_head_reap() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let key = "cancelled-head-key";
        let gate = serve.shared.rendition_build_gate(key);
        let reap_pause = Arc::new(tokio::sync::Barrier::new(2));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let result = spawn_cancellation_independent({
            let reap_pause = Arc::clone(&reap_pause);
            async move {
                let _build_guard = gate.lock_owned().await;
                let child = tokio::process::Command::new("sleep")
                    .arg("60")
                    .kill_on_drop(true)
                    .spawn()
                    .expect("deterministic head child");
                let pid = child.id().expect("head child pid");
                let mut owner = HeadChildOwner::with_reap_pause(child, reap_pause);
                let _ = started_tx.send(pid);
                owner.terminate_and_reap().await;
            }
        });
        let pid = started_rx.await.expect("head child started");
        drop(result); // the request waiting for the build result is cancelled
        reap_pause.wait().await;

        let retry_gate = serve.shared.rendition_build_gate(key);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), retry_gate.lock_owned())
                .await
                .is_err(),
            "same-key retry cannot acquire spawn authority while reap is paused"
        );
        reap_pause.wait().await;
        let retry_gate = serve.shared.rendition_build_gate(key);
        let retry = tokio::time::timeout(Duration::from_secs(2), retry_gate.lock_owned())
            .await
            .expect("same-key authority releases after confirmed reap");
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        drop(retry);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn production_head_deadline_reaps_child_and_releases_build_gate() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let key = "head-timeout";
        let gate = serve.shared.rendition_build_gate(key);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let timed = tokio::spawn(async move {
            let _build_guard = gate.lock_owned().await;
            let mut child = tokio::process::Command::new("sleep")
                .arg("60")
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("deterministic head-regeneration child");
            let stdout = child.stdout.take().expect("child stdout");
            let pid = child.id().expect("child pid");
            let _ = started_tx.send(pid);
            read_regenerated_head_before(child, stdout, Duration::from_millis(25), 1024).await
        });
        let pid = started_rx
            .await
            .expect("head child started under build gate");
        let error = tokio::time::timeout(Duration::from_secs(2), timed)
            .await
            .expect("production head deadline must settle")
            .expect("head deadline task")
            .expect_err("a silent child cannot produce an init");
        assert!(
            error.to_string().contains("exceeded its 0.0s deadline"),
            "{error}"
        );
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "deadline return requires confirmed child reap"
        );

        let released = serve.shared.rendition_build_gate(key);
        let released_guard =
            tokio::time::timeout(Duration::from_millis(250), released.lock_owned())
                .await
                .expect("head timeout releases the exact build gate");
        drop(released_guard);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_tombstone_replays_410_for_the_retention_window_then_releases() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());
        let rendition = synthetic_rendition(base.path()).await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_ended_route(store.as_ref(), &session_id, &generation).await;
        insert_finished_terminal_session(&serve, &session_id, rendition).await;

        let publication = serve
            .playlist(&session_id)
            .await
            .expect("terminal owner retained");
        assert!(matches!(
            publication.result,
            Err(VodError::Gone(Terminal::Deleted))
        ));
        assert!(
            serve
                .response_status_owner_is_current(&session_id, &publication.owner)
                .await
        );

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION - Duration::from_millis(1)).await;
        serve.maintain().await;
        assert!(matches!(
            serve
                .playlist(&session_id)
                .await
                .expect("410 owner retained through the documented window")
                .result,
            Err(VodError::Gone(Terminal::Deleted))
        ));

        tokio::time::advance(Duration::from_millis(1)).await;
        serve.maintain().await;
        assert!(serve.playlist(&session_id).await.is_none());
        assert!(
            !serve
                .response_status_owner_is_current(&session_id, &publication.owner)
                .await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_graphs_compact_immediately_and_route_eviction_is_fair_and_fail_closed() {
        const ENDED_COUNT: usize = TERMINAL_ROUTE_CONFIRM_BATCH * 2 + 3;
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());
        let rendition = synthetic_rendition(base.path()).await;
        let mut ended_ids = Vec::new();
        for _ in 0..ENDED_COUNT {
            let session_id = uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            activate_ended_route(store.as_ref(), &session_id, &generation).await;
            insert_finished_terminal_session(&serve, &session_id, Arc::clone(&rendition)).await;
            ended_ids.push(session_id);
        }

        let active_id = uuid::Uuid::new_v4().to_string();
        activate_control_route(
            store.as_ref(),
            &active_id,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await;
        insert_finished_terminal_session(&serve, &active_id, Arc::clone(&rendition)).await;

        let timeout_id = uuid::Uuid::new_v4().to_string();
        activate_ended_route(
            store.as_ref(),
            &timeout_id,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await;
        let error_id = uuid::Uuid::new_v4().to_string();
        activate_ended_route(store.as_ref(), &error_id, &uuid::Uuid::new_v4().to_string()).await;
        insert_finished_terminal_session(&serve, &timeout_id, Arc::clone(&rendition)).await;
        insert_finished_terminal_session(&serve, &error_id, Arc::clone(&rendition)).await;
        {
            let mut outcomes = serve
                .shared
                .terminal_route_test_outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for session_id in &ended_ids {
                outcomes.insert(session_id.clone(), TerminalRouteTestOutcome::Success);
            }
            outcomes.insert(timeout_id.clone(), TerminalRouteTestOutcome::Timeout);
            outcomes.insert(error_id.clone(), TerminalRouteTestOutcome::Error);
        }

        let total = ENDED_COUNT + 3;
        let sessions = serve.shared.sessions.lock().await;
        assert_eq!(sessions.len(), total);
        assert_eq!(
            sessions
                .values()
                .filter(|session| session.rendition.is_some())
                .count(),
            0,
            "completed terminal cleanup retains no heavyweight graph even before maintenance"
        );
        drop(sessions);

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION).await;
        for _ in 0..8 {
            serve.maintain().await;
        }
        let sessions = serve.shared.sessions.lock().await;
        assert!(ended_ids.iter().all(|id| !sessions.contains_key(id)));
        assert!(
            sessions.contains_key(&active_id),
            "an active durable route is retained"
        );
        assert!(
            sessions.contains_key(&timeout_id),
            "Store timeout fails closed"
        );
        assert!(sessions.contains_key(&error_id), "Store error fails closed");
        assert_eq!(
            sessions.len(),
            3,
            "bounded rotating batches eventually reach every successful candidate"
        );
    }

    #[tokio::test]
    async fn the_playlist_bytes_are_identical_across_the_sessions_whole_life() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let first = serve
            .playlist("sess-a")
            .await
            .expect("a vod session")
            .expect("playlist bytes")
            .0;
        let text = String::from_utf8(first.clone()).expect("utf8 playlist");
        assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"), "{text}");
        assert!(text.ends_with("#EXT-X-ENDLIST\n"), "{text}");
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:0\n"), "{text}");

        // A blocking fetch in the middle of its life...
        fetch(&serve, "sess-a", "seg00000.m4s").await;

        // ...and the playlist has not moved a byte.
        let second = serve
            .playlist("sess-a")
            .await
            .expect("a vod session")
            .expect("playlist bytes")
            .0;
        assert_eq!(first, second, "the playlist is immutable (plan §2.1)");
    }

    /// A capacity stall says why, after the producer it terminated is gone.
    ///
    /// A hold with no scheduled end terminates its producer — a stopped one
    /// goes on holding everything a running one held — which leaves the belief
    /// `Absent` and takes the reason with it. The status then said `waiting`
    /// with no hold at all, so a viewer's control plane saw a producer stop
    /// and never saw that the working set was full. `hold_reason` is the only
    /// fact that distinguishes "nothing is arriving because there is no room"
    /// from "nothing is arriving".
    #[tokio::test]
    async fn a_capacity_stall_still_says_no_room_after_its_producer_is_gone() {
        // Hand-built, because `driver_pass` rewrites `capacity_hold` on every
        // pass and `create` starts a producer in the background — between them
        // they decide both halves of what this test is asserting, and which one
        // wins is a race.
        //
        // So what is proved here is the reporting half: a hold recorded against
        // a rendition with no producer still reaches the wire, which is the arm
        // the regression lived in. That a hold with no scheduled end terminates
        // its producer is a different claim, and no test makes it.
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, "sess-a", Arc::clone(&rendition), Instant::now()).await;

        assert_eq!(
            serve.status("sess-a").await.expect("status").producer_hold,
            None,
            "a healthy rendition is not holding"
        );

        *rendition.capacity_hold.lock().expect("capacity hold") =
            Some(crate::prodsched::Hold::NoRoom { wanted: 4_096 });
        let held = serve.status("sess-a").await.expect("status");
        assert_eq!(
            held.producer_hold,
            Some("no_room"),
            "the reason outlives the producer it terminated"
        );

        // And it is cleared by the first pass that decides anything else,
        // rather than being latched for the life of the rendition.
        *rendition.capacity_hold.lock().expect("capacity hold") = None;
        assert_eq!(
            serve.status("sess-a").await.expect("status").producer_hold,
            None
        );
    }

    /// Reattachment removes a reader without going through `detach_reader`,
    /// so it needs the same wait retirement.
    ///
    /// A viewer who creates again lands on a different rendition; anything
    /// still parked on the one they left has no reader to be ranked against,
    /// and `playback_demands` marks a session's oldest wait foreground on
    /// exactly that absence — keeping the abandoned rendition producing for
    /// somebody who is no longer watching it.
    #[tokio::test]
    async fn reattaching_elsewhere_retires_the_waits_left_behind() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let first = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("the first rendition");

        let _parked = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: first.key.clone(),
                    index: 40,
                },
                "sess-a",
            )
            .expect("a parked request on the rendition being left");
        assert_eq!(serve.shared.pool.demands(&first.key).len(), 1);

        // Same session, a different recipe: a different rendition key, and the
        // reader moves to it.
        let mut moved = request("play-a", 0.0);
        moved.kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        serve
            .try_create(
                &moved,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-a".to_string(),
            )
            .await
            .expect("the replacement is VOD-presentable");
        let second = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("the replacement rendition");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the fixture must actually move the viewer to another rendition"
        );

        assert!(
            serve.shared.pool.demands(&first.key).is_empty(),
            "the request left behind goes with the reader that made it"
        );
    }

    /// Reattachment also stops the subtitle window the viewer left behind, and
    /// does it without fencing the id they are still watching under.
    ///
    /// A window is keyed by session id alone, so a viewer moving to another
    /// rendition leaves a live ffmpeg extracting a span for the recipe they
    /// moved off. `detach_reader` releases one on every real ending and its
    /// comment claimed reattachment converged there too; it did not, and it
    /// must not — releasing fences the id for half a minute, and this id
    /// belongs to somebody who is still watching, so the fence would refuse
    /// the first window of the attachment that just replaced it. The flight is
    /// stopped by name instead, which is what this pins from the serving side:
    /// gone, and the session still able to start the next one.
    #[tokio::test]
    async fn reattaching_elsewhere_stops_the_window_flight_it_left_behind() {
        // The window registry is process-global and keyed by session id
        // alone, so this fixture cannot use the shared `sess-a` literal: a
        // concurrent test releasing that id would fence or clear this one's
        // flight and the failure would read as a defect in the code.
        let session_id = &uuid::Uuid::new_v4().to_string();
        let base = crate::test_tempdir().expect("base");
        let (serve, mut file) = serve_on(base.path()).await;
        let subs = crate::test_tempdir().expect("subs");
        // Long enough that a window is a fraction of the runtime, which is the
        // only condition windowing asks about. The extraction itself is
        // injected, so nothing here reads the media.
        file.duration_ms = Some(4 * 60 * 60 * 1_000);
        create(&serve, &file, session_id, "play-a", &settings()).await;
        let first = rendition_of(&serve, session_id).await;

        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let hold = Arc::new(tokio::sync::Semaphore::new(0));
        let warm = |anchor: i64| {
            let started = Arc::clone(&started);
            let hold = Arc::clone(&hold);
            let subs_dir = subs.path().to_owned();
            let file = file.clone();
            async move {
                crate::subtitles::warm_vtt_window_with(
                    session_id,
                    Some(1),
                    &subs_dir,
                    &file,
                    0,
                    anchor,
                    30,
                    move |_tmp, _, _, _, _| async move {
                        started.add_permits(1);
                        hold.acquire().await.expect("hold").forget();
                        Ok(())
                    },
                )
                .await
            }
        };
        let running = || {
            let started = Arc::clone(&started);
            async move {
                tokio::time::timeout(Duration::from_secs(5), started.acquire())
                    .await
                    .expect("the injected extractor starts")
                    .expect("started semaphore remains open")
                    .forget();
            }
        };

        assert!(warm(600).await, "the viewer owns a window on the way in");
        running().await;
        assert!(crate::subtitles::owned_window_for_test(session_id).is_some());

        // Same session, a different recipe: a different rendition key, and the
        // reader moves to it.
        let mut moved = request("play-a", 0.0);
        moved.kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        serve
            .try_create(
                &moved,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                session_id.to_string(),
            )
            .await
            .expect("the replacement is VOD-presentable");
        assert!(
            !Arc::ptr_eq(&first, &rendition_of(&serve, session_id).await),
            "the fixture must actually move the viewer to another rendition"
        );

        assert!(
            crate::subtitles::owned_window_for_test(session_id).is_none(),
            "the flight for the recipe they left does not outlive the move"
        );
        assert!(
            warm(900).await,
            "and the viewer who is still watching is not fenced out of the next one"
        );
        running().await;
        assert_eq!(
            crate::subtitles::peak_window_flights_for_test(session_id),
            1,
            "one live flight per playback, across the move"
        );

        hold.add_permits(8);
        crate::subtitles::release_session_window(session_id).await;
    }

    /// The configured node cap reaches the pool, on create.
    ///
    /// Settings are re-read per create, so that is where the ceiling is
    /// applied — which is what lets an operator change it without a restart.
    /// A cap that lived only at construction would be frozen at whatever the
    /// node booted with, and the plan promised a setting.
    #[tokio::test]
    async fn the_configured_blocked_get_cap_reaches_the_pool() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        assert_eq!(
            serve.shared.pool.global_cap(),
            DEFAULT_GLOBAL_WAIT_CAP,
            "an unconfigured node still bounds its parked work"
        );

        let mut tuned = settings();
        tuned.blocked_get_cap = 3;
        create(&serve, &file, "sess-a", "play-a", &tuned).await;
        assert_eq!(
            serve.shared.pool.global_cap(),
            3,
            "the operator's number, applied where settings are read"
        );

        let mut raised = settings();
        raised.blocked_get_cap = 128;
        create(&serve, &file, "sess-b", "play-b", &raised).await;
        assert_eq!(
            serve.shared.pool.global_cap(),
            128,
            "and it tracks a later change rather than latching the first"
        );
    }

    /// A detached viewer stops being demand at the moment their reader goes,
    /// not when their abandoned request finally times out.
    ///
    /// `playback_demands` ranks a blocked request against the reader that
    /// asked for it. With the reader gone it falls back to marking that
    /// session's oldest wait foreground — so until the request's HTTP deadline
    /// expired, a viewer who had already left outranked one who was still
    /// watching and pointed the producer at media nobody wanted.
    #[tokio::test]
    async fn a_detached_viewer_stops_being_demand_with_its_reader() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");

        let _leaving = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 40,
                },
                "sess-a",
            )
            .expect("a parked request for the leaving viewer");
        let _staying = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 2,
                },
                "sess-b",
            )
            .expect("a parked request for the viewer who stays");
        assert_eq!(serve.shared.pool.demands(&rendition.key).len(), 2);

        rendition.detach_reader(&serve.shared.pool, "sess-a").await;

        let remaining = serve.shared.pool.demands(&rendition.key);
        assert_eq!(
            remaining.len(),
            1,
            "the departing viewer's parked request left with its reader"
        );
        assert_eq!(
            remaining[0].session, "sess-b",
            "and the viewer still watching keeps theirs"
        );
    }

    /// P1-2. A recorded failure carries its class to the session status.
    ///
    /// The prose was always there; the class is what a client can act on.
    /// Without it `DeliveryView::from_status` had nothing to publish, so a
    /// rendition that had genuinely failed still produced `action: none`.
    #[tokio::test]
    async fn a_recorded_failure_publishes_its_class_not_only_its_sentence() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let healthy = serve.status("sess-a").await.expect("status");
        assert_eq!(healthy.producer_decision, None, "nothing has failed");
        assert_eq!(healthy.producer_failed, None);

        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");
        record_failure(
            &serve.shared,
            &rendition,
            Reason::SourceChanged,
            "source changed after the fragment index was selected".to_owned(),
        );

        let failed = serve.status("sess-a").await.expect("status");
        assert_eq!(failed.producer_state, "failed");
        assert_eq!(
            failed.producer_decision,
            Some(Reason::SourceChanged.status()),
            "the class travels with the failure"
        );
        assert_eq!(
            failed.producer_failed.as_deref(),
            Some("source changed after the fragment index was selected"),
            "the operator's sentence is not replaced by the class"
        );
    }

    /// The first failure is the cause; a later one is its consequence.
    ///
    /// A publication fence records `source_changed` and then stops the
    /// generation, which ends by reporting that its sink refused the bytes.
    /// Overwriting would file the consequence as the diagnosis and publish a
    /// class naming the wrong subsystem — `producer_write_failed` for a source
    /// that moved. The sentence must not drift from the class either.
    #[tokio::test]
    async fn a_recorded_cause_is_not_overwritten_by_its_own_consequence() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");

        record_failure(
            &serve.shared,
            &rendition,
            Reason::SourceChanged,
            "source changed before fragment publication".to_owned(),
        );
        record_failure(
            &serve.shared,
            &rendition,
            Reason::ProducerWriteFailed,
            "sink refused: source changed before fragment publication".to_owned(),
        );

        let failure = rendition.failure().expect("a recorded failure");
        assert_eq!(
            failure.decision,
            Reason::SourceChanged,
            "the fence that stopped the generation is the diagnosis"
        );
        assert_eq!(
            failure.cause, "source changed before fragment publication",
            "the sentence stays with the class it was recorded beside"
        );
    }

    /// A source that moves under a rendition is published as a source change,
    /// end to end, not as whatever the generation happened to say on its way
    /// down.
    ///
    /// This is one of the classification choices that had no coverage: the
    /// class chosen at a `record_failure` site is invisible to a test that
    /// supplies its own.
    #[tokio::test]
    async fn a_source_that_moves_is_published_as_a_source_change() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let mut file = fixture_file();
        let shared_fixture = file.path.clone();
        let shared_before = tokio::fs::metadata(&shared_fixture)
            .await
            .expect("shared fixture metadata");
        file.path = base.path().join("source.mkv");
        tokio::fs::copy(&shared_fixture, &file.path)
            .await
            .expect("copy source fence fixture");
        let (serve, file) = serve_on_file(base.path(), file).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        // Move the source under the fence the rendition took at create.
        let mut bytes = tokio::fs::read(&file.path).await.expect("source bytes");
        bytes.extend_from_slice(b"moved");
        tokio::fs::write(&file.path, &bytes)
            .await
            .expect("rewrite the source");
        let shared_after = tokio::fs::metadata(&shared_fixture)
            .await
            .expect("shared fixture metadata after private rewrite");
        assert_eq!(
            (shared_before.len(), shared_before.modified().ok()),
            (shared_after.len(), shared_after.modified().ok()),
            "a source-change test must not rewrite the shared media fixture"
        );

        let answer = serve.segment("sess-a", "seg00000.m4s").await;
        assert!(
            matches!(
                answer,
                Some(VodPublication {
                    result: Err(VodError::ProducerFailed(_)),
                    ..
                })
            ),
            "a moved source must refuse the GET, not serve stale media"
        );

        let status = serve.status("sess-a").await.expect("status");
        assert_eq!(status.producer_state, "failed");
        assert_eq!(
            status.producer_decision,
            Some(Reason::SourceChanged.status()),
            "the class names what actually changed"
        );
        assert!(
            !Reason::SourceChanged.is_permanent(),
            "a moved source is a statement about this plan: a fresh create \
             re-plans against the file that is there now"
        );
    }

    /// Every generation outcome has a class, and each names what happened.
    ///
    /// A `match` that fell through to one bucket would compile and would make
    /// the whole vocabulary decorative, so this pins each arm. `Stream` maps
    /// onto rolling's existing `ReaderFailed` because it is the same fact —
    /// reading the producer's output failed — while landing and sink faults
    /// have no rolling equivalent and get their own names rather than borrow
    /// one that would misdescribe them.
    #[test]
    fn every_generation_failure_names_what_actually_happened() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let cases = [
            (
                Failure::InitDrift("init".to_owned()),
                Reason::RenditionInitChanged,
            ),
            (
                Failure::EngineChanged("engine".to_owned()),
                Reason::EngineChanged,
            ),
            (
                Failure::Landing("landing".to_owned()),
                Reason::MediaLandingFailed,
            ),
            (Failure::Stream("stream".to_owned()), Reason::ReaderFailed),
            (
                Failure::Sink(io::Error::other("sink")),
                Reason::ProducerWriteFailed,
            ),
        ];
        for (failure, expected) in cases {
            assert_eq!(
                classify_failure(&failure),
                expected,
                "{} must not be filed as something else",
                describe_failure(&failure)
            );
            // Permanence follows what a retry can change, not which subsystem
            // failed. A landing, stream or sink fault is a statement about
            // this attempt, and a fresh create can succeed. An engine change
            // is not: the engine baseline is established once per process, so
            // every reopen re-plans straight back into the same verdict.
            assert_eq!(
                classify_failure(&failure).is_permanent(),
                matches!(
                    expected,
                    Reason::RenditionInitChanged | Reason::EngineChanged
                ),
                "{} is classified with the wrong permanence",
                describe_failure(&failure)
            );
        }
    }

    #[tokio::test]
    async fn specialized_init_drift_records_only_the_rendition_scope() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;

        on_init_drift(
            &serve.shared,
            &rendition,
            "generation init bytes differ from the published identity".to_owned(),
        )
        .await;

        let failure = rendition.failure().expect("drift is recorded");
        assert_eq!(failure.decision, Reason::RenditionInitChanged);
        assert_ne!(failure.decision, Reason::EngineChanged);
        assert!(failure.decision.is_permanent());
        assert!(failure.cause.contains("published identity"));
    }

    /// The seam the P1-3 correction consists of: the meter a segment answer
    /// carries is the meter its own session publishes.
    ///
    /// Every other proof of this change supplies its own meter, which exercises
    /// the pump and the control view in isolation and would keep passing if
    /// `session_rendition` handed out a fresh meter per request — every VOD
    /// session would silently return to `delivered_bps: None`, erasing the
    /// advisory evidence while the whole suite stayed green. This drives the
    /// production entry point, so nothing
    /// here chooses the meter, and reads the count back off the session's own
    /// status rather than off the answer.
    #[tokio::test]
    async fn a_segment_answer_carries_the_meter_its_own_session_publishes() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        create(&serve, &file, "sess-b", "play-b", &settings()).await;

        assert_eq!(
            serve
                .status("sess-a")
                .await
                .expect("status")
                .delivered_bytes,
            0,
            "nothing has been delivered yet"
        );

        let ready = fetch(&serve, "sess-a", "seg00000.m4s").await;
        // Exactly what the response pump does, against the meter production
        // chose rather than one this test made.
        ready.delivery.note(4_096);

        assert_eq!(
            serve
                .status("sess-a")
                .await
                .expect("status")
                .delivered_bytes,
            4_096,
            "the session publishes what its own answer counted"
        );
        assert_eq!(
            serve
                .status("sess-b")
                .await
                .expect("status")
                .delivered_bytes,
            0,
            "two viewers of the same immutable rendition are metered apart"
        );
    }

    #[tokio::test]
    async fn a_blocking_get_materializes_the_segment_and_a_re_get_serves_the_same_bytes() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let first = fetch(&serve, "sess-a", "seg00000.m4s").await;
        let etag = first.etag.clone();
        let bytes = read_ready(first).await;
        assert!(!bytes.is_empty());

        let again = fetch(&serve, "sess-a", "seg00000.m4s").await;
        assert_eq!(again.etag, etag, "a strong etag is stable across GETs");
        assert_eq!(read_ready(again).await, bytes, "same bytes on the re-GET");

        // The init the map names is served with its own strong etag.
        let init = match serve.segment("sess-a", "init.mp4").await {
            Some(VodPublication {
                result: Ok(Some(ready)),
                ..
            }) => ready,
            other => panic!("init.mp4: {}", describe(other)),
        };
        assert!(init.etag.contains("-init-"), "{}", init.etag);
        assert!(init.len > 0);
    }

    #[tokio::test]
    async fn deadline_expiry_answers_a_typed_pending() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        let tight = VodSettings {
            block_budget: Duration::from_millis(1),
            ..settings()
        };
        create(&serve, &file, "sess-a", "play-a", &tight).await;
        let last = plan_len(&serve, "sess-a").await - 1;
        let touched = *serve.shared.sessions.lock().await["sess-a"]
            .last_touch
            .lock()
            .expect("touch lock");

        // The producer cannot possibly reach the last entry inside a
        // millisecond, so the hard deadline fires and the answer is the typed
        // retryable pending — never an open-ended wait, never a bare 404.
        match serve
            .segment("sess-a", &segment_name(last as u64))
            .await
            .expect("a vod session")
            .result
        {
            Err(VodError::Pending { retry_after }) => {
                assert_eq!(retry_after, PENDING_RETRY_AFTER);
            }
            other => panic!("expected Pending, got {other:?}"),
        }
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "a timed-out VOD materialization must not renew the session",
        );
    }

    /// A rendition that has recorded a failure gives its producer back.
    ///
    /// The failure is permanent — first-wins, never cleared — and it answers
    /// every waiter at the moment it lands, so the child that is still running
    /// is producing for nobody. The driver used to return the instant it saw a
    /// failure, and `Action::Idle` is the only thing that reclaims a producer,
    /// so nothing was left to reap it: the dormant purge refuses an admitted
    /// rendition outright and the last `Arc` drop waits on whichever generation
    /// task or watchdog outlives it. This drives the case that costs the most —
    /// a SIGSTOP'd child, which goes on holding the codec session a running one
    /// held — and asserts the physical outcome, that the pid is gone and
    /// reaped, not merely that a belief was rewritten.
    ///
    /// The latched capacity hold is checked here too, and deliberately not
    /// dressed up as a wire defect. The first version of this test asserted
    /// that a failed rendition published `producer_hold: no_room`, and it does
    /// not: `status` answers `failed` ahead of every belief arm, so the stale
    /// hold was already shielded. What is asserted is what is true — the field
    /// is cleared rather than latched for the life of the rendition, so the
    /// next reader of it is not the one who discovers it was never true.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_rendition_reclaims_its_producer_and_drops_its_hold() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;

        wait_until("the producer spawned", Duration::from_secs(10), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.last_child_pid.load(Relaxed) != 0 }
        })
        .await;
        let pid = rendition.last_child_pid.load(Relaxed) as libc::pid_t;

        // The expensive shape: stopped, so it makes no progress, and alive, so
        // it still owns everything a running producer owned.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        *rendition.capacity_hold.lock().expect("capacity hold") =
            Some(crate::prodsched::Hold::NoRoom { wanted: 4_096 });
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the fixture needs a live child to reclaim"
        );

        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::EngineChanged,
            "the fixture recorded a permanent rendition failure".to_owned(),
        );

        wait_until(
            "the producer was reclaimed",
            Duration::from_secs(10),
            || {
                let rendition = Arc::clone(&rendition);
                async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
            },
        )
        .await;
        // Reaped, not merely signalled: `perform(Terminate)` is `start_kill`
        // then `wait`, so a pid that still answers here is a zombie this
        // process owns and never collected.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the failed rendition's child is gone, not left holding its codec session"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "gone because it was reaped, not because signalling was refused"
        );
        assert!(
            rendition
                .capacity_hold
                .lock()
                .expect("capacity hold")
                .is_none(),
            "and the hold is cleared rather than latched for the rendition's life"
        );
        let status = serve.status("sess-a").await.expect("status");
        assert_eq!(status.producer_state, "failed");
        assert_eq!(
            status.producer_hold, None,
            "which the status already answered ahead of the belief, and still does"
        );
    }

    /// One unfinished terminal cleanup cannot stop the node's maintenance.
    ///
    /// `maintain` waits on every unfinished cleanup belonging to a tombstoned
    /// session before it does anything else, and that wait had no bound. A
    /// single cleanup that never completes therefore stopped the idle reap,
    /// the dormant-rendition purge, the tombstone eviction and every driver
    /// kick — for the life of the process, with nothing said.
    ///
    /// Two arms could install one: `begin_end` and the supersession sweep both
    /// wrote the tombstone and the cleanup before reading a rendition that may
    /// be `None`, and both then returned without an owner for it. Both now
    /// complete it, so this fixture has to install one by hand — which is the
    /// point. The bound is what holds when the next change to that ordering
    /// gets it wrong.
    #[tokio::test(start_paused = true)]
    async fn one_unfinished_terminal_cleanup_cannot_wedge_node_maintenance() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());

        let wedged = Arc::new(TerminalCleanup::new());
        insert_terminal_session(
            &serve,
            "sess-wedged",
            synthetic_rendition(base.path()).await,
            Arc::clone(&wedged),
        )
        .await;
        let reapable = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_ended_route(store.as_ref(), &reapable, &generation).await;
        insert_finished_terminal_session(&serve, &reapable, synthetic_rendition(base.path()).await)
            .await;

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION + Duration::from_secs(1)).await;
        // Paused time auto-advances to the next timer whenever the runtime is
        // idle, so the elapsed span is exactly the bound the wait armed — which
        // is what makes this an assertion about the documented number rather
        // than about finiteness. Without the timeout there is no timer to
        // advance to and the pass never returns at all.
        let before = tokio::time::Instant::now();
        serve.maintain().await;
        assert_eq!(
            tokio::time::Instant::now() - before,
            TERMINAL_CLEANUP_MAINTENANCE_WAIT,
            "the pass gave up on the wedged cleanup after exactly the documented bound"
        );

        assert!(
            !wedged.is_finished(),
            "the fixture's whole point is a cleanup that never completes"
        );
        // And the pass ran to its end anyway, which is the thing that matters:
        // the work behind the wait still happened.
        assert!(
            serve.playlist(&reapable).await.is_none(),
            "maintenance past its retention window still evicted the other tombstone"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_killed_producer_child_answers_every_waiter_producer_failed() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;

        // The driver spawns the generation for the session's own demand;
        // freeze it the moment its pid appears so nothing materializes.
        wait_until("the producer spawned", Duration::from_secs(10), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.last_child_pid.load(Relaxed) != 0 }
        })
        .await;
        let pid = rendition.last_child_pid.load(Relaxed);
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);

        let last = plan_len(&serve, "sess-a").await - 1;
        let waiter = {
            let serve = Arc::clone(&serve);
            let name = segment_name(last as u64);
            tokio::spawn(async move { serve.segment("sess-a", &name).await })
        };
        wait_until("the waiter registered", Duration::from_secs(10), || {
            let serve = Arc::clone(&serve);
            let key = rendition.key.clone();
            async move { serve.shared.pool.blocked_on(&key).is_some() }
        })
        .await;

        // Kill the child out from under the wait.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);

        match waiter
            .await
            .expect("waiter task")
            .expect("a vod session")
            .result
        {
            Err(VodError::ProducerFailed(cause)) => {
                assert!(!cause.is_empty());
            }
            other => panic!("a dead producer must answer typed, got {:?}", other),
        }
        // And the failure sticks: a later GET for a planned segment answers
        // the same typed refusal without waiting.
        match serve
            .segment("sess-a", &segment_name(last as u64))
            .await
            .expect("a vod session")
            .result
        {
            Err(VodError::ProducerFailed(_)) => {}
            other => panic!("expected ProducerFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_terminal_cause_answers_gone_and_supersession_spares_the_keeper() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let live_owner = serve.playlist("sess-a").await.expect("live owner").owner;

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(
            !serve
                .response_status_owner_is_current("sess-a", &live_owner)
                .await,
            "a live status snapshot cannot authorize after tombstoning"
        );
        let tombstone = serve.playlist("sess-a").await.expect("still ours");
        assert!(
            serve
                .response_status_owner_is_current("sess-a", &tombstone.owner)
                .await,
            "the exact tombstone snapshot authorizes its typed 410"
        );
        match tombstone.result {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("expected Gone(Deleted), got {other:?}"),
        }
        match serve
            .segment("sess-a", "seg00000.m4s")
            .await
            .expect("still ours")
            .result
        {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("expected Gone, got {other:?}"),
        }
        assert!(
            serve.end("sess-a", Terminal::AdminStop).await,
            "a second end is idempotent and keeps the first cause"
        );
        match serve.playlist("sess-a").await.expect("still ours").result {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("the first cause survives: {other:?}"),
        }
        assert!(serve.owns("sess-a").await, "tombstoned is still ours");
        assert!(!serve.end("sess-x", Terminal::Deleted).await, "not ours");

        // Supersession: two sessions of one playback, the newer one kept.
        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        create(&serve, &file, "sess-c", "play-b", &settings()).await;
        assert_eq!(
            serve.supersede("[\"user_id\",1]", "play-b", "sess-c").await,
            1
        );
        match serve.playlist("sess-b").await.expect("still ours").result {
            Err(VodError::Gone(Terminal::Superseded)) => {}
            other => panic!("expected Gone(Superseded), got {other:?}"),
        }
        assert!(
            serve.playlist("sess-c").await.expect("ours").is_ok(),
            "`keep` must survive the sweep"
        );

        let events = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = serve
                    .shared
                    .store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: None,
                        limit: 20,
                    })
                    .await
                    .expect("VOD lifecycle telemetry query");
                if events
                    .iter()
                    .filter(|event| matches!(event.event.as_str(), "session_start" | "session_end"))
                    .count()
                    >= 5
                {
                    break events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("VOD lifecycle telemetry persisted");
        let starts: Vec<_> = events
            .iter()
            .filter(|event| event.event == "session_start")
            .collect();
        let ends: Vec<_> = events
            .iter()
            .filter(|event| event.event == "session_end")
            .collect();
        assert_eq!(starts.len(), 3, "one start per attached VOD handle");
        assert_eq!(ends.len(), 2, "an idempotent second end emits nothing");
        assert!(starts.iter().all(|event| {
            event.method.as_deref() == Some("remux")
                && event.encoder.as_deref() == Some("vod")
                && event
                    .extra
                    .as_deref()
                    .is_some_and(|extra| extra.contains("\"presentation\":\"vod\""))
        }));
        assert!(ends
            .iter()
            .any(|event| event.reason.as_deref() == Some("client_released")));
        assert!(ends
            .iter()
            .any(|event| event.reason.as_deref() == Some("superseded")));
    }

    #[tokio::test]
    async fn durable_terminal_cause_refines_only_a_provisional_replaced_tombstone() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        assert!(serve.end("sess-a", Terminal::Replaced).await);
        assert!(serve.end("sess-a", Terminal::AdminStop).await);
        assert!(matches!(
            serve.playlist("sess-a").await.expect("still ours").result,
            Err(VodError::Gone(Terminal::AdminStop))
        ));

        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        assert!(serve.end("sess-b", Terminal::Deleted).await);
        assert!(serve.end("sess-b", Terminal::AdminStop).await);
        assert!(matches!(
            serve.playlist("sess-b").await.expect("still ours").result,
            Err(VodError::Gone(Terminal::Deleted))
        ));
    }

    /// Drive a whole small fixture to completion through real blocking GETs,
    /// then prove admission and that a second session on the same recipe
    /// rides the admitted rendition with no producer at all.
    #[tokio::test]
    async fn a_completed_rendition_admits_and_serves_a_second_session_without_a_producer() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let len = plan_len(&serve, "sess-a").await;
        for segment in 0..len {
            fetch(&serve, "sess-a", &segment_name(segment as u64)).await;
        }
        let rendition = rendition_of(&serve, "sess-a").await;
        wait_until("the rendition admitted", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.manifest.lock().await.is_admitted() }
        })
        .await;
        // The finished producer is reclaimed once the driver sees it idle
        // past every gap.
        wait_until("the producer reclaimed", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;

        // A second session on the same recipe attaches to the SAME rendition
        // and every segment is served instantly, no producer spawned.
        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        let second = rendition_of(&serve, "sess-b").await;
        assert!(
            Arc::ptr_eq(&rendition, &second),
            "one rendition per recipe, shared by its sessions"
        );
        for segment in 0..len {
            match serve
                .segment("sess-b", &segment_name(segment as u64))
                .await
                .expect("a vod session")
                .result
            {
                Ok(Some(ready)) => assert!(ready.len > 0),
                other => panic!("segment {segment} must serve instantly: {other:?}"),
            }
        }
        assert!(
            matches!(rendition.slot.belief().await, Producer::Absent { .. }),
            "an admitted rendition serves without spawning a producer"
        );
    }

    /// Resurrection: a new `VodServe` over the same base directory and store
    /// adopts the rendition — identical playlist bytes, materialized segments
    /// served without re-production.
    #[tokio::test]
    async fn a_new_vodserve_on_the_same_base_resurrects_the_rendition() {
        let base = crate::test_tempdir().expect("base");
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        let first = VodServe::new(base.path().to_path_buf(), Arc::clone(&store));
        create(&first, &file, "sess-a", "play-a", &settings()).await;
        let len = plan_len(&first, "sess-a").await;
        for segment in 0..len {
            fetch(&first, "sess-a", &segment_name(segment as u64)).await;
        }
        let playlist_before = first
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("bytes")
            .0;
        // Wait for the generation to finish so nothing writes under the
        // successor.
        let rendition = rendition_of(&first, "sess-a").await;
        wait_until("the first producer ended", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;
        drop(first);

        let second = VodServe::new(base.path().to_path_buf(), store);
        create(&second, &file, "sess-b", "play-b", &settings()).await;
        let playlist_after = second
            .playlist("sess-b")
            .await
            .expect("ours")
            .expect("bytes")
            .0;
        assert_eq!(
            playlist_before, playlist_after,
            "a resurrected rendition serves the identical playlist"
        );
        // Every materialized segment is adopted and served with no producer.
        for segment in 0..len {
            match second
                .segment("sess-b", &segment_name(segment as u64))
                .await
                .expect("a vod session")
                .result
            {
                Ok(Some(ready)) => assert!(ready.len > 0),
                other => panic!("adopted segment {segment} must serve instantly: {other:?}"),
            }
        }
        let adopted = rendition_of(&second, "sess-b").await;
        assert!(
            matches!(adopted.slot.belief().await, Producer::Absent { .. }),
            "adoption serves without re-production"
        );
    }

    #[tokio::test]
    async fn varying_parameter_sets_are_refused_instead_of_changing_presentation() {
        testfixtures::require_ffmpeg();
        let file = fixture_file();
        let (store, mut index) = store_with_index(&file).await;
        index.parameter_sets_constant = false;
        store
            .put_fragment_index(file.id, &index)
            .await
            .expect("replace the index");
        let base = crate::test_tempdir().expect("base");
        let serve = VodServe::new(base.path().to_path_buf(), store);

        let error = serve
            .try_create(
                &request("play-a", 0.0),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-a".into(),
            )
            .await
            .expect_err("varying parameter sets cannot make one immutable init");
        assert!(
            error.contains("vod_source_unsupported"),
            "the refusal must be typed: {error}"
        );
        assert!(!serve.owns("sess-a").await);
    }

    #[tokio::test]
    async fn serving_loss_racing_final_cluster_attachment_leaves_no_session() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let base = crate::test_tempdir().expect("cluster serving fence base");
        let (serve, file) = serve_on(base.path()).await;
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let authority = fence.authority();
        let generation = authority.admit().expect("initial serving authority");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        let create = tokio::spawn({
            let serve = Arc::clone(&serve);
            let file = file.clone();
            let pause = Arc::clone(&pause);
            async move {
                serve
                    .try_create_cluster(
                        &request("cluster-fenced", 0.0),
                        &file,
                        &settings(),
                        VodAttribution {
                            user_name: "paul",
                            item_title: "Fixture",
                            supersession_user: "[\"user_id\",1]",
                        },
                        "cluster-fenced-session".to_owned(),
                        VodServingAdmission::new(
                            authority,
                            generation,
                            Instant::now() + Duration::from_secs(30),
                        )
                        .with_pause_before_commit(pause),
                    )
                    .await
            }
        });

        pause.wait().await;
        fence.validation_set_ready(false).await;
        pause.wait().await;
        let error = create
            .await
            .expect("cluster create task")
            .expect_err("stale serving authority must refuse final attachment");
        assert!(crate::transcode::is_serving_fence_error(&error), "{error}");
        assert!(!serve.owns("cluster-fenced-session").await);

        fence.validation_set_ready(true).await;
        let recovered_authority = fence.authority();
        let recovered_generation = recovered_authority
            .admit()
            .expect("recovered serving authority");
        serve
            .try_create_cluster(
                &request("cluster-recovered", 0.0),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "cluster-recovered-session".to_owned(),
                VodServingAdmission::new(
                    recovered_authority,
                    recovered_generation,
                    Instant::now() + Duration::from_secs(30),
                ),
            )
            .await
            .expect("recovered authority may attach a new VOD session");
        assert!(serve.owns("cluster-recovered-session").await);
    }

    #[tokio::test]
    async fn requests_the_vod_presentation_cannot_serve_fail_typed() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;

        // A caller cannot bypass the manager's frozen encoder resolution.
        let mut transcode = request("play-a", 0.0);
        transcode.kind = SessionKind::Transcode { height: 720 };
        let transcode_error = serve
            .try_create(
                &transcode,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-t".into(),
            )
            .await
            .expect_err("transcode VOD requires a resolved recipe");
        assert!(transcode_error.contains("vod_recipe_unresolved"));

        // A subtitle burn changes the video pipeline.
        let mut burn = request("play-a", 0.0);
        burn.subtitle_burn = Some(0);
        let burn_error = serve
            .try_create(
                &burn,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-s".into(),
            )
            .await
            .expect_err("burn VOD requires a resolved pixel recipe");
        assert!(burn_error.contains("vod_recipe_unresolved"));

        // No index stored for the current identity.
        let empty_store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let bare = VodServe::new(base.path().join("bare"), empty_store);
        let index_error = bare
            .try_create(
                &request("play-a", 0.0),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-n".into(),
            )
            .await
            .expect_err("an unindexed file must not change presentation");
        assert!(index_error.contains("vod_index_pending"));
    }

    #[tokio::test]
    async fn first_play_queues_missing_source_preparation_with_discovery_off() {
        use plurx_core::store::SettingsStore as _;
        testfixtures::require_ffmpeg();
        let base = crate::test_tempdir().expect("base");
        let source = fixture_file();
        let metadata = std::fs::metadata(&source.path).expect("source metadata");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let sqlite = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = sqlite
            .create_library(&plurx_core::domain::NewLibrary {
                name: "First play".to_owned(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: Vec::new(),
                anime: false,
            })
            .await
            .expect("library");
        let item = sqlite
            .insert_item(&plurx_core::domain::NewItem {
                library_id: library.id,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "Cold source".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = sqlite
            .upsert_file(
                item,
                &source.path.to_string_lossy(),
                metadata.len() as i64,
                mtime,
                &plurx_core::domain::ProbeResult {
                    duration_ms: source.duration_ms,
                    container: source.container,
                    video_codec: source.video_codec,
                    width: source.width,
                    height: source.height,
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let file = sqlite.get_file(file_id).await.expect("read").expect("file");
        sqlite
            .put_setting(plurx_core::store::keys::VOD_INDEX_MINS, "0")
            .await
            .expect("disable discovery");
        let store: Arc<dyn Store> = sqlite.clone();
        let serve = VodServe::new_cluster(
            base.path().join("vod"),
            store,
            "node-a".to_owned(),
            base.path().join("indexes"),
            None,
        );
        let mut req = request("first-play", 0.0);
        req.file_id = file.id;
        let disabled = serve
            .try_create(
                &req,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "viewer",
                    item_title: "Cold source",
                    supersession_user: "viewer",
                },
                "disabled".to_owned(),
            )
            .await
            .expect_err("missing index");
        assert!(
            disabled.contains("shared preparation is disabled"),
            "{disabled}"
        );
        assert!(sqlite
            .analysis_requests(10)
            .await
            .expect("requests")
            .is_empty());

        sqlite
            .put_setting(plurx_core::store::keys::VOD_INDEX_CLUSTER_CACHE, "1")
            .await
            .expect("enable shared preparation");
        for session in ["first", "retry"] {
            let refused = serve
                .try_create(
                    &req,
                    &file,
                    &settings(),
                    VodAttribution {
                        user_name: "viewer",
                        item_title: "Cold source",
                        supersession_user: "viewer",
                    },
                    session.to_owned(),
                )
                .await
                .expect_err("rolling fallback is still needed");
            assert!(refused.contains("vod_index_pending"), "{refused}");
            assert!(
                refused.contains("exact copy preparation is queued"),
                "{refused}"
            );
        }
        let queued = sqlite.analysis_requests(10).await.expect("requests");
        assert_eq!(
            queued.len(),
            1,
            "repeated first play joins the same preparation"
        );
        assert_eq!(queued[0].file_id, file.id);
        assert_eq!(queued[0].target_node_id, "node-a");
        assert_eq!(queued[0].state, "queued");
        assert!(!queued[0].video_identity.is_empty());
        assert!(
            serve.shared.sessions.lock().await.is_empty(),
            "no incomplete VOD session attaches"
        );
    }

    #[tokio::test]
    async fn unknown_and_unsafe_segment_names_answer_a_404_shape() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let touched = *serve.shared.sessions.lock().await["sess-a"]
            .last_touch
            .lock()
            .expect("touch lock");

        for name in [
            "seg99999.m4s",  // past the plan's end
            "notes.txt",     // not a segment name at all
            "../etc/passwd", // traversal
            "seg../x.m4s",   // traversal dressed as a segment
            "seg00000.ts",   // the transcode extension is not this path's
            "seg.m4s",       // no digits
        ] {
            let publication = serve.segment("sess-a", name).await.expect("a vod session");
            match publication.result {
                Ok(None) => assert!(
                    serve
                        .response_owner_is_live("sess-a", &publication.owner)
                        .await,
                    "{name} must retain the exact attachment that classified its 404"
                ),
                other => panic!("{name} must be the caller's 404: {:?}", other),
            }
        }
        // A session this runtime never made is nobody's business here.
        assert!(serve.segment("sess-x", "seg00000.m4s").await.is_none());
        assert!(serve.playlist("sess-x").await.is_none());
        assert!(!serve.owns("sess-x").await);
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "invalid names and indexes never renew the VOD attachment",
        );
    }

    #[tokio::test]
    async fn status_describes_vod_without_touching_its_idle_lease() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("sess-a", 0).await;
        let touched = Instant::now();
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(rendition.recipe.file.clone()),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(touched),
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

        let status = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(status.id, "sess-a");
        assert_eq!(status.file_id, 1);
        assert_eq!(status.encoder, "vod");
        assert_eq!(status.playlist_shape, "vod");
        assert_eq!(status.producer_state, "waiting");
        assert_eq!(status.fetched_end_ms, 0);
        assert_eq!(status.materialized_segments, 0);
        assert!(status.planned_segments > 1);
        assert_eq!(status.working_set_budget_bytes, 8 << 30);
        assert!(matches!(
            serve.segment("sess-a", "notes.txt").await,
            Some(VodPublication {
                result: Ok(None),
                ..
            })
        ));
        let (_, owner) = serve
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("playlist");
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "diagnostics, lookups, and invalid objects must not keep an abandoned session alive",
        );
        assert!(serve.commit_resolved_media("sess-a", &owner, None).await);
        assert!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock")
                > touched,
            "only the resolved response commit renews the VOD session",
        );

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(
            serve.status("sess-a").await.is_none(),
            "a tombstone is not live"
        );
        assert!(serve.status("unknown").await.is_none());
    }

    #[tokio::test]
    async fn a_far_seek_reports_the_frontier_the_client_can_actually_fetch() {
        // The two frontiers answer different questions and a far seek pulls
        // them apart. `published_end_ms` is how much of the title exists,
        // counted from the start, and the activity page asks that. A client
        // parked past a hole asks a different one: is there anything ahead of
        // *me*. Reporting the first as the second leaves a wedged player being
        // told, correctly and uselessly, that the film begins.
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition
            .dir
            .write_init(b"fixture-init")
            .await
            .expect("init");
        rendition.attach_reader("sess-a", 20).await;
        {
            let mut manifest = rendition.manifest.lock().await;
            assert!(manifest.len() > 23, "the fixture needs a hole to seek past");
            for index in [0, 1, 2, 3, 4, 20, 21, 22] {
                assert!(manifest.materialize(index, 1_024, 0));
            }
        }
        rendition
            .readers
            .lock()
            .await
            .get_mut("sess-a")
            .expect("reader")
            .last_served = Some(20);
        let anchor_ms = rendition
            .plan
            .entry(20)
            .map(|entry| ticks_to_ms(entry.start_ticks, rendition.timescale) + 1)
            .expect("seek entry");
        let mut control_snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        control_snapshot.position_ms = anchor_ms;
        control_snapshot.buffered_from_ms = Some(anchor_ms);
        control_snapshot.buffered_through_ms = anchor_ms;
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(rendition.recipe.file.clone()),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: Some(control_snapshot),
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
        let end_of = |index: u32| {
            rendition
                .plan
                .entry(index)
                .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
                .expect("planned segment")
        };

        let status = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(
            status.published_end_ms,
            Some(end_of(4)),
            "the run from the start stops at the hole",
        );
        assert_eq!(
            status.ready_ahead_end_ms,
            Some(end_of(22)),
            "the run from this client's own segment does not",
        );
        assert_eq!(status.fetched_end_ms, end_of(20));
        assert_eq!(status.server_ready_state, "ready");
        assert_eq!(status.server_ready_anchor_ms, Some(anchor_ms));
        assert_eq!(status.server_ready_end_ms, Some(end_of(22)));
        assert_eq!(status.server_next_ready_start_ms, None);
        assert_eq!(status.server_next_ready_end_ms, None);
        assert_eq!(
            status.server_ready_seconds,
            Some((end_of(22) - anchor_ms) as f64 / 1_000.0)
        );
        assert!(
            status.published_end_ms < Some(status.fetched_end_ms),
            "and the title's frontier is behind this client, which is the trap",
        );

        // Fill the hole: with nothing to seek past the two answers are the
        // same, which is why this is a correction and not a second policy.
        {
            let mut manifest = rendition.manifest.lock().await;
            for index in 5..20 {
                assert!(manifest.materialize(index, 1_024, 0));
            }
        }
        let filled = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(filled.published_end_ms, filled.ready_ahead_end_ms);

        let beyond_end = end_of((rendition.plan.entries.len() - 1) as u32) + 1_000;
        {
            let mut sessions = serve.shared.sessions.lock().await;
            let snapshot = sessions
                .get_mut("sess-a")
                .and_then(|session| session.last_control_snapshot.as_mut())
                .expect("control snapshot");
            snapshot.position_ms = beyond_end;
            snapshot.buffered_from_ms = Some(beyond_end);
            snapshot.buffered_through_ms = beyond_end;
        }
        let beyond = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(beyond.server_ready_state, "missing");
        assert_eq!(beyond.server_ready_seconds, Some(0.0));
        assert_eq!(beyond.server_next_ready_start_ms, None);
        assert_eq!(beyond.server_next_ready_end_ms, None);
    }
