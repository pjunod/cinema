use super::*;
use plurx_core::store::JellyfinEntityKind as Kind;
#[cfg(feature = "hiqlite-contract-tests")]
use plurx_core::store::JellyfinIdentityStore;

#[tokio::test]
async fn jellyfin_ids_converge_retire_cascades_and_survive_integer_reuse() {
    for_each_backend(|store, backend| async move {
        let (user_id, file_id) = seed_file(&store, "jellyfin-id").await;
        let file = store
            .get_file(file_id)
            .await
            .expect("identity contract operation")
            .expect("identity contract operation");
        let item = store
            .get_item(file.item_id)
            .await
            .expect("identity contract operation")
            .expect("identity contract operation");
        let child = store
            .insert_item(&NewItem {
                library_id: item.library_id,
                kind: ItemKind::Episode,
                parent_id: Some(item.id),
                title: "Child".into(),
                year: None,
                season_number: Some(1),
                episode_number: Some(1),
            })
            .await
            .expect("identity contract operation");
        let child_file = store
            .upsert_file(
                child,
                "/jellyfin-id/child.mkv",
                10,
                1,
                &ProbeResult::default(),
            )
            .await
            .expect("identity contract operation");
        let left_ids = [file_id, child_file];
        let right_ids = [child_file, file_id, file_id];
        let (a, b) = tokio::join!(
            store.jellyfin_entity_ids(Kind::File, &left_ids),
            store.jellyfin_entity_ids(Kind::File, &right_ids)
        );
        let files = a.expect("identity contract operation");
        assert_eq!(
            files,
            b.expect("identity contract operation"),
            "{backend}: concurrent allocators return the winner"
        );
        assert_eq!(files.len(), 2);
        assert_eq!(
            store
                .jellyfin_entity_ids(Kind::File, &[file_id, child_file])
                .await
                .expect("identity contract operation"),
            files,
            "{backend}: repeated sync preserves IDs"
        );
        assert!(store
            .jellyfin_entity_ids(Kind::File, &[i64::MAX])
            .await
            .expect("identity contract operation")
            .is_empty());
        assert!(store.jellyfin_entity_ids(Kind::File, &[0]).await.is_err());
        assert!(store
            .jellyfin_entity_ids(Kind::File, &vec![1; 1001])
            .await
            .is_err());
        let items = store
            .jellyfin_entity_ids(Kind::Item, &[item.id, child])
            .await
            .expect("identity contract operation");
        let libraries = store
            .jellyfin_entity_ids(Kind::Library, &[item.library_id])
            .await
            .expect("identity contract operation");
        let old_library_wire = libraries[0].wire_id.clone();
        let users = store
            .jellyfin_entity_ids(Kind::User, &[user_id])
            .await
            .expect("identity contract operation");
        for id in files
            .iter()
            .chain(items.iter())
            .chain(libraries.iter())
            .chain(users.iter())
        {
            assert_eq!(id.wire_id.len(), 32);
            assert_ne!(
                uuid::Uuid::parse_str(&id.wire_id).expect("identity contract operation"),
                uuid::Uuid::nil()
            );
        }
        // Native file rescan changes source facts without replacing identity.
        assert_eq!(
            store
                .upsert_file(
                    item.id,
                    file.path.to_str().expect("identity contract operation"),
                    file.size + 1,
                    file.mtime + 1,
                    &ProbeResult::default()
                )
                .await
                .expect("identity contract operation"),
            file_id
        );
        assert_eq!(
            store
                .jellyfin_entity_ids(Kind::File, &[file_id, child_file])
                .await
                .expect("identity contract operation"),
            files
        );
        assert!(store
            .delete_library(item.library_id)
            .await
            .expect("identity contract operation"));
        for (kind, ids) in [
            (Kind::Library, libraries),
            (Kind::Item, items),
            (Kind::File, files),
        ] {
            for id in ids {
                assert_eq!(
                    store
                        .jellyfin_resolve_entity(kind, &id.wire_id)
                        .await
                        .expect("identity contract operation"),
                    None,
                    "{backend}: cascade retires every descendant"
                );
            }
        }
        // Highest INTEGER PRIMARY KEY can be reused, but its old wire ID cannot.
        let library = store
            .create_library(&NewLibrary {
                name: "Replacement".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/replacement")],
                anime: false,
            })
            .await
            .expect("identity contract operation");
        assert_eq!(
            library.id, item.library_id,
            "{backend}: exercise actual integer reuse"
        );
        let new_id = store
            .jellyfin_entity_ids(Kind::Library, &[library.id])
            .await
            .expect("identity contract operation");
        assert_ne!(new_id[0].wire_id, old_library_wire);
        assert_eq!(
            store
                .jellyfin_resolve_entity(Kind::Library, &old_library_wire)
                .await
                .expect("identity contract operation"),
            None
        );
        assert_eq!(
            store
                .jellyfin_resolve_entity(Kind::Library, &new_id[0].wire_id)
                .await
                .expect("identity contract operation"),
            Some(library.id)
        );
        assert_eq!(
            store
                .jellyfin_resolve_entity(Kind::User, &users[0].wire_id)
                .await
                .expect("identity contract operation"),
            Some(user_id)
        );
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_import_preserves_live_and_retired_ids_on_another_node() {
    let _case = HIQLITE_CASE.lock().await;
    let source = tempfile::tempdir().expect("identity contract operation");
    let path = source
        .path()
        .join(plurx_core::cluster::migration::SQLITE_FILENAME);
    let local = SqliteStore::open(&path).expect("identity contract operation");
    local
        .put_setting(plurx_core::store::keys::INSTANCE_ID, CONTRACT_INSTANCE_ID)
        .await
        .expect("identity contract operation");
    let original = local
        .create_user("original", "hash", false)
        .await
        .expect("identity contract operation");
    let old = local
        .jellyfin_entity_ids(Kind::User, &[original.id])
        .await
        .expect("identity contract operation");
    local
        .delete_user(original.id)
        .await
        .expect("identity contract operation");
    let replacement = local
        .create_user("replacement", "hash", false)
        .await
        .expect("identity contract operation");
    assert_eq!(original.id, replacement.id);
    let current = local
        .jellyfin_entity_ids(Kind::User, &[replacement.id])
        .await
        .expect("identity contract operation");
    drop(local);
    let prepared = prepare_sqlite_import(source.path()).expect("identity contract operation");
    let cluster = ContractCluster::start().await;
    let replicated = open_contract_hiqlite_store(&cluster).await;
    replicated
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("identity contract operation");
    assert_eq!(
        replicated
            .jellyfin_entity_ids(Kind::User, &[replacement.id])
            .await
            .expect("identity contract operation"),
        current
    );
    drop(replicated);
    let mut addresses = cluster.addresses.clone();
    addresses.rotate_left(1);
    let client = Client::remote(
        addresses,
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("identity contract operation");
    let reopened = HiqliteAuthStore::open(
        client,
        &cluster._root.path().join("jellyfin-alternate-telemetry.db"),
    )
    .await
    .expect("identity contract operation");
    assert_eq!(
        reopened
            .jellyfin_resolve_entity(Kind::User, &old[0].wire_id)
            .await
            .expect("identity contract operation"),
        None
    );
    assert_eq!(
        reopened
            .jellyfin_resolve_entity(Kind::User, &current[0].wire_id)
            .await
            .expect("identity contract operation"),
        Some(replacement.id)
    );
}
