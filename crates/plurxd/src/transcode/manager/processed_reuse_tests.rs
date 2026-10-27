use super::*;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
use plurx_core::playback::audio::AudioClaim;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};

use plurx_core::transcode::dv_processing::*;
use std::sync::atomic::AtomicBool;

// Test-only, one-shot pauses keyed by the unique successor id. No production
// polling, global policy or mutable behavior is installed in release builds.
#[derive(Default)]
struct PreparationPause {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

type PauseMap = std::collections::HashMap<(String, &'static str), Arc<PreparationPause>>;
static PAUSES: std::sync::OnceLock<std::sync::Mutex<PauseMap>> = std::sync::OnceLock::new();

impl TranscodeManager {
    pub(crate) async fn processed_preparation_test_boundary(session: &str, boundary: &'static str) {
        let pause = PAUSES
            .get_or_init(Default::default)
            .lock()
            .expect("preparation pauses")
            .remove(&(session.to_owned(), boundary));
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }
}

fn pause_at(session: &str, boundary: &'static str) -> Arc<PreparationPause> {
    let pause = Arc::new(PreparationPause::default());
    assert!(PAUSES
        .get_or_init(Default::default)
        .lock()
        .expect("preparation pauses")
        .insert((session.to_owned(), boundary), Arc::clone(&pause))
        .is_none());
    pause
}

async fn prepare(f: &Fixture, session: &str, json: &str) -> bool {
    f.manager
        .vod_prepare_first_before(
            json,
            session,
            f.user_id,
            PreparedVodOwner {
                node_id: "node".into(),
                source_authority: None,
            },
            f.manager.session_adoption_token(session).expect("adoption"),
            Some(1),
            Instant::now() + Duration::from_secs(10),
        )
        .await
}

fn spawn_prepare(f: &Fixture, session: &str, json: &str) -> tokio::task::JoinHandle<bool> {
    let manager = Arc::clone(&f.manager);
    let session = session.to_owned();
    let json = json.to_owned();
    let user_id = f.user_id;
    tokio::spawn(async move {
        manager
            .vod_prepare_first_before(
                &json,
                &session,
                user_id,
                PreparedVodOwner {
                    node_id: "node".into(),
                    source_authority: None,
                },
                manager.session_adoption_token(&session).expect("adoption"),
                Some(1),
                Instant::now() + Duration::from_secs(10),
            )
            .await
    })
}

async fn wait_for_cleanup(
    f: &Fixture,
    session: &str,
    owner: &crate::vodserve::ResponseOwner,
    completed: &PreparationPause,
) {
    tokio::time::timeout(Duration::from_secs(5), completed.entered.notified())
        .await
        .expect("successor-only cleanup");
    completed.release.notify_one();
    assert!(!f.manager.vod.response_owner_is_live(session, owner).await);
    f.manager
        .vod
        .test_assert_prepared_owner_detached(session, owner)
        .await;
    assert!(
        f.manager
            .vod
            .response_owner_is_live(&f.session_id, &f.incumbent_owner)
            .await
    );
    assert_eq!(f.encoding.admissions.in_use(), 1);
    assert_eq!(f.encoding.admissions.software_in_use(), 15);
}

#[tokio::test]
async fn staged_processed_cancellation_at_attachment_facts_and_seal_releases_only_successor() {
    for boundary in ["attached", "facts", "before-seal", "sealed"] {
        let mut f = fixture(DvDestination::Hdr10).await;
        let (_, session, json) = stage(&f).await;
        let pause = pause_at(&session, boundary);
        let work = spawn_prepare(&f, &session, &json);
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .expect(boundary);
        let owner = f
            .manager
            .vod
            .hls_facts(&session)
            .await
            .expect("attached")
            .response_owner;
        let completed = pause_at(&session, "cleanup-complete");
        work.abort();
        assert!(work.await.expect_err("cancelled").is_cancelled());
        wait_for_cleanup(&f, &session, &owner, &completed).await;
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
        assert_reservation_released(&mut f).await;
    }
}

#[tokio::test]
async fn staged_processed_epoch_motion_after_attachment_refuses_frozen_seal() {
    let mut f = fixture(DvDestination::Profile81).await;
    let (_, session, json) = stage(&f).await;
    let pause = pause_at(&session, "before-seal");
    let work = spawn_prepare(&f, &session, &json);
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .expect("at seal");
    let owner = f
        .manager
        .vod
        .hls_facts(&session)
        .await
        .expect("attached")
        .response_owner;
    let now = crate::media_sessions::unix_ms() + 120_000;
    let adopted = f
        .manager
        .store
        .claim_media_session_takeover(&plurx_core::domain::MediaSessionTakeover {
            incarnation_id: f.remote.incarnation_id.clone(),
            expected_owner_node_id: "node".into(),
            expected_owner_epoch: 1,
            next_owner_node_id: "survivor".into(),
            now_ms: now,
            lease_expires_at_ms: now + 60_000,
        })
        .await
        .expect("takeover")
        .expect("new epoch");
    assert_eq!(adopted.owner_epoch, 2);
    let completed = pause_at(&session, "cleanup-complete");
    pause.release.notify_one();
    assert!(
        !work.await.expect("preparation"),
        "must not refresh the admitted epoch"
    );
    wait_for_cleanup(&f, &session, &owner, &completed).await;
    f.manager
        .vod
        .end(&f.session_id, crate::vodserve::Terminal::Deleted)
        .await;
    assert_reservation_released(&mut f).await;
}

#[tokio::test]
async fn staged_processed_replacement_never_inherits_preparation_or_cleanup_authority() {
    for boundary in ["attached", "replay-owner"] {
        let f = fixture(DvDestination::Hdr10).await;
        let (incarnation, session, json) = stage(&f).await;
        if boundary == "replay-owner" {
            // Keep a real first attachment pending, as if the first caller
            // stopped after attachment but before its durable seal.
            assert!(
                f.manager
                    .resurrect_vod_from_recipe_before(
                        &json,
                        &session,
                        f.user_id,
                        f.manager
                            .session_adoption_token(&session)
                            .expect("adoption"),
                        Instant::now() + Duration::from_secs(5),
                        VodRecipeAttachment::FirstPreparation {
                            owner: PreparedVodOwner {
                                node_id: "node".into(),
                                source_authority: None
                            },
                            bootstrap: false,
                            processed_predecessor: Some(expectation(&f, &incarnation, &session)),
                            attachment_owner: Arc::new(std::sync::Mutex::new(None)),
                        },
                    )
                    .await
            );
        }
        let pause = pause_at(&session, boundary);
        let work = spawn_prepare(&f, &session, &json);
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .expect(boundary);
        let previous = f
            .manager
            .vod
            .hls_facts(&session)
            .await
            .expect("first owner")
            .response_owner;
        let replacement_incarnation = uuid::Uuid::new_v4().to_string();
        let mut replacement_recipe: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&json).expect("wire");
        replacement_recipe.incarnation_id = replacement_incarnation.clone();
        replacement_recipe.request.request_id = Some(replacement_incarnation.clone());
        let replacement_json =
            serde_json::to_string(&replacement_recipe).expect("replacement wire");
        assert!(
            f.manager
                .resurrect_vod_from_recipe_before(
                    &replacement_json,
                    &session,
                    f.user_id,
                    f.manager
                        .session_adoption_token(&session)
                        .expect("replacement adoption"),
                    Instant::now() + Duration::from_secs(5),
                    VodRecipeAttachment::FirstPreparation {
                        owner: PreparedVodOwner {
                            node_id: "node".into(),
                            source_authority: None
                        },
                        bootstrap: false,
                        processed_predecessor: Some(expectation(
                            &f,
                            &replacement_incarnation,
                            &session
                        )),
                        attachment_owner: Arc::new(std::sync::Mutex::new(None)),
                    },
                )
                .await,
            "real common attachment installs replacement"
        );
        let replacement = f
            .manager
            .vod
            .hls_facts(&session)
            .await
            .expect("replacement")
            .response_owner;
        let replacement_state = f.manager.vod.test_prepared_reader_state(&session).await;
        assert!(
            !f.manager
                .vod
                .response_owner_is_live(&session, &previous)
                .await
        );
        let completed = pause_at(&session, "cleanup-complete");
        pause.release.notify_one();
        assert!(!work.await.expect("preparation"), "{boundary}");
        wait_for_cleanup(&f, &session, &previous, &completed).await;
        assert!(
            f.manager
                .vod
                .response_owner_is_live(&session, &replacement)
                .await,
            "cleanup cannot end a same-id replacement at {boundary}"
        );
        assert!(
            !f.manager
                .vod
                .prepared_owner_matches(&session, &replacement, &incarnation)
                .await,
            "mark cannot change a same-id replacement at {boundary}"
        );
        assert_eq!(
            f.manager.vod.test_prepared_reader_state(&session).await,
            replacement_state
        );
        let route = f
            .manager
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .expect("route lookup")
            .expect("pending route");
        let response: serde_json::Value =
            serde_json::from_str(&route.response_json).expect("response");
        assert_eq!(response["prepared_output_capture_pending"], true);
        assert_ne!(response["prepared_output_capture_complete"], true);
        f.manager
            .vod
            .end(&session, crate::vodserve::Terminal::Deleted)
            .await;
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
    }
}

#[tokio::test]
async fn staged_processed_audio_canonicalization_reuses_default_and_alternate_aac_exactly() {
    use plurx_core::playback::audio::AudioAction;
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        for (selected, offset) in [(None, 0), (Some(1), 125)] {
            let mut f = fixture_with_audio(destination, Some((selected, offset))).await;
            let expected_index = selected.unwrap_or(0);
            assert_eq!(f.remote.request.audio_index, Some(expected_index));
            assert_eq!(f.remote.request.audio_offset_ms, offset);
            assert_eq!(f.encoding.options.audio_index, Some(expected_index));
            let expected_audio = f.encoding.options.audio.as_ref().expect("encoded AAC");
            assert!(
                matches!(&expected_audio.action, AudioAction::Encode { codec, sample_rate: 48_000, .. } if codec == "aac")
            );
            let context = f
                .remote
                .request
                .candidate_context
                .as_ref()
                .expect("private context");
            let original = context
                .catalog_query
                .as_ref()
                .expect("original copy provenance");
            assert_eq!(
                original.copy_contract,
                Some(if destination == DvDestination::Profile81 {
                    (false, true, true)
                } else {
                    (false, false, false)
                })
            );
            if selected.is_none() {
                assert!(
                    matches!(&original.audio_delivery.as_ref().expect("original audio").action, AudioAction::Copy { codec, .. } if codec == "aac")
                );
                assert_ne!(
                    original
                        .audio_delivery
                        .as_ref()
                        .expect("original audio")
                        .byte_identity(),
                    expected_audio.byte_identity()
                );
            }
            let incumbent = f
                .manager
                .vod
                .hls_facts(&f.session_id)
                .await
                .expect("incumbent")
                .response_owner;
            let (incarnation, session, json) = stage(&f).await;
            let remote: crate::media_sessions::RemoteStartRequest =
                serde_json::from_str(&json).expect("wire");
            assert!(remote.request.candidate_context.is_none());
            for mutation in ["track", "offset", "claim", "bytes"] {
                let mut changed = remote.clone();
                match mutation {
                    "track" => changed.request.audio_index = Some(1 - expected_index),
                    "offset" => changed.request.audio_offset_ms += 1,
                    "claim" => {
                        changed.request.audio_claim.as_mut().expect("claim").sinks[0].max_channels =
                            1
                    }
                    "bytes" => {
                        let AudioAction::Encode { bitrate_kbps, .. } = &mut changed
                            .request
                            .audio_delivery
                            .as_mut()
                            .expect("audio")
                            .action
                        else {
                            panic!("canonical AAC");
                        };
                        *bitrate_kbps += 32;
                    }
                    _ => unreachable!(),
                }
                assert!(
                    f.manager
                        .restore_staged_processed_root(
                            &mut changed,
                            &expectation(&f, &incarnation, &session),
                            Instant::now() + Duration::from_secs(5),
                        )
                        .await
                        .is_err(),
                    "{destination:?}/{selected:?}/{mutation}"
                );
                assert!(f.manager.vod.hls_facts(&session).await.is_none());
                assert!(
                    f.manager
                        .vod
                        .response_owner_is_live(&f.session_id, &incumbent)
                        .await
                );
            }
            assert!(
                prepare(&f, &session, &json).await,
                "canonical audio survives serialized first prime"
            );
            let facts = f
                .manager
                .vod
                .hls_facts(&session)
                .await
                .expect("attached successor");
            let actual = facts.encoding.as_ref().expect("reused encoding");
            assert!(Arc::ptr_eq(actual, &f.encoding));
            assert_eq!(actual.options.audio_index, Some(expected_index));
            assert_eq!(
                actual
                    .options
                    .audio
                    .as_ref()
                    .expect("audio")
                    .byte_identity(),
                expected_audio.byte_identity()
            );
            assert_eq!(f.encoding.admissions.in_use(), 1);
            assert_eq!(f.encoding.admissions.software_in_use(), 15);
            let route = f
                .manager
                .store
                .media_session_route_by_incarnation(&incarnation)
                .await
                .expect("route")
                .expect("sealed");
            let response: serde_json::Value =
                serde_json::from_str(&route.response_json).expect("response");
            assert_eq!(response["prepared_output_capture_complete"], true);
            let sealed: crate::media_sessions::RemoteStartRequest =
                serde_json::from_str(&route.recipe_json).expect("sealed recipe");
            assert_eq!(sealed.request.audio_index, Some(expected_index));
            assert_eq!(sealed.request.audio_offset_ms, offset);
            f.manager
                .vod
                .end(&session, crate::vodserve::Terminal::Deleted)
                .await;
            f.manager
                .vod
                .end(&f.session_id, crate::vodserve::Terminal::Deleted)
                .await;
            drop(facts);
            assert_reservation_released(&mut f).await;
        }
    }
}

// Byte-bound lifecycle controls, not Dolby pixel/reference-accuracy evidence.
// The real completed-window validator checks the generated HEVC/PQ stream;
// controlled driver scheduling keeps its real exclusive worker outside the
// slot without provoking a duplicate launch. This does not test driver/reap.
struct Fixture {
    _driver: Box<dyn Send + Sync>,
    _root: tempfile::TempDir,
    incumbent_owner: crate::vodserve::ResponseOwner,
    manager: Arc<TranscodeManager>,
    remote: crate::media_sessions::RemoteStartRequest,
    encoding: Arc<crate::vodencode::Encoding>,
    worker: Option<crate::vodencode::EncodeWorkerPermit>,
    receipt: DvProcessingReceipt,
    payloads: Vec<DvDigest>,
    user_id: i64,
    session_id: String,
}

fn expectation(f: &Fixture, incarnation: &str, session: &str) -> StagedProcessedPredecessor {
    StagedProcessedPredecessor {
        session_id: f.session_id.clone(),
        incarnation_id: f.remote.incarnation_id.clone(),
        owner_node_id: "node".into(),
        owner_epoch: 1,
        successor_session_id: session.to_owned(),
        successor_incarnation_id: incarnation.to_owned(),
    }
}

#[tokio::test]
async fn staged_processed_root_refuses_current_input_and_owner_drift_without_spending_capacity() {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let f = fixture(destination).await;
        let (incarnation, session, json) = stage(&f).await;
        let expected = expectation(&f, &incarnation, &session);
        let original: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&json).expect("wire");
        let mut positive = original.clone();
        drop(
            f.manager
                .restore_staged_processed_root(
                    &mut positive,
                    &expected,
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .expect("exact positive control"),
        );
        for mutation in [
            "height",
            "destination",
            "caps",
            "audio-offset",
            "audio-delivery",
            "audio-claim",
            "audio-track",
            "source-size",
            "file",
            "missing-destination",
            "previous-session",
            "geometry",
            "codec",
            "digest",
            "playback",
            "principal",
            "request-id",
        ] {
            let mut changed = original.clone();
            match mutation {
                "height" => changed.request.kind = SessionKind::Transcode { height: 32 },
                "destination" => {
                    changed.request.dv_processing_destination =
                        Some(if destination == DvDestination::Hdr10 {
                            DvDestination::Profile81
                        } else {
                            DvDestination::Hdr10
                        })
                }
                "caps" => {
                    changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .caps
                        .video[0]
                        .max_height = Some(32)
                }
                "audio-offset" => changed.request.audio_offset_ms = 1,
                "audio-delivery" => changed.request.audio_delivery = None,
                "audio-claim" => changed.request.audio_claim = None,
                "audio-track" => changed.request.audio_index = Some(987),
                "source-size" => changed.source_size += 1,
                "file" => changed.request.file_id += 1,
                "missing-destination" => changed.request.dv_processing_destination = None,
                "previous-session" => {
                    changed.request.previous_session_id = Some(f.session_id.clone())
                }
                "geometry" => {
                    changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .candidate
                        .width += 2
                }
                "codec" => {
                    changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .candidate
                        .planned_codec = Some("h264".into())
                }
                "digest" => {
                    let candidate = &mut changed
                        .candidate_catalog
                        .as_mut()
                        .expect("catalog")
                        .candidate;
                    candidate.recipe_digest = [92; 32];
                    candidate.id = CandidateId::for_recipe_digest(candidate.recipe_digest);
                    changed.candidate_id = Some(candidate.id);
                }
                "playback" => changed.request.playback_id = "another-play".into(),
                "principal" => {
                    changed.principal =
                        plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                            user_id: f.user_id + 1,
                        }
                }
                "request-id" => changed.request.request_id = Some(uuid::Uuid::new_v4().to_string()),
                _ => unreachable!(),
            }
            assert!(
                f.manager
                    .restore_staged_processed_root(
                        &mut changed,
                        &expected,
                        Instant::now() + Duration::from_secs(5)
                    )
                    .await
                    .is_err(),
                "{mutation}"
            );
            assert!(
                f.manager.vod.hls_facts(&session).await.is_none(),
                "refusal installed no successor"
            );
            assert_eq!(f.encoding.admissions.in_use(), 1);
            assert_eq!(f.encoding.admissions.software_in_use(), 15);
        }
        for (node, epoch) in [("node", 2), ("remote", 1)] {
            let mut changed = original.clone();
            let mut wrong = expectation(&f, &incarnation, &session);
            wrong.owner_node_id = node.into();
            wrong.owner_epoch = epoch;
            assert!(f
                .manager
                .restore_staged_processed_root(
                    &mut changed,
                    &wrong,
                    Instant::now() + Duration::from_secs(5)
                )
                .await
                .is_err());
        }
        assert!(
            f.manager.vod.hls_facts(&f.session_id).await.is_some(),
            "incumbent remains usable"
        );
        f.manager
            .store
            .put_setting(keys::SW_POOL_THREADS, "14")
            .await
            .expect("planning drift");
        let mut changed = original.clone();
        assert!(
            f.manager
                .restore_staged_processed_root(
                    &mut changed,
                    &expected,
                    Instant::now() + Duration::from_secs(5)
                )
                .await
                .is_err(),
            "a new planning generation cannot borrow the old graph"
        );
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
    }
}

#[tokio::test]
async fn first_processed_attachment_arms_exact_owner_cleanup_before_returning() {
    let mut f = fixture(DvDestination::Hdr10).await;
    let (incarnation, session, json) = stage(&f).await;
    let cleanup = FirstPreparationCleanup {
        vod: Arc::clone(&f.manager.vod),
        session_id: session.clone(),
        owner: Arc::new(std::sync::Mutex::new(None)),
    };
    assert!(
        f.manager
            .resurrect_vod_from_recipe_before(
                &json,
                &session,
                f.user_id,
                f.manager
                    .session_adoption_token(&session)
                    .expect("adoption"),
                Instant::now() + Duration::from_secs(5),
                VodRecipeAttachment::FirstPreparation {
                    owner: PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    bootstrap: false,
                    processed_predecessor: Some(expectation(&f, &incarnation, &session)),
                    attachment_owner: Arc::clone(&cleanup.owner)
                }
            )
            .await
    );
    let owner = cleanup
        .owner
        .lock()
        .expect("sink")
        .clone()
        .expect("armed by synchronous registry commit");
    let completed = pause_at(&session, "cleanup-complete");
    drop(cleanup);
    wait_for_cleanup(&f, &session, &owner, &completed).await;
    f.manager
        .vod
        .end(&f.session_id, crate::vodserve::Terminal::Deleted)
        .await;
    assert_reservation_released(&mut f).await;
}

async fn assert_reservation_released(f: &mut Fixture) {
    // Called only after the normal production owner cleanup has settled.
    // Exiting the controlled tasks can never resume a renderer generation.
    f.manager
        .vod
        .finish_controlled_driver_for_test(&f.encoding)
        .await;
    assert_eq!(
        f.encoding.admissions.in_use(),
        1,
        "held worker alone retains GPU credit"
    );
    assert_eq!(f.encoding.admissions.software_in_use(), 15);
    drop(f.worker.take());
    assert_eq!(f.encoding.admissions.in_use(), 0);
    assert_eq!(f.encoding.admissions.software_in_use(), 0);
    let fresh = f
        .encoding
        .admissions
        .try_admit_bundle(1, 15, &f.encoding.resources, Priority::Live)
        .expect("fresh mixed reservation after complete settlement");
    drop(fresh);
    assert_eq!(f.encoding.admissions.in_use(), 0);
    assert_eq!(f.encoding.admissions.software_in_use(), 0);
}

fn digest(bytes: &[u8]) -> DvDigest {
    DvDigest::new(hex::encode(Sha256::digest(bytes))).expect("bounded digest")
}

async fn fixture(destination: DvDestination) -> Fixture {
    fixture_with_audio(destination, None).await
}

#[tokio::test]
async fn processed_profile81_master_declares_hdr10_compatibility() {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let f = fixture(destination).await;
        let HlsPresentationResolution::Ready(context, file, _) = f
            .manager
            .hls_presentation_before(&f.session_id, Instant::now() + Duration::from_secs(10))
            .await
        else {
            panic!("admitted processed session must have an HLS presentation");
        };
        // Exercise the real admitted Encoding and publication path, then the
        // shared master writer; a hardcoded supplemental string misses the bug.
        let master = String::from_utf8(crate::http::hls::source_native_master(
            &file,
            None,
            &context,
            &Default::default(),
        ))
        .expect("HLS text");
        assert!(master.contains("VIDEO-RANGE=PQ"), "{master}");
        if destination == DvDestination::Profile81 {
            assert!(context.codecs.starts_with("hev1"), "{master}");
            assert!(master.contains("#EXT-X-VERSION:10\n"), "{master}");
            assert!(
                master.contains("SUPPLEMENTAL-CODECS=\"dvhe.08.06/db1p\""),
                "{master}"
            );
        } else {
            assert!(context.supplemental_codecs.is_none(), "{master}");
            assert!(!master.contains("SUPPLEMENTAL-CODECS="), "{master}");
        }
    }
}

async fn fixture_with_audio(
    destination: DvDestination,
    audio: Option<(Option<i64>, i64)>,
) -> Fixture {
    use std::os::unix::fs::MetadataExt;
    let root = crate::test_tempdir().expect("processed lifecycle");
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    let user = store
        .create_user("processed", "hash", false)
        .await
        .expect("user");
    let library = store
        .create_library(&NewLibrary {
            name: "processed".into(),
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
            title: "lifecycle control".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = root.path().join("source.mp4");
    std::fs::write(
        &path,
        include_bytes!("../../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4"),
    )
    .expect("source bytes");
    // The authored source starts at 48048/24000. Remux its actual pictures
    // onto a zero-origin film clock, retaining its 24000/1001 cadence rather
    // than pretending the source uses one coarse 1/24-second container tick.
    let video = root.path().join("source-video.mp4");
    std::fs::rename(&path, &video).expect("retain authored video");
    let mut mux = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    mux.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&video);
    if audio.is_some() {
        mux.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.125",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=0.125",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-disposition:a:0",
            "default",
            "-disposition:a:1",
            "0",
        ]);
    } else {
        mux.args(["-map", "0:v:0", "-an"]);
    }
    let mux = mux
        .args(["-c:v", "copy", "-video_track_timescale", "24000"])
        .arg(&path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("mux real fixture source");
    assert!(
        mux.status.success(),
        "{}",
        String::from_utf8_lossy(&mux.stderr)
    );
    let source_probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-show_packets",
            "-of",
            "json",
        ])
        .arg(&path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("probe real source clock and tracks");
    assert!(
        source_probe.status.success(),
        "{}",
        String::from_utf8_lossy(&source_probe.stderr)
    );
    let actual: serde_json::Value =
        serde_json::from_slice(&source_probe.stdout).expect("source probe");
    std::fs::write(root.path().join("source-probe.json"), &source_probe.stdout)
        .expect("retain actual source clock and tracks");
    let video = actual["streams"]
        .as_array()
        .expect("streams")
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .expect("actual source video");
    let rational = |name: &str| {
        let (numerator, denominator) = video[name]
            .as_str()
            .expect("measured source rational")
            .split_once('/')
            .expect("rational separator");
        (
            numerator.parse::<u32>().expect("numerator"),
            denominator.parse::<u32>().expect("denominator"),
        )
    };
    let source_tick = rational("time_base");
    let source_clock = crate::transcode::source_preparation::held_source_clock(&actual.to_string())
        .expect("observed held FORMAT clock");
    let source_origin = crate::transcode::source_preparation::held_dv_source_origin(
        video,
        source_tick,
        source_clock,
    )
    .expect("observed common origin");
    let (rate_num, rate_den) = rational("avg_frame_rate");
    let grid = plurx_core::transcode::VodFrameGrid::new(rate_num, rate_den).expect("measured grid");
    assert_eq!(source_tick, (1, 24000));
    assert_eq!((grid.numerator, grid.denominator), (24000, 1001));
    let keys: Vec<_> = actual["packets"]
        .as_array()
        .expect("actual source packets")
        .iter()
        .filter(|packet| packet["stream_index"] == video["index"])
        .enumerate()
        .map(|(ordinal, packet)| {
            let pts = packet["pts"].as_i64().expect("actual source PTS");
            assert_eq!(packet["dts"].as_i64(), Some(pts), "no reordered source");
            assert_eq!(packet["duration"].as_u64(), Some(u64::from(rate_den)));
            DvFrameKey {
                absolute_video_index: video["index"].as_u64().expect("video index") as u32,
                continuity_epoch: 0,
                pts: DvTimestamp::new(pts, source_tick.1).expect("source clock"),
                display_ordinal: ordinal as u64,
            }
        })
        .collect();
    assert_eq!(keys.len(), 3, "three real source pictures");
    assert_eq!(
        keys[0].pts,
        DvTimestamp::new(0, rate_num).expect("zero origin")
    );
    let metadata = std::fs::metadata(&path).expect("metadata");
    let mut probe = serde_json::json!({"format":{"duration":"2.0"},"streams":[{
        "index":0,"codec_type":"video","codec_name":"hevc","profile":"Main 10",
        "width":64,"height":64,"sample_aspect_ratio":"1:1","pix_fmt":"yuv420p10le",
        "field_order":"progressive",
        "color_transfer":"smpte2084","color_primaries":"bt2020","color_space":"bt2020nc",
        "side_data_list":[{"side_data_type":"DOVI configuration record","dv_profile":7,
            "dv_level":6,"rpu_present_flag":1,"el_present_flag":1,"bl_present_flag":1,
            "dv_bl_signal_compatibility_id":1}]
    }]});
    for field in [
        "avg_frame_rate",
        "r_frame_rate",
        "time_base",
        "start_pts",
        "start_time",
    ] {
        probe["streams"][0][field] = video[field].clone();
    }
    if audio.is_some() {
        let tracks: Vec<_> = actual["streams"]
            .as_array()
            .expect("streams")
            .iter()
            .filter(|stream| stream["codec_type"] == "audio")
            .cloned()
            .collect();
        assert_eq!(tracks.len(), 2);
        probe["streams"]
            .as_array_mut()
            .expect("streams")
            .extend(tracks);
    }
    let file_id = store
        .upsert_file(
            item,
            path.to_str().expect("path"),
            metadata.len() as i64,
            metadata.mtime(),
            &plurx_core::scan::probe::parse_probe_json(&probe),
        )
        .await
        .expect("file");
    for (key, value) in [
        (keys::DV_HDR_PROCESSING, "1"),
        (keys::DV_FEL_REENCODE, "1"),
        (keys::DV_CONVERT, "1"),
        (keys::SW_POOL_THREADS, "15"),
        (keys::MAX_HW_SESSIONS, "1"),
    ] {
        store.put_setting(key, value).await.expect("policy");
    }
    let manager = Arc::new(
        TranscodeManager::new(
            store.clone(),
            root.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_cache(
            root.path().join("cache"),
            crate::ffmpeg::ffmpeg_build().await,
            "node".into(),
        ),
    );
    manager.test_hooks();
    let backend = DvBackendIdentity::new(
        digest(b"lifecycle renderer"),
        digest(b"lifecycle libraries"),
        digest(b"lifecycle parser"),
        digest(b"lifecycle graph"),
        1,
        digest(b"lifecycle environment"),
    )
    .expect("backend control");
    if destination == DvDestination::Sdr {
        store
            .put_setting(keys::DV_DISPLAY_TARGET, "1")
            .await
            .expect("SDR display-target policy enabled before the planning snapshot");
        let source_file = store
            .get_file(file_id)
            .await
            .expect("file")
            .expect("source");
        // This lifecycle control never qualifies the installed scanner. Its
        // scoped executable emits the aggregate for the three known control
        // RPUs, while the real admitted title job proves source/claim/publish
        // custody. Physical scanner coverage stays in the ignored bundle test.
        use dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock;
        use plurx_core::scan::dv_title::{
            DvTitleStats, MasteringLuminance, MasteringPrimaries, TitleConstant, SCHEMA,
        };
        use std::os::unix::fs::PermissionsExt;
        let summarize = |raw: &[u8]| {
            let rpu = dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(raw)
                .expect("known control RPU");
            assert!(
                !rpu.header.use_prev_vdr_rpu_flag,
                "each source RPU is fresh"
            );
            let dm = rpu.vdr_dm_data.as_ref().expect("fresh control metadata");
            let Some(ExtMetadataBlock::Level1(l1)) = dm.get_block(1) else {
                panic!("control RPU needs L1");
            };
            assert!(l1.min_pq <= l1.avg_pq && l1.avg_pq <= l1.max_pq && l1.max_pq <= 4095);
            DvTitleStats {
                schema: SCHEMA,
                complete: true,
                decoded_frames: 1,
                valid_l1_frames: 1,
                missing_l1_frames: 0,
                invalid_l1_frames: 0,
                max_l1_pq: Some(l1.max_pq),
                mastering_luminance: match dm.get_block(6) {
                    Some(ExtMetadataBlock::Level6(l6)) => {
                        TitleConstant::Consistent(MasteringLuminance {
                            min_1e4_nits: u32::from(l6.min_display_mastering_luminance),
                            max_nits: u32::from(l6.max_display_mastering_luminance),
                        })
                    }
                    _ => TitleConstant::Unknown,
                },
                mastering_primaries: match dm.get_block(9) {
                    Some(ExtMetadataBlock::Level9(l9)) => {
                        TitleConstant::Consistent(MasteringPrimaries {
                            index: l9.source_primary_index,
                            custom_xy: (l9.source_primary_index == 255).then_some([
                                l9.source_primary_red_x,
                                l9.source_primary_red_y,
                                l9.source_primary_green_x,
                                l9.source_primary_green_y,
                                l9.source_primary_blue_x,
                                l9.source_primary_blue_y,
                                l9.source_primary_white_x,
                                l9.source_primary_white_y,
                            ]),
                        })
                    }
                    _ => TitleConstant::Unknown,
                },
            }
        };
        let raw_rpus = vec![
            include_bytes!("../../../../plurx-core/tests/fixtures/dv-runtime/frame-000.nal")
                .to_vec(),
            include_bytes!("../../../../plurx-core/tests/fixtures/dv-runtime/frame-001.nal")
                .to_vec(),
            include_bytes!("../../../../plurx-core/tests/fixtures/dv-runtime/frame-002.nal")
                .to_vec(),
        ];
        // The existing packet validator ties every bundled source RPU to
        // the actual authored pictures before remuxing their clock above.
        dv_validate_encoded_window(
            include_bytes!("../../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4"),
            (64, 64),
            24000,
            48048,
            1001,
            &raw_rpus,
            DvDestination::Profile81,
        )
        .expect("all three authored pictures carry these source RPUs");
        assert_eq!(raw_rpus.len(), keys.len());
        let frames: Vec<_> = raw_rpus.iter().map(|raw| summarize(raw)).collect();
        let mut stats = frames[0];
        stats.decoded_frames = frames.len() as u64;
        stats.valid_l1_frames = frames.len() as u64;
        stats.max_l1_pq = frames.iter().filter_map(|frame| frame.max_l1_pq).max();
        for frame in &frames {
            assert_eq!(
                frame.mastering_luminance, stats.mastering_luminance,
                "all authored L6 declarations must agree for this consistent aggregate"
            );
            assert_eq!(
                frame.mastering_primaries, stats.mastering_primaries,
                "all authored L9 declarations must agree for this consistent aggregate"
            );
        }
        let scanner = root.path().join("controlled-title-scanner");
        std::fs::write(
            &scanner,
            format!(
                r#"#!/bin/sh
set -eu
test "$2" = 0
test "$(wc -c < "$1")" -eq {}
test "$(sha256sum "$1" | cut -d ' ' -f 1)" = '{}'
printf '%s\n' '{}'
"#,
                source_file.size,
                hex::encode(Sha256::digest(
                    std::fs::read(&path).expect("held fixture bytes")
                )),
                serde_json::to_string(&stats).expect("controlled aggregate")
            ),
        )
        .expect("scoped scanner");
        std::fs::set_permissions(&scanner, std::fs::Permissions::from_mode(0o700))
            .expect("executable control");
        let tools = crate::vodencode::dv_runtime::ToolSet::lifecycle_fixture_with_scanner(
            backend.clone(),
            scanner,
        )
        .expect("artifact-hashed controlled scanner");
        let title_stats = crate::source_probe::publish_title_with_tools_for_control(
            &manager,
            &store,
            &source_file,
            Arc::new(tools),
        )
        .await
        .expect("real fenced job with controlled scanner outcome");
        assert_eq!(title_stats, stats);
        assert_eq!(
            title_stats.eligibility(),
            plurx_core::scan::dv_title::TitleEligibility::Eligible
        );
        assert_eq!(title_stats.decoded_frames, keys.len() as u64);
    }
    let snapshot = store
        .playback_planning_snapshot(file_id, &QUALITY_PLANNING_KEYS)
        .await
        .expect("planning")
        .expect("file");
    let file = &snapshot.file;
    assert_eq!(file.dolby_vision.profile, Some(7));
    let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
        "v":2,"video":[{"codec":"hevc","profiles":["main10"],"dv_profiles":[8],
            "present":if destination == DvDestination::Sdr { vec!["sdr"] } else { vec!["hdr10", "pq"] },
            "max_width":64,"max_height":64}],
        "display":(destination == DvDestination::Sdr).then(|| serde_json::json!({"hdr":false,"dolby_vision":false})),
        "containers":["mp4"],"transports":["hls"],"hls_hevc_sample_entries":["hvc1"],"dv_transport":"hls",
        "audio":["aac"],"audio_sinks":[{"codec":"aac","max_channels":2,"sample_rates_hz":[48000]}]
    })).expect("caps");
    let (display_target, display_target_note) = TranscodeManager::display_target_policy(
        destination == DvDestination::Sdr,
        caps.display.as_ref(),
        snapshot.probe_json.as_deref(),
    )
    .expect("frozen display-target policy");
    if destination == DvDestination::Sdr {
        let target = display_target.expect("eligible complete title selects SDR");
        assert!(target.is_sdr());
        assert_eq!(target.nits(), 100);
        target
            .resolved_sdr_floor()
            .expect("SDR floor frozen before admission");
    }
    let mut query = crate::media_pool::QualityCatalogRequest {
        file_id,
        source_size: file.size,
        source_mtime: file.mtime,
        caps: caps.clone(),
        audio_index: audio.and_then(|(index, _)| index),
        audio_offset_ms: audio.map_or(0, |(_, offset)| offset),
        audio_claim: AudioClaim::from_caps(&caps).expect("claim"),
        audio_delivery: None,
        subtitle_burn: None,
        presentation: Presentation::Vod,
        copy_contract: Some(if destination == DvDestination::Profile81 {
            (false, true, true)
        } else {
            (false, false, false)
        }),
    };
    if audio.is_some() {
        let selected =
            TranscodeManager::candidate_audio_from_snapshot(&snapshot, query.audio_index);
        let track = file
            .audio_streams
            .iter()
            .find(|stream| Some(stream.index) == selected);
        query.audio_delivery = Some(plurx_core::playback::audio::resolve_audio(
            track,
            &query.audio_claim.as_ref().expect("claim").profile(),
            plurx_core::playback::audio::AudioRoute::Progressive,
            query.audio_offset_ms,
        ));
    }
    let projected = TranscodeManager::processed_catalog_query(&snapshot, &query)
        .expect("canonical encoded audio");
    let mut resolved_file = file.clone();
    resolved_file.audio_offset_ms = projected.audio_offset_ms;
    let file = &resolved_file;
    let options = TranscodeOptions {
        target_height: 64,
        pipeline: Pipeline::Hdr10Passthrough,
        video_bitrate_kbps: 300,
        audio: projected.audio_delivery.clone(),
        audio_index: projected.audio_index,
        ..Default::default()
    };
    let facts =
        TranscodeManager::quality_facts_from_snapshot(&snapshot).expect("fresh decode facts");
    let plan = manager
        .resolve_movie_plan_with_facts(
            file,
            &options,
            Encoder::Software,
            &facts,
            &AttemptRestrictions::none(),
        )
        .expect("PQ encoder plan");
    let raster = (
        plan.output_contract().effective_width().expect("width"),
        plan.output_contract().effective_height().expect("height"),
    );
    let output = root.path().join("completed.mp4");
    let mut control = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    control
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=black:size=64x64:rate={rate_num}/{rate_den}"));
    if audio.is_some() {
        control.args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            // Put synthetic AAC encoder priming before its zero-origin
            // packet, so the mux does not shift the video by 1024/48000.
            "-af",
            "asetpts=PTS+1024",
            "-c:a",
            "aac",
            "-t",
            "0.126",
        ]);
    } else {
        control.arg("-an");
    }
    let (frame_colors, encoder_colors, primaries, transfer, matrix) = if destination
        == DvDestination::Sdr
    {
        (
            "setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709",
            "pools=none:frame-threads=1:bframes=0:keyint=3:colorprim=1:transfer=1:colormatrix=1",
            "bt709",
            "bt709",
            "bt709",
        )
    } else {
        (
            "setparams=range=limited:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
            "pools=none:frame-threads=1:bframes=0:keyint=3:colorprim=9:transfer=16:colormatrix=9",
            "bt2020",
            "smpte2084",
            "bt2020nc",
        )
    };
    // This black control is encoded directly in its destination color space.
    // It tests the delivery contract, not the renderer's HDR-to-SDR pixel mapping.
    let status = control
        .args([
            "-frames:v",
            "3",
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p10le",
            // The synthetic source has unspecified frame color properties.
            // Set them on frames too: encoder VUI flags alone leave the MP4
            // nclx primaries/transfer unspecified on supported FFmpeg builds.
            "-vf",
            frame_colors,
            "-preset",
            "ultrafast",
            "-x265-params",
            encoder_colors,
            "-color_primaries",
            primaries,
            "-color_trc",
            transfer,
            "-colorspace",
            matrix,
            "-video_track_timescale",
            "24000",
            "-movflags",
            // PQ tags in the SPS do not imply a container color record.
            // The completed-window validator independently requires nclx.
            "+frag_keyframe+empty_moov+default_base_moof+write_colr",
        ])
        .arg(&output);
    std::fs::write(
        root.path().join("completed-command.txt"),
        format!("{status:?}\n"),
    )
    .expect("retain exact control argv");
    let status = status
        .kill_on_drop(true)
        .output()
        .await
        .expect("encode control window");
    std::fs::write(root.path().join("completed-stderr.txt"), &status.stderr)
        .expect("retain encoder diagnostics");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let mut raw = dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(include_bytes!(
        "../../../../plurx-core/tests/fixtures/dv-runtime/frame-000.nal"
    ))
    .expect("authored control RPU");
    let dm = raw.vdr_dm_data.as_mut().expect("metadata");
    dm.remove_metadata_level(10);
    dm.remove_metadata_level(255);
    for curve in &mut raw.rpu_data_mapping.as_mut().expect("mapping").curves {
        let polynomial = curve.polynomial.as_mut().expect("affine control");
        polynomial.poly_coef_int[0][0] = 0;
        polynomial.poly_coef_int[0][1] = 1;
        polynomial.poly_coef[0][0] = 0;
        polynomial.poly_coef[0][1] = 0;
    }
    raw.modified = true;
    let rpus = vec![raw.write_hevc_unspec62_nalu().expect("bounded control RPU"); 3];

    let bytes = std::fs::read(&output).expect("completed bytes");
    let bytes = if destination == DvDestination::Profile81 {
        match dv_author_profile81_window(&bytes, raster, rate_num, 0, rate_den, &rpus, 6) {
            Ok(bytes) => bytes,
            Err(error) => {
                let retained = root.keep();
                panic!("actual reconstructed P81 authoring: {error:?}; retained {retained:?}");
            }
        }
    } else if destination == DvDestination::Sdr {
        dv_author_sdr_window(&bytes).expect("strip HDR-only metadata from actual SDR control")
    } else {
        bytes
    };
    std::fs::write(root.path().join("completed-validated-input.mp4"), &bytes)
        .expect("retain exact validation input");
    let source = Arc::new(
        crate::fragment_index_cluster::open_source_fence(file, None)
            .await
            .expect("held source"),
    );
    let source_facts = DvSourceFacts::new(
        file.dolby_vision,
        0,
        plurx_core::transcode::DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(
            std::fs::read(&path).expect("source hash"),
        )))
        .expect("source identity"),
        plurx_core::transcode::DecodeCacheIdentity::from_media_file(file),
        plurx_core::transcode::PlanSourceBinding::DescriptorBound,
        backend.parser_identity().clone(),
        DvRpuState::ValidatedFresh,
        DvElKind::Fel,
        Some(DvSourceCoverage::sampled(0, keys.clone()).expect("coverage")),
        Some(DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7BaseYuv420P10Limited).expect("BL")),
        Some(DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7ElYuv420P10Residual).expect("EL")),
    )
    .expect("source facts");
    let preferences = TranscodeManager::vod_dv_preferences_from_snapshot(&snapshot);
    let selected_plan = match dv_plan_completed_window(DvCompletedWindowInput {
        source: source_facts,
        preferences,
        backend: backend.clone(),
        destination,
        encoder: Encoder::Software,
        mode: DvProcessingMode::Fel,
        source_keys: &keys,
        source_rpus: &rpus,
        source_tick,
        source_origin,
        grid,
        first_global_frame: 0,
        encoded: &bytes,
        raster,
        export_policy: display_target
            .map_or(DvExportPolicy::PqBilinearYuv420p10V1 { raster }, |target| {
                DvExportPolicy::DisplayTargetYuv420p10V1 { raster, target }
            }),
        source_trace: digest(b"lifecycle trace"),
        observed_at: 1,
        decoder: plurx_core::transcode::DecodeBackend::Software,
    }) {
        Ok(plan) => plan,
        Err(error) => {
            let retained = root.keep();
            panic!("real completed-window validation: {error:?}; retained {retained:?}");
        }
    };
    let receipt = dv_receipt_completed_window(
        &selected_plan,
        &keys,
        &[DvDuration::new(
            i64::from(rate_den),
            rate_num,
            DvDurationProvenance::CodedPacketObserved,
        )
        .expect("duration"); 3],
        &rpus,
    )
    .expect("checked control receipt");
    let payloads = vec![digest(&bytes)];
    let runtime = Arc::new(crate::vodencode::dv_runtime::RuntimeRecipe {
        tools: Arc::new(crate::vodencode::dv_runtime::ToolSet::lifecycle_fixture(
            backend,
        )),
        source: Arc::clone(&source),
        source_offsets: Arc::new(tokio::sync::Semaphore::new(1)),
        display_target,
        display: caps.display.clone(),
        display_target_note: display_target_note.clone(),
        source_tick,
        source_origin,
        native_bl: (64, 64),
        source_clock,
        source_mastering: Ok(None),
        source_mastering_minimum: Ok(None),
        source_frame_probe: None,
        preferences,
        destination,
        mode: DvProcessingMode::Fel,
        decode: crate::dv_segment::RendererDecode::Software,
        reconstruction_slot: None,
        encoder: Encoder::Software,
        vaapi_hdr_policy: None,
        runtime_cache: root.path().join("runtime"),
        shader_cache: Arc::new(crate::test_tempdir().expect("shader cache")),
        prepared: std::sync::Mutex::new(None),
        failure_episode: digest(b"lifecycle failure"),
        preparation_budget: Duration::from_secs(5),
        report_revision: AtomicU64::new(0),
        published_receipt: std::sync::Mutex::new(None),
    });
    let mut resources = TranscodeResourceEstimate::of(&plan, &Workload::of(file, 64));
    resources.hardware_slot = true;
    resources.cpu_threads = 15;
    let mut encoding = crate::vodencode::Encoding {
        source_object_version: source.object_version().to_owned(),
        source_clock: runtime.source_clock,
        shared_audio: None,
        dv_processing: DvSelection::Selected(Box::new(selected_plan)),
        dv_runtime: Some(runtime),
        plan,
        resources,
        options,
        grid,
        reorder_frames: false,
        subtitle: None,
        subtitle_digest: None,
        ffmpeg_build: crate::ffmpeg::ffmpeg_build().await,
        executable: crate::ffmpeg::EncodedExecutable::capture()
            .await
            .expect("executable"),
        engine: crate::ffmpeg::EncodedEngine::capture(None)
            .await
            .expect("engine"),
        admissions: manager.admissions.clone(),
        store: store.clone(),
        speculative: AtomicBool::new(false),
        nonpreemptive_trial: false,
        candidate_recipe: None,
        production_proofs: Arc::new(crate::vodencode::CandidateProductionProofs::default()),
        active_production: std::sync::Mutex::new(
            crate::vodencode::ActiveProductionWindow::default(),
        ),
        queued: std::sync::Mutex::new(None),
        policy_retry: AtomicBool::new(false),
        handoff_wait: AtomicBool::new(false),
        last_refusal: std::sync::Mutex::new(None),
        handoff_claim: std::sync::Mutex::new(None),
        hooks: Box::new(crate::vodencode::NoopEncodingHooks),
    };
    let binding = crate::media_pool::PlanningBinding::from_snapshot(&snapshot);
    let recipe_digest = encoding
        .processed_candidate_digest(file, &binding)
        .expect("actual digest");
    encoding.candidate_recipe = Some(recipe_digest);
    let encoding = Arc::new(encoding);
    let driver = manager.vod.control_driver_for_test(&encoding);
    let permit = encoding
        .admissions
        .try_admit_bundle(1, 15, &resources, Priority::Live)
        .expect("mixed admission")
        .into();
    let worker = crate::vodencode::EncodePermit::clone(&permit)
        .try_claim_worker()
        .expect("exclusive worker");
    let candidate = QualityCandidate {
        id: CandidateId::for_recipe_digest(recipe_digest),
        recipe_digest,
        route: CandidateRoute::Encode,
        planned_codec: Some("hevc".into()),
        processed_dv: true,
        normalized_geometry: false,
        width: raster.0,
        height: raster.1,
        target_height: 64,
        average_bps: None,
        peak_bps: None,
        grade: encoding.delivered_output_grade(),
        decoder_compatible: true,
        complete_cache: false,
        sustainable: false,
    };
    let mut context = TranscodeManager::candidate_context(&candidate);
    context.display_target = display_target;
    context.display_target_note = display_target_note;
    context.catalog_query = Some(query.clone());
    context.canonical_caps = Some(caps.clone());
    context.planning_binding = Some(binding.clone());
    context.planning_snapshot = Some(Arc::new(snapshot.clone()));
    context.owner_node_id = Some("node".into());
    let incarnation = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let request = SessionRequest {
        continuous_media: None,
        quality_catalog: None,
        candidate_context: Some(Box::new(context)),
        sdr_master_codecs: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        file_id,
        playback_id: "processed-lifecycle".into(),
        request_id: Some(incarnation.clone()),
        control_sequence: None,
        automatic: true,
        previous_session_id: None,
        reopen_reason: None,
        kind: SessionKind::Transcode { height: 64 },
        start_seconds: 0.0,
        audio_index: projected.audio_index,
        audio_delivery: projected.audio_delivery,
        audio_claim: projected.audio_claim,
        subtitle_burn: None,
        audio_offset_ms: projected.audio_offset_ms,
        hdr10: destination != DvDestination::Sdr,
        dv_processing_destination: Some(destination),
        presentation: Presentation::Vod,
        block_budget_secs: None,
        transport: None,
    };
    let remote = crate::media_sessions::RemoteStartRequest {
        retained_output_receiver: Some(1),
        retained_output: None,
        candidate_id: Some(candidate.id),
        candidate_catalog: Some(crate::media_sessions::CandidateCatalogContext {
            caps,
            candidate,
            binding,
        }),
        presentation_target: None,
        decoder_caps: None,
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: incarnation.clone(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
            user_id: user.id,
        },
        source_size: file.size,
        source_mtime: file.mtime,
        typeless_playlist: true,
        library_channel: None,
        request,
    };
    let wire: crate::media_sessions::RemoteStartRequest =
        serde_json::from_str(&serde_json::to_string(&remote).expect("serialize typed envelope"))
            .expect("deserialize typed envelope");
    assert!(wire.request.candidate_context.is_none());
    assert_eq!(wire.request.audio_index, remote.request.audio_index);
    assert_eq!(wire.request.audio_offset_ms, remote.request.audio_offset_ms);
    assert_eq!(wire.request.audio_delivery, remote.request.audio_delivery);
    assert_eq!(wire.request.audio_claim, remote.request.audio_claim);
    assert_eq!(wire.request.dv_processing_destination, Some(destination));
    let settings = manager
        .vod_settings(&remote.request)
        .await
        .expect("settings")
        .expect("VOD");
    manager
        .vod
        .try_create(
            crate::vodserve::VodRecipeRequest {
                root_admission: Some(crate::vodserve::PreparedRootAdmission {
                    reuse_scope: manager.processed_hint_key(&snapshot, &query),
                    reused_parent: None,
                    instance_allowance: None,
                    instance_nonce: Some([1; 16]),
                    encoding: Arc::clone(&encoding),
                    permit,
                }),
                request: &remote.request,
                encoding: Some(Arc::clone(&encoding)),
                ..(&remote.request).into()
            },
            file,
            &settings,
            crate::vodserve::VodAttribution {
                user_name: "processed",
                item_title: "control",
                supersession_user: &principal_supersession_scope(&remote.principal),
            },
            session_id.clone(),
        )
        .await
        .expect("actual processed incumbent");
    manager
        .vod
        .assert_controlled_driver_held_for_test(&encoding)
        .await;
    let now = crate::media_sessions::unix_ms();
    let activation = plurx_core::domain::MediaSessionActivation {
        incarnation_id: incarnation,
        session_id: session_id.clone(),
        principal: remote.principal.clone(),
        playback_id: remote.request.playback_id.clone(),
        recovery_epoch: "processed-epoch".into(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: None,
        request_fingerprint: "a".repeat(64),
        owner_node_id: "node".into(),
        recipe_json: serde_json::to_string(&remote).expect("recipe"),
        response_json: "{}".into(),
        publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 60_000,
        expected_desired_revision: None,
    };
    manager
        .store
        .activate_media_session(&activation)
        .await
        .expect("activate")
        .expect("incumbent route");
    manager
        .store
        .settle_media_session_activation(
            &activation,
            plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now,
        )
        .await
        .expect("publish route")
        .expect("confirmed");
    Fixture {
        _driver: driver,
        incumbent_owner: manager
            .vod
            .hls_facts(&session_id)
            .await
            .expect("initial exact owner")
            .response_owner,
        _root: root,
        manager,
        remote,
        encoding,
        worker: Some(worker),
        receipt,
        payloads,
        user_id: user.id,
        session_id,
    }
}

async fn stage(f: &Fixture) -> (String, String, String) {
    let incarnation = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    let mut remote = f.remote.clone();
    remote.incarnation_id = incarnation.clone();
    remote.request.request_id = Some(incarnation.clone());
    remote.request.previous_session_id = None;
    let json = serde_json::to_string(&remote).expect("serialize drops all private context");
    let now = crate::media_sessions::unix_ms();
    f.manager
        .store
        .prepare_media_session(&plurx_core::domain::MediaSessionPreparation {
            quality_cancellation_key: None,
            incarnation_id: incarnation.clone(),
            session_id: session.clone(),
            principal: remote.principal,
            playback_id: remote.request.playback_id,
            expected_predecessor_incarnation_id: f.remote.incarnation_id.clone(),
            expected_predecessor_owner_node_id: "node".into(),
            expected_predecessor_owner_epoch: 1,
            request_fingerprint: "b".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: json.clone(),
            response_json: r#"{"prepared_output_capture_pending":true}"#.into(),
            media_origin_ms: 0,
            now_ms: now,
            expected_desired_revision: None,
            deadline_ms: now + 30_000,
        })
        .await
        .expect("stage")
        .expect("reserved");
    (incarnation, session, json)
}

async fn commit_and_retire(f: &mut Fixture, incarnation: &str, session: &str) {
    let now = crate::media_sessions::unix_ms();
    let committed = f
        .manager
        .store
        .commit_media_session_preparation(
            &f.remote.principal,
            &f.remote.request.playback_id,
            &plurx_core::domain::MediaSessionPreparationCommitRequest {
                staged_incarnation_id: incarnation.to_owned(),
                expected_predecessor_owner_node_id: "node".into(),
                expected_predecessor_owner_epoch: 1,
                now_ms: now,
                lease_expires_at_ms: now + 60_000,
                control_receipt: None,
                expected_desired_revision: None,
            },
        )
        .await
        .expect("durable CAS")
        .expect("committed exact predecessor");
    assert_eq!(committed.route.session_id, session);
    let predecessor = committed.predecessor.expect("retired predecessor");
    assert_eq!(predecessor.session_id, f.session_id);
    assert!(
        f.manager
            .vod
            .end(&predecessor.session_id, crate::vodserve::Terminal::Deleted)
            .await
    );
    let published = f
        .manager
        .store
        .complete_media_session_handoff(
            incarnation,
            "node",
            1,
            plurx_core::domain::MediaSessionProjectionCompletion::PredecessorAcknowledged,
            now,
        )
        .await
        .expect("acknowledged retirement")
        .expect("current successor");
    assert_eq!(published.publication_ready_at_ms, 0);
    f.remote = serde_json::from_str(&committed.route.recipe_json).expect("sealed wire recipe");
    assert!(
        f.remote.retained_output.is_none(),
        "ephemeral root is not a disk artifact"
    );
    f.session_id = session.to_owned();
    assert_eq!(f.encoding.admissions.in_use(), 1);
    assert_eq!(f.encoding.admissions.software_in_use(), 15);
}

#[tokio::test]
async fn staged_processed_second_successor_survives_normal_commit_and_parent_retirement() {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let mut f = fixture(destination).await;
        for _ in 0..2 {
            let (incarnation, session, json) = stage(&f).await;
            assert!(prepare(&f, &session, &json).await);
            commit_and_retire(&mut f, &incarnation, &session).await;
            let facts = f
                .manager
                .vod
                .hls_facts(&session)
                .await
                .expect("committed HLS facts");
            let encoding = facts.encoding.expect("actual processed encoding");
            assert!(Arc::ptr_eq(&encoding, &f.encoding));
            assert_eq!(
                encoding.preserves_processed_dv(),
                destination == DvDestination::Profile81
            );
            assert_eq!(encoding.processed_fel(), Some(true));
            // A selected descriptor is not report authority. Only the checked
            // control window's publication to this actual owner awards HDR10-E.
            assert!(f
                .manager
                .vod
                .dv_effective_report(&session, &incarnation)
                .await
                .is_none());
            f.manager
                .vod
                .test_publish_processed_control(&session, f.receipt.clone(), &f.payloads)
                .await;
            let report = f
                .manager
                .vod
                .dv_effective_report(&session, &incarnation)
                .await;
            let report = report.expect("actual-owner-bound control publication");
            assert!(report.fel_contributed());
            let wire = serde_json::to_value(&report).expect("public processing report");
            assert_eq!(wire["generation"].as_str(), Some(incarnation.as_str()));
            if destination == DvDestination::Hdr10 {
                assert_eq!(wire["hdr10_enhanced"], serde_json::json!(true));
                assert!(wire.get("profile81_reconstructed").is_none());
            } else {
                // P81 reports its actual DV carrier, never HDR10-E: the
                // served window's receipt says the FEL was composed.
                assert_eq!(wire["hdr10_enhanced"], serde_json::json!(false));
                assert_eq!(wire["profile81_reconstructed"], serde_json::json!(true));
                assert!(report.profile81_reconstructed());
            }
            let (_, root) = f
                .manager
                .vod
                .capture_staged_processed_root(
                    &SessionRequest {
                        request_id: Some("future-incarnation".into()),
                        ..f.remote.request.clone()
                    },
                    &StagedProcessedPredecessor {
                        session_id: session.clone(),
                        incarnation_id: incarnation.clone(),
                        owner_node_id: "node".into(),
                        owner_epoch: 1,
                        successor_session_id: "future-session".into(),
                        successor_incarnation_id: "future-incarnation".into(),
                    },
                    &principal_supersession_scope(&f.remote.principal),
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .expect("new seed survives original-parent retirement");
            assert!(Arc::ptr_eq(&root.encoding, &f.encoding));
            assert!(root.permit.clone().try_claim_worker().is_none());
            assert!(
                f.encoding
                    .admissions
                    .try_admit_bundle(1, 15, &f.encoding.resources, Priority::Live)
                    .is_none(),
                "an unrelated fresh session cannot borrow incumbent credits"
            );
        }
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await;
        assert_eq!(f.encoding.admissions.software_in_use(), 15);
        drop(f.worker.take());
        assert_eq!(f.encoding.admissions.in_use(), 0);
        assert_eq!(f.encoding.admissions.software_in_use(), 0);
    }
}

#[tokio::test]
async fn staged_processed_roots_reconstruct_both_destinations_without_serialized_authority_or_speed(
) {
    for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
        let mut f = fixture(destination).await;
        let (incarnation, session, json) = stage(&f).await;
        let mut serialized: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&json).expect("wire");
        assert!(serialized.request.candidate_context.is_none());
        assert!(
            f.manager
                .restore_candidate_context(&mut serialized)
                .await
                .is_err(),
            "ordinary restore still refuses identical JSON"
        );
        assert!(
            !f.manager
                .vod_prepare_first_before(
                    &json,
                    &session,
                    f.user_id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    f.manager
                        .session_adoption_token(&session)
                        .expect("adoption"),
                    None,
                    Instant::now() + Duration::from_secs(5)
                )
                .await,
            "no admitted epoch grants no root authority"
        );
        assert!(f.manager.vod.hls_facts(&session).await.is_none());
        assert!(
            f.manager
                .vod_prepare_first_before(
                    &json,
                    &session,
                    f.user_id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    f.manager
                        .session_adoption_token(&session)
                        .expect("adoption"),
                    Some(1),
                    Instant::now() + Duration::from_secs(5)
                )
                .await,
            "same root prepares despite sustainable=false"
        );
        let expected = StagedProcessedPredecessor {
            session_id: session.clone(),
            incarnation_id: incarnation.clone(),
            owner_node_id: "node".into(),
            owner_epoch: 1,
            successor_session_id: uuid::Uuid::new_v4().to_string(),
            successor_incarnation_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut next = f.remote.request.clone();
        next.request_id = Some(expected.successor_incarnation_id.clone());
        next.previous_session_id = None;
        let (_, root) = f
            .manager
            .vod
            .capture_staged_processed_root(
                &next,
                &expected,
                &principal_supersession_scope(&f.remote.principal),
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("successor retained its own provenance");
        assert!(Arc::ptr_eq(&root.encoding, &f.encoding));
        assert!(root.permit.clone().try_claim_worker().is_none());
        drop(root);
        assert_eq!(f.encoding.admissions.in_use(), 1);
        assert_eq!(f.encoding.admissions.software_in_use(), 15);
        let owner = f
            .manager
            .vod
            .hls_facts(&session)
            .await
            .expect("actual successor")
            .response_owner;
        assert!(
            f.manager
                .vod_prepare_first_before(
                    &json,
                    &session,
                    f.user_id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    f.manager.session_adoption_token(&session).expect("replay"),
                    Some(1),
                    Instant::now() + Duration::from_secs(5)
                )
                .await
        );
        assert!(
            f.manager.vod.response_owner_is_live(&session, &owner).await,
            "replay did not recapture/replace"
        );
        assert!(
            f.manager
                .vod
                .end(&f.session_id, crate::vodserve::Terminal::Deleted)
                .await
        );
        assert_eq!(f.encoding.admissions.software_in_use(), 15);
        assert!(
            f.manager
                .vod
                .end(&session, crate::vodserve::Terminal::Deleted)
                .await
        );
        assert_eq!(
            f.encoding.admissions.software_in_use(),
            15,
            "exclusive worker retains credit"
        );
        assert!(
            !prepare(&f, &session, &json).await,
            "completed capture without a local graph cannot mint authority again"
        );
        drop(f.worker.take());
        assert_eq!(f.encoding.admissions.in_use(), 0);
        assert_eq!(f.encoding.admissions.software_in_use(), 0);
    }
}

#[tokio::test]
async fn staged_processed_sdr_root_keeps_main10_and_sdr_grade_across_admission_and_replay() {
    let mut f = fixture(DvDestination::Sdr).await;
    let (_, session, json) = stage(&f).await;
    assert!(
        prepare(&f, &session, &json).await,
        "fresh admitted SDR successor"
    );
    assert_admitted_delivery(&f, &session, DvDestination::Sdr).await;
    let owner = f
        .manager
        .vod
        .hls_facts(&session)
        .await
        .expect("successor")
        .response_owner;
    assert!(
        prepare(&f, &session, &json).await,
        "same owner replays its prepare"
    );
    assert!(f.manager.vod.response_owner_is_live(&session, &owner).await);
    assert_admitted_delivery(&f, &session, DvDestination::Sdr).await;
    assert!(
        f.manager
            .vod
            .end(&f.session_id, crate::vodserve::Terminal::Deleted)
            .await
    );
    assert!(
        f.manager
            .vod
            .end(&session, crate::vodserve::Terminal::Deleted)
            .await
    );
    drop(f.worker.take());
    assert_eq!(f.encoding.admissions.in_use(), 0);
    assert_eq!(f.encoding.admissions.software_in_use(), 0);
}

// Both calls run behind the newly admitted successor, before and after its
// idempotent prepare replay. The carrier remains HDR10 for processed SDR.
async fn assert_admitted_delivery(f: &Fixture, session: &str, destination: DvDestination) {
    let expected_grade = if destination == DvDestination::Sdr {
        OutputGrade::Sdr
    } else {
        OutputGrade::Hdr10
    };
    assert_eq!(f.encoding.options.pipeline, Pipeline::Hdr10Passthrough);
    let recovered = f
        .manager
        .recover(session)
        .await
        .expect("idempotent create answer");
    assert_eq!(recovered.grade, expected_grade);
    let HlsPresentationResolution::Ready(context, file, _) = f
        .manager
        .hls_presentation_before(session, Instant::now() + Duration::from_secs(10))
        .await
    else {
        panic!("admitted successor must have a frozen HLS presentation");
    };
    assert_eq!(file.video_codec.as_deref(), Some("hevc"));
    assert_eq!(file.bit_depth, Some(10));
    assert!(context.codecs.starts_with("hvc1") || context.codecs.starts_with("hev1"));
    let master = String::from_utf8(crate::http::hls::source_native_master(
        &file,
        None,
        &context,
        &Default::default(),
    ))
    .expect("HLS text");
    if destination == DvDestination::Sdr {
        assert!(file.hdr.is_none());
        assert!(file.hdr_format.is_none());
        assert!(!master.contains("VIDEO-RANGE=PQ"), "{master}");
        assert!(!master.contains("VIDEO-RANGE=HLG"), "{master}");
        assert!(!master.contains("avc1"), "{master}");
        assert!(context.supplemental_codecs.is_none(), "{master}");
        assert_eq!(recovered.processed_dv_profile, None);
    } else {
        assert!(master.contains("VIDEO-RANGE=PQ"), "{master}");
    }
}
