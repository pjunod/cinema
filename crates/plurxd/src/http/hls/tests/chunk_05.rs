
    #[tokio::test(start_paused = true)]
    async fn started_session_guard_holds_replacement_gate_until_cleanup_settles() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "guard-lifetime").await;
        let request = crate::transcode::SessionRequest {
            control_sequence: None,
            file_id: 1,
            playback_id: "guard-lifetime-player".to_owned(),
            request_id: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: crate::transcode::SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Live,
            block_budget_secs: None,
            transport: None,
        };
        let replacement = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("replacement gate");
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (released_tx, released_rx) = tokio::sync::oneshot::channel();
        let mut guard = StartedSessionGuard::new(
            fixture.state.clone(),
            fixture.state.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
            "guard-lifetime".to_owned(),
            7,
            "guard-lifetime-request".to_owned(),
            Some(replacement),
        );
        guard.hold_cleanup_for_test(settled_tx, release_rx, released_tx);
        drop(guard);
        settled_rx
            .await
            .expect("cleanup reached its settlement seam");

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let blocked = tokio::spawn({
            let state = fixture.state.clone();
            let request = request.clone();
            async move {
                let blocked = tokio::time::timeout(
                    Duration::from_secs(1),
                    state.transcode.acquire_cluster_takeover_replacement(
                        &request,
                        7,
                        tokio::time::Instant::now() + Duration::from_secs(10),
                    ),
                );
                tokio::pin!(blocked);
                let mut entered_tx = Some(entered_tx);
                std::future::poll_fn(|context| {
                    let result = std::future::Future::poll(blocked.as_mut(), context);
                    if result.is_pending() {
                        if let Some(entered_tx) = entered_tx.take() {
                            let _ = entered_tx.send(());
                        }
                    }
                    result
                })
                .await
            }
        });
        entered_rx
            .await
            .expect("replacement waiter registered behind the cleanup-owned gate");
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            blocked.await.expect("replacement waiter task").is_err(),
            "cleanup must retain the replacement gate"
        );

        release_tx.send(()).expect("cleanup release");
        released_rx
            .await
            .expect("replacement guard was dropped after cleanup settlement");
        let reacquired = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("cleanup settlement releases the replacement gate");
        drop(reacquired);

        let replacement = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("replacement gate for disarm");
        let mut disarmed = StartedSessionGuard::new(
            fixture.state.clone(),
            fixture.state.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
            "guard-lifetime".to_owned(),
            7,
            "guard-disarm-request".to_owned(),
            Some(replacement),
        );
        disarmed.disarm();
        let reacquired = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("disarm releases the replacement gate synchronously");
        drop(reacquired);
    }

    #[tokio::test]
    async fn durable_activation_commit_cannot_straddle_serving_loss() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let authority = fence.authority();
        let generation = authority.admit().expect("initial authority");
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut commit = tokio::spawn({
            let authority = authority.clone();
            let entered = Arc::clone(&entered);
            async move {
                let transition = authority
                    .commit_guard_before(
                        generation,
                        std::time::Instant::now() + Duration::from_secs(5),
                    )
                    .await
                    .ok_or_else(|| {
                        ApiError::ServiceUnavailable(
                            "authority admitted commit could not acquire transition".to_owned(),
                        )
                    })?;
                entered.wait().await;
                release_rx.await.expect("release activation commit");
                Ok::<_, ApiError>((7_u8, transition))
            }
        });
        entered.wait().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut commit)
                .await
                .is_err(),
            "the HTTP wait may expire without cancelling the commit owner"
        );
        let loss = tokio::spawn(async move { fence.validation_set_ready(false).await });
        tokio::task::yield_now().await;
        assert!(
            !loss.is_finished(),
            "serving loss waits until the bounded activation commit ends"
        );
        release_tx.send(()).expect("release commit");
        let (result, transition) = commit
            .await
            .expect("commit task")
            .expect("authority admitted commit");
        assert_eq!(result, 7);
        drop(transition);
        loss.await.expect("serving loss task");

        let stale = authority
            .commit_guard_before(
                generation,
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .await;
        assert!(
            stale.is_none(),
            "a stale activation cannot acquire a commit guard"
        );
    }

    #[tokio::test]
    async fn replayed_start_guard_owns_neither_worker_nor_original_claim() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = "recovered-start-worker";
        let fixture = HlsDeliveryFixture::publish(dir.path(), session_id).await;
        let user = fixture
            .state
            .store
            .create_user("replayed-guard", "hash", false)
            .await
            .expect("create guard user");
        let request_id = "replayed-guard-request";
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let fingerprint = "a".repeat(64);
        let now_ms = unix_ms();
        assert!(matches!(
            fixture
                .state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "replayed-guard-player",
                    &incarnation_id,
                    now_ms,
                    now_ms.saturating_add(60_000),
                )
                .await
                .expect("claim original request"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        let guard = StartedSessionGuard::replayed(
            fixture.state.clone(),
            fixture.state.node_id.clone(),
            incarnation_id.clone(),
            session_id.to_owned(),
            user.id,
            request_id.to_owned(),
            None,
        );

        drop(guard);
        tokio::task::yield_now().await;
        assert!(
            fixture.worker_is_registered(session_id).await,
            "a duplicate start never owns the recovered worker"
        );
        let retry_incarnation = uuid::Uuid::new_v4().to_string();
        let retry_now_ms = unix_ms();
        assert!(matches!(
            fixture
                .state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "replayed-guard-player",
                    &retry_incarnation,
                    retry_now_ms,
                    retry_now_ms.saturating_add(60_000),
                )
                .await
                .expect("inspect original claim"),
            MediaSessionRequestClaim::InFlight {
                incarnation_id: active,
                ..
            } if active == incarnation_id
        ));
        assert!(fixture
            .state
            .store
            .fail_media_session_request(user.id, request_id, &incarnation_id, unix_ms())
            .await
            .expect("settle original claim"));
        assert!(
            fixture
                .state
                .transcode
                .stop_session(session_id, "test")
                .await
        );
    }

    #[tokio::test]
    async fn pre_worker_request_guard_releases_an_owned_claim_for_immediate_retry() {
        let (_app, state) = super::super::tests::test_app_with_state();
        let user = state
            .store
            .create_user("request-guard", "hash", false)
            .await
            .expect("create request-guard user");
        let request_id = "request-guard-attempt";
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let retry_incarnation = uuid::Uuid::new_v4().to_string();
        let fingerprint = "b".repeat(64);
        let now_ms = unix_ms();
        assert!(matches!(
            state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "request-guard-player",
                    &incarnation_id,
                    now_ms,
                    now_ms.saturating_add(60_000),
                )
                .await
                .expect("claim guarded request"),
            MediaSessionRequestClaim::Acquired { .. }
        ));

        drop(MediaSessionRequestGuard::new(
            state.clone(),
            user.id,
            request_id.to_owned(),
            incarnation_id,
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let retry_now_ms = unix_ms();
            match state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "request-guard-player",
                    &retry_incarnation,
                    retry_now_ms,
                    retry_now_ms.saturating_add(60_000),
                )
                .await
                .expect("retry guarded request")
            {
                MediaSessionRequestClaim::Acquired { incarnation_id }
                    if incarnation_id == retry_incarnation =>
                {
                    break;
                }
                MediaSessionRequestClaim::InFlight { .. }
                    if tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                other => panic!("guarded claim did not become retryable: {other:?}"),
            }
        }
        assert!(state
            .store
            .fail_media_session_request(user.id, request_id, &retry_incarnation, unix_ms(),)
            .await
            .expect("settle retry claim"));
    }

    // ---- segment delivery, through the real response ------------------------
    //
    // These drive the handler, not `SegmentDelivery`. The correction under
    // test is *where* bytes are counted — at open, or as the response body
    // drains — and a test that calls `note_read` by hand cannot tell those
    // apart.

    /// The correction itself: a segment's bytes are counted as the response
    /// body drains, not when the file opens.
    ///
    /// Counting at open credits a client that fetched a header and then
    /// stalled with a whole segment's worth of throughput, which is precisely
    /// the reading that makes a delivery freeze look like a healthy transfer.
    /// Both halves are pinned: the meter is still at zero when the handler
    /// returns, and reaches the segment's length only once the body is read.
    #[tokio::test]
    async fn segment_bytes_are_counted_as_the_body_drains_not_at_open() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "drain").await;
        fixture.make_segment_window_servable().await;
        let body = vec![7_u8; 12 * 1024];
        tokio::fs::write(dir.path().join("seg00001.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("drain".to_owned(), "seg00001.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(body.len().to_string().as_str())
        );
        assert_eq!(
            fixture.delivered_bytes(),
            0,
            "opening a segment delivers nothing: the client has only been handed a body"
        );

        let mut stream = response.into_body().into_data_stream();
        let mut read = 0_usize;
        while let Some(chunk) = stream.next().await {
            read += chunk.expect("segment chunk").len();
        }
        assert_eq!(read, body.len(), "the whole segment reached the client");
        assert_eq!(
            fixture.delivered_bytes(),
            body.len() as i64,
            "the meter tracks bytes actually read out of the segment"
        );
        assert!(
            fixture.settle().await.is_empty(),
            "a complete delivery is not an incident"
        );
    }

    #[tokio::test]
    async fn completed_segment_eof_does_not_wait_for_a_blocked_producer_transition() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "nonblocking-eof").await;
        fixture.make_segment_window_servable().await;
        let body = vec![11_u8; 12 * 1024];
        tokio::fs::write(dir.path().join("seg00001.m4s"), &body)
            .await
            .expect("segment bytes");
        let response = segment(
            State(fixture.state.clone()),
            AxPath(("nonblocking-eof".to_owned(), "seg00001.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");

        // Model an encoder replacement or hold/resume transition that owns
        // the physical signal gate. EOF may commit lease/frontier state and
        // queue flow work, but must not hold END_STREAM behind this gate.
        let transition = fixture.hold_child_transition().await;
        let delivered = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            axum::body::to_bytes(response.into_body(), body.len() + 1),
        )
        .await
        .expect("response EOF is independent of producer signaling")
        .expect("segment body");
        assert_eq!(delivered.len(), body.len());
        drop(transition);
    }

    async fn install_vod_http_session(
        fixture: &HlsDeliveryFixture,
        base: &std::path::Path,
        session_id: &str,
    ) -> crate::transcode::MediaResponseOwner {
        fixture
            .state
            .transcode
            .install_vod_http_test_session(session_id, fixture.file_id(), base)
            .await;
        let publication = fixture
            .state
            .transcode
            .vod_playlist(session_id)
            .await
            .expect("VOD fixture ownership");
        let _playlist = publication.result.expect("VOD fixture playlist");
        publication.owner
    }

    async fn vod_ready(
        path: &std::path::Path,
        advertised_len: u64,
    ) -> crate::vodserve::SegmentReady {
        vod_ready_metered(
            path,
            advertised_len,
            std::sync::Arc::new(crate::meter::Meter::new()),
        )
        .await
    }

    /// The same answer, against a meter the caller keeps a handle to.
    ///
    /// Production takes this meter from the session the segment was opened
    /// for, so holding it here is what lets a test read the same counter the
    /// control view will publish.
    async fn vod_ready_metered(
        path: &std::path::Path,
        advertised_len: u64,
        delivery: std::sync::Arc<crate::meter::Meter>,
    ) -> crate::vodserve::SegmentReady {
        crate::vodserve::SegmentReady {
            delivery,
            file: tokio::fs::File::open(path)
                .await
                .expect("open VOD response object"),
            len: advertised_len,
            etag: format!("http-test-{advertised_len}"),
        }
    }

    /// P1-3. A VOD body counts the bytes it hands over, where they leave.
    ///
    /// The counter must not move when the segment is merely opened, and must
    /// not move when the response is merely built: it moves as the body is
    /// drained, because that is the only moment a byte has actually reached
    /// this viewer. Without it `DeliveryView` reported `delivered_bps: None`
    /// on every VOD session; the value remains useful telemetry even though it
    /// no longer controls preparation admission.
    #[tokio::test]
    async fn a_vod_body_counts_its_delivered_bytes_where_they_leave() {
        let dir = crate::test_tempdir().expect("VOD HTTP directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        let headers = RelayHeaders::default();
        let session_id = "vod-metered";
        let owner = install_vod_http_session(&fixture, dir.path(), session_id).await;

        let path = dir.path().join("metered.m4s");
        let bytes = vec![7_u8; 24 * 1024];
        tokio::fs::write(&path, &bytes).await.expect("VOD object");

        let delivery = std::sync::Arc::new(crate::meter::Meter::new());
        let response = vod_segment_response(
            &fixture.state,
            session_id,
            "seg00004.m4s",
            &headers,
            vod_ready_metered(&path, bytes.len() as u64, std::sync::Arc::clone(&delivery)).await,
            owner,
        )
        .await
        .expect("VOD response");
        assert_eq!(
            delivery.total_bytes(),
            0,
            "opening a segment and building a response delivers nothing"
        );

        let drained = axum::body::to_bytes(response.into_body(), bytes.len() + 1)
            .await
            .expect("VOD body")
            .len();
        assert_eq!(drained, bytes.len());
        assert_eq!(
            delivery.total_bytes(),
            bytes.len() as i64,
            "every byte the viewer took is counted, and only those"
        );
    }

    /// An abandoned body credits exactly what the client actually took.
    ///
    /// A viewer who walks away mid-segment must not leave behind a rate built
    /// from bytes that went nowhere — that is what would make a stalled link
    /// look fast enough to stage a successor against. The assertion is an
    /// equality rather than an upper bound: "less than the whole segment"
    /// holds for a counter that never moved at all, which is how a version of
    /// this test proved nothing.
    ///
    /// What it does **not** pin is the ordering of the `note` against the
    /// downstream acknowledgement. The local body channel holds one chunk and
    /// the pump awaits each chunk's acknowledgement before reading the next,
    /// so it never reads ahead of what it has sent, and counting at read time
    /// is externally indistinguishable from counting at acknowledgement time.
    /// That ordering is argued from the code, not proved here; pinning it
    /// would need a downstream this test can park mid-chunk.
    #[tokio::test]
    async fn an_abandoned_vod_body_counts_nothing_it_did_not_hand_over() {
        let dir = crate::test_tempdir().expect("VOD HTTP directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        let headers = RelayHeaders::default();
        let session_id = "vod-abandoned";
        let owner = install_vod_http_session(&fixture, dir.path(), session_id).await;

        let path = dir.path().join("abandoned.m4s");
        let bytes = vec![9_u8; 512 * 1024];
        tokio::fs::write(&path, &bytes).await.expect("VOD object");

        let delivery = std::sync::Arc::new(crate::meter::Meter::new());
        let response = vod_segment_response(
            &fixture.state,
            session_id,
            "seg00005.m4s",
            &headers,
            vod_ready_metered(&path, bytes.len() as u64, std::sync::Arc::clone(&delivery)).await,
            owner,
        )
        .await
        .expect("VOD response");

        // Take a prefix and abandon the rest. Asserting only "less than the
        // whole" would hold for a counter that never moved at all, and would
        // also hold if bytes were counted at read time — the exact defect the
        // acknowledgement ordering exists to prevent. The prefix has to be
        // counted *exactly*: everything handed over, nothing that was not.
        let mut body = response.into_body().into_data_stream();
        let mut taken = 0_i64;
        for _ in 0..2 {
            let chunk = futures_util::StreamExt::next(&mut body)
                .await
                .expect("a chunk")
                .expect("chunk bytes");
            taken += chunk.len() as i64;
        }
        drop(body);
        tokio::task::yield_now().await;

        assert!(taken > 0, "the fixture must actually hand over a prefix");
        assert!(
            taken < bytes.len() as i64,
            "the fixture must abandon the body before it completes"
        );
        assert_eq!(
            delivery.total_bytes(),
            taken,
            "exactly the bytes the viewer took — a reader that counted at read \
             time would be ahead of this, and one that never counted behind it"
        );
    }

    /// Decision 1 of docs/streaming/MEDIA-BODY-BUFFERS.md, taken on the §5.1.1
    /// measurement: the shared media read is 128 KiB, and the delivery-proof
    /// unit stays 4 KiB beside it rather than following it.
    ///
    /// The read size is a memory/throughput trade-off, so it is pinned by
    /// value: moving it (back to 256 KiB, or anywhere else) is a new decision
    /// that must go through the measurement again, not a drive-by edit.
    ///
    /// The acknowledgement unit is pinned by value *and* by definition. While
    /// the read is 128 KiB, `4 * 1024` and `MEDIA_BODY_READ_BUFFER / 32` are
    /// the same number, so no value assertion can tell them apart; a unit
    /// derived from the read would pass every value check today and then move
    /// with the next read-size change. So the definition in
    /// `media_sessions.rs` must be a literal byte count that names no other
    /// constant. Re-coupling the two inside the pumps, rather than in the
    /// constants, fails
    /// `a_media_body_is_proved_in_acknowledgement_units_not_storage_read_units`.
    #[test]
    fn the_shared_media_read_is_128_kib_and_the_delivery_proof_stays_4_kib() {
        assert_eq!(
            MEDIA_BODY_READ_BUFFER,
            128 * 1024,
            "the shared media read size is Decision 1's 128 KiB; changing it \
             needs the §5.1 measurement re-run, not just this assertion"
        );
        assert_eq!(
            MEDIA_BODY_ACK_GRANULARITY,
            4 * 1024,
            "the delivery-proof unit is 4 KiB whatever the read size is"
        );
        assert_eq!(
            MEDIA_BODY_READ_BUFFER / MEDIA_BODY_ACK_GRANULARITY,
            32,
            "one storage read is split into 32 acknowledgements; if this ratio \
             is 1 the read size has become the proof granularity again"
        );

        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/media_sessions.rs"
        ));
        let definition = source
            .split_once("pub(crate) const MEDIA_BODY_ACK_GRANULARITY: usize =")
            .and_then(|(_, rest)| rest.split_once(';'))
            .map(|(value, _)| value.trim())
            .expect("MEDIA_BODY_ACK_GRANULARITY is defined in media_sessions.rs");
        assert!(
            !definition.is_empty()
                && definition
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '_' || c == '*' || c.is_whitespace()),
            "MEDIA_BODY_ACK_GRANULARITY must be a literal byte count, not derived \
             from MEDIA_BODY_READ_BUFFER or any other constant; found `{definition}`"
        );
    }

    /// The unit a body is proved delivered in is `MEDIA_BODY_ACK_GRANULARITY`,
    /// and it does not move when `MEDIA_BODY_READ_BUFFER` does.
    ///
    /// The pump acknowledges a chunk, then counts it, then -- at the last byte
    /// -- renews the lease and moves the fetched-segment frontier. So the
    /// chunk size is the resolution of that whole proof: the most a response
    /// can over-credit, and the largest object a single body poll can make
    /// look complete. Raising the storage read to 128 KiB without splitting it
    /// here would raise that resolution 32x and let `init.mp4`, subtitle
    /// segments and audio-only renditions -- every media object at or below
    /// the read size -- commit on one poll from a client that then walked
    /// away. This pins the two numbers apart: the object below is one storage
    /// read and many acknowledgements.
    #[tokio::test]
    async fn a_media_body_is_proved_in_acknowledgement_units_not_storage_read_units() {
        use futures_util::StreamExt;

        const {
            assert!(
                MEDIA_BODY_ACK_GRANULARITY < MEDIA_BODY_READ_BUFFER,
                "the proof granularity is only meaningful while it is finer than the read"
            )
        };

        let dir = crate::test_tempdir().expect("VOD HTTP directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        let headers = RelayHeaders::default();
        let session_id = "vod-ack-granularity";
        let owner = install_vod_http_session(&fixture, dir.path(), session_id).await;

        // One 64 KiB object: comfortably inside a single MEDIA_BODY_READ_BUFFER
        // read, and the exact size class (init/subtitle/audio segment) the
        // coarser proof would have completed on the first poll.
        let path = dir.path().join("granularity.m4s");
        let bytes = vec![5_u8; 64 * 1024];
        tokio::fs::write(&path, &bytes).await.expect("VOD object");
        assert!(bytes.len() <= MEDIA_BODY_READ_BUFFER);

        let delivery = std::sync::Arc::new(crate::meter::Meter::new());
        let response = vod_segment_response(
            &fixture.state,
            session_id,
            "seg00005.m4s",
            &headers,
            vod_ready_metered(&path, bytes.len() as u64, std::sync::Arc::clone(&delivery)).await,
            owner,
        )
        .await
        .expect("VOD response");

        let mut body = response.into_body().into_data_stream();
        let first = body
            .next()
            .await
            .expect("a first chunk")
            .expect("a readable first chunk");
        assert_eq!(
            first.len(),
            MEDIA_BODY_ACK_GRANULARITY,
            "one poll takes one acknowledgement unit, not one storage read"
        );
        drop(body);
        tokio::task::yield_now().await;

        assert_eq!(
            delivery.total_bytes(),
            MEDIA_BODY_ACK_GRANULARITY as i64,
            "and exactly that unit is credited, not the whole object"
        );
        assert_eq!(
            vod_fetched_segment(&fixture, session_id).await,
            None,
            "an object smaller than one storage read still cannot complete on one poll"
        );
    }

    async fn vod_fetched_segment(fixture: &HlsDeliveryFixture, session_id: &str) -> Option<i64> {
        let crate::transcode::HlsSessionInfo::Vod(status) = fixture
            .state
            .transcode
            .hls_session_status(session_id)
            .await
            .expect("VOD fixture status")
        else {
            panic!("fixture was not VOD");
        };
        status.fetched_segment
    }

    #[tokio::test]
    async fn vod_stream_finalizer_commits_only_exact_live_response_bodies() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("VOD HTTP directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        let headers = RelayHeaders::default();

        // Exact EOF: lease and the segment frontier both commit.
        let full_id = "vod-full";
        let full_owner = install_vod_http_session(&fixture, dir.path(), full_id).await;
        let full_path = dir.path().join("full.m4s");
        let full_bytes = vec![1_u8; 24 * 1024];
        tokio::fs::write(&full_path, &full_bytes)
            .await
            .expect("full VOD object");
        let full = vod_segment_response(
            &fixture.state,
            full_id,
            "seg00003.m4s",
            &headers,
            vod_ready(&full_path, full_bytes.len() as u64).await,
            full_owner,
        )
        .await
        .expect("full VOD response");
        assert_eq!(
            axum::body::to_bytes(full.into_body(), full_bytes.len() + 1)
                .await
                .expect("full VOD body")
                .len(),
            full_bytes.len()
        );
        assert_eq!(vod_fetched_segment(&fixture, full_id).await, Some(3));

        // A strict subset Range proves demand and renews the lease, but does
        // not claim that the client owns the complete immutable segment.
        let range_id = "vod-range";
        let range_owner = install_vod_http_session(&fixture, dir.path(), range_id).await;
        let range_path = dir.path().join("range.m4s");
        let range_bytes = vec![2_u8; 16 * 1024];
        tokio::fs::write(&range_path, &range_bytes)
            .await
            .expect("range VOD object");
        let touched_before = fixture
            .state
            .transcode
            .vod_last_touch_for_test(range_id)
            .await
            .expect("range touch before");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let range_headers = RelayHeaders {
            range: Some("bytes=1024-2047".to_owned()),
            if_range: Some(format!("\"http-test-{}\"", range_bytes.len())),
            ..RelayHeaders::default()
        };
        let range = vod_segment_response(
            &fixture.state,
            range_id,
            "seg00004.m4s",
            &range_headers,
            vod_ready(&range_path, range_bytes.len() as u64).await,
            range_owner,
        )
        .await
        .expect("range VOD response");
        assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            axum::body::to_bytes(range.into_body(), 2_048)
                .await
                .expect("range VOD body")
                .len(),
            1_024
        );
        assert!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(range_id)
                .await
                .expect("range touch after")
                > touched_before
        );
        assert_eq!(vod_fetched_segment(&fixture, range_id).await, None);

        // Dropping the body before EOF cannot renew or move the frontier.
        let drop_id = "vod-drop";
        let drop_owner = install_vod_http_session(&fixture, dir.path(), drop_id).await;
        let drop_path = dir.path().join("drop.m4s");
        let drop_bytes = vec![3_u8; 64 * 1024];
        tokio::fs::write(&drop_path, &drop_bytes)
            .await
            .expect("drop VOD object");
        let drop_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(drop_id)
            .await
            .expect("drop touch");
        let dropped = vod_segment_response(
            &fixture.state,
            drop_id,
            "seg00005.m4s",
            &headers,
            vod_ready(&drop_path, drop_bytes.len() as u64).await,
            drop_owner,
        )
        .await
        .expect("droppable VOD response");
        let mut dropped = dropped.into_body().into_data_stream();
        assert!(dropped.next().await.is_some_and(|chunk| chunk.is_ok()));
        drop(dropped);
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(drop_id)
                .await,
            Some(drop_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, drop_id).await, None);

        // A short object reaches storage EOF but not the promised response
        // length, so it is not successful media delivery.
        let short_id = "vod-short";
        let short_owner = install_vod_http_session(&fixture, dir.path(), short_id).await;
        let short_path = dir.path().join("short.m4s");
        tokio::fs::write(&short_path, vec![4_u8; 1_024])
            .await
            .expect("short VOD object");
        let short_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(short_id)
            .await
            .expect("short touch");
        let short = vod_segment_response(
            &fixture.state,
            short_id,
            "seg00006.m4s",
            &headers,
            vod_ready(&short_path, 2_048).await,
            short_owner,
        )
        .await
        .expect("short VOD response");
        let mut short = short.into_body().into_data_stream();
        assert_eq!(
            short
                .next()
                .await
                .expect("short VOD data")
                .expect("readable short prefix")
                .len(),
            1_024
        );
        assert!(
            short.next().await.is_some_and(|chunk| chunk.is_err()),
            "advertised short read must terminate the HTTP body with an error"
        );
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(short_id)
                .await,
            Some(short_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, short_id).await, None);

        // A storage error terminates the body and discards the completion.
        let error_id = "vod-error";
        let error_owner = install_vod_http_session(&fixture, dir.path(), error_id).await;
        let error_path = dir.path().join("unreadable-vod.m4s");
        tokio::fs::create_dir(&error_path)
            .await
            .expect("unreadable VOD object");
        let error_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(error_id)
            .await
            .expect("error touch");
        let error = vod_segment_response(
            &fixture.state,
            error_id,
            "seg00007.m4s",
            &headers,
            vod_ready(&error_path, 1).await,
            error_owner,
        )
        .await
        .expect("error VOD response");
        let mut error = error.into_body().into_data_stream();
        assert!(error.next().await.is_some_and(|chunk| chunk.is_err()));
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(error_id)
                .await,
            Some(error_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, error_id).await, None);

        // Resolution before same-id reattachment carries the old incarnation;
        // even exact EOF cannot touch the successor or its reader frontier.
        let replaced_id = "vod-replaced";
        let stale_owner = install_vod_http_session(&fixture, dir.path(), replaced_id).await;
        let replaced_path = dir.path().join("replaced.m4s");
        let replaced_bytes = vec![5_u8; 8 * 1024];
        tokio::fs::write(&replaced_path, &replaced_bytes)
            .await
            .expect("replaced VOD object");
        let stale_response = vod_segment_response(
            &fixture.state,
            replaced_id,
            "seg00008.m4s",
            &headers,
            vod_ready(&replaced_path, replaced_bytes.len() as u64).await,
            stale_owner,
        )
        .await
        .expect("stale VOD response");
        let _successor_owner = install_vod_http_session(&fixture, dir.path(), replaced_id).await;
        let successor_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(replaced_id)
            .await
            .expect("successor touch");
        assert_eq!(
            axum::body::to_bytes(stale_response.into_body(), replaced_bytes.len() + 1)
                .await
                .expect("stale body remains readable")
                .len(),
            replaced_bytes.len()
        );
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(replaced_id)
                .await,
            Some(successor_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, replaced_id).await, None);
    }

    #[tokio::test]
    async fn range_and_bodyless_segment_responses_keep_delivery_truth() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "range").await;
        fixture.mark_started().await;
        let body = vec![5_u8; 16 * 1024];
        tokio::fs::write(dir.path().join("seg00004.m4s"), &body)
            .await
            .expect("segment bytes");

        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=1024-2047".parse().expect("range"));
        let response = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            headers,
        )
        .await
        .expect("partial response");
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 2_048)
                .await
                .expect("partial body")
                .len(),
            1_024
        );
        let partial_delivery = fixture
            .wait_for_delivery_projection("segment-range", None)
            .await;
        assert_eq!(fixture.delivered_bytes(), 1_024);
        assert_eq!(fixture.last_renewal_kind().await, "segment-range");
        assert_eq!(
            fixture.fetched_segment(),
            -1,
            "a completed byte range proves demand but not a complete segment"
        );
        assert_eq!(partial_delivery.fetched_segment, None);

        let mut full_span = HeaderMap::new();
        full_span.insert(header::RANGE, "bytes=0-".parse().expect("full range"));
        let full_span = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            full_span,
        )
        .await
        .expect("full-span range response");
        assert_eq!(full_span.status(), StatusCode::PARTIAL_CONTENT);
        let full_span_etag = full_span
            .headers()
            .get(header::ETAG)
            .cloned()
            .expect("rolling response ETag");
        assert_eq!(
            axum::body::to_bytes(full_span.into_body(), body.len() + 1)
                .await
                .expect("full-span body")
                .len(),
            body.len()
        );
        let actor_delivery = fixture
            .wait_for_delivery_projection("segment-range", Some(4))
            .await;
        assert_eq!(
            fixture.fetched_segment(),
            4,
            "a Range response that contains every byte advances the frontier"
        );
        assert_eq!(actor_delivery.fetched_segment, Some(4));
        assert_eq!(actor_delivery.pending_fetched_segment, None);
        let mut stale_if_range = HeaderMap::new();
        stale_if_range.insert(
            header::RANGE,
            "bytes=1024-2047".parse().expect("stale conditional range"),
        );
        stale_if_range.insert(
            header::IF_RANGE,
            "W/\"stale-generation\"".parse().expect("weak If-Range"),
        );
        let complete = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            stale_if_range,
        )
        .await
        .expect("stale If-Range response");
        assert_eq!(complete.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(complete.into_body(), body.len() + 1)
                .await
                .expect("complete fallback body")
                .len(),
            body.len(),
            "weak or stale If-Range must ignore Range"
        );
        let delivered_after_full_span = fixture.delivered_bytes();

        let mut conditional = HeaderMap::new();
        conditional.insert(header::IF_NONE_MATCH, full_span_etag);
        let not_modified = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            conditional,
        )
        .await
        .expect("conditional response");
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            fixture.fetched_segment(),
            4,
            "the client has the cached object"
        );
        assert_eq!(fixture.actor_delivery().await.fetched_segment, Some(4));
        let renewal_before_rejection = fixture.last_renewal_kind().await;
        let frontier_before_rejection = fixture.fetched_segment();

        let mut unsatisfiable = HeaderMap::new();
        unsatisfiable.insert(
            header::RANGE,
            "bytes=999999-".parse().expect("invalid range value"),
        );
        let rejected = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            unsatisfiable,
        )
        .await
        .expect("range response");
        assert_eq!(rejected.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(fixture.delivered_bytes(), delivered_after_full_span);
        assert_eq!(fixture.last_renewal_kind().await, renewal_before_rejection);
        assert_eq!(fixture.fetched_segment(), frontier_before_rejection);
        assert!(
            fixture.settle().await.is_empty(),
            "valid partial and intentionally bodyless responses are not incomplete deliveries"
        );

        let init_dir = crate::test_tempdir().expect("init directory");
        let init_fixture = HlsDeliveryFixture::publish(init_dir.path(), "init-range").await;
        init_fixture.mark_started().await;
        tokio::fs::write(init_dir.path().join("init.mp4"), vec![9_u8; 4_096])
            .await
            .expect("init bytes");
        let mut init_headers = HeaderMap::new();
        init_headers.insert(header::RANGE, "bytes=0-3".parse().expect("init range"));
        let init_response = segment(
            State(init_fixture.state.clone()),
            AxPath(("init-range".to_owned(), "init.mp4".to_owned())),
            init_headers,
        )
        .await
        .expect("init partial response");
        assert_eq!(
            axum::body::to_bytes(init_response.into_body(), 16)
                .await
                .expect("init body")
                .len(),
            4
        );
        assert_eq!(init_fixture.delivered_bytes(), 4);

        let mut stale_init_headers = HeaderMap::new();
        stale_init_headers.insert(header::RANGE, "bytes=0-3".parse().expect("init range"));
        stale_init_headers.insert(
            header::IF_RANGE,
            "\"different-representation\""
                .parse()
                .expect("init If-Range"),
        );
        let complete_init = segment(
            State(init_fixture.state.clone()),
            AxPath(("init-range".to_owned(), "init.mp4".to_owned())),
            stale_init_headers,
        )
        .await
        .expect("init If-Range fallback");
        assert_eq!(complete_init.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(complete_init.into_body(), 4_097)
                .await
                .expect("complete init body")
                .len(),
            4_096
        );
        assert_eq!(init_fixture.delivered_bytes(), 4_100);
        assert!(init_fixture.settle().await.is_empty());
    }

    /// A response body dropped mid-segment is the case nothing else observes:
    /// the handler has already returned, the stream never reaches EOF, and no
    /// error is raised. Only `Drop` can name it, which also makes it the
    /// easiest classification to lose to a later refactor.
    #[tokio::test]
    async fn an_abandoned_segment_body_is_recorded_as_response_dropped() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "abandoned").await;
        fixture.make_segment_window_servable().await;
        let renewal_before = fixture.last_renewal_kind().await;
        let frontier_before = fixture.fetched_segment();
        let body = vec![3_u8; 64 * 1024];
        tokio::fs::write(dir.path().join("seg00002.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("abandoned".to_owned(), "seg00002.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");
        let mut stream = response.into_body().into_data_stream();
        let first = stream
            .next()
            .await
            .expect("a first chunk")
            .expect("a readable first chunk");
        assert!(
            first.len() < body.len(),
            "the fixture must leave the body partially consumed"
        );
        drop(stream);

        let events = fixture.delivery_events(1).await;
        let dropped = events
            .iter()
            .find(|event| event.reason.as_deref() == Some("response_dropped"))
            .expect(
                "an abandoned body is recorded without blaming storage or inventing client intent",
            );
        assert_eq!(dropped.event, "segment_delivery_incomplete");
        let extra = dropped.extra.as_deref().unwrap_or_default();
        assert!(
            extra.contains("\"segment\":\"seg00002.m4s\"")
                && extra.contains(&format!("\"expected_bytes\":{}", body.len()))
                && extra.contains(&format!("\"delivered_bytes\":{}", first.len()))
                && extra.contains("\"producer_attempt\":")
                && extra.contains("\"response_incarnation\":")
                && extra.contains("\"segment_start_ms\":")
                && extra.contains("\"segment_duration_ms\":")
                && extra.contains("\"producer_superseded\":false")
                && extra.contains("\"client_disposition\":\"unknown\"")
                && extra.contains("\"cut_class\":\"unclassified_drop\""),
            "the event names the exact response, timeline, generation, and cut: {extra}"
        );
        assert_eq!(
            fixture.delivered_bytes(),
            first.len() as i64,
            "only the bytes the client actually took are delivery"
        );
        assert_eq!(
            fixture.last_renewal_kind().await,
            renewal_before,
            "a dropped response cannot renew the playback lease"
        );
        assert_eq!(
            fixture.fetched_segment(),
            frontier_before,
            "a dropped response cannot advance the consumed frontier"
        );
    }

    #[tokio::test]
    async fn a_stream_resolved_before_retirement_cannot_commit_after_eof() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "retired-body").await;
        fixture.make_segment_window_servable().await;
        let body = vec![7_u8; 32 * 1024];
        tokio::fs::write(dir.path().join("seg00003.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("retired-body".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("resolved response");
        assert!(
            fixture
                .state
                .transcode
                .stop_session("retired-body", "test-retirement")
                .await
        );
        let renewal_after_retirement = fixture.last_renewal_kind().await;
        let frontier_after_retirement = fixture.fetched_segment();

        assert_eq!(
            axum::body::to_bytes(response.into_body(), body.len() + 1)
                .await
                .expect("already-authorized bytes")
                .len(),
            body.len()
        );
        assert_eq!(
            fixture.last_renewal_kind().await,
            renewal_after_retirement,
            "EOF from an obsolete incarnation cannot renew it"
        );
        assert_eq!(
            fixture.fetched_segment(),
            frontier_after_retirement,
            "EOF from an obsolete incarnation cannot move its frontier"
        );
    }

    #[tokio::test]
    async fn a_stream_from_an_old_producer_attempt_cannot_advance_its_successor() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "old-attempt-body").await;
        fixture.make_segment_window_servable().await;
        let body = vec![9_u8; 32 * 1024];
        tokio::fs::write(dir.path().join("seg00003.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("old-attempt-body".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("response opened on attempt zero");
        assert_eq!(fixture.begin_producer_attempt().await, Ok(1));
        let renewal_after_replacement = fixture.last_renewal_kind().await;

        assert_eq!(
            axum::body::to_bytes(response.into_body(), body.len() + 1)
                .await
                .expect("already-open predecessor bytes")
                .len(),
            body.len()
        );
        assert_eq!(
            fixture.last_renewal_kind().await,
            renewal_after_replacement,
            "predecessor EOF cannot renew the successor attempt"
        );
        assert_eq!(fixture.fetched_segment(), -1);
        assert_eq!(fixture.actor_delivery().await.fetched_segment, None);
    }

    #[tokio::test]
    async fn accepted_predecessor_eof_cannot_project_after_successor_reset() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "projection-race").await;
        fixture.make_segment_window_servable().await;
        let body = vec![5_u8; 32 * 1024];
        tokio::fs::write(dir.path().join("seg00003.m4s"), &body)
            .await
            .expect("segment bytes");
        let response = segment(
            State(fixture.state.clone()),
            AxPath(("projection-race".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("predecessor response");

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture.pause_response_projection(Arc::clone(&pause));
        let body_len = body.len();
        let drain = tokio::spawn(async move {
            axum::body::to_bytes(response.into_body(), body_len + 1)
                .await
                .expect("predecessor body")
        });
        pause.wait().await;
        assert_eq!(
            fixture.begin_producer_attempt().await,
            Ok(1),
            "successor admission resets the compatibility projection"
        );
        pause.wait().await;
        assert_eq!(drain.await.expect("body task").len(), body_len);
        assert_eq!(fixture.fetched_segment(), -1);
        assert_eq!(fixture.actor_delivery().await.fetched_segment, None);
    }

    /// A storage error mid-body is its own classification, separate from an
    /// abandoned response and from a short one. Nothing reached `fail()`
    /// before this test, so `storage_read_error` could have stopped being
    /// emitted with no check noticing.
    #[tokio::test]
    async fn an_unreadable_segment_body_is_recorded_as_storage_read_error() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unreadable").await;
        fixture.make_segment_window_servable().await;
        let renewal_before = fixture.last_renewal_kind().await;
        let frontier_before = fixture.fetched_segment();
        // A directory opens like a file and reports a length, then fails its
        // first read with EISDIR — a storage failure the handler meets only
        // after the response headers are already on the wire.
        tokio::fs::create_dir(dir.path().join("seg00003.m4s"))
            .await
            .expect("unreadable segment");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("unreadable".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");
        let mut stream = response.into_body().into_data_stream();
        assert!(
            stream.next().await.is_some_and(|chunk| chunk.is_err()),
            "the body surfaces the storage error to the client"
        );
        // The `unfold` stops rather than re-polling a failed reader. That is
        // belt-and-braces — `ReaderStream` already drops its reader on error —
        // so this pins the reachable contract (the body ends at the error)
        // rather than the guard itself.
        assert!(
            stream.next().await.is_none(),
            "the body ends at the storage error"
        );

        let events = fixture.delivery_events(1).await;
        let failed = events
            .iter()
            .find(|event| event.reason.as_deref() == Some("storage_read_error"))
            .expect("a failed storage read is reported as one");
        assert_eq!(failed.event, "segment_delivery_incomplete");
        assert!(
            failed
                .extra
                .as_deref()
                .is_some_and(|extra| extra.contains("\"error\"")),
            "the operator gets the underlying error text"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "segment_delivery_incomplete")
                .count(),
            1,
            "one response produces one terminal classification"
        );
        assert_eq!(fixture.last_renewal_kind().await, renewal_before);
        assert_eq!(fixture.fetched_segment(), frontier_before);
    }

    #[tokio::test]
    async fn an_unreadable_small_init_never_commits_lease_or_frontier() {
        let dir = crate::test_tempdir().expect("init directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unreadable-init").await;
        fixture.make_segment_window_servable().await;
        tokio::fs::create_dir(dir.path().join("init.mp4"))
            .await
            .expect("unreadable init");
        let renewal_before = fixture.last_renewal_kind().await;
        let frontier_before = fixture.fetched_segment();

        assert!(
            segment(
                State(fixture.state.clone()),
                AxPath(("unreadable-init".to_owned(), "init.mp4".to_owned())),
                HeaderMap::new(),
            )
            .await
            .is_err(),
            "the buffered init read must fail before a response is committed"
        );
        assert_eq!(fixture.last_renewal_kind().await, renewal_before);
        assert_eq!(fixture.fetched_segment(), frontier_before);
        assert!(fixture
            .delivery_events(1)
            .await
            .iter()
            .any(|event| event.reason.as_deref() == Some("storage_read_error")));
    }

    /// `exact_hls_context` opens `init.mp4` for the playlist generator, not
    /// for a client: there is no response body on that path. Its bytes must
    /// not move the session's delivery meter, and — because the read is
    /// bounded well below a large init's real length — a bounded read that
    /// returned everything it asked for must not be reported as a response
    /// that ended early.
    #[tokio::test]
    async fn web_hls_startup_init_probe_is_not_client_delivery_or_a_short_response() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "probe").await;
        fixture.make_segment_window_servable().await;
        // Past the inspection bound, so the read stops short of the file's
        // advertised length by design.
        let oversized = vec![0_u8; (INIT_INSPECTION_LIMIT_BYTES + 4_096) as usize];
        tokio::fs::write(dir.path().join("init.mp4"), &oversized)
            .await
            .expect("oversized init");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "hvc1.2.4.L150.B0,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "probe", context).await;
        assert!(matches!(
            resolved,
            Err(HlsInitInspectionError::Response {
                code: "hls_init_invalid",
                ..
            })
        ));
        assert_eq!(
            fixture.delivered_bytes(),
            0,
            "a playlist-time codec sniff delivers nothing to anyone"
        );
        assert!(
            fixture.settle().await.is_empty(),
            "a bounded read that returned every byte it asked for is not a truncated response"
        );
    }

    /// When a server-internal read *does* fail, the event still has to be
    /// readable as internal. An untagged `segment_delivery_incomplete` for
    /// `init.mp4` is indistinguishable from a client fetch that broke, which
    /// is the availability/delivery conflation this telemetry exists to end.
    #[tokio::test]
    async fn web_hls_startup_failed_init_probe_is_internal_and_unavailable() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "probe-error").await;
        fixture.make_segment_window_servable().await;
        tokio::fs::create_dir(dir.path().join("init.mp4"))
            .await
            .expect("unreadable init");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "hvc1.2.4.L150.B0,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        assert!(matches!(
            exact_hls_context(&fixture.state, "probe-error", context).await,
            Err(HlsInitInspectionError::Response {
                code: "init_inspection_unavailable",
                ..
            })
        ));

        let events = fixture.delivery_events(1).await;
        let failed = events
            .iter()
            .find(|event| event.reason.as_deref() == Some("storage_read_error"))
            .expect("a failed internal read is still reported");
        assert!(
            failed
                .extra
                .as_deref()
                .is_some_and(|extra| extra.contains("\"purpose\":\"internal_probe\"")),
            "an operator reading a freeze can tell a codec sniff from a segment fetch: {:?}",
            failed.extra
        );
        assert_eq!(
            fixture.delivered_bytes(),
            0,
            "a failed internal read is not a partial delivery"
        );
    }

    #[test]
    /// Both 503s a viewer hit on 2026-09-21 were codeless, so every client
    /// correctly refused to retry them and showed a terminal overlay for a
    /// title that was merely still loading. Neither is a failure; both are
    /// "ask again", which is what `startup_timeout` means to the ladder all
    /// three clients already run. This pins the sidecar half; the placement
    /// deadline's arm is inside the start handler and is covered by the
    /// integration path rather than here.
    fn a_pending_sidecar_is_a_named_not_yet_answer() {
        let pending = format!(
            "{}the source is still being read for this track",
            crate::subtitles::SIDECAR_PENDING_PREFIX
        );
        match session_start_error(5208, pending) {
            ApiError::TypedRetry {
                status,
                code,
                retry_after_seconds,
                ..
            } => {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(code, "startup_timeout");
                assert_eq!(retry_after_seconds, SIDECAR_PENDING_RETRY_AFTER_SECS);
            }
            _ => panic!("a pending sidecar must be a named not-yet answer"),
        }
        // An extraction that genuinely failed is NOT a not-yet: it is about
        // this track, the negative memo already remembers it, and retrying it
        // three times would just replay the same failure at the viewer.
        assert!(!crate::subtitles::is_sidecar_pending_error(
            "text subtitle extraction failed; suppressing retries for now"
        ));
    }

    #[test]
    fn bounded_admission_failure_is_a_retryable_503() {
        let capacity =
            "transcode capacity is temporarily unavailable: background encoding did not yield";
        let response = session_start_error(42, capacity.into()).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(matches!(
            session_start_error(42, "ffmpeg failed".into()),
            ApiError::Internal(_)
        ));
    }

    #[test]
    fn omitted_public_presentation_means_vod_never_live() {
        let create: CreateSession = serde_json::from_value(serde_json::json!({
            "playback_id": "older-native-client",
            "copy": true
        }))
        .expect("create body");
        assert_eq!(
            create.into_request(42, 1080).presentation,
            crate::transcode::Presentation::Vod
        );
    }

    #[test]
    fn every_vod_ineligibility_reaches_the_wire_as_a_typed_refusal() {
        let cases = [
            ("vod_disabled", StatusCode::SERVICE_UNAVAILABLE),
            ("vod_index_pending", StatusCode::SERVICE_UNAVAILABLE),
            ("vod_transcode_unavailable", StatusCode::NOT_IMPLEMENTED),
            ("vod_subtitle_burn_unavailable", StatusCode::NOT_IMPLEMENTED),
            ("vod_source_unsupported", StatusCode::UNPROCESSABLE_ENTITY),
            (
                "vod_video_geometry_unknown",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "vod_frame_cadence_unknown",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            ("vod_source_rescan_required", StatusCode::CONFLICT),
            ("vod_engine_unattested", StatusCode::SERVICE_UNAVAILABLE),
            ("vod_audio_track_missing", StatusCode::UNPROCESSABLE_ENTITY),
            (
                "vod_subtitle_track_missing",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            ("vod_invalid_height", StatusCode::BAD_REQUEST),
            ("vod_recipe_unresolved", StatusCode::INTERNAL_SERVER_ERROR),
            ("vod_reopen_required", StatusCode::CONFLICT),
            ("live_presentation_removed", StatusCode::GONE),
        ];
        for (code, status) in cases {
            let error = crate::transcode::vod_refusal_error(code, "named reason");
            match session_start_error(42, error) {
                ApiError::Typed {
                    status: actual_status,
                    code: actual_code,
                    message,
                } => {
                    assert_eq!(actual_status, status, "{code}");
                    assert_eq!(actual_code, code, "{code}");
                    assert_eq!(message, "named reason", "{code}");
                }
                other => panic!("{code} was not typed: {other:?}"),
            }
        }
    }

    /// §7.3 requirement 3 is about VIEWER intent, not wire presence. Android's
    /// `sessionHeight` answers a subtitle burn with the source height before it
    /// consults quality, so an Auto viewer with a burned subtitle posts a
    /// height; inferring stickiness from that field alone makes the heaviest
    /// session type there is permanently unsteppable on Android while the
    /// identical Apple session steps. `quality_auto` is the intent itself.
    #[test]
    fn stall_stickiness_follows_stated_quality_intent_not_a_posted_height() {
        let burn_under_auto = serde_json::json!({
            "playback_id": "android-player",
            // The Original/forced-burn source-height promise, not a rung pick.
            "height": 2160,
            "subtitle_burn": 3,
            "quality_auto": true,
        });
        let create: CreateSession = serde_json::from_value(burn_under_auto).expect("create body");
        let request = create.into_request(17, 2160);
        assert!(
            request.automatic,
            "a burn's source-height promise is not a manual quality pick"
        );
        assert_eq!(
            request.kind,
            crate::transcode::SessionKind::Transcode { height: 2160 }
        );

        // The other direction is just as explicit: a viewer who picked a rung
        // stays on it even though the body would otherwise read as Auto.
        let manual_without_height = serde_json::json!({
            "playback_id": "android-player",
            "quality_auto": false,
        });
        let create: CreateSession =
            serde_json::from_value(manual_without_height).expect("create body");
        assert!(!create.into_request(17, 720).automatic);

        // Android Original+copy has no height by design. Its explicit false
        // must override the legacy heightless-body inference or the server
        // silently changes the viewer's mode back to Auto.
        let original_copy = serde_json::json!({
            "playback_id": "android-player",
            "copy": true,
            "quality_auto": false,
        });
        let create: CreateSession =
            serde_json::from_value(original_copy).expect("original copy body");
        let request = create.into_request(17, 2160);
        assert!(!request.automatic);
        assert!(matches!(
            request.kind,
            crate::transcode::SessionKind::Copy { .. }
        ));

        // Legacy clients omit the field, so the old inference has to survive
        // untouched in both of its arms.
        let silent_auto = serde_json::json!({ "playback_id": "apple-player" });
        let create: CreateSession = serde_json::from_value(silent_auto).expect("create body");
        assert!(create.into_request(17, 720).automatic);

        let silent_manual = serde_json::json!({ "playback_id": "apple-player", "height": 480 });
        let create: CreateSession = serde_json::from_value(silent_manual).expect("create body");
        assert!(!create.into_request(17, 480).automatic);
    }

    #[test]
    fn a_typed_stall_reopen_reaches_the_claim_unchanged() {
        let body = serde_json::json!({
            "playback_id": "native-player",
            "request_id": "stall-attempt",
            "previous_session_id": "previous-session",
            "reopen_reason": "stall",
            "start": 42.5,
            "audio": 2,
            "subtitle_burn": 5
        });
        let create: CreateSession = serde_json::from_value(body).expect("typed create body");
        let request = create.into_request(17, 1080);

        assert!(request.automatic);
        assert_eq!(
            request.previous_session_id.as_deref(),
            Some("previous-session")
        );
        assert_eq!(
            request.reopen_reason,
            Some(crate::transcode::ReopenReason::Stall)
        );
        assert_eq!(request.start_seconds, 42.5);
        assert_eq!(request.audio_index, Some(2));
        assert_eq!(request.subtitle_burn, Some(5));

        let unknown = serde_json::json!({
            "playback_id": "native-player",
            "request_id": "future-attempt",
            "previous_session_id": "previous-session",
            "reopen_reason": "network_changed"
        });
        assert!(serde_json::from_value::<CreateSession>(unknown).is_err());
    }

    #[test]
    fn an_invalid_bound_reopen_is_a_400_not_an_idempotency_conflict() {
        let response = session_start_error(
            42,
            "invalid stall reopen: the previous session does not belong to this user, playback, and file"
                .into(),
        )
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A build that cannot burn is a fact the viewer can act on, not an
    /// "internal server error" whose message is deliberately dropped.
    #[test]
    fn a_build_that_cannot_burn_says_so_instead_of_hiding_behind_a_500() {
        let error = "this server's media tools cannot do that: this server's ffmpeg build has no \
             subtitles filter, which burning text subtitles into the picture requires";
        match session_start_error(42, error.into()) {
            ApiError::Typed {
                status,
                code,
                message,
            } => {
                assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
                assert_eq!(code, "unsupported_build");
                assert!(
                    message.contains("subtitles filter") && !message.contains("cannot do that:"),
                    "the viewer reads the reason, not the classification: {message}"
                );
            }
            other => panic!("expected a typed refusal, got {other:?}"),
        }
    }

    /// Every playlist refusal, from the session's verdict to the wire.
    ///
    /// These used to be `ApiError::NotFound("transcode session")` — one
    /// anonymous 404 that hls.js escalates to a fatal `levelLoadError`
    /// whatever caused it. The status now separates what the client can do
    /// about it, and the typed body carries the sentence a person reads.
    #[tokio::test]
    async fn each_playlist_refusal_reaches_the_client_as_itself() {
        use axum::body::to_bytes;

        let rows = [
            (
                PlaylistError::SessionGone,
                StatusCode::NOT_FOUND,
                "session_gone",
                "no longer running",
            ),
            (
                PlaylistError::ProducerExited("exit status: 1".into()),
                StatusCode::BAD_GATEWAY,
                "producer_failed",
                "exit status: 1",
            ),
            (
                PlaylistError::ProducerEnded("progress deadline elapsed".into()),
                StatusCode::BAD_GATEWAY,
                "producer_ended",
                "already listed remains available",
            ),
            (
                PlaylistError::SessionFailed("the encoder never produced any video".into()),
                StatusCode::BAD_GATEWAY,
                "session_failed",
                "never produced any video",
            ),
            (
                PlaylistError::InsufficientCapacity(
                    "rolling_insufficient_capacity: requested 2.00x".into(),
                ),
                StatusCode::BAD_GATEWAY,
                "rolling_insufficient_capacity",
                "requested 2.00x",
            ),
            (
                // The #263 case: still inside the server's own recovery.
                // 503, not 404, and hls.js's level-load retry is the right
                // response to it.
                PlaylistError::StartupTimedOut(std::time::Duration::from_secs(45)),
                StatusCode::SERVICE_UNAVAILABLE,
                "startup_timeout",
                "still preparing",
            ),
        ];
        for (err, status, code, fragment) in rows {
            let response = playlist_error("sess-1", err.clone()).into_response();
            assert_eq!(response.status(), status, "{code}");
            let body = to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("body");
            let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
            assert_eq!(json["code"], code);
            let message = json["message"].as_str().expect("message");
            assert_eq!(err.retryable(), code == "startup_timeout", "{code}");
            assert!(
                message.to_lowercase().contains(fragment),
                "{code}: \"{message}\" should contain \"{fragment}\""
            );
            // The sentence the old overlay showed sent people to a log file on
            // a box they were not sitting at. Nothing here may do that.
            assert!(!message.contains("Settings"), "{code}: {message}");
        }
    }

    #[test]
    fn fmp4_media_segments_use_the_iso_segment_mime_type() {
        assert_eq!(segment_content_type("init.mp4"), "video/mp4");
        assert_eq!(segment_content_type("seg00000.m4s"), "video/iso.segment");
        assert_eq!(segment_content_type("seg00000.ts"), "video/mp2t");
    }

    fn hls_file(subtitle_streams: Vec<SubtitleStream>) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 5615,
            item_id: 1,
            path: "/media/Scary Movie.mkv".into(),
            size: 19_000_000_000,
            mtime: 1,
            duration_ms: Some(120_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: Some("dolby_vision".into()),
            hdr_format: Some("Dolby Vision".into()),
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(40_000_000),
            audio_streams: vec![],
            subtitle_streams,
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    /// A node that can do everything, so the review's answers are the
    /// client's answers and nothing is confounded by what this server proved
    /// at boot.
    fn capable_node() -> plurx_core::playback::RenderCaps {
        plurx_core::playback::RenderCaps::proven(true)
    }

    /// `hls_file` carries a bare "Dolby Vision" label — deliberately, because
    /// the HLS master tests exist to prove a stream with no configuration
    /// record still gets a coherent playlist. The plan review needs the
    /// opposite: a title whose profile is actually knowable, or every
    /// derivation below answers "no Dolby Vision" for a reason that has
    /// nothing to do with the client.
    fn dolby_vision_p8_file() -> MediaFile {
        let mut file = hls_file(Vec::new());
        file.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".into());
        file
    }

    fn caps_v2(json: &str) -> plurx_core::playback::DeviceCaps {
        serde_json::from_str(json).expect("the v2 document parses")
    }

    /// A client that decodes Profile 8 Dolby Vision over HLS. The plan for
    /// `hls_file` (a P8-labelled 2160p HEVC title) is to preserve it.
    fn dolby_vision_client() -> plurx_core::playback::DeviceCaps {
        caps_v2(
            r#"{"v":2,
                "video":[{"codec":"hevc","present":["sdr","pq"],"dv_profiles":[5,8],
                          "max_height":2160}],
                "audio":["aac"],"containers":["mkv","mp4"],
                "dv_transport":"hls",
                "display":{"hdr":true,"dolby_vision":true}}"#,
        )
    }

    /// The same hardware minus the Dolby Vision decoder — Chrome, in other
    /// words, which is the client the whole plan was written for.
    fn no_dolby_vision_client() -> plurx_core::playback::DeviceCaps {
        caps_v2(
            r#"{"v":2,
                "video":[{"codec":"hevc","present":["sdr"],"dv_profiles":[],
                          "max_height":2160}],
                "audio":["aac"],"containers":["mkv","mp4"],
                "display":{"hdr":false,"dolby_vision":false}}"#,
        )
    }

    /// The acceptance case from PLAYBACK-CAPS-V2-PLAN M3: a create whose body
    /// claims Dolby Vision for a client whose caps enumerate none.
    ///
    /// It **succeeds**, with the server's plan, and says so. Paul's ruling
    /// (2026-08-29): "I don't see a reason for it to prevent functionality."
    /// A refusal here would turn a client bug into an unplayable title, which
    /// is strictly worse than the tone-mapped stream the viewer would
    /// otherwise have got anyway.
    #[test]
    fn a_create_that_claims_more_than_its_caps_gets_the_servers_plan_and_a_note() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,  // the body says preserve_dolby_vision
            false, // …and asks for no HDR10 rung
            NOW_MS,
        );

        assert!(
            !review.preserve_dolby_vision,
            "the caps enumerate no Dolby Vision profile, so the plan cannot preserve it"
        );
        assert!(review.mismatched);
        assert_eq!(
            review.notes,
            vec!["plan_mismatch: client asked preserve_dolby_vision=true, \
                 server derived false"
                .to_owned()],
            "the note names the field, the claim, and the answer — it is what the \
             stats overlay shows a viewer asking why the badge changed"
        );
    }

    /// The case that must stay silent: the client and the server agree.
    ///
    /// This is the one that should be every create once the fleet has moved,
    /// and the absence of notes is the signal. A review that leaves a note on
    /// an agreeing create makes `plan_notes` useless, because a field that is
    /// always populated is a field nobody reads.
    #[test]
    fn an_agreeing_create_leaves_no_note_at_all() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            true,
            NOW_MS,
        );
        assert!(review.preserve_dolby_vision);
        assert!(review.hdr10, "the caps present PQ on hevc");
        assert!(!review.mismatched);
        assert!(review.notes.is_empty(), "{:?}", review.notes);
    }

    /// And when the conversion does *not* happen, the echo is not trusted
    /// either.
    ///
    /// This is the half the test below does not reach. Deriving the conversion
    /// fixed the case where it could be derived; it left the cases where it
    /// comes back `false` delivering exactly what the original regression
    /// delivered — raw dual-layer to a build that enumerated nothing. There
    /// are two of them and both are ordinary: an operator who turned the
    /// conversion off, and a row the M2 backfill has not reached, which has a
    /// Profile 7 label and no columns to build a record from.
    ///
    /// `preserve_dolby_vision: true` from such a build is not a claim about
    /// dual-layer. It is the server's own answer, echoed back — and the server
    /// only answered `true` because it expected to convert.
    #[test]
    fn a_build_with_no_caps_document_is_never_handed_unconverted_dual_layer() {
        let p7 = dolby_vision_p7_file();

        let mut off = capable_node();
        off.dolby_vision_convert = false;
        let refused = legacy_trusted_review(&p7, &off, true, false);
        assert!(!refused.convert_dolby_vision);
        assert!(
            !refused.preserve_dolby_vision,
            "keeping Profile 7 for a build that enumerated nothing is the black \
             screen this milestone exists to stop delivering"
        );
        assert!(
            refused
                .notes
                .iter()
                .any(|note| note.contains("Dolby Vision declined")),
            "and the session says why: {:?}",
            refused.notes
        );
        assert!(
            !refused.mismatched,
            "not a mismatch: there is no document here for the client to have \
             exceeded, and the counter that measures that must stay meaningful"
        );

        // A row with a Profile 7 label and no columns is the same answer by a
        // different route: it cannot convert, so it cannot preserve.
        let mut label_only = p7.clone();
        label_only.dolby_vision = Default::default();
        let node = capable_node();
        assert!(!legacy_trusted_review(&label_only, &node, true, false).preserve_dolby_vision);

        // Everything else is unchanged. A converting create still preserves,
        // because the conversion needs the RPUs to survive the filter…
        let converting = legacy_trusted_review(&p7, &node, true, false);
        assert!(converting.convert_dolby_vision && converting.preserve_dolby_vision);
        assert!(converting.notes.is_empty(), "{:?}", converting.notes);

        // …and a single-layer source is trusted exactly as it always was, on
        // the same node that refused the dual-layer one.
        let p8 = dolby_vision_p8_file();
        let single_layer = legacy_trusted_review(&p8, &off, true, false);
        assert!(
            single_layer.preserve_dolby_vision,
            "the rule is about dual-layer, not about Dolby Vision"
        );
        assert!(single_layer.notes.is_empty(), "{:?}", single_layer.notes);
    }

    /// A build that sends no caps document still gets the conversion.
    ///
    /// The regression this exists for shipped and reached production: the
    /// legacy-trusted arm returned no review at all, so `apply_plan_review`
    /// never ran, so `convert_dolby_vision` kept the `false`
    /// `SessionKind::Copy` was built with — and the client's *silence* on a
    /// field it has no way to speak about outvoted the server's own decision.
    /// `/decision` logged "Profile 7 converted to Profile 8.1 for this
    /// device"; the session created a second later delivered raw Profile 7;
    /// Safari answered `stream_rejected ... browser refused the remux
    /// stream`; the fallback tonemapped the title to SDR. Nothing in the
    /// cluster had ever run a conversion.
    ///
    /// Restored on `main` after #850 removed it. The derivation it guards
    /// survived that PR; only the proof went, and without the proof reverting
    /// the one-line derivation to `false` goes green again. This effort's line
    /// kept its copy throughout, so the merge has one test and one history.
    #[test]
    fn a_build_with_no_caps_document_still_converts_profile_7() {
        let mut p7 = dolby_vision_p8_file();
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);

        let node = capable_node();
        let review = legacy_trusted_review(&p7, &node, true, false);
        assert!(
            review.convert_dolby_vision,
            "the echo cannot carry this field, so trusting it means deriving it"
        );
        assert!(
            review.preserve_dolby_vision,
            "and the echo is still trusted"
        );
        assert!(!review.hdr10, "for every field the echo *can* carry");

        // The conversion follows the preservation, exactly as
        // `review_client_plan` clamps it: a client that declined Dolby Vision
        // is not handed a converted stream by a flag nobody looked at.
        assert!(
            !legacy_trusted_review(&p7, &node, false, false).convert_dolby_vision,
            "declining Dolby Vision declines the conversion with it"
        );

        // An operator switch still wins.
        let mut off = capable_node();
        off.dolby_vision_convert = false;
        assert!(!legacy_trusted_review(&p7, &off, true, false).convert_dolby_vision);

        // And a source the conversion cannot be built for keeps the delivery
        // it has always had, rather than being routed to an index that could
        // never exist.
        let p8 = dolby_vision_p8_file();
        assert!(
            !legacy_trusted_review(&p8, &node, true, false).convert_dolby_vision,
            "there is nothing to convert a Profile 8 source into"
        );
        let mut label_only = p7.clone();
        label_only.dolby_vision.level = None;
        assert!(
            !legacy_trusted_review(&label_only, &node, true, false).convert_dolby_vision,
            "no columns, no conversion — the record could not be built"
        );
    }

    /// A Profile 7 title with the columns the conversion needs — the source
    /// shape the whole of this milestone is about.
    fn dolby_vision_p7_file() -> MediaFile {
        let mut p7 = dolby_vision_p8_file();
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);
        p7
    }

    fn review_log() -> ReviewContext<'static> {
        ReviewContext {
            file_id: 70,
            user_id: 1,
            client_build: "safari/test",
        }
    }

    /// The *arm*, not the review it returns.
    ///
    /// `a_build_with_no_caps_document_still_converts_profile_7` above proves
    /// `legacy_trusted_review` derives the conversion. It cannot prove that a
    /// create ever reaches it — and reaching it is precisely what was broken
    /// before #842: the no-caps arm returned `None`, `apply_plan_review` never
    /// ran, and the session kept the `convert_dolby_vision: false` it was
    /// built with. Restoring `None` here (the mutation) leaves every review
    /// test green and puts raw Profile 7 back on the wire, so this is the test
    /// that has to fail.
    ///
    /// The three shapes are asserted together because the counters are one
    /// population split three ways: a create that lands in two of them, or in
    /// none, is a migration metric that can never reach zero.
    #[test]
    fn a_create_that_sends_no_caps_document_still_gets_a_review() {
        let p7 = dolby_vision_p7_file();
        let node = capable_node();
        let before = plan_derivation::snapshot();

        // The straggler: no caps document at all. It gets a review, and the
        // review carries the one field the echo could not.
        let legacy = plan_review_for(None, None, &p7, &node, true, false, NOW_MS, &review_log())
            .expect("a build that sends no caps document must still get a review");
        assert!(
            legacy.convert_dolby_vision,
            "the arm that trusts the echo has to derive the conversion, or the \
             session preserves raw Profile 7 — the pre-#842 delivery no \
             consumer decoder takes"
        );
        assert!(legacy.preserve_dolby_vision);
        assert!(!legacy.mismatched);

        // A readable v2 document is re-derived instead, from the client's own
        // enumeration. This client takes 8 and not 7, which is the population
        // the conversion exists for, so it converts too — by a different
        // route, which is the point of asserting both.
        let rederived = plan_review_for(
            Some(&dolby_vision_client()),
            None,
            &p7,
            &node,
            true,
            false,
            NOW_MS,
            &review_log(),
        )
        .expect("a v2 document is re-derived, not discarded");
        assert!(rederived.convert_dolby_vision);
        assert!(!rederived.mismatched);

        // A document this build cannot read is not a document, and it lands on
        // the same trust path a build sending none takes. Not `None`: that
        // skips `apply_plan_review` altogether, which leaves the echoed
        // `preserve_dolby_vision` unclamped beside a `convert_dolby_vision`
        // nobody set — raw Profile 7, from any client that can post a `"v":1`
        // body.
        let unusable = caps_v2(r#"{"v":1,"video":[],"audio":[],"containers":[]}"#);
        let unreadable = plan_review_for(
            Some(&unusable),
            None,
            &p7,
            &node,
            true,
            false,
            NOW_MS,
            &review_log(),
        )
        .expect(
            "an unreadable caps document falls through to the client's echo, which is a review",
        );
        assert!(
            unreadable.convert_dolby_vision,
            "and the echo it falls through to derives the conversion, exactly as \
             the no-caps arm does"
        );

        // One create, one counter — asserted as a lower bound, because the
        // counters are process-global `AtomicU64`s and nineteen other tests in
        // this binary call `review_client_plan` on other threads. An exact
        // delta here is a test that fails on an unrelated change, under
        // another test's name; a lower bound still goes red under every
        // mutation of the arms above, which is the property being pinned.
        let after = plan_derivation::snapshot();
        assert!(
            after.0 - before.0 >= 1,
            "the no-caps arm counts a legacy_trusted create"
        );
        assert!(
            after.1 - before.1 >= 1,
            "the unreadable-document arm counts an unusable_caps create"
        );
        assert!(
            after.2 - before.2 >= 1,
            "the v2 arm counts a rederived create"
        );
    }

    /// The session's own answer for both badge fields, for every kind of
    /// session that can carry Dolby Vision.
    ///
    /// Read off the session that was built rather than the decision that
    /// suggested one, because a burn or a forced rung produces a delivery
    /// `/decision` never promised. The two fields are asserted together
    /// because they are read together: a session badged `dolby_vision` whose
    /// profile is absent, or a profile on a session whose range says HDR10,
    /// is a worse answer than either field alone would be.
    #[test]
    fn a_sessions_badge_names_the_range_and_the_profile_it_actually_carries() {
        use crate::transcode::SessionKind;
        use plurx_core::transcode::OutputGrade;

        let mut p7 = dolby_vision_p8_file();
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);

        let copy = |preserve: bool, convert: bool| SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: preserve,
            convert_dolby_vision: convert,
        };
        let badge = |kind: &SessionKind| {
            (
                session_delivered_dynamic_range(Some(&p7), kind, OutputGrade::Sdr),
                session_delivered_dolby_vision_profile(Some(&p7), kind),
            )
        };

        assert_eq!(
            badge(&copy(true, true)),
            (Some("dolby_vision"), Some(8)),
            "a converting session delivers Dolby Vision, and the profile is the \
             one the conversion made — not the 7 the source row says"
        );
        assert_eq!(
            badge(&copy(true, false)),
            (Some("dolby_vision"), Some(7)),
            "the same range as the converting answer, which is why the profile \
             has to be on the wire at all"
        );
        assert_eq!(
            badge(&copy(false, false)),
            (Some("hdr10"), None),
            "a stripped stream carries no Dolby Vision to name"
        );
        assert_eq!(
            badge(&SessionKind::Transcode { height: 1080 }),
            (Some("sdr"), None),
            "no plurx encode rung produces Dolby Vision"
        );

        // A source the store could not load says nothing rather than guessing.
        assert_eq!(
            session_delivered_dolby_vision_profile(None, &copy(true, true)),
            None
        );
    }

    /// A create body with nothing set, to be spread over.
    fn bare_create() -> CreateSession {
        CreateSession {
            intent: None,
            control_sequence: None,
            playback_id: String::new(),
            request_id: None,
            previous_session_id: None,
            reopen_reason: None,
            height: None,
            quality_auto: None,
            subtitle_burn: None,
            subtitle_burn_sdr: None,
            native_subtitles: None,
            subtitle: None,
            start: None,
            audio: None,
            copy: None,
            aac: None,
            preserve_dolby_vision: None,
            hdr10: None,
            caps: None,
            overrides: None,
            audio_offset_ms: None,
            presentation: None,
            block_budget_secs: None,
            transport: None,
        }
    }

    fn resolver_state() -> AppState {
        let root = crate::test_temp_path(format!("plurx-resolve-plan-{}", uuid::Uuid::new_v4()));
        AppState::new(
            "test".to_owned(),
            std::sync::Arc::new(
                plurx_core::store::SqliteStore::open_in_memory().expect("resolver store"),
            ),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "test-node".to_owned(),
            Default::default(),
            Default::default(),
            std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
        )
    }

    fn envelope(
        recipe: u64,
        quality: plurx_core::playback::DesiredQuality,
    ) -> plurx_core::playback::MediaIntentEnvelope {
        use plurx_core::playback::{
            DesiredCodec, DesiredDynamicRange, DesiredSelection, DesiredSubtitles,
            MediaIntentEnvelope,
        };
        MediaIntentEnvelope {
            lifetime_id: "lifetime-a".to_owned(),
            recipe_revision: recipe,
            destination_revision: 1,
            transport_revision: 1,
            selection: DesiredSelection {
                quality,
                codec: DesiredCodec::Auto,
                dynamic_range: DesiredDynamicRange::Auto,
                audio_track: None,
                audio_offset_ms: 0,
                subtitles: DesiredSubtitles::Off,
            },
        }
    }

    fn a_viewer() -> plurx_core::domain::User {
        plurx_core::domain::User {
            id: 7,
            username: "paul".to_owned(),
            password_hash: String::new(),
            is_admin: false,
            created_at: 0,
        }
    }

    async fn create_with(
        state: &AppState,
        body: CreateSession,
    ) -> Result<Json<StartResponse>, ApiError> {
        create(
            crate::http::extract::AuthUser(a_viewer()),
            State(state.clone()),
            AxPath(404_404),
            HeaderMap::new(),
            super::super::network::RemoteAddress(None),
            Json(body),
        )
        .await
    }

    #[tokio::test]
    async fn constrained_hevc_create_without_hls_is_refused_before_request_admission() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};

        let state = resolver_state();
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "HEVC create guard".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "HEVC copy ingress".into(),
                year: Some(2026),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let user = state
            .store
            .create_user("hevc-guard", "hash", false)
            .await
            .expect("user");
        for (case, extradata_size) in [("minimal", 23), ("complete", 97)] {
            let file_id = state
                .store
                .upsert_file(
                    item,
                    &format!("/media/{case}-hvcc.mp4"),
                    1_000 + extradata_size,
                    1,
                    &ProbeResult {
                        duration_ms: Some(60_000),
                        container: Some("mp4".into()),
                        video_codec: Some("hevc".into()),
                        video_codec_tag: Some("hvc1".into()),
                        field_order: None,
                        video_profile: Some("Main".into()),
                        width: Some(1920),
                        height: Some(1080),
                        bit_depth: Some(8),
                        raw_json: Some(format!(
                            r#"{{"streams":[{{"codec_type":"video","codec_name":"hevc","codec_tag_string":"hvc1","extradata_size":{extradata_size}}}]}}"#
                        )),
                        ..Default::default()
                    },
                )
                .await
                .expect("file");
            let request_id = format!("hevc-guard-{case}");
            let playback_id = format!("hevc-guard-player-{case}");
            let caps = caps_v2(
                r#"{"v":2,
                    "video":[{"codec":"hevc","present":["sdr"],"max_height":2160}],
                    "audio":["aac"],"containers":["mp4"],
                    "transports":["progressive"],
                    "progressive_hevc_sample_entries":["hvc1"],
                    "display":{"hdr":false,"dolby_vision":false}}"#,
            );
            let error = match create(
                crate::http::extract::AuthUser(user.clone()),
                State(state.clone()),
                AxPath(file_id),
                HeaderMap::new(),
                super::super::network::RemoteAddress(None),
                Json(CreateSession {
                    playback_id: playback_id.clone(),
                    request_id: Some(request_id.clone()),
                    copy: Some(true),
                    caps: Some(caps),
                    ..bare_create()
                }),
            )
            .await
            {
                Ok(_) => panic!("{case}: a client that omitted HLS cannot execute copy-HLS"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                ApiError::Typed {
                    status: StatusCode::CONFLICT,
                    code: "unsupported_hevc_delivery",
                    ..
                }
            ));

            let incarnation = uuid::Uuid::new_v4().to_string();
            let now = unix_ms();
            assert!(matches!(
                state
                    .store
                    .claim_media_session_request(
                        user.id,
                        &request_id,
                        &"c".repeat(64),
                        &playback_id,
                        &incarnation,
                        now,
                        now.saturating_add(60_000),
                    )
                    .await
                    .expect("inspect request admission"),
                MediaSessionRequestClaim::Acquired { .. }
            ));
            assert!(state
                .store
                .fail_media_session_request(user.id, &request_id, &incarnation, unix_ms())
                .await
                .expect("settle inspection claim"));
        }
    }

    /// The ask is durable before the create is answered — proved by a create
    /// that is never answered successfully at all.
    ///
    /// §1 asks for canonical desired ownership persisted *before* an
    /// intent-changing request is reported accepted. A test on a create that
    /// succeeds cannot tell that ordering from the opposite one, because both
    /// end with a row and a 200. This one asks for a file that does not exist,
    /// so the create fails — and the row is there anyway. That is only true if
    /// the write happens before the request is decided, which is the property.
    ///
    /// It also happens to be the honest behaviour: a viewer who asked for
    /// something asked for it, whether or not the session they asked through
    /// could be built.
    /// An `AppState` whose store can actually serve a create.
    ///
    /// `HlsDeliveryFixture` cannot: its file is `/media/Heat.mkv`, a path with
    /// no bytes behind it, so no fragment index can exist for it and every
    /// create against it stops at `vod_index_pending` before reaching anything
    /// worth testing. That is fine for what that fixture is for and fatal for
    /// a test about what a create *decides*, because a create refused for an
    /// unrelated reason is indistinguishable from one refused for the right
    /// one.
    ///
    /// So this puts a real encoded source behind a real file row and indexes
    /// it, which is the combination the vodserve suite already builds and the
    /// HTTP suite never had. It costs an ffmpeg index build, which is why it
    /// is a helper rather than something every test pays for.
    async fn servable_state() -> (AppState, plurx_core::domain::User, i64) {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        plurx_core::testfixtures::require_ffmpeg();
        let source = plurx_core::testfixtures::source("clean-cra");
        let store: Arc<dyn plurx_core::store::Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "L".into(),
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
                title: "Fixture".into(),
                year: Some(2026),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let metadata = std::fs::metadata(&source).expect("the fixture source exists");
        let file_id = store
            .upsert_file(
                item,
                source.to_str().expect("a utf-8 fixture path"),
                metadata.len() as i64,
                1,
                &ProbeResult {
                    duration_ms: Some(12_000),
                    ..ProbeResult::default()
                },
            )
            .await
            .expect("file");
        let file = store
            .get_file(file_id)
            .await
            .expect("read back")
            .expect("the file row is there");

        // The index the VOD path refuses without, built once for the whole
        // test binary.
        //
        // Not a micro-optimisation. Indexing runs ffmpeg over the fixture, and
        // doing that once per test put enough CPU into this binary to tip two
        // timing-sensitive `seek_coalescing` tests over on the coverage runner,
        // where instrumentation makes everything slower — they passed before
        // this fixture existed and failed after. The index is the same bytes
        // every time, so building it three times was only ever cost, and the
        // cost landed on someone else's test.
        static INDEX: tokio::sync::OnceCell<plurx_core::segplan::FragmentIndex> =
            tokio::sync::OnceCell::const_new();
        let index = INDEX
            .get_or_init(|| async {
                let runtime = crate::test_tempdir().expect("runtime cache dir");
                let outcome = crate::fragindex::build(
                    &file,
                    plurx_core::transcode::CopyVideoOptions::new(
                        crate::ffmpeg::has_dovi_rpu().await,
                        false,
                    ),
                    runtime.path(),
                    Duration::from_secs(120),
                )
                .await;
                let crate::fragindex::IndexOutcome::Built(index) = outcome else {
                    panic!("the fixture must index: {outcome:?}");
                };
                *index
            })
            .await;
        store
            .put_fragment_index(file_id, index)
            .await
            .expect("store the index");

        let user = store
            .create_user("servable", "hash", false)
            .await
            .expect("viewer");
        let root = crate::test_temp_path(format!("plurx-servable-{}", uuid::Uuid::new_v4()));
        let state = AppState::new(
            "test".to_owned(),
            Arc::clone(&store),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "test-node".to_owned(),
            Default::default(),
            Default::default(),
            std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        (state, user, file_id)
    }
