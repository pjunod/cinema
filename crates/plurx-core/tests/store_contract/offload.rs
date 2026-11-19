//! Offload on both backends: an item whose files Curator deleted is kept by
//! every prune, found again by ids when files return, and unmarked the moment
//! a file attaches (the `files` trigger, installed by
//! `install_offloaded_schema` on the replicated store).
use super::*;
use plurx_core::domain::{ItemKind, MetadataPatch, NewItem, ProbeResult};

#[tokio::test]
async fn an_offloaded_item_survives_prunes_is_found_by_ids_and_clears_when_a_file_attaches() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: format!("Offload {backend}"),
                kind: LibraryKind::Movies,
                paths: vec![format!("/offload-{backend}").into()],
                anime: false,
            })
            .await
            .expect("library");
        let movie = |title: &str| NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: title.into(),
            year: Some(2022),
            season_number: None,
            episode_number: None,
        };
        let kept = store.insert_item(&movie("Tar")).await.expect("kept");
        let plain = store.insert_item(&movie("Plain")).await.expect("plain");
        store
            .apply_metadata(
                kept,
                &MetadataPatch {
                    tmdb_id: Some(817758),
                    ..Default::default()
                },
            )
            .await
            .expect("tmdb");

        // Files for both, a second copy for `twin`.
        let twin = store.insert_item(&movie("Twin")).await.expect("twin");
        let probe = ProbeResult::default();
        let kept_file = store
            .upsert_file(kept, &format!("/offload-{backend}/Tar.mkv"), 10, 10, &probe)
            .await
            .expect("file");
        let plain_file = store
            .upsert_file(
                plain,
                &format!("/offload-{backend}/Plain.mkv"),
                10,
                10,
                &probe,
            )
            .await
            .expect("file");
        let twin_a = store
            .upsert_file(
                twin,
                &format!("/offload-{backend}/Twin-a.mkv"),
                10,
                10,
                &probe,
            )
            .await
            .expect("file");
        store
            .upsert_file(
                twin,
                &format!("/offload-{backend}/Twin-b.mkv"),
                10,
                10,
                &probe,
            )
            .await
            .expect("file");
        store
            .ensure_library_root_fingerprint(library.id, &library.paths, "fp", true)
            .await
            .expect("root");

        // A refused reconcile (wrong root) marks nothing and removes nothing.
        let refused = store
            .reconcile_library(library.id, "wrong", &[kept_file], 1, &[kept], 100)
            .await
            .expect("reconcile");
        assert!(
            matches!(refused, ReconcileOutcome::RefusedRoot { .. }),
            "{backend}"
        );
        let item = store.get_item(kept).await.expect("get").expect("item");
        assert_eq!(
            item.offloaded_at, None,
            "{backend}: a refused removal marks nothing"
        );

        // Applied: `kept` is marked inside the transaction and survives the
        // prune, `plain` goes, and `twin` still holds a file so it stays
        // on disk, unmarked.
        let applied = store
            .reconcile_library(
                library.id,
                "fp",
                &[kept_file, plain_file, twin_a],
                3,
                &[kept, twin],
                100,
            )
            .await
            .expect("reconcile");
        assert!(
            matches!(applied, ReconcileOutcome::Applied { .. }),
            "{backend}"
        );
        let item = store.get_item(kept).await.expect("get").expect("kept");
        assert_eq!(item.offloaded_at, Some(100), "{backend}");
        assert!(
            store.get_item(plain).await.expect("get").is_none(),
            "{backend}"
        );
        let item = store.get_item(twin).await.expect("get").expect("twin");
        assert_eq!(item.offloaded_at, None, "{backend}: another copy remains");
        assert_eq!(
            store.prune_empty_items(library.id).await.expect("prune"),
            0,
            "{backend}"
        );
        assert!(
            store.get_item(kept).await.expect("get").is_some(),
            "{backend}"
        );
        assert_eq!(
            store
                .offloaded_leaf_counts(&[kept])
                .await
                .expect("counts")
                .get(&kept)
                .copied(),
            Some(plurx_core::domain::LeafOffload {
                offloaded: 1,
                leaves: 1
            }),
            "{backend}"
        );

        // The enrichment jobs leave it alone: there is nothing to match or
        // fetch art for, and no file to probe. Its unmarked twin is still work.
        let pending: Vec<i64> = store
            .items_needing_metadata(Some(library.id), true, None)
            .await
            .expect("metadata work")
            .iter()
            .map(|item| item.id)
            .collect();
        assert!(
            !pending.contains(&kept),
            "{backend}: offloaded is not metadata work"
        );
        assert!(
            pending.contains(&twin),
            "{backend}: an on-disk title still is"
        );

        // Nor does the artwork retry sweep: an offloaded title a provider has
        // answered for and with no poster is not artwork work, while an
        // on-disk one is.
        for id in [kept, twin] {
            store
                .apply_metadata(
                    id,
                    &MetadataPatch {
                        enriched: true,
                        ..Default::default()
                    },
                )
                .await
                .expect("enriched");
        }
        let missing: Vec<i64> = store
            .items_missing_artwork(Some(library.id), 0, 100)
            .await
            .expect("artwork work")
            .iter()
            .map(|item| item.id)
            .collect();
        assert!(
            !missing.contains(&kept),
            "{backend}: offloaded is not artwork work"
        );
        assert!(
            missing.contains(&twin),
            "{backend}: an on-disk matched title with no poster still is"
        );

        // Ids only, never a title.
        let found = store
            .find_offloaded_movie(library.id, Some(817758), None)
            .await
            .expect("find")
            .expect("by tmdb");
        assert_eq!(found.id, kept, "{backend}");
        assert!(store
            .find_offloaded_movie(library.id, Some(1), None)
            .await
            .expect("find")
            .is_none());

        // A file attaching takes the mark off, on every write path.
        store
            .upsert_file(
                kept,
                &format!("/offload-{backend}/Tar-returned.mkv"),
                10,
                10,
                &ProbeResult::default(),
            )
            .await
            .expect("file");
        let item = store.get_item(kept).await.expect("get").expect("item");
        assert_eq!(item.offloaded_at, None, "{backend}");
        assert!(store
            .find_offloaded_movie(library.id, Some(817758), None)
            .await
            .expect("find")
            .is_none());
    })
    .await;
}
