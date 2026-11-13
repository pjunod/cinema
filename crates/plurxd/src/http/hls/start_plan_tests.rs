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
/// revisions — and the refusal closes the ingress's view.
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
    assert_eq!(reads(&applied, "binding_mismatch"), 1, "Authority once");
    // The ingress closes its view; on SQLite the store is the only copy and
    // every floor is met, so the next plan is the store's new revision.
    let before = refusal_closes();
    close_after_refusal(&applied);
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
