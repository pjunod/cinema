use super::*;
use plurx_core::store::{
    JellyfinCatalogPage, JellyfinCatalogQuery, JellyfinCatalogSort as Sort, JellyfinClientFamily,
    JellyfinEntityKind as Kind, JellyfinLoginWrite,
};

async fn query_for(store: &Arc<dyn Store>, user_id: i64) -> JellyfinCatalogQuery {
    let hash = plurx_core::auth::hash_token("catalog-test-login");
    assert!(store
        .replace_jellyfin_login(
            JellyfinLoginWrite {
                user_id,
                token_hash: hash.clone(),
                device_digest: plurx_core::auth::hash_token("catalog-device"),
                client_family: JellyfinClientFamily::AndroidTv,
                device_label: None,
                expected_password_hash: "hash".into(),
                created_at: unix_seconds(),
            },
            None
        )
        .await
        .expect("catalog login"));
    let users = store
        .jellyfin_entity_ids(Kind::User, &[user_id])
        .await
        .expect("user identity");
    JellyfinCatalogQuery {
        mode: plurx_core::store::JellyfinCatalogMode::Browse,
        today: None,
        user_wire: users[0].wire_id.clone(),
        token_hash: hash,
        parent_wire: None,
        item_wire: None,
        recursive: true,
        start: 0,
        limit: 100,
        kinds: vec![],
        search: None,
        sorts: vec![Sort::SortName],
        descending: false,
    }
}
async fn primed(store: &Arc<dyn Store>, query: &JellyfinCatalogQuery) -> JellyfinCatalogPage {
    for _ in 0..4 {
        let page = store
            .jellyfin_catalog_page(query.clone())
            .await
            .expect("catalog page");
        assert!(!page.source_overflow);
        if page.missing.is_empty() {
            return page;
        }
        for (name, kind) in [
            ("item", Kind::Item),
            ("file", Kind::File),
            ("library", Kind::Library),
        ] {
            let ids = page
                .missing
                .iter()
                .filter(|m| m.kind == name)
                .map(|m| m.id)
                .collect::<Vec<_>>();
            for chunk in ids.chunks(1000) {
                store
                    .jellyfin_entity_ids(kind, chunk)
                    .await
                    .expect("prime current mappings");
            }
        }
        // Re-read the entire projection; do not attach IDs to the earlier bodies.
    }
    panic!("fixture catalog failed to converge")
}
fn item(
    library_id: i64,
    kind: ItemKind,
    parent_id: Option<i64>,
    title: &str,
    number: Option<i32>,
) -> NewItem {
    NewItem {
        library_id,
        kind,
        parent_id,
        title: title.into(),
        year: None,
        season_number: number,
        episode_number: number,
    }
}

#[tokio::test]
async fn jellyfin_catalog_pages_2500_tied_names_once_and_keeps_empty_page_totals() {
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "catalog-page").await;
        let seeded = store.get_file(file).await.expect("file").expect("file");
        let library = store
            .get_item(seeded.item_id)
            .await
            .expect("item")
            .expect("item")
            .library_id;
        // The seed is outside the search; 2,500 exact ties force the ID tie-break.
        for _ in 0..2500 {
            store
                .insert_item(&item(library, ItemKind::Movie, None, "Exact tie", None))
                .await
                .expect("tie fixture");
        }
        let mut q = query_for(&store, uid).await;
        q.search = Some("Exact tie".into());
        q.limit = 500;
        let mut all = Vec::new();
        for start in (0..2500).step_by(500) {
            q.start = start;
            let page = primed(&store, &q).await;
            assert!(page.authorized && page.parent_valid, "{backend}");
            assert_eq!(page.total, 2500, "{backend}");
            assert_eq!(page.items.len(), 500, "{backend}");
            all.extend(
                page.items
                    .iter()
                    .map(|i| i["wire_id"].as_str().expect("wire ID").to_owned()),
            );
        }
        assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), 2500, "{backend}");
        q.descending = true;
        q.start = 0;
        let reverse = primed(&store, &q).await;
        assert_eq!(
            reverse.items[0]["wire_id"].as_str(),
            all.last().map(String::as_str)
        );
        for (start, limit) in [(2500, 100), (i64::MAX, 100), (0, 0)] {
            q.start = start;
            q.limit = limit;
            let empty = primed(&store, &q).await;
            assert_eq!(empty.total, 2500);
            assert!(empty.items.is_empty());
        }
        q.limit = 501;
        assert!(store.jellyfin_catalog_page(q).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn jellyfin_catalog_snapshot_keeps_hierarchy_watch_sources_and_retirement_scoped() {
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "catalog-hierarchy").await;
        let native = store.get_file(file).await.expect("file").expect("file");
        let movie = store
            .get_item(native.item_id)
            .await
            .expect("item")
            .expect("item");
        let shows = store
            .create_library(&NewLibrary {
                name: "Series library".into(),
                kind: LibraryKind::Shows,
                paths: vec![PathBuf::from("/series")],
                anime: false,
            })
            .await
            .expect("shows");
        let show = store
            .insert_item(&item(shows.id, ItemKind::Show, None, "Series", None))
            .await
            .expect("show");
        let season = store
            .insert_item(&item(
                shows.id,
                ItemKind::Season,
                Some(show),
                "Season",
                Some(1),
            ))
            .await
            .expect("season");
        let ep1 = store
            .insert_item(&item(
                shows.id,
                ItemKind::Episode,
                Some(season),
                "100% literal",
                Some(1),
            ))
            .await
            .expect("episode");
        let ep2 = store
            .insert_item(&item(
                shows.id,
                ItemKind::Episode,
                Some(season),
                "100 ordinary",
                Some(2),
            ))
            .await
            .expect("episode");
        let home = store
            .create_library(&NewLibrary {
                name: "Personal".into(),
                kind: LibraryKind::Home,
                paths: vec![PathBuf::from("/personal")],
                anime: false,
            })
            .await
            .expect("home");
        store
            .insert_item(&item(home.id, ItemKind::Movie, None, "Private movie", None))
            .await
            .expect("home item");
        store
            .put_progress(uid, movie.id, 1234, Some(10000))
            .await
            .expect("progress");
        let mut q = query_for(&store, uid).await;
        let first = primed(&store, &q).await;
        assert_eq!(first.total, 5, "{backend}: personal library excluded");
        let projected = first
            .items
            .iter()
            .find(|i| i["native_id"] == movie.id)
            .expect("movie projection");
        assert_eq!(projected["position_ms"], 1234);
        assert_eq!(projected["sources"][0]["native_id"], file);
        assert!(projected["sources"][0].get("path").is_none());
        assert!(projected["sources"][0].get("probe_json").is_none());
        let season_wire = first
            .items
            .iter()
            .find(|i| i["native_id"] == season)
            .expect("season")["wire_id"]
            .as_str()
            .expect("wire")
            .to_owned();
        q.parent_wire = Some(season_wire.clone());
        q.recursive = false;
        q.sorts = vec![Sort::Index];
        q.start = 1;
        let second = primed(&store, &q).await;
        assert_eq!(second.total, 2);
        assert_eq!(second.items[0]["native_id"], ep2);
        assert!(second.items[0]["grandparent_wire"].is_string());
        q.start = 0;
        q.search = Some("%".into());
        let literal = primed(&store, &q).await;
        assert_eq!(literal.total, 1);
        assert_eq!(literal.items[0]["native_id"], ep1);
        q.search = None;
        q.token_hash = plurx_core::auth::hash_token("not-the-user-token");
        let denied = store
            .jellyfin_catalog_page(q.clone())
            .await
            .expect("denied snapshot");
        assert!(!denied.authorized);
        assert_eq!(denied.total, 0);
        assert!(denied.items.is_empty());
        q.token_hash = plurx_core::auth::hash_token("catalog-test-login");
        let private_parent = store
            .insert_item(&item(
                home.id,
                ItemKind::Show,
                None,
                "Private ancestor",
                None,
            ))
            .await
            .expect("private parent");
        let cross_library = store
            .insert_item(&item(
                movie.library_id,
                ItemKind::Movie,
                Some(private_parent),
                "Malformed public child",
                None,
            ))
            .await
            .expect("cross-library fixture");
        let cross_wire = store
            .jellyfin_entity_ids(Kind::Item, &[cross_library])
            .await
            .expect("cross wire")[0]
            .wire_id
            .clone();
        let mut cross_query = q.clone();
        cross_query.parent_wire = None;
        cross_query.item_wire = Some(cross_wire);
        let scoped = primed(&store, &cross_query).await;
        assert_eq!(scoped.total, 1);
        assert!(scoped.items[0]["parent_title"].is_null());
        assert!(scoped.items[0]["parent_wire"].is_null());
        let private_wire = store
            .jellyfin_entity_ids(Kind::Item, &[private_parent])
            .await
            .expect("private wire")[0]
            .wire_id
            .clone();
        cross_query.item_wire = None;
        cross_query.parent_wire = Some(private_wire);
        let denied_parent = store
            .jellyfin_catalog_page(cross_query)
            .await
            .expect("private parent query");
        assert!(!denied_parent.parent_valid);
        assert!(denied_parent.items.is_empty());
        assert!(store.delete_library(shows.id).await.expect("retire tree"));
        let retired = store
            .jellyfin_catalog_page(q.clone())
            .await
            .expect("retired snapshot");
        assert!(!retired.parent_valid);
        assert!(retired.items.is_empty());
        assert!(store.delete_user(uid).await.expect("retire user"));
        let replacement = store
            .create_user("replacement-catalog-user", "hash", false)
            .await
            .expect("replacement");
        let new_q = query_for(&store, replacement.id).await;
        assert_ne!(new_q.user_wire, q.user_wire);
        let old = store.jellyfin_catalog_page(q).await.expect("old scope");
        assert!(!old.authorized);
        assert!(old.items.is_empty());
        assert!(primed(&store, &new_q)
            .await
            .items
            .iter()
            .all(|i| i["position_ms"] == 0));
    })
    .await;
}

#[tokio::test]
async fn jellyfin_catalog_rails_filter_series_before_paging_and_share_native_next_episode() {
    use plurx_core::store::JellyfinCatalogMode as Mode;
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "catalog-rails").await;
        let movie = store
            .get_file(file)
            .await
            .expect("movie file")
            .expect("movie file")
            .item_id;
        store
            .put_progress(uid, movie, 1200, Some(10000))
            .await
            .expect("resume movie");
        let library = store
            .create_library(&NewLibrary {
                name: "Rail series".into(),
                kind: LibraryKind::Shows,
                paths: vec!["/rail-series".into()],
                anime: false,
            })
            .await
            .expect("shows");
        let mut target = None;
        // A series filter applied after an already-limited 500-row rail would
        // lose the last series. Native predicates run before facade paging.
        for n in 0..501 {
            let show = store
                .insert_item(&item(
                    library.id,
                    ItemKind::Show,
                    None,
                    &format!("Series {n:04}"),
                    None,
                ))
                .await
                .expect("show");
            let season = store
                .insert_item(&item(
                    library.id,
                    ItemKind::Season,
                    Some(show),
                    "Season",
                    Some(1),
                ))
                .await
                .expect("season");
            let watched = store
                .insert_item(&item(
                    library.id,
                    ItemKind::Episode,
                    Some(season),
                    "Episode one",
                    Some(1),
                ))
                .await
                .expect("episode");
            let following = store
                .insert_item(&item(
                    library.id,
                    ItemKind::Episode,
                    Some(season),
                    "Episode two",
                    Some(2),
                ))
                .await
                .expect("episode");
            store
                .set_watched(uid, watched, true)
                .await
                .expect("watched episode");
            if n == 500 {
                target = Some((show, following));
            }
        }
        let mut q = query_for(&store, uid).await;
        q.mode = Mode::Resume;
        q.sorts = vec![Sort::WatchUpdated];
        q.descending = true;
        let resume = primed(&store, &q).await;
        assert_eq!(resume.total, 1, "{backend}");
        assert_eq!(resume.items[0]["native_id"], movie);
        assert_eq!(resume.items[0]["position_ms"], 1200);
        let (show, following) = target.expect("target series");
        q.mode = Mode::NextUp;
        q.parent_wire = Some(
            store
                .jellyfin_entity_ids(Kind::Item, &[show])
                .await
                .expect("show identity")[0]
                .wire_id
                .clone(),
        );
        q.sorts = vec![Sort::SortName];
        q.descending = false;
        let next = primed(&store, &q).await;
        assert_eq!(next.total, 1, "{backend}");
        assert_eq!(next.items[0]["native_id"], following);
        let native = store.next_up(uid, 1000).await.expect("native next up");
        assert!(native.iter().any(|row| row.item.id == following));
        q.start = 1;
        let empty = primed(&store, &q).await;
        assert_eq!(empty.total, 1);
        assert!(empty.items.is_empty());
        store
            .put_progress(uid, following, 500, Some(10000))
            .await
            .expect("resume episode");
        q.start = 0;
        assert!(
            primed(&store, &q).await.items.is_empty(),
            "native next-up excludes a show with active resume"
        );
        q.mode = Mode::Resume;
        let resumed = primed(&store, &q).await;
        assert_eq!(resumed.total, 1);
        assert_eq!(resumed.items[0]["native_id"], following);
    })
    .await;
}

#[tokio::test]
async fn jellyfin_artwork_mapping_checks_switch_generation_supported_library_and_retirement() {
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "catalog-artwork").await;
        let movie = store
            .get_file(file)
            .await
            .expect("file")
            .expect("file")
            .item_id;
        store
            .apply_metadata(
                movie,
                &MetadataPatch {
                    poster_path: Some("1-poster-c123456789abcdef0.png".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("mapped poster");
        let ids = store
            .jellyfin_entity_ids(Kind::Item, &[movie])
            .await
            .expect("item ID");
        let wire = &ids[0].wire_id;
        assert!(store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("disabled image")
            .is_none());
        store
            .set_jellyfin_compatibility(true)
            .await
            .expect("enable");
        let image = store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("mapped image")
            .expect("mapped image");
        assert_eq!(image.wire_id, *wire, "{backend}");
        assert_eq!(image.filename, "1-poster-c123456789abcdef0.png");
        assert!(store
            .jellyfin_catalog_artwork(wire.clone(), true)
            .await
            .expect("missing backdrop")
            .is_none());
        store
            .set_jellyfin_compatibility(false)
            .await
            .expect("disable");
        assert!(store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("disabled image")
            .is_none());
        store
            .set_jellyfin_compatibility(true)
            .await
            .expect("reenable");
        let changed = store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("new image generation")
            .expect("image");
        assert_ne!(image.generation, changed.generation);
        let home = store
            .create_library(&NewLibrary {
                name: "Private artwork".into(),
                kind: LibraryKind::Home,
                paths: vec!["/private-art".into()],
                anime: false,
            })
            .await
            .expect("private library");
        let private = store
            .insert_item(&item(home.id, ItemKind::Movie, None, "Private", None))
            .await
            .expect("private item");
        store
            .apply_metadata(
                private,
                &MetadataPatch {
                    poster_path: Some("2-poster-c123456789abcdef0.png".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("private art");
        let private_wire = store
            .jellyfin_entity_ids(Kind::Item, &[private])
            .await
            .expect("private ID")[0]
            .wire_id
            .clone();
        assert!(store
            .jellyfin_catalog_artwork(private_wire, false)
            .await
            .expect("private art denied")
            .is_none());
        let q = query_for(&store, uid).await;
        let lib = primed(&store, &q)
            .await
            .items
            .iter()
            .find(|i| i["native_id"] == movie)
            .expect("movie")["library_wire"]
            .as_str()
            .expect("library wire")
            .to_owned();
        let native_lib = store
            .jellyfin_resolve_entity(Kind::Library, &lib)
            .await
            .expect("library")
            .expect("library");
        store
            .delete_library(native_lib)
            .await
            .expect("retire library");
        assert!(store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("retired image")
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn jellyfin_catalog_upcoming_and_similar_use_real_dates_genres_and_live_source_identity() {
    use plurx_core::store::JellyfinCatalogMode as Mode;
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "catalog-related").await;
        let movie = store
            .get_file(file)
            .await
            .expect("file")
            .expect("file")
            .item_id;
        let library = store
            .get_item(movie)
            .await
            .expect("item")
            .expect("item")
            .library_id;
        store
            .apply_metadata(
                movie,
                &MetadataPatch {
                    genres: Some(vec!["Adventure".into()]),
                    ..Default::default()
                },
            )
            .await
            .expect("source genre");
        let related = store
            .insert_item(&item(library, ItemKind::Movie, None, "Related", None))
            .await
            .expect("related movie");
        store
            .apply_metadata(
                related,
                &MetadataPatch {
                    genres: Some(vec!["Adventure".into()]),
                    ..Default::default()
                },
            )
            .await
            .expect("related genre");
        store
            .insert_item(&item(library, ItemKind::Movie, None, "No metadata", None))
            .await
            .expect("unknown genre");
        let mut q = query_for(&store, uid).await;
        q.mode = Mode::Similar;
        q.item_wire = Some(
            store
                .jellyfin_entity_ids(Kind::Item, &[movie])
                .await
                .expect("source identity")[0]
                .wire_id
                .clone(),
        );
        let similar = primed(&store, &q).await;
        assert_eq!(similar.total, 1, "{backend}");
        assert_eq!(similar.items[0]["native_id"], related);
        q.item_wire = Some(uuid::Uuid::new_v4().simple().to_string());
        let missing = primed(&store, &q).await;
        assert!(!missing.parent_valid);
        assert!(missing.items.is_empty());
        let shows = store
            .create_library(&NewLibrary {
                name: "Dated episodes".into(),
                kind: LibraryKind::Shows,
                paths: vec!["/dated".into()],
                anime: false,
            })
            .await
            .expect("show library");
        let show = store
            .insert_item(&item(shows.id, ItemKind::Show, None, "Dated show", None))
            .await
            .expect("show");
        let season = store
            .insert_item(&item(
                shows.id,
                ItemKind::Season,
                Some(show),
                "Season",
                Some(1),
            ))
            .await
            .expect("season");
        let mut future = None;
        for (n, date) in [(1, Some("2026-10-01")), (2, Some("2026-10-03")), (3, None)] {
            let episode = store
                .insert_item(&item(
                    shows.id,
                    ItemKind::Episode,
                    Some(season),
                    "Episode",
                    Some(n),
                ))
                .await
                .expect("episode");
            if let Some(date) = date {
                store
                    .apply_metadata(
                        episode,
                        &MetadataPatch {
                            air_date: Some(date.into()),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("air date");
            }
            if n == 2 {
                future = Some(episode);
            }
        }
        q.item_wire = None;
        q.mode = Mode::Upcoming;
        q.today = Some("2026-10-03".into());
        q.sorts = vec![Sort::PremiereDate];
        let upcoming = primed(&store, &q).await;
        assert_eq!(upcoming.total, 1);
        assert_eq!(
            upcoming.items[0]["native_id"],
            future.expect("dated target")
        );
        q.today = None;
        assert!(store.jellyfin_catalog_page(q).await.is_err());
    })
    .await;
}
