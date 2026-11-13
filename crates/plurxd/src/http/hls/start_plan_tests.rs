//! D3b, contract §4.3–4.4 and §8: the owner's plan at the ingress's binding,
//! a legacy route's authority plan, and a setting saved between the plan and
//! the owner.

use super::*;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
use plurx_core::store::{SqliteStore, Store};

/// A store with one probed source, and the source's id.
pub(crate) async fn planned_source() -> (Arc<dyn Store>, i64) {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let library = store
        .create_library(&NewLibrary {
            name: "plan".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/plan".into()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "planned".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let file = store
        .upsert_file(
            item,
            "/plan/movie.mkv",
            100,
            42,
            &ProbeResult {
                video_codec: Some("h264".into()),
                raw_json: Some(r#"{"streams":[]}"#.into()),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    (store, file)
}

fn reads(applied: &AppliedView, reason: &str) -> u64 {
    applied
        .observation()
        .reads
        .iter()
        .filter(|(view, source, read_reason, _)| {
            *view == "planning" && *source == "authority" && *read_reason == reason
        })
        .map(|(_, _, _, count)| *count)
        .sum()
}

/// The ingress plans once from its view; an owner at the same revision
/// plans at the same binding from its own view, with no authority read.
#[tokio::test]
async fn an_owner_at_the_ingress_revision_plans_at_its_binding_from_its_view() {
    let (store, file) = planned_source().await;
    let applied = AppliedView::authority(Arc::clone(&store));
    let ingress = ingress_plan(&applied, file)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!(ingress.source, ViewSource::View);
    assert_eq!(ingress.snapshot.item_title.as_deref(), Some("planned"));
    let owner = owner_plan(&applied, file, Some(&ingress.binding))
        .await
        .expect("owner plan")
        .expect("source");
    assert_eq!(owner.binding, ingress.binding);
    assert_eq!(owner.source, ViewSource::View);
    assert_eq!(reads(&applied, "binding_mismatch"), 0);
    // The plan answers its own file revision's probe, and no other.
    assert_eq!(
        ingress.probe_for(&ingress.snapshot.file),
        Some(Some(r#"{"streams":[]}"#))
    );
    let mut replaced = ingress.snapshot.file.clone();
    replaced.mtime += 1;
    assert_eq!(ingress.probe_for(&replaced), None);
}

/// §4.4: a planning setting saved between the ingress's plan and the
/// owner's start. The owner's plan at the old binding is refused after one
/// authority read — the session is never built from a mix of the two
/// revisions — and the ingress's refusal arm (`remote_plan_refused`, review
/// S1/S3) closes both planning floors and answers the typed retryable code.
#[tokio::test]
async fn a_setting_saved_before_the_owner_starts_refuses_rather_than_mixing() {
    let (store, file) = planned_source().await;
    let applied = AppliedView::authority(Arc::clone(&store));
    let ingress = ingress_plan(&applied, file)
        .await
        .expect("plan")
        .expect("source");
    store
        .put_setting(plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS, "20")
        .await
        .expect("save a planning key");
    let refused = owner_plan(&applied, file, Some(&ingress.binding)).await;
    assert!(
        matches!(refused, Err(OwnerPlanError::Changed)),
        "{refused:?}"
    );
    // One plan, one count (review N1): the store's answer at another binding
    // is the one authority read, not also a view read.
    assert_eq!(reads(&applied, "binding_mismatch"), 1, "Authority once");
    assert_eq!(
        applied
            .observation()
            .view_total(AppliedTable::Files, ViewSource::View),
        1,
        "only the ingress's own plan read the view"
    );
    assert!(!applied.floor_noted(AppliedTable::Settings));
    assert!(!applied.floor_noted(AppliedTable::Files));
    let before = refusal_closes();
    let answer = remote_plan_refused(&applied, "node-b");
    assert!(
        matches!(
            answer,
            crate::http::error::ApiError::Typed {
                code: "quality_catalog_unavailable",
                ..
            }
        ),
        "{answer:?}"
    );
    assert!(
        applied.floor_noted(AppliedTable::Settings),
        "the refusal closes the view"
    );
    assert!(
        applied.floor_noted(AppliedTable::Files),
        "the refusal closes the view"
    );
    assert_eq!(refusal_closes(), before + 1);
    let replanned = ingress_plan(&applied, file)
        .await
        .expect("plan")
        .expect("source");
    assert_ne!(replanned.binding, ingress.binding);
    assert_eq!(
        replanned.setting(plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS),
        Some("20")
    );
    // The owner, sent the new binding, builds at it.
    let owner = owner_plan(&applied, file, Some(&replanned.binding))
        .await
        .expect("owner plan")
        .expect("source");
    assert_eq!(owner.binding, replanned.binding);
}

/// Review B1: a catalog's quorum read refutes the plan this node's view
/// answered, before the claim. The create closes its planning view and runs
/// once more from an Authority plan, and that second attempt's answer — a
/// success — is the create's, not the 503 the first attempt returned.
#[tokio::test]
async fn a_stale_view_plan_replans_once_from_authority_before_the_claim() {
    let (store, file) = planned_source().await;
    let applied = AppliedView::authority(Arc::clone(&store));
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let before = stale_replans();
    let budget = crate::media_pool::CreateStartupBudget::new(5_000);
    let answer = budget
        .scope(replan_once_after_stale(&applied, || async {
            let first = attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0;
            let plan = ingress_plan(&applied, file)
                .await
                .expect("plan")
                .expect("source");
            crate::media_pool::capture_create_plan(&plan);
            if first {
                // What `quality_catalog` notes when a node's quorum read
                // answers another binding than the plan's.
                crate::media_pool::note_create_catalog(
                    &crate::media_pool::QualityCatalogResult::unavailable(
                        crate::media_pool::CatalogCause::StaleSource,
                    ),
                );
                return Err(crate::http::error::ApiError::typed(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "quality_catalog_unavailable",
                    "quality discovery is incomplete",
                ));
            }
            Ok(plan.source)
        }))
        .await;
    assert_eq!(
        answer.expect("the re-planned create succeeds"),
        ViewSource::Authority
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert!(
        applied.floor_noted(AppliedTable::Settings),
        "the view is closed"
    );
    assert!(
        applied.floor_noted(AppliedTable::Files),
        "the view is closed"
    );
    assert_eq!(
        applied.observation().last_planning_fallback,
        Some(ViewFallback::Floor),
        "the second plan read Authority"
    );
    assert_eq!(stale_replans(), before + 1);
}

/// Review B1's bounds: no re-plan after the claim (something durable then
/// exists), none for a failure no catalog refuted, and never a second one.
#[tokio::test]
async fn a_create_replans_only_before_its_claim_and_only_once() {
    let (store, file) = planned_source().await;
    let applied = AppliedView::authority(Arc::clone(&store));
    let stale = || {
        crate::media_pool::note_create_catalog(
            &crate::media_pool::QualityCatalogResult::unavailable(
                crate::media_pool::CatalogCause::StaleSource,
            ),
        )
    };
    let refused = || {
        Err::<(), _>(crate::http::error::ApiError::ServiceUnavailable(
            "refused".into(),
        ))
    };
    for case in ["after the claim", "not refuted", "refuted twice"] {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let budget = crate::media_pool::CreateStartupBudget::new(5_000);
        let answer = budget
            .scope(replan_once_after_stale(&applied, || async {
                attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let plan = ingress_plan(&applied, file)
                    .await
                    .expect("plan")
                    .expect("source");
                crate::media_pool::capture_create_plan(&plan);
                match case {
                    "after the claim" => {
                        crate::media_pool::own_create_until(
                            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                        );
                        stale();
                    }
                    "refuted twice" => stale(),
                    _ => {}
                }
                refused()
            }))
            .await;
        assert!(answer.is_err(), "{case}");
        let expected = if case == "refuted twice" { 2 } else { 1 };
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::Relaxed),
            expected,
            "{case}"
        );
    }
}

/// Review S2: before a start that carries no binding goes to a remote owner
/// (which plans from Authority), the ingress confirms its view plan at
/// Authority — or closes its view and refuses rather than let the two sides
/// plan different revisions.
#[tokio::test]
async fn an_unbound_remote_start_is_confirmed_at_authority_first() {
    let (store, file) = planned_source().await;
    let applied = AppliedView::authority(Arc::clone(&store));
    let plan = ingress_plan(&applied, file)
        .await
        .expect("plan")
        .expect("source");
    confirm_unbound_remote(&applied, &plan)
        .await
        .expect("the same revision is confirmed");
    assert_eq!(reads(&applied, "unbound"), 1);
    assert!(!applied.floor_noted(AppliedTable::Files));
    store
        .put_setting(plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED, "1")
        .await
        .expect("save a planning key");
    let refused = confirm_unbound_remote(&applied, &plan).await;
    assert!(
        matches!(
            refused,
            Err(crate::http::error::ApiError::Typed {
                code: "quality_catalog_unavailable",
                ..
            })
        ),
        "{refused:?}"
    );
    assert!(applied.floor_noted(AppliedTable::Settings));
    assert!(applied.floor_noted(AppliedTable::Files));
    // An Authority plan is already Authority's: nothing more to read.
    let authority = StartPlan::new((*plan.snapshot).clone(), ViewSource::Authority);
    confirm_unbound_remote(&applied, &authority)
        .await
        .expect("an Authority plan");
    assert_eq!(reads(&applied, "unbound"), 2);
}

/// Reviews S1 and S2, where they act: the placement loop stops at a refused
/// plan, and a start with no binding is confirmed before a remote owner
/// takes it. Source pins of `create_with_purpose_attempt`.
#[test]
fn the_placement_loop_stops_at_a_refused_plan_and_confirms_an_unbound_start() {
    let source = include_str!("create.rs");
    let start = source
        .find("async fn create_with_purpose_attempt(")
        .expect("the create");
    let body: String = source[start..]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let failed = body
        .find("last_error=Some(error);ifplacement_ended{break;}")
        .expect("a failed attempt ends placement when the plan was refused");
    assert!(body[..failed].contains("placement_ended=super::start_plan::ends_placement(failure);"));
    assert!(body.contains(
        "ifremote_request.candidate_catalog.is_none()&&!unbound_confirmed{ifletSome(plan)=start_plan.as_deref(){ifletErr(error)=super::start_plan::confirm_unbound_remote(&state.applied,plan).await{last_error=Some(error);break;}}"
    ));
    assert!(body.contains(
        "RemoteStartFailure::PlanChanged)=>Err(super::start_plan::remote_plan_refused(&state.applied,&candidate)"
    ));
}

/// §4.3: a legacy remote start carries no binding, so the owner plans from
/// Authority — once for the whole start.
#[tokio::test]
async fn a_legacy_remote_start_plans_from_authority() {
    let (store, file) = planned_source().await;
    let applied = AppliedView::authority(Arc::clone(&store));
    let plan = owner_plan(&applied, file, None)
        .await
        .expect("owner plan")
        .expect("source");
    assert_eq!(plan.source, ViewSource::Authority);
    assert_eq!(reads(&applied, "unbound"), 1);
    assert_eq!(
        applied.observation().last_planning_fallback,
        Some(ViewFallback::Unbound)
    );
    // A source gone at Authority is no plan: the chain reads and refuses as
    // before.
    assert!(owner_plan(&applied, file + 1_000, None)
        .await
        .expect("owner plan")
        .is_none());
}

// ---- D3d: the catalog reads the create's plan, a peer's reads its view ----

/// A node on a store with one probed source; its applied view is that store's.
async fn catalog_node() -> (crate::state::AppState, i64) {
    let (store, file) = planned_source().await;
    let root = crate::test_temp_path(format!("plurx-d3d-{}", uuid::Uuid::new_v4()));
    let state = crate::state::AppState::new(
        "test".to_owned(),
        store,
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
        Arc::new(crate::logbuf::LogBuffer::new(64)),
    );
    state.serving.validation_set_ready(true).await;
    (state, file)
}

fn catalog_request(plan: &StartPlan) -> crate::media_pool::QualityCatalogRequest {
    crate::media_pool::QualityCatalogRequest {
        audio_claim: None,
        audio_delivery: None,
        copy_contract: None,
        file_id: plan.snapshot.file.id,
        source_size: plan.snapshot.file.size,
        source_mtime: plan.snapshot.file.mtime,
        caps: plurx_core::playback::DeviceCaps {
            v: 2,
            ..Default::default()
        },
        audio_index: None,
        audio_offset_ms: 0,
        subtitle_burn: None,
        presentation: crate::transcode::Presentation::Vod,
    }
}

fn soon() -> tokio::time::Instant {
    tokio::time::Instant::now() + std::time::Duration::from_secs(5)
}

/// D3d, §4.6: in create scope the local catalog is answered from the create's
/// plan and reads no planning snapshot of its own. The store moves on after
/// the plan is captured (a planning setting is saved); the create's catalog
/// still describes the plan's revision and is complete, where a quorum read
/// at the plan's binding (what it did before D3d) is refuted as stale.
#[tokio::test]
async fn the_create_catalog_answers_from_the_create_plan_and_reads_nothing() {
    let (state, file) = catalog_node().await;
    let plan = ingress_plan(&state.applied, file)
        .await
        .expect("plan")
        .expect("source");
    let request = catalog_request(&plan);
    // Anything the catalog read would see this.
    state
        .store
        .put_setting(plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS, "20")
        .await
        .expect("a planning key moves after the plan");

    // Through the create's own scope: `quality_catalog` finds the captured plan.
    let budget = crate::media_pool::CreateStartupBudget::new(5_000);
    let through_scope = budget
        .scope(async {
            crate::media_pool::capture_create_plan(&plan);
            state
                .media_pool
                .quality_catalog(&state, request.clone())
                .await
        })
        .await;
    assert!(
        through_scope.complete && through_scope.causes.is_empty(),
        "the create's catalog describes its plan: {:?}",
        through_scope.causes
    );

    // The plan source directly, and the counterfactual: the same request by
    // quorum at the plan's binding is refuted, because it reads the store.
    let from_plan = crate::media_pool::local_quality_catalog(
        &state,
        &request,
        soon(),
        crate::media_pool::CatalogSource::Plan(&plan),
    )
    .await;
    assert!(
        from_plan.complete && from_plan.causes.is_empty(),
        "{:?}",
        from_plan.causes
    );
    let by_quorum = crate::media_pool::local_quality_catalog(
        &state,
        &request,
        soon(),
        crate::media_pool::CatalogSource::Quorum(Some(&plan.binding)),
    )
    .await;
    assert_eq!(
        by_quorum.causes,
        vec![crate::media_pool::CatalogCause::StaleSource],
        "a quorum read sees the saved key"
    );
    // A plan for another file is not the create's plan for this request: the
    // catalog reads the Store.
    let mut other = request.clone();
    other.file_id += 1;
    let budget = crate::media_pool::CreateStartupBudget::new(5_000);
    let unplanned = budget
        .scope(async {
            crate::media_pool::capture_create_plan(&plan);
            state.media_pool.quality_catalog(&state, other).await
        })
        .await;
    assert_eq!(
        unplanned.causes,
        vec![crate::media_pool::CatalogCause::StaleSource],
        "no plan for that file: it is the quorum read's answer for a missing source"
    );
}

/// D3d, §4.6: a peer's catalog request reads this node's view at the binding
/// it is sent; the same binding is answered from the view, a binding Authority
/// holds differently is `StaleSource` after one authority read, a request with
/// no binding reads quorum as before, and `StaleSource` is the cause that
/// re-plans the sending create, through the same wire answer a peer sends.
#[tokio::test]
async fn a_peers_catalog_reads_its_view_and_a_stale_sender_replans() {
    let (state, file) = catalog_node().await;
    let ingress = ingress_plan(&state.applied, file)
        .await
        .expect("plan")
        .expect("source");
    let request = catalog_request(&ingress);
    let (planned_before, stale_before) = peer_catalog_counts();

    let agreed = crate::media_pool::local_quality_catalog(
        &state,
        &request,
        soon(),
        crate::media_pool::CatalogSource::PeerView(&ingress.binding),
    )
    .await;
    assert!(
        agreed.complete && agreed.causes.is_empty(),
        "{:?}",
        agreed.causes
    );
    assert_eq!(
        reads(&state.applied, "binding_mismatch"),
        0,
        "the view answered"
    );
    assert_eq!(peer_catalog_counts(), (planned_before + 1, stale_before));

    // The sender planned from a replica that is behind: Authority holds
    // another revision of the settings by the time the peer reads.
    state
        .store
        .put_setting(plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS, "20")
        .await
        .expect("a planning key moves");
    let stale = crate::media_pool::local_quality_catalog(
        &state,
        &request,
        soon(),
        crate::media_pool::CatalogSource::PeerView(&ingress.binding),
    )
    .await;
    assert_eq!(
        stale.causes,
        vec![crate::media_pool::CatalogCause::StaleSource]
    );
    assert!(!stale.complete);
    assert_eq!(
        reads(&state.applied, "binding_mismatch"),
        1,
        "Authority once"
    );
    assert_eq!(
        peer_catalog_counts(),
        (planned_before + 1, stale_before + 1)
    );

    // A missing source is `StaleSource`, as the quorum read always answered.
    let mut gone = request.clone();
    gone.file_id += 1_000;
    let missing = crate::media_pool::local_quality_catalog(
        &state,
        &gone,
        soon(),
        crate::media_pool::CatalogSource::PeerView(&ingress.binding),
    )
    .await;
    assert_eq!(
        missing.causes,
        vec![crate::media_pool::CatalogCause::StaleSource]
    );

    // The whole path: the peer's answer crosses the wire as JSON, the
    // ingress notes it, and the create closes its view and plans once more
    // from Authority; that attempt's answer is the create's.
    let wire: crate::media_pool::QualityCatalogResult =
        serde_json::from_slice(&serde_json::to_vec(&stale).expect("serialize"))
            .expect("the peer's answer round-trips");
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let before = stale_replans();
    let budget = crate::media_pool::CreateStartupBudget::new(5_000);
    let answer = budget
        .scope(replan_once_after_stale(&state.applied, || async {
            let first = attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0;
            let plan = ingress_plan(&state.applied, file)
                .await
                .expect("plan")
                .expect("source");
            crate::media_pool::capture_create_plan(&plan);
            if first {
                // `quality_catalog` notes what its peers answered.
                crate::media_pool::note_create_catalog(&wire);
                return Err(crate::http::error::ApiError::typed(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "quality_catalog_unavailable",
                    "quality discovery is incomplete",
                ));
            }
            // The re-plan is at the revision the peer holds: it agrees.
            let agreed = crate::media_pool::local_quality_catalog(
                &state,
                &catalog_request(&plan),
                soon(),
                crate::media_pool::CatalogSource::PeerView(&plan.binding),
            )
            .await;
            assert!(agreed.complete, "{:?}", agreed.causes);
            Ok(plan.source)
        }))
        .await;
    assert_eq!(
        answer.expect("the re-planned create succeeds"),
        ViewSource::Authority
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(stale_replans(), before + 1);
    assert!(state.applied.floor_noted(AppliedTable::Settings));

    // No binding, no view: quorum, with nothing to compare.
    let legacy = crate::media_pool::local_quality_catalog(
        &state,
        &request,
        soon(),
        crate::media_pool::CatalogSource::Quorum(None),
    )
    .await;
    assert!(legacy.complete, "{:?}", legacy.causes);
}

/// The two places that decide a catalog's source stay the only ones: the
/// create's scope answers from its plan, the peer handler from the view, and
/// every other caller says Quorum by name.
#[test]
fn the_catalog_sources_are_chosen_in_three_places() {
    let pool = include_str!("../../media_pool.rs");
    let squashed = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let pool_body = squashed(pool);
    assert!(pool_body.contains("Some(plan)=>CatalogSource::Plan(plan),None=>CatalogSource::Quorum(expected_binding.as_ref()),"));
    assert!(pool_body.contains("CatalogSource::Quorum(Some(&catalog.binding)),"));
    let internal = squashed(include_str!("../internal_media.rs"));
    assert!(internal.contains("Some(expected)=>crate::media_pool::CatalogSource::PeerView(expected),None=>crate::media_pool::CatalogSource::Quorum(None),"));
}
