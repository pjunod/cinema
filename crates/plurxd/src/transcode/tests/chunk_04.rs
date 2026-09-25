
    #[test]
    fn explicit_flow_uses_reported_runway_and_preserves_capacity_bounds() {
        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: 2_000,
            global_max_bytes: 8_000,
        };
        let mut demand = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        demand.position_ms = 120_000;
        demand.buffered_through_ms = 140_000;
        demand.playback_rate = 1.0;
        let physical = Ahead {
            seconds: 5,
            bytes: 1_000,
        };

        let active = evaluate_flow(FlowInputs {
            physical_ahead: Some(physical),
            produced_end_ms: Some(80_000),
            staged_publication_seconds: None,
            // Pre-existing coverage: the startup grant is already spent, so
            // these assertions are about steady-state flow control.
            startup_protected: false,
            media_origin_ms: 100_000,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 1_000,
            global_ahead_bytes: 1_000,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(active.policy, "publication_clock_explicit");
        assert_eq!(active.production_ahead_seconds, Some(60));
        assert_eq!(active.production_target_seconds, Some(16));
        assert_eq!(active.hold, None);

        demand.playback_rate = 2.0;
        let faster = evaluate_flow(FlowInputs {
            physical_ahead: Some(physical),
            produced_end_ms: Some(80_000),
            staged_publication_seconds: None,
            // Pre-existing coverage: the startup grant is already spent, so
            // these assertions are about steady-state flow control.
            startup_protected: false,
            media_origin_ms: 100_000,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 1_000,
            global_ahead_bytes: 1_000,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(
            faster.production_target_seconds,
            Some(32),
            "the publication batch scales with playback rate"
        );
        assert_eq!(faster.hold, None);

        demand.buffered_through_ms = 140_000;
        let capacity = evaluate_flow(FlowInputs {
            physical_ahead: Some(Ahead {
                seconds: 0,
                bytes: 2_001,
            }),
            produced_end_ms: Some(120_000),
            staged_publication_seconds: None,
            // Pre-existing coverage: the startup grant is already spent, so
            // these assertions are about steady-state flow control.
            startup_protected: false,
            media_origin_ms: 100_000,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 1_000,
            global_ahead_bytes: 1_000,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(
            capacity.hold.map(|hold| hold.reason),
            Some(AheadHoldReason::Bytes),
            "explicit observations cannot bypass the physical scratch bound"
        );

        let unbounded_time = evaluate_flow(FlowInputs {
            physical_ahead: Some(Ahead {
                seconds: 10_000,
                bytes: 0,
            }),
            produced_end_ms: Some(10_000_000),
            staged_publication_seconds: None,
            // Pre-existing coverage: the startup grant is already spent, so
            // these assertions are about steady-state flow control.
            startup_protected: false,
            media_origin_ms: 0,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 0,
            global_ahead_bytes: 0,
            limits: AheadLimits {
                max_secs: 0,
                max_bytes: 2_000,
                global_max_bytes: 8_000,
            },
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(unbounded_time.production_target_seconds, Some(32));
        assert_eq!(
            unbounded_time.hold, None,
            "a zero configured time limit is disabled, not a target of zero"
        );
    }

    /// The deadlock this change exists to remove.
    ///
    /// This assertion used to read the other way — a `hold` before publication
    /// held the producer at a target of zero — and that is what shipped. It
    /// cost every fallback session: the client reports `hold` because its
    /// `<video>` has never received a byte and is therefore paused, the
    /// producer stops at zero, no playlist is ever written, and the playlist
    /// request that same client is blocked on spends `PLAYLIST_WAIT_BUDGET`
    /// and returns 503. The client cannot report `active` until it plays, and
    /// it cannot play until this produces, so nothing ever broke the tie.
    #[test]
    fn a_starting_client_cannot_hold_a_session_that_has_published_nothing() {
        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: 2_000,
            global_max_bytes: 8_000,
        };
        let mut demand = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        demand.demand = crate::playback_control::PlaybackDemand::Hold;
        demand.playback_rate = 0.0;
        // What a client that has not started actually sends: the playhead is
        // the position it asked to start at, and it has buffered nothing past
        // it. `test_default` carries a 15 s runway, which would derive a target
        // above the floor all by itself and leave the floor untested.
        demand.buffered_through_ms = demand.position_ms;
        demand.buffered_from_ms = None;
        let starting = |demand: &crate::playback_control::PlaybackDemandSnapshot,
                        media_origin_ms,
                        published_end_ms| {
            evaluate_flow(FlowInputs {
                physical_ahead: None,
                produced_end_ms: published_end_ms,
                staged_publication_seconds: None,
                startup_protected: true,
                media_origin_ms,
                lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
                demand: Some(demand),
                demand_observation_age: None,
                global_live_bytes: 0,
                global_ahead_bytes: 0,
                limits,
                currently_suspended: false,
                scratch_grant_exhausted: None,
            })
        };
        let starting_with_ahead = |demand: &crate::playback_control::PlaybackDemandSnapshot,
                                   media_origin_ms,
                                   published_end_ms: i64,
                                   currently_suspended| {
            evaluate_flow(FlowInputs {
                physical_ahead: Some(Ahead {
                    seconds: published_end_ms / 1_000,
                    bytes: 0,
                }),
                produced_end_ms: Some(published_end_ms),
                staged_publication_seconds: None,
                startup_protected: true,
                media_origin_ms,
                lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
                demand: Some(demand),
                demand_observation_age: None,
                global_live_bytes: 0,
                global_ahead_bytes: 0,
                limits,
                currently_suspended,
                scratch_grant_exhausted: None,
            })
        };

        let nothing_published = starting(&demand, 0, None);
        assert_eq!(
            nothing_published.hold, None,
            "a hold from a client that has never played must not stop production"
        );
        assert_eq!(
            nothing_published.production_target_seconds, None,
            "a starting session has no time target: the limiter that would \
             enforce one releases below the floor it would be guarding"
        );

        // The incident trace: 32 seconds produced, 20 seconds fetched, no
        // presentation progress. Publication and buffering are observations,
        // not presentation evidence, so neither may create a time/demand hold.
        let mut incident_trace = demand.clone();
        incident_trace.position_ms = 0;
        incident_trace.buffered_from_ms = Some(0);
        incident_trace.buffered_through_ms = 20_000;
        let published_burst = starting_with_ahead(&incident_trace, 0, 32_000, false);
        assert_eq!(
            published_burst.hold, None,
            "32 s published / 20 s fetched / 0 ms presented remains startup-protected"
        );
        assert_eq!(published_burst.production_target_seconds, None);

        // A mid-film start is the same session with a non-zero media origin.
        // `published_end_ms` is session-relative (see `ahead_of`), so the floor
        // is 12 seconds of *this session's* media wherever in the film it
        // began — and the client's position is absolute, so the anchor and the
        // origin agree and no `Time` hold reinstates the deadlock under
        // another reason.
        let mut mid_film = demand.clone();
        mid_film.position_ms = 90_000;
        mid_film.buffered_through_ms = 90_000;
        let mid = starting(&mid_film, 90_000, None);
        assert_eq!(mid.hold, None, "a start ninety seconds in is still a start");
        assert_eq!(mid.production_target_seconds, None);

        // The same start with real published media and a real physical ahead,
        // which is the shape that can actually raise a `Time` hold. Eight
        // seconds published, ninety seconds of origin: if the anchor and the
        // origin disagreed the ahead would read as ninety-eight and the
        // limiter would fire.
        let mid_running = starting_with_ahead(&mid_film, 90_000, 8_000, false);
        assert_eq!(
            mid_running.hold, None,
            "a mid-film start awaiting presentation is still a start"
        );

        // **The quadrant that reinstated the deadlock under another name.**
        // `time_release_threshold` releases a suspended session at half its
        // target, so a starting session suspended for one tick used to come
        // back to a resume line it could never cross: stopped producers
        // publish nothing, so the ahead never falls, and a holding client's
        // anchor never advances.
        for published_ms in [2_000, 8_000, 10_000] {
            let resumed = starting_with_ahead(&demand, 0, published_ms, true);
            assert_eq!(
                resumed.hold, None,
                "a starting session suspended at {published_ms} ms must be \
                 remain runnable until presentation, not be pinned"
            );
        }

        // Byte limits are never suspended — disk is a hard bound at every
        // stage, and a starting session is not exempt from it.
        let over_disk = evaluate_flow(FlowInputs {
            physical_ahead: Some(Ahead {
                seconds: 2,
                bytes: limits.max_bytes + 1,
            }),
            produced_end_ms: Some(2_000),
            staged_publication_seconds: None,
            startup_protected: true,
            media_origin_ms: 0,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 0,
            global_ahead_bytes: 0,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(
            over_disk.hold.map(|hold| hold.reason),
            Some(AheadHoldReason::Bytes),
            "the startup grant suspends the clock, never the disk"
        );

        // Presentation is spent once. A producer retry empties the index, and
        // must not hand startup protection back.
        let retried = evaluate_flow(FlowInputs {
            physical_ahead: None,
            produced_end_ms: None,
            staged_publication_seconds: None,
            startup_protected: false,
            media_origin_ms: 0,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 0,
            global_ahead_bytes: 0,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(
            retried.hold.map(|hold| hold.reason),
            None,
            "an empty replacement keeps running until one publication batch is staged"
        );

        // The fleet-wide disk cap still answers while the index is empty. It
        // is the only limit that can, and the copy publish gate is exactly
        // when a row of paused clients could otherwise fill a full disk.
        let full_disk = evaluate_flow(FlowInputs {
            physical_ahead: None,
            produced_end_ms: None,
            staged_publication_seconds: None,
            startup_protected: true,
            media_origin_ms: 0,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 9_000,
            global_ahead_bytes: 9_000,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(
            full_disk.hold.map(|hold| hold.reason),
            Some(AheadHoldReason::Global),
            "the startup grant is not a licence to ignore the scratch ceiling"
        );

        // `End` is not a client waiting to start; it is one that has gone. It
        // suspends from any state, published or not, or a viewer who closes the
        // tab during startup leaves an encoder running to the floor.
        demand.demand = crate::playback_control::PlaybackDemand::End;
        let ended = starting(&demand, 0, None);
        assert_eq!(
            ended.hold.map(|hold| hold.reason),
            Some(AheadHoldReason::Demand)
        );
        assert_eq!(ended.production_target_seconds, Some(0));
    }

    #[test]
    fn a_seek_uses_its_target_rather_than_the_playhead_it_left() {
        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: 2_000,
            global_max_bytes: 8_000,
        };
        let mut demand = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        demand.demand = crate::playback_control::PlaybackDemand::Active;
        demand.position_ms = 10_000;
        demand.seek_target_ms = Some(100_000);
        demand.buffered_through_ms = 130_000;
        demand.playback_rate = 1.0;
        let seeking = evaluate_flow(FlowInputs {
            physical_ahead: Some(Ahead {
                seconds: 10,
                bytes: 0,
            }),
            produced_end_ms: Some(65_000),
            staged_publication_seconds: None,
            // Pre-existing coverage: the startup grant is already spent, so
            // these assertions are about steady-state flow control.
            startup_protected: false,
            media_origin_ms: 100_000,
            lease_mode: crate::playback_control::RollingLeaseMode::Explicit,
            demand: Some(&demand),
            demand_observation_age: None,
            global_live_bytes: 0,
            global_ahead_bytes: 0,
            limits,
            currently_suspended: false,
            scratch_grant_exhausted: None,
        });
        assert_eq!(seeking.production_ahead_seconds, Some(65));
        assert_eq!(seeking.production_target_seconds, Some(16));
        assert_eq!(
            seeking.hold, None,
            "physical playhead distance must not replace staged publication inventory"
        );
    }

    #[test]
    fn capacity_holds_ignore_media_time_and_release_on_byte_reserve() {
        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: 2_000,
            global_max_bytes: 8_000,
        };
        let secs = |n| Ahead {
            seconds: n,
            bytes: 0,
        };
        let bytes = |n| Ahead {
            seconds: 0,
            bytes: n,
        };
        // Media-time pacing belongs to the publication clock, not capacity.
        assert!(!should_suspend(secs(179), 0, 0, limits, false));
        assert!(!should_suspend(secs(181), 0, 0, limits, false));
        assert!(should_suspend(bytes(2_001), 0, 0, limits, false));
        // The global budget holds a session that is individually well behaved
        // — several healthy 4K streams fill a disk between them.
        assert!(should_suspend(secs(10), 8_001, 0, limits, false));

        assert!(!should_suspend(secs(10_000), 0, 0, limits, true));
        // The global byte cap enters on total scratch but releases on the
        // drainable reserve across all sessions. Retention behind the client
        // is intentionally absent from this half-cap comparison.
        assert!(should_suspend(secs(10), 8_001, 4_001, limits, true));
        assert!(!should_suspend(secs(10), 8_001, 4_000, limits, true));
        // Time is fine but bytes are not: still held.
        assert!(should_suspend(
            Ahead {
                seconds: 10,
                bytes: 1_001
            },
            0,
            0,
            limits,
            true
        ));

        // A disabled limit never suspends, whatever its number.
        let off = AheadLimits {
            max_secs: 0,
            max_bytes: 0,
            global_max_bytes: 0,
        };
        assert!(!should_suspend(secs(10_000), 1 << 40, 1 << 40, off, false));
        assert!(!should_suspend(secs(10_000), 1 << 40, 1 << 40, off, true));

        assert_eq!(ahead_hold(secs(151), 0, 0, limits, true), None);
        assert_eq!(
            ahead_hold(bytes(1_001), 0, 0, limits, true),
            Some(AheadHold {
                reason: AheadHoldReason::Bytes,
                release_value: 1_000,
            })
        );
        assert_eq!(
            ahead_hold(secs(10), 8_001, 4_001, limits, true),
            Some(AheadHold {
                reason: AheadHoldReason::Global,
                release_value: 4_000,
            })
        );
    }

    /// Normal publication discovery runs on the session clock; the 15-second
    /// global sweep is only repair. Pin the maximum normal discovery lag.
    #[test]
    fn publication_clock_is_faster_than_the_repair_backstop() {
        assert_eq!(ROLLING_PUBLICATION_POLL, Duration::from_millis(250));
        assert!(ROLLING_PUBLICATION_POLL < FLOW_CONTROL_REPAIR_INTERVAL);
    }

    /// Four retention floors can sit above the old half-cap release line even
    /// after every client has fetched through the published frontier. Those
    /// bytes are real disk usage, so they still trigger the cap while running;
    /// they are not drainable reserve, so they cannot keep a held producer
    /// stopped forever.
    #[test]
    fn retained_floors_cannot_make_global_release_unreachable() {
        let indexes: Vec<SegmentIndex> = (0..4)
            .map(|session| SegmentIndex {
                segs: (0..3)
                    .map(|segment| SegmentMeta {
                        index: segment,
                        name: format!("s{session}-seg{segment}.m4s"),
                        start_ms: segment * 4_000,
                        end_ms: (segment + 1) * 4_000,
                        bytes: 500,
                        visibility: SegmentVisibility::Advertised,
                    })
                    .collect(),
                revision: 0,
            })
            .collect();
        let global_live: i64 = indexes.iter().map(SegmentIndex::total_bytes).sum();
        let global_ahead: i64 = indexes
            .iter()
            .map(|index| ahead_of(index, 12_000).expect("published").bytes)
            .sum();
        assert_eq!(global_live, 6_000, "retention floors remain on disk");
        assert_eq!(global_ahead, 0, "every client drained its reserve");

        let limits = AheadLimits {
            max_secs: 180,
            max_bytes: 2_000,
            global_max_bytes: 8_000,
        };
        assert_eq!(
            ahead_hold(Ahead::default(), global_live, global_ahead, limits, true),
            None,
            "retained floors above the 4,000-byte release line are not a permanent hold"
        );
    }

    #[test]
    fn segment_index_parsing() {
        assert_eq!(segment_index("seg00000.ts"), Some(0));
        assert_eq!(segment_index("seg00042.m4s"), Some(42));
        assert_eq!(segment_index("seg12345.ts"), Some(12345));
        assert_eq!(segment_index("init.mp4"), None);
        assert_eq!(segment_index("index.m3u8"), None);
        assert_eq!(segment_index("seg.ts"), None);
    }

    #[test]
    fn a_pruned_segment_never_waits_for_a_producer_that_cannot_restore_it() {
        assert!(!SegmentVisibility::Grace {
            removed_at: Instant::now(),
            serve_until: Instant::now(),
        }
        .is_servable(Instant::now()));
    }

    #[test]
    fn an_ended_published_producer_fails_only_requests_beyond_its_frontier() {
        assert!(!producer_request_beyond_frontier(Some(41), Some(42)));
        assert!(!producer_request_beyond_frontier(Some(42), Some(42)));
        assert!(producer_request_beyond_frontier(Some(43), Some(42)));
        assert!(producer_request_beyond_frontier(Some(0), None));
        assert!(producer_request_beyond_frontier(None, Some(42)));
    }

    /// The legacy single-ffmpeg copy gives up preservation, not just
    /// conversion, and says so in every field a client reads.
    ///
    /// This is the path a converting title takes the first time it is played,
    /// before its third fragment index exists — so it is not an exotic
    /// fallback, it is the normal first watch. Three fields have to agree that
    /// the stream is the HDR10 base: the argv's bitstream filter (through
    /// `preserve_dolby_vision`), the playlist (through `convert_dolby_vision`)
    /// and the create response's badge (through the `SessionKind` the caller
    /// returns). Any one of them left at the decision's answer is a lie to a
    /// different consumer, and the viewer only ever sees the last.
    #[test]
    fn a_copy_that_cannot_convert_serves_and_describes_the_hdr10_base() {
        let asked = CopySessionOptions {
            transcode_audio: false,
            preserve_dolby_vision: true,
            convert_dolby_vision: true,
        };
        let served = served_copy_options(&profile7_file(), asked);
        assert!(
            !served.preserve_dolby_vision,
            "keeping the RPUs on a path that cannot rewrite them hands dual-layer \
             Profile 7 to a client that said it decodes 8 and not 7"
        );
        assert!(
            !served.convert_dolby_vision,
            "and claiming the conversion advertises dvh1.08 over media that is \
             plain HDR10"
        );
        assert!(
            served.transcode_audio == asked.transcode_audio,
            "audio is unrelated"
        );

        // Every other session is untouched: a preserving copy of a source this
        // path can actually hand over still preserves, which is the case that
        // would break if the strip-down were unconditional.
        let preserving = CopySessionOptions {
            convert_dolby_vision: false,
            ..asked
        };
        assert!(
            served_copy_options(&profile5_file(), preserving).preserve_dolby_vision,
            "a client that claimed the source's own profile still gets it"
        );
    }

    /// The guard cannot be keyed on the conversion flag alone, because the
    /// state that produced the black screen is the one where that flag is
    /// already `false`.
    ///
    /// `preserve && !convert` on a dual-layer source is what every way of
    /// splitting the pair arrives at: a session built by an older node during
    /// a rolling upgrade, an operator who turned the conversion off, a row
    /// whose Dolby Vision columns are missing so `file_can_convert_to_p81`
    /// refuses it. On this path there is no RPU rewrite, so preserving means
    /// the source's own Profile 7 record, its RPUs and its type-63
    /// enhancement layer, handed over intact — the delivery Safari answered
    /// with `MEDIA_ERR_DECODE`. The file is the fact this path can never be
    /// wrong about, so the file is what it asks.
    #[test]
    fn a_copy_never_preserves_dual_layer_dolby_vision_it_cannot_convert() {
        let asked = CopySessionOptions {
            transcode_audio: false,
            preserve_dolby_vision: true,
            convert_dolby_vision: false,
        };
        assert!(
            !served_copy_options(&profile7_file(), asked).preserve_dolby_vision,
            "a conversion nobody asked for is exactly the case that reaches a \
             browser as dual-layer Profile 7"
        );

        // A row scanned before the Dolby Vision columns existed answers from
        // its label, and answers the conservative way: it cannot convert
        // either, so preserving it would be the same undecodable delivery.
        let mut label_only = profile7_file();
        label_only.dolby_vision = Default::default();
        assert_eq!(
            label_only.hdr_format.as_deref(),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
            "the fallback this asserts about is the label"
        );
        assert!(
            !served_copy_options(&label_only, asked).preserve_dolby_vision,
            "a label-only Profile 7 row can neither convert nor preserve"
        );

        // And nothing else moves: a source with no dual layer to refuse is
        // copied exactly as before.
        let mut hdr10 = profile5_file();
        hdr10.hdr = Some("hdr10".into());
        hdr10.hdr_format = Some("HDR10".into());
        assert!(
            served_copy_options(&hdr10, asked).preserve_dolby_vision,
            "the guard is about dual-layer Dolby Vision, not about copies"
        );
    }

    fn profile7_file() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            id: 7,
            path: PathBuf::from("/media/profile7.mkv"),
            hdr_format: Some("Dolby Vision · Profile 7 (HDR10-compatible)".into()),
            dolby_vision: plurx_core::domain::DolbyVisionFacts {
                profile: Some(7),
                level: Some(9),
                bl_compat_id: Some(1),
                ..Default::default()
            },
            ..profile5_file()
        }
    }

    /// And the call site, which the function above cannot speak for.
    ///
    /// `served_copy_options` still pins legacy/takeover narrowing. A fresh
    /// GOP-aware session has the post-mux converter, though, so this reads the
    /// argv the real process is given and proves that the RPU survives for the
    /// in-process stage while the enhancement layer does not.
    ///
    /// It asserts the invariant rather than the complete command: type 62
    /// reaches the converter, type 63 does not, and movenc is allowed to carry
    /// the source Dolby Vision side data until plurx replaces its record.
    #[tokio::test]
    async fn a_fresh_copy_conversion_preserves_the_rpu_argv_it_spawns() {
        use plurx_core::store::SqliteStore;
        use tracing_subscriber::prelude::*;

        super::require_ffmpeg();
        let media = crate::test_tempdir().expect("media dir");
        let src = media.path().join("profile7.mp4");
        write_real_hevc_video(&src, 4).await;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file_with_probe_at(
            &store,
            &src.to_string_lossy(),
            plurx_core::domain::ProbeResult {
                duration_ms: Some(4_000),
                container: Some("mp4".into()),
                video_codec: Some("hevc".into()),
                width: Some(160),
                height: Some(120),
                bit_depth: Some(10),
                hdr: Some("dolby_vision".into()),
                hdr_format: Some("Dolby Vision · Profile 7 (HDR10-compatible)".into()),
                ..Default::default()
            },
        )
        .await;

        let work = crate::test_tempdir().expect("work");
        let mgr = Arc::new(
            TranscodeManager::new(
                Arc::clone(&store),
                work.path().to_path_buf(),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            // Pinned rather than probed: the two strip shapes differ only in
            // whether this build has the `dovi_rpu` bitstream filter, and a
            // test whose argv depends on the host's ffmpeg asserts something
            // different on every machine.
            .with_dv_strippable(false),
        );

        // A ring, and the argv line is the *oldest* thing in it: `start_copy`
        // spawns a real ffmpeg whose stderr is logged a line at a time, so a
        // capacity anywhere near the number of events would evict the one
        // entry this test exists to read and fail on the `expect` below —
        // green mutation, red truth. Sized for a noisy encoder rather than
        // for the handful of lines the happy path emits.
        let logs = Arc::new(crate::logbuf::LogBuffer::new(8192));
        let subscriber =
            tracing_subscriber::registry().with(crate::logbuf::BufferLayer(Arc::clone(&logs)));
        // Through the crate helper rather than `set_default` directly: the
        // argv callsite is one every copy test reaches, and a test on another
        // thread reaching it first while this thread is the only registered
        // dispatcher caches it as never-interesting — see the helper.
        let guard = crate::test_tracing_default(subscriber);
        let started = mgr
            .start_copy(
                file_id,
                0.0,
                None,
                // What live-HLS recovery builds from a converting session's
                // `SessionKind::Copy`: `start_live_recovery_session` copies
                // both flags straight off `req.kind`.
                CopySessionOptions {
                    transcode_audio: false,
                    preserve_dolby_vision: true,
                    convert_dolby_vision: true,
                },
                "paul",
                "pb-strip",
            )
            .await;

        // The argv is logged by the producer task, not by the call that
        // returns the session, so `start_copy` completing is not the moment
        // the line exists. `set_default` is thread-local and this is a
        // current-thread runtime, so the wait is also what lets that task be
        // polled at all — a bare read here passes on an idle machine and
        // fails under a loaded one, which is the flake this loop removes.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let argv = loop {
            if let Some(line) = logs
                .tail("trace", 8192)
                .into_iter()
                .map(|entry| entry.message)
                .find(|message| message.contains("copy-video HLS ffmpeg args"))
            {
                break line;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the copy path logs the argv it is about to spawn"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        drop(guard);

        assert!(
            argv.contains("-strict unofficial"),
            "a converting copy must let ffmpeg retain Dolby Vision side data: {argv}"
        );
        assert!(
            !argv.contains("62-63"),
            "the RPU (62) has to reach the in-process converter: {argv}"
        );
        assert!(
            argv.contains("-tag:v hvc1"),
            "the served stream is the HDR10 base, so the sample entry is the \
             compatible one: {argv}"
        );
        assert!(
            argv.contains("remove_types=32-34|63"),
            "the converting recipe keeps the RPU and drops the enhancement layer: {argv}"
        );

        let info = started.expect("the copy session starts");
        assert!(
            matches!(
                info.kind,
                SessionKind::Copy {
                    preserve_dolby_vision: true,
                    convert_dolby_vision: true,
                    ..
                }
            ),
            "the badge the create response is computed from has to describe the \
             same stream the argv produces: {:?}",
            info.kind
        );
    }

    /// The playlist for a converting session describes what the conversion
    /// produces, not what the source is.
    ///
    /// The source is Profile 7, so every other arm of `copied_hls_codecs`
    /// would read the source's ffprobe record and answer `dvh1.07.LL` — the
    /// one profile this client's caps said it cannot decode — over media whose
    /// sample entry is `hvc1` and whose configuration record plurx wrote as
    /// profile 8. Two lies at once, to the only client class that ever reaches
    /// this branch.
    #[tokio::test]
    async fn a_converting_session_advertises_the_profile_it_produces() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let mut file = store
            .get_file(file_id)
            .await
            .expect("read file")
            .expect("seeded file");
        file.hdr = Some("dolby_vision".into());
        file.audio_streams = Vec::new();
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(9);
        file.dolby_vision.bl_compat_id = Some(1);

        // The source's own record, which is what the other arms would read.
        let probe = r#"{"streams":[{"codec_type":"video","profile":"Main 10","level":150,"side_data_list":[{"side_data_type":"DOVI configuration record","dv_profile":7,"dv_level":9,"dv_bl_signal_compatibility_id":1}]}]}"#;

        let (codecs, supplemental) = copied_hls_codecs(
            &file,
            None,
            CopySessionOptions {
                transcode_audio: false,
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
            },
            Some(probe),
        );
        assert!(
            codecs.starts_with("hvc1"),
            "8.1 is a backward-compatible enhancement of HDR10, so the base \
             sample entry stays hvc1: {codecs}"
        );
        assert!(
            !codecs.contains("dvh1"),
            "the primary codec must not name a Dolby Vision profile at all: {codecs}"
        );
        assert_eq!(
            supplemental.as_deref(),
            Some("dvh1.08.09/db1p"),
            "the level is the source's, the profile is what the conversion made, \
             and db1p is the HDR10-compatible base"
        );

        // The same file with the conversion off is the contrast: the
        // source's record wins and the playlist says 7.
        let (codecs, supplemental) = copied_hls_codecs(
            &file,
            None,
            CopySessionOptions {
                transcode_audio: false,
                preserve_dolby_vision: true,
                convert_dolby_vision: false,
            },
            Some(probe),
        );
        assert!(codecs.contains("dvh1.07"), "{codecs}");
        assert_eq!(supplemental, None);
    }

    #[tokio::test]
    async fn hls_codec_metadata_matches_copy_and_audio_conversion() {
        use plurx_core::domain::AudioStream;
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let mut file = store
            .get_file(file_id)
            .await
            .expect("read file")
            .expect("seeded file");
        file.hdr = Some("dolby_vision".into());
        file.audio_streams = vec![AudioStream {
            index: 0,
            codec: "eac3".into(),
            channels: Some(6),
            sample_rate: Some(48_000),
            language: Some("eng".into()),
            title: None,
            default: true,
        }];

        assert_eq!(
            copied_hls_codecs(
                &file,
                None,
                CopySessionOptions {
                    convert_dolby_vision: false,
                    transcode_audio: false,
                    preserve_dolby_vision: true,
                },
                Some(
                    r#"{"streams":[{"codec_type":"video","profile":"Main 10","level":150,"side_data_list":[{"side_data_type":"DOVI configuration record","dv_profile":8,"dv_level":6,"dv_bl_signal_compatibility_id":1}]}]}"#
                ),
            ),
            (
                "hvc1.2.4.L150.B0,ec-3".to_owned(),
                Some("dvh1.08.06/db1p".to_owned())
            )
        );
        assert_eq!(
            copied_hls_codecs(
                &file,
                None,
                CopySessionOptions {
                    convert_dolby_vision: false,
                    transcode_audio: true,
                    preserve_dolby_vision: false,
                },
                None,
            ),
            ("hvc1,mp4a.40.2".to_owned(), None)
        );

        file.hdr = None;
        file.video_codec = Some("h264".into());
        assert_eq!(
            copied_hls_codecs(
                &file,
                None,
                CopySessionOptions {
                    convert_dolby_vision: false,
                    transcode_audio: true,
                    preserve_dolby_vision: false,
                },
                Some(r#"{"streams":[{"codec_type":"video","profile":"High","level":50}]}"#,),
            ),
            ("avc1.640032,mp4a.40.2".to_owned(), None)
        );
    }

    #[tokio::test]
    async fn segment_delivery_counts_reads_and_names_incomplete_storage() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let dir = crate::test_tempdir().expect("segment-delivery directory");
        let mut raw_session = test_session(dir.path().to_path_buf());
        raw_session.file_id = file_id;
        let session = Arc::new(raw_session);
        let mut delivery = SegmentDelivery::new(
            SegmentDeliveryContext {
                store: Arc::clone(&store),
                session: Arc::clone(&session),
                producer_attempt: session.control.current_producer_attempt(),
                session_id: "delivery-test".to_owned(),
                segment: "seg00001.m4s".to_owned(),
                segment_start_ms: Some(2_000),
                segment_duration_ms: Some(2_000),
                encoder: "test".to_owned(),
            },
            1_024,
            None,
        );

        delivery.note_read(512, Duration::from_millis(300));
        delivery.note_read(128, Duration::from_millis(350));
        delivery.finish();

        assert_eq!(session.delivery.total_bytes(), 640);
        let events = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let events = store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: None,
                        limit: 20,
                    })
                    .await
                    .expect("segment-delivery telemetry query");
                if events.len() >= 2 {
                    return events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("segment-delivery telemetry persisted");

        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "segment_delivery_wait")
                .count(),
            1,
            "only the first slow storage read is reported"
        );
        let incomplete = events
            .iter()
            .find(|event| event.event == "segment_delivery_incomplete")
            .expect("incomplete delivery event");
        assert_eq!(incomplete.reason.as_deref(), Some("storage_unexpected_eof"));
        assert!(incomplete
            .extra
            .as_deref()
            .is_some_and(|extra| extra.contains("\"delivered_bytes\":640")));
        assert!(incomplete.extra.as_deref().is_some_and(|extra| {
            extra.contains("\"segment_start_ms\":2000")
                && extra.contains("\"segment_duration_ms\":2000")
                && extra.contains("\"cut_class\":\"source_eof\"")
                && extra.contains("\"producer_superseded\":false")
        }));
        assert!(
            incomplete
                .extra
                .as_deref()
                .is_some_and(|extra| extra.contains("\"purpose\":\"client_response\"")),
            "every delivery event names who was reading"
        );
    }

    /// The observable is the emitted warning, not the predicate.
    ///
    /// `segment_storage_stall_signal_is_normalized_by_read_size` below pins
    /// `storage_read_is_slow` itself, which is necessary but not sufficient:
    /// it passes with the guard in `SegmentDelivery::note_storage_read` left
    /// at the old duration-only test, because it never calls it. This one
    /// drives the guard. Both reads sit exactly on the 250 ms event boundary,
    /// so duration alone cannot tell them apart; only the normalized rate
    /// can. The discriminator is *which* read is named: a duration-only guard
    /// reports the first (healthy 1 MiB/s) read and then latches
    /// `slow_read_reported`, so the single event carries
    /// `delivered_bytes: 262144` instead of the 266240 that means "the 4 KiB
    /// read at 16 KiB/s is the one that stalled".
    #[tokio::test]
    async fn a_large_read_at_the_event_boundary_emits_no_storage_stall_warning() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let dir = crate::test_tempdir().expect("segment-stall directory");
        let mut raw_session = test_session(dir.path().to_path_buf());
        raw_session.file_id = file_id;
        let session = Arc::new(raw_session);
        let mut delivery = SegmentDelivery::new(
            SegmentDeliveryContext {
                store: Arc::clone(&store),
                session: Arc::clone(&session),
                producer_attempt: session.control.current_producer_attempt(),
                session_id: "stall-warning-test".to_owned(),
                segment: "seg00001.m4s".to_owned(),
                segment_start_ms: Some(2_000),
                segment_duration_ms: Some(2_000),
                encoder: "test".to_owned(),
            },
            266_240,
            None,
        );
        let boundary = Duration::from_millis(250);

        // 256 KiB in 250 ms is 1 MiB/s: an ordinary NAS read, not a stall.
        delivery.note_read(256 * 1024, boundary);
        // 4 KiB in the same 250 ms is 16 KiB/s: that is a stall.
        delivery.note_read(4 * 1024, boundary);

        let events = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: None,
                        limit: 20,
                    })
                    .await
                    .expect("segment-stall telemetry query");
                if events
                    .iter()
                    .any(|event| event.event == "segment_delivery_wait")
                {
                    return events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the slow read is reported");

        let waits: Vec<_> = events
            .iter()
            .filter(|event| event.event == "segment_delivery_wait")
            .collect();
        assert_eq!(waits.len(), 1, "one storage stall warning per body");
        assert_eq!(waits[0].reason.as_deref(), Some("storage_read_slow"));
        assert!(
            waits[0]
                .extra
                .as_deref()
                .is_some_and(|extra| extra.contains("\"delivered_bytes\":266240")),
            "the warning names the 4 KiB read, not the healthy 256 KiB one: {:?}",
            waits[0].extra
        );
        assert_eq!(session.delivery.total_bytes(), 266_240);
    }

    #[test]
    fn segment_storage_stall_signal_is_normalized_by_read_size() {
        let boundary = Duration::from_millis(250);

        assert!(
            !storage_read_is_slow(256 * 1024, boundary),
            "a 1 MiB/s body read is healthy even at the event duration boundary"
        );
        assert!(
            storage_read_is_slow(4 * 1024, boundary),
            "the same duration at 16 KiB/s is a storage stall"
        );
    }

    #[tokio::test]
    async fn segment_delivery_drop_names_same_session_producer_replacement() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let dir = crate::test_tempdir().expect("segment-delivery replacement directory");
        let mut raw_session = test_session(dir.path().to_path_buf());
        raw_session.file_id = file_id;
        let session = Arc::new(raw_session);
        let delivery = SegmentDelivery::new(
            SegmentDeliveryContext {
                store: Arc::clone(&store),
                session: Arc::clone(&session),
                producer_attempt: session.control.current_producer_attempt(),
                session_id: "delivery-replaced".to_owned(),
                segment: "seg00002.m4s".to_owned(),
                segment_start_ms: Some(4_000),
                segment_duration_ms: Some(2_000),
                encoder: "test".to_owned(),
            },
            1_024,
            None,
        );

        session.replacing_child.store(true, Release);
        drop(delivery);

        let event = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(event) = store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: None,
                        limit: 20,
                    })
                    .await
                    .expect("segment-delivery telemetry query")
                    .into_iter()
                    .find(|event| event.reason.as_deref() == Some("response_dropped"))
                {
                    return event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("replacement telemetry persisted");
        let extra = event.extra.as_deref().unwrap_or_default();
        assert!(extra.contains("\"producer_superseded\":true"));
        assert!(extra.contains("\"cut_class\":\"producer_superseded\""));
        assert!(extra.contains("\"client_disposition\":\"unknown\""));
    }

    fn reopen_request(
        file_id: i64,
        playback_id: &str,
        request_id: &str,
        previous_session_id: &str,
    ) -> SessionRequest {
        SessionRequest {
            control_sequence: None,
            file_id,
            playback_id: playback_id.into(),
            request_id: Some(request_id.into()),
            automatic: true,
            previous_session_id: Some(previous_session_id.into()),
            reopen_reason: Some(ReopenReason::Stall),
            // The handler's first Auto answer is intentionally not authority
            // for a stall reopen; claim normalization replaces this from the
            // named predecessor's already-resolved height.
            kind: SessionKind::Transcode { height: 1080 },
            start_seconds: 12.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        }
    }

    struct ReopenFixture<'a> {
        session_id: &'a str,
        dir: PathBuf,
        user_name: &'a str,
        playback_id: &'a str,
        file_id: i64,
        target_height: i64,
        automatic: bool,
        kind: SessionKind,
    }

    async fn insert_reopen_fixture(
        manager: &TranscodeManager,
        fixture: ReopenFixture<'_>,
    ) -> Arc<Session> {
        let mut session = test_session(fixture.dir);
        session.user_name = fixture.user_name.into();
        session.playback_id = fixture.playback_id.into();
        session.file_id = fixture.file_id;
        session.target_height = fixture.target_height;
        session.automatic = fixture.automatic;
        session.kind = fixture.kind;
        session.method = match fixture.kind {
            SessionKind::Transcode { .. } => crate::delivery::Method::Transcode,
            SessionKind::Copy { .. } => crate::delivery::Method::HlsCopy,
        };
        let session = Arc::new(session);
        manager
            .sessions
            .lock()
            .await
            .insert(fixture.session_id.into(), Arc::clone(&session));
        session
    }

    /// Build a session directory with a real playlist and real files, so the
    /// index, the retention window and the pruner are all exercised against
    /// what ffmpeg actually writes.
    async fn seeded_session_dir(dir: &std::path::Path, count: i64, secs_each: f64) {
        let mut playlist = String::from(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-PLAYLIST-TYPE:EVENT\n",
        );
        for i in 0..count {
            playlist.push_str(&format!("#EXTINF:{secs_each:.3},\nseg{i:05}.ts\n"));
            tokio::fs::write(dir.join(format!("seg{i:05}.ts")), vec![b'x'; 1024])
                .await
                .expect("write seg");
        }
        tokio::fs::write(dir.join("index.m3u8"), playlist)
            .await
            .expect("write playlist");
    }

    async fn expire_seeded_prefix(session: &Session, count: usize) {
        let removed_at = Instant::now() - Duration::from_secs(2);
        let serve_until = Instant::now() - Duration::from_secs(1);
        let mut segments = session.segments.lock().await;
        for segment in segments.segs.iter_mut().take(count) {
            segment.visibility = SegmentVisibility::Grace {
                removed_at,
                serve_until,
            };
        }
    }

    async fn install_seeded_served_playlist_snapshot(
        session: &Session,
        producer_attempt: u64,
    ) -> (i64, i64) {
        let raw = tokio::fs::read(session.dir.join("index.m3u8"))
            .await
            .expect("seeded playlist");
        let index = SegmentIndex {
            segs: parse_playlist(&String::from_utf8_lossy(&raw)),
            revision: 0,
        };
        let first_segment = index.segs.first().expect("first segment").index;
        let last_segment = index.segs.last().expect("last segment").index;
        let end_ms = index
            .produced_playable_end_ms()
            .expect("seeded playable end");
        let raw = served_live_playlist(
            raw,
            Some(first_segment),
            Some(last_segment),
            session.takeover.as_ref(),
        )
        .expect("seeded served playlist");
        session.publication.lock().await.served = Some(ServedPlaylistSnapshot {
            raw: Arc::from(raw),
            producer_attempt,
            revision: 1,
            last_segment,
            first_segment,
            end_ms,
            duration_ms: end_ms,
            end_list: false,
            available_at: Instant::now(),
        });
        (last_segment, end_ms)
    }

    async fn install_seeded_served_playlist(session: &Session, producer_attempt: u64) {
        let (last_segment, end_ms) =
            install_seeded_served_playlist_snapshot(session, producer_attempt).await;
        assert!(
            session
                .control
                .observe_publication(crate::playback_control::RollingPublicationObservation {
                    producer_attempt,
                    publication_commit: true,
                    demand_sequence: None,
                    produced_segment: Some(last_segment),
                    produced_end_ms: Some(end_ms),
                    playlist_ready: true,
                    published_segment: Some(last_segment),
                    published_end_ms: Some(end_ms),
                    published_first_segment: Some(0),
                    published_start_ms: Some(0),
                    media_origin_ms: 0,
                    next_media_sequence: last_segment.saturating_add(1),
                    resolved_fetched_segment: None,
                    resolved_fetched_end_ms: None,
                })
                .await,
            "seeded publication must reach the control actor"
        );
    }

    async fn complete_seeded_replacement(
        session: Arc<Session>,
        dir: PathBuf,
        count: i64,
        secs_each: f64,
        first_segment: Option<&'static [u8]>,
    ) -> u64 {
        let (replacement, attempt) = session
            .kill_child_for_replacement()
            .await
            .expect("prepublication replacement");
        clear_session_dir(&dir).await.expect("clear predecessor");
        session.confirm_predecessor_scratch_cleared();
        seeded_session_dir(&dir, count, secs_each).await;
        if let Some(bytes) = first_segment {
            tokio::fs::write(dir.join("seg00000.ts"), bytes)
                .await
                .expect("write successor segment");
        }
        *session.child.lock().await = Some(AttemptChild::new(
            session.control.current_producer_attempt(),
            long_running_child(),
            session.control.clone(),
            None,
        ));
        replacement.complete();
        session.refresh_segments().await;
        install_seeded_served_playlist(&session, attempt).await;
        attempt
    }

    /// The manager must hold the first live response below the rolling
    /// publication runway, then release it as soon as the configured 48-second
    /// cushion exists. This pins both the asynchronous polling behavior and
    /// the per-session one-way publication state.
    #[tokio::test]
    async fn first_live_transcode_playlist_waits_for_two_segments() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 11, 4.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("startup-gate".into(), Arc::clone(&session));

        let held =
            tokio::time::timeout(Duration::from_millis(250), mgr.playlist("startup-gate")).await;
        assert!(
            held.is_err(),
            "a playlist below the 48-second runway must not escape the startup gate"
        );
        assert!(!session.playlist_published.load(Relaxed));

        // The legacy publication clock advances while the request waits, so
        // one complete segment beyond the fixed 48-second runway is required
        // to cover that elapsed wall time without widening the request bound.
        seeded_session_dir(dir.path(), 13, 4.0).await;
        session
            .publication_cycle("startup-gate")
            .await
            .expect("publish complete startup runway plus elapsed time");
        assert!(!session.failed.load(Relaxed), "startup fixture retired");
        assert!(
            session.publication.lock().await.served.is_some(),
            "complete runway must install a served snapshot"
        );
        let playlist = tokio::time::timeout(Duration::from_secs(2), mgr.playlist("startup-gate"))
            .await
            .expect("48-second playlist should be released")
            .expect("playlist");
        let text = String::from_utf8(playlist).expect("utf8 playlist");
        assert!(text.contains("seg00000.ts"));
        assert!(text.contains("seg00011.ts"));
        assert!(
            !session.playlist_published.load(Relaxed),
            "the read-only fixture does not impersonate response authorization"
        );
        let actor_delivery = session
            .control
            .snapshot()
            .await
            .expect("rolling actor")
            .delivery;
        assert!(actor_delivery.playlist_ready);
        assert_eq!(actor_delivery.published_segment, Some(11));
        assert_eq!(actor_delivery.published_end_ms, Some(48_000));
        assert_eq!(actor_delivery.next_media_sequence, 12);
        let status = mgr.session_status("startup-gate").await.expect("status");
        assert_eq!(status.producer_attempt, Some(0));
        assert_eq!(status.playlist_ready, Some(true));
        assert_eq!(status.published_segment, Some(11));
        assert_eq!(status.published_end_ms, Some(48_000));
        assert_eq!(status.next_media_sequence, Some(12));
        assert_eq!(status.pending_fetched_segment, None);
    }

    #[tokio::test]
    async fn waiting_playlist_rebinds_to_a_prepublication_successor_attempt() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.playlist_wait_override_ms.store(5_000, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("attempt-handoff".into(), Arc::clone(&session));

        let playlist = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.playlist("attempt-handoff").await }
        });
        tokio::time::sleep(PLAYLIST_WAIT_POLL * 2).await;
        assert!(
            !playlist.is_finished(),
            "request is waiting on attempt zero"
        );

        let successor = session
            .control
            .begin_producer_attempt()
            .await
            .expect("prepublication successor");
        session.reset_compatibility_delivery(successor).await;
        seeded_session_dir(dir.path(), 12, 4.0).await;

        let bytes = tokio::time::timeout(Duration::from_secs(2), playlist)
            .await
            .expect("successor playlist deadline")
            .expect("playlist task")
            .expect("the original request follows the successor");
        assert!(String::from_utf8(bytes)
            .expect("playlist text")
            .contains("seg00011.ts"));
        assert_eq!(session.control.current_producer_attempt(), successor);
    }

    #[tokio::test]
    async fn predecessor_playlist_cannot_open_the_successor_startup_gate() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let manager_dir = crate::test_tempdir().expect("manager tempdir");
        seeded_session_dir(dir.path(), 12, 4.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        session.publication_worker_started.store(true, Release);
        install_seeded_served_playlist_snapshot(&session, 0).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .playlist_publication_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            manager_dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.playlist_wait_override_ms.store(5_000, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("publication-handoff".into(), Arc::clone(&session));

        let playlist = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.playlist("publication-handoff").await }
        });
        pause.wait().await;

        session.replacing_child.store(true, Release);
        let successor = session
            .control
            .begin_producer_attempt()
            .await
            .expect("prepublication successor");
        session.reset_compatibility_delivery(successor).await;
        pause.wait().await;
        clear_session_dir(dir.path())
            .await
            .expect("clear predecessor");
        session.confirm_predecessor_scratch_cleared();
        seeded_session_dir(dir.path(), 11, 4.0).await;
        session.replacing_child.store(false, Release);

        tokio::time::sleep(PLAYLIST_WAIT_POLL * 2).await;
        assert!(
            !playlist.is_finished(),
            "a successor below the runway must not inherit the predecessor's open gate"
        );
        assert_eq!(
            session.compatibility_playlist_published(successor),
            Some(false)
        );

        seeded_session_dir(dir.path(), 12, 4.0).await;
        install_seeded_served_playlist(&session, successor).await;
        let bytes = tokio::time::timeout(Duration::from_secs(2), playlist)
            .await
            .expect("successor cushion deadline")
            .expect("playlist task")
            .expect("successor playlist");
        assert!(String::from_utf8(bytes)
            .expect("playlist text")
            .contains("seg00011.ts"));
        let delivery = session
            .control
            .snapshot()
            .await
            .expect("rolling actor")
            .delivery;
        assert!(delivery.playlist_ready);
        assert_eq!(delivery.published_segment, Some(11));
        assert_eq!(delivery.published_end_ms, Some(48_000));
        assert_eq!(
            session.compatibility_playlist_published(successor),
            Some(false),
            "the read-only fixture does not impersonate response authorization"
        );
    }

    #[tokio::test]
    async fn returned_playlist_bytes_publish_actor_readiness_without_a_second_read() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 12, 4.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        session.publication_worker_started.store(true, Release);
        install_seeded_served_playlist(&session, 0).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .playlist_publication_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("exact-publication".into(), Arc::clone(&session));

        let playlist = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.playlist("exact-publication").await }
        });
        pause.wait().await;
        tokio::fs::remove_file(dir.path().join("index.m3u8"))
            .await
            .expect("force the later refresh read to fail");
        pause.wait().await;

        let bytes = playlist
            .await
            .expect("playlist task")
            .expect("the already-read exact playlist remains servable");
        assert!(String::from_utf8(bytes)
            .expect("playlist text")
            .contains("seg00011.ts"));
        let delivery = session
            .control
            .snapshot()
            .await
            .expect("rolling actor")
            .delivery;
        assert!(delivery.playlist_ready);
        assert_eq!(delivery.published_segment, Some(11));
        assert_eq!(delivery.published_end_ms, Some(48_000));
        assert_eq!(
            session.kill_child_for_replacement().await.err(),
            Some(crate::playback_control::ProducerAttemptRejection::PlaylistPublished),
            "bytes returned to a client permanently close in-place fallback"
        );
    }

    /// **The #263 regression.** A playlist request must outlive the server's
    /// own startup recovery, because a session inside that recovery is one the
    /// server is still successfully starting.
    ///
    /// A stalled hardware start is given the actor hardware budget before it is
    /// retried and then the actor software budget for the fallback to
    /// produce real output. The wait used to be an independently chosen 30 s,
    /// sized against the copy path's publish gate and never against this — so
    /// at 30 s the request 404'd, hls.js escalated it to a fatal
    /// `levelLoadError`, and the viewer was told the stream had permanently
    /// failed with twelve seconds of legitimate recovery still to run.
    ///
    /// Reverting the budget to that 30 s fails this assertion.
    #[test]
    fn the_playlist_wait_outlives_the_startup_recovery_it_has_to_cover() {
        let recovery = ACTOR_HARDWARE_STARTUP_BUDGET + ACTOR_SOFTWARE_STARTUP_BUDGET;
        assert!(
            PLAYLIST_WAIT_BUDGET > recovery,
            "a playlist request may not give up ({PLAYLIST_WAIT_BUDGET:?}) while the \
             session is still inside its own hardware->software recovery ({recovery:?})"
        );
        assert_eq!(
            PLAYLIST_WAIT_BUDGET,
            recovery + PLAYLIST_WAIT_SLACK,
            "HTTP patience must be derived only from both actor budgets plus named handoff slack"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn playlist_reclassification_reuses_one_absolute_deadline() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("playlist deadline dir");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.playlist_wait_override_ms.store(25, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("shared-deadline".into(), session);

        let deadline = mgr.playlist_request_deadline();
        assert!(matches!(
            mgr.playlist_with_owner_before("shared-deadline", deadline)
                .await
                .map_err(|error| error.error),
            Err(PlaylistError::StartupTimedOut(_))
        ));
        let after_first = tokio::time::Instant::now();
        assert!(matches!(
            mgr.playlist_with_owner_before("shared-deadline", deadline)
                .await
                .map_err(|error| error.error),
            Err(PlaylistError::StartupTimedOut(_))
        ));
        assert_eq!(
            tokio::time::Instant::now(),
            after_first,
            "reclassification must not mint another playlist wait budget"
        );
    }

    #[tokio::test]
    async fn playlist_deadline_bounds_work_after_exact_bytes_are_observed() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("playlist bounded work dir");
        seeded_session_dir(dir.path(), 12, 4.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.playlist_wait_override_ms.store(40, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("bounded-ready-playlist".into(), Arc::clone(&session));

        // Exact bytes and actor observation are ready, but the response still
        // needs the retained-window projection. Holding that lock models any
        // post-read storage/catalog stall: it must share the same deadline,
        // not extend the request indefinitely after useful bytes exist.
        let segments = session.segments.lock().await;
        let budget = Duration::from_millis(40);
        let started = Instant::now();
        let refused = mgr
            .playlist_with_owner_before("bounded-ready-playlist", mgr.playlist_request_deadline())
            .await
            .map_err(|error| error.error);
        assert!(matches!(
            refused,
            Err(PlaylistError::StartupTimedOut(waited))
                if waited == mgr.playlist_wait()
        ));
        assert!(
            started.elapsed() >= budget && started.elapsed() < Duration::from_secs(1),
            "the outer absolute deadline must own every await after the playlist read"
        );
        drop(segments);
    }

    #[tokio::test]
    async fn rejected_rolling_snapshots_obey_the_request_loop_deadline() {
        use plurx_core::store::SqliteStore;

        for missing_boundary in [false, true] {
            let dir = crate::test_tempdir().expect("rejected snapshot dir");
            seeded_session_dir(dir.path(), 2, 2.0).await;
            let session = Arc::new(test_session(dir.path().to_path_buf()));
            // Model an already-published live producer whose rewrite remains
            // inconsistent for this whole request. Keep the fixture's retained
            // catalog fixed; no background flow worker repairs it for the test.
            session.playlist_published.store(true, Relaxed);
            session.flow_worker_started.store(true, Release);
            if missing_boundary {
                session.segments.lock().await.segs =
                    parse_playlist("#EXTM3U\n#EXTINF:2.000,\nseg00002.ts\n");
            } else {
                tokio::fs::write(dir.path().join("index.m3u8"), "#EXTM3U\n")
                    .await
                    .expect("header-only rewrite");
            }
            let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let mgr = Arc::new(TranscodeManager::new(
                store,
                dir.path().join("manager-work"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            ));
            let budget = Duration::from_millis(40);
            let started = Instant::now();
            let deadline = started + budget;
            // Exercise the actual request loop without its outer I/O timeout:
            // that safety net otherwise hides a loop that retries forever.
            let refused = tokio::time::timeout(
                Duration::from_secs(1),
                mgr.playlist_with_owner_for_session_before(
                    "rejected-snapshot",
                    Arc::clone(&session),
                    deadline,
                    budget,
                ),
            )
            .await
            .expect("the request loop must enforce its own deadline")
            .map_err(|error| error.error);
            assert!(matches!(
                refused,
                Err(PlaylistError::StartupTimedOut(waited)) if waited == budget
            ));
            assert!(started.elapsed() >= budget);
            assert!(
                !session.failed.load(Relaxed),
                "an inconsistent snapshot is retryable"
            );

            session.fail(PlaylistError::SessionFailed("producer failed".into()));
            let refused = mgr
                .playlist_with_owner_for_session_before(
                    "rejected-snapshot",
                    Arc::clone(&session),
                    deadline,
                    budget,
                )
                .await
                .map_err(|error| error.error);
            assert_eq!(
                refused.err(),
                Some(PlaylistError::SessionFailed("producer failed".into())),
                "an expired request must still prefer the terminal producer cause"
            );
        }
    }

    /// The wait ends at its budget and not a poll before it.
    ///
    /// Pinned in milliseconds through the same deadline the production budget
    /// uses, because the value that budget takes is pinned separately above:
    /// together they say a session publishing at 42 s is served and one that
    /// never publishes is refused, without a 45-second test.
    #[tokio::test]
    async fn a_playlist_is_held_for_the_whole_budget_and_no_longer() {
        use plurx_core::store::SqliteStore;

        async fn manager(dir: &std::path::Path, budget_ms: u64) -> Arc<TranscodeManager> {
            let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let mgr = Arc::new(TranscodeManager::new(
                store,
                dir.join("manager-work"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            ));
            mgr.playlist_wait_override_ms.store(budget_ms, Relaxed);
            mgr
        }

        // Published late — but inside the budget. Held, then served.
        let dir = crate::test_tempdir().expect("tempdir");
        let session = watchdog_session(dir.path(), None, false);
        let mgr = manager(dir.path(), 4_000).await;
        mgr.sessions
            .lock()
            .await
            .insert("slow-start".into(), Arc::clone(&session));
        let publish = {
            let path = dir.path().to_path_buf();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                seeded_session_dir(&path, 12, 4.0).await;
            })
        };
        let started = Instant::now();
        let served = mgr.playlist("slow-start").await;
        assert!(
            served.is_ok(),
            "a session still starting inside the budget must be served, got {served:?}"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "it must have actually waited rather than found the playlist already there"
        );
        publish.await.expect("publisher");
        assert!(
            session
                .control
                .snapshot()
                .await
                .is_some_and(|lease| lease.last_renewal_kind == "test-start"),
            "resolving bytes alone must not impersonate a completed HTTP response"
        );

        // Never published. Refused at the deadline — and as a retryable
        // "still starting", never as a session that failed or went away.
        let empty = crate::test_tempdir().expect("tempdir");
        let stuck = watchdog_session(empty.path(), None, false);
        let mgr = manager(empty.path(), 600).await;
        mgr.sessions
            .lock()
            .await
            .insert("never-starts".into(), Arc::clone(&stuck));
        let started = Instant::now();
        let refused = mgr.playlist("never-starts").await;
        assert_eq!(
            refused,
            Err(PlaylistError::StartupTimedOut(Duration::from_millis(600)))
        );
        let err = refused.expect_err("refused");
        assert_eq!(err.code(), "startup_timeout");
        assert!(
            err.retryable(),
            "a stream the server may still publish is not a permanent failure"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "the deadline must be the budget, not an earlier poll count"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "and it must actually end at the budget"
        );
        assert!(
            stuck
                .control
                .snapshot()
                .await
                .is_some_and(|lease| lease.last_renewal_kind == "test-start"),
            "a timed-out playlist miss must not renew the playback lease"
        );
    }

    /// A terminal verdict is reported as itself, immediately, by every later
    /// reader — the whole point of recording *why* alongside the flag.
    #[tokio::test]
    async fn a_terminal_verdict_names_its_cause_to_every_later_reader() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let session = watchdog_session(dir.path(), None, false);
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        // A long budget, so a wait would be unmistakable: a session that has
        // already lost must not spend one poll of it.
        mgr.playlist_wait_override_ms.store(30_000, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("stalled".into(), Arc::clone(&session));

        session.fail(PlaylistError::SessionFailed(
            "the encoder never produced any video".into(),
        ));
        let started = Instant::now();
        let refused = mgr.playlist("stalled").await;
        assert_eq!(
            refused,
            Err(PlaylistError::SessionFailed(
                "the encoder never produced any video".into()
            ))
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a failed session still returns immediately — the longer budget is only \
             ever spent on a session that is genuinely still starting"
        );
        assert!(!refused.expect_err("refused").retryable());

        // First verdict wins. A later reader that merely notices the flag
        // must not overwrite the real cause with a vaguer restatement.
        session.fail(PlaylistError::ProducerExited("exit status: 1".into()));
        assert_eq!(
            mgr.playlist("stalled").await,
            Err(PlaylistError::SessionFailed(
                "the encoder never produced any video".into()
            ))
        );
    }

    /// Every cause carries a distinct machine-readable code and a sentence
    /// that is actually about that cause. The HLS route branches on the code;
    /// the overlay shows the message.
    #[test]
    fn every_playlist_refusal_is_distinguishable_and_readable() {
        let all = [
            PlaylistError::SessionGone,
            PlaylistError::ProducerExited("exit status: 1".into()),
            PlaylistError::SessionFailed("the encoder never produced any video".into()),
            PlaylistError::StartupTimedOut(Duration::from_secs(45)),
        ];
        let codes: std::collections::HashSet<&str> = all.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), all.len(), "each cause needs its own code");
        for err in &all {
            let message = err.message();
            assert!(
                message.len() > 20 && !message.contains("Settings"),
                "a refusal must explain itself rather than send someone to a log \
                 file on a headless box: {message}"
            );
        }
        // The two details a person can only get from the server.
        assert!(all[1].message().contains("exit status: 1"));
        assert!(all[3].message().contains("45"));
        // And exactly one of them is a "not yet" rather than a "never".
        assert_eq!(
            all.iter().filter(|e| e.retryable()).count(),
            1,
            "only a startup that ran out of budget is retryable"
        );
    }

    /// An actor-authorized retry deliberately kills one producer before
    /// installing its replacement. That gap is not a terminal exit:
    /// playlist startup must keep waiting instead of poisoning the successor.
    #[tokio::test]
    async fn playlist_waits_while_a_killed_producer_is_being_replaced() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let child = tokio::process::Command::new("false")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn false");
        let session = watchdog_session(dir.path(), Some(child), false);
        let _replacement = session
            .kill_child_for_replacement()
            .await
            .expect("a live session may replace its child");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("replacement-gap".into(), Arc::clone(&session));

        let waiting =
            tokio::time::timeout(Duration::from_millis(250), mgr.playlist("replacement-gap")).await;
        assert!(
            waiting.is_err(),
            "an intentional replacement gap keeps the publication window open"
        );
        assert!(
            !session.failed.load(Relaxed),
            "the killed predecessor must not poison its replacement"
        );
    }

    #[tokio::test]
    async fn published_attempt_rejection_preserves_the_predecessor() {
        let dir = crate::test_tempdir().expect("dir");
        let sentinel = dir.path().join("predecessor-segment");
        tokio::fs::write(&sentinel, b"still playable")
            .await
            .expect("sentinel");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        assert!(
            session
                .control
                .observe_publication(crate::playback_control::RollingPublicationObservation {
                    producer_attempt: 0,
                    publication_commit: true,
                    demand_sequence: None,
                    produced_segment: Some(0),
                    produced_end_ms: Some(4_000),
                    playlist_ready: true,
                    published_segment: Some(0),
                    published_end_ms: Some(4_000),
                    published_first_segment: Some(0),
                    published_start_ms: Some(0),
                    media_origin_ms: 0,
                    next_media_sequence: 1,
                    resolved_fetched_segment: None,
                    resolved_fetched_end_ms: None,
                })
                .await
        );

        let hardware_rejection = match session.kill_child_for_replacement().await {
            Err(rejection) => rejection,
            Ok(_) => panic!("published hardware predecessor must not be changed"),
        };
        assert_eq!(
            hardware_rejection,
            crate::playback_control::ProducerAttemptRejection::PlaylistPublished
        );
        assert!(tokio::fs::metadata(&sentinel).await.is_ok());
        assert!(session
            .child
            .lock()
            .await
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None))));

        session.kill_child().await;
    }

    #[tokio::test]
    async fn expired_attempt_cannot_install_a_spawned_replacement() {
        let dir = crate::test_tempdir().expect("dir");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let (mut replacement, producer_attempt) = session
            .kill_child_for_replacement()
            .await
            .expect("admit replacement before expiry");
        session
            .control
            .set_renewal_for_test(
                Instant::now() - Duration::from_secs(SESSION_IDLE_SECS + 1),
                "expired-after-admission",
            )
            .await;

        let candidate = long_running_child();
        let candidate_pid = candidate.id();
        assert!(candidate_pid.is_some());
        let rejection = session
            .install_replacement_child(producer_attempt, candidate)
            .await;
        assert_eq!(
            rejection,
            Err(crate::playback_control::ProducerAttemptRejection::SessionEnded),
            "final actor authorization must report the expired attempt"
        );
        replacement.settle_terminal_rejection();
        let lease = session.control.snapshot().await.expect("rolling actor");
        assert!(lease.retired && lease.expiration_claimed);
        assert!(
            !session.failed.load(Relaxed),
            "actor-owned expiry must not be relabeled as replacement cancellation"
        );
        assert!(
            session.replacing_child.load(Acquire),
            "terminal rejection remains path-fenced"
        );
        let mut installed = session.child.lock().await;
        assert_ne!(
            installed.as_ref().and_then(|child| child.id()),
            candidate_pid
        );
        assert!(
            installed
                .as_mut()
                .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_)))),
            "the session retains only its already-dead predecessor handle"
        );
        #[cfg(unix)]
        {
            let pid = i32::try_from(candidate_pid.expect("candidate pid fits u32"))
                .expect("candidate pid fits i32");
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH),
                "install rejection returns only after the candidate is reaped"
            );
        }
    }

    #[tokio::test]
    async fn cancellation_after_actor_mutation_before_reply_stays_fenced() {
        let dir = crate::test_tempdir().expect("dir");
        let predecessor = dir.path().join("seg00000.ts");
        tokio::fs::write(&predecessor, b"predecessor")
            .await
            .expect("predecessor segment");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let reply_pause = Arc::new(tokio::sync::Barrier::new(2));
        session
            .control
            .pause_producer_attempt_reply(Arc::clone(&reply_pause));

        let replacement = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                let _ = session.kill_child_for_replacement().await;
            }
        });
        reply_pause.wait().await;
        assert_eq!(
            session.control.current_producer_attempt(),
            1,
            "the actor has committed ownership before delivering its reply"
        );
        replacement.abort();
        assert!(replacement
            .await
            .expect_err("replacement task must abort")
            .is_cancelled());
        assert!(session.replacing_child.load(Acquire));
        assert!(session.failed.load(Relaxed));
        assert!(predecessor.exists());

        // Release the actor so it can observe the dropped reply and process
        // the retirement command scheduled by the transaction's Drop.
        reply_pause.wait().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drop-triggered actor retirement");
        assert!(session.coherent_path_producer_attempt().await.is_none());
    }

    #[tokio::test]
    async fn cancelled_post_admission_replacement_stays_fenced_and_retires() {
        let dir = crate::test_tempdir().expect("dir");
        let predecessor = dir.path().join("seg00000.ts");
        tokio::fs::write(&predecessor, b"predecessor")
            .await
            .expect("predecessor segment");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .replacement_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));

        let replacement = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                let _ = session.kill_child_for_replacement().await;
            }
        });
        pause.wait().await;
        tokio::task::yield_now().await;
        assert_eq!(session.control.current_producer_attempt(), 1);
        replacement.abort();
        assert!(
            replacement
                .await
                .expect_err("replacement task must abort")
                .is_cancelled(),
            "the fixture cancels after actor admission and projection reset"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drop-triggered actor retirement");
        assert!(session.replacing_child.load(Acquire));
        assert!(session.failed.load(Relaxed));
        assert!(predecessor.exists(), "the old bytes still physically exist");
        assert!(
            session.coherent_path_producer_attempt().await.is_none(),
            "cancelled replacement cannot reopen predecessor paths as attempt one"
        );
    }

    #[tokio::test]
    async fn retirement_after_authorization_cannot_publish_a_replacement() {
        let dir = crate::test_tempdir().expect("dir");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let (mut replacement, producer_attempt) = session
            .kill_child_for_replacement()
            .await
            .expect("admit replacement before retirement");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .producer_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));

        let candidate = long_running_child();
        let candidate_pid = candidate.id();
        let install = session.install_replacement_child(producer_attempt, candidate);
        let retire = async {
            pause.wait().await;
            session.control.end().await.expect("end verdict");
            pause.wait().await;
        };
        let (result, ()) = tokio::join!(install, retire);

        assert_eq!(
            result,
            Err(crate::playback_control::ProducerAttemptRejection::SessionEnded),
            "retirement between actor admission and assignment must win"
        );
        replacement.settle_terminal_rejection();
        assert!(!session.failed.load(Relaxed));
        assert!(session.replacing_child.load(Acquire));
        assert_ne!(
            session
                .child
                .lock()
                .await
                .as_ref()
                .and_then(|child| child.id()),
            candidate_pid,
            "the retired session cannot own the spawned candidate"
        );
        #[cfg(unix)]
        {
            let pid = i32::try_from(candidate_pid.expect("candidate pid fits u32"))
                .expect("candidate pid fits i32");
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH),
                "final-fence rejection returns only after the candidate is reaped"
            );
        }
    }

    /// Drive the hardware-to-software fallback itself, paused after it kills
    /// the predecessor. This pins the production call site as well as the
    /// replacement marker: a playlist poll inside that gap must not poison the
    /// successor the fallback is about to install.
    #[tokio::test]
    async fn playlist_waits_during_the_production_fallback_replacement() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let (mgr, _work, _cache) = cached_manager(&store);
        let mgr = Arc::new(mgr);
        let dir = crate::test_tempdir().expect("session dir");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        mgr.sessions
            .lock()
            .await
            .insert("fallback-gap".into(), Arc::clone(&session));

        // The gap is the property, and it belongs to begin_child_replacement
        // — the production primitive the actor's retry executor opens before
        // it terminates the failed attempt. Holding one open here observes
        // exactly the window a real retry creates, without going through a
        // ladder helper production no longer runs.
        let replacement = session.begin_child_replacement().await;
        assert!(
            session.replacing_child.load(Relaxed),
            "an open replacement marks the session as replacing"
        );

        let waiting =
            tokio::time::timeout(Duration::from_millis(250), mgr.playlist("fallback-gap")).await;
        assert!(
            waiting.is_err(),
            "the production replacement gap keeps the publication window open"
        );
        assert!(
            !session.failed.load(Relaxed),
            "the production replacement's killed predecessor must not poison its successor"
        );

        // Completing rather than dropping mid-admission: a cancelled
        // admission deliberately fails the session, and that is a different
        // test's property.
        replacement.complete();
        assert!(
            !session.replacing_child.load(Relaxed),
            "a completed replacement reopens the publication window"
        );
        session.kill_child().await;
    }

    /// A playback that has already spent its recovery does not get a second
    /// one — and finding that out costs it nothing.
    ///
    /// The ordering is the assertion. `begin_child_replacement` is the point
    /// after which this session has no producer until a new one is installed,
    /// so the reservation is taken before it: a refusal that arrived later
    /// would turn "this playback already recovered" into "this playback has
    /// nothing playing".
    ///
    /// The fixture puts the ledger on the recipe the actor names directly
    /// rather than building an alternate to hang it off. Which of the two
    /// frozen recipes a decision resolves to is tested where that resolution
    /// lives; what is under test here is what the executor does with a budget
    /// once it has one.
    #[tokio::test]
    async fn a_spent_recovery_budget_refuses_the_retry_before_anything_is_torn_down() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let dir = crate::test_tempdir().expect("session dir");
        seeded_session_dir(dir.path(), 1, 4.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));

        // An earlier attempt of this playback took the budget and settled it.
        let failed_plan_digest = "a".repeat(64);
        let predecessor = crate::playback_control::ProducerRecoveryLedger::new(
            Arc::clone(&store),
            7,
            "playback-spent",
            "epoch-spent",
            "incarnation-1",
        )
        .expect("a complete identity");
        assert!(
            matches!(
                predecessor
                    .reserve(1, 1, &failed_plan_digest, &"b".repeat(64), 1_000)
                    .await
                    .expect("reserve"),
                crate::playback_control::RecoveryReservation::Held(_)
            ),
            "the predecessor takes the budget"
        );
        assert_eq!(
            predecessor
                .settle(crate::playback_control::RecoveryOutcome::Exhausted, 1_100)
                .await
                .expect("settle"),
            crate::playback_control::RecoverySettlement::Settled
        );

        let mut opts = mgr.options_for_tone_map(
            Encoder::VideoToolbox,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        opts.pipeline = Pipeline::Cpu;
        let retry = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            dir.path(),
            "spent-budget",
        )
        .await
        .expect("a CPU-pipeline hardware attempt has a software rung")
        .with_recovery(DecodeRecoveryReservation {
            ledger: crate::playback_control::ProducerRecoveryLedger::new(
                Arc::clone(&store),
                7,
                "playback-spent",
                "epoch-spent",
                // A continuation of the same playback: same epoch, new
                // incarnation. This is the reopen the durable budget exists
                // for.
                "incarnation-2",
            )
            .expect("a complete identity"),
            failed_plan_digest,
        });

        let failed_attempt = session
            .control
            .begin_producer_attempt()
            .await
            .expect("admit the predecessor attempt");
        *session.child.lock().await = Some(AttemptChild::new(
            failed_attempt,
            long_running_child(),
            session.control.clone(),
            None,
        ));

        let result = execute_prepublication_transcode_retry(
            Arc::clone(&session),
            &retry,
            1,
            failed_attempt,
            &retry.actor_recipe,
            crate::playback_control::ProducerDecisionReason::SourceDecodeRetry,
            "spent-budget",
        )
        .await;

        assert!(
            matches!(&result, Err(error) if error.contains("spent")),
            "the refusal names which refusal it was, got {result:?}"
        );
        assert!(
            !session.replacing_child.load(Acquire),
            "a refused budget must not have opened a replacement"
        );
        assert!(
            !session.failed.load(Relaxed),
            "the session is not failed by a recovery it was never entitled to"
        );
        assert!(
            session
                .child
                .lock()
                .await
                .as_ref()
                .is_some_and(|child| child.id().is_some()),
            "the failed producer is still exactly where the actor left it"
        );
        session.kill_child().await;
    }
