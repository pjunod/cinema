
    #[test]
    fn public_playback_ids_and_segment_ranges_are_bounded() {
        assert!(valid_playback_id("player-a"));
        assert!(!valid_playback_id("   "));
        assert!(!valid_playback_id("player\r\nforged"));
        assert!(!valid_playback_id(&"p".repeat(129)));

        assert_eq!(requested_byte_range(None, 100), Ok(None));
        assert_eq!(
            requested_byte_range(Some("bytes=10-19"), 100),
            Ok(Some((10, 19)))
        );
        assert_eq!(
            requested_byte_range(Some("bytes=90-"), 100),
            Ok(Some((90, 99)))
        );
        assert_eq!(
            requested_byte_range(Some("bytes=-10"), 100),
            Ok(Some((90, 99)))
        );
        assert_eq!(
            requested_byte_range(Some("bytes=-200"), 100),
            Ok(Some((0, 99)))
        );
        assert_eq!(requested_byte_range(Some("bytes=10-9"), 100), Err(()));
        assert_eq!(requested_byte_range(Some("bytes=100-"), 100), Err(()));
        assert_eq!(requested_byte_range(Some("bytes=0-1,3-4"), 100), Err(()));
        assert!(range_covers_object(None, 100));
        assert!(range_covers_object(
            requested_byte_range(Some("bytes=0-"), 100).expect("open range"),
            100
        ));
        assert!(range_covers_object(
            requested_byte_range(Some("bytes=-200"), 100).expect("full suffix"),
            100
        ));
        assert!(!range_covers_object(Some((0, 98)), 100));
        assert!(!range_covers_object(Some((1, 99)), 100));

        let mut headers = RelayHeaders {
            range: Some("bytes=10-19".to_owned()),
            if_range: None,
            ..RelayHeaders::default()
        };
        assert_eq!(
            range_for_current_etag(&headers, "\"current\""),
            Some("bytes=10-19")
        );
        headers.if_range = Some("\"current\"".to_owned());
        assert_eq!(
            range_for_current_etag(&headers, "\"current\""),
            Some("bytes=10-19"),
            "an exact strong validator preserves Range"
        );
        for non_match in [
            "W/\"current\"",
            "\"previous\"",
            "Sun, 23 Aug 2026 08:00:00 GMT",
            "",
        ] {
            headers.if_range = Some(non_match.to_owned());
            assert_eq!(
                range_for_current_etag(&headers, "\"current\""),
                None,
                "{non_match:?} must fall back to the complete representation"
            );
        }
    }

    #[test]
    fn mixed_rollout_peer_terminal_statuses_require_fresh_route_agreement() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::GONE,
        ] {
            assert!(
                relay_status_requires_reclassification(status),
                "{status} from one peer is not authoritative during ownership handoff"
            );
        }
        assert!(!relay_status_requires_reclassification(StatusCode::OK));
        assert!(!relay_status_requires_reclassification(
            StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    async fn add_http_text_subtitle(fixture: &mut HlsDeliveryFixture, session_id: &str) {
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("fixture file lookup")
            .expect("fixture file");
        let probe = plurx_core::domain::ProbeResult {
            duration_ms: file.duration_ms,
            container: file.container.clone(),
            video_codec: file.video_codec.clone(),
            video_codec_tag: file.video_codec_tag.clone(),
            field_order: file.field_order.clone(),
            video_profile: file.video_profile.clone(),
            width: file.width,
            height: file.height,
            bit_depth: file.bit_depth,
            hdr: file.hdr.clone(),
            dolby_vision: Default::default(),
            hdr_format: file.hdr_format.clone(),
            max_cll: file.max_cll,
            max_fall: file.max_fall,
            mastering_max_luminance: file.mastering_max_luminance,
            luminance_source: file.luminance_source.clone(),
            bitrate: file.bitrate,
            audio_streams: file.audio_streams.clone(),
            subtitle_streams: vec![SubtitleStream {
                index: 0,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: Some("English".into()),
                default: true,
                forced: false,
                hearing_impaired: false,
            }],
            raw_json: None,
            creation_time: None,
        };
        fixture
            .store
            .upsert_file(
                file.item_id,
                file.path.to_str().expect("fixture path"),
                file.size,
                file.mtime,
                &probe,
            )
            .await
            .expect("install text subtitle");
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("updated fixture lookup")
            .expect("updated fixture");
        fixture
            .refresh_frozen_presentation_from_store(session_id)
            .await;
        tokio::fs::create_dir_all(&fixture.state.subs_dir)
            .await
            .expect("subtitle cache");
        tokio::fs::write(
            crate::subtitles::vtt_path(&fixture.state.subs_dir, &file, 0),
            b"WEBVTT\n\n",
        )
        .await
        .expect("published VTT sidecar");
    }

    struct WindowFixtureSubtitleSource {
        whole_runs: Arc<std::sync::atomic::AtomicUsize>,
        whole_started: Arc<tokio::sync::Semaphore>,
        whole_release: Arc<tokio::sync::Semaphore>,
        runs: Arc<std::sync::atomic::AtomicUsize>,
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
        /// Window producers running *right now*.
        ///
        /// Counted with a drop guard rather than by subtracting completions,
        /// because the case this exists to measure is the producer that never
        /// completes: a superseded extraction is dropped mid-body, and a
        /// counter incremented at the end of the body would never learn it had
        /// gone. The guard runs whether the future finishes or is dropped, so
        /// this is the physical number of live extractions.
        live: Arc<std::sync::atomic::AtomicUsize>,
        /// The highest `live` ever reached.
        ///
        /// Sampling `live` from the test task can only observe instants the
        /// test task is awake for, and after an await that waited for
        /// settlement those instants are the ones where an overlap is least
        /// likely. This is recorded by the producers themselves, so an overlap
        /// that existed for one poll is still visible afterwards.
        peak_live: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// Decrements the live-producer count however its producer ends.
    struct LiveProducer(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for LiveProducer {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl WindowFixtureSubtitleSource {
        fn counting() -> Self {
            Self {
                whole_runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                whole_started: Arc::new(tokio::sync::Semaphore::new(0)),
                whole_release: Arc::new(tokio::sync::Semaphore::new(0)),
                runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                started: Arc::new(tokio::sync::Semaphore::new(0)),
                release: Arc::new(tokio::sync::Semaphore::new(0)),
                live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                peak_live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn window_runs(&self) -> usize {
            self.runs.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn live_window_producers(&self) -> usize {
            self.live.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn peak_window_producers(&self) -> usize {
            self.peak_live.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl SubtitleSegmentSource for WindowFixtureSubtitleSource {
        fn read_whole<'a>(
            &'a self,
            dir: &'a Path,
            file: &'a MediaFile,
            index: i64,
        ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
            Box::pin(crate::subtitles::read_cached_vtt(dir, file, index))
        }

        fn read_window<'a>(
            &'a self,
            dir: &'a Path,
            file: &'a MediaFile,
            index: i64,
            anchor_seconds: i64,
            window_seconds: i64,
        ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
            Box::pin(crate::subtitles::read_cached_window(
                dir,
                file,
                index,
                anchor_seconds,
                window_seconds,
            ))
        }

        fn warm_whole<'a>(
            &'a self,
            dir: &'a Path,
            file: &'a MediaFile,
            index: i64,
        ) -> BoxFuture<'a, ()> {
            let runs = Arc::clone(&self.whole_runs);
            let started = Arc::clone(&self.whole_started);
            let whole_release = Arc::clone(&self.whole_release);
            Box::pin(crate::subtitles::warm_vtt_with(
                dir,
                file,
                index,
                move |tmp, _, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    started.add_permits(1);
                    whole_release
                        .acquire()
                        .await
                        .expect("release whole producer")
                        .forget();
                    tokio::fs::write(tmp, b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nwhole cue\n")
                        .await
                        .map_err(|error| error.to_string())
                },
            ))
        }

        fn warm_window<'a>(
            &'a self,
            session: &'a str,
            sequence: Option<u64>,
            dir: &'a Path,
            file: &'a MediaFile,
            index: i64,
            anchor_seconds: i64,
            window_seconds: i64,
        ) -> BoxFuture<'a, bool> {
            let runs = Arc::clone(&self.runs);
            let started = Arc::clone(&self.started);
            let release = Arc::clone(&self.release);
            let live = Arc::clone(&self.live);
            let peak_live = Arc::clone(&self.peak_live);
            Box::pin(crate::subtitles::warm_vtt_window_with(
                session,
                sequence,
                dir,
                file,
                index,
                anchor_seconds,
                window_seconds,
                move |tmp, _, _, anchor_seconds, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let now = live.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    peak_live.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    let _live = LiveProducer(live);
                    // The partial write happens BEFORE the park, so a producer
                    // that is superseded has a temp file on disk when it dies.
                    // A real ffmpeg is the same shape — it is writing while it
                    // runs — and without it "an abort unlinks its own temp
                    // file" would be a claim about a file that never existed.
                    tokio::fs::write(&tmp, b"WEBVTT\n\n")
                        .await
                        .map_err(|error| error.to_string())?;
                    started.add_permits(1);
                    // A superseded producer is dropped exactly here, which is
                    // what makes `live` fall again without this body ever
                    // reaching its end.
                    // `forget` rather than holding the guard: a permit that
                    // returned to the semaphore when one producer finished
                    // would silently release the next one, and this fixture
                    // exists to decide exactly which producers get to run.
                    release
                        .acquire()
                        .await
                        .expect("release window producer")
                        .forget();
                    let start = format_vtt_timestamp(anchor_seconds as f64 + 1.0);
                    let end = format_vtt_timestamp(anchor_seconds as f64 + 2.0);
                    tokio::fs::write(tmp, format!("WEBVTT\n\n{start} --> {end}\nready cue\n"))
                        .await
                        .map_err(|error| error.to_string())
                },
            ))
        }

        fn window_flight_is_live<'a>(
            &'a self,
            dir: &'a Path,
            file: &'a MediaFile,
            index: i64,
            anchor_seconds: i64,
            window_seconds: i64,
        ) -> BoxFuture<'a, bool> {
            Box::pin(crate::subtitles::window_flight_is_live(
                dir,
                file,
                index,
                anchor_seconds,
                window_seconds,
            ))
        }

        fn whole_track_state<'a>(
            &'a self,
            dir: &'a Path,
            file: &'a MediaFile,
            index: i64,
        ) -> BoxFuture<'a, crate::subtitles::SidecarState> {
            Box::pin(crate::subtitles::sidecar_state(dir, file, index))
        }
    }

    #[tokio::test]
    async fn stored_whole_track_skips_window_warm() {
        use crate::subtitle_source::{
            RepresentationEntry, RepresentationFormat, TrackEntry, TrackKind, Verdict,
        };
        use sha2::Digest as _;

        let dir = crate::test_tempdir().expect("session directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "stored-whole-vtt").await;
        add_http_text_subtitle(&mut fixture, "stored-whole-vtt").await;
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("lookup")
            .expect("file");
        tokio::fs::remove_file(crate::subtitles::vtt_path(
            &fixture.state.subs_dir,
            &file,
            0,
        ))
        .await
        .expect("cold cache");
        tokio::fs::write(dir.path().join("seg00000.ts"), b"video")
            .await
            .expect("video segment");
        tokio::fs::write(
            dir.path().join("index.m3u8"),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.000,\nseg00000.ts\n",
        )
        .await
        .expect("playlist");

        let root = crate::subtitle_source::store_root(&fixture.state.runtime_cache_dir);
        let local = crate::subtitle_source::file_dir(&root, file.id);
        std::fs::create_dir_all(&local).expect("store directory");
        let bytes = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\ncaption\n";
        let sha256 = hex::encode(sha2::Sha256::digest(bytes));
        let name =
            crate::subtitle_source::representation_file_name(0, RepresentationFormat::Webvtt, &sha256)
                .expect("name");
        std::fs::write(local.join(&name), bytes).expect("stored VTT");
        crate::subtitle_source::testing::write_manifest(
            &root,
            file.id,
            crate::subtitle_source::testing::stamp_of(&file.path),
            vec![TrackEntry {
                ordinal: 0,
                kind: TrackKind::Text,
                representations: vec![RepresentationEntry {
                    format: RepresentationFormat::Webvtt,
                    origin: crate::subtitle_source::RepresentationOrigin::Extracted,
                    verdict: Verdict::Kept,
                    attempts: 1,
                    file: Some(name),
                    sha256: Some(sha256),
                    bytes: bytes.len() as u64,
                }],
                verdict: Verdict::Kept,
                attempts: 1,
                file: None,
                sha256: None,
            }],
        );

        let response = subtitle_vtt_local_before(
            &fixture.state,
            "stored-whole-vtt",
            0,
            "seg00000.vtt",
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("stored segment");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(crate::subtitles::vtt_path(&fixture.state.subs_dir, &file, 0).exists());
        assert!(
            !crate::subtitles::vtt_window_path(&fixture.state.subs_dir, &file, 0, 0, 200).exists(),
            "the whole-track store answer skips the window warm"
        );
    }

    /// M7 R-M2 B7: exercise the real subtitle-segment handler boundary while
    /// replacing only the producer. The first requests must not wait for the
    /// held extraction, concurrent first touches must share its production
    /// flight, and the same advertised segment must expose its cues after the
    /// window publishes. A segment the video playlist never advertised stays
    /// a 404 throughout.
    #[tokio::test]
    async fn subtitles_cold_segment_is_empty_nonblocking_then_serves_one_window_flight() {
        let dir = crate::test_tempdir().expect("session directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "subtitle-window").await;
        add_http_text_subtitle(&mut fixture, "subtitle-window").await;
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("fixture file lookup")
            .expect("fixture file");
        tokio::fs::remove_file(crate::subtitles::vtt_path(
            &fixture.state.subs_dir,
            &file,
            0,
        ))
        .await
        .expect("return fixture to a cold cache");
        tokio::fs::write(dir.path().join("seg00000.ts"), b"video")
            .await
            .expect("video segment");
        tokio::fs::write(
            dir.path().join("index.m3u8"),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.000,\nseg00000.ts\n",
        )
        .await
        .expect("video playlist");
        assert_eq!(
            fixture
                .state
                .transcode
                .segment_window("subtitle-window", 0)
                .await,
            Some((0.0, 4.0)),
            "the fixture advertises exactly one subtitle interval"
        );

        let source = Arc::new(WindowFixtureSubtitleSource::counting());
        let request = |state: AppState, source: Arc<WindowFixtureSubtitleSource>| async move {
            subtitle_vtt_local_before_with_source(
                &state,
                "subtitle-window",
                0,
                "seg00000.vtt",
                Instant::now() + Duration::from_secs(5),
                source.as_ref(),
            )
            .await
            .expect("advertised subtitle segment")
        };
        let first = tokio::spawn(request(fixture.state.clone(), Arc::clone(&source)));
        let second = tokio::spawn(request(fixture.state.clone(), Arc::clone(&source)));

        let _started = tokio::time::timeout(Duration::from_secs(2), source.started.acquire())
            .await
            .expect("window producer starts")
            .expect("started semaphore remains open");
        let _whole_started =
            tokio::time::timeout(Duration::from_secs(2), source.whole_started.acquire())
                .await
                .expect("whole-track producer starts")
                .expect("whole started semaphore remains open");
        // Three seconds, not two, and the reason is the publication wait: the
        // second request can find the first's flight already live and stand
        // still for up to `SUBTITLE_SEGMENT_PUBLICATION_WAIT` hoping it
        // publishes. It never does here — the producer is parked — so the
        // answer is still the empty segment, and the point of the bound is
        // that the request is not waiting for the *extraction*, which this
        // fixture holds open indefinitely.
        let (first, second) = tokio::time::timeout(Duration::from_secs(3), async {
            (
                first.await.expect("first request task"),
                second.await.expect("second request task"),
            )
        })
        .await
        .expect("cold requests return without waiting for the held producer");
        assert_eq!(
            source.runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "concurrent first touches share exactly one window producer"
        );
        assert_eq!(
            source.whole_runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the artificially slow whole-track producer is also single-flight"
        );
        for response in [first, second] {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&axum::http::HeaderValue::from_static("no-store"))
            );
            let body = response
                .into_body()
                .collect()
                .await
                .expect("empty VTT body")
                .to_bytes();
            assert_eq!(body.as_ref(), b"WEBVTT\n\n");
        }

        let missing = subtitle_vtt_local_before_with_source(
            &fixture.state,
            "subtitle-window",
            0,
            "seg00001.vtt",
            Instant::now() + Duration::from_secs(5),
            source.as_ref(),
        )
        .await;
        assert!(matches!(
            missing,
            Err(ApiError::NotFound("subtitle segment"))
        ));

        source.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    crate::subtitles::read_cached_window(&fixture.state.subs_dir, &file, 0, 0, 200,)
                        .await,
                    Ok(Some(_))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("window publishes");

        let ready = request(fixture.state.clone(), Arc::clone(&source)).await;
        assert_eq!(ready.status(), StatusCode::OK);
        assert_eq!(
            ready.headers().get(header::CACHE_CONTROL),
            Some(&axum::http::HeaderValue::from_static("no-store"))
        );
        let body = ready
            .into_body()
            .collect()
            .await
            .expect("ready VTT body")
            .to_bytes();
        let body = String::from_utf8(body.to_vec()).expect("utf8 VTT");
        assert!(body.contains("ready cue"), "{body}");
        assert_eq!(
            source.runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "serving the published window does not relaunch production"
        );

        source.whole_release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("slow whole-track producer settles before fixture cleanup");
    }

    /// M7 R-M3: seek coalescing at the production boundary.
    ///
    /// The acceptance language is that "a 20-seek storm starts work only for
    /// the settled target". Nineteen logical `supersedes` verdicts do not say
    /// that — they say the latch can rank two numbers. What says it is a storm
    /// driven through real control exchanges and the real subtitle-segment
    /// handler, with the producer counted as it spawns and as it dies.
    mod seek_coalescing {
        use super::*;

        /// One segment per window anchor, so a storm walks a real grid rather
        /// than re-requesting one key. Sixty-second segments sit under the
        /// hundred-and-twenty-second retention cap, and pinning the operator's
        /// span to its thirty-second minimum makes every segment start its own
        /// anchor. The fixture film is 6,000 s and a window declines past its
        /// midpoint, so every segment used here stays well below 3,000 s.
        const SEGMENT_SECONDS: i64 = 60;
        const SEGMENTS: i64 = 15;
        const WINDOW_SECONDS: i64 = 30;

        /// A subtitle-capable session with a cold cache and a long playlist.
        async fn cold_windowed_fixture(
            dir: &std::path::Path,
            session_id: &str,
        ) -> (HlsDeliveryFixture, MediaFile) {
            let mut fixture = HlsDeliveryFixture::publish(dir, session_id).await;
            fixture
                .store
                .put_setting(
                    plurx_core::store::keys::SUBTITLE_WINDOW_SECS,
                    &WINDOW_SECONDS.to_string(),
                )
                .await
                .expect("pin the operator's window span");
            add_http_text_subtitle(&mut fixture, session_id).await;
            let file = fixture
                .store
                .get_file(fixture.file_id())
                .await
                .expect("fixture file lookup")
                .expect("fixture file");
            tokio::fs::remove_file(crate::subtitles::vtt_path(
                &fixture.state.subs_dir,
                &file,
                0,
            ))
            .await
            .expect("return the fixture to a cold cache");
            let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:60\n");
            for index in 0..SEGMENTS {
                let name = format!("seg{index:05}.ts");
                tokio::fs::write(dir.join(&name), b"video")
                    .await
                    .expect("video segment");
                playlist.push_str(&format!("#EXTINF:60.000,\n{name}\n"));
            }
            tokio::fs::write(dir.join("index.m3u8"), playlist.as_bytes())
                .await
                .expect("video playlist");
            // The segment index is built by the publication flow, not by the
            // file on disk, so ask for the last interval once. It both primes
            // the index every subtitle request reads and proves the fixture
            // really advertises the grid this storm walks.
            assert_eq!(
                fixture
                    .state
                    .transcode
                    .segment_window(session_id, SEGMENTS - 1)
                    .await,
                Some((
                    ((SEGMENTS - 1) * SEGMENT_SECONDS) as f64,
                    (SEGMENTS * SEGMENT_SECONDS) as f64
                )),
                "the fixture advertises every segment the storm asks for"
            );
            (fixture, file)
        }

        /// Wait until the producer a request just started is actually running.
        ///
        /// `warm_window` returns as soon as the flight is owned; the extraction
        /// itself begins one poll later. Counting live producers before that
        /// happens would measure the scheduler, not the contract.
        async fn producer_started(source: &WindowFixtureSubtitleSource) {
            match tokio::time::timeout(Duration::from_secs(5), source.started.acquire()).await {
                Ok(permit) => permit.expect("started semaphore remains open").forget(),
                Err(_) => panic!(
                    "no window producer started: runs={} live={}",
                    source.window_runs(),
                    source.live_window_producers()
                ),
            }
        }

        async fn subtitle_segment(
            state: &AppState,
            session: &str,
            segment: i64,
            source: &WindowFixtureSubtitleSource,
        ) -> Response {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match subtitle_vtt_local_before_with_source(
                    state,
                    session,
                    0,
                    &format!("seg{segment:05}.vtt"),
                    deadline,
                    source,
                )
                .await
                {
                    Ok(response) => return response,
                    Err(ApiError::Typed { code, .. })
                        if code == "response_state_changed" && Instant::now() < deadline =>
                    {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("an advertised subtitle segment: {error:?}"),
                }
            }
        }

        /// One accepted control exchange naming where the client now is.
        async fn settle_on(
            fixture: &HlsDeliveryFixture,
            session_id: &str,
            generation: &str,
            client: &str,
            sequence: u64,
            target_seconds: i64,
        ) -> Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        > {
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Web,
            );
            let target_ms = target_seconds.saturating_mul(1_000);
            // A storm is a scrub bar being dragged, so the snapshots that drive
            // it are seeks: the client is not where it was, and `position_ms`
            // still names the old place until the seek lands.
            snapshot.render_state = crate::playback_control::RenderState::Seeking;
            snapshot.position_ms = target_ms.saturating_sub(4_000).max(0);
            snapshot.seek_target_ms = Some(target_ms);
            snapshot.buffered_from_ms = Some(target_ms);
            snapshot.buffered_through_ms = target_ms.saturating_add(15_000);
            fixture
                .state
                .transcode
                .hls_session_control(crate::playback_control::LocalControlRequest {
                    session_id,
                    generation,
                    owner_node_id: "test-node",
                    owner_epoch: 1,
                    client_instance_id: client,
                    sequence,
                    snapshot,
                    prepared_successor:
                        crate::playback_control::PreparedSuccessorObservation::NotRequested,
                })
                .await
                .expect("the storm session is local")
        }

        /// The twenty destinations. It walks forward and scrubs back, because
        /// a real storm is a scrub bar being dragged, not a monotonic ramp —
        /// and a revisited anchor proves an aborted flight left no memo behind
        /// to suppress its own retry.
        const STORM: [i64; 20] = [
            0, 3, 1, 6, 2, 9, 4, 11, 5, 13, 8, 14, 7, 12, 10, 6, 9, 3, 11, 7,
        ];

        #[tokio::test]
        async fn a_twenty_seek_storm_starts_work_only_for_the_settled_target() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client = uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &generation,
                "test-node",
            )
            .await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());
            let window_seconds = fixture.state.subtitle_window_seconds().await;

            // Fact 6 needs a window that really published, and it has to
            // publish before the storm moves on, so the first destination is
            // driven to completion on its own.
            let first = subtitle_segment(&fixture.state, session_id, STORM[0], source.as_ref()).await;
            assert_eq!(first.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;
            source.release.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        STORM[0] * SEGMENT_SECONDS,
                        window_seconds,
                    )
                    .await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the first window publishes");

            let mut sequence = 0_u64;
            // A create that the storm has already passed, held back so it
            // reaches admission after a later snapshot was accepted.
            let mut pending_create: Option<(u64, f64)> = None;
            let mut refusals = 0_usize;
            for segment in STORM {
                sequence += 1;
                let target_seconds = segment * SEGMENT_SECONDS;

                let outcome = settle_on(
                    &fixture,
                    session_id,
                    &generation,
                    &client,
                    sequence,
                    target_seconds,
                )
                .await
                .expect("the storm's exchanges are accepted");
                assert_eq!(
                    outcome.disposition,
                    crate::playback_control::ControlDisposition::Accepted
                );

                // Fact 1, with the ordering the acceptance names: the snapshot
                // for this destination has just been accepted, and only now
                // does the create for the *previous* one reach admission. The
                // settled target it is judged against is therefore read from
                // the live actor after the later exchange landed, which is the
                // race a create loses in production.
                //
                // `create` itself needs a store, a transcode manager and an
                // authenticated user, so the acceptance reaches the decision
                // where the handler reaches it — through `admit_restart`, which
                // returns the same `ApiError` the client receives and sits
                // above every producer and activation in `create`'s body.
                if let Some((stale_sequence, stale_start)) = pending_create.take() {
                    let settled = fixture
                        .state
                        .transcode
                        .settled_target_for_session(session_id)
                        .await;
                    match admit_restart(settled, Some(stale_sequence), stale_start, None) {
                        Err(ApiError::Typed { status, code, .. }) => {
                            assert_eq!(status, StatusCode::CONFLICT);
                            assert_eq!(code, "playback_target_superseded");
                            refusals += 1;
                        }
                        other => panic!("a create for a destination the storm left: {other:?}"),
                    }
                }

                // Fact 2: the ordering rules the storm relies on are unchanged.
                assert!(matches!(
                    settle_on(
                        &fixture,
                        session_id,
                        &generation,
                        &client,
                        sequence,
                        target_seconds
                    )
                    .await,
                    Ok(crate::playback_control::LocalControlResult {
                        disposition: crate::playback_control::ControlDisposition::Replay,
                        ..
                    })
                ));
                assert!(matches!(
                    settle_on(
                        &fixture,
                        session_id,
                        &generation,
                        &client,
                        sequence - 1,
                        target_seconds
                    )
                    .await,
                    Err(crate::playback_control::ControlStateError::StaleSequence)
                ));

                let response =
                    subtitle_segment(&fixture.state, session_id, segment, source.as_ref()).await;
                assert_eq!(response.status(), StatusCode::OK);
                if segment != STORM[0] {
                    // Every destination but the first is cold, so every one of
                    // them spawns. The first is already published and correctly
                    // starts nothing at all.
                    producer_started(source.as_ref()).await;
                }

                // Fact 4, asserted at every step rather than at the end: one
                // playback never has more than one live window producer.
                assert!(
                    source.live_window_producers() <= 1,
                    "sequence {sequence} left {} window producers alive",
                    source.live_window_producers()
                );

                // Hold back this destination's create so the next iteration
                // presents it after a later snapshot has landed.
                pending_create = Some((sequence, target_seconds as f64));

                // Above the protocol's 250 ms exchange floor, which is what
                // makes the next sequence an acceptance rather than a
                // rate-limit rejection.
                tokio::time::sleep(Duration::from_millis(260)).await;
            }

            assert_eq!(
                refusals,
                STORM.len() - 1,
                "every destination but the first is superseded before its own create lands"
            );

            // Fact 8: exactly one producer survives, and it carries the target
            // the client actually settled on.
            let settled = fixture
                .state
                .transcode
                .settled_target_for_session(session_id)
                .await
                .expect("the storm settled somewhere");
            let final_anchor =
                crate::subtitles::window_anchor_seconds(settled.anchor_ms / 1_000, window_seconds);
            let owned = crate::subtitles::owned_window_for_test(session_id)
                .expect("the settled destination is still being extracted");
            assert_eq!(
                owned.0, final_anchor,
                "the survivor carries the final target"
            );
            assert_eq!(owned.1, window_seconds);
            assert_eq!(owned.2, Some(sequence));
            assert_eq!(
                source.live_window_producers(),
                1,
                "the storm settles on exactly one live producer"
            );
            assert_eq!(
                source.peak_window_producers(),
                1,
                "and never had two extractors running at once during it"
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1,
                "nor two flights, which is the wider span the settlement wait \
                     exists to keep from overlapping — a displaced flight is still \
                     alive while it kills its child and clears the registries"
            );

            // Release every held producer before judging what published.
            //
            // Without this the storm's abandoned anchors are absent for a
            // reason that says nothing about supersession: the fixture never
            // let any of them past its gate. Opening the gate now means a
            // producer that was merely *parked* would wake and publish, while
            // one that was actually aborted is gone and cannot. Waiting for the
            // survivor's own window to appear proves the permits really flowed.
            source.release.add_permits(STORM.len());
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        final_anchor,
                        window_seconds,
                    )
                    .await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the surviving producer publishes once it is released");

            // Fact 5: every destination the storm passed through published
            // nothing. Only the first, which was deliberately driven to
            // completion, is on disk.
            for segment in STORM {
                if segment == STORM[0] || segment * SEGMENT_SECONDS == final_anchor {
                    continue;
                }
                assert!(
                    matches!(
                        crate::subtitles::read_cached_window(
                            &fixture.state.subs_dir,
                            &file,
                            0,
                            segment * SEGMENT_SECONDS,
                            window_seconds,
                        )
                        .await,
                        Ok(None)
                    ),
                    "an abandoned window at {segment} published bytes"
                );
            }

            // Fact 6: the window that did publish is untouched by every
            // supersession that followed it.
            assert!(matches!(
                crate::subtitles::read_cached_window(
                    &fixture.state.subs_dir,
                    &file,
                    0,
                    STORM[0] * SEGMENT_SECONDS,
                    window_seconds,
                )
                .await,
                Ok(Some(_))
            ));

            // Nothing was left half-written. An abort unlinks its own temp
            // file and only its own temp file, so inverting that unlink to the
            // cache name — the one way a cancellation could reach a published
            // sidecar — shows up here as a `.tmp-` entry that never goes away.
            //
            // Waited for rather than sampled once: the survivor's publication is
            // what proved the gate opened, and `read_cached_window` starts
            // answering at the atomic write, one unlink before that producer is
            // finished. Sampling on that edge measures how fast the runner is.
            // The wait still fails on the defect, because a temp file nothing
            // will ever unlink outlasts any timeout.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let mut stray = None;
                    let mut entries = tokio::fs::read_dir(&fixture.state.subs_dir)
                        .await
                        .expect("subtitle cache directory");
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with(".tmp-") {
                            stray = Some(name);
                            break;
                        }
                    }
                    match stray {
                        None => return,
                        Some(_) => tokio::task::yield_now().await,
                    }
                }
            })
            .await
            .expect("every abandoned window unlinked its own temp file");

            // Release the whole-track producer last: publishing it prunes the
            // matching windows, which would erase the evidence above.
            source.whole_release.add_permits(4);
            crate::subtitles::release_session_window(session_id).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }

        /// Fact 3, first half: what admission hands the activation.
        ///
        /// This half is cheap and shallow on purpose — it pins that admission
        /// names the predecessor it saw and asks for a compare-and-swap rather
        /// than a last-writer-wins overwrite. The half that matters, that the
        /// durable pointer actually refuses the loser, is the test below it.
        #[test]
        fn an_admitted_successor_fences_the_predecessor_it_replaces() {
            let admitted = admit_restart(
                Some(crate::playback_control::SettledTarget {
                    sequence: 4,
                    anchor_ms: 1_800_000,
                }),
                Some(9),
                1_800.0,
                Some("incarnation-4"),
            )
            .expect("a create carrying the newest sequence is admitted");
            assert!(admitted.fence_predecessor);
            assert_eq!(
                admitted.expected_predecessor_incarnation_id.as_deref(),
                Some("incarnation-4")
            );

            let first_play = admit_restart(None, Some(1), 0.0, None)
                .expect("a first play has no ordering to be stale against");
            assert!(
                first_play.fence_predecessor,
                "an ordinary start is a CAS against the pointer being absent"
            );
            assert_eq!(first_play.expected_predecessor_incarnation_id, None);
        }

        /// Fact 3, second half: the fence admission asks for is real.
        ///
        /// `fence_predecessor` is a constant in the code, so asserting on it
        /// proves only that the constant is still there. What the storm
        /// actually depends on is that the durable pointer honours it — that a
        /// losing restart naming the predecessor cannot take the pointer back
        /// once the winner has moved it. That is a Store contract, so it is
        /// asserted against the Store.
        #[tokio::test]
        async fn the_pointer_refuses_a_loser_that_still_names_the_old_incarnation() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let predecessor = uuid::Uuid::new_v4().to_string();
            let fixture = HlsDeliveryFixture::publish(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &predecessor,
                "test-node",
            )
            .await;

            let winner = uuid::Uuid::new_v4().to_string();
            let admitted = admit_restart(None, Some(9), 1_800.0, Some(predecessor.as_str()))
                .expect("the newest sequence is admitted");
            let fingerprint = "b".repeat(64);
            let claim = |incarnation: String, fingerprint: String| {
                let store = Arc::clone(&fixture.store);
                async move {
                    let now_ms = crate::media_sessions::unix_ms();
                    store
                        .claim_media_session_request(
                            7,
                            &incarnation,
                            &fingerprint,
                            "player-control",
                            &incarnation,
                            now_ms,
                            now_ms + 60_000,
                        )
                        .await
                        .expect("claim the restart's request");
                    assert!(store
                        .assign_media_session_request_owner(
                            7,
                            &incarnation,
                            &incarnation,
                            "test-node",
                            now_ms,
                        )
                        .await
                        .expect("assign the request owner"));
                }
            };
            let activation = |incarnation: &str, admitted: &RestartAdmission| {
                let now_ms = crate::media_sessions::unix_ms();
                plurx_core::domain::MediaSessionActivation {
                    recovery_epoch: String::new(),
                    expected_desired_revision: None,
                    incarnation_id: incarnation.to_owned(),
                    // A restart is a new session taking over one playback, so
                    // the session id moves and the playback id is what the
                    // pointer is keyed by.
                    session_id: uuid::Uuid::new_v4().to_string(),
                    user_id: 7,
                    playback_id: "player-control".to_owned(),
                    expected_predecessor_incarnation_id: admitted
                        .expected_predecessor_incarnation_id
                        .clone(),
                    fence_predecessor: admitted.fence_predecessor,
                    request_id: Some(incarnation.to_owned()),
                    request_fingerprint: fingerprint.clone(),
                    owner_node_id: "test-node".to_owned(),
                    recipe_json: "{}".to_owned(),
                    response_json: "{}".to_owned(),
                    publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                    media_origin_ms: 0,
                    now_ms,
                    lease_expires_at_ms: now_ms + 60_000,
                }
            };

            claim(winner.clone(), fingerprint.clone()).await;
            assert!(
                fixture
                    .store
                    .activate_media_session(&activation(&winner, &admitted))
                    .await
                    .expect("winner activation")
                    .is_some(),
                "the winner names the incarnation the pointer holds, so its CAS succeeds"
            );

            // The loser was admitted against the same predecessor — it lost the
            // race, not the ordering check — and still asks to take the pointer
            // from an incarnation that no longer holds it.
            let loser = uuid::Uuid::new_v4().to_string();
            claim(loser.clone(), fingerprint.clone()).await;
            assert!(
                fixture
                    .store
                    .activate_media_session(&activation(&loser, &admitted))
                    .await
                    .expect("loser activation")
                    .is_none(),
                "a storm's loser cannot take the pointer back from the successor"
            );
        }

        /// The ownership latch's own arms, driven directly.
        ///
        /// Through the handler these are unreachable: the settled-destination
        /// gate answers first, so a request for a different anchor is refused
        /// before the latch ever sees it. The latch still has to be right —
        /// the two guards mask each other, and a bug in either would hide
        /// behind the other — so this drives `warm_vtt_window_with` itself.
        #[tokio::test]
        async fn the_owner_latch_joins_refuses_and_displaces_by_authority() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let started = Arc::new(tokio::sync::Semaphore::new(0));
            let hold = Arc::new(tokio::sync::Semaphore::new(0));

            let warm = |sequence: Option<u64>, anchor: i64| {
                let runs = Arc::clone(&runs);
                let started = Arc::clone(&started);
                let hold = Arc::clone(&hold);
                let subs_dir = fixture.state.subs_dir.clone();
                let file = file.clone();
                let session_id = session_id.clone();
                async move {
                    crate::subtitles::warm_vtt_window_with(
                        &session_id,
                        sequence,
                        &subs_dir,
                        &file,
                        0,
                        anchor,
                        WINDOW_SECONDS,
                        move |_tmp, _, _, _, _| async move {
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            started.add_permits(1);
                            hold.acquire().await.expect("hold").forget();
                            Ok(())
                        },
                    )
                    .await
                }
            };
            let count = || runs.load(std::sync::atomic::Ordering::SeqCst);
            let running = || {
                let started = Arc::clone(&started);
                async move {
                    tokio::time::timeout(Duration::from_secs(5), started.acquire())
                        .await
                        .expect("a producer starts")
                        .expect("started semaphore remains open")
                        .forget();
                }
            };

            assert!(warm(Some(5), 600).await, "a first claim starts a flight");
            running().await;
            assert_eq!(count(), 1);

            assert!(
                warm(Some(5), 600).await,
                "the same destination joins the flight already running"
            );
            assert_eq!(count(), 1, "and never spawns a second extractor for it");

            assert!(
                !warm(Some(5), 900).await,
                "an equal sequence is not a newer ordering fact"
            );
            assert!(
                !warm(Some(4), 900).await,
                "and an older one is evidence in the wrong direction"
            );
            assert!(
                !warm(None, 900).await,
                "nor does the absence of one displace a flight that has it"
            );
            assert_eq!(count(), 1, "none of those started anything");
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the flight is kept")
                    .0,
                600
            );

            assert!(warm(Some(6), 900).await, "a newer sequence displaces");
            running().await;
            assert_eq!(count(), 2);
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the successor owns the slot")
                    .0,
                900
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1,
                "the predecessor settled before the successor started"
            );

            // Session end fences the id: a request already on its way cannot
            // leave an extraction behind a playback that is gone.
            crate::subtitles::release_session_window(session_id).await;
            assert!(crate::subtitles::owned_window_for_test(session_id).is_none());
            assert!(
                !warm(Some(9), 1_200).await,
                "a released session starts nothing, however new its authority"
            );
            assert_eq!(count(), 2);

            crate::subtitles::clear_release_fence_for_test(session_id);
            assert!(
                warm(Some(9), 1_200).await,
                "and gets its bridge back once the fence lapses"
            );
            running().await;
            assert_eq!(count(), 3);

            hold.add_permits(8);
            crate::subtitles::release_session_window(session_id).await;
        }

        /// Abandoning a window stops the flight and leaves the session able to
        /// start the next one; releasing stops it and fences the id.
        ///
        /// That difference is the whole reason the second operation exists.
        /// Reattachment moves a viewer to a different rendition under the same
        /// session id, so the flight they left is extracting a span for the
        /// recipe they moved off and is worth stopping — but the id belongs to
        /// somebody still watching. Releasing it fences that id for
        /// `WINDOW_RELEASE_FENCE`, which would refuse the first window of the
        /// attachment that just replaced it: a recipe change costing half a
        /// minute of subtitles. Reaching for `release_session_window` there
        /// looks like the tidy fix and is a regression.
        ///
        /// The named flight matters too. A successor claiming the session
        /// between the read and the stop carries a different id and must
        /// survive, or a viewer who reattaches twice quickly loses the second
        /// attachment's work to the first one's cleanup.
        #[tokio::test]
        async fn abandoning_a_window_stops_the_flight_without_fencing_the_session() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let started = Arc::new(tokio::sync::Semaphore::new(0));
            let hold = Arc::new(tokio::sync::Semaphore::new(0));

            let warm = |sequence: Option<u64>, anchor: i64| {
                let started = Arc::clone(&started);
                let hold = Arc::clone(&hold);
                let subs_dir = fixture.state.subs_dir.clone();
                let file = file.clone();
                let session_id = session_id.clone();
                async move {
                    crate::subtitles::warm_vtt_window_with(
                        &session_id,
                        sequence,
                        &subs_dir,
                        &file,
                        0,
                        anchor,
                        WINDOW_SECONDS,
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
                        .expect("a producer starts")
                        .expect("started semaphore remains open")
                        .forget();
                }
            };

            assert!(warm(Some(5), 600).await, "a first claim starts a flight");
            running().await;
            let flight = crate::subtitles::session_window_flight(session_id)
                .expect("the flight is named while it is live");

            // A stale identity names nothing: the live flight is untouched.
            crate::subtitles::abandon_session_window(session_id, flight.wrapping_add(1)).await;
            assert_eq!(
                crate::subtitles::session_window_flight(session_id),
                Some(flight),
                "an identity that is not this flight's stops nothing"
            );

            crate::subtitles::abandon_session_window(session_id, flight).await;
            assert!(
                crate::subtitles::owned_window_for_test(session_id).is_none(),
                "the named flight is stopped and settled"
            );

            // The difference from release, stated as the viewer experiences
            // it: the very next window is admitted rather than refused for
            // half a minute.
            assert!(
                warm(Some(6), 900).await,
                "abandoning does not fence the session that is still watching"
            );
            running().await;
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the replacement owns the slot")
                    .0,
                900
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1,
                "and the abandoned flight settled before the replacement started"
            );

            hold.add_permits(8);
            crate::subtitles::release_session_window(session_id).await;
        }

        /// A first play with no authority is displaced by the first request
        /// that has one — a settled destination is a fact, and the anchor a
        /// session happened to open on is not.
        #[tokio::test]
        async fn an_ordering_fact_displaces_a_flight_started_without_one() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hold = Arc::new(tokio::sync::Semaphore::new(0));
            let warm = |sequence: Option<u64>, anchor: i64| {
                let runs = Arc::clone(&runs);
                let hold = Arc::clone(&hold);
                let subs_dir = fixture.state.subs_dir.clone();
                let file = file.clone();
                let session_id = session_id.clone();
                async move {
                    crate::subtitles::warm_vtt_window_with(
                        &session_id,
                        sequence,
                        &subs_dir,
                        &file,
                        0,
                        anchor,
                        WINDOW_SECONDS,
                        move |_tmp, _, _, _, _| async move {
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            hold.acquire().await.expect("hold").forget();
                            Ok(())
                        },
                    )
                    .await
                }
            };

            assert!(warm(None, 300).await);
            assert!(warm(Some(1), 600).await, "an authority outranks none");
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the successor owns the slot")
                    .0,
                600
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1
            );

            hold.add_permits(4);
            crate::subtitles::release_session_window(session_id).await;
        }

        /// Terminal retirement releases the owner through the production path,
        /// not through a test calling the release function by hand.
        #[tokio::test]
        async fn rolling_retirement_releases_the_window_a_playback_owned() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, _file) = cold_windowed_fixture(dir.path(), session_id).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            let response = subtitle_segment(&fixture.state, session_id, 3, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;
            assert!(crate::subtitles::owned_window_for_test(session_id).is_some());

            assert!(
                fixture
                    .state
                    .transcode
                    .stop_session(session_id, "admin")
                    .await,
                "the fixture session retires"
            );
            assert!(
                crate::subtitles::owned_window_for_test(session_id).is_none(),
                "terminal retirement released the window owner"
            );
            assert_eq!(
                source.live_window_producers(),
                0,
                "and waited for its extraction to settle"
            );

            source.whole_release.add_permits(4);
        }

        /// The coverage rule, at its edges.
        ///
        /// A window is refused when the viewer has left it behind or when it is
        /// too far ahead to be a buffer head. Between those, including the
        /// window immediately ahead of the playhead, it is exactly the bridge
        /// this feature exists to build.
        #[test]
        fn coverage_admits_the_look_ahead_and_refuses_both_ends() {
            let settled = crate::playback_control::SettledTarget {
                sequence: 4,
                anchor_ms: 190_000,
            };
            assert!(
                settled.covered_by_window(0, 200),
                "the window holding the playhead"
            );
            assert!(
                settled.covered_by_window(200, 200),
                "and the one immediately ahead of it, which is what a player is fetching"
            );
            assert!(
                !settled.covered_by_window(1_800, 200),
                "a window half an hour ahead is a destination the viewer left, not a buffer"
            );
            assert!(
                !settled.covered_by_window(0, 30),
                "a short span behind the playhead has nothing left to give it"
            );
            assert!(
                settled.covered_by_window(180, 30),
                "while the short span holding it still does"
            );
            assert!(
                settled.covered_by_window(300, 30),
                "and a short span keeps the measured client lead, not its own length"
            );
            let head = crate::playback_control::SettledTarget {
                sequence: 1,
                anchor_ms: 0,
            };
            assert!(head.covered_by_window(0, 200), "a first play at the head");
        }

        /// Fact 7. `None` is the absence of an ordering fact, not evidence of
        /// staleness, so a client that has never exchanged still gets its
        /// bridge.
        #[tokio::test]
        async fn a_first_play_with_no_settled_target_still_warms() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());
            assert!(
                fixture
                    .state
                    .transcode
                    .settled_target_for_session(session_id)
                    .await
                    .is_none(),
                "the fixture session has never exchanged"
            );

            let response = subtitle_segment(&fixture.state, session_id, 2, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;
            assert_eq!(source.window_runs(), 1);
            let owned = crate::subtitles::owned_window_for_test(session_id)
                .expect("the first play owns its flight");
            assert_eq!(owned.2, None, "with no ordering fact behind it");

            source.release.add_permits(2);
            source.whole_release.add_permits(2);
            crate::subtitles::release_session_window(session_id).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }

        /// The deterministic core: three anchors traversed, a same-anchor
        /// request joining rather than spawning, a different anchor with no
        /// newer authority refused, and the slot gone at session end.
        #[tokio::test]
        async fn one_session_traverses_anchors_joins_refuses_and_releases() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client = uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &generation,
                "test-node",
            )
            .await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());
            let window_seconds = fixture.state.subtitle_window_seconds().await;

            let mut sequence = 0;
            for segment in [1_i64, 4, 9] {
                sequence += 1;
                settle_on(
                    &fixture,
                    session_id,
                    &generation,
                    &client,
                    sequence,
                    segment * SEGMENT_SECONDS,
                )
                .await
                .expect("accepted exchange");
                let response =
                    subtitle_segment(&fixture.state, session_id, segment, source.as_ref()).await;
                assert_eq!(response.status(), StatusCode::OK);
                producer_started(source.as_ref()).await;
                let owned = crate::subtitles::owned_window_for_test(session_id)
                    .expect("the current destination is owned");
                assert_eq!(owned.0, segment * SEGMENT_SECONDS);
                assert_eq!(source.live_window_producers(), 1);
                assert_eq!(source.window_runs() as u64, sequence);
                tokio::time::sleep(Duration::from_millis(260)).await;
            }

            // Same anchor, same destination: joined, never respawned.
            let response = subtitle_segment(&fixture.state, session_id, 9, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                source.window_runs(),
                3,
                "a second request for the live destination joins it"
            );

            // A different anchor with no newer exchange behind it: the live
            // flight is not evidence-free to kill, so nothing starts.
            let response = subtitle_segment(&fixture.state, session_id, 2, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                source.window_runs(),
                3,
                "a different anchor without newer authority starts nothing"
            );
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the live flight is kept")
                    .0,
                9 * SEGMENT_SECONDS
            );

            // Session end releases the owner, and does it by waiting for the
            // real producer rather than by dropping a handle.
            crate::subtitles::release_session_window(session_id).await;
            assert!(crate::subtitles::owned_window_for_test(session_id).is_none());
            assert_eq!(
                source.live_window_producers(),
                0,
                "release waits for the extraction to settle"
            );
            assert!(
                matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        9 * SEGMENT_SECONDS,
                        window_seconds,
                    )
                    .await,
                    Ok(None)
                ),
                "a released flight publishes nothing"
            );

            source.whole_release.add_permits(2);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }

        /// The window a settled client cannot reach is never started at all —
        /// the refusal happens in the handler, before the owner is consulted.
        #[tokio::test]
        async fn a_window_that_misses_the_settled_target_starts_nothing() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client = uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &generation,
                "test-node",
            )
            .await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            settle_on(
                &fixture,
                session_id,
                &generation,
                &client,
                1,
                12 * SEGMENT_SECONDS,
            )
            .await
            .expect("accepted exchange");
            let response = subtitle_segment(&fixture.state, session_id, 1, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = response
                .into_body()
                .collect()
                .await
                .expect("empty VTT body")
                .to_bytes();
            assert_eq!(
                body.as_ref(),
                b"WEBVTT\n\n",
                "the unchanged empty fallback still answers"
            );
            assert_eq!(
                source.window_runs(),
                0,
                "no window is started for a destination the client already left"
            );
            assert!(crate::subtitles::owned_window_for_test(session_id).is_none());

            source.whole_release.add_permits(2);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }
        /// The last second of a warm, which is the common case at a window
        /// boundary: the previous segment's request already kicked this window,
        /// and this one arrives while it is finishing. Standing still for the
        /// tail of a flight that is *already running* turns an empty segment into
        /// real cues without starting anything.
        ///
        /// The bound is the point. A request that waits longer than
        /// `SUBTITLE_SEGMENT_PUBLICATION_WAIT` is a request AVPlayer has already
        /// given up on, with the muxed video stalled behind it.
        #[tokio::test]
        async fn a_subtitle_segment_waits_out_a_live_window_and_no_longer() {
            let dir = crate::test_tempdir().expect("session directory");
            // A fresh id per test: the ownership, warmup and memo registries
            // are process-global, so a fixed id is a collision waiting for the
            // day two of these run in one binary.
            let wait_session = &uuid::Uuid::new_v4().to_string();
            let (fixture, _file) = cold_windowed_fixture(dir.path(), wait_session).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            // First request: nothing is live yet, so it answers immediately and
            // leaves a flight behind it.
            let cold = subtitle_segment(&fixture.state, wait_session, 1, source.as_ref()).await;
            assert_eq!(cold.status(), StatusCode::OK);
            assert_eq!(
                cold.into_body()
                    .collect()
                    .await
                    .expect("cold body")
                    .to_bytes()
                    .as_ref(),
                b"WEBVTT\n\n",
                "a request that finds no live flight does not stand still for one"
            );
            producer_started(source.as_ref()).await;

            // Second request for the same anchor, with the producer released a
            // moment later: the wait catches the publication.
            let waiting = tokio::spawn({
                let state = fixture.state.clone();
                let source = Arc::clone(&source);
                let session = wait_session.clone();
                async move { subtitle_segment(&state, &session, 1, source.as_ref()).await }
            });
            tokio::time::sleep(Duration::from_millis(120)).await;
            source.release.add_permits(1);
            let served = tokio::time::timeout(Duration::from_secs(3), waiting)
                .await
                .expect("the waiting request settles")
                .expect("request task");
            assert_eq!(served.status(), StatusCode::OK);
            let body = String::from_utf8(
                served
                    .into_body()
                    .collect()
                    .await
                    .expect("served body")
                    .to_bytes()
                    .to_vec(),
            )
            .expect("utf8 VTT");
            assert!(
                body.contains("ready cue"),
                "a window that publishes inside the wait is served as real cues: {body}"
            );
            assert_eq!(
                source.window_runs(),
                1,
                "waiting for a live flight starts no second extraction"
            );
            crate::subtitles::release_session_window(wait_session).await;
        }

        /// The other half of the bound: a live flight that does *not* publish is
        /// abandoned, and the empty segment goes out inside the engine's budget.
        #[tokio::test]
        async fn a_live_window_that_does_not_publish_still_answers_inside_the_wait() {
            let dir = crate::test_tempdir().expect("session directory");
            let timeout_session = &uuid::Uuid::new_v4().to_string();
            let (fixture, _file) = cold_windowed_fixture(dir.path(), timeout_session).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            let cold = subtitle_segment(&fixture.state, timeout_session, 1, source.as_ref()).await;
            assert_eq!(cold.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;

            // The producer stays parked for the whole of this request.
            let began = std::time::Instant::now();
            let response = subtitle_segment(&fixture.state, timeout_session, 1, source.as_ref()).await;
            let waited = began.elapsed();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .expect("empty body")
                    .to_bytes()
                    .as_ref(),
                b"WEBVTT\n\n"
            );
            // Both bounds. The ceiling alone passes with the wait deleted,
            // and "it did not wait at all" is precisely what this test is
            // named after not happening. The floor proves the live flight was
            // waited on; the ceiling is AVPlayer's constraint.
            assert!(
                waited >= SUBTITLE_SEGMENT_PUBLICATION_WAIT - SUBTITLE_SEGMENT_PUBLICATION_POLL,
                "a live flight must be waited on, not skipped: waited {waited:?}"
            );
            assert!(
                waited < SUBTITLE_SEGMENT_PUBLICATION_WAIT + Duration::from_millis(600),
                "a subtitle segment must answer inside AVPlayer's patience, waited {waited:?}"
            );
            // The fixture producer is parked on a zero-permit semaphore, so
            // without this its flight and warmup entry outlive the test in the
            // process-global registries.
            crate::subtitles::release_session_window(timeout_session).await;
        }

        /// An empty segment says "there are no cues here". While a sidecar is
        /// warming that is true; once its extraction has failed it is a lie, and
        /// the player keeps the bytes whatever `no-store` says.
        ///
        /// Off by default, because whether each engine keeps its picture through
        /// a subtitle refusal is a device measurement nobody has taken — so the
        /// shipped answer is still the empty segment, and the switch is what an
        /// operator who *has* taken it turns on.
        #[tokio::test]
        async fn a_failed_subtitle_extraction_is_refused_only_when_the_operator_asked() {
            let dir = crate::test_tempdir().expect("session directory");
            let memo_session = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), memo_session).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            crate::subtitles::remember_whole_track_failure_for_test(
                &fixture.state.subs_dir,
                &file,
                0,
                "the source could not be read",
                Duration::from_secs(90),
            )
            .await;

            let default_off = subtitle_segment(&fixture.state, memo_session, 1, source.as_ref()).await;
            assert_eq!(
                default_off.status(),
                StatusCode::OK,
                "the shipped default keeps the empty segment"
            );
            assert_eq!(
                default_off
                    .into_body()
                    .collect()
                    .await
                    .expect("empty body")
                    .to_bytes()
                    .as_ref(),
                b"WEBVTT\n\n"
            );

            fixture
                .store
                .put_setting(plurx_core::store::keys::SUBTITLE_NOT_READY_503, "1")
                .await
                .expect("the operator turns the refusal on");

            let refused = subtitle_segment(&fixture.state, memo_session, 2, source.as_ref()).await;
            assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
            let retry_after = refused
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .expect("a refusal says when a retry could achieve anything");
            assert!(
                (80..=90).contains(&retry_after),
                "Retry-After is the memo's own remaining time, not a constant: got {retry_after}"
            );
            crate::subtitles::release_session_window(memo_session).await;
            crate::subtitles::forget_whole_track_failure_for_test(&fixture.state.subs_dir, &file, 0)
                .await;
        }
    }

    #[tokio::test]
    async fn published_subtitle_playlist_refuses_in_place_video_attempt_handoff() {
        let dir = crate::test_tempdir().expect("session directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "subtitle-handoff").await;
        add_http_text_subtitle(&mut fixture, "subtitle-handoff").await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture
            .state
            .transcode
            .set_subtitle_playlist_commit_pause(Arc::clone(&pause));
        let owner_pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture.pause_playlist_publication(Arc::clone(&owner_pause));
        let mut predecessor = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:4\n");
        for index in 0..12 {
            let name = format!("seg{index:05}.ts");
            tokio::fs::write(dir.path().join(&name), format!("old-{index}"))
                .await
                .expect("predecessor segment");
            predecessor.push_str(&format!("#EXTINF:4.000,\n{name}\n"));
        }
        tokio::fs::write(dir.path().join("index.m3u8"), predecessor)
            .await
            .expect("predecessor playlist");
        let state = fixture.state.clone();
        let waiting =
            tokio::spawn(async move { subtitle_playlist_local(&state, "subtitle-handoff", 0).await });
        tokio::time::timeout(Duration::from_secs(5), owner_pause.wait())
            .await
            .expect("subtitle request read predecessor playlist");

        assert_eq!(
            fixture.begin_producer_attempt().await,
            Err(crate::playback_control::ProducerAttemptRejection::PlaylistPublished),
            "published media permanently closes in-place producer replacement"
        );
        tokio::time::timeout(Duration::from_secs(5), owner_pause.wait())
            .await
            .expect("release predecessor playlist publication");
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("subtitle response reached the exact-owner commit seam");
        assert!(
            !waiting.is_finished(),
            "subtitle response reached the exact-owner commit seam"
        );
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("release subtitle response commit");
        let response = waiting
            .await
            .expect("subtitle task")
            .expect("subtitle response commits against its published owner");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("subtitle playlist body");
        let text = String::from_utf8(body.to_vec()).expect("subtitle playlist text");
        assert!(
            text.lines().any(|line| line == "seg00011.vtt"),
            "subtitle child playlist retains its published owner: {text}"
        );
        assert_eq!(fixture.last_renewal_kind().await, "subtitle-playlist");
    }

    #[tokio::test]
    async fn real_subtitle_playlist_cannot_commit_after_vod_same_id_reattachment() {
        let dir = crate::test_tempdir().expect("VOD subtitle directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        add_http_text_subtitle(&mut fixture, "rolling-unused").await;
        let session_id = "vod-subtitle-replaced";
        let _predecessor = install_vod_http_session(&fixture, dir.path(), session_id).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture
            .state
            .transcode
            .set_subtitle_playlist_commit_pause(Arc::clone(&pause));
        let state = fixture.state.clone();
        let pending = tokio::spawn(async move { subtitle_playlist_local(&state, session_id, 0).await });
        pause.wait().await;

        let _successor = install_vod_http_session(&fixture, dir.path(), session_id).await;
        let successor_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(session_id)
            .await
            .expect("successor touch");
        pause.wait().await;

        assert!(
            pending.await.expect("subtitle task").is_err(),
            "predecessor bytes must fail their exact-owner commit"
        );
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(session_id)
                .await,
            Some(successor_touch),
            "stale subtitle bytes cannot renew the same-id successor"
        );
    }

    /// A route whose durable recipe the takeover path accepts. The fixture is
    /// `media_sessions`' own, so "eligible" cannot mean two things.
    fn eligible_owner_loss_route() -> MediaSessionRoute {
        let mut route = crate::media_sessions::takeover_eligible_route(
            "00000000-0000-4000-8000-0000000000e2",
            "00000000-0000-4000-8000-0000000000e1",
        );
        route.owner_node_id = "node-gone".to_owned();
        route.owner_epoch = 3;
        route.lease_expires_at_ms = NOW_MS;
        route.updated_at_ms = NOW_MS;
        route.media_origin_ms = 90_000;
        route.fetched_through_ms = 60_000;
        route.produced_playable_through_ms = 120_000;
        route
    }

    /// The same route with a recipe nothing will ever take over.
    fn untakeoverable_owner_loss_route() -> MediaSessionRoute {
        let mut route = eligible_owner_loss_route();
        route.recipe_json = route
            .recipe_json
            .replace("\"typeless_playlist\":true", "\"typeless_playlist\":false");
        assert!(
            route.recipe_json.contains("\"typeless_playlist\":false"),
            "the EVENT fixture must actually clear the flag"
        );
        route
    }

    async fn error_body(error: ApiError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let http_status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect error body")
            .to_bytes();
        (
            http_status,
            serde_json::from_slice(&bytes).expect("typed errors are JSON objects"),
        )
    }

    /// A route a successor can still take over keeps the retryable 503 it has
    /// always had — but it now says where to reopen if the client stops
    /// waiting, and that recovery carries a discontinuity either way.
    #[tokio::test]
    async fn an_eligible_route_stays_retryable_and_still_says_where_to_reopen() {
        let route = eligible_owner_loss_route();
        let (status, body) = error_body(owner_transition_answer(&route)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "media_owner_transition");
        assert_eq!(
            body["reopen_required"], false,
            "a client that is willing to wait should keep waiting"
        );
        assert_eq!(body["continuous"], false);
        assert!(body["film_position_ms"].is_i64());
    }

    /// Hard owner loss the takeover path can never answer (plan §10.3). The
    /// client is told the session is gone rather than to wait for an event
    /// that cannot happen, and is handed where to reopen.
    #[tokio::test]
    async fn an_untakeoverable_route_answers_gone_with_the_resume_and_no_continuity_claim() {
        // The EVENT case: nothing will ever replace this session.
        let route = untakeoverable_owner_loss_route();
        let (status, body) = error_body(owner_transition_answer(&route)).await;

        assert_eq!(
            status,
            StatusCode::GONE,
            "a session no node will serve again is gone, not temporarily unavailable"
        );
        assert_eq!(body["code"], "media_owner_lost");
        assert_eq!(body["reopen_required"], true);
        assert_eq!(
            body["continuous"], false,
            "§10.3 forbids guessing transparency, and the client reads the field, not the prose"
        );
        // 60 s fetched, pulled back by one transcode segment plus the
        // overlap margin, on a session whose origin is 90 s into the film.
        let overlap_ms = i64::from(plurx_core::transcode::SEGMENT_SECONDS) * 1_000 + 2_000;
        assert_eq!(body["film_position_ms"], 90_000 + 60_000 - overlap_ms);
        assert_eq!(body["film_frontier_ms"], 90_000 + 120_000);
    }

    /// The wiring, not the helper. Every public media path that answers an
    /// owner transition must go through the classifier — a call site left on
    /// the old unconditional 503 would make this fail while every unit test
    /// on the helper still passed.
    #[tokio::test]
    async fn a_public_status_read_of_an_untakeoverable_route_answers_gone() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "owner-loss-unrelated").await;
        let user = fixture
            .store
            .create_user("owner-loss", "hash", false)
            .await
            .expect("owner-loss user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: incarnation_id.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "owner-loss".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "former-owner".to_owned(),
                // Active, but outside its owner lease: routing calls this a
                // transition, and the recipe decides which kind.
                lease_expires_at_ms: 2,
                // An EVENT session — refused by `attempt_takeover` forever.
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: 1,
            },
        )
        .await;

        let Err(answer) = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await
        else {
            panic!("an expired active route is not local status absence")
        };
        assert!(
            matches!(
                answer,
                ApiError::TypedDetail {
                    status: StatusCode::GONE,
                    code: "media_owner_lost",
                    ..
                }
            ),
            "the public status path must classify rather than always answer 503"
        );
    }

    /// The EVENT half of §4's non-expansion rule, driven through the function
    /// that owns it rather than through the flag it reads.
    #[tokio::test]
    async fn attempt_takeover_refuses_an_event_session() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "event-refusal-unrelated").await;
        let mut route = untakeoverable_owner_loss_route();
        route.owner_node_id = "some-other-node".to_owned();

        let refusal = crate::media_sessions::attempt_takeover_for_test(&fixture.state, route)
            .await
            .expect_err("an EVENT session is never taken over");
        assert!(
            refusal.contains("EVENT playlist"),
            "the EVENT gate must fire before any later refusal: {refusal}"
        );
    }

    /// Takeover will not adopt a live-HLS session while the switch that
    /// governs the live engine is off, and says which switch it was.
    ///
    /// Takeover is the only producer of a `Presentation::Live` recipe —
    /// `takeover_recipe_is_valid` requires that stamp and both public and
    /// worker ingress refuse it — so before this it was the one path that
    /// could put a session on the retained engine with the fallback disabled.
    /// The window is narrow: a session that started while the fallback was on,
    /// kept running when an operator turned it off, and then outlived its
    /// owner. It is also the reason the rule is worth having, because the
    /// setting is replicated and every node agrees about it, so a refusal here
    /// can only ever mean what it says.
    ///
    /// The message is asserted, not only the refusal. "Takeover failed" sends
    /// an operator to look at the node that died; naming the switch sends them
    /// to the one they turned off.
    #[tokio::test]
    async fn attempt_takeover_refuses_a_live_session_while_the_fallback_is_off() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "fallback-off-unrelated").await;
        let route = || {
            let mut route = eligible_owner_loss_route();
            route.owner_node_id = "some-other-node".to_owned();
            route
        };

        // With the fallback on, this fixture gets past the switch: whatever it
        // is refused for next, it is not this. Without that half the test
        // would pass against a build that refuses every takeover.
        let allowed = crate::media_sessions::attempt_takeover_for_test(&fixture.state, route())
            .await
            .err()
            .unwrap_or_default();
        assert!(
            !allowed.contains("live HLS fallback"),
            "the switch is on, so it cannot be the reason: {allowed}"
        );

        for stored in ["0", "false", " OFF ", "no"] {
            fixture
                .store
                .put_setting(plurx_core::store::keys::VOD_LIVE_RECOVERY, stored)
                .await
                .expect("disable the fallback");
            let refusal = crate::media_sessions::attempt_takeover_for_test(&fixture.state, route())
                .await
                .expect_err("a live session is not adopted while the engine is disabled");
            assert!(
                refusal.contains("live HLS fallback is disabled cluster-wide"),
                "{stored:?} must refuse and name the switch: {refusal}"
            );
        }
    }

    async fn control_body(response: Response) -> (StatusCode, serde_json::Value) {
        let http_status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect control body")
            .to_bytes();
        (
            http_status,
            serde_json::from_slice(&bytes).expect("control errors are JSON objects"),
        )
    }

    fn control_request(generation: String) -> crate::playback_control::ControlRequestV1 {
        crate::playback_control::ControlRequestV1 {
            intent: None,
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation,
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: crate::playback_control::PlaybackDemand::Active,
            position_ms: 1_000,
            buffered_from_ms: Some(0),
            buffered_through_ms: 10_000,
            playback_rate: 1.0,
            render_state: crate::playback_control::RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: crate::playback_control::ClientSelection {
                quality: crate::playback_control::QualitySelection::Auto { height: None },
                audio_track: None,
                subtitle: crate::playback_control::SubtitleSelection {
                    mode: crate::playback_control::SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: crate::playback_control::CodecPolicy::Auto,
                dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
            },
            capabilities: Some(crate::playback_control::DynamicCapabilities {
                platform: crate::playback_control::ClientPlatform::Web,
                max_height: 1080,
                codecs: vec![crate::playback_control::CodecPolicy::H264],
                dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: None,
        }
    }

    /// A live local worker and a durable route naming it — both halves are
    /// needed, because staging takes the actor's slot *and* writes a row
    /// fenced on the route's current generation.
    async fn staging_fixture(dir: &std::path::Path) -> (HlsDeliveryFixture, String, MediaSessionRoute) {
        staging_fixture_for_playback(dir, "stage-player").await
    }

    /// A playback id nothing else in the process shares.
    ///
    /// The pending-candidate map and the active-successor registry are both
    /// process-global and keyed by playback id alone, while `staging_fixture`'s
    /// own `stage-player` is shared by every test that uses it. A test that read
    /// either map under that name would be reading other tests' work, and would
    /// pass or fail on whatever else happened to be running.
    fn unique_playback_id(label: &str) -> String {
        format!("{label}-{}", uuid::Uuid::new_v4())
    }

    async fn staging_fixture_for_playback(
        dir: &std::path::Path,
        playback_id: &str,
    ) -> (HlsDeliveryFixture, String, MediaSessionRoute) {
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir, &session_id).await;
        let user = fixture
            .store
            .create_user("stage-on-prepare", "hash", false)
            .await
            .expect("staging user");
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let predecessor_request = crate::transcode::SessionRequest {
            control_sequence: None,
            file_id: fixture.file_id(),
            playback_id: playback_id.to_owned(),
            request_id: Some(incarnation_id.clone()),
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: crate::transcode::SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        let predecessor_recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation_id.clone(),
            user_id: user.id,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: false,
            library_channel: None,
            request: predecessor_request.clone(),
        };
        let predecessor_start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(6_000_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 2160,
            encoder: "test".to_owned(),
            vod: false,
            ladder: Vec::new(),
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &incarnation_id,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id,
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: playback_id.to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: predecessor_request.durable_intent_fingerprint(user.id),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: unix_ms().saturating_add(900_000),
                recipe_json: serde_json::to_string(&predecessor_recipe).expect("recipe"),
                response_json: serde_json::to_string(&predecessor_start).expect("response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;
        (fixture, session_id, route)
    }
