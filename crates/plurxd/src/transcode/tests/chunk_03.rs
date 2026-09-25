
    /// Cumulative speed hides a slowdown behind a fast start; the recent rate
    /// is what predicts whether the viewer's reserve is about to drain. The
    /// smoothing is tested as arithmetic — a test that had to sleep to make a
    /// sample could not cover the rejection rules at all.
    #[test]
    fn recent_rate_smooths_and_rejects() {
        // First usable sample seeds the average: 2s of content in 1s = 2.00x.
        assert_eq!(recent_rate_step(-1, 1_000, 2_000), Some(2_000));
        // Subsequent samples bend it rather than replacing it: a crawl after a
        // fast start pulls down, but not all the way in one step.
        let bent = recent_rate_step(2_000, 1_000, 100).expect("a rate");
        assert!(
            bent < 2_000 && bent > 100,
            "smoothed toward the new rate: {bent}"
        );
        // Sustained crawling converges on the truth.
        let mut r = 2_000;
        for _ in 0..40 {
            r = recent_rate_step(r, 1_000, 100).expect("a rate");
        }
        assert!(r < 200, "converged on the real rate: {r}");

        // A gap longer than the cutoff spans a suspend — it must not report
        // the stopped time as slowness.
        assert_eq!(
            recent_rate_step(2_000, RECENT_SAMPLE_MAX_GAP_MS + 1, 500),
            None
        );
        // Two adjacent ffmpeg blocks are jitter, not signal.
        assert_eq!(recent_rate_step(2_000, RECENT_SAMPLE_MIN_MS - 1, 400), None);
        // Output going backwards is nonsense, not a negative rate.
        assert_eq!(recent_rate_step(2_000, 1_000, -50), None);
    }

    #[test]
    fn server_ready_uses_absolute_origin_and_does_not_cross_pruned_media() {
        let segment = |index, start_ms, end_ms, pruned| SegmentMeta {
            index,
            name: format!("seg{index:05}.ts"),
            start_ms,
            end_ms,
            bytes: if pruned { 0 } else { 1_024 },
            visibility: if pruned {
                SegmentVisibility::Deleted
            } else {
                SegmentVisibility::Advertised
            },
        };
        let index = SegmentIndex {
            segs: vec![
                segment(0, 0, 4_000, true),
                segment(1, 4_000, 8_000, false),
                segment(2, 8_000, 12_000, false),
            ],
            revision: 1,
        };

        let ready = index.server_ready(720_000, 725_500);
        assert_eq!(ready.state, "ready");
        assert_eq!(ready.anchor_ms, Some(725_500));
        assert_eq!(ready.end_ms, Some(732_000));
        assert_eq!(ready.seconds, Some(6.5));
        assert_eq!(
            ready.next_start_ms, None,
            "the anchored run is not a later island"
        );
        assert_eq!(ready.next_end_ms, None);

        let evicted = index.server_ready(720_000, 721_000);
        assert_eq!(evicted.state, "unavailable");
        assert_eq!(evicted.seconds, None, "evicted is not a measured zero");
        assert_eq!(evicted.next_start_ms, Some(724_000));
        assert_eq!(evicted.next_end_ms, Some(732_000));

        let beyond = index.server_ready(720_000, 733_000);
        assert_eq!(beyond.state, "missing");
        assert_eq!(beyond.seconds, Some(0.0));
        assert_eq!(
            beyond.next_start_ms, None,
            "media behind the anchor is not later"
        );
        assert_eq!(beyond.next_end_ms, None);
    }

    /// End to end through the atomics: the rate appears, and a gapped sample
    /// leaves it alone.
    #[test]
    fn recent_speed_survives_a_hold() {
        let p = Progress::new();
        let generation = p.begin_attempt();
        assert_eq!(p.recent_speed(), None, "nothing to average yet");
        // Baseline, then a sample far enough apart in wall clock to count.
        // (The test's own clock barely advances, so the baseline is backdated
        // rather than slept for.)
        apply_progress_line(&p, generation, "out_time_us=0");
        p.sample_wall_ms.store(-1_000, Relaxed);
        p.sample_out_ms.store(0, Relaxed);
        apply_progress_line(&p, generation, "out_time_us=4000000");
        let seen = p.recent_speed().expect("a rate once two samples exist");
        assert!(seen > 0.0);

        // Now a sample that looks like it spans a long hold: unchanged.
        p.sample_wall_ms
            .store(-RECENT_SAMPLE_MAX_GAP_MS * 2, Relaxed);
        apply_progress_line(&p, generation, "out_time_us=5000000");
        assert_eq!(
            p.recent_speed(),
            Some(seen),
            "the hold did not count as slow"
        );
    }

    /// Watching a stream is not fetching from it.
    ///
    /// The stats overlay polls session status every couple of seconds while it
    /// is open. If that poll touched the idle clock, leaving the overlay up
    /// would keep an abandoned encoder alive forever — the exact leak the idle
    /// reaper exists to prevent. This is already true; the test is here so it
    /// stays true, because the natural way to write `session_status` is to
    /// reuse `touch` and nothing would visibly break.
    #[tokio::test]
    async fn polling_status_does_not_keep_a_session_alive() {
        let root = crate::test_tempdir().expect("status fixture");
        let session_id = "status-does-not-renew";
        let fixture = HlsDeliveryFixture::publish(root.path(), session_id).await;
        let mgr = Arc::clone(&fixture.state.transcode);

        // Let the idle clock advance, then poll status several times.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let before = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("rolling actor");
        assert!(
            before.idle_for >= Duration::from_millis(300),
            "the fixture establishes a measurable pre-poll idle interval"
        );
        let before_status = mgr.session_status(session_id).await.expect("status");
        assert_eq!(
            before_status.hold_reason, None,
            "a running session is not held"
        );
        assert_eq!(before_status.resume_below_seconds, None);
        assert_eq!(before_status.resume_below_bytes, None);
        for _ in 0..5 {
            assert!(mgr.session_status(session_id).await.is_some());
        }
        let after = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("rolling actor");
        assert_eq!(
            after.last_renewal_kind, "test-start",
            "status polling must not renew the playback lease"
        );
        assert!(
            after.idle_for >= before.idle_for,
            "idle time must keep running while status is polled"
        );

        // A successfully resolved fetch DOES reset it — the contrast that
        // keeps the assertion above from passing vacuously on a session whose
        // clock never moved. This is the shared response commit point; the
        // actual readers would long-poll for output this fixture cannot make.
        let owner = MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            producer_attempt: fixture.session.control.current_producer_attempt(),
            session: Arc::clone(&fixture.session),
        });
        assert!(
            mgr.commit_resolved_media(session_id, &owner, "test-fetch", None, true)
                .await
        );
        let renewed = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("rolling actor");
        assert_eq!(
            renewed.last_renewal_kind, "test-fetch",
            "the accepted response records its exact renewal source"
        );
        assert!(
            renewed.idle_for < after.idle_for,
            "fetching from a session is what keeps it alive"
        );
        assert!(mgr.stop_session(session_id, "test").await);
    }

    #[tokio::test]
    async fn rolling_status_authz_maps_to_attempt_status() {
        let root = crate::test_tempdir().expect("status fixture");
        let session_id = "status-publication";
        let fixture = HlsDeliveryFixture::publish_actor_managed(root.path(), session_id).await;
        let publication = fixture
            .state
            .transcode
            .hls_session_status_publication(session_id)
            .await
            .expect("rolling status publication");

        fixture
            .state
            .transcode
            .authorize_response_publication(
                session_id,
                &publication.owner,
                MediaResponsePublication::attempt_status("status", None),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("session status must use a typed attempt-status object");
    }

    /// The playlist is the only place a copied segment's true duration is
    /// written down, which is the whole reason this parser exists.
    #[test]
    fn playlist_parsing_believes_extinf_not_the_index() {
        // A copy session: `hls_time` is only a floor there, and real segments
        // run to the source's GOP. Index arithmetic against a nominal 4 s
        // would call the third segment 8-12s; it is actually 14.5-24.5s.
        let copy = "#EXTM3U\n\
                    #EXT-X-VERSION:7\n\
                    #EXT-X-TARGETDURATION:11\n\
                    #EXT-X-PLAYLIST-TYPE:EVENT\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:4.000,\n\
                    seg00000.m4s\n\
                    #EXTINF:10.500,\n\
                    seg00001.m4s\n\
                    #EXTINF:10.000,\n\
                    seg00002.m4s\n";
        let segs = parse_playlist(copy);
        assert_eq!(segs.len(), 3, "the EXT-X-MAP init file is not a segment");
        assert_eq!(segs[0].name, "seg00000.m4s");
        assert_eq!((segs[0].start_ms, segs[0].end_ms), (0, 4_000));
        assert_eq!((segs[1].start_ms, segs[1].end_ms), (4_000, 14_500));
        assert_eq!((segs[2].start_ms, segs[2].end_ms), (14_500, 24_500));

        let index = SegmentIndex { segs, revision: 0 };
        assert_eq!(index.produced_playable_end_ms(), Some(24_500));
        assert_eq!(index.end_ms_of(1), Some(14_500));
        assert_eq!(index.window_ms_of(1), Some((4_000, 14_500)));
        assert_eq!(index.end_ms_of(9), None);

        // A transcode playlist, where the grid is forced and even.
        let transcode = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n\
                         #EXTINF:4.000000,\n seg00000.ts\n\
                         #EXTINF:4.000000,\nseg00001.ts\n";
        let segs = parse_playlist(transcode);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].end_ms, 8_000);

        // Junk: a URI with no EXTINF, an EXTINF with no URI, an unparseable
        // duration. None of them may invent a segment or shift the timeline.
        let junk = "#EXTM3U\nseg00007.ts\n#EXTINF:abc,\nseg00008.ts\n#EXTINF:2.0,\n";
        assert!(parse_playlist(junk).is_empty());
        assert!(parse_playlist("").is_empty());
    }

    #[test]
    fn retained_vod_validation_rejects_every_unpaired_playlist_uri() {
        let valid = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n";
        assert!(validated_vod_part(valid).is_some());
        assert!(validated_vod_part(
            "#EXTM3U\nstray.ts\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n"
        )
        .is_none());
        assert!(validated_vod_part(
            "#EXTM3U\n#EXTINF:1.0,\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n"
        )
        .is_none());
        assert!(validated_vod_part(
            "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:1.0,\n#EXT-X-ENDLIST\n"
        )
        .is_none());
        for injected in [
            "#EXT-X-KEY:METHOD=AES-128,URI=\"https://attacker.invalid/key\"",
            "#EXT-X-MAP:URI=\"init.mp4\"",
            "#EXT-X-BYTERANGE:1024@0",
        ] {
            let playlist =
                format!("#EXTM3U\n{injected}\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n");
            assert!(
                validated_vod_part(&playlist).is_none(),
                "legacy adoption must reject {injected}"
            );
        }
        assert!(validated_vod_part(
            "#EXTM3U\n#EXTINF:2.0,\n#EXT-X-VERSION:3\nseg00000.ts\n#EXT-X-ENDLIST\n"
        )
        .is_none());
    }

    #[tokio::test]
    async fn resumable_and_assembled_publications_reject_empty_segments() {
        let directory = crate::test_tempdir().expect("generation");
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n",
        )
        .await
        .expect("playlist");
        tokio::fs::write(directory.path().join("seg00000.ts"), b"")
            .await
            .expect("empty segment");
        let capability = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("generation capability");

        assert!(
            read_validated_part(&capability, MAX_PRETRANSCODE_PART_PLAYLIST_BYTES)
                .await
                .is_none(),
            "an empty resumable segment must not become a checkpoint"
        );
        assert!(
            assembled_publication(
                &capability,
                &[crate::produce::Part {
                    segments: vec!["seg00000.ts".to_owned()],
                    durations_ms: vec![2_000],
                }],
                None
            )
            .await
            .is_none(),
            "an empty assembled segment must not become a published generation"
        );
    }

    #[tokio::test]
    async fn retained_part_validation_rejects_overwritten_extinf_but_allows_killed_tail() {
        let directory = crate::test_tempdir().expect("retained part");
        tokio::fs::write(directory.path().join("seg00000.ts"), b"segment")
            .await
            .expect("segment");
        let capability = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("part capability");

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            "#EXTM3U\n#EXTINF:1.0,\n#EXTINF:2.0,\nseg00000.ts\n",
        )
        .await
        .expect("ambiguous playlist");
        assert!(
            read_validated_part(&capability, MAX_PRETRANSCODE_PART_PLAYLIST_BYTES)
                .await
                .is_none(),
            "a second EXTINF must not overwrite persisted resume authority"
        );

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:3.0,\n",
        )
        .await
        .expect("killed tail playlist");
        let validated = read_validated_part(&capability, MAX_PRETRANSCODE_PART_PLAYLIST_BYTES)
            .await
            .expect("one unmatched killed tail is droppable");
        assert_eq!(validated.part.segments, ["seg00000.ts"]);
        assert_eq!(validated.part.durations_ms, [2_000]);
        // The shape measures only what the playlist lists: the truncated tail
        // the kill left behind is not part of what a receipt would be bound to.
        assert_eq!(validated.shape.len(), 1);
        assert_eq!(validated.shape[0].0, "seg00000.ts");
        assert_eq!(validated.shape[0].2, 2_000);
    }

    /// A live EVENT playlist needs both more than one segment and enough media
    /// runway before its first response. A long first segment alone still
    /// leaves hls.js at the writer edge, while a completed short title must not
    /// be held until the request timeout.
    #[test]
    fn live_transcode_publication_requires_real_cushion_or_endlist() {
        let one_short = b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                          #EXTINF:2.000,\nseg00000.ts\n";
        let one_long = b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                         #EXTINF:8.000,\nseg00000.ts\n";
        let two_short = b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                          #EXTINF:1.000,\nseg00000.ts\n\
                          #EXTINF:1.000,\nseg00001.ts\n";
        let two_ready = b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                          #EXTINF:2.000,\nseg00000.ts\n\
                          #EXTINF:2.000,\nseg00001.ts\n";
        let completed_short = b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                                #EXTINF:1.000,\nseg00000.ts\n#EXT-X-ENDLIST\n";

        assert!(!transcode_first_playlist_ready(one_short));
        assert!(!transcode_first_playlist_ready(one_long));
        assert!(!transcode_first_playlist_ready(two_short));
        assert!(transcode_first_playlist_ready(two_ready));
        assert!(transcode_first_playlist_ready(completed_short));
    }

    fn rolling_playlist(durations: &[f64], end: bool) -> String {
        let mut playlist =
            String::from("#EXTM3U\n#EXT-X-TARGETDURATION:16\n#EXT-X-PLAYLIST-TYPE:EVENT\n");
        for (index, duration) in durations.iter().enumerate() {
            playlist.push_str(&format!("#EXTINF:{duration:.6},\nseg{index:05}.ts\n"));
        }
        if end {
            playlist.push_str("#EXT-X-ENDLIST\n");
        }
        playlist
    }

    async fn accept_rolling_publication_demand(
        session: &Session,
        sequence: u64,
        position_ms: i64,
        playback_rate: f64,
        demand: crate::playback_control::PlaybackDemand,
        render_state: crate::playback_control::RenderState,
    ) {
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        snapshot.demand = demand;
        snapshot.position_ms = position_ms;
        snapshot.buffered_from_ms = Some(position_ms);
        snapshot.buffered_through_ms = position_ms.saturating_add(5_000);
        snapshot.playback_rate = playback_rate;
        snapshot.render_state = render_state;
        let outcome = session
            .control
            .control(crate::playback_control::LocalControlRequest {
                session_id: "rolling-budget-session",
                generation: "rolling-budget-generation",
                owner_node_id: "test-node",
                owner_epoch: 1,
                client_instance_id: "00000000-0000-4000-8000-000000000001",
                sequence,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            })
            .await
            .expect("publication demand accepted");
        assert_eq!(
            outcome.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
    }

    async fn commit_rolling_publication_media(
        session: &Session,
        segment_index: i64,
        segment_end_ms: i64,
    ) -> bool {
        let producer_attempt = session.control.current_producer_attempt();
        let handoff = session.control.media_commit_handoff(
            producer_attempt,
            segment_index,
            Some(segment_end_ms),
            Arc::clone(&session.compatibility_attempt),
            Arc::clone(&session.high_segment),
            Arc::clone(&session.fetched_end_ms),
        );
        session
            .control
            .commit_media(
                "segment",
                producer_attempt,
                Some(segment_index),
                Some(segment_end_ms),
                Some(handoff),
                Instant::now() + Duration::from_secs(5),
            )
            .await
    }

    #[tokio::test(start_paused = true)]
    async fn rolling_publication_budget_two_x_writer_tracks_one_x_for_one_simulated_hour() {
        let directory = crate::test_tempdir().expect("publication budget");
        let session = test_session(directory.path().to_path_buf());
        let started = Instant::now();
        let mut previous_frontier = 0;

        for step in 0..=225_i64 {
            let elapsed_ms = step.saturating_mul(16_000);
            accept_rolling_publication_demand(
                &session,
                u64::try_from(step + 1).expect("sequence"),
                elapsed_ms,
                1.0,
                crate::playback_control::PlaybackDemand::Active,
                crate::playback_control::RenderState::Rendering,
            )
            .await;
            let produced_end_ms = 48_000_i64.saturating_add(elapsed_ms.saturating_mul(2));
            let segment_count =
                usize::try_from(produced_end_ms / 6_000 + 1).expect("segment count");
            tokio::fs::write(
                directory.path().join("index.m3u8"),
                rolling_playlist(&vec![6.0; segment_count], false),
            )
            .await
            .expect("writer playlist");
            session
                .publication_cycle_at(
                    "rolling_publication_budget_hour",
                    started + Duration::from_millis(u64::try_from(elapsed_ms).expect("elapsed")),
                )
                .await
                .expect("bounded publication");

            let clock = session.publication.lock().await;
            let served = clock.served.as_ref().expect("served snapshot");
            assert!(
                served.end_ms >= previous_frontier,
                "frontier regressed at step {step}"
            );
            assert!(
                served.end_ms.saturating_sub(elapsed_ms)
                    <= rolling_initial_runway_ms(1.0) + ROLLING_SEGMENT_MAX_MS,
                "writer-speed lead leaked at step {step}: F={} C={elapsed_ms}",
                served.end_ms,
            );
            let protected_ms = elapsed_ms
                .saturating_sub(ROLLING_PUBLICATION_GUARD_MS)
                .saturating_sub(ROLLING_BACK_BUFFER_MS)
                .max(0);
            let first_start_ms = served.first_segment.saturating_mul(6_000);
            assert!(
                first_start_ms <= protected_ms,
                "protected segment was pruned at step {step}: start={first_start_ms} protected={protected_ms}",
            );
            assert_eq!(clock.budget_anchor_sequence, Some((step + 1) as u64));
            previous_frontier = served.end_ms;
            drop(clock);
            tokio::time::advance(Duration::from_millis(251)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rolling_publication_budget_one_point_two_x_writer_sustains_one_x_for_thirty_minutes() {
        let directory = crate::test_tempdir().expect("publication capacity");
        let session = test_session(directory.path().to_path_buf());
        let started = Instant::now();
        let generation = session.progress.begin_attempt();
        apply_progress_line(&session.progress, generation, "out_time_us=0");
        session.progress.sample_wall_ms.store(-1_000, Relaxed);
        session.progress.sample_out_ms.store(0, Relaxed);
        apply_progress_line(&session.progress, generation, "out_time_us=1200000");
        assert_eq!(session.progress.recent_speed(), Some(1.2));
        session
            .progress
            .sample_wall_ms
            .store(-RECENT_SAMPLE_MAX_GAP_MS * 2, Relaxed);
        apply_progress_line(&session.progress, generation, "out_time_us=2400000");
        assert_eq!(
            session.progress.recent_speed(),
            Some(1.2),
            "an intentional producer hold must not invent insufficient capacity"
        );

        for step in 0..=112_i64 {
            let elapsed_ms = step.saturating_mul(16_000);
            accept_rolling_publication_demand(
                &session,
                u64::try_from(step + 1).expect("sequence"),
                elapsed_ms,
                1.0,
                crate::playback_control::PlaybackDemand::Active,
                crate::playback_control::RenderState::Rendering,
            )
            .await;
            let produced_end_ms = 48_000_i64
                .saturating_add(((elapsed_ms as f64) * 1.2).round().min(i64::MAX as f64) as i64);
            let segment_count =
                usize::try_from(produced_end_ms / 6_000 + 1).expect("segment count");
            tokio::fs::write(
                directory.path().join("index.m3u8"),
                rolling_playlist(&vec![6.0; segment_count], false),
            )
            .await
            .expect("writer playlist");
            session
                .publication_cycle_at(
                    "rolling_publication_budget_capacity",
                    started + Duration::from_millis(u64::try_from(elapsed_ms).expect("elapsed")),
                )
                .await
                .expect("sustainable producer remains live");
            let clock = session.publication.lock().await;
            let served = clock.served.as_ref().expect("served snapshot");
            assert!(
                served.end_ms.saturating_sub(elapsed_ms)
                    <= rolling_initial_runway_ms(1.0) + ROLLING_SEGMENT_MAX_MS,
                "bounded lead at step {step}"
            );
            drop(clock);
            tokio::time::advance(Duration::from_millis(251)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rolling_publication_budget_low_rate_retires_before_the_window_can_skip() {
        let directory = crate::test_tempdir().expect("low-rate budget");
        let session = Arc::new(test_session(directory.path().to_path_buf()));
        let started = Instant::now();
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0; 32], false),
        )
        .await
        .expect("writer playlist");

        for step in 0..=6_i64 {
            let elapsed_ms = step.saturating_mul(16_000);
            let position_ms = step.saturating_mul(4_000);
            accept_rolling_publication_demand(
                &session,
                u64::try_from(step + 1).expect("sequence"),
                position_ms,
                0.25,
                crate::playback_control::PlaybackDemand::Active,
                crate::playback_control::RenderState::Rendering,
            )
            .await;
            session
                .publication_cycle_at(
                    "rolling_publication_budget_low_rate",
                    started + Duration::from_millis(u64::try_from(elapsed_ms).expect("elapsed")),
                )
                .await
                .expect("low-rate floor remains safe before exhaustion");
            tokio::time::advance(Duration::from_millis(251)).await;
        }
        accept_rolling_publication_demand(
            &session,
            8,
            28_000,
            0.25,
            crate::playback_control::PlaybackDemand::Active,
            crate::playback_control::RenderState::Rendering,
        )
        .await;
        session.publication.lock().await.next_publish_at =
            Some(Instant::now() - Duration::from_millis(1));

        session.ensure_publication_worker("rolling_publication_budget_low_rate");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !session.failed.load(Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("publication worker must retire the exhausted presentation");
        let failure = session.failure_reason();
        assert!(
            matches!(&failure, PlaylistError::SessionFailed(reason) if reason.starts_with("rolling_window_budget_exhausted:")),
            "unexpected worker verdict: {failure:?}"
        );
        assert_eq!(failure.code(), "session_failed");
        let lease = session
            .control
            .snapshot()
            .await
            .expect("active lease retained");
        assert_eq!(
            lease.demand.as_ref().map(|demand| demand.position_ms),
            Some(28_000),
            "bounded recovery must inherit the accepted film position"
        );
        assert_ne!(
            lease.terminal,
            Some(crate::playback_control::RollingTerminalCause::PauseExpired),
            "active window exhaustion is not pause expiry"
        );
    }

    #[tokio::test]
    async fn rolling_publication_budget_legacy_rapid_fetch_cannot_release_the_writer_tail() {
        let directory = crate::test_tempdir().expect("legacy publication budget");
        let session = test_session(directory.path().to_path_buf());
        let started = Instant::now();
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0; 3], false),
        )
        .await
        .expect("bootstrap playlist");
        session
            .publication_cycle_at("rolling_publication_budget_legacy", started)
            .await
            .expect("legacy bootstrap");
        assert!(
            commit_rolling_publication_media(&session, 2, 48_000).await,
            "bootstrap fetch"
        );

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0; 12], false),
        )
        .await
        .expect("burst writer playlist");
        session.publication.lock().await.next_publish_at =
            Some(Instant::now() - Duration::from_millis(1));
        session
            .publication_cycle_at(
                "rolling_publication_budget_legacy",
                started + ROLLING_PUBLICATION_TARGET,
            )
            .await
            .expect("bounded legacy publication");
        let clock = session.publication.lock().await;
        let served = clock.served.as_ref().expect("legacy snapshot");
        assert_eq!(served.end_ms, 80_000);
        assert_eq!(served.last_segment, 4);
        assert!(
            served.end_ms <= 16_000 + ROLLING_INITIAL_RUNWAY_MS + ROLLING_SEGMENT_MAX_MS,
            "rapid writer inventory escaped the fixed legacy clock plus one segment"
        );
        assert!(
            served.end_ms <= 48_000 + ROLLING_INITIAL_RUNWAY_MS,
            "rapid fetching released media beyond the fixed legacy fetch allowance"
        );
    }

    #[tokio::test]
    async fn rolling_publication_budget_legacy_variable_bootstrap_and_explicit_cutover() {
        let directory = crate::test_tempdir().expect("legacy variable publication budget");
        let session = test_session(directory.path().to_path_buf());
        let started = Instant::now();
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 15.0, 5.0], false),
        )
        .await
        .expect("variable bootstrap playlist");
        session
            .publication_cycle_at("rolling_publication_budget_legacy_variable", started)
            .await
            .expect("legacy variable bootstrap");
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|served| (served.last_segment, served.end_ms)),
            Some((2, 47_000)),
            "the safe prefix below the 48 second boundary must bootstrap"
        );
        assert!(
            commit_rolling_publication_media(&session, 2, 47_000).await,
            "legacy bootstrap fetch"
        );

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 15.0, 5.0, 8.0, 8.0, 8.0, 8.0], false),
        )
        .await
        .expect("paced variable playlist");
        session.publication.lock().await.next_publish_at =
            Some(Instant::now() - Duration::from_millis(1));
        session
            .publication_cycle_at(
                "rolling_publication_budget_legacy_variable",
                started + ROLLING_PUBLICATION_TARGET,
            )
            .await
            .expect("paced legacy publication");
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|served| served.end_ms),
            Some(76_000),
            "legacy publication exposes the last complete endpoint inside its clock"
        );

        accept_rolling_publication_demand(
            &session,
            1,
            20_000,
            1.0,
            crate::playback_control::PlaybackDemand::Active,
            crate::playback_control::RenderState::Rendering,
        )
        .await;
        session.publication.lock().await.next_publish_at =
            Some(Instant::now() - Duration::from_millis(1));
        session
            .publication_cycle_at(
                "rolling_publication_budget_legacy_variable",
                started + ROLLING_PUBLICATION_TARGET * 2,
            )
            .await
            .expect("explicit cutover publication");
        let clock = session.publication.lock().await;
        assert_eq!(clock.budget_anchor_sequence, Some(1));
        assert_eq!(
            clock.served.as_ref().map(|served| served.end_ms),
            Some(84_000),
            "accepted explicit demand replaces the legacy wall-clock budget"
        );
    }

    #[tokio::test]
    async fn rolling_publication_budget_commit_deadlines_begin_when_snapshot_is_available() {
        let directory = crate::test_tempdir().expect("publication commit clock");
        let session = test_session(directory.path().to_path_buf());
        accept_rolling_publication_demand(
            &session,
            1,
            0,
            1.0,
            crate::playback_control::PlaybackDemand::Active,
            crate::playback_control::RenderState::Rendering,
        )
        .await;
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0; 4], false),
        )
        .await
        .expect("writer playlist");
        let before_commit = Instant::now();
        session
            .publication_cycle_at(
                "rolling_publication_budget_commit_clock",
                before_commit - Duration::from_secs(60),
            )
            .await
            .expect("publication with an old policy observation");
        let clock = session.publication.lock().await;
        let served = clock.served.as_ref().expect("served snapshot");
        assert!(served.available_at >= before_commit);
        assert_eq!(
            clock.next_publish_at,
            Some(served.available_at + ROLLING_PUBLICATION_TARGET)
        );
        assert_eq!(
            clock.hard_deadline,
            Some(served.available_at + ROLLING_PUBLICATION_HARD)
        );
    }

    #[tokio::test]
    async fn rolling_publication_budget_eof_keeps_the_slow_viewers_protected_tail() {
        let directory = crate::test_tempdir().expect("EOF publication budget");
        let session = test_session(directory.path().to_path_buf());
        accept_rolling_publication_demand(
            &session,
            1,
            360_000,
            1.0,
            crate::playback_control::PlaybackDemand::Active,
            crate::playback_control::RenderState::Rendering,
        )
        .await;
        session.publication.lock().await.retention_first_segment = Some(70);
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[6.0; 80], true),
        )
        .await
        .expect("completed writer playlist");
        session
            .publication_cycle("rolling_publication_budget_eof")
            .await
            .expect("EOF publication");
        let clock = session.publication.lock().await;
        let served = clock.served.as_ref().expect("EOF snapshot");
        assert_eq!(
            served.first_segment, 53,
            "EOF must clamp an ahead-of-playback retention request to the protected segment"
        );
        assert_eq!(served.last_segment, 79);
        assert!(served.end_list);
        assert!(String::from_utf8_lossy(&served.raw).contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn rolling_publication_budget_target_only_rounding_is_a_negative_control() {
        let mut frontier_ms = ROLLING_INITIAL_RUNWAY_MS;
        let mut violated = false;
        for step in 1..=60_i64 {
            frontier_ms = frontier_ms.saturating_add(18_000);
            let consumed_ms = step.saturating_mul(16_000);
            let desired_ms = consumed_ms.saturating_add(ROLLING_INITIAL_RUNWAY_MS);
            violated |= frontier_ms.saturating_sub(desired_ms) > ROLLING_SEGMENT_MAX_MS;
        }
        assert!(
            violated,
            "granting a fresh rounded batch per target must not satisfy cumulative accounting"
        );
    }

    #[test]
    fn rolling_publication_budget_renderer_keeps_only_the_selected_variable_duration_prefix() {
        let raw = rolling_playlist(&[16.0, 5.0, 11.0, 6.0, 9.0], false);
        let served = served_live_playlist(raw.into_bytes(), Some(1), Some(3), None)
            .expect("bounded variable-duration prefix");
        let served = String::from_utf8(served).expect("UTF-8 playlist");
        assert!(!served.contains("seg00000.ts"), "{served}");
        for retained in ["seg00001.ts", "seg00002.ts", "seg00003.ts"] {
            assert!(served.contains(retained), "missing {retained}: {served}");
        }
        assert!(!served.contains("seg00004.ts"), "{served}");
        let index = SegmentIndex {
            segs: parse_playlist(&served),
            revision: 0,
        };
        assert_eq!(index.segs.len(), 3);
        assert_eq!(index.produced_playable_end_ms(), Some(22_000));
    }

    #[test]
    fn mkv_hls_schedule_scales_runway_and_batches_with_playback_rate() {
        let cases = [
            (0.25, 4_000, 48_000),
            (0.5, 8_000, 48_000),
            (1.0, 16_000, 48_000),
            (2.0, 32_000, 96_000),
            (4.0, 64_000, 124_000),
        ];
        for (rate, batch_ms, initial_ms) in cases {
            assert_eq!(
                rolling_publication_batch_ms(rate),
                batch_ms,
                "{rate}x batch"
            );
            assert_eq!(
                rolling_initial_runway_ms(rate),
                initial_ms,
                "{rate}x runway"
            );
            assert!(
                !rolling_insufficient_capacity(rate, Some(rate)),
                "{rate}x at capacity"
            );
        }
        assert!(rolling_insufficient_capacity(2.0, Some(1.0)));
        assert!(rolling_insufficient_capacity(1.0, Some(0.8)));
        assert!(!rolling_insufficient_capacity(0.5, Some(0.8)));
    }

    #[tokio::test]
    async fn mkv_hls_schedule_publishes_only_due_cumulative_media() {
        let directory = crate::test_tempdir().expect("publication clock");
        let session = test_session(directory.path().to_path_buf());
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 16.0], false),
        )
        .await
        .expect("initial writer playlist");
        session
            .publication_cycle("schedule-test")
            .await
            .expect("initial publication");
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|snapshot| (snapshot.revision, snapshot.last_segment)),
            Some((1, 2))
        );

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 16.0, 6.0], false),
        )
        .await
        .expect("staged writer playlist");
        session
            .publication_cycle("schedule-test")
            .await
            .expect("staging cycle");
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|snapshot| snapshot.last_segment),
            Some(2),
            "a short raw writer revision leaked before its scheduled batch"
        );

        assert!(
            commit_rolling_publication_media(&session, 2, 48_000).await,
            "legacy fetch frontier"
        );

        {
            let mut clock = session.publication.lock().await;
            let due = Instant::now() - Duration::from_millis(1);
            clock.next_publish_at = Some(due);
            clock.served.as_mut().expect("served snapshot").available_at =
                due - ROLLING_PUBLICATION_TARGET;
        }
        session
            .publication_cycle("schedule-test")
            .await
            .expect("scheduled publication");
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|snapshot| (snapshot.revision, snapshot.last_segment)),
            Some((2, 3)),
            "a due target publishes only the earned completed prefix"
        );

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 16.0, 6.0, 10.0], false),
        )
        .await
        .expect("complete staged batch");
        session
            .publication_cycle("schedule-test")
            .await
            .expect("rate-sized publication");
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|snapshot| (snapshot.revision, snapshot.last_segment)),
            Some((2, 3)),
            "new writer inventory stays private until the next target"
        );
    }

    #[tokio::test]
    async fn mkv_hls_schedule_fetch_stopped_session_has_a_finite_hard_deadline() {
        let directory = crate::test_tempdir().expect("publication deadline");
        let session = test_session(directory.path().to_path_buf());
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 16.0], false),
        )
        .await
        .expect("writer playlist");
        session
            .publication_cycle("deadline-test")
            .await
            .expect("initial publication");
        session.publication.lock().await.hard_deadline =
            Some(Instant::now() - Duration::from_millis(1));
        let reason = session
            .publication_cycle("deadline-test")
            .await
            .expect_err("a stopped writer cannot keep the presentation forever");
        assert!(reason.contains("hard deadline"), "{reason}");
    }

    async fn scratch_hold_write_playlist(session: &Session, durations: &[f64]) {
        tokio::fs::write(
            session.dir.join("index.m3u8"),
            rolling_playlist(durations, false),
        )
        .await
        .expect("writer playlist");
    }

    async fn scratch_hold_set(session: &Session, reason: Option<AheadHoldReason>, since: Instant) {
        *session.suspended_at.lock().await = reason.map(|reason| SuspendedAt {
            since,
            hold: AheadHold {
                reason,
                release_value: 1,
            },
        });
        session.suspended.store(reason.is_some(), Relaxed);
    }

    /// Publish once, then hold the producer for `reason` starting a second
    /// later. Returns the served snapshot's publication instant.
    async fn scratch_hold_published_and_held(session: &Session, reason: AheadHoldReason) -> Instant {
        scratch_hold_write_playlist(session, &[16.0, 16.0, 16.0]).await;
        session
            .publication_cycle("scratch-hold-deadline")
            .await
            .expect("initial publication");
        let published = session
            .publication
            .lock()
            .await
            .served
            .as_ref()
            .expect("served snapshot")
            .available_at;
        scratch_hold_set(session, Some(reason), published + Duration::from_secs(1)).await;
        published
    }

    /// A producer held because global scratch is exhausted is waiting on
    /// capacity, and scratch_put lets its starved writer wait as long as the
    /// session lives. The 24 s publication deadline used to retire it anyway,
    /// with the client still rendering from its buffer. Once the hold clears
    /// the deadline runs its full length again, so a producer that stays
    /// stuck is still retired.
    ///
    /// Run twice: with a slow speed frozen by the stop the capacity arm fires
    /// first on the deadline; with no measured speed only the hard-deadline
    /// arm can retire the session. Both must wait out the hold.
    #[tokio::test]
    async fn scratch_hold_publication_deadline_waits_out_a_global_scratch_hold() {
        for (recent_milli, retired_by) in [
            (Some(500), "rolling_insufficient_capacity"),
            (None, "hard deadline"),
        ] {
            let directory = crate::test_tempdir().expect("publication deadline");
            let session = test_session(directory.path().to_path_buf());
            let published =
                scratch_hold_published_and_held(&session, AheadHoldReason::Global).await;
            if let Some(recent_milli) = recent_milli {
                session.progress.recent_milli.store(recent_milli, Relaxed);
            }
            let held_until = published + ROLLING_PUBLICATION_HARD * 3;
            let mut now = published;
            while now < held_until {
                now += Duration::from_secs(1);
                session
                    .publication_cycle_at("scratch-hold-deadline", now)
                    .await
                    .unwrap_or_else(|reason| {
                        panic!("a producer held for scratch is waiting, not stuck: {reason}")
                    });
            }

            scratch_hold_set(&session, None, held_until).await;
            session
                .publication_cycle_at(
                    "scratch-hold-deadline",
                    held_until + ROLLING_PUBLICATION_HARD - Duration::from_secs(1),
                )
                .await
                .expect("the deadline restarts from the end of the hold");
            let reason = session
                .publication_cycle_at(
                    "scratch-hold-deadline",
                    held_until + ROLLING_PUBLICATION_HARD + Duration::from_secs(1),
                )
                .await
                .expect_err("a producer still stuck after the hold clears is retired");
            assert!(reason.contains(retired_by), "{retired_by}: {reason}");
        }
    }

    /// Before the first snapshot the capacity check has no deadline: a
    /// session that has staged its initial runway at a measured speed below
    /// the playback rate is retired at once. A writer starved for global
    /// scratch stops FFmpeg with its last measured speed frozen, so that
    /// check must wait out the hold too, and apply as before once it clears.
    #[tokio::test]
    async fn scratch_hold_publication_deadline_prepublication_capacity_waits_out_a_global_scratch_hold(
    ) {
        let directory = crate::test_tempdir().expect("publication deadline");
        let session = test_session(directory.path().to_path_buf());
        let started = Instant::now();
        // Anchor the legacy publication budget below the initial runway.
        scratch_hold_write_playlist(&session, &[16.0, 16.0]).await;
        session
            .publication_cycle_at("scratch-hold-prepublication", started)
            .await
            .expect("below the initial runway nothing is judged");
        // Exactly the initial runway, measured at half speed, staged ten
        // seconds later: the legacy budget now wants 58 s, so nothing
        // publishes and the session stays in its pre-publication state.
        session.progress.recent_milli.store(500, Relaxed);
        scratch_hold_write_playlist(&session, &[16.0, 16.0, 16.0]).await;
        scratch_hold_set(&session, Some(AheadHoldReason::Global), started).await;
        let held_until = started + ROLLING_PUBLICATION_HARD * 3;
        let mut now = started + Duration::from_secs(10);
        while now < held_until {
            session
                .publication_cycle_at("scratch-hold-prepublication", now)
                .await
                .unwrap_or_else(|reason| {
                    panic!("a producer held for scratch is waiting, not slow: {reason}")
                });
            now += Duration::from_secs(1);
        }
        assert!(
            session.publication.lock().await.served.is_none(),
            "the hold is observed before the first snapshot"
        );

        scratch_hold_set(&session, None, held_until).await;
        let reason = session
            .publication_cycle_at("scratch-hold-prepublication", held_until)
            .await
            .expect_err("a slow producer is judged as before once the hold clears");
        assert!(
            reason.starts_with("rolling_insufficient_capacity:"),
            "{reason}"
        );
    }

    async fn scratch_hold_keeps_the_deadline(reason: AheadHoldReason) {
        let directory = crate::test_tempdir().expect("publication deadline");
        let session = test_session(directory.path().to_path_buf());
        let published = scratch_hold_published_and_held(&session, reason).await;
        session
            .publication_cycle_at(
                "scratch-hold-deadline",
                published + ROLLING_PUBLICATION_HARD - Duration::from_secs(1),
            )
            .await
            .expect("inside the deadline");
        let error = session
            .publication_cycle_at(
                "scratch-hold-deadline",
                published + ROLLING_PUBLICATION_HARD + Duration::from_secs(1),
            )
            .await
            .expect_err("only a scratch hold extends the publication deadline");
        assert!(error.contains("hard deadline"), "{reason:?}: {error}");
    }

    /// Only a scratch hold suspends the deadline. A producer held because it
    /// ran ahead of demand has nothing to wait for from the budget, and keeps
    /// the ordinary deadline.
    #[tokio::test]
    async fn scratch_hold_publication_deadline_still_applies_to_a_demand_hold() {
        scratch_hold_keeps_the_deadline(AheadHoldReason::Demand).await;
    }

    /// The per-session byte bound is the closest neighbour to the global one,
    /// but it holds a producer for running ahead of its own client, not for
    /// shared capacity. It keeps the ordinary deadline.
    #[tokio::test]
    async fn scratch_hold_publication_deadline_still_applies_to_a_bytes_hold() {
        scratch_hold_keeps_the_deadline(AheadHoldReason::Bytes).await;
    }

    #[tokio::test]
    async fn rolling_publication_budget_retention_cannot_pass_the_protected_prefix() {
        let directory = crate::test_tempdir().expect("retention snapshot");
        let session = Arc::new(test_session(directory.path().to_path_buf()));
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 16.0], false),
        )
        .await
        .expect("initial playlist");
        for index in 0..3 {
            tokio::fs::write(
                directory.path().join(format!("seg{index:05}.ts")),
                vec![u8::try_from(index).expect("test index"); 32],
            )
            .await
            .expect("segment");
        }
        session
            .publication_cycle("retention-snapshot")
            .await
            .expect("initial snapshot");
        session.refresh_segments().await;
        session.fetched_end_ms.store(200_000, Relaxed);
        gc_expired_segments(&session).await;
        assert_eq!(
            session
                .publication
                .lock()
                .await
                .served
                .as_ref()
                .map(|served| served.first_segment),
            Some(0),
            "a prune-only repair pass changed the immutable served snapshot"
        );
        assert!(
            commit_rolling_publication_media(&session, 2, 48_000).await,
            "legacy fetch frontier"
        );

        tokio::fs::write(
            directory.path().join("index.m3u8"),
            rolling_playlist(&[16.0, 16.0, 16.0, 16.0], false),
        )
        .await
        .expect("advanced playlist");
        tokio::fs::write(directory.path().join("seg00003.ts"), vec![3_u8; 32])
            .await
            .expect("advanced segment");
        {
            let mut clock = session.publication.lock().await;
            let due = Instant::now() - Duration::from_millis(1);
            clock.next_publish_at = Some(due);
            clock.served.as_mut().expect("served snapshot").available_at =
                due - ROLLING_PUBLICATION_TARGET;
        }
        session
            .publication_cycle("retention-snapshot")
            .await
            .expect("sliding snapshot");
        let served = session
            .publication
            .lock()
            .await
            .served
            .as_ref()
            .expect("served snapshot")
            .raw
            .clone();
        let served = String::from_utf8_lossy(&served);
        assert!(served.contains("#EXT-X-MEDIA-SEQUENCE:0"), "{served}");
        let segments = session.segments.lock().await;
        assert!(matches!(
            segments.segs[0].visibility,
            SegmentVisibility::Advertised
        ));
        assert_eq!(
            segments.segs[0].bytes, 32,
            "protected media remains charged"
        );
        assert!(directory.path().join("seg00000.ts").exists());
    }

    #[tokio::test]
    async fn mkv_hls_retention_retired_objects_are_read_only_until_grace() {
        use plurx_core::store::SqliteStore;

        let directory = crate::test_tempdir().expect("retired grace");
        let manager = Arc::new(TranscodeManager::new(
            Arc::new(SqliteStore::open_in_memory().expect("store")),
            directory.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session = Arc::new(test_session(directory.path().to_path_buf()));
        tokio::fs::write(directory.path().join("seg00000.ts"), b"promised")
            .await
            .expect("segment");
        tokio::fs::write(directory.path().join("init.mp4"), b"init")
            .await
            .expect("init");
        let now = Instant::now();
        let serve_until = now + Duration::from_secs(30);
        session.segments.lock().await.segs.push(SegmentMeta {
            index: 0,
            name: "seg00000.ts".to_owned(),
            start_ms: 0,
            end_ms: 16_000,
            bytes: 8,
            visibility: SegmentVisibility::Grace {
                removed_at: now,
                serve_until,
            },
        });
        let producer_attempt = session.control.current_producer_attempt();
        manager.retired_presentations.lock().await.insert(
            "retired-grace".to_owned(),
            RetiredPresentation {
                session: Arc::clone(&session),
                producer_attempt,
                serve_until,
            },
        );

        let segment = match manager
            .segment_for_publication("retired-grace", "seg00000.ts")
            .await
            .expect("segment lookup")
        {
            SegmentPublication::Ready(segment) => segment,
            _ => panic!("promised retired segment was not readable"),
        };
        let owner = segment.response_owner();
        let authorization = manager
            .authorize_response_publication(
                "retired-grace",
                &owner,
                MediaResponsePublication::attempt_media("segment-range", Some("seg00000.ts")),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("read-only range authorization");
        manager
            .commit_authorized_media(authorization, true, Instant::now() + Duration::from_secs(1))
            .await
            .expect("read-only completion");
        let init = match manager
            .segment_for_publication("retired-grace", "init.mp4")
            .await
            .expect("init lookup")
        {
            SegmentPublication::Ready(init) => init,
            _ => panic!("promised retired init was not readable"),
        };
        let init_authorization = manager
            .authorize_response_publication(
                "retired-grace",
                &init.response_owner(),
                MediaResponsePublication::attempt_media("init-segment", Some("init.mp4")),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("read-only init authorization");
        manager
            .commit_authorized_media(
                init_authorization,
                true,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("read-only init completion");
        assert_eq!(
            session
                .control
                .snapshot()
                .await
                .expect("actor")
                .delivery
                .fetched_segment,
            None,
            "grace reads must not renew or advance retired delivery"
        );

        if let SegmentVisibility::Grace { serve_until, .. } =
            &mut session.segments.lock().await.segs[0].visibility
        {
            *serve_until = Instant::now() - Duration::from_millis(1);
        }
        assert!(matches!(
            manager
                .segment_for_publication("retired-grace", "seg00000.ts")
                .await
                .expect("expired lookup"),
            SegmentPublication::Missing(_)
        ));
    }

    #[test]
    fn mkv_hls_retention_grace_deadline_is_half_open() {
        let removed_at = Instant::now();
        let serve_until = removed_at + Duration::from_secs(64);
        let visibility = SegmentVisibility::Grace {
            removed_at,
            serve_until,
        };
        assert!(visibility.is_servable(serve_until - Duration::from_millis(1)));
        assert!(!visibility.is_servable(serve_until));
        assert!(!visibility.is_servable(serve_until + Duration::from_millis(1)));
        assert!(matches!(
            visibility,
            SegmentVisibility::Grace {
                removed_at: recorded,
                ..
            } if recorded == removed_at
        ));
    }

    #[test]
    fn every_rolling_writer_is_wrapped_in_one_fixed_covering_target() {
        let early = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                      #EXTINF:2.0,\nseg00000.ts\n";
        let later = b"#EXTM3U\n#EXT-X-TARGETDURATION:9\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
                      #EXTINF:2.0,\nseg00000.ts\n#EXTINF:8.5,\nseg00001.ts\n";
        assert!(validate_rolling_target(early).is_ok());
        assert!(validate_rolling_target(later).is_ok());

        for raw in [early.as_slice(), later.as_slice()] {
            let served = String::from_utf8(
                served_live_playlist(raw.to_vec(), Some(0), None, None).expect("served playlist"),
            )
            .expect("UTF-8");
            assert_eq!(served.matches("#EXT-X-TARGETDURATION:16\n").count(), 1);
            assert!(!served.contains("#EXT-X-TARGETDURATION:2\n"));
            assert!(!served.contains("#EXT-X-TARGETDURATION:9\n"));
        }
    }

    #[test]
    fn rolling_target_validation_rejects_one_tick_over_without_rounding() {
        let exact = b"#EXTM3U\n#EXT-X-TARGETDURATION:16\n#EXTINF:16.0,\nseg00000.ts\n";
        let over = b"#EXTM3U\n#EXT-X-TARGETDURATION:17\n#EXTINF:16.000001,\nseg00000.ts\n";
        assert!(validate_rolling_target(exact).is_ok());
        let reason = validate_rolling_target(over).expect_err("oversized EXTINF");
        assert!(reason.contains("exceeded the fixed 16s"), "{reason}");
    }

    /// The writer keeps an append-only EVENT history, but a client must never
    /// be offered names retention already unlinked. This pure pin also covers
    /// the HLS shape: a playlist that removes old entries is a sliding media
    /// playlist, not an EVENT playlist, and its first sequence advances.
    #[test]
    fn served_live_playlist_advances_past_the_pruned_prefix() {
        let raw = "#EXTM3U\n\
                   #EXT-X-VERSION:7\n\
                   #EXT-X-TARGETDURATION:10\n\
                   #EXT-X-MEDIA-SEQUENCE:0\n\
                   #EXT-X-PLAYLIST-TYPE:EVENT\n\
                   #EXT-X-MAP:URI=\"init.mp4\"\n\
                   #EXTINF:4.000,\n\
                   seg00000.m4s\n\
                   #EXTINF:10.500,\n\
                   seg00001.m4s\n\
                   #EXT-X-DISCONTINUITY\n\
                   #EXTINF:6.000,\n\
                   seg00002.m4s\n\
                   #EXT-X-ENDLIST\n";

        let first = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), Some(0), None, None)
                .expect("valid rolling snapshot"),
        )
        .expect("initial playlist");
        assert!(!first.contains("#EXT-X-PLAYLIST-TYPE:EVENT"), "{first}");
        assert!(first.contains("#EXT-X-START:TIME-OFFSET=0"), "{first}");
        assert!(first.contains("#EXT-X-TARGETDURATION:16"), "{first}");

        let served = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), Some(2), None, None)
                .expect("valid rolling snapshot"),
        )
        .expect("playlist utf8");
        assert!(served.contains("#EXT-X-MEDIA-SEQUENCE:2"), "{served}");
        assert!(!served.contains("#EXT-X-PLAYLIST-TYPE:EVENT"), "{served}");
        assert!(!served.contains("seg00000.m4s"), "{served}");
        assert!(!served.contains("seg00001.m4s"), "{served}");
        assert!(
            served.contains("#EXT-X-DISCONTINUITY\n#EXTINF:6.000,\nseg00002.m4s"),
            "tags attached to the first retained segment survive: {served}"
        );
        assert!(served.ends_with("seg00002.m4s\n#EXT-X-ENDLIST\n"));
    }

    /// The rolling contract is not merely that EVENT disappears after a
    /// prune; it is that one session URL presents the same typeless envelope
    /// before and after that boundary. Only MEDIA-SEQUENCE and the retained
    /// body are allowed to advance.
    #[test]
    fn typeless_sliding_playlist_keeps_one_shape_across_pruning() {
        let raw = "#EXTM3U\n\
                   #EXT-X-VERSION:7\n\
                   #EXT-X-TARGETDURATION:10\n\
                   #EXT-X-MEDIA-SEQUENCE:0\n\
                   #EXT-X-PLAYLIST-TYPE:EVENT\n\
                   #EXT-X-MAP:URI=\"init.mp4\"\n\
                   #EXTINF:4.000,\n\
                   seg00000.m4s\n\
                   #EXTINF:4.000,\n\
                   seg00001.m4s\n\
                   #EXTINF:4.000,\n\
                   seg00002.m4s\n";
        let before = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), Some(0), None, None)
                .expect("valid rolling snapshot"),
        )
        .expect("before utf8");
        let after = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), Some(2), None, None)
                .expect("valid rolling snapshot"),
        )
        .expect("after utf8");

        for playlist in [&before, &after] {
            assert!(
                !playlist.contains("#EXT-X-PLAYLIST-TYPE:EVENT"),
                "{playlist}"
            );
            assert!(
                playlist.contains("#EXT-X-START:TIME-OFFSET=0"),
                "{playlist}"
            );
        }
        let stable_headers = |playlist: &str| {
            playlist
                .lines()
                .take_while(|line| !line.starts_with("#EXTINF:"))
                .filter(|line| !line.starts_with("#EXT-X-MEDIA-SEQUENCE:"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(stable_headers(&before), stable_headers(&after));
        assert!(before.contains("#EXT-X-MEDIA-SEQUENCE:0"), "{before}");
        assert!(after.contains("#EXT-X-MEDIA-SEQUENCE:2"), "{after}");
        assert!(before.contains("seg00000.m4s"), "{before}");
        assert!(!after.contains("seg00000.m4s"), "{after}");
        assert!(after.contains("seg00002.m4s"), "{after}");
    }

    #[test]
    fn rolling_playlist_refuses_an_inconsistent_writer_snapshot() {
        let raw = b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:4.0,\nseg00000.ts\n";
        assert!(
            served_live_playlist(raw.to_vec(), Some(1), None, None).is_none(),
            "a missing retained boundary must not expose the raw EVENT history"
        );
        assert!(
            served_live_playlist(
                b"#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n".to_vec(),
                Some(0),
                None,
                None
            )
            .is_none(),
            "no playable segment is not a presentation"
        );
    }

    #[test]
    fn takeover_playlist_declares_one_monotone_discontinuity() {
        let raw = "#EXTM3U\n\
                   #EXT-X-VERSION:7\n\
                   #EXT-X-TARGETDURATION:2\n\
                   #EXT-X-MEDIA-SEQUENCE:4\n\
                   #EXT-X-PLAYLIST-TYPE:EVENT\n\
                   #EXT-X-MAP:URI=\"init-e2.mp4\"\n\
                   #EXTINF:2.000,\n\
                   seg00004.m4s\n\
                   #EXTINF:2.000,\n\
                   seg00005.m4s\n";
        let takeover = SessionTakeoverStart {
            provisional_session_id: "provisional-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            origin_base_ms: 0,
            frontier_offset_ms: 8_000,
            media_sequence: 4,
            discontinuity_sequence: 1,
            owner_epoch: 2,
        };
        let first = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), Some(4), None, Some(&takeover))
                .expect("valid rolling snapshot"),
        )
        .expect("takeover playlist");
        assert!(first.contains("#EXT-X-MEDIA-SEQUENCE:4"), "{first}");
        assert!(first.contains("#EXT-X-DISCONTINUITY-SEQUENCE:0"), "{first}");
        assert_eq!(first.matches("#EXT-X-DISCONTINUITY\n").count(), 1);
        assert!(first.contains("#EXT-X-MAP:URI=\"init-e2.mp4\""));

        let slid = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), Some(5), None, Some(&takeover))
                .expect("valid rolling snapshot"),
        )
        .expect("slid takeover playlist");
        assert!(slid.contains("#EXT-X-DISCONTINUITY-SEQUENCE:1"), "{slid}");
        assert!(!slid.contains("#EXT-X-DISCONTINUITY\n"), "{slid}");
    }

    /// A successor numbers from its epoch floor, so "nothing has been pruned
    /// yet" is that floor and not zero. Measuring from zero made the first
    /// response of every takeover claim it had already begun sliding, and
    /// pinned MEDIA-SEQUENCE to 0 while the segments on disk were numbered in
    /// the millions.
    #[test]
    fn an_untouched_takeover_playlist_reports_its_epoch_floor() {
        let raw = "#EXTM3U\n\
                   #EXT-X-VERSION:7\n\
                   #EXT-X-TARGETDURATION:2\n\
                   #EXT-X-MEDIA-SEQUENCE:2000000\n\
                   #EXT-X-MAP:URI=\"init-e2.mp4\"\n\
                   #EXTINF:2.000,\n\
                   seg2000000.m4s\n\
                   #EXTINF:2.000,\n\
                   seg2000001.m4s\n";
        let takeover = SessionTakeoverStart {
            provisional_session_id: "provisional-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            origin_base_ms: 0,
            frontier_offset_ms: 8_000,
            media_sequence: 2_000_000,
            discontinuity_sequence: 1,
            owner_epoch: 2,
        };
        let served = String::from_utf8(
            served_live_playlist(raw.as_bytes().to_vec(), None, None, Some(&takeover))
                .expect("valid rolling snapshot"),
        )
        .expect("takeover playlist");

        assert!(served.contains("#EXT-X-MEDIA-SEQUENCE:2000000"), "{served}");
        assert!(
            served.contains("#EXT-X-DISCONTINUITY-SEQUENCE:0"),
            "{served}"
        );
        assert_eq!(served.matches("#EXT-X-DISCONTINUITY\n").count(), 1);
        assert!(served.contains("seg2000000.m4s"), "{served}");
        assert!(served.contains("seg2000001.m4s"), "{served}");

        // Every URI the successor advertises must be one the serving path
        // will actually hand back.
        for line in served.lines() {
            let line = line.trim();
            if let Some(uri) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
                let uri = uri.trim_end_matches('"');
                assert!(
                    is_safe_segment(uri),
                    "advertised init {uri} must be servable"
                );
            } else if !line.starts_with('#') && !line.is_empty() {
                assert!(
                    is_safe_segment(line),
                    "advertised segment {line} must be servable"
                );
            }
        }
    }

    // ---- the append-oriented index (review §2.6) ----------------------------

    /// The steady state: a grown playlist APPENDS to the index. Known
    /// entries keep their measured sizes without being re-parsed, and the new
    /// entry's timeline continues from the held cursor.
    #[test]
    fn the_index_appends_what_is_new_and_keeps_what_it_measured() {
        let two = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:3.0,\nseg00001.ts\n";
        let mut index = SegmentIndex {
            segs: parse_playlist(two),
            revision: 0,
        };
        index.segs[0].bytes = 111;
        index.segs[1].bytes = 222;

        let three = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:3.0,\nseg00001.ts\n\
                     #EXTINF:2.5,\nseg00002.ts\n";
        assert!(
            !index.extend_from_playlist(three),
            "an append, not a rebuild"
        );
        assert_eq!(index.segs.len(), 3);
        assert_eq!(
            (index.segs[0].bytes, index.segs[1].bytes),
            (111, 222),
            "measured sizes stay put"
        );
        assert_eq!(
            (index.segs[2].start_ms, index.segs[2].end_ms),
            (5_000, 7_500),
            "the new entry continues the held timeline"
        );
        assert_eq!(
            index.segs[2].bytes, 0,
            "and is not pretended to be measured"
        );

        // Nothing new: still not a rebuild, still three.
        assert!(!index.extend_from_playlist(three));
        assert_eq!(index.segs.len(), 3);
    }

    /// A playlist with fewer entries than the index is a truncation or a
    /// recovery; what is held describes files that are gone. Rebuild, and
    /// drop the carried sizes with the timeline they measured.
    #[test]
    fn a_shrunken_playlist_rebuilds_the_index() {
        let three = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n\
                     #EXTINF:2.0,\nseg00002.ts\n";
        let mut index = SegmentIndex {
            segs: parse_playlist(three),
            revision: 0,
        };
        index.segs[0].bytes = 999;
        let one = "#EXTM3U\n#EXTINF:4.0,\nseg00000.ts\n";
        assert!(index.extend_from_playlist(one), "a shrink is a rebuild");
        assert_eq!(index.segs.len(), 1);
        assert_eq!(index.segs[0].end_ms, 4_000, "the new timeline, not the old");
        assert_eq!(
            index.segs[0].bytes, 0,
            "carried sizes went with the old one"
        );
    }

    /// The fallback respawn clears the directory and rewrites the timeline
    /// from the same seek point — same names, same indices, different cut
    /// points. The sentinel is the last known entry's duration: when it
    /// disagrees, everything held describes a timeline that no longer
    /// exists.
    #[test]
    fn a_replaced_timeline_is_caught_by_its_last_known_entry() {
        let two = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n";
        let mut index = SegmentIndex {
            segs: parse_playlist(two),
            revision: 0,
        };
        index.segs[1].bytes = 555;
        // Same names, but the second segment now cuts at 5s, and a third
        // exists. A blind append would graft a new timeline onto a stale one.
        let replaced = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:5.0,\nseg00001.ts\n\
                        #EXTINF:2.0,\nseg00002.ts\n";
        assert!(
            index.extend_from_playlist(replaced),
            "the disagreement rebuilds"
        );
        assert_eq!(index.segs.len(), 3);
        assert_eq!(
            (index.segs[1].start_ms, index.segs[1].end_ms),
            (2_000, 7_000),
            "the rewritten cut, not the remembered one"
        );
        assert_eq!(index.segs[1].bytes, 0, "and no stale size survives it");
    }

    #[test]
    fn a_prepared_stale_prefix_cannot_regress_a_newer_index() {
        let three = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n\
                     #EXTINF:2.0,\nseg00002.ts\n";
        let two = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n";
        let mut current = SegmentIndex {
            segs: parse_playlist(three),
            revision: 0,
        };
        current.segs[0].bytes = 111;
        let observed = SegmentIndex {
            segs: parse_playlist(two),
            revision: 0,
        };

        assert_eq!(current.merge_prepared(0, observed), Some(false));
        assert_eq!(current.segs.len(), 3, "an older concurrent read is ignored");
        assert_eq!(current.segs[0].bytes, 111, "measured state is retained");
    }

    #[test]
    fn an_older_same_attempt_rewrite_cannot_land_after_a_newer_one() {
        let base = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n";
        let newer = "#EXTM3U\n#EXTINF:3.0,\nseg00000.ts\n#EXTINF:3.0,\nseg00001.ts\n";
        let older = "#EXTM3U\n#EXTINF:4.0,\nseg00000.ts\n#EXTINF:4.0,\nseg00001.ts\n";
        let mut current = SegmentIndex {
            segs: parse_playlist(base),
            revision: 0,
        };
        let captured_revision = current.revision;

        assert_eq!(
            current.merge_prepared(
                captured_revision,
                SegmentIndex {
                    segs: parse_playlist(newer),
                    revision: 0,
                },
            ),
            Some(true),
            "the first completed rewrite lands"
        );
        assert_eq!(
            current.merge_prepared(
                captured_revision,
                SegmentIndex {
                    segs: parse_playlist(older),
                    revision: 0,
                },
            ),
            None,
            "the out-of-order preparation is fenced by its captured revision"
        );
        assert_eq!(current.segs[0].end_ms, 3_000);
        assert_eq!(current.revision, captured_revision + 1);
    }

    /// The two byte figures answer different questions (review §2.7): the
    /// AHEAD figure is the client's reserve, the TOTAL is what the disk
    /// actually holds — the retention window behind the frontier is on disk
    /// too, and summing reserves let several healthy sessions blow through
    /// the documented scratch cap by a whole window each.
    #[test]
    fn the_disk_budget_counts_retained_bytes_the_reserve_does_not() {
        let mut index = SegmentIndex {
            segs: (0..10)
                .map(|i| SegmentMeta {
                    index: i,
                    name: format!("seg{i:05}.ts"),
                    start_ms: i * 4_000,
                    end_ms: (i + 1) * 4_000,
                    bytes: 1_000_000,
                    visibility: SegmentVisibility::Advertised,
                })
                .collect(),
            revision: 0,
        };
        // Frontier at 20s: five segments ahead of it, five behind it —
        // retained for scrubbing, and every one of them still on disk.
        let ahead = ahead_of(&index, 20_000).expect("published");
        assert_eq!(ahead.bytes, 5_000_000, "the reserve is what pacing sees");
        assert_eq!(
            index.total_bytes(),
            10_000_000,
            "the disk holds the retention window too — the cap must count it"
        );
        // Retention deletes two; the budget follows the disk down.
        for seg in index.segs.iter_mut().take(2) {
            seg.bytes = 0;
            seg.visibility = SegmentVisibility::Deleted;
        }
        assert_eq!(index.total_bytes(), 8_000_000);
        assert_eq!(
            ahead_of(&index, 20_000).expect("published").bytes,
            5_000_000,
            "and the reserve never noticed — different questions"
        );
    }

    /// Flow control's limits come from the snapshot while it is fresh: an
    /// admin's change lands within the TTL, and the hot path stops paying
    /// three settings reads per segment event.
    #[tokio::test]
    async fn flow_limits_are_snapshotted_until_stale() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );

        let first = mgr.ahead_limits().await;
        assert_eq!(first.max_secs, HLS_AHEAD_MAX_SECS_DEFAULT);

        store
            .put_setting(keys::HLS_AHEAD_MAX_SECS, "999")
            .await
            .expect("setting");
        assert_eq!(
            mgr.ahead_limits().await.max_secs,
            HLS_AHEAD_MAX_SECS_DEFAULT,
            "within the TTL the snapshot answers — that is the whole point"
        );
        mgr.forget_cached_limits();
        assert_eq!(
            mgr.ahead_limits().await.max_secs,
            999,
            "a stale snapshot re-reads the settings"
        );
    }

    #[tokio::test]
    async fn mkv_hls_scratch_reservations_serialize_concurrent_starts() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let per_session = 100_i64;
        let reservation = per_session + ROLLING_SCRATCH_IN_FLIGHT_BYTES;
        store
            .put_setting(keys::HLS_AHEAD_MAX_BYTES, &per_session.to_string())
            .await
            .expect("per-session ceiling");
        store
            .put_setting(
                keys::HLS_SCRATCH_MAX_BYTES,
                &reservation.saturating_mul(2).saturating_sub(1).to_string(),
            )
            .await
            .expect("global ceiling");

        let first = mgr
            .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
            .await
            .expect("first permit");
        assert!(mgr
            .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
            .await
            .is_err());
        drop(first);
        assert!(mgr
            .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
            .await
            .is_ok());
    }

    /// Missing information never shortens a promise.
    #[test]
    fn scratch_charge_only_the_audited_transport_earns_a_shorter_promise() {
        assert_eq!(
            ReleaseClass::from_transport(Some("hlsjs")),
            ReleaseClass::HlsJs
        );
        for conservative in [
            None,
            Some("native"),
            Some("airplay"),
            Some("hls"),
            Some("hlsjs2"),
            Some(""),
        ] {
            assert_eq!(
                ReleaseClass::from_transport(conservative),
                ReleaseClass::Conservative,
                "{conservative:?} is not the audited teardown contract"
            );
        }
        assert_eq!(
            EXACT_RELEASE_ALLOWANCE.as_secs(),
            u64::from(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS),
            "the allowance is one session segment target, not the server's blocking budget"
        );

        assert!(session_transport_is_valid("hlsjs"));
        assert!(session_transport_is_valid("native"));
        assert!(!session_transport_is_valid(""));
        assert!(!session_transport_is_valid("HLSJS"), "labels stay bounded");
        assert!(!session_transport_is_valid("hls js"));
        assert!(!session_transport_is_valid(&"x".repeat(33)));
    }

    /// One accepted release moves the promise in, once. A duplicate DELETE
    /// cannot extend it, renew it, or shorten it a second time.
    #[test]
    fn scratch_charge_an_exact_release_shortens_one_promise_and_never_renews_it() {
        let now = Instant::now();
        let release = RetiredRelease::new();
        let original = now + Duration::from_secs(360);
        release.publish(original);

        assert!(release.accept_release(now, EXACT_RELEASE_ALLOWANCE));
        let shortened = release
            .shorten_to(now + EXACT_RELEASE_ALLOWANCE)
            .expect("the first release moves the deadline");
        assert_eq!(release.deadline(), Some(shortened));
        assert!(shortened < original);

        // A duplicate DELETE arrives a minute later.
        let later = now + Duration::from_secs(60);
        assert!(
            !release.accept_release(later, EXACT_RELEASE_ALLOWANCE),
            "the latch is taken; a second release is a no-op"
        );
        assert_eq!(
            release.shorten_to(later + EXACT_RELEASE_ALLOWANCE),
            None,
            "a later deadline is not a shortening"
        );
        assert_eq!(release.deadline(), Some(shortened));
    }

    #[tokio::test]
    async fn scratch_charge_a_stale_or_unknown_release_reaches_no_promise() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        assert_eq!(
            mgr.accept_exact_session_release("never-existed", ReleaseClass::HlsJs)
                .await,
            None,
            "an unknown session id names no incarnation"
        );

        let dir = crate::test_tempdir().expect("scratch");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        mgr.register_session_for_test("live-one", Arc::clone(&session))
            .await;
        assert_eq!(
            mgr.accept_exact_session_release("live-one", ReleaseClass::Conservative)
                .await,
            None,
            "a class with no allowance records nothing at all"
        );
        assert_eq!(
            session.retired_release.released(),
            None,
            "and leaves the latch untaken for a later eligible release"
        );
    }

    /// The startup allowance has to cover the gate that actually holds the
    /// first playlist back, or a session admitted under it can never publish
    /// anything for a client to drain.
    #[test]
    fn scratch_charge_startup_sizing_covers_the_effective_publish_gate() {
        // 48 s of rolling runway at 1x, plus one 16 s segment, at 8 Mb/s.
        let bytes = rolling_startup_bytes(Some(8_000_000.0), 1.0);
        let media = 64 * 8_000_000 / 8;
        assert!(
            bytes > media,
            "sizing must exceed the {media} bytes of startup media: {bytes}"
        );
        assert!(
            bytes
                > i64::from(plurx_core::transcode::COPY_PUBLISH_GATE_SECS)
                    .saturating_mul(8_000_000 / 8),
            "the copy writer's own 12 s gate is not the effective one"
        );
        // Scaling with playback rate moves the runway with it.
        assert!(rolling_startup_bytes(Some(8_000_000.0), 2.0) > bytes);
        // An unknown rate gets the bootstrap, which is explicitly not a bound.
        assert_eq!(
            rolling_startup_bytes(None, 1.0),
            ROLLING_SCRATCH_UNKNOWN_RATE_BYTES
        );
        assert_eq!(
            rolling_scratch_envelope(None, 1.0),
            ROLLING_SCRATCH_UNKNOWN_RATE_BYTES
        );
        // Nothing smaller than one in-flight object is ever admitted.
        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: HLS_AHEAD_MAX_BYTES_DEFAULT,
            global_max_bytes: HLS_SCRATCH_MAX_BYTES_DEFAULT,
        };
        assert_eq!(
            RollingScratchSizing::Startup(1).grant_bytes(limits),
            ROLLING_SCRATCH_MIN_GRANT_BYTES
        );
        // And nothing larger than the ceiling the whole-reservation path uses.
        assert_eq!(
            RollingScratchSizing::Startup(i64::MAX).grant_bytes(limits),
            RollingScratchSizing::SessionCeiling.grant_bytes(limits)
        );
    }

    /// A real refusal, from the real admission path, through the real HTTP
    /// mapper. Constructing an already-prefixed string and asserting the
    /// mapper answers 503 would pass on the broken build too: the defect was
    /// that `reserve_rolling_scratch` returned a bare `String` that carried
    /// no class at all, so `session_start_error` fell through to an anonymous
    /// 500 and the browser treated a full scratch budget as a corrupt movie.
    #[tokio::test]
    async fn scratch_charge_real_refusal_answers_503_through_the_http_mapper() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let per_session = 100_i64;
        let reservation = per_session + ROLLING_SCRATCH_IN_FLIGHT_BYTES;
        store
            .put_setting(keys::HLS_AHEAD_MAX_BYTES, &per_session.to_string())
            .await
            .expect("per-session ceiling");
        store
            .put_setting(keys::HLS_SCRATCH_MAX_BYTES, &reservation.to_string())
            .await
            .expect("global ceiling");

        let _admitted = mgr
            .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
            .await
            .expect("the budget admits exactly one");
        let refusal = mgr
            .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
            .await
            .expect_err("a full budget refuses the next start");

        assert!(
            is_retryable_capacity_error(&refusal),
            "a full scratch budget is temporary contention, not a server fault: {refusal}"
        );
        assert!(
            refusal.contains(&reservation.to_string()),
            "the operator needs the figures to tell a full budget from a stuck cleanup: {refusal}"
        );
        assert_eq!(
            crate::http::hls::session_start_error_status_for_test(7, refusal),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// Real retained inventory, not reservations, is what the budget sums.
    ///
    /// One playback plus three successive replacements is the incident: at
    /// the default 2 GiB + 64 MiB reservation the fourth could never be
    /// admitted, whatever was actually on the disk. Here the first three
    /// retire with a small measured inventory and the fourth fits.
    #[tokio::test]
    async fn scratch_charge_a_fourth_replacement_fits_when_the_retained_bytes_fit() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        // The shipped defaults: a 2 GiB per-session ceiling under an 8 GiB
        // budget, which admits exactly three full reservations.
        let ceiling = HLS_AHEAD_MAX_BYTES_DEFAULT + ROLLING_SCRATCH_IN_FLIGHT_BYTES;
        assert!(HLS_SCRATCH_MAX_BYTES_DEFAULT / ceiling == 3);

        let mut retired = Vec::new();
        for _ in 0..3 {
            let permit = mgr
                .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
                .await
                .expect("a replacement admits");
            let key = permit.key();
            let ledger = permit.ledger();
            // Real transitions: fence the writers, then convert on a
            // completed measurement. No permit is dropped to make room.
            ledger.begin_retirement(key);
            let generation = ledger.inventory_generation(key).expect("generation");
            assert!(ledger.commit_quiescent_measurement(key, generation, 128 * 1024 * 1024));
            retired.push(permit);
        }
        let (charged, _) = mgr.global_flow_bytes().await;
        assert_eq!(charged, 3 * 128 * 1024 * 1024);
        let fourth = mgr
            .reserve_rolling_scratch(RollingScratchSizing::SessionCeiling)
            .await;
        assert!(
            fourth.is_ok(),
            "384 MiB of retained media must not exhaust an 8 GiB budget"
        );
        let keys: Vec<_> = retired.iter().map(|permit| permit.key()).collect();
        drop((fourth, retired));
        assert_eq!(
            mgr.scratch_snapshot().total,
            3 * 128 * 1024 * 1024,
            "dropping the owner is not proof that a file disappeared; the bytes stay charged"
        );
        for key in keys {
            // What cleanup does once it has proved the names are gone.
            mgr.scratch_ledger.account_unlink_all(key);
        }
        assert_eq!(mgr.scratch_snapshot().total, 0);
        assert_eq!(mgr.scratch_snapshot().entries, 0);
    }

    /// A partial scan is not an inventory. The scanner preserves the previous
    /// charge on a stat failure, which is right and also indistinguishable at
    /// the call site from "measured that much" — so it reports which happened
    /// and the conversion refuses to collapse a reservation onto a guess.
    #[tokio::test]
    async fn scratch_charge_an_incomplete_measurement_keeps_the_conservative_charge() {
        let dir = crate::test_tempdir().expect("scratch");
        tokio::fs::write(dir.path().join("init.mp4"), vec![0_u8; 17])
            .await
            .expect("init");
        let session = test_session(dir.path().to_path_buf());
        assert_eq!(
            session.measure_scratch_bytes().await,
            ScratchMeasurement::Complete(17)
        );

        let missing = test_session(dir.path().join("gone-with-the-session"));
        assert_eq!(
            missing.measure_scratch_bytes().await,
            ScratchMeasurement::Absent,
            "a removed directory owes nothing; that is not the same as a failed read"
        );

        // A sparse fixture: the scanner sums apparent length, so the charge
        // is the promise the namespace makes and not its allocated blocks.
        let sparse = crate::test_tempdir().expect("sparse");
        let file = tokio::fs::File::create(sparse.path().join("seg00001.m4s"))
            .await
            .expect("sparse segment");
        file.set_len(4 * 1024 * 1024)
            .await
            .expect("apparent length");
        drop(file);
        let sparse_session = test_session(sparse.path().to_path_buf());
        assert_eq!(
            sparse_session.measure_scratch_bytes().await,
            ScratchMeasurement::Complete(4 * 1024 * 1024)
        );
    }

    #[tokio::test]
    async fn mkv_hls_scratch_measurement_includes_non_playlist_files() {
        let dir = crate::test_tempdir().expect("scratch");
        tokio::fs::write(dir.path().join("init.mp4"), vec![0_u8; 17])
            .await
            .expect("init");
        tokio::fs::write(dir.path().join("segment.tmp"), vec![0_u8; 29])
            .await
            .expect("temporary segment");
        let session = test_session(dir.path().to_path_buf());
        let _ = session.refresh_scratch_bytes().await;
        assert_eq!(session.live_bytes.load(Acquire), 46);
    }

    /// A pruned segment's file is deleted on purpose; the flag is what stops
    /// every later refresh from re-statting it forever, and an extend must
    /// not lose it.
    #[test]
    fn a_pruned_segment_stays_pruned_through_an_append() {
        let two = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n";
        let mut index = SegmentIndex {
            segs: parse_playlist(two),
            revision: 0,
        };
        index.segs[0].bytes = 0;
        index.segs[0].visibility = SegmentVisibility::Deleted;
        let three = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXTINF:2.0,\nseg00001.ts\n\
                     #EXTINF:2.0,\nseg00002.ts\n";
        assert!(!index.extend_from_playlist(three));
        assert_eq!(index.segs[0].visibility, SegmentVisibility::Deleted);
        assert_eq!(
            index.segs[2].visibility,
            SegmentVisibility::Advertised,
            "the new entry is advertised"
        );
    }

    #[test]
    fn ahead_is_measured_against_the_fetched_frontier() {
        let index = SegmentIndex {
            segs: (0..10)
                .map(|i| SegmentMeta {
                    index: i,
                    name: format!("seg{i:05}.ts"),
                    start_ms: i * 4_000,
                    end_ms: (i + 1) * 4_000,
                    bytes: 1_000_000,
                    visibility: SegmentVisibility::Advertised,
                })
                .collect(),
            revision: 0,
        };
        // 40s published, nothing fetched → the whole 40s is reserve.
        let a = ahead_of(&index, 0).expect("published");
        assert_eq!(a.seconds, 40);
        assert_eq!(a.bytes, 10_000_000);
        // Frontier at 16s → 24s and six segments of reserve.
        let a = ahead_of(&index, 16_000).expect("published");
        assert_eq!(a.seconds, 24);
        assert_eq!(a.bytes, 6_000_000);
        // Caught up entirely.
        let a = ahead_of(&index, 40_000).expect("published");
        assert_eq!((a.seconds, a.bytes), (0, 0));
        // Nothing published is not "zero ahead" — it is "don't know".
        assert_eq!(ahead_of(&SegmentIndex::default(), 0), None);
    }

    /// One control exchange as the player sends it, carrying the same position
    /// and buffer the flow evaluation was given. The two sides of the loop must
    /// be reading one client, not two fixtures that happen to agree.
    fn control_request(
        demand: &crate::playback_control::PlaybackDemandSnapshot,
        render_state: crate::playback_control::RenderState,
        starved: bool,
    ) -> crate::playback_control::ControlRequestV1 {
        crate::playback_control::ControlRequestV1 {
            intent: None,
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation: uuid::Uuid::new_v4().to_string(),
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: demand.demand,
            position_ms: demand.position_ms,
            buffered_from_ms: demand.buffered_from_ms,
            buffered_through_ms: demand.buffered_through_ms,
            playback_rate: demand.playback_rate,
            render_state,
            seek_target_ms: demand.seek_target_ms,
            observed_download_bps: demand.observed_download_bps,
            selection: demand.selection.clone(),
            capabilities: demand.capabilities.clone(),
            observation: starved.then_some(crate::playback_control::ClientObservation {
                dropped_frames: None,
                decoder_state: Some(crate::playback_control::DecoderState::Starved),
                error_code: None,
                error_detail: None,
            }),
            acknowledgement: None,
            // The wire names, exactly as a client that has rolled out declares
            // them.
            supported_actions: Some(vec![
                "hold".to_owned(),
                "terminal".to_owned(),
                "retry_resource".to_owned(),
            ]),
        }
    }

    #[test]
    fn seek_buffer_gap_is_not_counted_as_producer_runway() {
        let mut demand = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        // Last accepted snapshot in the September 14 incident. No actual
        // buffer yet; the 68-second clock jump must not count as runway.
        demand.position_ms = 797_711;
        demand.buffered_from_ms = None;
        demand.buffered_through_ms = 797_711;
        demand.seek_target_ms = Some(729_643);
        demand.render_state = crate::playback_control::RenderState::Seeking;
        let flow = |demand: &crate::playback_control::PlaybackDemandSnapshot| {
            evaluate_flow(FlowInputs {
                physical_ahead: Some(Ahead {
                    seconds: 0,
                    bytes: 0,
                }),
                published_end_ms: Some(86_086),
                staged_publication_seconds: None,
                media_origin_ms: 729_643,
                lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
                demand: Some(demand),
                global_live_bytes: 64_391_174,
                global_ahead_bytes: 0,
                limits: AheadLimits {
                    max_secs: 180,
                    max_bytes: 0,
                    global_max_bytes: 0,
                },
                currently_suspended: true,
                scratch_grant_exhausted: None,
                startup_protected: false,
            })
        };
        let stale = flow(&demand);
        assert_eq!(demand.runway_ms(), 0);
        assert_eq!(stale.production_ahead_seconds, Some(86));
        assert_eq!(stale.production_target_seconds, Some(16));
        assert_eq!(stale.hold, None);

        // Counterfactual: if the client were presenting at this position and
        // could report its real 11.5-second buffer without a stale seek, the
        // same producer/frontier would be released. No extra bytes are needed.
        demand.seek_target_ms = None;
        demand.render_state = crate::playback_control::RenderState::Rendering;
        demand.buffered_from_ms = Some(797_711);
        demand.buffered_through_ms = 809_211;
        let current = flow(&demand);
        assert_eq!(current.production_ahead_seconds, Some(18));
        assert_eq!(current.production_target_seconds, Some(16));
        assert_eq!(current.hold, None);
    }

    #[test]
    fn lifecycle_refill_empty_and_loaded_waits_are_not_answered_with_their_hold() {
        // The whole loop, joined: the freeze produces the hold, and the
        // resolver must not hand that hold back as the answer to the freeze.
        //
        // In explicit lease mode the production target is measured from the
        // client's own buffer anchor, so a player frozen at 120 s with an empty
        // buffer asks for 30 s of reserve, already has 60 s published, and is
        // held on time. Testing the resolver alone would prove half of it —
        // that a hold can be withheld — and none of the part that matters,
        // which is that this is the hold a stalled client actually meets.
        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: 2_000,
            global_max_bytes: 8_000,
        };
        let mut demand = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        demand.position_ms = 120_000;
        // An empty buffer at the frozen position: nothing behind it, nothing
        // ahead of it. This is the reading that collapses the target.
        demand.buffered_from_ms = Some(120_000);
        demand.buffered_through_ms = 120_000;
        demand.playback_rate = 1.0;
        let flow = |currently_suspended| {
            evaluate_flow(FlowInputs {
                physical_ahead: Some(Ahead {
                    seconds: 60,
                    bytes: 1_000,
                }),
                published_end_ms: Some(80_000),
                staged_publication_seconds: Some(16),
                // Pre-existing coverage: the startup grant is already spent, so
                // these assertions are about steady-state flow control.
                startup_protected: false,
                media_origin_ms: 100_000,
                lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
                demand: Some(&demand),
                global_live_bytes: 1_000,
                global_ahead_bytes: 1_000,
                limits,
                currently_suspended,
                scratch_grant_exhausted: None,
            })
        };
        let held = flow(false);
        assert_eq!(held.production_ahead_seconds, Some(16));
        assert_eq!(
            held.production_target_seconds,
            Some(16),
            "the publication clock asks for one fixed batch",
        );
        assert_eq!(
            held.hold,
            Some(AheadHold {
                reason: AheadHoldReason::Time,
                release_value: 0,
            }),
        );
        assert_eq!(
            flow(true).hold.map(|hold| hold.release_value),
            Some(0),
            "the next publication tick releases the staged-batch hold",
        );

        // The same hold, as the control plane sees it, for a client that is
        // stalled, starved, and 30 s behind a frontier it has been served.
        let wedged = crate::playback_control::DeliveryView {
            presentation: "live-recovery".to_owned(),
            producer_state: "held".to_owned(),
            produced_through_ms: Some(180_000),
            fetched_through_ms: 150_000,
            delivered_bps: Some(4_000_000),
            delivered_idle_ms: Some(20_000),
            recent_producer_speed: Some(1.0),
            client_runway_ms: 0,
            admitted: None,
            producer_decision: None,
            hold_reason: held.hold.map(|hold| {
                match hold.reason {
                    AheadHoldReason::Demand => "demand",
                    AheadHoldReason::Time => "time",
                    AheadHoldReason::Bytes => "bytes",
                    AheadHoldReason::Global => "global",
                }
                .to_owned()
            }),
            subtitle_readiness: None,
            preparation: None,
            owner_node_hash: "n-0123456789abcdef".to_owned(),
            owner_epoch: 1,
        };
        let stalled = control_request(&demand, crate::playback_control::RenderState::Stalled, true);
        assert_eq!(
            crate::playback_control::resolve_action(
                &crate::playback_control::ControlAction::None,
                &wedged,
                &stalled,
            ),
            crate::playback_control::ControlAction::None,
            "the hold the stall manufactured must not be the answer to the stall",
        );

        // The second incident shape: AVPlayer reported about twenty-two
        // seconds of contiguous loaded media while the same producer was
        // time-held. That hold is normal source pacing; it is not authority
        // over a decoder that has bytes and still presents nothing. Remove the
        // fetch gap so this assertion can pass only through the loaded-media
        // boundary rather than accidentally repeating the empty-buffer case.
        let mut loaded_demand = demand.clone();
        loaded_demand.buffered_through_ms = 142_000;
        let loaded_flow = evaluate_flow(FlowInputs {
            physical_ahead: Some(Ahead {
                seconds: 60,
                bytes: 1_000,
            }),
            published_end_ms: Some(80_000),
            staged_publication_seconds: Some(16),
            startup_protected: false,
            media_origin_ms: 100_000,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&loaded_demand),
            global_live_bytes: 1_000,
            global_ahead_bytes: 1_000,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(loaded_flow.production_target_seconds, Some(16));
        assert_eq!(
            loaded_flow.hold.map(|hold| hold.reason),
            Some(AheadHoldReason::Time),
            "the producer is correctly paced while already-loaded media waits downstream"
        );
        let mut loaded_wait = wedged.clone();
        loaded_wait.client_runway_ms = 22_000;
        loaded_wait.fetched_through_ms = loaded_wait
            .produced_through_ms
            .expect("known published frontier");
        let loaded_stall = control_request(
            &loaded_demand,
            crate::playback_control::RenderState::Stalled,
            true,
        );
        assert_eq!(
            crate::playback_control::resolve_action(
                &crate::playback_control::ControlAction::None,
                &loaded_wait,
                &loaded_stall,
            ),
            crate::playback_control::ControlAction::None,
            "a producer hold cannot own a loaded-but-unpresented wait"
        );

        // The opposite supply boundary already has a reachable exit: when the
        // published frontier is below the fixed demand target, time pacing is
        // not holding the producer. No refill credit or second policy is
        // needed to make this state progress.
        let supply_pending = evaluate_flow(FlowInputs {
            physical_ahead: Some(Ahead {
                seconds: 20,
                bytes: 1_000,
            }),
            published_end_ms: Some(20_000),
            staged_publication_seconds: None,
            startup_protected: false,
            media_origin_ms: 100_000,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            global_live_bytes: 1_000,
            global_ahead_bytes: 1_000,
            limits,
            currently_suspended: true,
            scratch_grant_exhausted: None,
        });
        assert_eq!(supply_pending.production_target_seconds, Some(16));
        assert_eq!(
            supply_pending.hold, None,
            "fixed-position supply below target must reach the existing resume operation"
        );

        // The same production hold, to a client that is playing with a full
        // buffer, is still an instruction — that is what the hold is for.
        let mut playing = wedged.clone();
        playing.client_runway_ms = 60_000;
        let rendering = control_request(
            &demand,
            crate::playback_control::RenderState::Rendering,
            false,
        );
        assert_eq!(
            crate::playback_control::resolve_action(
                &crate::playback_control::ControlAction::None,
                &playing,
                &rendering,
            ),
            crate::playback_control::ControlAction::Hold {
                reason: crate::playback_control::HoldReason::Time,
                revisit_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
            },
        );
    }
