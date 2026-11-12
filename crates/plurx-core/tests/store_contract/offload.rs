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

        // Marked, then pruned: only the unmarked fileless leaf goes.
        assert_eq!(store.mark_items_offloaded(&[kept], 100).await.expect("mark"), 1, "{backend}");
        assert_eq!(store.prune_empty_items(library.id).await.expect("prune"), 1, "{backend}");
        assert!(store.get_item(kept).await.expect("get").is_some(), "{backend}");
        assert!(store.get_item(plain).await.expect("get").is_none(), "{backend}");
        assert_eq!(
            store.offloaded_leaf_counts(&[kept]).await.expect("counts").get(&kept),
            Some(&1),
            "{backend}"
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
                &format!("/offload-{backend}/Tar.mkv"),
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
