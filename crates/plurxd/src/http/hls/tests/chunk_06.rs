
    /// A create carries the ask it recorded, not one re-read at activation.
    ///
    /// This is the difference the whole ordering rests on, and it is invisible
    /// outside one window. `create` records the viewer's ask at its first step
    /// and activates much later; an activation that re-read the ask instead of
    /// carrying the recorded one would read back whatever has been asked for
    /// *since* and agree with itself — advancing the pointer to a session built
    /// for a selection the viewer has already left, which is the exact failure
    /// the compare exists to prevent.
    ///
    /// So the create is frozen between the two, the ask is moved underneath it,
    /// and the activation has to refuse.
    ///
    /// The control is not optional and its absence is why the first version of
    /// this test was deleted rather than fixed: on a fixture that cannot serve
    /// a create at all, "the create was refused" is true for reasons that have
    /// nothing to do with the ask, and the test passes with the compare removed
    /// entirely. The unraced create below has to be *accepted*, or the refusal
    /// above means nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_create_is_refused_when_the_ask_moves_before_it_activates() {
        use plurx_core::playback::DesiredQuality;
        let (state, user, file_id) = servable_state().await;
        let playback = "moving-ask-player";

        // The control first, so a fixture that has stopped being servable
        // fails here rather than passing the refusal for the wrong reason.
        let control = create(
            crate::http::extract::AuthUser(user.clone()),
            State(state.clone()),
            AxPath(file_id),
            HeaderMap::new(),
            super::super::network::RemoteAddress(None),
            Json(CreateSession {
                playback_id: "control-player".into(),
                intent: Some(envelope(1, DesiredQuality::Original)),
                copy: Some(true),
                ..bare_create()
            }),
        )
        .await;
        assert!(
            control.is_ok(),
            "an unraced create must be accepted, or the refusal below proves \
             nothing: {:?}",
            control.err()
        );

        let pause = super::pause_create_after_ask_recorded(&state);

        let creating = {
            let state = state.clone();
            let user = user.clone();
            tokio::spawn(async move {
                create(
                    crate::http::extract::AuthUser(user),
                    State(state),
                    AxPath(file_id),
                    HeaderMap::new(),
                    super::super::network::RemoteAddress(None),
                    Json(CreateSession {
                        playback_id: playback.into(),
                        intent: Some(envelope(1, DesiredQuality::Original)),
                        copy: Some(true),
                        ..bare_create()
                    }),
                )
                .await
                .is_ok()
            })
        };

        let held = pause.reached().await;

        // The viewer changes their mind while it is frozen.
        let moved = state
            .store
            .record_desired_selection(
                user.id,
                playback,
                &"9".repeat(64),
                "v1;quality=manual:720",
                9_000,
            )
            .await
            .expect("the viewer's new ask");
        assert_eq!(
            moved.revision, 2,
            "the ask moved while the create was frozen"
        );

        held.release();
        let completed = tokio::time::timeout(Duration::from_secs(120), creating)
            .await
            .expect("the create must finish once released")
            .expect("create task");

        assert!(
            !completed,
            "a create whose ask moved underneath it must not be answered as accepted"
        );
        assert!(
            state
                .store
                .media_session_route_for_playback(user.id, playback)
                .await
                .expect("route")
                .is_none(),
            "and it must leave the playback pointing nowhere"
        );
    }

    #[tokio::test]
    async fn an_ask_on_create_is_recorded_before_the_create_is_answered() {
        use plurx_core::playback::DesiredQuality;
        let state = resolver_state();
        let ask = envelope(1, DesiredQuality::Original);

        let answer = create_with(
            &state,
            CreateSession {
                playback_id: "player-a".into(),
                intent: Some(ask.clone()),
                ..bare_create()
            },
        )
        .await;
        assert!(
            answer.is_err(),
            "this create names a file that does not exist and cannot succeed"
        );

        let recorded = state
            .store
            .desired_selection(7, "player-a")
            .await
            .expect("reading the desired row");
        let recorded = recorded.expect("the ask is recorded even though the create failed");
        assert_eq!(
            recorded.digest,
            ask.digest(),
            "and it is the ask the viewer actually made"
        );
        assert_eq!(
            recorded.canonical_form,
            ask.selection.canonical_form(),
            "stored beside its digest so an operator can read it back"
        );
    }

    /// The control protocol being off must not make the ask disappear.
    ///
    /// This is the configuration the handoff singles out. With the setting off
    /// a session gets no control bootstrap and therefore no control channel, so
    /// every rule that lives on the control path stops running. `resolver_state`
    /// has never written the setting, so it is off here in exactly the way it is
    /// off in production by default — and the row still lands, because the write
    /// is on create and reads nothing about control.
    /// The revision a create records reaches the pointer it eventually writes.
    ///
    /// Three things have to line up for the fence to mean anything: a create
    /// records the ask, an activation compares a revision, and a pointer stores
    /// one. Each was proved alone, and nothing ran all three against a real
    /// source — so an activation handed a re-read, or a pointer filled from
    /// anywhere but the ask table, would look correct from either end.
    #[tokio::test]
    async fn a_created_playback_points_at_the_ask_its_create_recorded() {
        use plurx_core::playback::DesiredQuality;
        let (state, user, file_id) = servable_state().await;
        let ask = envelope(1, DesiredQuality::Original);

        let accepted = create(
            crate::http::extract::AuthUser(user.clone()),
            State(state.clone()),
            AxPath(file_id),
            HeaderMap::new(),
            super::super::network::RemoteAddress(None),
            Json(CreateSession {
                playback_id: "chain-player".into(),
                intent: Some(ask.clone()),
                copy: Some(true),
                ..bare_create()
            }),
        )
        .await
        .expect("the create must be accepted");
        assert!(
            !accepted.0.session_id.is_empty(),
            "a create answers with a session"
        );

        let recorded = state
            .store
            .desired_selection(user.id, "chain-player")
            .await
            .expect("reading the ask")
            .expect("the create recorded its viewer's ask");
        assert_eq!(recorded.digest, ask.digest());

        // The pointer's own revision is what the fence reads. A pointer written
        // for a playback with an ask must carry that ask's revision, or the
        // next legitimate write is refused as though it came from an old
        // binary — the guard turned against the thing it protects.
        let carried = state
            .store
            .validation_playback_pointer_desired_revision(user.id, "chain-player")
            .await
            .expect("reading the pointer revision");
        assert_eq!(
            carried,
            Some(recorded.revision),
            "the pointer carries the ask that was current when it was written"
        );
    }

    /// Two creates for one playback, with the control protocol off.
    ///
    /// The configuration §1 singles out, and the one where nothing else can
    /// help: with control off there is no channel, no `ControlState`, and no
    /// exchange — the create body is the entire record of what the viewer
    /// wants. Two of them for the same playback is the shape a viewer produces
    /// by changing their selection while the first is still starting.
    ///
    /// Exactly one may end up owning the pointer, and the pointer must name the
    /// ask that owner was built for. "Either could win" is the correct
    /// expectation and "both did" is the bug: two pointers cannot exist, but a
    /// pointer advanced by the loser after the winner landed is a viewer
    /// watching the selection they abandoned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overlapping_creates_with_control_off_settle_on_one_ask() {
        use plurx_core::playback::DesiredQuality;
        let (state, user, file_id) = servable_state().await;
        state
            .store
            .put_setting(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1, "0")
            .await
            .expect("turning the default-on control protocol off");
        assert_eq!(
            state
                .store
                .get_setting(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1)
                .await
                .expect("reading the control setting"),
            Some("0".to_owned()),
            "the control protocol is off, which is the case under test"
        );
        let playback = "overlapping-player";

        let spawn_create = |quality| {
            let state = state.clone();
            let user = user.clone();
            tokio::spawn(async move {
                create(
                    crate::http::extract::AuthUser(user),
                    State(state),
                    AxPath(file_id),
                    HeaderMap::new(),
                    super::super::network::RemoteAddress(None),
                    Json(CreateSession {
                        playback_id: playback.into(),
                        intent: Some(envelope(1, quality)),
                        copy: Some(true),
                        ..bare_create()
                    }),
                )
                .await
            })
        };
        let first = spawn_create(DesiredQuality::Original);
        let second = spawn_create(DesiredQuality::Manual { height: 720 });
        let (first, second) = tokio::join!(first, second);
        let first = first.expect("first task");
        let second = second.expect("second task");
        let accepted = [first.is_ok(), second.is_ok()]
            .into_iter()
            .filter(|accepted| *accepted)
            .count();
        assert!(
            accepted >= 1,
            "with control off, a viewer who asks twice must still get a session: first={:?}, second={:?}",
            first.as_ref().err(),
            second.as_ref().err(),
        );

        // Whatever the pointer ended up naming, it names the ask that is
        // recorded for the playback. A pointer carrying anything else is a
        // session serving a selection nothing says the viewer wants.
        let recorded = state
            .store
            .desired_selection(user.id, playback)
            .await
            .expect("reading the ask")
            .expect("two creates recorded an ask between them");
        let carried = state
            .store
            .validation_playback_pointer_desired_revision(user.id, playback)
            .await
            .expect("reading the pointer revision");
        if let Some(carried) = carried {
            assert_eq!(
                carried, recorded.revision,
                "the pointer names the ask the store has recorded, not an \
                 earlier one a loser advanced it to"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_ask_refuses_the_create_and_records_nothing() {
        use plurx_core::playback::DesiredQuality;
        let state = resolver_state();
        let mut broken = envelope(1, DesiredQuality::Auto { height: None, candidate_id: None });
        broken.recipe_revision = 0;

        let answer = create_with(
            &state,
            CreateSession {
                playback_id: "player-bad".into(),
                intent: Some(broken),
                ..bare_create()
            },
        )
        .await;
        match answer {
            Err(ApiError::BadRequest(message)) => assert!(
                message.contains("recipe"),
                "the refusal names the axis that is wrong, got {message}"
            ),
            Err(other) => panic!("a zero revision must be a 400, got {other:?}"),
            Ok(_) => panic!("a zero revision must be refused, not answered"),
        }
        assert!(
            state
                .store
                .desired_selection(7, "player-bad")
                .await
                .expect("reading the desired row")
                .is_none(),
            "nothing may be recorded for an ask that was refused"
        );
    }

    /// A body with no envelope is the body every deployed client sends, and it
    /// must behave exactly as it did before the field existed.
    #[tokio::test]
    async fn a_create_without_an_ask_records_nothing() {
        let state = resolver_state();
        let _ = create_with(
            &state,
            CreateSession {
                playback_id: "player-legacy".into(),
                ..bare_create()
            },
        )
        .await;
        assert!(
            state
                .store
                .desired_selection(7, "player-legacy")
                .await
                .expect("reading the desired row")
                .is_none(),
            "an older client asked for nothing new and owns no desired row"
        );
    }

    #[test]
    fn a05_candidate_hls_isolation_requires_actual_auto_not_protocol_or_manual() {
        use plurx_core::playback::DesiredQuality;
        let mut body = bare_create();
        assert!(!body.candidate_auto_policy(), "old inferred Auto is not a candidate claim");
        body.quality_auto = Some(true);
        assert!(body.candidate_auto_policy());
        body.copy = Some(true);
        assert!(!body.candidate_auto_policy(), "compat Original keeps legacy semantics");
        body.intent = Some(envelope(1, DesiredQuality::Auto { height: None, candidate_id: None }));
        assert!(body.candidate_auto_policy(), "typed Auto is authoritative");
        body.intent = Some(envelope(1, DesiredQuality::Manual { height: 1080 }));
        assert!(!body.candidate_auto_policy(), "typed manual wins over compat Auto");
        body.intent = Some(envelope(1, DesiredQuality::Original));
        assert!(!body.candidate_auto_policy());
        body.intent = Some(envelope(1, DesiredQuality::Auto { height: None, candidate_id: None }));
        body.height = Some(1440);
        assert!(!body.candidate_auto_policy(), "the explicit 1440 route is preserved");
    }

    #[tokio::test]
    async fn a05_candidate_auto_fallback_ignores_both_legacy_prior_branches() {
        let state = resolver_state();
        let mut source = hls_file(Vec::new());
        source.height = Some(2160);
        let prior = plurx_core::domain::NetworkPrior {
            credential_generation: Default::default(),
            client_class: "web".to_owned(),
            network_fingerprint: "candidate-isolation".to_owned(),
            sustained_kbps: Some(1),
            worst_rung_height: Some(2160),
            starved_at_ms: Some(unix_ms()),
            sample_count: 99,
            updated_at_ms: unix_ms(),
            link_worst_rung_height: None,
            link_starved_at_ms: None,
        };
        let neutral = resolve_height(&state, Some(&source), None, false, None).await;
        let isolated = resolve_height(
            &state, Some(&source),
            crate::http::stream::prior_for_candidate_policy(Some(&prior), true),
            false, None,
        ).await;
        let legacy = resolve_height(&state, Some(&source), Some(&prior), false, None).await;
        assert_eq!(isolated, neutral);
        assert!(legacy < isolated, "legacy policy still consumes its old rate/starvation history");
        assert_eq!(resolve_height(&state, Some(&source), Some(&prior), false, Some(1440)).await, 1440);
        assert_eq!(resolve_height(&state, Some(&source), Some(&prior), false, Some(2160)).await, 2160);
    }

    #[tokio::test]
    async fn a05_geometry_promoted_compat_copy_carries_resolved_auto_policy() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        let state = resolver_state();
        let library = state.store.create_library(&NewLibrary {
            name: "A05 geometry promotion".into(), kind: LibraryKind::Movies,
            paths: vec![], anime: false,
        }).await.expect("library");
        let item = state.store.insert_item(&NewItem {
            library_id: library.id, kind: ItemKind::Movie, parent_id: None,
            title: "geometry promotion".into(), year: None,
            season_number: None, episode_number: None,
        }).await.expect("item");
        let id = state.store.upsert_file(item, "/media/a05-geometry.mkv", 1000, 1, &ProbeResult {
            video_codec: Some("h264".into()), width: Some(3840), height: Some(2160),
            bit_depth: Some(8), raw_json: Some(serde_json::json!({"streams":[{
                "index":0,"codec_type":"video","codec_name":"h264","profile":"High",
                "width":3840,"height":2160,"pix_fmt":"yuv420p",
                "sample_aspect_ratio":"1:1","r_frame_rate":"24/1","avg_frame_rate":"24/1"
            }]}).to_string()), ..Default::default()
        }).await.expect("file");
        let source = state.store.get_file(id).await.expect("file read").expect("source");
        let body = CreateSession {
            playback_id: "geometry-auto".into(), copy: Some(true), quality_auto: Some(true),
            caps: Some(serde_json::from_value(serde_json::json!({"v":2,"video":[{
                "codec":"h264","max_width":1920,"max_height":1080
            }]})).expect("bounded decoder caps")), ..bare_create()
        };
        assert!(!body.candidate_auto_policy(), "the original wire copy was not candidate Auto");
        let resolved = resolve_plan(PlanInputs {
            snapshot: None,
            state: &state, user_id: 7, file_id: id, source: Some(&source), network_prior: None, network_identity: None, incumbent_receipt: None,
        }, None, body).await.expect("geometry-promoted plan");
        assert!(matches!(resolved.request.kind, crate::transcode::SessionKind::Transcode { .. }));
        assert!(resolved.candidate_auto_policy, "response policy must follow actual normalized Auto");
        assert_eq!(resolved.intent_fingerprint, resolved.request.durable_intent_fingerprint(7),
            "the private resolved policy field does not alter request identity");
    }

    async fn resolved_height(state: &AppState, source: &MediaFile, asked: Option<i64>) -> i64 {
        let body = CreateSession {
            playback_id: "player-a".into(),
            height: asked,
            ..bare_create()
        };
        resolve_plan(
            PlanInputs {
            snapshot: None,
                state,
                user_id: 7,
                file_id: source.id,
                source: Some(source),
                network_prior: None,
                network_identity: None,
                incumbent_receipt: None,
            },
            None,
            body,
        )
        .await
        .expect("resolve")
        .height
    }

    /// The three arms of the height resolution — the largest thing
    /// `resolve_plan` extracted that was not already a named function. (The
    /// native-subtitle validation and the `request_id` length check are also
    /// inline; both are covered through the real handler by
    /// `hls_create_rejects_native_subtitle_indices_it_cannot_serve`.)
    ///
    /// They are policy, not arithmetic, and each is a different promise:
    ///
    /// * **the source's own height is never snapped and never downgraded** —
    ///   it is the Original/forced-burn promise the player reads as
    ///   `sessionHeight`;
    /// * **an explicit rung from a menu snaps onto the ladder**, so a stray
    ///   number lands somewhere the encoder has rungs for;
    /// * **an above-ladder height passes through as what it is**, rather than
    ///   being pulled down to the nearest rung.
    ///
    /// Splitting the match is how one of the three gets lost, so this test
    /// exists to make that loud.
    #[tokio::test]
    async fn the_height_resolution_keeps_its_three_promises() {
        let state = resolver_state();
        let mut odd_source = hls_file(Vec::new());
        odd_source.height = Some(900);

        assert_eq!(
            resolved_height(&state, &odd_source, Some(900)).await,
            900,
            "the source's own height is a promise, and is neither snapped nor \
             downgraded",
        );
        assert_eq!(
            resolved_height(&state, &odd_source, Some(1079)).await,
            crate::transcode::snap_height(1079),
            "a stray rung from a menu snaps onto the ladder",
        );
        let above = crate::transcode::MAX_HEIGHT;
        assert_eq!(
            resolved_height(&state, &odd_source, Some(above)).await,
            above,
            "an above-ladder height passes through as what it is",
        );
        // A below-ladder ask lands on the ladder's lowest rung. The new 144p
        // rung meets `MIN_HEIGHT`, so snapping and clamping agree at the floor.
        let floor = resolved_height(&state, &odd_source, Some(1)).await;
        assert_eq!(floor, crate::transcode::snap_height(1));
        assert_eq!(floor, crate::transcode::MIN_HEIGHT);
        // Upward it does bind, and it is the only thing that does: an
        // above-ladder height passes the snap through untouched, so without
        // the clamp a body asking for 100000 builds a transcode at 100000.
        assert_eq!(
            resolved_height(&state, &odd_source, Some(crate::transcode::MAX_HEIGHT + 1)).await,
            crate::transcode::MAX_HEIGHT,
        );
    }

    /// Auto is the arm every client that does not pin a quality takes, and the
    /// only one that reads the stored network prior or the HDR10 ask. Both
    /// reach `auto_height_for_request` or neither does — and a resolver that
    /// dropped either would still return a plausible height, which is why this
    /// compares against the same call rather than against a constant.
    #[tokio::test]
    async fn auto_asks_the_ladder_with_everything_it_was_given() {
        let state = resolver_state();
        let mut source = hls_file(Vec::new());
        source.height = Some(2160);
        let prior = plurx_core::domain::NetworkPrior {
            credential_generation: Default::default(),
            client_class: "web".to_owned(),
            network_fingerprint: "fingerprint".to_owned(),
            // A measured slow link, which is the whole reason Auto reads the
            // prior at all.
            sustained_kbps: Some(4_000),
            worst_rung_height: Some(1080),
            starved_at_ms: Some(1),
            sample_count: 12,
            updated_at_ms: 1,
            link_worst_rung_height: None,
            link_starved_at_ms: None,
        };

        for hdr10 in [false, true] {
            let body = CreateSession {
                playback_id: "player-a".into(),
                height: None,
                hdr10: Some(hdr10),
                ..bare_create()
            };
            let resolved = resolve_plan(
                PlanInputs {
            snapshot: None,
                    state: &state,
                    user_id: 7,
                    file_id: source.id,
                    source: Some(&source),
                    network_prior: Some(&prior),
                    network_identity: None,
                    incumbent_receipt: None,
                },
                None,
                body,
            )
            .await
            .expect("resolve auto")
            .height;
            let expected = state
                .transcode
                .auto_height_for_request(Some(&source), Some(&prior), hdr10)
                .await
                .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT);
            assert_eq!(
                resolved, expected,
                "auto must carry the source, the prior and hdr10={hdr10}",
            );
        }
    }

    /// The fingerprint is taken from the intent as sent, before the review is
    /// applied — so a retry of the same body recovers the same session even on
    /// a node whose review would decide differently.
    #[tokio::test]
    async fn the_intent_fingerprint_ignores_the_review() {
        let state = resolver_state();
        let source = dolby_vision_p8_file();
        // Built twice rather than cloned: `CreateSession` is a wire type and
        // deriving `Clone` on it for a test would be the test changing the
        // contract to suit itself.
        let body = || CreateSession {
            playback_id: "player-a".into(),
            copy: Some(true),
            preserve_dolby_vision: Some(false),
            hdr10: Some(false),
            ..bare_create()
        };
        let inputs = || PlanInputs {
            snapshot: None,
            state: &state,
            user_id: 7,
            file_id: source.id,
            source: Some(&source),
            network_prior: None,
            network_identity: None,
            incumbent_receipt: None,
        };
        let plain = resolve_plan(inputs(), None, body())
            .await
            .expect("resolve without a review");
        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &source,
            &capable_node(),
            true,
            true,
            NOW_MS,
        );
        let reviewed = resolve_plan(inputs(), Some(review), body())
            .await
            .expect("resolve with a review");

        assert_eq!(
            plain.intent_fingerprint, reviewed.intent_fingerprint,
            "the review must not move the fingerprint, or a transport retry \
             on a differently-deciding binary gets a 409 instead of its own \
             session",
        );
        assert_ne!(
            plain.request.kind, reviewed.request.kind,
            "and the review must still reach the request",
        );
        assert_eq!(
            plain.plan_notes,
            Vec::<String>::new(),
            "no review, nothing to note",
        );
    }

    /// A review's notes reach the resolved plan. They are what the create
    /// response shows a client about a plan it did not get, so dropping them
    /// on the floor between `apply_plan_review` and `ResolvedPlan` would be
    /// silent.
    #[tokio::test]
    async fn a_reviews_notes_reach_the_resolved_plan() {
        let state = resolver_state();
        let source = dolby_vision_p8_file();
        // A body that claims more than its own caps support: the review
        // overrides it and says so. Same fixture as
        // `a_create_that_claims_more_than_its_caps_gets_the_servers_plan_and_a_note`,
        // because a note is what that case exists to produce.
        let review = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &source,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(!review.notes.is_empty(), "fixture must produce notes");
        let expected = review.notes.clone();
        let resolved = resolve_plan(
            PlanInputs {
            snapshot: None,
                state: &state,
                user_id: 7,
                file_id: source.id,
                source: Some(&source),
                network_prior: None,
                network_identity: None,
                incumbent_receipt: None,
            },
            Some(review),
            CreateSession {
                playback_id: "player-a".into(),
                copy: Some(true),
                preserve_dolby_vision: Some(true),
                ..bare_create()
            },
        )
        .await
        .expect("resolve with notes");
        assert_eq!(resolved.plan_notes, expected);
    }

    /// The server's re-derivation reaches the session, and the body never
    /// does.
    ///
    /// `review_client_plan` is well covered; this is the wire between it and
    /// the request that gets built, and each field it carries fails
    /// differently if the wire is cut. `preserve_dolby_vision` reverts to the
    /// client's own echo — the pre-caps-v2 bug where a blanket `dv=1` got
    /// Safari a preserved Profile 7 it could not decode. `hdr10` reverts to a
    /// claim the caps did not support. And `convert_dolby_vision` is never set
    /// at all: `into_request` leaves it false because a client has no way to
    /// ask for a conversion, so this assignment is the *only* one, and without
    /// it the whole milestone is dead code that ships and does nothing.
    #[test]
    fn the_reconciled_plan_reaches_the_request_and_the_body_cannot() {
        let body = CreateSession {
            playback_id: "p".into(),
            copy: Some(true),
            // The client asks for the opposite of everything the review says.
            preserve_dolby_vision: Some(false),
            hdr10: Some(false),
            ..bare_create()
        };
        let mut request = body.into_request(1, 0);
        let asked = request.kind;
        assert!(
            matches!(
                asked,
                crate::transcode::SessionKind::Copy {
                    convert_dolby_vision: false,
                    ..
                }
            ),
            "a client cannot ask to be handed a conversion: {asked:?}"
        );

        // …including a client that asks for everything adjacent to one. The
        // conversion is not a wire field, so no combination of body values can
        // produce it — which is what makes the assignment below the only one.
        let eager = CreateSession {
            playback_id: "p".into(),
            copy: Some(true),
            preserve_dolby_vision: Some(true),
            hdr10: Some(true),
            ..bare_create()
        }
        .into_request(1, 0);
        assert!(
            matches!(
                eager.kind,
                crate::transcode::SessionKind::Copy {
                    convert_dolby_vision: false,
                    preserve_dolby_vision: true,
                    ..
                }
            ),
            "asking to preserve is not asking to convert: {:?}",
            eager.kind
        );

        let notes = apply_plan_review(
            &mut request,
            PlanReview {
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                hdr10: true,
                notes: vec!["a note".to_owned()],
                mismatched: false,
            },
        );
        let crate::transcode::SessionKind::Copy {
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } = request.kind
        else {
            panic!("a copy request stays a copy request");
        };
        assert!(preserve_dolby_vision, "the server's answer, not the body's");
        assert!(
            convert_dolby_vision,
            "the only assignment there is — without it the conversion never runs"
        );
        assert!(request.hdr10, "and the same for the HDR10 request");
        assert_eq!(notes, vec!["a note".to_owned()]);

        // A transcode request has no Dolby Vision fields to carry, and must
        // still take the notes and the HDR10 answer.
        let mut transcode = CreateSession {
            playback_id: "p".into(),
            height: Some(1080),
            ..bare_create()
        }
        .into_request(1, 1080);
        apply_plan_review(
            &mut transcode,
            PlanReview {
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                hdr10: true,
                notes: Vec::new(),
                mismatched: false,
            },
        );
        assert!(matches!(
            transcode.kind,
            crate::transcode::SessionKind::Transcode { .. }
        ));
        assert!(transcode.hdr10);
    }

    /// A Profile 7 title reaches a Profile-8 client as a conversion, and the
    /// conversion follows the preservation wherever that goes.
    ///
    /// The two flags are set together and clamped together. `convert` is not a
    /// wire field — a client has no way to be right or wrong about it — so it
    /// is never compared against anything the body asked for; it follows
    /// `preserve_dolby_vision`, and the `compatible_hdr_base` retry is the
    /// case that makes it matter. A client that decoded Dolby Vision, failed
    /// on this title, and asked for the plain HDR10 base must not be handed a
    /// *converted* Dolby Vision stream instead: that is the same stream it
    /// just failed on, wearing a different profile number.
    #[test]
    fn a_declined_dolby_vision_plan_declines_the_converted_kind_too() {
        let mut file = dolby_vision_p8_file();
        file.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);

        // The ordinary answer for a client that takes 8 and not 7.
        let converted = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(converted.convert_dolby_vision);
        assert!(
            converted.preserve_dolby_vision,
            "there is nothing to convert in a stream the filter removed"
        );

        // The client declines Dolby Vision by not asking for it. The
        // conversion goes with it.
        let declined = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false,
            false,
            NOW_MS,
        );
        assert!(!declined.preserve_dolby_vision);
        assert!(
            !declined.convert_dolby_vision,
            "a client that declined Dolby Vision declined the converted kind"
        );

        // And the same through the named override, which is the path Apple's
        // retry actually takes.
        let overridden = review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                ..Default::default()
            }),
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(!overridden.preserve_dolby_vision);
        assert!(!overridden.convert_dolby_vision);
    }

    /// Apple's `forceCompatibleHDRBase` retry, and why it needs a name.
    ///
    /// The client decoded the Dolby Vision stream, failed on this title, and
    /// is asking for the HDR10-compatible base. Its caps are still correct —
    /// the device really does take Profile 8 — so the re-derivation says
    /// "preserve", and without a named override the create would hand it back
    /// exactly the stream it just failed on, forever. The override lowers the
    /// plan and leaves its own reason, and it is *not* counted as a mismatch:
    /// nothing disagreed, the client asked for something legitimate.
    #[test]
    fn the_compatible_base_override_lowers_the_plan_without_being_a_mismatch() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                force: None,
            }),
            &file,
            &capable_node(),
            false, // the retry's body echoes the lowered plan
            true,
            NOW_MS,
        );

        assert!(!review.preserve_dolby_vision);
        assert!(
            !review.mismatched,
            "a named override is an explanation, not a disagreement"
        );
        assert_eq!(
            review.notes,
            vec!["override compatible_hdr_base: Dolby Vision declined by the client".to_owned()]
        );
    }

    /// The same override on a title the plan was never going to send as
    /// Dolby Vision.
    ///
    /// Harmless — the plan is already what the client is asking for — but
    /// worth a line, because a client retrying the compatible base on an
    /// HDR10 title is a client chasing a failure that came from somewhere
    /// else, and the next person debugging it should not have to guess that.
    #[test]
    fn the_compatible_base_override_says_so_when_it_had_nothing_to_decline() {
        let mut file = hls_file(Vec::new());
        file.hdr = Some("hdr10".into());
        file.hdr_format = Some("HDR10".into());
        let review = review_client_plan(
            &no_dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                force: None,
            }),
            &file,
            &capable_node(),
            false,
            false,
            NOW_MS,
        );
        assert!(!review.preserve_dolby_vision);
        assert!(!review.mismatched);
        assert_eq!(
            review.notes,
            vec![
                "override compatible_hdr_base had nothing to decline: the plan was \
                 not Dolby Vision"
                    .to_owned()
            ]
        );
    }

    /// The quality menu is fed *into* the derivation, not compared against
    /// it.
    ///
    /// A forced transcode is a viewer's answer to a different question — how
    /// big should this stream be — and the plan it produces is still the
    /// server's. It leaves a note anyway: a viewer who forced a transcode and
    /// then read an SDR badge deserves those two facts next to each other
    /// rather than a support thread.
    #[test]
    fn a_forced_rung_produces_the_servers_plan_for_that_force_and_says_which() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: None,
                force: Some("transcode".to_owned()),
            }),
            &file,
            &capable_node(),
            false,
            true,
            NOW_MS,
        );
        assert!(
            !review.preserve_dolby_vision,
            "a transcode cannot preserve Dolby Vision; it re-encodes"
        );
        assert!(!review.mismatched);
        assert_eq!(review.notes, vec!["override force=transcode".to_owned()]);
    }

    /// The `hdr10` field is a claim about the CLIENT, and the review checks
    /// it against the client's own document — not against what this node or
    /// this source can do.
    ///
    /// Keeping those separate matters: `hdr10_grade_for` still refuses the
    /// rung for any source, height or build that did not prove the chain, and
    /// folding that refusal in here would report a "mismatch" every time a
    /// perfectly honest client asked for a rung this particular title cannot
    /// have.
    #[test]
    fn the_hdr10_claim_is_checked_against_the_document_not_against_the_node() {
        let file = dolby_vision_p8_file();
        // A node that proved nothing at all: no RPU render, no HDR10 chain.
        let bare = plurx_core::playback::RenderCaps::proven(false);
        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &bare,
            false,
            true,
            NOW_MS,
        );
        assert!(
            review.hdr10,
            "the document presents PQ on hevc, so the claim stands; whether this \
             node can honour it is `hdr10_grade_for`'s question, asked later"
        );

        // …and a client whose document does not present PQ cannot claim it,
        // however capable the node.
        let review = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false,
            true,
            NOW_MS,
        );
        assert!(!review.hdr10);
        assert!(review.mismatched);
        assert!(
            review
                .notes
                .iter()
                .any(|note| note.contains("client asked hdr10=true, server derived false")),
            "{:?}",
            review.notes
        );
    }

    /// The build label is what every create log line is keyed by, so it has
    /// to be right for the population it is counting — and it is assembled
    /// from strings a caller chose.
    #[test]
    fn the_build_label_prefers_the_document_and_bounds_every_source() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            axum::http::HeaderValue::from_str(&"M".repeat(400)).expect("ascii"),
        );

        let named = caps_v2(
            r#"{"v":2,"client":{"kind":"ios","build":"86"},
                "video":[],"audio":[],"containers":[]}"#,
        );
        assert_eq!(client_build_label(Some(&named), &headers), "ios/86");

        // A document with a client block that says nothing falls through to
        // the header, same as no document at all.
        let anonymous = caps_v2(r#"{"v":2,"client":{},"video":[],"audio":[],"containers":[]}"#);
        for caps in [Some(&anonymous), None] {
            let label = client_build_label(caps, &headers);
            assert_eq!(
                label.chars().count(),
                48,
                "an unbounded header does not belong in a log line: {label}"
            );
        }

        assert_eq!(client_build_label(None, &HeaderMap::new()), "unknown");

        // The body is exactly as caller-controlled as the header, and a
        // newline inside it is how one log line becomes two forged ones. The
        // early return for a named client must not skip the bounding.
        let hostile = caps_v2(
            "{\"v\":2,\"client\":{\"kind\":\"ios\\n2026-08-30 WARN plan_mismatch: forged\",\
             \"build\":\"86\"},\"video\":[],\"audio\":[],\"containers\":[]}",
        );
        let label = client_build_label(Some(&hostile), &headers);
        assert!(
            !label.contains('\n') && !label.chars().any(char::is_control),
            "{label:?}"
        );
        assert!(label.chars().count() <= 48 * 2 + 1, "{label:?}");

        // A client block that is nothing but control characters has no
        // printable content, so it falls through rather than logging a blank.
        let blank = caps_v2(
            "{\"v\":2,\"client\":{\"kind\":\"\\n\\t\",\"build\":\"\"},\
             \"video\":[],\"audio\":[],\"containers\":[]}",
        );
        assert_eq!(
            client_build_label(Some(&blank), &HeaderMap::new()),
            "unknown"
        );
    }

    /// The client may always ask for LESS than the plan allows.
    ///
    /// This is the direction that keeps Apple's compatible-base retry from
    /// becoming an infinite loop even before that client learns to send
    /// `overrides.compatible_hdr_base` — and it is not a hole in E4, because
    /// the direction E4 exists to close is the client claiming MORE than its
    /// own document supports. A viewer declining Dolby Vision gets a stream
    /// that plays; a client handed Dolby Vision it cannot decode gets black.
    #[test]
    fn a_client_may_decline_the_plan_but_not_exceed_it() {
        let file = dolby_vision_p8_file();
        let declined = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false, // "don't preserve Dolby Vision, I just failed on it"
            false,
            NOW_MS,
        );
        assert!(
            !declined.preserve_dolby_vision,
            "the retry must not be handed back the stream it failed on"
        );
        assert!(!declined.mismatched, "declining is not disagreeing");
        assert!(declined.notes.is_empty(), "{:?}", declined.notes);

        // The other direction is still clamped, logged and reported.
        let exceeded = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(!exceeded.preserve_dolby_vision);
        assert!(exceeded.mismatched);
    }

    /// The shape every real create has, and the one the counters have to stay
    /// quiet for.
    ///
    /// `hdr10` is a per-TITLE request — the web sets it only when the
    /// decision it is acting on said `transcode` + `hdr10`. Deriving it from
    /// the caps document alone would set it true on every create from any
    /// PQ-capable client, including every SDR title: the ladder ceiling would
    /// move for requests that never asked, and the mismatch counter would sit
    /// permanently off zero, which is the one thing that would make it
    /// useless.
    #[test]
    fn an_sdr_title_from_a_pq_capable_client_is_not_a_mismatch() {
        let mut file = dolby_vision_p8_file();
        file.hdr = None;
        file.hdr_format = None;

        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false, // the body carries no preserve_dolby_vision…
            false, // …and no hdr10, because there is nothing to ask for
            NOW_MS,
        );
        assert!(!review.hdr10, "the client asked for no HDR10 rung");
        assert!(!review.preserve_dolby_vision);
        assert!(
            !review.mismatched,
            "an ordinary SDR create must not report a plan mismatch"
        );
        assert!(review.notes.is_empty(), "{:?}", review.notes);

        // The same client asking for the rung on a title that has one is
        // granted it, because its document backs the claim.
        let mut hdr10 = dolby_vision_p8_file();
        hdr10.hdr = Some("hdr10".into());
        hdr10.hdr_format = Some("HDR10".into());
        let asked = review_client_plan(
            &dolby_vision_client(),
            None,
            &hdr10,
            &capable_node(),
            false,
            true,
            NOW_MS,
        );
        assert!(asked.hdr10);
        assert!(!asked.mismatched);
    }

    /// The counters are the fleet's only view of this, so something has to
    /// prove they move — and that they cannot report more outcomes than
    /// reviews.
    #[test]
    fn every_review_is_counted_and_the_parts_never_exceed_the_total() {
        let before = plan_derivation::snapshot();
        let file = dolby_vision_p8_file();

        review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                force: Some("transcode".to_owned()),
            }),
            &file,
            &capable_node(),
            false,
            false,
            NOW_MS,
        );

        // Lower bounds, for the reason given in
        // `a_create_that_sends_no_caps_document_still_gets_a_review`: these are
        // process-global counters and this binary's tests run in parallel, so
        // an exact delta is a flake wearing a regression's name. What survives
        // the trade is what the test is for — every review counts itself, the
        // exceeding one counts a mismatch, the overridden one counts an
        // override — plus the invariant in the name, which holds whoever else
        // is counting: neither part can exceed the total.
        let after = plan_derivation::snapshot();
        assert!(after.2 - before.2 >= 3, "one `rederived` per review");
        assert!(after.3 - before.3 >= 1, "the exceeding review mismatched");
        assert!(
            after.4 - before.4 >= 1,
            "the review carrying two overrides counted itself overridden"
        );
        assert!(after.3 <= after.2 && after.4 <= after.2);
    }

    fn hls_context(
        codecs: &str,
        supplemental_codecs: Option<&str>,
    ) -> crate::transcode::HlsContext {
        crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 5615,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: codecs.into(),
            supplemental_codecs: supplemental_codecs.map(str::to_owned),
            frame_rate: Some(24_000.0 / 1_001.0),
        }
    }

    fn sdr_context() -> crate::transcode::HlsContext {
        hls_context("avc1.640034,mp4a.40.2", None)
    }

    #[test]
    fn sdr_master_declares_only_complete_frozen_output_components() {
        let file = hls_file(vec![]);
        let mut context = sdr_context();
        assert!(!master_playlist(&file, None, &context).contains("CODECS="));
        let mut facts = crate::transcode::FrozenHlsCodecFacts::audio(None, true, false);
        facts.bind_output_avc_init("avc1.64001F".to_owned());
        context.codec_facts = Some(facts);
        assert!(!master_playlist(&file, None, &context).contains("CODECS="));
        let mut facts = crate::transcode::FrozenHlsCodecFacts::audio(None, false, false);
        facts.bind_output_avc_init("avc1.64001F".to_owned());
        context.codec_facts = Some(facts);
        let master = master_playlist(&file, None, &context);
        assert!(master.contains("CODECS=\"avc1.64001F\""), "{master}");
        assert!(!master.contains("mp4a.40.2"));
        assert!(!master.contains("VIDEO-RANGE="));
        assert_eq!(master.matches("#EXT-X-STREAM-INF:").count(), 1);
    }

    #[test]
    fn source_probe_preserves_fractional_video_frame_rate() {
        let probe = r#"{
            "streams": [
                {"codec_type":"audio", "avg_frame_rate":"0/0"},
                {"codec_type":"video", "avg_frame_rate":"24000/1001", "r_frame_rate":"24/1"}
            ]
        }"#;
        let rate = video_frame_rate(probe).expect("frame rate");
        assert!((rate - 23.976).abs() < 0.001, "{rate}");
        assert!(video_frame_rate("not json").is_none());
    }

    fn sub(
        codec: &str,
        language: &str,
        title: &str,
        default: bool,
        forced: bool,
    ) -> SubtitleStream {
        SubtitleStream {
            index: 0,
            codec: codec.into(),
            language: Some(language.into()),
            title: Some(title.into()),
            default,
            forced,
            hearing_impaired: false,
        }
    }

    #[test]
    fn playback_audio_offset_is_bounded_and_carried_by_the_session() {
        let request = CreateSession {
            intent: None,
            control_sequence: None,
            playback_id: "player".into(),
            request_id: Some("attempt".into()),
            previous_session_id: None,
            reopen_reason: None,
            height: None,
            quality_auto: None,
            subtitle_burn: None,
            subtitle_burn_sdr: None,
            native_subtitles: None,
            subtitle: None,
            start: Some(12.0),
            audio: None,
            copy: Some(false),
            aac: None,
            preserve_dolby_vision: None,
            hdr10: None,
            audio_offset_ms: Some(20_000),
            presentation: None,
            block_budget_secs: None,
            transport: None,
            caps: None,
            overrides: None,
        }
        .into_request(7, 1080);

        assert_eq!(request.audio_offset_ms, 15_000);
        assert_eq!(request.file_id, 7);
    }

    #[test]
    fn bitmap_fallback_still_carries_an_explicit_burn_request() {
        let request = CreateSession {
            intent: None,
            control_sequence: None,
            playback_id: "apple-bitmap".into(),
            request_id: None,
            previous_session_id: None,
            reopen_reason: None,
            height: Some(2160),
            quality_auto: None,
            subtitle_burn: Some(5),
            subtitle_burn_sdr: None,
            native_subtitles: Some(true),
            subtitle: None,
            start: None,
            audio: None,
            copy: None,
            aac: None,
            preserve_dolby_vision: None,
            hdr10: None,
            audio_offset_ms: None,
            presentation: None,
            block_budget_secs: None,
            transport: None,
            caps: None,
            overrides: None,
        }
        .into_request(5615, 2160);
        assert_eq!(request.subtitle_burn, Some(5));
        assert!(matches!(
            request.kind,
            crate::transcode::SessionKind::Transcode { height: 2160 }
        ));
    }

    /// The create guard's arithmetic, without a server around it: session
    /// kind and no-burn grade in, wire range out, one guard's verdict on it.
    ///
    /// `burn_would_discard_this_session_hdr` is these three lines plus a store
    /// read for the grade, so this pins everything about it that can be wrong
    /// without pinning `grade_preview`'s own encoder proof — which belongs to
    /// the pipeline, and which the HTTP regressions exercise end to end.
    #[test]
    fn the_create_guard_reads_the_grade_this_session_would_deliver_without_a_burn() {
        use plurx_core::playback::{burn_would_discard_hdr, delivered_dynamic_range};
        use plurx_core::transcode::OutputGrade;

        let mut file = hls_file(vec![]);
        let copy = crate::transcode::SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: true,
            convert_dolby_vision: false,
        };
        let transcode = crate::transcode::SessionKind::Transcode { height: 2160 };

        let verdict =
            |kind: &crate::transcode::SessionKind, file: &MediaFile, base_grade: OutputGrade| {
                let (method, preserve, _) = session_delivery_shape(kind);
                let range = delivered_dynamic_range(file, method, preserve, base_grade);
                (range, burn_would_discard_hdr(range, true))
            };

        for hdr in ["dolby_vision", "hdr10", "hlg"] {
            file.hdr = Some(hdr.into());
            // Row 1: an HDR copy has no encode to burn into, so keeping the
            // grade and honouring the request are genuinely exclusive.
            assert!(
                verdict(&copy, &file, OutputGrade::Sdr).1,
                "{hdr}: an HDR copy must refuse"
            );
            // Row 2: the one the old predicate got wrong. Since the M4 rung a
            // transcode can negotiate HDR10, and a burn into it drops the
            // grade — being a transcode is not an exemption any more.
            assert_eq!(
                verdict(&transcode, &file, OutputGrade::Hdr10),
                ("hdr10", true),
                "{hdr}: a negotiated HDR10 transcode is an HDR delivery"
            );
            // Row 3: already tone-mapped. The old create guard refused this
            // because the *file* was HDR; nothing is being taken away.
            assert_eq!(
                verdict(&transcode, &file, OutputGrade::Sdr),
                ("sdr", false),
                "{hdr}: an already tone-mapped transcode keeps its burn"
            );
        }

        // Row 4: an SDR source, by any method.
        file.hdr = None;
        assert_eq!(verdict(&copy, &file, OutputGrade::Sdr), ("sdr", false));
        assert_eq!(verdict(&transcode, &file, OutputGrade::Sdr), ("sdr", false));
    }

    /// The session's answer is the one that wins once playback attaches, so
    /// it has to be read off the session that was built — not off the
    /// decision that suggested one. A DV remux the client can take delivers
    /// Dolby Vision; the same file behind a rung or a burn delivers SDR,
    /// because the encoder tone-maps it.
    #[test]
    fn a_session_reports_the_dynamic_range_of_the_stream_it_just_built() {
        let file = hls_file(vec![]);
        let copy = |preserve: bool| crate::transcode::SessionKind::Copy {
            convert_dolby_vision: false,
            aac: false,
            preserve_dolby_vision: preserve,
        };
        use plurx_core::transcode::OutputGrade;
        assert_eq!(
            session_delivered_dynamic_range(Some(&file), &copy(true), OutputGrade::Sdr),
            Some("dolby_vision")
        );
        // Stripped: what reaches the client is the compatible base layer.
        let mut base = file.clone();
        base.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        assert_eq!(
            session_delivered_dynamic_range(Some(&base), &copy(false), OutputGrade::Sdr),
            Some("hdr10")
        );
        assert_eq!(
            session_delivered_dynamic_range(
                Some(&file),
                &crate::transcode::SessionKind::Transcode { height: 1080 },
                OutputGrade::Sdr,
            ),
            Some("sdr"),
            "an SDR-grade transcode is H.264 8-bit, whatever the source carried"
        );
        // …and the HDR10 rung of the same source says so, on the same helper.
        // The session's grade is what decides it, so a rung the server refused
        // cannot report HDR10 by having been asked for.
        assert_eq!(
            session_delivered_dynamic_range(
                Some(&file),
                &crate::transcode::SessionKind::Transcode { height: 1080 },
                OutputGrade::Hdr10,
            ),
            Some("hdr10"),
            "the Dolby Vision → HDR10 rung is not SDR"
        );
        // A file that vanished from the store mid-request says nothing at
        // all rather than guessing; the client keeps what it had.
        assert_eq!(
            session_delivered_dynamic_range(None, &copy(true), OutputGrade::Sdr),
            None
        );
    }

    #[test]
    fn native_hls_master_advertises_selection_language_names_and_forced_metadata() {
        let file = hls_file(vec![
            sub("subrip", "ita", "Forced", true, true),
            sub("subrip", "ita", "Regular", false, false),
            sub("subrip", "eng", "Forced", false, false),
            sub("subrip", "eng", "Regular", false, false),
            sub("webvtt", "eng", "SDH", false, false),
        ]);
        let master = master_playlist(&file, Some(2), &sdr_context());

        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
        assert!(master.contains(
            "#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,\
             RESOLUTION=3840x2160,FRAME-RATE=23.976,CLOSED-CAPTIONS=NONE,SUBTITLES=\"subs\""
        ));
        assert!(!master.contains("CODECS="));
        assert!(!master.contains("#EXT-X-INDEPENDENT-SEGMENTS"));
        assert!(master.contains("NAME=\"English · Forced\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,FORCED=YES,URI=\"subs/2/index.m3u8\""));
        assert!(master.contains(
            "NAME=\"Italian · Forced\",LANGUAGE=\"it\",DEFAULT=NO,AUTOSELECT=YES,FORCED=YES"
        ));
        assert!(master.contains("NAME=\"English · Regular\",LANGUAGE=\"en\",DEFAULT=NO"));
        assert!(master.contains("NAME=\"English · SDH\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,FORCED=NO,CHARACTERISTICS=\"public.accessibility.transcribes-spoken-dialog,public.accessibility.describes-music-and-sound\""));
        assert!(master.ends_with("index.m3u8\n"));
    }

    #[test]
    fn continuous_master_keeps_verified_variants_and_one_common_subtitle_group() {
        let file = hls_file(vec![sub("subrip", "eng", "Regular", false, false),
            sub("webvtt", "eng", "SDH", false, false), sub("hdmv_pgs_subtitle", "eng", "Bitmap", false, false)]);
        let original = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",URI=\"audio/exact/index.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=1280x720,CODECS=\"avc1.640032,mp4a.40.2\",AUDIO=\"audio\"\nvideo/low/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1920x1080,CODECS=\"avc1.640032,mp4a.40.2\",AUDIO=\"audio\"\nvideo/high/index.m3u8\n";
        let master = String::from_utf8(continuous_master_with_subtitles(original.as_bytes().to_vec(), &file, Some(1)).expect("caption family")).expect("UTF-8");
        assert_eq!(master.matches("TYPE=AUDIO").count(), 1);
        assert_eq!(master.matches("TYPE=SUBTITLES").count(), 2);
        assert_eq!(master.matches(",SUBTITLES=\"subs\"").count(), 2);
        assert!(master.contains("DEFAULT=YES"));
        assert!(!master.contains("subs/2/"));
        assert!(!master.contains("40000000") && !master.contains("3840x2160"));
        for line in original.lines().filter(|line| !line.starts_with("#EXT-X-STREAM-INF:")) { assert!(master.lines().any(|candidate| candidate == line)); }
        let none = hls_file(vec![]);
        assert_eq!(continuous_master_with_subtitles(original.as_bytes().to_vec(), &none, None).expect("no native captions"), original.as_bytes());
    }

    #[tokio::test]
    async fn terminal_quality_snapshot_preserves_pins_and_requires_exact_attachment() {
        use plurx_core::playback::continuous_quality::{QualityAttachment,QualityLedger,QualityInterval,QualityOperation,QualityTransitionRequest};
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        activate_fixture_route(&fixture,&session_id,"terminal-quality-read").await;
        let route = fixture.store.media_session_route(&session_id).await.expect("route read").expect("active parent");
        let attachment = QualityAttachment { client_instance_id:uuid::Uuid::new_v4().to_string(),lifetime_id:"film".into(),
            attachment_id:uuid::Uuid::new_v4().to_string(),family_id:"a".repeat(64) };
        let interval = QualityInterval { artifact_id:"b".repeat(64),rendition_id:"c".repeat(64),timescale:24000,
            from_tick:0,through_tick:48048,byte_length:1024 };
        let transition = QualityTransitionRequest {version:1,generation:route.incarnation_id.clone(),control_epoch:route.owner_epoch as u64,
            sequence:1,attachment:attachment.clone(),transaction_id:uuid::Uuid::new_v4().to_string(),
            operation:QualityOperation::Prepare { intent_revision:1,target_rendition_id:interval.rendition_id.clone() }};
        let mut ledger = QualityLedger::new(route.incarnation_id.clone(),route.owner_epoch as u64,attachment.clone()).expect("ledger");
        ledger.apply(&transition,unix_ms()).expect("intent");ledger.ready(&transition.transaction_id,vec![interval.clone()]).expect("owner ready");
        let mut scheduled = transition.clone();scheduled.sequence=2;scheduled.operation=QualityOperation::Scheduled {intervals:vec![interval.clone()]};
        ledger.apply(&scheduled,unix_ms()).expect("scheduled");
        assert!(fixture.store.write_quality_ledger(&ledger,&route.owner_node_id,0,unix_ms()).await.expect("durable scheduled facts"));
        fixture.store.end_media_session(&session_id,"deleted",unix_ms()).await.expect("End").expect("terminal parent");
        let request = crate::vodserve::QualityScheduleRequest { version:1,generation:route.incarnation_id.clone(),control_epoch:route.owner_epoch as u64,
            attachment,transition:None,frontier:None,window:None };
        let read = |body:crate::vodserve::QualityScheduleRequest| quality_schedule(State(fixture.state.clone()),AxPath(session_id.clone()),
            Bytes::from(serde_json::to_vec(&body).expect("request JSON")));
        let before = fixture.store.quality_ledger(&route.incarnation_id).await.expect("before read").expect("ledger");
        let response = read(request.clone()).await;
        assert_eq!(response.status(),StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(),QUALITY_SCHEDULE_MAX_RESPONSE_BYTES).await.expect("bounded body");
        let reply:crate::vodserve::QualityScheduleResponse=serde_json::from_slice(&body).expect("terminal proof");
        assert!(reply.terminal && reply.valid_for(&request));assert!(reply.receipt.is_none());
        assert_eq!(reply.ledger.transactions[0].reserved,vec![interval.clone()]);
        assert_eq!(fixture.store.quality_ledger(&route.incarnation_id).await.expect("after read").expect("ledger"),before,"read cannot reduce dependencies");
        let mut wrong = request.clone();wrong.attachment.attachment_id=uuid::Uuid::new_v4().to_string();
        assert_eq!(read(wrong).await.status(),StatusCode::CONFLICT);
        let mut late = request.clone();late.transition=Some(scheduled);
        assert_eq!(read(late).await.status(),StatusCode::GONE,"End cannot reserve late media");
        let mut disposed = transition;disposed.sequence=3;disposed.operation=QualityOperation::Disposed { artifacts:vec![interval.artifact_id] };
        let mut completed=request;completed.transition=Some(disposed);
        assert_eq!(read(completed).await.status(),StatusCode::OK);
        assert!(fixture.store.quality_reserved_intervals(&interval.rendition_id).await.expect("released pins").is_empty());
    }

    /// Ended-session reconciliation needs only the Store. Relaying it to an
    /// owner that has left the cluster answered 503 forever, so a departed
    /// owner stranded every pin the client was trying to release.
    #[tokio::test]
    async fn terminal_quality_reconciliation_is_answered_without_its_departed_owner() {
        use plurx_core::playback::continuous_quality::{QualityAttachment,QualityLedger,QualityInterval,QualityOperation,QualityTransitionRequest};
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-terminal-worker").await;
        let user = fixture.store.create_user("terminal-departed", "hash", false).await.expect("terminal user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let now_ms = unix_ms();
        let route = activate_ready(&fixture.store, MediaSessionActivation {
            recovery_epoch: String::new(), expected_desired_revision: None,
            incarnation_id: uuid::Uuid::new_v4().to_string(), session_id: session_id.clone(),
            user_id: user.id, playback_id: "terminal-departed".to_owned(),
            expected_predecessor_incarnation_id: None, fence_predecessor: false, request_id: None,
            request_fingerprint: "a".repeat(64),
            // Not this node, and not any reachable member.
            owner_node_id: "departed-owner".to_owned(),
            lease_expires_at_ms: now_ms.saturating_add(60_000),
            recipe_json: "{}".to_owned(), response_json: "{}".to_owned(),
            publication_ready_at_ms: 0, media_origin_ms: 0, now_ms,
        }).await;
        assert_ne!(route.owner_node_id, fixture.state.node_id);
        let attachment = QualityAttachment { client_instance_id:uuid::Uuid::new_v4().to_string(),lifetime_id:"film".into(),
            attachment_id:uuid::Uuid::new_v4().to_string(),family_id:"a".repeat(64) };
        let interval = QualityInterval { artifact_id:"b".repeat(64),rendition_id:"c".repeat(64),timescale:24000,
            from_tick:0,through_tick:48048,byte_length:1024 };
        let transition = QualityTransitionRequest {version:1,generation:route.incarnation_id.clone(),control_epoch:route.owner_epoch as u64,
            sequence:1,attachment:attachment.clone(),transaction_id:uuid::Uuid::new_v4().to_string(),
            operation:QualityOperation::Prepare { intent_revision:1,target_rendition_id:interval.rendition_id.clone() }};
        let mut ledger = QualityLedger::new(route.incarnation_id.clone(),route.owner_epoch as u64,attachment.clone()).expect("ledger");
        ledger.apply(&transition,unix_ms()).expect("intent");ledger.ready(&transition.transaction_id,vec![interval.clone()]).expect("owner ready");
        let mut scheduled = transition.clone();scheduled.sequence=2;scheduled.operation=QualityOperation::Scheduled {intervals:vec![interval.clone()]};
        ledger.apply(&scheduled,unix_ms()).expect("scheduled");
        assert!(fixture.store.write_quality_ledger(&ledger,&route.owner_node_id,0,unix_ms()).await.expect("durable scheduled facts"));
        fixture.store.end_media_session(&session_id,"deleted",unix_ms()).await.expect("End").expect("terminal parent");
        let send = |body:crate::vodserve::QualityScheduleRequest| quality_schedule(State(fixture.state.clone()),AxPath(session_id.clone()),
            Bytes::from(serde_json::to_vec(&body).expect("request JSON")));
        let read = crate::vodserve::QualityScheduleRequest { version:1,generation:route.incarnation_id.clone(),control_epoch:route.owner_epoch as u64,
            attachment,transition:None,frontier:None,window:None };
        assert_eq!(send(read.clone()).await.status(),StatusCode::OK,"a terminal read is a Store read");
        let mut disposed = transition;disposed.sequence=3;disposed.operation=QualityOperation::Disposed { artifacts:vec![interval.artifact_id] };
        let mut completed = read.clone();completed.transition=Some(disposed);
        let response = send(completed.clone()).await;
        assert_eq!(response.status(),StatusCode::OK,"terminal disposal must not wait on the departed owner");
        let body = axum::body::to_bytes(response.into_body(),QUALITY_SCHEDULE_MAX_RESPONSE_BYTES).await.expect("bounded body");
        let reply:crate::vodserve::QualityScheduleResponse=serde_json::from_slice(&body).expect("terminal proof");
        assert!(reply.terminal && reply.receipt.is_some() && reply.valid_for(&completed));
        assert!(fixture.store.quality_reserved_intervals(&interval.rendition_id).await.expect("released pins").is_empty());
        // Authorization is unchanged: the wrong generation is still refused.
        let mut stale = read;stale.generation=uuid::Uuid::new_v4().to_string();
        assert_eq!(send(stale).await.status(),StatusCode::CONFLICT);
    }

    /// The session id is the capability and the route needs no login, so an
    /// unknown capability must be answered before it spends the node-wide
    /// schedule budget; otherwise a spray of random UUIDs 429s every viewer.
    #[tokio::test]
    async fn unknown_quality_schedule_capabilities_do_not_spend_the_node_budget() {
        use plurx_core::playback::continuous_quality::QualityAttachment;
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), &uuid::Uuid::new_v4().to_string()).await;
        let request = crate::vodserve::QualityScheduleRequest { version:1,generation:uuid::Uuid::new_v4().to_string(),control_epoch:1,
            attachment:QualityAttachment { client_instance_id:uuid::Uuid::new_v4().to_string(),lifetime_id:"film".into(),
                attachment_id:uuid::Uuid::new_v4().to_string(),family_id:"a".repeat(64) },
            transition:None,frontier:None,window:None };
        assert!(request.valid());
        for _ in 0..8 {
            let response = quality_schedule(State(fixture.state.clone()),AxPath(uuid::Uuid::new_v4().to_string()),
                Bytes::from(serde_json::to_vec(&request).expect("request JSON"))).await;
            assert_eq!(response.status(),StatusCode::NOT_FOUND);
        }
        assert_eq!(fixture.state.media_sessions.quality_schedule_admissions_in_window(),0,
            "an unknown capability spent the node-wide schedule budget");
    }

    /// Only a failure whose premise is gone may answer 4xx: clients settle any
    /// 4xx except 429 by one ledger read and drop the viewer's change.
    #[test]
    fn quality_schedule_refusal_table_names_messages_the_owner_still_produces() {
        // The refusal table matches exact owner messages. A renamed message
        // would silently turn a final refusal into a retried 503.
        let owner = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/vod/serve/quality.rs"));
        for message in QUALITY_SCHEDULE_REFUSALS {
            assert!(owner.contains(&format!("\"{message}")), "{message}");
        }
    }

    #[test]
    fn quality_schedule_failures_separate_final_refusals_from_transient_ones() {
        use plurx_core::playback::continuous_quality::QualityTransitionError;
        for transient in [
            "database error: disk I/O error",
            "storage task failed: join error",
            "continuous family is not attached",
            "quality parent is not attached",
            "quality schedule revision changed repeatedly",
            "quality window revision changed repeatedly",
            "controlled settlement exceeded its response deadline",
            "quality ledger settlement exceeded its inherited deadline",
            "controlled admission exceeded its original deadline",
            "quality preparation ledger disappeared",
        ] {
            assert_eq!(classify_quality_schedule_error(transient),QualityScheduleFailure::Transient,"{transient}");
        }
        for refused in [
            "quality family changed",
            "quality attachment changed",
            "quality owner epoch changed",
            "quality target is outside its family",
            "quality response owner changed",
        ] {
            assert_eq!(classify_quality_schedule_error(refused),QualityScheduleFailure::Refused,"{refused}");
        }
        for error in [QualityTransitionError::StaleSequence,QualityTransitionError::ConflictingReplay,QualityTransitionError::Capacity] {
            assert_eq!(classify_quality_schedule_error(&error.to_string()),QualityScheduleFailure::Refused,"{error}");
        }
        assert_eq!(classify_quality_schedule_error("controlled parent ended"),QualityScheduleFailure::Ended);
        assert_eq!(classify_quality_schedule_error("invalid quality schedule request"),QualityScheduleFailure::Invalid);
        let transient = QualityScheduleFailure::Transient.response();
        assert_eq!(transient.status(),StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(transient.headers().get(header::RETRY_AFTER).and_then(|value| value.to_str().ok()),Some("1"));
        assert_eq!(QualityScheduleFailure::Refused.response().status(),StatusCode::CONFLICT);
        assert_eq!(QualityScheduleFailure::Ended.response().status(),StatusCode::GONE);
    }

    #[test]
    fn continuous_catalog_pairs_require_one_worker_and_distinct_actual_rasters() {
        use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
        let candidate = |node: &str, seed: u8, target, width, height| crate::media_pool::WorkerQualityCandidate {
            node_id: node.into(),
            binding: None, dispatch_supported: true, partial: false,
            candidate: QualityCandidate {
                id: CandidateId::for_recipe_digest([seed;32]), recipe_digest: [seed;32],
                route: CandidateRoute::Encode, normalized_geometry: true, width, height,
                target_height: target, average_bps: None, peak_bps: None,
                grade: plurx_core::transcode::OutputGrade::Sdr,
                decoder_compatible: true, complete_cache: false, sustainable: true,
            },
        };
        let primary = candidate("one",1,1080,1920,1080);
        let other_node = candidate("two",2,720,1280,720);
        let alias = candidate("one",3,900,1920,1080);
        assert!(continuous_candidates_from_workers(&[primary.clone(), other_node.clone(), alias.clone()]).pairs.is_empty());
        let companion = candidate("one",4,720,1280,720);
        let response = continuous_candidates_from_workers(&[primary.clone(),other_node,alias,companion.clone()]);
        let pair = response.pairs.iter().find(|pair|pair.primary_candidate_id==primary.candidate.id).expect("same worker family");
        assert_eq!(pair.companion_candidate_id,companion.candidate.id);
        let mut unsupported = companion;
        unsupported.candidate.normalized_geometry = false;
        assert!(continuous_candidates_from_workers(&[primary.clone(), unsupported.clone()]).pairs.is_empty());
        unsupported.candidate.normalized_geometry = true;
        unsupported.dispatch_supported = false;
        assert!(continuous_candidates_from_workers(&[primary.clone(), unsupported.clone()]).pairs.is_empty());
        unsupported.dispatch_supported = true;
        unsupported.partial = true;
        assert!(continuous_candidates_from_workers(&[primary, unsupported]).pairs.is_empty());
    }

    #[test]
    fn continuous_bootstrap_uses_durable_owner_without_legacy_control() {
        let mut route = eligible_owner_loss_route();
        route.state = "active".into();
        route.response_json = "{}".into(); // No legacy control bootstrap.
        route.lease_expires_at_ms = unix_ms().saturating_add(60_000);
        let bootstrap = ContinuousQualityBootstrap::from_route(&route).expect("active owner");
        assert_eq!(bootstrap.generation, route.incarnation_id);
        assert_eq!(bootstrap.control_epoch, 3);
        assert_eq!(bootstrap.schedule_url, format!("/api/v1/hls/{}/quality-schedule", route.session_id));
        assert_eq!(bootstrap.family_url, format!("/api/v1/hls/{}/quality-family", route.session_id));
        route.owner_epoch = 9_007_199_254_740_992;
        assert!(ContinuousQualityBootstrap::from_route(&route).is_err());
        route.owner_epoch = 3;
        route.state = "ended".into();
        assert!(ContinuousQualityBootstrap::from_route(&route).is_err());
        route.state = "active".into();
        route.lease_expires_at_ms = unix_ms() - 1;
        assert!(ContinuousQualityBootstrap::from_route(&route).is_err());
        route.lease_expires_at_ms = unix_ms().saturating_add(60_000);
        route.session_id = "malformed".into();
        assert!(ContinuousQualityBootstrap::from_route(&route).is_err());
    }

    #[test]
    fn continuous_create_is_a_strict_separate_two_role_envelope() {
        let wire = serde_json::json!({"version":1,"family_generation":uuid::Uuid::new_v4().to_string(),
            "primary_candidate_id":"a".repeat(32),"companion_candidate_id":"b".repeat(32),
            "start":{"playback_id":"family","caps":{"v":2}}});
        let request: CreateContinuousFamily = serde_json::from_value(wire.clone()).expect("family envelope");
        assert!(request.valid());
        let mut duplicate = wire.clone(); duplicate["companion_candidate_id"] = duplicate["primary_candidate_id"].clone();
        assert!(!serde_json::from_value::<CreateContinuousFamily>(duplicate).expect("shape").valid());
        let mut extra = wire; extra["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CreateContinuousFamily>(extra).is_err());
    }

    #[test]
    fn hdr_master_declares_the_range_and_exact_session_codecs() {
        let file = hls_file(vec![]);
        let stripped = hls_context("hvc1.2.4.L150.B0,mp4a.40.2", None);
        let master = master_playlist(&file, None, &stripped);
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
        assert!(master.contains(
            "#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,\
             RESOLUTION=3840x2160,FRAME-RATE=23.976,VIDEO-RANGE=PQ,\
             CODECS=\"hvc1.2.4.L150.B0,mp4a.40.2\""
        ));

        let compatible_dv = hls_context("hvc1.2.4.L150.B0,ec-3", Some("dvh1.08.10/db1p"));
        let master = master_playlist(&file, None, &compatible_dv);
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:10\n"));
        assert!(master.contains("VIDEO-RANGE=PQ"));
        assert!(master.contains("CODECS=\"hvc1.2.4.L150.B0,ec-3\""));
        assert!(master.contains("SUPPLEMENTAL-CODECS=\"dvh1.08.10/db1p\""));

        // A tone-mapped session keeps its source's HDR library metadata, but
        // its H.264 bytes and master are SDR.
        let transcoded = hls_context("avc1.640034,mp4a.40.2", None);
        let master = master_playlist(&file, None, &transcoded);
        assert!(!master.contains("VIDEO-RANGE="));
        assert!(!master.contains("CODECS="));
    }

    #[test]
    fn hdr_diagnostics_isolate_master_attributes_without_changing_the_child() {
        let file = hls_file(vec![sub("subrip", "eng", "Forced", true, false)]);
        let context = hls_context("hvc1.2.4.L150.B0,ec-3", None);

        let minimal = master_playlist_diagnostic(&file, None, &context, Some("video-only"));
        assert_eq!(
            minimal,
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,RESOLUTION=3840x2160,FRAME-RATE=23.976,CLOSED-CAPTIONS=NONE\nindex.m3u8\n"
        );

        let range = master_playlist_diagnostic(&file, None, &context, Some("video-only-range"));
        assert!(range.contains("VIDEO-RANGE=PQ,CLOSED-CAPTIONS=NONE\n"));
        assert!(!range.contains("CODECS="));
        assert!(!range.contains("SUBTITLES="));

        let codecs = master_playlist_diagnostic(&file, None, &context, Some("video-only-codecs"));
        assert!(codecs.contains("CODECS=\"hvc1.2.4.L150.B0,ec-3\""));
        assert!(!codecs.contains("VIDEO-RANGE="));

        let hdr = master_playlist_diagnostic(&file, None, &context, Some("video-only-hdr"));
        assert!(hdr.contains("VIDEO-RANGE=PQ"));
        assert!(hdr.contains("CODECS=\"hvc1.2.4.L150.B0,ec-3\""));
        assert!(hdr.ends_with("index.m3u8\n"));
    }

    /// The HDR10 rung's master, built from the codec string the session
    /// actually advertises.
    ///
    /// The point is that **no new attribute logic was written for it**. The
    /// existing rule — an `hvc1`/`hev1`/`dvh1`/`dvhe` prefix plus an HDR
    /// source column — already emits `VIDEO-RANGE=PQ` and `CODECS`, and a
    /// re-encoded HDR10 rung satisfies both. A second implementation of that
    /// rule is a second thing to keep in step.
    #[test]
    fn the_hdr10_rungs_master_gets_pq_and_hevc_codecs_from_the_existing_rule() {
        let file = hls_file(vec![]);
        // Exactly what the 1080p branch of
        // `transcode::transcoded_hls_codecs(OutputGrade::Hdr10, height)` puts
        // on the session.
        let context = hls_context("hvc1.2.4.H120.90,mp4a.40.2", None);
        let master = master_playlist(&file, None, &context);
        assert!(master.contains("VIDEO-RANGE=PQ"), "{master}");
        assert!(
            master.contains("CODECS=\"hvc1.2.4.H120.90,mp4a.40.2\""),
            "{master}"
        );
        // No SUPPLEMENTAL-CODECS: the RPU is consumed by the reshape, so
        // there is no Dolby Vision enhancement left to declare, and the
        // master stays at compatibility version 7.
        assert!(!master.contains("SUPPLEMENTAL-CODECS"), "{master}");
        assert!(master.contains("#EXT-X-VERSION:7"), "{master}");

        // The SDR rung of the same source is H.264, and its master must stay
        // SDR — the source's `hdr` column alone is not permission to claim PQ
        // for a tone-mapped picture.
        let sdr = master_playlist(&file, None, &hls_context("avc1.640034,mp4a.40.2", None));
        assert!(!sdr.contains("VIDEO-RANGE="), "{sdr}");
        assert!(!sdr.contains("CODECS="), "{sdr}");
    }

    #[test]
    fn high_tier_hevc_without_native_subtitles_uses_the_media_playlist() {
        let bitmap_only = hls_file(vec![sub(
            "hdmv_pgs_subtitle",
            "eng",
            "English",
            false,
            false,
        )]);
        let michael = hls_context("hvc1.2.4.H153.90,mp4a.40.2", None);
        assert!(should_serve_high_tier_media_playlist(
            &bitmap_only,
            &michael
        ));

        let main_tier = hls_context("hvc1.2.4.L153.B0,mp4a.40.2", None);
        assert!(!should_serve_high_tier_media_playlist(
            &bitmap_only,
            &main_tier
        ));

        let mut sdr_high_tier = bitmap_only.clone();
        sdr_high_tier.hdr = None;
        assert!(!should_serve_high_tier_media_playlist(
            &sdr_high_tier,
            &michael
        ));

        let native_text = hls_file(vec![sub("subrip", "eng", "English", false, false)]);
        assert!(!should_serve_high_tier_media_playlist(
            &native_text,
            &michael
        ));

        let high_profile_h264 = hls_context("avc1.640034,mp4a.40.2", None);
        assert!(!should_serve_high_tier_media_playlist(
            &bitmap_only,
            &high_profile_h264
        ));
    }

    #[test]
    fn high_tier_hdr_init_is_relabelled_for_apple_hls() {
        let file = hls_file(vec![sub(
            "hdmv_pgs_subtitle",
            "eng",
            "English",
            false,
            false,
        )]);
        // hvcC version 1, Main 10, High tier, compatibility 4, level 153.
        let mut init = vec![0, 0, 0, 21];
        init.extend_from_slice(b"hvcC");
        init.extend_from_slice(&[1, 0x22, 0x20, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 153]);
        assert_eq!(
            hevc_codec_from_init(&init, "hvc1").as_deref(),
            Some("hvc1.2.4.H153.90")
        );
        assert!(normalize_high_tier_hevc_init(&file, &mut init));
        assert_eq!(
            hevc_codec_from_init(&init, "hvc1").as_deref(),
            Some("hvc1.2.4.L153.90")
        );
        assert!(!normalize_high_tier_hevc_init(&file, &mut init));

        let mut sdr = file;
        sdr.hdr = None;
        init[9] |= 0x20;
        assert!(!normalize_high_tier_hevc_init(&sdr, &mut init));
    }

    fn hls_context_with(codecs: &str, supplemental: Option<&str>) -> crate::transcode::HlsContext {
        crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: codecs.to_owned(),
            supplemental_codecs: supplemental.map(str::to_owned),
            frame_rate: None,
        }
    }

    /// A real muxer init carrying the Profile 7 record a stripping copy leaves
    /// behind, plus the `dby1` brand movenc writes beside it.
    fn init_with_a_stale_dolby_vision_record() -> Vec<u8> {
        use plurx_core::fmp4::{FragmentReader, Unit};
        let feed = plurx_core::testfixtures::pipe("clean-cra");
        let mut reader = FragmentReader::new();
        reader.push(&feed);
        let Ok(Some(Unit::Init(mut init))) = reader.next_unit() else {
            panic!("the fixture opens with an init");
        };
        let record = plurx_core::fmp4::DolbyVisionRecord::new(7, 6, true, true, false, 0)
            .expect("a describable record");
        assert!(plurx_core::fmp4::set_dolby_vision_record(&mut init, &record).expect("insert"));
        init.bytes[8..12].copy_from_slice(b"dby1");
        init.bytes
    }

    /// The legacy muxer path's catch: a playlist that advertises no Dolby
    /// Vision must not serve an init that declares it.
    ///
    /// That path has ffmpeg's own HLS muxer write `init.mp4` straight to disk
    /// with no reader in between, so neither `copyseg`'s removal nor the VOD
    /// promotion's runs. `filter_units` took the layers out by NAL type and
    /// left the DOVI side data the record was written from, so the init
    /// declares Profile 7 with an enhancement layer over a stream carrying
    /// neither — which VideoToolbox honours by refusing the hardware path.
    #[test]
    fn an_init_declaring_dolby_vision_the_playlist_does_not_is_stripped() {
        plurx_core::testfixtures::require_ffmpeg();
        let mut init = init_with_a_stale_dolby_vision_record();
        assert!(
            init.windows(4).any(|f| f == b"dvcC" || f == b"dvvC"),
            "the fixture must carry a record for this test to mean anything"
        );

        let stripped = hls_context_with("hvc1.2.4.L153.90", None);
        assert!(strip_unadvertised_dolby_vision(&stripped, &mut init));
        assert!(
            !init.windows(4).any(|f| f == b"dvcC" || f == b"dvvC"),
            "the record survived a playlist that advertises plain HEVC"
        );
        // The brand goes with it: `dby1` over a sample entry with no record is
        // the contradictory init AVPlayer refuses outright, which is a harder
        // failure than the stutter.
        assert!(!init.windows(4).any(|f| f == b"dby1"));
        // Idempotent — an init with nothing to remove is not an error.
        assert!(!strip_unadvertised_dolby_vision(&stripped, &mut init));
    }

    /// …and a session that DOES advertise Dolby Vision keeps its record.
    ///
    /// Both spellings, because a preserved Profile 5 names Dolby Vision in
    /// `CODECS` with no `SUPPLEMENTAL-CODECS` beside it — so "no supplemental"
    /// is not the question, and asking it would strip the record off the one
    /// kind of session that genuinely needs it.
    #[test]
    fn an_advertised_dolby_vision_session_keeps_its_record() {
        plurx_core::testfixtures::require_ffmpeg();
        let original = init_with_a_stale_dolby_vision_record();

        // Converted, and preserved-8.1: named in SUPPLEMENTAL-CODECS.
        for supplemental in ["dvh1.08.06/db1p", "dvh1.08.06/db4h"] {
            let mut init = original.clone();
            let context = hls_context_with("hvc1.2.4.L153.90", Some(supplemental));
            assert!(!strip_unadvertised_dolby_vision(&context, &mut init));
            assert_eq!(init, original, "{supplemental}");
        }

        // Preserved Profile 5: named in CODECS, nothing supplemental.
        let mut init = original.clone();
        let profile5 = hls_context_with("dvh1.05.06", None);
        assert!(!strip_unadvertised_dolby_vision(&profile5, &mut init));
        assert_eq!(init, original);
    }

    /// Anything this reader does not account for byte-for-byte is served as it
    /// is, rather than as the reader's idea of it.
    ///
    /// Two shapes, and the second is the one that matters. Bytes that do not
    /// parse at all are refused by the parse itself. Bytes that parse but
    /// carry more than the init — a fragment appended, a trailing box this
    /// model does not walk — would have their tail silently dropped, because
    /// the rewrite replaces the whole buffer with what the reader accounted
    /// for. This rewrites a file a client is about to play; truncating it is a
    /// worse outcome than leaving the record in.
    #[test]
    fn anything_the_reader_does_not_fully_account_for_is_left_alone() {
        let context = hls_context_with("hvc1.2.4.L153.90", None);

        let mut garbage = b"\x00\x00\x00\x10ftypiso6\x00\x00\x00\x00not-a-moov".to_vec();
        let before = garbage.clone();
        assert!(!strip_unadvertised_dolby_vision(&context, &mut garbage));
        assert_eq!(garbage, before);

        let mut empty = Vec::new();
        assert!(!strip_unadvertised_dolby_vision(&context, &mut empty));
        assert!(empty.is_empty());

        // A real init with a stale record — which this DOES strip — plus a
        // trailing byte it does not account for, which must stop it.
        plurx_core::testfixtures::require_ffmpeg();
        let mut alone = init_with_a_stale_dolby_vision_record();
        assert!(
            strip_unadvertised_dolby_vision(&context, &mut alone),
            "the control: this init on its own is rewritten"
        );

        let mut with_tail = init_with_a_stale_dolby_vision_record();
        let expected = with_tail.clone();
        with_tail.extend_from_slice(b"trailing");
        let before_tail = with_tail.clone();
        assert!(
            !strip_unadvertised_dolby_vision(&context, &mut with_tail),
            "an init with bytes past its end must not be rewritten"
        );
        assert_eq!(with_tail, before_tail, "and not truncated");
        assert_ne!(with_tail.len(), expected.len());
    }

    #[test]
    fn hevc_codec_uses_the_published_profile_tier_level_and_constraints() {
        // hvcC version 1, Main 10, High tier, compatibility 0x20000000
        // (RFC 6381 reverse-bit value 4), constraint B0, level 150. This is
        // the header carried by the live HDR regression file's init segment.
        let mut init = vec![0, 0, 0, 21];
        init.extend_from_slice(b"hvcC");
        init.extend_from_slice(&[1, 0x22, 0x20, 0, 0, 0, 0xB0, 0, 0, 0, 0, 0, 150]);

        assert_eq!(
            hevc_codec_from_init(&init, "hvc1").as_deref(),
            Some("hvc1.2.4.H150.B0")
        );
        assert!(hevc_codec_from_init(&init[..12], "hvc1").is_none());
    }

    #[test]
    fn avc_codec_from_init_reads_the_avcc_triplet() {
        let mut init = valid_avc_init();
        let avc1 = plurx_core::fmp4::avc_rfc6381_codec(&init)
            .expect("valid sample-entry layout")
            .expect("AVC codec");
        assert!(avc1.starts_with("avc1."));

        plurx_core::testfixtures::replace_avc_sample_entry(&mut init, *b"avc3");
        assert_eq!(
            plurx_core::fmp4::avc_rfc6381_codec(&init)
                .expect("valid avc3 layout")
                .expect("AVC codec"),
            avc1.replacen("avc1.", "avc3.", 1)
        );
    }

    #[test]
    fn avc_codec_from_init_ignores_an_earlier_decoy_box() {
        let mut init = valid_avc_init();
        let expected = plurx_core::fmp4::avc_rfc6381_codec(&init).expect("valid AVC layout");
        let ftyp_size =
            u32::from_be_bytes(init.bytes[0..4].try_into().expect("ftyp size")) as usize;
        let mut decoy = 20_u32.to_be_bytes().to_vec();
        decoy.extend_from_slice(b"free");
        decoy.extend_from_slice(&[0, 0, 0, 12]);
        decoy.extend_from_slice(b"avcC");
        decoy.extend_from_slice(&[1, 0x4D, 0, 0x1F]);
        init.bytes.splice(ftyp_size..ftyp_size, decoy);
        let init = parse_avc_init(&init.bytes);

        assert_eq!(
            plurx_core::fmp4::avc_rfc6381_codec(&init).expect("decoy init remains valid"),
            expected
        );
    }

    #[test]
    fn avc_codec_from_init_rejects_ambiguous_sample_descriptions() {
        let mut init = valid_avc_init();
        let expected = plurx_core::fmp4::avc_rfc6381_codec(&init).expect("valid AVC layout");
        plurx_core::testfixtures::duplicate_avc_sample_entry(&mut init);
        assert_eq!(
            plurx_core::fmp4::avc_rfc6381_codec(&init)
                .expect("matching duplicate descriptions remain valid"),
            expected
        );

        plurx_core::testfixtures::replace_second_avc_triplet(&mut init, 0x4D, 0, 0x1F);
        assert!(matches!(
            plurx_core::fmp4::avc_rfc6381_codec(&init),
            Err(plurx_core::fmp4::Fmp4Error::Unsupported(_))
        ));
    }

    #[test]
    fn avc_codec_from_init_refuses_a_truncated_box() {
        let mut init = valid_avc_init();
        init.bytes.truncate(init.bytes.len() - 1);

        assert!(matches!(
            plurx_core::fmp4::avc_rfc6381_codec(&init),
            Err(plurx_core::fmp4::Fmp4Error::Malformed(_))
        ));
    }

    fn parse_avc_init(bytes: &[u8]) -> plurx_core::fmp4::Init {
        use plurx_core::fmp4::{FragmentReader, Unit};
        let mut reader = FragmentReader::new();
        reader.push(bytes);
        let Some(Unit::Init(init)) = reader.next_unit().expect("fixture init parses") else {
            panic!("the fixture opens with an init");
        };
        init
    }

    fn valid_avc_init() -> plurx_core::fmp4::Init {
        let feed = plurx_core::testfixtures::pipe("h264");
        parse_avc_init(&feed)
    }

    /// Preparation can outlive the five-second response admission phase;
    /// both native entry points share the fixed outer preparation deadline.
    #[tokio::test]
    async fn native_master_preparation_can_wait_six_seconds_for_exact_init() {
        let dir = crate::test_tempdir().expect("delayed native init");
        let fixture =
            HlsDeliveryFixture::publish_copy_actor_managed(dir.path(), "delayed-native-init").await;
        fixture.mark_started().await;
        let init_path = dir.path().join("init.mp4");
        let feed = plurx_core::testfixtures::pipe("h264");
        let init = parse_avc_init(&feed);
        // Copy response admission validates its first fragment as well as
        // init. Seed that fragment so this isolates delayed init preparation,
        // rather than waiting for a missing first-media authorization object.
        tokio::fs::write(dir.path().join("seg00000.m4s"), &feed[init.bytes.len()..])
            .await
            .expect("complete first AVC fragment");
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(6)).await;
            tokio::fs::write(init_path, init.bytes)
                .await
                .expect("delayed exact init");
        });
        let started = Instant::now();
        let deadline = started + Duration::from_secs(12);
        let query = || PlaylistQuery {
            native: Some(1),
            subtitle: None,
            diagnostic: None,
        };
        // Each request runs as its own heap-allocated task in production.
        // Joining both handler state machines inline on a 2 MiB debug test
        // thread overflows it, so box them the way a spawned task holds them.
        let (master, legacy) = tokio::join!(
            Box::pin(master_playlist_response_local_before(
                &fixture.state,
                "delayed-native-init",
                query(),
                deadline,
                deadline
            )),
            Box::pin(playlist_local_before(
                &fixture.state,
                "delayed-native-init",
                query(),
                deadline,
                deadline
            )),
        );
        assert_eq!(
            master.expect("master waits through preparation").status(),
            StatusCode::OK
        );
        assert_eq!(
            legacy
                .expect("legacy native bridge waits through preparation")
                .status(),
            StatusCode::OK
        );
        assert!(started.elapsed() >= Duration::from_secs(6));
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "no renewed outer deadline"
        );
        writer.await.expect("init writer");
    }

    #[tokio::test]
    async fn actual_avc_init_binds_video_without_inventing_missing_audio_facts() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "avc-facts").await;
        fixture.make_segment_window_servable().await;
        let init = valid_avc_init();
        let expected = plurx_core::fmp4::avc_rfc6381_codec(&init)
            .expect("valid avcC").expect("AVC codec");
        tokio::fs::write(dir.path().join("init.mp4"), &init.bytes).await.expect("write init");
        let mut context = hls_context_with("avc1,mp4a.40.2", None);
        context.codec_facts = Some(crate::transcode::FrozenHlsCodecFacts::audio(None, true, false));
        let resolved = exact_hls_context(&fixture.state, "avc-facts", context.clone()).await.expect("unknown audio resolves without declaration");
        assert_eq!(resolved.codec_facts.expect("component facts").complete_sdr_codecs(), None);
        context.codec_facts = Some(crate::transcode::FrozenHlsCodecFacts::audio(None, false, false));
        let resolved = exact_hls_context(&fixture.state, "avc-facts", context).await.expect("video-only init resolves");
        let facts = resolved.codec_facts.expect("component facts");
        assert_eq!(facts.complete_sdr_codecs(), Some(expected));
        assert!(serde_json::to_string(&facts).expect("serializable facts").contains("OutputInit"));
    }

    #[tokio::test]
    async fn an_fmp4_avc_session_normalises_its_codec_from_the_init() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "avc-init").await;
        fixture.make_segment_window_servable().await;
        let init = valid_avc_init();
        let expected = plurx_core::fmp4::avc_rfc6381_codec(&init)
            .expect("valid sample-entry layout")
            .expect("fixture carries avcC");
        tokio::fs::write(dir.path().join("init.mp4"), &init.bytes)
            .await
            .expect("AVC init");

        let context = crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "avc1.640034,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "avc-init", context)
            .await
            .expect("valid AVC init is supported");

        assert_eq!(resolved.codecs, format!("{expected},mp4a.40.2"));
    }

    #[tokio::test]
    async fn an_fmp4_avc_session_refuses_ambiguous_sample_descriptions() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "ambiguous-avc-init").await;
        fixture.make_segment_window_servable().await;
        let mut init = valid_avc_init();
        plurx_core::testfixtures::duplicate_avc_sample_entry(&mut init);
        plurx_core::testfixtures::replace_second_avc_triplet(&mut init, 0x4D, 0, 0x1F);
        tokio::fs::write(dir.path().join("init.mp4"), &init.bytes)
            .await
            .expect("ambiguous AVC init");
        let context = crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "avc1.640034,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };

        let error = exact_hls_context(&fixture.state, "ambiguous-avc-init", context)
            .await
            .expect_err("ambiguous AVC identity must not be published");
        assert!(matches!(
            error,
            HlsInitInspectionError::Response {
                code: "hls_init_unsupported",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn an_mpegts_session_keeps_its_static_codec_string() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "mpegts-avc").await;
        let context = crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "avc1.640034,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };

        let resolved = exact_hls_context_at(
            &fixture.state,
            "mpegts-avc",
            context.clone(),
            false,
            Instant::now() + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET,
        )
        .await
        .expect("MPEG-TS needs no init inspection");

        assert_eq!(resolved, context);
    }

    /// A `dvcC` record laid out the way the reference episode I Profile 5 title's init
    /// segment carries it: version 1.0, profile 5, level 6, RPU and BL
    /// present, no enhancement layer, compatibility id 0.
    fn dolby_vision_init(profile: u8, level: u8) -> Vec<u8> {
        let mut init = vec![0, 0, 0, 32];
        init.extend_from_slice(b"dvcC");
        init.extend_from_slice(&[
            1,
            0,
            (profile << 1) | (level >> 5),
            ((level & 0x1f) << 3) | 0b101,
            0,
        ]);
        init.extend_from_slice(&[0; 19]);
        init
    }

    fn valid_dolby_vision_init(profile: u8, level: u8) -> Vec<u8> {
        use plurx_core::fmp4::{FragmentReader, Unit};
        let feed = plurx_core::testfixtures::pipe("clean-cra");
        let mut reader = FragmentReader::new();
        reader.push(&feed);
        let Some(Unit::Init(mut init)) = reader.next_unit().expect("fixture init parses") else {
            panic!("the fixture opens with an init");
        };
        let record = plurx_core::fmp4::DolbyVisionRecord::new(profile, level, true, false, true, 0)
            .expect("a describable Dolby Vision record");
        assert!(
            plurx_core::fmp4::set_dolby_vision_record(&mut init, &record)
                .expect("insert Dolby Vision record")
        );
        init.bytes
    }

    fn valid_hevc_init() -> Vec<u8> {
        use plurx_core::fmp4::{FragmentReader, Unit};
        let feed = plurx_core::testfixtures::pipe("clean-cra");
        let mut reader = FragmentReader::new();
        reader.push(&feed);
        let Some(Unit::Init(init)) = reader.next_unit().expect("fixture init parses") else {
            panic!("the fixture opens with an init");
        };
        init.bytes
    }

    #[test]
    fn dolby_vision_codec_is_read_from_its_own_configuration_record() {
        let init = dolby_vision_init(5, 6);
        assert_eq!(
            dolby_vision_codec_from_init(&init, "dvh1").as_deref(),
            Some("dvh1.05.06")
        );
        assert_eq!(
            dolby_vision_codec_from_init(&init, "dvhe").as_deref(),
            Some("dvhe.05.06")
        );
        // Profile 8 level 10 exercises the level's high bit, which lives in
        // the profile's byte.
        assert_eq!(
            dolby_vision_codec_from_init(&dolby_vision_init(8, 10), "dvh1").as_deref(),
            Some("dvh1.08.10")
        );
        assert_eq!(
            dolby_vision_codec_from_init(&dolby_vision_init(7, 33), "dvh1").as_deref(),
            Some("dvh1.07.33")
        );
        // Truncated, absent and empty records decline rather than guess.
        assert!(dolby_vision_codec_from_init(&init[..16], "dvh1").is_none());
        assert!(dolby_vision_codec_from_init(b"nothing here at all", "dvh1").is_none());
        assert!(dolby_vision_codec_from_init(&dolby_vision_init(0, 6), "dvh1").is_none());
        assert!(dolby_vision_codec_from_init(&dolby_vision_init(5, 0), "dvh1").is_none());
    }

    /// The regression this milestone exists for. A preserved Profile 5 init
    /// carries `dvh1` + `hvcC` + `dvcC`; reading the tier out of `hvcC` and
    /// publishing `dvh1.2.4.L150.90` is what AVPlayer refused with CoreMedia
    /// -15517 while the same session's CODECS-free media playlist played.
    #[tokio::test]
    async fn a_preserved_dolby_vision_master_keeps_its_dolby_vision_identifier() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "dv").await;
        fixture.make_segment_window_servable().await;
        let init = valid_dolby_vision_init(5, 6);
        tokio::fs::write(dir.path().join("init.mp4"), &init)
            .await
            .expect("dolby vision init");

        let context = crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "dvh1.05.06,ec-3".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "dv", context)
            .await
            .expect("the valid Dolby Vision init is supported");
        assert_eq!(
            resolved.codecs, "dvh1.05.06,ec-3",
            "a Dolby Vision master must not be rewritten into HEVC shape"
        );
    }

    /// The same probe also heals the other half of the failure: when the
    /// library row carried no DOVI side data, `copied_hls_codecs` advertises a
    /// bare `dvh1`, which the code's own comment calls fatal during asset
    /// preparation. The init knows the answer.
    #[tokio::test]
    async fn a_bare_dolby_vision_declaration_is_completed_from_the_init() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "dv-bare").await;
        fixture.make_segment_window_servable().await;
        tokio::fs::write(dir.path().join("init.mp4"), valid_dolby_vision_init(5, 6))
            .await
            .expect("dolby vision init");

        let context = crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "dvh1,ec-3".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "dv-bare", context)
            .await
            .expect("the valid Dolby Vision init is supported");
        assert_eq!(resolved.codecs, "dvh1.05.06,ec-3");
    }

    /// A Dolby Vision declaration without its required configuration record
    /// is invalid rather than an invitation to advertise scanner guesses.
    #[tokio::test]
    async fn web_hls_startup_dolby_init_without_configuration_is_invalid() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "dv-nodvcc").await;
        fixture.make_segment_window_servable().await;
        let init = valid_hevc_init();
        tokio::fs::write(dir.path().join("init.mp4"), &init)
            .await
            .expect("init without dvcC");

        let context = crate::transcode::HlsContext {
            codec_facts: None,
            bandwidth: None,
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "dvh1.05.06,ec-3".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "dv-nodvcc", context).await;
        assert!(matches!(
            resolved,
            Err(HlsInitInspectionError::Response {
                code: "hls_init_invalid",
                ..
            })
        ));
    }

    #[test]
    fn a_profile_5_master_declares_dolby_vision_without_a_supplemental_codec() {
        let file = hls_file(vec![]);
        let profile5 = hls_context("dvh1.05.06,ec-3", None);
        let master = master_playlist(&file, None, &profile5);

        // Profile 5 has no compatible base layer to declare, so the master
        // stays at version 7 — SUPPLEMENTAL-CODECS would drag it to 10, which
        // this code already documents as rejection-prone.
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
        assert!(master.contains("VIDEO-RANGE=PQ"));
        assert!(master.contains("CODECS=\"dvh1.05.06,ec-3\""));
        assert!(!master.contains("SUPPLEMENTAL-CODECS"));
    }

    /// Who gets the accessibility tag: the muxer's answer first, the track's
    /// name only as a fallback. The fallback is not decoration — files
    /// probed before plurx read the disposition carry `false` for it, and
    /// re-probing is a manual scan, so the naming convention is still the
    /// only signal an existing library has.
    #[test]
    fn sdh_renditions_are_tagged_from_the_disposition_before_the_title() {
        let accessibility = "CHARACTERISTICS=\"public.accessibility.transcribes-spoken-dialog,public.accessibility.describes-music-and-sound\"";
        let flagged = SubtitleStream {
            hearing_impaired: true,
            ..sub("subrip", "eng", "English", false, false)
        };
        assert!(subtitle_characteristics(&flagged).is_some());

        let file = hls_file(vec![
            flagged,
            sub("subrip", "eng", "English SDH", false, false),
            sub("subrip", "eng", "Regular", false, false),
        ]);
        let master = master_playlist(&file, None, &sdr_context());
        assert_eq!(
            master.matches(accessibility).count(),
            2,
            "the flagged track and the named one, and only those: {master}"
        );
        let line = |name: &str| {
            master
                .lines()
                .find(|line| line.contains(&format!("NAME=\"{name}\"")))
                .unwrap_or_else(|| panic!("no rendition named {name} in {master}"))
        };
        assert!(
            line("English · English").contains(accessibility),
            "the disposition tags a track its title says nothing about"
        );
        assert!(line("English · English SDH").contains(accessibility));
        assert!(!line("English · Regular").contains(accessibility));

        // A track with no title at all is not an accessibility track by
        // default — silence is not a claim.
        let untitled = SubtitleStream {
            title: None,
            ..sub("subrip", "eng", "", false, false)
        };
        assert_eq!(subtitle_characteristics(&untitled), None);
    }

    #[test]
    fn duplicate_manual_renditions_do_not_violate_hls_autoselect_uniqueness() {
        let file = hls_file(vec![
            sub("subrip", "eng", "Regular", false, false),
            sub("webvtt", "eng", "Alternate", false, false),
        ]);
        let master = master_playlist(&file, None, &sdr_context());
        assert!(!master.contains("CODECS="));
        assert_eq!(master.matches("AUTOSELECT=NO").count(), 2);

        let selected = master_playlist(&file, Some(1), &sdr_context());
        assert!(selected
            .contains("NAME=\"English · Alternate\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES"));
    }

    #[test]
    fn bitmap_and_styled_subtitles_stay_out_of_native_renditions() {
        let file = hls_file(vec![
            sub("subrip", "eng", "Regular", false, false),
            sub("hdmv_pgs_subtitle", "eng", "PGS", false, false),
            sub("dvd_subtitle", "eng", "VobSub", false, false),
            sub("ass", "eng", "Styled Signs", false, false),
            sub("ssa", "eng", "Styled Dialogue", false, false),
            // The MP4 case, and the common one: every WEB-DL carries these.
            // `SubTrackDto.text` says true of them (they do have text to
            // extract), which is exactly why `native` exists — the master
            // must not carry a rendition this path cannot slice.
            sub("mov_text", "eng", "MP4 Timed Text", false, false),
        ]);
        let master = master_playlist(&file, None, &sdr_context());
        assert!(master.contains("subs/0/index.m3u8"));
        for index in 1..=5 {
            assert!(!master.contains(&format!("subs/{index}/index.m3u8")));
        }
    }

    #[test]
    fn subtitle_playlist_and_vtt_mirror_video_segments_at_resume_timeline() {
        let video = b"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:4.000000,\nseg00000.m4s\n#EXTINF:6.000000,\nseg00001.m4s\n";
        let playlist = subtitle_media_playlist(video);
        assert!(playlist.contains("#EXT-X-TARGETDURATION:6"));
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));
        assert!(playlist.contains("#EXTINF:4.000000,\nseg00000.vtt"));
        assert!(playlist.contains("#EXTINF:6.000000,\nseg00001.vtt"));
        assert!(!playlist.contains("#EXT-X-ENDLIST"));

        let sliding = subtitle_media_playlist(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:2\n#EXTINF:6.0,\nseg00002.m4s\n",
        );
        assert!(sliding.contains("#EXT-X-MEDIA-SEQUENCE:2"), "{sliding}");
        assert!(sliding.contains("seg00002.vtt"), "{sliding}");
        assert!(
            !sliding.contains("#EXT-X-PLAYLIST-TYPE:EVENT"),
            "the subtitle rendition mirrors the video's sliding shape: {sliding}"
        );

        let source = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\npast\n\ncue-id\n00:00:09.000 --> 00:00:11.000 align:start\ncrossing\n\n00:00:15.250 --> 00:00:16.500\nfuture\n";
        let first = String::from_utf8(slice_webvtt(source, 10.0, 0.0, 4.0)).expect("utf8");
        assert!(first.contains("X-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000"));
        assert!(!first.contains("past"));
        assert!(first.contains("00:00:00.000 --> 00:00:01.000 align:start"));
        assert!(!first.contains("future"));

        let second = String::from_utf8(slice_webvtt(source, 10.0, 4.0, 10.0)).expect("utf8");
        assert!(second.contains("X-TIMESTAMP-MAP=MPEGTS:360000,LOCAL:00:00:00.000"));
        assert!(!second.contains("crossing"));
        assert!(second.contains("00:00:01.250 --> 00:00:02.500"));

        let finished = subtitle_media_playlist(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4.0,\nseg00000.m4s\n#EXT-X-ENDLIST\n",
        );
        assert!(finished.ends_with("seg00000.vtt\n#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn forced_detection_reads_words_not_substrings() {
        // The contract case: disposition says nothing, the title says Forced.
        assert!(track_is_forced(&sub(
            "subrip", "ita", "Forced", false, false
        )));
        assert!(track_is_forced(&sub(
            "subrip",
            "eng",
            "English (Forced)",
            false,
            false
        )));
        assert!(track_is_forced(&sub(
            "subrip",
            "eng",
            "forced signs",
            false,
            false
        )));
        // Disposition alone is still enough.
        assert!(track_is_forced(&sub(
            "subrip", "eng", "Regular", false, true
        )));

        // The bug: a substring test hid these tracks from Apple's subtitle
        // menu entirely, because a forced rendition is only offered when the
        // presentation language matches.
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "Non-Forced",
            false,
            false
        )));
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "non forced",
            false,
            false
        )));
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "Not Forced",
            false,
            false
        )));
        assert!(!track_is_forced(&sub(
            "subrip", "eng", "Unforced", false, false
        )));
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "Reinforced Audio",
            false,
            false
        )));
    }

    #[test]
    fn language_tags_come_from_the_shared_alias_table() {
        // The ten this module used to know.
        assert_eq!(language_tag(Some("eng")), "en");
        assert_eq!(language_tag(Some("zho")), "zh");
        // And the ones it did not, which used to reach AVPlayer as
        // non-BCP-47 codes no viewer preference could ever match.
        assert_eq!(language_tag(Some("dut")), "nl");
        assert_eq!(language_tag(Some("cze")), "cs");
        assert_eq!(language_tag(Some("gre")), "el");
        assert_eq!(language_tag(Some("rum")), "ro");
        assert_eq!(language_name(Some("cze")), "Czech");
        // Unknown stays unknown rather than becoming a guess.
        assert_eq!(language_tag(Some("xyz")), "xyz");
        assert_eq!(language_tag(None), "und");
    }

    #[test]
    fn rendition_names_are_unique_within_the_group() {
        // Two untitled English tracks: RFC 8216 §4.3.4.1 makes NAME
        // MUST-unique, and the client resolves options by name — so a
        // duplicate is the client picking the wrong track, not a wart.
        let file = hls_file(vec![
            SubtitleStream {
                index: 0,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: None,
                default: false,
                forced: false,
                hearing_impaired: false,
            },
            SubtitleStream {
                index: 1,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: None,
                default: false,
                forced: false,
                hearing_impaired: false,
            },
        ]);
        let master = master_playlist_with(&file, None, &sdr_context(), MasterRungs::default());
        assert_eq!(master.matches("NAME=\"English\"").count(), 1, "{master}");
        assert!(master.contains("NAME=\"English (2)\""), "{master}");
    }

    #[test]
    fn rendition_attributes_survive_hostile_titles() {
        // A quoted-string attribute has no escape, so a quote, a comma or a
        // line break in a title is not a formatting problem — it is a
        // playlist that no longer parses.
        let file = hls_file(vec![sub(
            "subrip",
            "eng",
            "The \"Good\" One, v2\r\nsecond line",
            false,
            false,
        )]);
        let master = master_playlist_with(&file, None, &sdr_context(), MasterRungs::default());
        let line = master
            .lines()
            .find(|line| line.starts_with("#EXT-X-MEDIA:"))
            .expect("a rendition line");
        assert_eq!(
            line.matches('"').count() % 2,
            0,
            "unbalanced quotes: {line}"
        );
        assert!(!line.contains("\"Good\""), "{line}");
        assert!(line.contains("URI=\"subs/0/index.m3u8\""), "{line}");
        assert_eq!(
            master
                .lines()
                .filter(|l| l.starts_with("#EXT-X-MEDIA:"))
                .count(),
            1
        );
    }

    #[test]
    fn the_master_says_it_has_no_captions_and_its_one_rung_stays_inert() {
        let file = hls_file(vec![
            sub("subrip", "ita", "Forced", false, true),
            sub("subrip", "ita", "Forced Signs", false, true),
        ]);

        // Default: the shape that plays on the device today. Two forced
        // tracks share a language, so RFC 8216's uniqueness rule keeps them
        // manually selectable.
        let shipped = master_playlist_with(&file, None, &sdr_context(), MasterRungs::default());
        // No longer a rung. A variant with no caption track must say so, and
        // a phantom CEA-608 option in the text group shifts every rendition
        // ordinal beneath it — which is how a client asking for "the first
        // subtitle rendition" gets the second one.
        assert!(
            shipped.contains(
                "#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,\
                 RESOLUTION=3840x2160,FRAME-RATE=23.976,CLOSED-CAPTIONS=NONE,\
                 SUBTITLES=\"subs\""
            ),
            "{shipped}"
        );
        assert_eq!(shipped.matches("AUTOSELECT=NO").count(), 2, "{shipped}");
        assert!(!shipped.contains("CODECS="), "{shipped}");

        // The remaining rung, alone: it changes the renditions and leaves the
        // variant line's caption attribute exactly where it now always is.
        let forced = master_playlist_with(
            &file,
            None,
            &sdr_context(),
            MasterRungs {
                forced_autoselect: true,
            },
        );
        assert_eq!(
            forced.matches("CLOSED-CAPTIONS=NONE").count(),
            1,
            "{forced}"
        );
        assert_eq!(forced.matches("AUTOSELECT=YES").count(), 2, "{forced}");
    }

    #[test]
    fn a_cue_spanning_a_segment_boundary_keeps_its_authored_end() {
        // 6 s windows; the cue runs 5.0 → 8.0, straddling the boundary.
        let source = b"WEBVTT\n\nspan\n00:00:05.000 --> 00:00:08.000\ncrossing\n";
        let first = String::from_utf8(slice_webvtt(source, 0.0, 0.0, 6.0)).expect("utf8");
        let second = String::from_utf8(slice_webvtt(source, 0.0, 6.0, 12.0)).expect("utf8");

        // It appears in both windows it intersects...
        assert!(first.contains("crossing"), "{first}");
        assert!(second.contains("crossing"), "{second}");
        // ...and the copy in the first window runs to its AUTHORED end rather
        // than being cut off at the boundary. Clipping it there is what tore
        // a line of dialogue in half and flickered at every 6 s seam.
        assert!(first.contains("00:00:05.000 --> 00:00:08.000"), "{first}");
        // The trailing copy still starts at the window edge, because cue
        // times in this scheme are segment-local and WebVTT cannot spell a
        // negative one. Its identifier is what lets a player reconcile the
        // two.
        assert!(second.contains("00:00:00.000 --> 00:00:02.000"), "{second}");
        assert!(second.contains("span"), "{second}");
    }

    #[test]
    fn cue_shifting_is_relative_to_the_media_origin_not_the_request() {
        // The P0-2 shape, as the slicer sees it: a copy session asked to
        // start at 12.3 s whose media actually begins at the 10 s keyframe.
        // A cue authored at 14.0 s belongs 4.0 s into the session, not 1.7.
        let source = b"WEBVTT\n\n00:00:14.000 --> 00:00:16.000\nline\n";
        let correct = String::from_utf8(slice_webvtt(source, 10.0, 0.0, 6.0)).expect("utf8");
        assert!(
            correct.contains("00:00:04.000 --> 00:00:06.000"),
            "{correct}"
        );

        let by_request = String::from_utf8(slice_webvtt(source, 12.3, 0.0, 6.0)).expect("utf8");
        assert!(by_request.contains("00:00:01.700"), "{by_request}");
        assert!(
            !by_request.contains("00:00:04.000 -->"),
            "shifting by the request leads the picture by the seek's distance from its keyframe"
        );
    }

    /// The route group's shipped shape (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8,
    /// Decision D-M8-J): a state built by the production constructor holds
    /// the unfilled slot, so every point reads the no-op, a release runs
    /// through its three points to a durable End, and an admitted preparation
    /// candidate takes the priming path (see
    /// `shipped_shape_primes_an_admitted_candidate`).
    #[tokio::test]
    async fn hls_route_shipped_shape() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let root = crate::test_tempdir().expect("shipped state root");
        let state = crate::state::AppState::new_unhooked(
            "test".into(),
            Arc::clone(&fixture.store),
            crate::state::Dirs {
                artwork: root.path().join("artwork"),
                transcode: root.path().join("transcode"),
                cache: root.path().join("cache"),
                subs: root.path().join("subs"),
                runtime_cache: root.path().join("runtime"),
                renditions: root.path().join("renditions"),
            },
            "shipped-node".into(),
            plurx_core::transcode::EncoderCaps::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        let hooks = state.hls_route_hooks.get();
        let installed: &dyn std::any::Any = hooks;
        assert!(
            installed.is::<NoopHlsRouteHooks>(),
            "the production constructor leaves the route group on the no-op hooks"
        );
        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        for (point, mut hook) in [
            ("after_create_ask_recorded", hooks.after_create_ask_recorded()),
            ("before_dispatch_answer", hooks.before_dispatch_answer("p")),
            ("before_preparation_settlement", hooks.before_preparation_settlement("i")),
            ("before_preparation_planning", hooks.before_preparation_planning("p")),
            ("before_preparation_registered", hooks.before_preparation_registered("p")),
            ("after_release_fence_closed", hooks.after_release_fence_closed(&session_id)),
            ("after_release_tombstoned", hooks.after_release_tombstoned(&session_id)),
            ("after_link_source_validated", hooks.after_link_source_validated()),
            ("after_staged_link_route_read", hooks.after_staged_link_route_read()),
        ] {
            assert!(
                hook.as_mut().poll(&mut context).is_ready(),
                "the production {point} point is ready at its first poll"
            );
        }
        assert!(!hooks.staged_read_fault("i"));
        assert!(!hooks.preparation_settlement_fault("i"));
        assert!(!hooks.refuses_preparation_planning("p"));
        assert!(!hooks.release_commit_unknown(&session_id));
        assert!(
            hooks.primes_prepared_successor(),
            "production stages and primes every admitted successor"
        );

        let user = fixture
            .store
            .create_user("shipped-release", "hash", false)
            .await
            .expect("release user");
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "shipped-release".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: state.node_id.clone(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;
        let settlement = match state
            .media_sessions
            .begin_release_reconciliation(&session_id)
            .await
        {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
        };
        let released = tokio::time::timeout(
            Duration::from_secs(10),
            release_session(
                state.clone(),
                session_id.clone(),
                settlement,
                crate::vodserve::Terminal::Deleted,
                "released by client",
            ),
        )
        .await
        .expect("the production release points do not hold the release");
        assert_eq!(released, StatusCode::NO_CONTENT);
        assert!(
            fixture
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("pointer after release")
                .is_none(),
            "the release reached its durable End"
        );

        shipped_shape_primes_an_admitted_candidate().await;
    }

    /// The production priming arm of `process_preparation_candidate`, driven
    /// on a state built by the production constructor: an admitted candidate
    /// stages **and primes** its successor.
    ///
    /// Only the priming path reaches the registration point, so the test
    /// holds a pause there. Arming it installs the test hooks, which must keep
    /// answering priming as the no-op did. While the successor is held, the
    /// candidate is superseded, so after registration it tears itself down
    /// and no encoder is launched. A production that never primed stages the
    /// row only, never reaches the point, and fails at `reached`.
    async fn shipped_shape_primes_an_admitted_candidate() {
        let dir = crate::test_tempdir().expect("priming state dir");
        let playback_id = unique_playback_id("shipped-priming");
        let session_id = uuid::Uuid::new_v4().to_string();
        let (fixture, session_id, route) = staging_route_on(
            HlsDeliveryFixture::publish_unhooked(dir.path(), &session_id).await,
            session_id,
            &playback_id,
        )
        .await;
        let installed: &dyn std::any::Any = fixture.state.hls_route_hooks.get();
        assert!(
            installed.is::<NoopHlsRouteHooks>(),
            "the production constructor leaves the route group on the no-op hooks"
        );
        let registered = pause_before_preparation_registered(&fixture.state, &playback_id);
        let rearmed: &dyn std::any::Any = fixture.state.hls_route_hooks.get();
        assert!(
            rearmed.is::<HlsRouteTestHooks>(),
            "arming a point installs the test hooks"
        );
        assert!(
            fixture.state.hls_route_hooks.get().primes_prepared_successor(),
            "arming a point on a production-built state keeps its candidates priming"
        );

        // The admitted transition `an_admitted_preparation_candidate_reaches_the_ledger`
        // drives: a 2160p copy being delivered, and the viewer asks for 1080p.
        let source = staging_source(&fixture).await;
        let mut recipe = crate::transcode::SessionRequest {
            continuous_media: None,
quality_catalog: None,
            candidate_context: None,
            playback_id: playback_id.clone(),
            ..staged_candidate_request()
        };
        recipe.file_id = source.id;
        recipe.kind = crate::transcode::SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        let delivered =
            crate::playback_control::EffectiveSelection::from_request(&recipe, 2160, None);
        let selection = crate::playback_control::ClientSelection {
            quality: crate::playback_control::QualitySelection::Manual { height: 1080 },
            audio_track: None,
            subtitle: crate::playback_control::SubtitleSelection {
                mode: crate::playback_control::SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: crate::playback_control::CodecPolicy::Auto,
            dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
        };
        let digest = selection.desired().digest();
        let candidate = tokio::spawn(process_preparation_candidate(
            fixture.state.clone(),
            PendingCandidateGuard::begin(&playback_id, &digest),
            PreparationCandidateInputs {
                accepted_observation: None,
                session_id: session_id.clone(),
                route: route.clone(),
                recipe: RemoteStartRequest {
                    retained_output: None,
                    retained_output_receiver: None,
                    candidate_catalog: None,
                    candidate_id: None,
                    presentation_target: None,
                    decoder_caps: None,
                    protocol_version: crate::media_pool::PROTOCOL_VERSION,
                    incarnation_id: route.incarnation_id.clone(),
                    user_id: route.user_id,
                    source_size: 0,
                    source_mtime: 0,
                    typeless_playlist: false,
                    library_channel: None,
                    request: recipe,
                },
                planning_caps: None,
                planning_overrides: None,
                selection,
                observed_download_bps: Some(100_000_000),
                delivered,
                delivered_bps: Some(10_000_000),
                capabilities: Some(crate::playback_control::DynamicCapabilities {
                    presentation_target: None,
                    decoder_caps: None,
                    platform: crate::playback_control::ClientPlatform::Apple,
                    max_height: 2160,
                    codecs: vec![crate::playback_control::CodecPolicy::H264],
                    dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                    dual_player_preparation: true,
                }),
                platform: crate::playback_control::ClientPlatform::Apple,
                purpose: PreparationPurpose::SelectionChange,
                accepted_film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
            },
        ));
        let held = registered.reached().await;
        assert!(
            fixture
                .store
                .staged_media_session_for_playback(route.user_id, &playback_id)
                .await
                .expect("ledger read at the registration point")
                .is_none(),
            "the priming path reserves after it registers, so nothing is staged yet"
        );
        cancel_preparations_for_superseded_predecessor(&playback_id, None);
        held.release();
        tokio::time::timeout(Duration::from_secs(10), candidate)
            .await
            .expect("the superseded candidate finishes")
            .expect("candidate task");
        assert!(
            !has_active_preparation_for_ask(&playback_id, &digest),
            "the superseded successor is not left registered and priming"
        );

        // The test constructor's state is the other side: its candidates
        // stage without priming until a test turns priming on.
        let hooked = crate::state::AppState::new(
            "test".into(),
            Arc::clone(&fixture.store),
            crate::state::Dirs {
                artwork: dir.path().join("hooked-artwork"),
                transcode: dir.path().join("hooked-transcode"),
                cache: dir.path().join("hooked-cache"),
                subs: dir.path().join("hooked-subs"),
                runtime_cache: dir.path().join("hooked-runtime"),
                renditions: dir.path().join("hooked-renditions"),
            },
            "hooked-node".into(),
            plurx_core::transcode::EncoderCaps::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        assert!(!hooked.hls_route_hooks.get().primes_prepared_successor());
        prime_prepared_successors(&hooked, true);
        assert!(hooked.hls_route_hooks.get().primes_prepared_successor());
    }

    /// `before_dispatch_answer` sits after the dispatch and before the
    /// exchange reads the pending map: an exchange that dispatches nothing,
    /// held at the point while a candidate for its own ask appears, answers
    /// `staging` from the map it reads after the point.
    #[tokio::test]
    async fn dispatch_answer_point_precedes_the_pending_map_read() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-answer-point");
        let (fixture, _session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        let request = preparing_control_request(&route);
        let pause = pause_before_dispatch_answer(&fixture.state, &playback_id);
        let exchange = tokio::spawn({
            let fixture_state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move {
                control_local_inner(&fixture_state, &route, request, unix_ms() + 4_000).await
            }
        });
        let held = pause.reached().await;
        assert!(
            pending_candidate_for_playback(&playback_id).is_none(),
            "the opening ask dispatches nothing"
        );
        let planted = PendingCandidateGuard::begin(&playback_id, &request.selection.desired().digest());
        held.release();
        let (status, body) = control_body(
            tokio::time::timeout(Duration::from_secs(10), exchange)
                .await
                .expect("the exchange finishes once released")
                .expect("exchange task"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            preparation_state(&body),
            "staging",
            "the answer reads the pending map after the point"
        );
        drop(planted);
    }

    /// `before_preparation_settlement` sits below the settlement's retry
    /// deadline: a settlement delayed there past its whole retry budget
    /// expires without committing, as one behind a Store that slow would.
    #[tokio::test]
    async fn a_settlement_delayed_past_its_retry_budget_expires_uncommitted() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                prepared_proof: None,
                planning_registration: None,
                quality_intent: None,
                planning_cancellation: None,
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (status, prepare_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();
        tokio::time::sleep(Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            committed_media_origin_ms: prepare_body["action"]["media_origin_ms"].as_i64(),
            first_frame_unix_ms: Some(unix_ms()),
        });
        delay_next_preparation_settlement(
            &fixture.state,
            &route.incarnation_id,
            PREPARATION_SETTLEMENT_RETRY_BUDGET + Duration::from_millis(200),
        );
        let (failed_status, _) = control_body(
            control_local_inner(&fixture.state, &route, request, unix_ms().saturating_add(500))
                .await,
        )
        .await;
        assert_eq!(failed_status, StatusCode::SERVICE_UNAVAILABLE);
        tokio::time::sleep(PREPARATION_SETTLEMENT_RETRY_BUDGET + Duration::from_secs(1)).await;
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("pointer after the expired settlement")
                .expect("the predecessor stays current")
                .incarnation_id,
            route.incarnation_id,
            "the delay spent the retry budget, so the settlement expired uncommitted"
        );
    }

    /// `after_release_fence_closed` sits between the publication fence and the
    /// durable End: a release held there has closed the session's publication
    /// fence, and its durable route is still live.
    #[tokio::test]
    async fn release_fence_point_sits_between_the_fence_and_the_durable_end() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "release-fence-unrelated").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let _owner = install_vod_http_session(&fixture, dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("release-fence", "hash", false)
            .await
            .expect("release fence user");
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "release-fence".to_owned(),
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
        assert!(
            !fixture
                .state
                .transcode
                .session_publication_fenced_for_test(&session_id),
            "no release has fenced the session yet"
        );

        let pause = pause_release_after_fence(&fixture.state, &session_id);
        let deletion = tokio::spawn({
            let state = fixture.state.clone();
            let session_id = session_id.clone();
            async move { delete(State(state), AxPath(session_id)).await }
        });
        let held = pause.reached().await;
        assert!(
            fixture
                .state
                .transcode
                .session_publication_fenced_for_test(&session_id),
            "the publication fence is closed before the point"
        );
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("route read")
                .expect("the durable route")
                .state,
            "active",
            "the durable End runs after the point"
        );
        held.release();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), deletion)
                .await
                .expect("the release finishes once released")
                .expect("delete task"),
            StatusCode::NO_CONTENT
        );
    }

    /// `after_release_tombstoned` sits between the durable End's proof and the
    /// exact remote owner's terminal projection: a release held there has armed
    /// the ended row's projection and has not yet tried the owner, which is
    /// unreachable here, so the release is deferred only after the point.
    #[tokio::test]
    async fn release_tombstone_point_sits_between_the_durable_proof_and_the_remote_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture =
            HlsDeliveryFixture::publish(dir.path(), "release-tombstone-unrelated").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let user = fixture
            .store
            .create_user("release-tombstone", "hash", false)
            .await
            .expect("release tombstone user");
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "release-tombstone".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "remote-owner-node".to_owned(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;
        let pause = pause_release_after_tombstone(&fixture.state, &session_id);
        let settlement = match fixture
            .state
            .media_sessions
            .begin_release_reconciliation(&session_id)
            .await
        {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
        };
        let release = tokio::spawn({
            let state = fixture.state.clone();
            let session_id = session_id.clone();
            async move {
                release_session(
                    state,
                    session_id,
                    settlement,
                    crate::vodserve::Terminal::Deleted,
                    "released by client",
                )
                .await
            }
        });
        let held = pause.reached().await;
        let ended = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("route read")
            .expect("the ended row");
        assert_eq!(ended.state, "ended");
        assert_ne!(
            ended.publication_ready_at_ms, MEDIA_SESSION_PUBLICATION_BLOCKED,
            "the durable End's terminal projection is armed before the point"
        );
        assert!(
            !fixture
                .state
                .transcode
                .session_publication_fenced_for_test(&session_id),
            "the durable tombstone is projected locally before the point"
        );
        assert!(!release.is_finished(), "the release is held at the point");
        held.release();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(60), release)
                .await
                .expect("the release finishes once released")
                .expect("release task"),
            StatusCode::SERVICE_UNAVAILABLE,
            "the unreachable remote owner is tried after the point, and the release deferred"
        );
    }
    #[tokio::test]
    async fn a05_prepared_http_observation_is_optional_auth_not_capability_authority() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback = unique_playback_id("a05-optional-observation-auth");
        let (fixture, session, route) = staging_fixture_for_playback(dir.path(), &playback).await;
        let request = preparing_control_request(&route);
        let mut headers = HeaderMap::new();
        headers.insert("X-Plurx-Link-Receipt", uuid::Uuid::new_v4().to_string().parse().expect("A05 prepared fixture"));
        headers.insert("authorization", "Bearer invalid-observation-token".parse().expect("A05 prepared fixture"));
        let remote = Some("192.0.2.8:12345".parse().expect("A05 prepared fixture"));
        assert!(super::prepared_link::authenticate(&fixture.state, &headers, remote,
            std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.is_none());
        let response = super::control::control(State(fixture.state.clone()), AxPath(session),
            headers.clone(), crate::http::network::RemoteAddress(remote),
            Bytes::from(serde_json::to_vec(&request).expect("A05 prepared fixture"))).await;
        let (status, _) = control_body(response).await;
        assert_eq!(status, StatusCode::OK, "invalid observation credentials do not reject valid capability control");
        let token = "a05-prepared-observation-owned-token";
        fixture.store.create_token(&plurx_core::auth::hash_token(token), route.user_id, None).await.expect("A05 prepared fixture");
        headers.insert("authorization", format!("Bearer {token}").parse().expect("A05 prepared fixture"));
        assert!(super::prepared_link::authenticate(&fixture.state, &headers, remote,
            std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.is_some());
        assert!(super::prepared_link::authenticate(&fixture.state, &headers, remote,
            std::time::Instant::now()).await.is_none(), "expired original deadline is not renewed");
        headers.remove("X-Plurx-Link-Receipt");
        assert!(super::prepared_link::authenticate(&fixture.state, &headers, remote,
            std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.is_none());
    }
    #[tokio::test]
    async fn a05_prepared_auth_ignores_all_forwarding_headers_and_requires_ipv4_socket() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback = unique_playback_id("a05-peer-only-observation-auth");
        let (fixture, _, route) = staging_fixture_for_playback(dir.path(), &playback).await;
        let token = "a05-peer-only-observation-owned-token";
        fixture.store.create_token(&plurx_core::auth::hash_token(token), route.user_id, None)
            .await.expect("owned authentication token");
        let mut headers = HeaderMap::new();
        headers.insert("X-Plurx-Link-Receipt", uuid::Uuid::new_v4().to_string().parse().expect("nonce"));
        headers.insert("authorization", format!("Bearer {token}").parse().expect("authorization"));
        headers.insert("forwarded", "for=198.51.100.9".parse().expect("forwarded"));
        headers.insert("x-forwarded-for", "203.0.113.9".parse().expect("xff"));
        headers.insert("x-real-ip", "192.0.99.9".parse().expect("real ip"));
        let remote = Some("192.0.2.8:12345".parse().expect("IPv4 socket"));
        let observation = super::prepared_link::authenticate(&fixture.state, &headers, remote,
            std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.expect("actual IPv4 peer authority");
        assert_eq!(observation.network_fingerprint(), "192.0.2.0/24");
        for unsupported in [None, Some("[2001:db8::8]:12345".parse().expect("IPv6 socket"))] {
            assert!(super::prepared_link::authenticate(&fixture.state, &headers, unsupported,
                std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.is_none(),
                "forwarding headers cannot authorize a missing or unsupported socket");
        }
        headers.remove("forwarded");
        headers.remove("x-forwarded-for");
        assert!(super::prepared_link::authenticate(&fixture.state, &headers, None,
            std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.is_none(),
            "X-Real-IP alone cannot authorize an absent socket");
    }

    #[tokio::test]
    async fn a05_unknown_original_prepared_branch_requires_authenticated_incumbent_and_own_stage_body() {
        use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
        use super::link_receipts::{ClientLinkSample, SessionBinding};
        let dir = crate::test_tempdir().expect("source");
        let playback = unique_playback_id("a05-unknown-original");
        // An automatic session's evidence counts only behind a proved
        // incumbent, so the route is a real Auto incumbent: it serves the
        // fixture's actual source and selected this candidate, the one its
        // link receipts are bound to below.
        let incumbent_digest = [7;32];
        let incumbent = QualityCandidate {id:CandidateId::for_recipe_digest(incumbent_digest), recipe_digest:incumbent_digest,
            route:CandidateRoute::Encode, normalized_geometry:true, width:1920, height:1080, target_height:1080,
            average_bps:Some(8_000_000), peak_bps:Some(12_000_000), grade:plurx_core::transcode::OutputGrade::Sdr,
            decoder_compatible:true, complete_cache:false, sustainable:true};
        let session_id = uuid::Uuid::new_v4().to_string();
        let published = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let (fixture, session, route) =
            staging_route_with_incumbent_on(published, session_id, &playback, Some(incumbent.clone())).await;
        fixture.store.put_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS, "1").await.expect("setting");
        let control = preparing_control_request(&route);
        accepted_exchange(&fixture, &route, &control).await;
        let user = fixture.store.get_user(route.user_id).await.expect("user query").expect("user");
        let token = "a05-unknown-original-owned-token";
        fixture.store.create_token(&plurx_core::auth::hash_token(token), user.id, None).await.expect("token");
        let remote = Some("192.0.2.8:12345".parse().expect("peer"));
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().expect("auth"));
        headers.insert("user-agent", "Mozilla/5.0".parse().expect("UA"));
        let mut network = crate::http::network::identity(&headers, remote).expect("network");
        network.user_id = Some(user.id);
        network.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(user.id, user.created_at, &user.password_hash));
        let file = fixture.store.get_file(fixture.file_id()).await.expect("file query").expect("file");
        let source = super::link_receipts::binding(&network, &file, incumbent.recipe_digest, incumbent.route).await.expect("source identity");
        fixture.state.link_receipts.register(SessionBinding {source, session:session.clone(), incarnation:route.incarnation_id.clone(), owner_epoch:route.owner_epoch});
        let (nonce, eof) = fixture.state.link_receipts.mint(&session, "seg00001.m4s", "incumbent-etag", 4_000_000, Some(4000), true).expect("body");
        let sample = ClientLinkSample {receipt:nonce.clone(), object_name:"seg00001.m4s".into(), etag:"incumbent-etag".into(), body_bytes:4_000_000,
            body_duration_ms:1000, age_ms:0, network_load:Some(true), from_cache:Some(false), producer_paced:Some(false),
            cause:plurx_core::domain::NetworkPriorCause::Link, negative:false, media_duration_ms:Some(4000), presenting:true, stalled:false, runway_ms:12000};
        headers.insert("X-Plurx-Link-Receipt", nonce.parse().expect("nonce"));
        let http = super::prepared_link::authenticate(&fixture.state, &headers, remote,
            std::time::Instant::now() + crate::playback_control::EXCHANGE_DEADLINE).await.expect("ordinary auth");
        let observation = super::prepared_link::capture(&fixture.state, &route, &control,
            crate::playback_control::ControlDisposition::Accepted, Some(http)).await.expect("accepted actor");
        let digest = [12;32];
        let candidate = QualityCandidate {id:CandidateId::for_recipe_digest(digest), recipe_digest:digest, route:CandidateRoute::Remux,
            normalized_geometry:true, width:3840, height:2160, target_height:2160, average_bps:Some(90_000_000), peak_bps:None,
            grade:plurx_core::transcode::OutputGrade::Sdr, decoder_compatible:true, complete_cache:false, sustainable:true};
        let mut request = serde_json::from_str::<RemoteStartRequest>(&route.recipe_json).expect("recipe").request;
        request.candidate_context = Some(Box::new(crate::transcode::CandidateExecutionContext {
            planning_snapshot:None,
            retained_output:None, canonical_caps:None, selected_candidate:candidate.clone(), planning_binding:None,
            owner_node_id:Some(fixture.state.node_id.clone()),
            candidate_id:candidate.id, recipe_digest:digest, normalized_geometry:true, grade:candidate.grade, profile:None}));
        assert!(observation.proposed_proof(&fixture.state, &file, &mut request, &candidate, super::link_receipts::advisory_deadline()).await.is_none(), "header without EOF cannot start a trial");
        eof(std::time::Instant::now(), unix_ms());
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&session), &sample).await.is_some());
        let proof = observation.proposed_proof(&fixture.state, &file, &mut request, &candidate, super::link_receipts::advisory_deadline()).await.expect("authenticated unknown-cost copy trial");
        assert!(request.candidate_context.as_ref().expect("context").retained_output.is_none(), "trial never invents retained full output");
        let mut known = candidate.clone(); known.peak_bps = Some(90_000_000);
        assert!(observation.proposed_proof(&fixture.state, &file, &mut request, &known, super::link_receipts::advisory_deadline()).await.is_none(), "known cost cannot become unknown trial");
        let staged = uuid::Uuid::new_v4().to_string();
        let incarnation = uuid::Uuid::new_v4().to_string();
        let deadline = unix_ms()+15000;
        assert!(observation.gate.stage_preparation_for_owner(incarnation.clone(), control.generation.clone(), deadline,
            i64::try_from(control.control_epoch).expect("epoch"), Some(control.selection.desired().digest())).await);
        let activation = plurx_core::domain::MediaSessionActivation {incarnation_id:incarnation.clone(), session_id:staged.clone(), user_id:user.id,
            playback_id:playback, recovery_epoch:String::new(), expected_predecessor_incarnation_id:None, fence_predecessor:false,
            request_id:None, request_fingerprint:"a".repeat(64), owner_node_id:fixture.state.node_id.clone(), recipe_json:"{}".into(), response_json:"{}".into(),
            publication_ready_at_ms:plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED, media_origin_ms:0, now_ms:unix_ms(), lease_expires_at_ms:deadline, expected_desired_revision:None};
        assert!(fixture.store.activate_media_session(&activation).await.expect("stage activation").is_some());
        proof.register(&fixture.state, &staged, &incarnation, deadline, tokio_util::sync::CancellationToken::new()).await;
        let (own_nonce, own_eof) = fixture.state.link_receipts.mint(&staged, "seg00002.m4s", "own-etag", 4096, Some(2000), true).expect("registered trial body");
        let own = ClientLinkSample {receipt:own_nonce, object_name:"seg00002.m4s".into(), etag:"own-etag".into(), body_bytes:4096, media_duration_ms:Some(2000), ..sample};
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &own).await.is_none());
        own_eof(std::time::Instant::now(), unix_ms());
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &own).await.is_some(), "trial acquires its own authenticated body");
        let (late_nonce, late_eof) = fixture.state.link_receipts.mint(&staged, "seg00003.m4s", "late-etag", 4096, Some(2000), true).expect("late body");
        late_eof(std::time::Instant::now(), unix_ms());
        let late = ClientLinkSample {receipt:late_nonce, object_name:"seg00003.m4s".into(), etag:"late-etag".into(), ..own};
        assert!(observation.gate.begin_abort_preparation_for_owner(&incarnation, i64::try_from(control.control_epoch).expect("epoch")).await);
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &late).await.is_none(), "aborted trial cannot claim a late completed body");
    }

    #[tokio::test]
    async fn a05_staged_intake_uses_own_eof_and_refuses_aborted_actor_without_claim() {
        a05_staged_intake_fixture(0).await;
    }

    #[tokio::test]
    async fn a05_staged_intake_timeout_is_unknown_and_preserves_raw_claim_retry() {
        a05_staged_intake_fixture(1).await;
    }

    #[tokio::test]
    async fn a05_staged_intake_abort_during_final_route_await_invalidates_token_without_claim() {
        a05_staged_intake_fixture(2).await;
    }

    #[tokio::test]
    async fn a05_committed_preparation_drops_staged_proof_and_binds_as_ordinary() {
        a05_staged_intake_fixture(3).await;
    }

    #[tokio::test]
    async fn a05_aborted_preparation_retires_staged_proof_and_binding() {
        a05_staged_intake_fixture(4).await;
    }

    async fn a05_staged_intake_fixture(mode: u8) {
        use plurx_core::domain::{MediaSessionActivation, NetworkPriorCause, CredentialGeneration};
        use super::link_receipts::ClientLinkSample;
        use plurx_core::playback::candidate::CandidateRoute;
        let dir = crate::test_tempdir().expect("A05 prepared fixture");
        let playback = unique_playback_id("a05-staged-intake");
        let (fixture, session, route) = staging_fixture_for_playback(dir.path(), &playback).await;
        let request = preparing_control_request(&route);
        accepted_exchange(&fixture, &route, &request).await;
        let gate = fixture.state.transcode.session_preparation_gate(&session).await.expect("A05 prepared fixture");
        let desired = request.selection.desired().digest();
        let fence = gate.accepted_observation(crate::playback_control::AcceptedControlIdentity {
            generation: request.generation.clone(), owner_epoch: request.control_epoch,
            client_instance_id: request.client_instance_id.clone(), sequence: request.sequence,
            fingerprint: request.fingerprint().expect("A05 prepared fixture"), desired_digest: desired.clone(),
        }).await.expect("A05 prepared fixture");
        let staged = uuid::Uuid::new_v4().to_string();
        let incarnation = uuid::Uuid::new_v4().to_string();
        let now = unix_ms();
        let deadline = now + 60_000;
        assert!(gate.stage_preparation_for_owner(incarnation.clone(), request.generation.clone(),
            deadline, i64::try_from(request.control_epoch).expect("A05 prepared fixture"), Some(desired)).await);
        let activation = MediaSessionActivation {
            incarnation_id: incarnation.clone(), session_id: staged.clone(), user_id: route.user_id,
            playback_id: playback.clone(), recovery_epoch: String::new(), expected_predecessor_incarnation_id: None,
            fence_predecessor: false, request_id: None, request_fingerprint: "a".repeat(64),
            owner_node_id: fixture.state.node_id.clone(), recipe_json: "{}".into(), response_json: "{}".into(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0, now_ms: now, lease_expires_at_ms: deadline, expected_desired_revision: None,
        };
        assert!(fixture.store.activate_media_session(&activation).await.expect("A05 prepared fixture").is_some());
        let user = fixture.store.get_user(route.user_id).await.expect("A05 prepared fixture").expect("A05 prepared fixture");
        let network = crate::telemetry::NetworkIdentity {
            user_id: Some(user.id), credential_generation: Some(CredentialGeneration::derive(user.id, user.created_at, &user.password_hash)),
            client_class: "web".into(), network_fingerprint: "192.0.2.0/24".into(),
        };
        let file = fixture.store.get_file(fixture.file_id()).await.expect("A05 prepared fixture").expect("A05 prepared fixture");
        let source = super::link_receipts::binding(&network, &file, [7;32], CandidateRoute::Encode).await.expect("A05 prepared fixture");
        let binding = super::link_receipts::SessionBinding { source, session: staged.clone(), incarnation: incarnation.clone(), owner_epoch: 1 };
        fixture.state.link_receipts.register_staged(binding, super::prepared_link::StagedProof {
            stage: gate.staged_observation_is_current(fence.clone(), incarnation.clone(), deadline).await.expect("A05 prepared fixture"),
            gate: Arc::clone(&gate), fence, incarnation: incarnation.clone(), owner_epoch: 1,
            deadline_unix_ms: deadline, cancelled: tokio_util::sync::CancellationToken::new(),
            trial_deadline: None,
        });
        let (nonce, eof) = fixture.state.link_receipts.mint(&staged, "seg00001.m4s", "etag", 4096, Some(4000), true).expect("A05 prepared fixture");
        let mut sample = ClientLinkSample { receipt: nonce, object_name: "seg00001.m4s".into(), etag: "etag".into(),
            body_bytes:4096, body_duration_ms:1000, age_ms:0, network_load:Some(true), from_cache:Some(false),
            producer_paced:Some(false), cause:NetworkPriorCause::Link, negative:false, media_duration_ms:Some(4000),
            presenting:false, stalled:false, runway_ms:0 };
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_none(), "header without own EOF is not proof");
        eof(std::time::Instant::now(), now);
        struct StalledObservation;
        impl crate::playback_control::PreparationGate for StalledObservation {
            fn stage_preparation_for_owner<'a>(&'a self, _: String, _: String, _: i64, _: i64, _: Option<String>) -> crate::playback_control::GateAnswer<'a> { Box::pin(async { false }) }
            fn may_commit_preparation_for_owner<'a>(&'a self, _: &'a str, _: i64) -> crate::playback_control::GateAnswer<'a> { Box::pin(async { false }) }
            fn begin_abort_preparation_for_owner<'a>(&'a self, _: &'a str, _: i64) -> crate::playback_control::GateAnswer<'a> { Box::pin(async { false }) }
            fn reject_preparation_commit_for_owner<'a>(&'a self, _: &'a str, _: i64) -> crate::playback_control::GateAnswer<'a> { Box::pin(async { false }) }
            fn settle_preparation_for_owner<'a>(&'a self, _: &'a str, _: bool, _: i64) -> crate::playback_control::GateAnswer<'a> { Box::pin(async { false }) }
            fn observation_is_current<'a>(&'a self, _: crate::playback_control::AcceptedControlFence)
                -> crate::playback_control::GateAnswer<'a> {
                Box::pin(std::future::pending())
            }
        }
        if mode == 3 {
            fixture.store.put_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS, "1").await.expect("setting");
            fixture.store.settle_media_session_activation(&activation,
                plurx_core::domain::MediaSessionActivationSettlement::Confirm { publication_ready_at_ms: 0 }, now)
                .await.expect("commit").expect("published successor");
            let committed = fixture.store.media_session_route(&staged).await.expect("route query").expect("committed route");
            assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_none(),
                "a staged row outliving its commit refuses the committed route");
            fixture.state.link_receipts.settle_committed(&committed, &fixture.state.node_id);
            assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_some(),
                "the committed successor accepts its completed body as an ordinary binding");
            assert!(fixture.state.link_receipts.current_positive_until(&fixture.state, &network, &file, Some(&sample.receipt),
                Some(&playback), Some(&fixture.state.node_id), super::link_receipts::advisory_deadline()).await.is_some(),
                "the committed successor proves like any registered session");
            assert!(fixture.state.link_receipts.mint(&staged, "seg00002.m4s", "etag2", 4096, Some(4000), true).is_some(),
                "the committed successor keeps minting after its stage ends");
            return;
        }
        if mode == 4 {
            fixture.state.link_receipts.retire_staged(&staged, &incarnation);
            assert!(fixture.state.link_receipts.mint(&staged, "seg00002.m4s", "etag2", 4096, Some(4000), true).is_none(),
                "a retired stage mints nothing");
            assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_none(),
                "a retired stage accepts nothing");
            return;
        }
        if mode == 2 {
            let pause = super::pause_staged_link_route_result(&fixture.state);
            let intake = fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample);
            tokio::pin!(intake);
            let held = tokio::select! {
                held = pause.reached() => held,
                _ = &mut intake => panic!("intake must suspend inside final route result await"),
            };
            assert!(gate.begin_abort_preparation_for_owner(&incarnation, i64::try_from(request.control_epoch).expect("A05 prepared fixture")).await);
            held.release();
            assert!(intake.await.is_none(), "same desired digest cannot rescue an aborted stage token");
            assert!(!fixture.state.link_receipts.raw_claimed_for_test(&sample.receipt), "refusal cannot consume raw claim");
            return;
        }
        if mode == 1 {
            fixture.state.link_receipts.replace_staged_gate_for_test(&staged, Arc::new(StalledObservation));
            let query_started = std::time::Instant::now();
            assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_none(),
                "unresponsive optional observational query is bounded Unknown");
            assert!(query_started.elapsed() < Duration::from_millis(500));
            fixture.state.link_receipts.replace_staged_gate_for_test(&staged, Arc::clone(&gate));
        }
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_some(), "own completed body is observable before commit");
        assert!(fixture.state.link_receipts.current_positive_until(&fixture.state, &network, &file, Some(&sample.receipt), Some(&playback), Some(&fixture.state.node_id), super::link_receipts::advisory_deadline()).await.is_none(), "staged proof cannot lend incumbent admission");
        let (second, complete) = fixture.state.link_receipts.mint(&staged, "seg00002.m4s", "etag2", 4096, Some(4000), true).expect("A05 prepared fixture");
        complete(std::time::Instant::now(), now);
        sample.receipt = second; sample.object_name = "seg00002.m4s".into(); sample.etag = "etag2".into();
        assert!(gate.begin_abort_preparation_for_owner(&incarnation, i64::try_from(request.control_epoch).expect("A05 prepared fixture")).await);
        assert!(fixture.state.link_receipts.accept(&fixture.state, &network, Some(&staged), &sample).await.is_none(), "aborted actor refuses its still-completed body");
    }

    #[test]
    fn planning_binding_detects_same_timestamp_probe_and_generation_changes() {
        let mut snapshot = plurx_core::store::PlaybackPlanningSnapshot {
            file: staged_source_file(), probe_json: Some("{\"streams\":[]}".to_owned()),
            settings: Default::default(), generation: 1,
        };
        let original = crate::media_pool::PlanningBinding::from_snapshot(&snapshot);
        snapshot.probe_json = Some("{\"streams\":[{\"codec_name\":\"hevc\"}]}".to_owned());
        assert_ne!(original, crate::media_pool::PlanningBinding::from_snapshot(&snapshot));
        snapshot.probe_json = Some("{\"streams\":[]}".to_owned());
        snapshot.generation += 1;
        assert_ne!(original, crate::media_pool::PlanningBinding::from_snapshot(&snapshot));
        assert_eq!(snapshot.file.mtime, staged_source_file().mtime);
    }

    #[tokio::test]
    async fn expired_catalog_budget_refuses_before_validation_or_source_work() {
        let state = resolver_state();
        let request = crate::media_pool::QualityCatalogRequest {
            audio_claim: None, audio_delivery: None,
            file_id: -1, source_size: -1, source_mtime: -1,
            caps: Default::default(), copy_contract: None, audio_index: None,
            audio_offset_ms: 0, subtitle_burn: None,
            presentation: crate::transcode::Presentation::Vod,
        };
        let outcome = crate::media_pool::local_quality_catalog(
            &state, &request, tokio::time::Instant::now(), None,
        ).await;
        assert!(!outcome.complete);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.causes, vec![crate::media_pool::CatalogCause::LocalDeadline]);
        let invalid = crate::media_pool::local_quality_catalog(
            &state, &request, tokio::time::Instant::now() + Duration::from_secs(1), None,
        ).await;
        assert!(matches!(invalid.causes.as_slice(), [crate::media_pool::CatalogCause::RequestInvalid(_)]));
    }

    #[tokio::test]
    async fn required_legacy_auto_discovery_reports_overflow_but_copy_and_manual_survive() {
        let state = resolver_state();
        state.store.put_setting(plurx_core::store::keys::PLAYBACK_DISPLAY_AWARE_AUTO, "1").await.expect("enable saved choice");
        let source = staged_source_file();
        let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
            "v": 2, "video": (0..65).map(|_| serde_json::json!({"codec":"h264", "present":["sdr"]})).collect::<Vec<_>>(),
            "audio":["aac"], "containers":["mp4"], "transports":["hls"]
        })).expect("legacy caps");
        for (auto, copy, height, expected) in [
            (true, false, None, StatusCode::BAD_REQUEST),
            (false, false, Some(1440), StatusCode::BAD_REQUEST),
            (false, false, Some(720), StatusCode::OK),
            (true, true, None, StatusCode::OK),
        ] {
            let body = CreateSession { playback_id: "legacy-catalog-contract".into(), caps: Some(caps.clone()), quality_auto: Some(auto), copy: Some(copy), height, ..bare_create() };
            let result = resolve_plan(PlanInputs { snapshot: None, state: &state, user_id: 1, file_id: source.id, source: Some(&source), network_prior: None, network_identity: None, incumbent_receipt: None }, None, body).await;
            assert_eq!(result.map(|_| StatusCode::OK).unwrap_or_else(|error| error.into_response().status()), expected);
        }
    }

    #[tokio::test]
    async fn local_worker_preserves_request_accounting_and_remaining_budget() {
        let state = resolver_state();
        let counts = plurx_core::store::HttpStoreOperationCounts::default();
        let budget = crate::media_pool::CreateStartupBudget::new(2_000);
        let parent_deadline = crate::media_pool::create_stage_deadline(Duration::from_secs(10));
        plurx_core::store::scope_http_store_operations(counts.clone(), budget.scope(async {
            let store = Arc::clone(&state.store);
            crate::media_pool::spawn_create_worker(async move {
                store.watch_state(1, 1).await.expect("worker read");
                assert!(crate::media_pool::create_stage_deadline(Duration::from_secs(50)) < parent_deadline);
            }).await.expect("owned worker");
            let store = Arc::clone(&state.store);
            tokio::spawn(async move { store.watch_state(1, 1).await.expect("unrelated background read"); }).await.expect("background task");
        })).await;
        assert_eq!(counts.watch_reads(), 1, "only the owned critical-path read belongs to the request");
    }

    #[tokio::test]
    async fn manual_and_copy_preparation_retain_a_request_local_catalog() {
        let state = resolver_state();
        let source = staged_source_file();
        let route = crate::media_sessions::takeover_eligible_route("old", "00000000-0000-4000-8000-0000000000a1");
        let mut predecessor = staged_predecessor_recipe(&route);
        let caps = caps_v2(r#"{"v":2,"video":[{"codec":"h264","present":["sdr"],"max_height":2160}],"audio":["eac3"],"containers":["mkv","mp4"],"transports":["hls"]}"#);
        predecessor.decoder_caps = Some(crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1).expect("decoder evidence"));
        for quality in [crate::playback_control::QualitySelection::Original, crate::playback_control::QualitySelection::Manual { height: 720 }] {
            let selection = crate::playback_control::ClientSelection { quality, audio_track: None,
                subtitle: crate::playback_control::SubtitleSelection { mode: crate::playback_control::SubtitleMode::Off, track: None },
                audio_offset_ms: 0, codec: crate::playback_control::CodecPolicy::Auto, dynamic_range: crate::playback_control::DynamicRangePolicy::Auto };
            let request = plan_preparation_candidate(&state, &predecessor, Some(&caps), None, &selection, &source, 1080).await.expect("ordinary successor plan");
            assert!(request.candidate_context.is_none());
            let catalog = request.quality_catalog.as_ref().expect("optional canonical ladder is retained independently");
            assert!(!catalog.complete, "missing source is incomplete, not proof of an empty ladder");
        }
    }
