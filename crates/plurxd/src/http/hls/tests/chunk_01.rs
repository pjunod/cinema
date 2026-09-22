    /// The release class comes from the durable recipe, so a remote owner, an
    /// idempotent replay and an owner takeover all read the answer the create
    /// wrote — and a recipe an older node wrote, which carries no transport
    /// at all, reads as conservative.
    #[test]
    fn seek_scratch_release_class_comes_from_the_durable_recipe() {
        use plurx_core::domain::MediaSessionRoute;

        fn route_with(recipe: serde_json::Value) -> MediaSessionRoute {
            MediaSessionRoute {
                incarnation_id: "i".to_owned(),
                session_id: "s".to_owned(),
                user_id: 1,
                playback_id: "p".to_owned(),
                request_fingerprint: "f".to_owned(),
                owner_node_id: "n".to_owned(),
                owner_epoch: 1,
                lease_expires_at_ms: 0,
                state: "active".to_owned(),
                recipe_json: recipe.to_string(),
                response_json: "{}".to_owned(),
                produced_playable_through_ms: 0,
                fetched_through_ms: 0,
                media_origin_ms: 0,
                media_sequence: 0,
                discontinuity_sequence: 0,
                updated_at_ms: 0,
                recovery_epoch: String::new(),
                terminal_reason: None,
                publication_ready_at_ms: 0,
                drain_deadline_ms: None,
            }
        }

        let recipe = |transport: Option<&str>| {
            let mut request = serde_json::json!({
                "file_id": 1,
                "playback_id": "p",
                "request_id": "i",
                "control_sequence": serde_json::Value::Null,
                "automatic": false,
                "previous_session_id": serde_json::Value::Null,
                "reopen_reason": serde_json::Value::Null,
                // `SessionKind` is internally tagged on its own `kind`.
                "kind": {"kind": "copy", "aac": false, "preserve_dolby_vision": false,
                         "convert_dolby_vision": false},
                "start_seconds": 0.0,
                "audio_index": serde_json::Value::Null,
                "subtitle_burn": serde_json::Value::Null,
                "audio_offset_ms": 0,
                "hdr10": false,
                "presentation": "vod",
            });
            if let Some(transport) = transport {
                request["transport"] = transport.into();
            }
            serde_json::json!({
                "protocol_version": 1,
                "incarnation_id": "i",
                "user_id": 1,
                "source_size": 1,
                "source_mtime": 1,
                "request": request,
            })
        };

        assert_eq!(
            exact_release_class(&route_with(recipe(Some("hlsjs")))),
            crate::transcode::ReleaseClass::HlsJs
        );
        assert_eq!(
            exact_release_class(&route_with(recipe(Some("native")))),
            crate::transcode::ReleaseClass::Conservative
        );
        assert_eq!(
            exact_release_class(&route_with(recipe(None))),
            crate::transcode::ReleaseClass::Conservative,
            "a recipe from before this field keeps the original promise"
        );
        assert_eq!(
            exact_release_class(&route_with(serde_json::json!({"not": "a recipe"}))),
            crate::transcode::ReleaseClass::Conservative,
            "an unreadable recipe shortens nothing"
        );
    }

    /// A deliberate new play mints a budget; every continuation inherits one.
    ///
    /// The whole decoder-recovery budget rests on this one expression. Mint on
    /// a continuation and a viewer whose file cannot be decoded gets a fresh
    /// automatic retry after every reopen, seek and track change — an
    /// unbounded loop against a decoder that will never succeed, which is the
    /// outcome the ledger was built to make impossible.
    #[test]
    fn a_new_play_mints_a_budget_and_every_continuation_inherits_one() {
        use plurx_core::domain::MediaSessionRoute;

        let route = |recovery_epoch: &str| MediaSessionRoute {
            recovery_epoch: recovery_epoch.to_owned(),
            drain_deadline_ms: None,
            incarnation_id: "inc".to_owned(),
            session_id: "sess".to_owned(),
            user_id: 1,
            playback_id: "playback".to_owned(),
            request_fingerprint: "f".repeat(64),
            owner_node_id: "node".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: 0,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: 0,
        };

        // No predecessor: the playback pointer named nobody, so this is a
        // deliberate new play and it gets one budget.
        let minted = super::recovery_epoch_for(None);
        assert!(
            uuid::Uuid::parse_str(&minted).is_ok(),
            "a minted epoch is server-owned and unguessable, never derived \
             from anything a client sends"
        );
        assert_ne!(
            minted,
            super::recovery_epoch_for(None),
            "two deliberate plays are two budgets"
        );

        // A predecessor: this continues a playback that already has one.
        assert_eq!(
            super::recovery_epoch_for(Some(&route("epoch-one"))),
            "epoch-one",
            "a continuation spends the budget it already has"
        );

        // A predecessor from before the column existed. Inherited as empty —
        // no budget — rather than minted. Minting here would give every
        // continuation of a pre-upgrade session a fresh allowance, which is
        // the failure this prevents arriving through an upgrade.
        assert_eq!(
            super::recovery_epoch_for(Some(&route(""))),
            "",
            "a session that never had a budget does not gain one by continuing"
        );
    }

    /// One start mints one epoch, and the session and the durable row get the
    /// same one.
    ///
    /// `recovery_epoch_for` is deliberately not pure: with no predecessor it
    /// draws a fresh UUID, and the test above asserts that two calls differ.
    /// That makes "call it where each value is needed" — which reads as the
    /// careful choice, and which this file's own comment used to recommend —
    /// the way to give one new play two budgets: the live session believes in
    /// an epoch that reaches no durable row, and the next continuation
    /// inherits the other and finds it unspent. One automatic recovery per
    /// attempt against a file that cannot be decoded is the loop the ledger
    /// exists to close, so this is pinned rather than left to review.
    ///
    /// A control-flow property of one `async fn` that needs a store, a
    /// transcode manager and an authenticated owner to reach, so nothing
    /// executes it. Pinned where it lives, like the reservation above.
    #[test]
    fn one_start_mints_one_recovery_epoch_for_both_the_session_and_the_row() {
        // The production half only: this module's own text contains every
        // literal it asserts about, so counting the whole file counts the
        // test. That used to need a split at `\nmod tests {`; since the tests
        // moved out of `hls.rs` into this directory, the file *is* the
        // production half and the split has nothing to find — it returned
        // `None` and this `expect` panicked. Read the file whole instead.
        let source = include_str!("../../hls.rs");
        assert_eq!(
            source
                .matches("recovery_epoch_for(activation_predecessor.as_ref())")
                .count(),
            1,
            "the epoch is minted once per start; a second call mints a second budget"
        );
        assert!(
            source.contains(
                "let recovery_epoch = recovery_epoch_for(activation_predecessor.as_ref());"
            ),
            "the one mint is bound to a local"
        );
        let after_mint = source
            .split_once("let recovery_epoch = recovery_epoch_for(activation_predecessor.as_ref());")
            .expect("the mint")
            .1;
        assert!(
            after_mint.contains("recovery_epoch: recovery_epoch.clone(),"),
            "the session identity and the activation both read that local"
        );
        assert_eq!(
            after_mint
                .matches("recovery_epoch: recovery_epoch.clone(),")
                .count(),
            2,
            "both the session identity and the durable activation, and nothing else"
        );
        // And the mint precedes placement, because the session that carries it
        // is built by the task the placement loop spawns.
        let placement = source
            .find("let mut start_task = tokio::spawn(async move {")
            .expect("the placement task");
        let mint = source
            .find("let recovery_epoch = recovery_epoch_for(activation_predecessor.as_ref());")
            .expect("the mint");
        assert!(
            mint < placement,
            "a mint after placement cannot reach the session placement starts"
        );
    }

    /// The reservation is a control-flow property of one `async fn` that needs
    /// a store, a transcode manager and an authenticated owner to reach, so
    /// nothing executes it. Pin it where it lives instead: reverting the
    /// confirmation to the bare lease deadline, or handing the recovery read
    /// the reserved deadline it must never be given, fails here.
    #[test]
    fn the_confirmation_reserves_recovery_budget_inside_the_owner_lease() {
        let source = include_str!("../../hls.rs");
        let reservation = source
            .split_once("    let confirmation_deadline = activation_confirmation_deadline(")
            .expect("the confirmation must be awaited on its own reserved deadline")
            .1
            .split_once("    let route = match confirmation {")
            .expect("confirmation settlement boundary")
            .0;
        assert!(
            reservation.contains("tokio::time::timeout_at(")
                && reservation.contains("confirmation_deadline,")
                && reservation.contains("settle_media_session_activation("),
            "the reserved deadline must bound the confirmation write itself"
        );
        assert!(
            !reservation.contains("timeout_at(lease_deadline"),
            "the confirmation must not be awaited on the raw owner lease"
        );
        // The recovery read is the beneficiary; giving it the reserved
        // deadline would hand back the very budget the reservation bought.
        assert!(
            source.contains("wait_for_confirmed_activation(&state, &activation, lease_deadline)"),
            "the committed-write recovery read must keep the full owner lease"
        );
        // A reservation that is taken only when it fits whole is the cut that
        // vanished in the case that needed it.
        let split = source
            .split_once("fn activation_confirmation_deadline(")
            .expect("the lease split must be one named function")
            .1;
        // Measuring anything other than the lease actually left would make
        // the split degrade against a constant, which is not a split at all.
        const REMAINING: &str =
            "lease_deadline.saturating_duration_since(tokio::time::Instant::now())";
        assert!(
            split.contains(REMAINING)
                && split.contains("ACTIVATION_CONFIRMATION_RECOVERY_MARGIN.min(remaining / 2)"),
            "the reservation must degrade with the remaining lease, never be skipped"
        );
    }

    /// M3's acceptance is that a seek storm starts production for exactly one
    /// target. The latch decides which; this decides whether a restart that
    /// arrives for a superseded one is refused. The handler cannot be reached
    /// from a test -- it needs a store, a transcode manager and an
    /// authenticated user -- so the decision lives in one function and this
    /// pins it.
    mod restart_supersession {
        use super::super::restart_is_superseded;
        use crate::playback_control::SettledTarget;

        fn settled(sequence: u64, anchor_ms: i64) -> Option<SettledTarget> {
            Some(SettledTarget {
                sequence,
                anchor_ms,
            })
        }

        #[test]
        fn a_client_that_sends_no_sequence_is_never_refused() {
            // Every client before the field existed, and Apple and Android
            // until they send it. Absent means do the work.
            assert!(!restart_is_superseded(settled(90, 1_800_000), None, 5.0));
        }

        #[test]
        fn a_playback_with_no_settled_target_is_never_refused() {
            // No session, a retired actor, or a client that has not exchanged
            // yet. Nothing to order against, so do the work.
            assert!(!restart_is_superseded(None, Some(3), 5.0));
        }

        #[test]
        fn a_restart_for_a_destination_the_client_left_is_refused() {
            // Sequence 3 asked for 5 s; the client has since settled on 1,800 s
            // at sequence 90. That session is waste before it spawns.
            assert!(restart_is_superseded(settled(90, 1_800_000), Some(3), 5.0));
        }

        #[test]
        fn the_honest_seek_arriving_next_is_never_the_one_refused() {
            // The create can reach the server before its own snapshot does, so
            // only a strictly later exchange supersedes. Equal and later both
            // proceed -- refusing either is the failure this milestone exists
            // to prevent.
            assert!(!restart_is_superseded(
                settled(90, 1_800_000),
                Some(90),
                5.0
            ));
            assert!(!restart_is_superseded(
                settled(90, 1_800_000),
                Some(91),
                5.0
            ));
        }

        #[test]
        fn a_seek_that_lands_beside_its_target_is_not_refused() {
            // 1,799.64 s against a settled 1,800 s is the same destination;
            // seeking is not exact, and refusing this would make every honest
            // seek cancel its own session.
            assert!(!restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                1_799.64
            ));
        }

        #[test]
        fn a_malformed_start_cannot_cancel_a_session_the_viewer_wants() {
            // NaN and a negative both read as the head. That can only make a
            // restart look *less* superseded, which is the safe direction: a
            // malformed body must not be able to refuse work.
            assert!(!restart_is_superseded(settled(90, 0), Some(3), f64::NAN));
            assert!(!restart_is_superseded(settled(90, 0), Some(3), -12.0));
            // ... and it is still refused when the settled target really is
            // somewhere else, so the clamp does not become a bypass.
            assert!(restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                f64::NAN
            ));
        }

        #[test]
        fn an_absurd_start_saturates_instead_of_wrapping_into_a_real_anchor() {
            // The float-to-int cast saturates. Were it to wrap, a huge start
            // could land on a small anchor and compare equal to a destination
            // the viewer actually wants.
            assert!(restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                f64::MAX
            ));
            assert!(restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                f64::INFINITY
            ));
        }
    }
    use super::*;

    /// The replacement seam, which is where a viewer's quality change actually
    /// arrives.
    ///
    /// lab6 recorded one decision against 949 accepted exchanges because
    /// `ControlState::last_selection` only sees a change *within* a session,
    /// and Apple's `selectQuality` replaces the session instead. Keyed by
    /// playback id rather than by `previous_session_id`, which the Apple
    /// client sets only on a stall reopen — a gate needing that field on a
    /// non-stall reopen matches nothing, which is exactly what the first
    /// attempt at this measured.
    #[test]
    fn a_replacement_is_measured_against_the_session_it_replaced() {
        let selection = |height: i64| crate::playback_control::EffectiveSelection {
            height,
            ..sample_effective_selection()
        };
        let playback = format!("seam-playback-{}", std::process::id());
        let first = format!("{playback}-session-1");
        let second = format!("{playback}-session-2");

        // The playback's first session has nothing to be measured against.
        assert_eq!(
            remember(&playback, &first, &selection(2160), &asked_1080()),
            None,
        );
        // Its later exchanges answer nothing, so a session is never measured
        // against itself.
        assert_eq!(
            remember(&playback, &first, &selection(2160), &asked_1080()),
            None,
        );
        // A new session for the same playback is the viewer's quality change,
        // and it is measured against what the old session was delivering.
        let (previous, previous_grade) =
            remember(&playback, &second, &selection(1080), &asked_720())
                .expect("the replaced session is remembered");
        assert_eq!(previous.height, 2160);
        assert_eq!(previous_grade, seam_grade());
        // Exactly once: the replacement's later exchanges answer nothing, or
        // one change would be counted for the life of the session.
        assert_eq!(
            remember(&playback, &second, &selection(1080), &asked_720()),
            None,
        );
        // And a change back is its own transition.
        let (back, _) = remember(
            &playback,
            &format!("{playback}-session-3"),
            &selection(2160),
            &asked_1080(),
        )
        .expect("the second session is remembered too");
        assert_eq!(back.height, 1080);
        // A playback this node has never served is not a measurement.
        assert_eq!(
            remember(
                &format!("seam-other-{}", std::process::id()),
                "some-session",
                &selection(720),
                &asked_720(),
            ),
            None,
        );
    }

    /// Three gates, and the count fills with things no viewer did if any one
    /// of them is dropped.
    ///
    /// Found by review, all three confirmed against the shipped clients: a
    /// playback id outlives the film (Apple mints one per `PlayerView` and
    /// autoplay-next is `stop()`-then-`start()` on the same controller), and
    /// the client reopens for its own reasons — a Dolby Vision fallback to the
    /// HDR10 base or to a compatibility transcode, a burn-in retry, a failure
    /// retry, an out-of-window seek — none of which declare a `reopen_reason`
    /// and all of which move the delivered recipe on exactly the two axes
    /// whose counts are the evidence about `PREPARED_AXIS`.
    #[test]
    fn a_replacement_the_viewer_did_not_ask_for_is_not_measured() {
        let selection = |height: i64| crate::playback_control::EffectiveSelection {
            height,
            ..sample_effective_selection()
        };

        // An episode boundary: same playback, new session, different film.
        let binge = format!("seam-binge-{}", std::process::id());
        assert_eq!(
            remember(&binge, "episode-1", &selection(2160), &asked_1080()),
            None,
        );
        assert_eq!(
            remember_delivered_selection(
                &binge,
                "episode-2",
                SEAM_FILE + 1,
                &asked_720(),
                &selection(1080),
                seam_grade(),
            ),
            None,
            "a new film is not a viewer changing quality",
        );

        // The client's own recovery: same film, new session, unchanged ask.
        let fallback = format!("seam-fallback-{}", std::process::id());
        assert_eq!(
            remember(&fallback, "before", &selection(2160), &asked_1080()),
            None,
        );
        assert_eq!(
            remember(&fallback, "after", &selection(1080), &asked_1080()),
            None,
            "a Dolby Vision fallback changes what is delivered, not what was asked",
        );

        // And the real thing still measures, so the gates are not simply off.
        let real = format!("seam-real-{}", std::process::id());
        assert_eq!(
            remember(&real, "one", &selection(2160), &asked_1080()),
            None
        );
        assert!(
            remember(&real, "two", &selection(1080), &asked_720()).is_some(),
            "same film, new session, and the viewer asked for something else",
        );
    }

    /// The table is bounded, because a node serves unboundedly many playbacks.
    #[test]
    fn the_remembered_selections_are_bounded() {
        let tag = format!("bound-{}", std::process::id());
        let first = format!("{tag}-0");
        let kept = format!("{tag}-kept");
        remember(&first, "s", &sample_effective_selection(), &asked_1080());
        remember(&kept, "s", &sample_effective_selection(), &asked_1080());
        for index in 1..=MAX_REMEMBERED_SELECTIONS {
            remember(
                &format!("{tag}-{index}"),
                "s",
                &sample_effective_selection(),
                &asked_1080(),
            );
            // Touched throughout, and therefore kept: eviction is by last
            // touch, not by first sight. Insertion order would drop the
            // two-hour film first, and mid-film is exactly when a viewer
            // changes quality.
            remember(&kept, "s", &sample_effective_selection(), &asked_1080());
        }
        let guard = DELIVERED_SELECTIONS.lock().expect("lock");
        let table = guard.as_ref().expect("table");
        assert!(table.by_playback.len() <= MAX_REMEMBERED_SELECTIONS);
        assert_eq!(table.by_playback.len(), table.order.len());
        assert!(
            !table.by_playback.contains_key(&first),
            "the least recently touched entry is evicted",
        );
        assert!(
            table.by_playback.contains_key(&kept),
            "and one touched throughout survives however many arrive after it",
        );
    }

    /// One film, one grade — the seam's other two gates held constant so a
    /// test that means to vary the session varies only the session.
    const SEAM_FILE: i64 = 4_242;

    fn seam_grade() -> crate::playback_control::GradeIntent {
        crate::playback_control::GradeIntent {
            hdr10: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        }
    }

    fn remember(
        playback: &str,
        session: &str,
        delivered: &crate::playback_control::EffectiveSelection,
        asked: &crate::playback_control::ClientSelection,
    ) -> Option<(
        crate::playback_control::EffectiveSelection,
        crate::playback_control::GradeIntent,
    )> {
        remember_delivered_selection(playback, session, SEAM_FILE, asked, delivered, seam_grade())
    }

    fn asked_at(
        quality: crate::playback_control::QualitySelection,
    ) -> crate::playback_control::ClientSelection {
        crate::playback_control::ClientSelection {
            quality,
            audio_track: Some(0),
            subtitle: crate::playback_control::SubtitleSelection {
                mode: crate::playback_control::SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: crate::playback_control::CodecPolicy::Auto,
            dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
        }
    }

    fn asked_1080() -> crate::playback_control::ClientSelection {
        asked_at(crate::playback_control::QualitySelection::Manual { height: 1080 })
    }

    fn asked_720() -> crate::playback_control::ClientSelection {
        asked_at(crate::playback_control::QualitySelection::Manual { height: 720 })
    }

    /// A minimal delivered selection; only `height` matters to these tests.
    fn sample_effective_selection() -> crate::playback_control::EffectiveSelection {
        crate::playback_control::EffectiveSelection {
            height: 1080,
            quality_auto: false,
            codec: "server_selected".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
            audio_track: None,
            audio_offset_ms: 0,
            subtitle_burn: None,
        }
    }

    /// A fixed clock for the plan-review tests. `review_client_plan` reads it
    /// to decide which of the client's learned limits still apply, so a test
    /// on the real clock would rot the moment a fixture aged out.
    const NOW_MS: i64 = 1_756_400_000_000;
    use crate::transcode::HlsDeliveryFixture;
    use http_body_util::BodyExt;
    use std::time::Duration;

    async fn activate_ready(
        store: &Arc<dyn plurx_core::store::Store>,
        mut activation: MediaSessionActivation,
    ) -> MediaSessionRoute {
        activation.publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED;
        store
            .activate_media_session(&activation)
            .await
            .expect("prepare media route")
            .expect("media route preparation accepted");
        let publication_ready_at_ms = activation
            .expected_predecessor_incarnation_id
            .as_ref()
            .map_or(0, |_| {
                activation
                    .now_ms
                    .saturating_add(plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS)
            });
        store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms,
                },
                activation.now_ms,
            )
            .await
            .expect("confirm media route")
            .expect("media route confirmation accepted")
    }

    async fn activate_fixture_route(
        fixture: &HlsDeliveryFixture,
        session_id: &str,
        playback_id: &str,
    ) {
        let user = fixture
            .store
            .create_user(playback_id, "hash", false)
            .await
            .expect("status fixture user");
        let now_ms = unix_ms();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_owned(),
                user_id: user.id,
                playback_id: playback_id.to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: now_ms.saturating_add(60_000),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn driven_local_body_rejects_queued_data_after_terminal_failure() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let (accepted, rejected) = tokio::sync::oneshot::channel();
        sender
            .send(DrivenLocalChunk {
                bytes: Bytes::from_static(b"stale-chunk"),
                accepted,
            })
            .await
            .expect("body receiver");
        let terminal = StreamedBodyTerminal::new();
        terminal.fail(
            std::io::ErrorKind::TimedOut,
            "body deadline expired".to_owned(),
        );
        drop(sender);

        let mut body = driven_local_body(
            receiver,
            terminal,
            tokio::time::Instant::now() + Duration::from_secs(60),
        );
        let error = body
            .frame()
            .await
            .expect("terminal error frame")
            .expect_err("the driven body must expose the producer failure");
        assert!(error.to_string().contains("body deadline expired"));
        assert!(body.frame().await.is_none());
        assert!(
            rejected.await.is_err(),
            "stale bytes must not be acknowledged"
        );
    }

    #[test]
    fn same_incarnation_publication_rejection_is_retryable_not_not_found() {
        let error = response_publication_rejection(
            crate::transcode::MediaResponsePublicationRejection::StateChanged,
        );
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "response_state_changed",
                ..
            }
        ));
    }

    #[test]
    fn beyond_frontier_publication_rejection_is_typed_producer_ended() {
        let error = response_publication_rejection(
            crate::transcode::MediaResponsePublicationRejection::ProducerEnded(
                "process_exit".to_owned(),
            ),
        );
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::BAD_GATEWAY,
                code: "producer_ended",
                ref message,
            } if message.contains("process_exit")
        ));
    }

    #[test]
    fn if_none_match_uses_http_weak_comparison() {
        assert!(etag_matches(Some("W/\"artifact\""), "\"artifact\""));
        assert!(etag_matches(
            Some("\"other\", W/\"artifact\""),
            "\"artifact\""
        ));
        assert!(etag_matches(Some("*"), "\"artifact\""));
        assert!(!etag_matches(Some("W/\"other\""), "\"artifact\""));
    }

    #[test]
    fn vod_resurrection_uncertainty_is_retryable_not_not_found() {
        let error = vod_resurrection_unavailable();
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "vod_resurrection_unavailable",
                ..
            }
        ));
    }

    #[test]
    fn publication_subdeadline_never_outlives_its_request() {
        let request_deadline = Instant::now();
        assert_eq!(
            response_publication_deadline_before(request_deadline),
            request_deadline
        );
    }

    #[tokio::test(start_paused = true)]
    async fn prepared_body_rejects_buffered_bytes_after_absolute_lifetime() {
        let response = bound_admitted_media_body(Response::new(Body::from("late bytes")));
        tokio::time::advance(MAX_ADMITTED_MEDIA_BODY_LIFETIME).await;
        let error = response
            .into_body()
            .into_data_stream()
            .next()
            .await
            .expect("expired prepared body terminal item")
            .expect_err("expired prepared bytes must not be exposed");
        assert!(error.to_string().contains("maximum admitted body lifetime"));
    }

    #[tokio::test(start_paused = true)]
    async fn streamed_response_settlement_capacity_is_bounded_before_visibility() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&slots)
            .acquire_owned()
            .await
            .expect("first settlement permit");
        let deadline = tokio::time::Instant::now().into_std() + Duration::from_millis(1);
        let error = reserve_response_completion_from(slots, deadline)
            .await
            .expect_err("a second streamed response must not exceed settlement capacity");
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "response_completion_capacity",
                ..
            }
        ));
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_store_wait_uses_the_absolute_attempt_deadline() {
        let deadline = tokio::time::Instant::now() + TERMINAL_COMMIT_RETRY_BUDGET;
        let stalled = tokio::spawn(async move {
            terminal_io_before(deadline, std::future::pending::<()>()).await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(TERMINAL_COMMIT_RETRY_BUDGET).await;
        assert_eq!(
            stalled.await.expect("bounded Store wait task"),
            None,
            "a Store operation cannot outlive the terminal attempt budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn local_start_pin_timeout_is_retryable_service_unavailable() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let pending = pin_shared_session_for_local_start(
            deadline,
            std::future::pending::<Result<bool, StoreError>>(),
        );
        tokio::pin!(pending);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(matches!(
            pending.await,
            Err(ApiError::ServiceUnavailable(message))
                if message == crate::transcode::start_infrastructure_error(
                    "shared cache pin exceeded the start deadline"
                )
        ));
    }

    #[tokio::test]
    async fn active_durable_route_without_local_worker_maps_to_owner_transition() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-worker").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: generation.clone(),
            user_id: 7,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: true,
            library_channel: None,
            request: crate::transcode::SessionRequest {
                control_sequence: None,
                file_id: 1,
                playback_id: "control-transition".to_owned(),
                request_id: Some(generation.clone()),
                automatic: true,
                previous_session_id: None,
                reopen_reason: None,
                kind: crate::transcode::SessionKind::Transcode { height: 720 },
                start_seconds: 0.0,
                audio_index: None,
                subtitle_burn: None,
                audio_offset_ms: 0,
                hdr10: false,
                presentation: crate::transcode::Presentation::Vod,
                block_budget_secs: None,
                transport: None,
            },
        };
        let start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(60_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 720,
            encoder: "software".to_owned(),
            vod: false,
            ladder: vec![],
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &generation,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let route = MediaSessionRoute {
            recovery_epoch: String::new(),
            incarnation_id: generation.clone(),
            session_id,
            user_id: 7,
            playback_id: "control-transition".to_owned(),
            request_fingerprint: "a".repeat(64),
            owner_node_id: "test-node".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: unix_ms() + 60_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: serde_json::to_string(&recipe).expect("recipe"),
            response_json: serde_json::to_string(&start).expect("response"),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: unix_ms(),
            drain_deadline_ms: None,
        };
        let request = crate::playback_control::ControlRequestV1 {
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
        };

        let response = control_local(&fixture.state, &route, request, i64::MAX).await;
        assert_eq!(response.status(), StatusCode::TOO_EARLY);
    }

    #[tokio::test]
    async fn public_delete_tombstones_expired_route_but_reports_unreachable_exact_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-delete-worker").await;
        let user = fixture
            .store
            .create_user("delete-transition", "hash", false)
            .await
            .expect("delete user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id,
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "delete-transition".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "former-owner".to_owned(),
                // The row is explicitly active but already outside its owner
                // lease at current wall time: routing must call this a
                // transition, while DELETE must still tombstone it.
                lease_expires_at_ms: 2,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: 1,
            },
        )
        .await;
        let mut stale_cached_local = route.clone();
        stale_cached_local.owner_node_id = fixture.state.node_id.clone();
        fixture
            .state
            .media_sessions
            .cache_route(stale_cached_local)
            .await;

        let Err(transition) = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await
        else {
            panic!("expired active route is not local status absence")
        };
        // The recipe here is `{}`, which no successor can act on, so the
        // expired owner is loss rather than a transition. DELETE below must
        // tombstone it either way.
        assert!(matches!(
            transition,
            ApiError::TypedDetail {
                status: StatusCode::GONE,
                code: "media_owner_lost",
                ..
            }
        ));

        assert_eq!(
            delete(State(fixture.state.clone()), AxPath(session_id.clone()),).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "durable termination is not a claim that unreachable owner cleanup settled"
        );
        let ended = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("ended route lookup")
            .expect("ended route");
        assert_eq!(ended.state, "ended");
        assert_eq!(ended.owner_node_id, route.owner_node_id);
        assert_eq!(ended.incarnation_id, route.incarnation_id);
        let Err(terminal) = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await
        else {
            panic!("terminal status must not return a response")
        };
        assert!(matches!(
            terminal,
            ApiError::Typed {
                status: StatusCode::GONE,
                code: "media_session_ended",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn public_delete_confirmed_absence_still_releases_a_local_attachment() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        assert!(fixture.worker_is_registered(&session_id).await);

        assert_eq!(
            delete(State(fixture.state.clone()), AxPath(session_id.clone()),).await,
            StatusCode::NO_CONTENT
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture.worker_is_registered(&session_id).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("confirmed durable absence eventually releases the rolling attachment");
    }

    #[tokio::test]
    async fn public_delete_tombstones_and_stops_the_exact_local_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "delete-exact-unrelated").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let _owner = install_vod_http_session(&fixture, dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("delete-exact", "hash", false)
            .await
            .expect("delete exact user");
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: incarnation_id.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "delete-exact".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;

        assert_eq!(
            delete(State(fixture.state.clone()), AxPath(session_id.clone()),).await,
            StatusCode::NO_CONTENT
        );
        let ended = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("ended exact route lookup")
            .expect("ended exact route");
        assert_eq!(ended.state, "ended");
        assert_eq!(ended.incarnation_id, incarnation_id);
        assert_eq!(ended.owner_node_id, route.owner_node_id);
        assert_eq!(ended.terminal_reason.as_deref(), Some("deleted"));
        assert_eq!(
            ended.publication_ready_at_ms, 0,
            "204 requires durable exact-owner projection completion"
        );
        assert!(
            fixture
                .state
                .transcode
                .hls_session_status(&session_id)
                .await
                .is_none(),
            "204 means the exact local VOD attachment has settled, not only that its route was tombstoned"
        );
    }

    #[tokio::test]
    async fn durable_delete_tombstone_blocks_media_before_local_stop() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "delete-gap-unrelated").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let _owner = install_vod_http_session(&fixture, dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("delete-gap", "hash", false)
            .await
            .expect("delete gap user");
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id,
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "delete-gap".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        release_after_tombstone_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), Arc::clone(&pause));
        let detach_pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture
            .state
            .transcode
            .set_vod_terminal_detach_pause_for_test(Arc::clone(&detach_pause));
        let deletion = tokio::spawn({
            let state = fixture.state.clone();
            let session_id = session_id.clone();
            async move { delete(State(state), AxPath(session_id)).await }
        });
        tokio::time::timeout(Duration::from_secs(5), detach_pause.wait())
            .await
            .expect("VOD cleanup reached its pre-detach seam");
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("release reached its post-tombstone seam");
        assert!(
            fixture
                .state
                .transcode
                .vod_has_attached_reader_for_test(&session_id)
                .await,
            "the barrier is specifically inside the tombstone-to-VOD-detach gap"
        );
        let media = playlist(
            State(fixture.state.clone()),
            AxPath(session_id.clone()),
            Query(PlaylistQuery::default()),
            HeaderMap::new(),
        )
        .await;
        assert!(
            matches!(
                media,
                Err(ApiError::Typed {
                    status: StatusCode::GONE,
                    code: "media_session_ended",
                    ..
                })
            ),
            "the cached durable tombstone must refuse media before actor cleanup"
        );

        tokio::time::timeout(Duration::from_secs(5), detach_pause.wait())
            .await
            .expect("release VOD detach");
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("release durable deletion");
        assert_eq!(
            deletion.await.expect("delete gap task"),
            StatusCode::NO_CONTENT
        );
        release_after_tombstone_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    #[tokio::test]
    async fn lingering_local_status_cannot_publish_after_durable_owner_moves_remote() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        assert!(fixture.worker_is_registered(&session_id).await);
        let user = fixture
            .store
            .create_user("status-owner-moved", "hash", false)
            .await
            .expect("status user");
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "status-owner-moved".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "new-owner".to_owned(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;

        let result = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await;
        assert!(
            matches!(result, Err(ApiError::Conflict(_))),
            "a lingering predecessor actor cannot authorize stale status"
        );
        let rerouted = tokio::time::timeout(
            Duration::from_millis(250),
            status_local_before_with_relay(
                &fixture.state,
                &session_id,
                Instant::now() + Duration::from_millis(200),
                true,
            ),
        )
        .await
        .expect("status reroute is bounded by the inherited deadline");
        assert!(matches!(rerouted, Err(ApiError::ServiceUnavailable(_))));
    }

    #[tokio::test]
    async fn status_classifies_durable_absence_before_local_telemetry() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        assert!(fixture.worker_is_registered(&session_id).await);
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        status_telemetry_observers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), Arc::clone(&lookups));

        let result = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotFound("hls session"))));
        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "an unowned capability must not query a lingering local actor"
        );
        status_telemetry_observers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    async fn assert_rolling_status_before_and_after_media(
        fixture: &HlsDeliveryFixture,
        dir: &std::path::Path,
        session_id: &str,
    ) {
        activate_fixture_route(fixture, session_id, &format!("{session_id}-playback")).await;

        let before = status(
            State(fixture.state.clone()),
            AxPath(session_id.to_owned()),
            HeaderMap::new(),
        )
        .await
        .expect("status before media publication");
        assert_eq!(before.status(), StatusCode::OK);
        let before_body = axum::body::to_bytes(before.into_body(), 1024 * 1024)
            .await
            .expect("status body before media");
        let before_json: serde_json::Value =
            serde_json::from_slice(&before_body).expect("status JSON before media");
        assert!(before_json.get("producer_state").is_some());
        assert!(matches!(
            fixture.last_renewal_kind().await,
            "test-transcode-start" | "test-copy-start"
        ));

        fixture.hold_actor_managed_producer().await;
        let held_before = fixture.actor_snapshot().await;
        assert_eq!(held_before.producer_control.physical_flow, "held");
        let held = status(
            State(fixture.state.clone()),
            AxPath(session_id.to_owned()),
            HeaderMap::new(),
        )
        .await
        .expect("status while producer held");
        assert_eq!(held.status(), StatusCode::OK);
        let held_after = fixture.actor_snapshot().await;
        assert_status_snapshot_is_observational(held_before, held_after);
        fixture.mark_started().await;

        let segment_name = "seg00000.m4s";
        let segment_bytes = b"published-media";
        tokio::fs::write(dir.join(segment_name), segment_bytes)
            .await
            .expect("status fixture media");
        let media = segment(
            State(fixture.state.clone()),
            AxPath((session_id.to_owned(), segment_name.to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("published media response");
        let delivered = axum::body::to_bytes(media.into_body(), segment_bytes.len() + 1)
            .await
            .expect("published media body");
        assert_eq!(delivered.as_ref(), segment_bytes);
        fixture
            .wait_for_delivery_projection("media-segment", Some(0))
            .await;

        let published_before = fixture.actor_snapshot().await;
        let after = status(
            State(fixture.state.clone()),
            AxPath(session_id.to_owned()),
            HeaderMap::new(),
        )
        .await
        .expect("status after media publication");
        assert_eq!(after.status(), StatusCode::OK);
        let after_body = axum::body::to_bytes(after.into_body(), 1024 * 1024)
            .await
            .expect("status body after media");
        let after_json: serde_json::Value =
            serde_json::from_slice(&after_body).expect("status JSON after media");
        assert_eq!(
            after_json["delivered_bytes"],
            serde_json::Value::from(segment_bytes.len() as i64)
        );
        assert_eq!(
            fixture.last_renewal_kind().await,
            "media-segment",
            "status remains observational after publication"
        );
        let published_after = fixture.actor_snapshot().await;
        assert_status_snapshot_is_observational(published_before, published_after);
    }

    fn assert_status_snapshot_is_observational(
        mut before: crate::playback_control::RollingLeaseSnapshot,
        mut after: crate::playback_control::RollingLeaseSnapshot,
    ) {
        before.delivery.producer_progress_idle_ms = 0;
        after.delivery.producer_progress_idle_ms = 0;
        if let Some(exit) = before.delivery.producer_exit.as_mut() {
            exit.observed_idle_ms = 0;
        }
        if let Some(exit) = after.delivery.producer_exit.as_mut() {
            exit.observed_idle_ms = 0;
        }
        before.producer_control.deadline_remaining_ms =
            before.producer_control.deadline_remaining_ms.map(|_| 0);
        after.producer_control.deadline_remaining_ms =
            after.producer_control.deadline_remaining_ms.map(|_| 0);
        before.producer_control.due_overdue_ms = before.producer_control.due_overdue_ms.map(|_| 0);
        after.producer_control.due_overdue_ms = after.producer_control.due_overdue_ms.map(|_| 0);
        before.producer_control.executor_pending_decision_age_ms = before
            .producer_control
            .executor_pending_decision_age_ms
            .map(|_| 0);
        after.producer_control.executor_pending_decision_age_ms = after
            .producer_control
            .executor_pending_decision_age_ms
            .map(|_| 0);
        before.producer_control.pending_probe_deadline_remaining_ms = before
            .producer_control
            .pending_probe_deadline_remaining_ms
            .map(|_| 0);
        after.producer_control.pending_probe_deadline_remaining_ms = after
            .producer_control
            .pending_probe_deadline_remaining_ms
            .map(|_| 0);
        // The read is itself an actor command, so its ingress coordinate must
        // advance. It is ordering evidence, not playback-state mutation.
        before.producer_control.last_applied_sequence = 0;
        after.producer_control.last_applied_sequence = 0;

        assert_eq!(after.mode, before.mode);
        assert_eq!(after.last_renewal_kind, before.last_renewal_kind);
        assert_eq!(after.demand, before.demand);
        assert_eq!(after.settled_target, before.settled_target);
        assert_eq!(after.delivery, before.delivery);
        assert_eq!(after.retired, before.retired);
        assert_eq!(after.terminal, before.terminal);
        assert_eq!(after.expiration_claimed, before.expiration_claimed);
        assert_eq!(after.producer_control, before.producer_control);
    }

    #[tokio::test]
    async fn rolling_status_authz_covers_live_transcode_and_remux() {
        let transcode_dir = crate::test_tempdir().expect("transcode status directory");
        let transcode_id = uuid::Uuid::new_v4().to_string();
        let transcode =
            HlsDeliveryFixture::publish_actor_managed(transcode_dir.path(), &transcode_id).await;
        assert_rolling_status_before_and_after_media(
            &transcode,
            transcode_dir.path(),
            &transcode_id,
        )
        .await;

        let remux_dir = crate::test_tempdir().expect("remux status directory");
        let remux_id = uuid::Uuid::new_v4().to_string();
        let feed = plurx_core::testfixtures::pipe_with_distinct_hevc_sample_entries("closed-gop");
        let mut reader = plurx_core::fmp4::FragmentReader::new();
        reader.push(&feed);
        let Some(plurx_core::fmp4::Unit::Init(init)) =
            reader.next_unit().expect("parse remux init fixture")
        else {
            panic!("remux fixture must begin with init");
        };
        tokio::fs::write(remux_dir.path().join("init.mp4"), init.bytes)
            .await
            .expect("write complete remux init");
        let remux =
            HlsDeliveryFixture::publish_copy_actor_managed(remux_dir.path(), &remux_id).await;
        assert_rolling_status_before_and_after_media(&remux, remux_dir.path(), &remux_id).await;
    }

    #[tokio::test]
    async fn rolling_status_authz_does_not_change_vod_status_publication() {
        let dir = crate::test_tempdir().expect("VOD status directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let _owner = install_vod_http_session(&fixture, dir.path(), &session_id).await;
        activate_fixture_route(&fixture, &session_id, "vod-status-playback").await;

        let response = status(
            State(fixture.state.clone()),
            AxPath(session_id),
            HeaderMap::new(),
        )
        .await
        .expect("VOD status remains independently authorized");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("VOD status body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("VOD status JSON");
        assert!(json.get("producer_state").is_some());
    }

    #[tokio::test]
    async fn cancelling_public_delete_does_not_cancel_its_admitted_cleanup_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        release_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), Arc::clone(&pause));

        let request = tokio::spawn({
            let state = fixture.state.clone();
            let session_id = session_id.clone();
            async move { delete(State(state), AxPath(session_id)).await }
        });
        pause.wait().await;
        request.abort();
        assert!(request
            .await
            .expect_err("public DELETE request cancelled")
            .is_cancelled());
        pause.wait().await;

        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture.worker_is_registered(&session_id).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached DELETE cleanup survives caller cancellation");
        release_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    #[tokio::test]
    async fn commit_unknown_delete_stays_fenced_until_definitive_reconciliation() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        inject_release_error(&session_id);
        let settlement = match fixture
            .state
            .media_sessions
            .begin_release_reconciliation(&session_id)
            .await
        {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
        };

        assert_eq!(
            release_session(
                fixture.state.clone(),
                session_id.clone(),
                Arc::clone(&settlement),
                crate::vodserve::Terminal::Deleted,
                "released by client",
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            fixture
                .state
                .media_sessions
                .route_resolution_before(
                    &session_id,
                    &fixture.state.node_id,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .is_err(),
            "a commit-unknown End retains its fail-closed publication fence"
        );

        let definitive = fixture
            .store
            .end_media_session(&session_id, "deleted", unix_ms())
            .await
            .expect("idempotent release reconciliation");
        assert!(definitive.is_none(), "fixture has no durable route");
        fixture
            .state
            .media_sessions
            .complete_release_absent(&session_id)
            .await;
        fixture
            .state
            .transcode
            .complete_session_release(&session_id);
        fixture
            .state
            .media_sessions
            .complete_release_settlement(&session_id, &settlement, StatusCode::NO_CONTENT)
            .await;
        assert!(matches!(
            fixture
                .state
                .media_sessions
                .route_resolution_before(
                    &session_id,
                    &fixture.state.node_id,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("definitive absence after reconciliation"),
            DurableRouteResolution::Absent
        ));
    }

    #[tokio::test]
    async fn public_delete_refuses_visibility_when_settlement_slots_are_saturated() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "delete-slot-unrelated").await;
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&slots)
            .acquire_owned()
            .await
            .expect("hold only DELETE settlement slot");
        let status = delete_with_slots(
            fixture.state.clone(),
            uuid::Uuid::new_v4().to_string(),
            slots,
            Instant::now() + Duration::from_millis(10),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        drop(held);
    }

    /// One client instance across a drain-release exchange, so the ascending
    /// sequences are judged against each other rather than being waved through
    /// as a new instance.
    fn drain_control_request(
        generation: String,
        sequence: u64,
    ) -> crate::playback_control::ControlRequestV1 {
        crate::playback_control::ControlRequestV1 {
            intent: None,
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation,
            control_epoch: 1,
            client_instance_id: "6f1c2f8e-4a3b-4d5e-9f60-71a2b3c4d5e6".to_owned(),
            sequence,
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

    /// A `switched` on a draining predecessor releases it, and only on an
    /// exchange that was actually accepted.
    ///
    /// Every assertion here corresponds to a way the first placement of this
    /// hook was wrong. It ran before the exchange, so the packet reporting the
    /// switch was answered `410 session_ended` — hence the status assertion,
    /// which is not incidental. It ran on every packet that named the state,
    /// accepted or not — hence the refused case, because a refused exchange
    /// proves nothing about what reached a screen. And it lived in
    /// `control_inner`, which a relayed request never reaches — hence this
    /// test entering through `control_authorized`, the relay's own door, so
    /// that moving the hook back out of `control_local` fails here.
    #[tokio::test]
    async fn a_switch_releases_the_drain_through_the_relay_and_only_when_accepted() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("drain-release", "hash", false)
            .await
            .expect("drain release user");
        let recipe_json = crate::media_sessions::takeover_eligible_route(&session_id, &generation)
            .recipe_json
            .replace("\"user_id\":7", &format!("\"user_id\":{}", user.id));
        let start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(60_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 720,
            encoder: "software".to_owned(),
            vod: false,
            ladder: vec![],
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &generation,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                expected_desired_revision: None,
                incarnation_id: generation.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "drain-release".to_owned(),
                recovery_epoch: "drain-release".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: now_ms.saturating_add(60_000),
                recipe_json: recipe_json.clone(),
                response_json: serde_json::to_string(&start).expect("start response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms,
            },
        )
        .await;

        // The drain is put on the row the way production puts it there: by
        // committing a successor. Setting the column by hand would test the
        // hook against a state the store never produces.
        let successor_incarnation = uuid::Uuid::new_v4().to_string();
        let successor_session = uuid::Uuid::new_v4().to_string();
        fixture
            .store
            .prepare_media_session(&plurx_core::domain::MediaSessionPreparation {
                expected_desired_revision: None,
                incarnation_id: successor_incarnation.clone(),
                session_id: successor_session.clone(),
                user_id: user.id,
                playback_id: "drain-release".to_owned(),
                expected_predecessor_incarnation_id: generation.clone(),
                expected_predecessor_owner_node_id: fixture.state.node_id.clone(),
                expected_predecessor_owner_epoch: 1,
                request_fingerprint: "b".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                recipe_json,
                response_json: serde_json::to_string(&start).expect("successor response"),
                media_origin_ms: 0,
                now_ms,
                deadline_ms: now_ms.saturating_add(60_000),
            })
            .await
            .expect("stage successor")
            .expect("staging wins");
        fixture
            .store
            .commit_media_session_preparation(
                user.id,
                "drain-release",
                &plurx_core::domain::MediaSessionPreparationCommitRequest {
                    expected_desired_revision: None,
                    staged_incarnation_id: successor_incarnation.clone(),
                    expected_predecessor_owner_node_id: fixture.state.node_id.clone(),
                    expected_predecessor_owner_epoch: 1,
                    now_ms,
                    lease_expires_at_ms: now_ms.saturating_add(60_000),
                    control_receipt: None,
                },
            )
            .await
            .expect("commit successor")
            .expect("commit wins");

        let draining = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("predecessor route")
            .expect("the predecessor keeps serving while it drains");
        assert_eq!(draining.state, "active");
        assert!(
            draining.drain_deadline_ms.is_some(),
            "the commit is what puts the predecessor into its drain",
        );

        let switched = || crate::playback_control::ActionAcknowledgement {
            action_id: uuid::Uuid::new_v4().to_string(),
            state: crate::playback_control::AcknowledgementState::Switched,
            buffered_through_ms: None,
            committed_media_origin_ms: None,
            first_frame_unix_ms: Some(unix_ms()),
        };

        // A refused exchange: the generation names an incarnation this route
        // is not on, so the relay's own fence answers before any acceptance.
        let mut refused = drain_control_request(generation.clone(), 1);
        refused.acknowledgement = Some(switched());
        let response = crate::http::internal_media_sessions::control_authorized(
            fixture.state.clone(),
            crate::playback_control::ControlRelayRequest {
                session_id: session_id.clone(),
                generation: uuid::Uuid::new_v4().to_string(),
                expected_owner_node_id: fixture.state.node_id.clone(),
                expected_owner_epoch: 1,
                deadline_unix_ms: unix_ms().saturating_add(4_000),
                control: refused,
            },
        )
        .await;
        assert!(
            !response.status().is_success(),
            "a control naming another generation is refused",
        );
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("route after refusal")
                .map(|route| route.state),
            Some("active".to_owned()),
            "a refused exchange proves nothing about what reached a screen, \
             and must not end the stream the client is still watching",
        );

        // An accepted exchange carrying no acknowledgement is ordinary
        // traffic on a draining route, and leaves the drain alone.
        let response = crate::http::internal_media_sessions::control_authorized(
            fixture.state.clone(),
            crate::playback_control::ControlRelayRequest {
                session_id: session_id.clone(),
                generation: generation.clone(),
                expected_owner_node_id: fixture.state.node_id.clone(),
                expected_owner_epoch: 1,
                deadline_unix_ms: unix_ms().saturating_add(4_000),
                control: drain_control_request(generation.clone(), 1),
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("route after silent exchange")
                .map(|route| route.state),
            Some("active".to_owned()),
            "the drain runs its window unless a client says it is done with it",
        );

        // And the exchange that does say so: answered, then released. Spaced
        // past `MIN_CONTROL_INTERVAL`, which the previous exchange has just
        // started; a rate-limited packet is refused, and a refusal is what the
        // case above is already about.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut releasing = drain_control_request(generation.clone(), 2);
        releasing.acknowledgement = Some(switched());
        let response = crate::http::internal_media_sessions::control_authorized(
            fixture.state.clone(),
            crate::playback_control::ControlRelayRequest {
                session_id: session_id.clone(),
                generation: generation.clone(),
                expected_owner_node_id: fixture.state.node_id.clone(),
                expected_owner_epoch: 1,
                deadline_unix_ms: unix_ms().saturating_add(4_000),
                control: releasing,
            },
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the packet that reports the switch is answered; ending the row \
             ahead of it is what answered it 410",
        );
        let released = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("route after the switch")
            .expect("the row is still readable once it has ended");
        assert_eq!(released.state, "ended");
        assert_eq!(released.terminal_reason.as_deref(), Some("superseded"));
    }

    #[tokio::test]
    async fn terminal_control_cancellation_and_reaper_preserve_one_durable_reply() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("terminal-cancellation", "hash", false)
            .await
            .expect("terminal cancellation user");
        let recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: generation.clone(),
            user_id: user.id,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: true,
            library_channel: None,
            request: crate::transcode::SessionRequest {
                control_sequence: None,
                file_id: fixture.file_id(),
                playback_id: "terminal-cancellation".to_owned(),
                request_id: Some(generation.clone()),
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
            },
        };
        let start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(60_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 720,
            encoder: "software".to_owned(),
            vod: false,
            ladder: vec![],
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &generation,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: generation.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "terminal-cancellation".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: now_ms.saturating_add(60_000),
                recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                response_json: serde_json::to_string(&start).expect("start response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms,
            },
        )
        .await;
        let request = crate::playback_control::ControlRequestV1 {
            intent: None,
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation: generation.clone(),
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: crate::playback_control::PlaybackDemand::End,
            position_ms: 1_000,
            buffered_from_ms: Some(0),
            buffered_through_ms: 10_000,
            playback_rate: 0.0,
            render_state: crate::playback_control::RenderState::Ended,
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
                platform: crate::playback_control::ClientPlatform::Apple,
                max_height: 1080,
                codecs: vec![crate::playback_control::CodecPolicy::H264],
                dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: None,
        };

        // A future discarded before owner-local admission must not enqueue or
        // mutate anything later.
        drop(control_local(
            &fixture.state,
            &route,
            request.clone(),
            i64::MAX,
        ));
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("route after unpolled control")
                .map(|route| route.state),
            Some("active".to_owned())
        );
        assert!(fixture
            .store
            .media_session_terminal_ack(&session_id, unix_ms())
            .await
            .expect("ack after unpolled control")
            .is_none());

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture.pause_control_after_acceptance(Arc::clone(&pause));
        let control = tokio::spawn({
            let state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move { control_local(&state, &route, request, i64::MAX).await }
        });
        pause.wait().await;

        let retry_a = tokio::spawn({
            let state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move { control_local(&state, &route, request, i64::MAX).await }
        });
        let retry_b = tokio::spawn({
            let state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move { control_local(&state, &route, request, i64::MAX).await }
        });
        tokio::task::yield_now().await;

        // Run the production reaper verdict after actor End but before the
        // final status join, with two exact retries also attached. All three
        // waiters must share the one actor-installed continuation, and the
        // reaper must not mistake the retired actor for abandoned cleanup.
        assert!(fixture.reaper_pass_keeps_worker(&session_id).await);
        assert!(fixture.worker_is_registered(&session_id).await);
        control.abort();
        assert!(matches!(control.await, Err(error) if error.is_cancelled()));
        pause.wait().await;

        let retry_a = retry_a.await.expect("first retry task");
        let retry_b = retry_b.await.expect("second retry task");
        assert_eq!(retry_a.status(), StatusCode::OK);
        assert_eq!(retry_b.status(), StatusCode::OK);
        let retry_a = axum::body::to_bytes(retry_a.into_body(), 64 * 1024)
            .await
            .expect("first retry body");
        let retry_b = axum::body::to_bytes(retry_b.into_body(), 64 * 1024)
            .await
            .expect("second retry body");
        assert_eq!(retry_a, retry_b, "exact retries share one terminal result");

        let acknowledgement = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(acknowledgement) = fixture
                    .store
                    .media_session_terminal_ack(&session_id, unix_ms())
                    .await
                    .expect("terminal acknowledgement")
                {
                    break acknowledgement;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("accepted End continuation must commit after HTTP cancellation");
        assert_eq!(acknowledgement.sequence, 1);
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("settled terminal route")
                .map(|route| route.state),
            Some("ended".to_owned())
        );
    }

    #[tokio::test]
    async fn settled_rolling_and_vod_routes_replay_the_durable_terminal_ack() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-worker").await;
        let user = fixture
            .store
            .create_user("terminal-replay", "hash", false)
            .await
            .expect("terminal replay user");

        for (label, presentation, lease_timeout_ms, producer_state, admitted) in [
            (
                "rolling",
                crate::transcode::Presentation::Live,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
                "exited",
                None,
            ),
            (
                "vod",
                crate::transcode::Presentation::Vod,
                crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                "complete",
                Some(true),
            ),
        ] {
            let session_id = uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client_instance_id = uuid::Uuid::new_v4().to_string();
            let recipe = RemoteStartRequest {
                protocol_version: crate::media_pool::PROTOCOL_VERSION,
                incarnation_id: generation.clone(),
                user_id: user.id,
                source_size: 1,
                source_mtime: 1,
                typeless_playlist: true,
                library_channel: None,
                request: crate::transcode::SessionRequest {
                    control_sequence: None,
                    file_id: fixture.file_id(),
                    playback_id: format!("terminal-{label}"),
                    request_id: Some(generation.clone()),
                    automatic: true,
                    previous_session_id: None,
                    reopen_reason: None,
                    kind: crate::transcode::SessionKind::Transcode { height: 720 },
                    start_seconds: 0.0,
                    audio_index: None,
                    subtitle_burn: None,
                    audio_offset_ms: 0,
                    hdr10: false,
                    presentation,
                    block_budget_secs: None,
                    transport: None,
                },
            };
            let start = StartResponse {
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
                duration_ms: Some(60_000),
                start_seconds: 0.0,
                media_origin_ms: Some(0),
                height: 720,
                encoder: "software".to_owned(),
                vod: label == "vod",
                ladder: vec![],
                prior_kbps: None,
                delivered_dynamic_range: Some("sdr".to_owned()),
                delivered_dolby_vision_profile: None,
                control: crate::playback_control::ControlBootstrap::new(
                    &session_id,
                    &generation,
                    1,
                    lease_timeout_ms,
                ),
                plan_notes: Vec::new(),
            };
            let now_ms = unix_ms();
            let route = activate_ready(
                &fixture.store,
                MediaSessionActivation {
                    recovery_epoch: String::new(),
                    expected_desired_revision: None,
                    incarnation_id: generation.clone(),
                    session_id: session_id.clone(),
                    user_id: user.id,
                    playback_id: format!("terminal-{label}"),
                    expected_predecessor_incarnation_id: None,
                    fence_predecessor: false,
                    request_id: None,
                    request_fingerprint: "a".repeat(64),
                    owner_node_id: fixture.state.node_id.clone(),
                    lease_expires_at_ms: now_ms.saturating_add(60_000),
                    recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                    response_json: serde_json::to_string(&start).expect("start response"),
                    publication_ready_at_ms: 0,
                    media_origin_ms: 0,
                    now_ms,
                },
            )
            .await;
            let request = crate::playback_control::ControlRequestV1 {
                intent: None,
                protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
                generation: generation.clone(),
                control_epoch: 1,
                client_instance_id: client_instance_id.clone(),
                sequence: 7,
                demand: crate::playback_control::PlaybackDemand::End,
                position_ms: 1_000,
                buffered_from_ms: Some(0),
                buffered_through_ms: 10_000,
                playback_rate: 0.0,
                render_state: crate::playback_control::RenderState::Ended,
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
                // Sequence > 1 retries may omit capabilities; replay
                // telemetry must come from the retained accepted result.
                capabilities: None,
                observation: None,
                acknowledgement: None,
                supported_actions: None,
            };
            let terminal_time_ms = unix_ms();
            let terminal = crate::playback_control::ControlResponseV1 {
                protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
                generation: generation.clone(),
                control_epoch: 1,
                accepted_sequence: request.sequence,
                server_time_unix_ms: terminal_time_ms,
                lease: crate::playback_control::PlaybackLeaseView {
                    state: "ended".to_owned(),
                    renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
                    expires_at_unix_ms: terminal_time_ms,
                },
                delivery: crate::playback_control::DeliveryView {
                    presentation: if label == "vod" {
                        "vod".to_owned()
                    } else {
                        "live-recovery".to_owned()
                    },
                    producer_state: producer_state.to_owned(),
                    produced_through_ms: Some(60_000),
                    fetched_through_ms: 10_000,
                    delivered_bps: None,
                    delivered_idle_ms: None,
                    recent_producer_speed: None,
                    client_runway_ms: 9_000,
                    admitted,
                    producer_decision: None,
                    hold_reason: None,
                    subtitle_readiness: None,
                    preparation: None,
                    owner_node_hash: "n-0123456789abcdef".to_owned(),
                    owner_epoch: 1,
                },
                effective_selection: crate::playback_control::EffectiveSelection {
                    quality_auto: true,
                    height: 720,
                    audio_track: None,
                    subtitle_burn: None,
                    audio_offset_ms: 0,
                    codec: "server_selected".to_owned(),
                    dynamic_range: Some("sdr".to_owned()),
                },
                action: crate::playback_control::ControlAction::None,
            };
            let acknowledgement = MediaSessionTerminalAck {
                incarnation_id: generation.clone(),
                session_id: session_id.clone(),
                owner_node_id: route.owner_node_id.clone(),
                owner_epoch: route.owner_epoch,
                client_instance_id,
                sequence: i64::try_from(request.sequence).expect("bounded sequence"),
                request_fingerprint: request
                    .fingerprint()
                    .expect("valid terminal request fingerprint"),
                response_json: serde_json::to_string(&RetainedTerminalResponse {
                    platform: crate::playback_control::ClientPlatform::Apple,
                    response: terminal.clone(),
                })
                .expect("terminal response"),
                expires_at_ms: terminal_time_ms.saturating_add(60_000),
                updated_at_ms: terminal_time_ms,
            };
            let faults = TerminalCommitFaults::default();
            let injected = if label == "rolling" {
                &faults.fail_before_commit
            } else {
                &faults.fail_after_commit
            };
            injected.store(1, std::sync::atomic::Ordering::Release);
            let terminal_store: Arc<dyn plurx_core::store::Store> = fixture.store.clone();
            assert!(
                persist_terminal_ack_with_faults(terminal_store, acknowledgement, Some(&faults),)
                    .await,
                "{label} terminal acknowledgement resolves the injected Store failure"
            );
            assert_eq!(
                injected.load(std::sync::atomic::Ordering::Acquire),
                0,
                "{label} consumed its injected before/after-commit failure"
            );
            let retained = terminal_ack_replay(
                &fixture.state,
                &route,
                &request,
                unix_ms().saturating_add(4_000),
            )
            .await
            .expect("terminal acknowledgement lookup")
            .expect("exact terminal acknowledgement");
            assert_eq!(
                retained.platform,
                Some(crate::playback_control::ClientPlatform::Apple),
                "{label} replay must retain the originally accepted platform"
            );
            let body = Bytes::from(serde_json::to_vec(&request).expect("terminal request"));
            let response = control_inner(
                fixture.state.clone(),
                session_id,
                body,
                unix_ms().saturating_add(4_000),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{label} replay");
            let response_body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("bounded terminal replay body");
            assert_eq!(
                serde_json::from_slice::<crate::playback_control::ControlResponseV1>(
                    &response_body,
                )
                .expect("decode terminal replay"),
                terminal,
                "{label} replay must return the exact durable acknowledgement"
            );
            let mut changed_payload = request.clone();
            changed_payload.position_ms += 1;
            let changed_response = control_inner(
                fixture.state.clone(),
                route.session_id.clone(),
                Bytes::from(
                    serde_json::to_vec(&changed_payload).expect("changed terminal request"),
                ),
                unix_ms().saturating_add(4_000),
            )
            .await;
            assert_eq!(
                changed_response.status(),
                StatusCode::GONE,
                "{label} cannot reuse the terminal sequence for a changed payload"
            );
        }
    }
