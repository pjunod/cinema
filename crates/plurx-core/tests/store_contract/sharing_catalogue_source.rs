//! Production source readers against real three-voter candidate order maintenance.
use super::*;
use plurx_core::{
    sharing::*,
    sharing_catalogue::*,
    store::sharing_catalogue_source::{candidate_item_identity_statements, candidate_statements},
    store::{SharingSourceCatalogueStore, SharingStore},
};
use uuid::Uuid;
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_catalogue_allocator_three_voters_preserves_import_ids_and_fenced_identity() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("fresh import target");
    let client = hiqlite::Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("candidate client");
    for result in client
        .txn(
            candidate_statements()
                .into_iter()
                .chain(candidate_item_identity_statements())
                .map(|sql| (sql, hiqlite::params!()))
                .collect::<Vec<_>>(),
        )
        .await
        .expect("atomic candidate")
    {
        result.expect("candidate statement");
    }
    let source = tempfile::tempdir().expect("legacy source");
    populated_v14_import_fixture(source.path());
    let source_connection =
        rusqlite::Connection::open(source.path().join("plurx.db")).expect("source allocator");
    source_connection
        .execute_batch(plurx_core::store::sharing_catalogue_source::CANDIDATE_ITEM_IDENTITY_SCHEMA)
        .expect("source candidate allocator");
    source_connection
        .execute(
            "UPDATE item_identity_watermark SET high_water=1000 WHERE singleton=1",
            [],
        )
        .expect("deleted source identities remain spent");
    drop(source_connection);
    let prepared = prepare_sqlite_import(source.path()).expect("legacy archive");
    store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("actual legacy parity import under candidate allocator");
    let library = store
        .create_library(&NewLibrary {
            name: "Allocator movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/synthetic")],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    let item = NewItem {
        library_id: library,
        kind: ItemKind::Movie,
        parent_id: None,
        title: "Allocated movie".into(),
        year: None,
        season_number: None,
        episode_number: None,
    };
    let old = store
        .insert_item(&item)
        .await
        .expect("ordinary allocation after import");
    assert_eq!(
        old, 1001,
        "import preserves watermark above all live source IDs"
    );
    client
        .execute("DELETE FROM items WHERE id=$1", hiqlite::params!(old))
        .await
        .expect("delete highest item");
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    let lease = acquired(
        store
            .acquire_lease("scan:allocator", "node-a", clock, clock + 90000)
            .await
            .expect("lease"),
        "Hiqlite",
    );
    let replacement = publication_successor(&lease);
    let new = store
        .insert_item_fenced(&item, &lease, &replacement)
        .await
        .expect("fenced monotone allocation");
    assert_eq!(new, old + 1);
    assert!(
        store
            .insert_item_fenced(&item, &lease, &replacement)
            .await
            .is_err(),
        "stale fence must not allocate"
    );
    let explicit=client.execute("INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) VALUES($1,$2,'movie','Old explicit writer','old',1000,1000)",hiqlite::params!(old,library)).await.expect_err("old explicit writer cannot reuse deleted ID");
    assert!(
        explicit.to_string().contains("item_identity_reuse"),
        "{explicit}"
    );
    let implicit=client.execute("INSERT INTO items(library_id,kind,title,sort_title,added_at,updated_at) VALUES($1,'movie','Old implicit writer','old',1000,1000)",hiqlite::params!(library)).await.expect_err("old implicit writer is fenced");
    assert!(
        implicit.to_string().contains("item_identity_reuse"),
        "{implicit}"
    );
    let (a, b) = tokio::join!(store.insert_item(&item), store.insert_item(&item));
    let a = a.expect("first concurrent allocation");
    let b = b.expect("second concurrent allocation");
    assert_ne!(a, b);
    assert_eq!(a.min(b), new + 1);
    assert_eq!(a.max(b), new + 2);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_catalogue_source_three_voters_refuse_removed_scope_and_preserve_live_boundaries() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    let client = hiqlite::Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("candidate migration client");
    let mut libraries = Vec::new();
    for name in ["Exported source movies", "Private source movies"] {
        libraries.push(
            store
                .create_library(&NewLibrary {
                    name: name.into(),
                    kind: LibraryKind::Movies,
                    paths: vec![PathBuf::from("/synthetic")],
                    anime: false,
                })
                .await
                .expect("library")
                .id,
        );
    }
    store.sharing_identity(1000).await.expect("source identity");
    for result in client
        .txn(
            candidate_statements()
                .into_iter()
                .chain(candidate_item_identity_statements())
                .map(|sql| (sql, hiqlite::params!()))
                .collect::<Vec<_>>(),
        )
        .await
        .expect("atomic candidate maintenance")
    {
        result.expect("candidate statement");
    }
    let invitation = Uuid::new_v4();
    let grant = Uuid::new_v4();
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![libraries[0]],
            created_at_ms: 1000,
            expires_at_ms: 2000,
        })
        .await
        .expect("invite");
    store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: Uuid::new_v4(),
            recipient_name: "Synthetic recipient".into(),
            credential_hash: "b".repeat(64),
            now_ms: 1001,
        })
        .await
        .expect("claim");
    store.approve_share(grant, 1, 1002).await.expect("approve");
    let first = 9_007_199_254_740_993_i64;
    let mut statements: Vec<(String, hiqlite::Params)> = Vec::new();
    for n in 0..150 {
        statements.push((
            "INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) VALUES($1,$2,'movie',$3,$4,1000,1000)"
                .into(),
            hiqlite::params!(
                first + n,
                libraries[0],
                format!("Movie {n:03}"),
                format!("{n:03}")
            ),
        ));
    }
    statements.push(("INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) VALUES($1,$2,'movie','Private metadata','private',1000,1000)".into(),hiqlite::params!(first+1000,libraries[1])));
    for result in client
        .txn(statements)
        .await
        .expect("finite replicated fixture")
    {
        result.expect("fixture insert");
    }
    let mut request = CataloguePageRequest {
        credential_hash: "b".repeat(64),
        grant_id: grant,
        library_id: SourceId::parse(&libraries[0].to_string()).expect("ID"),
        parent_id: None,
        query: String::new(),
        boundary: None,
        limit: 60,
    };
    let page = store
        .source_catalogue_page(request.clone())
        .await
        .expect("consistent page")
        .expect("authorized");
    assert_eq!(page.records.len(), 60);
    assert!(page.has_more);
    assert_eq!(page.records[0].item.item_id.as_str(), first.to_string());
    let last = page.records.last().expect("boundary");
    request.boundary = Some(CatalogueBoundary {
        sort_key: last.boundary_sort_key.clone(),
        item_id: last.item.item_id.clone(),
    });
    client
        .execute(
            "UPDATE items SET overview='Continuous metadata change' WHERE library_id=$1",
            hiqlite::params!(libraries[0]),
        )
        .await
        .expect("metadata-only write");
    let next = store
        .source_catalogue_page(request.clone())
        .await
        .expect("live page")
        .expect("allowed");
    assert_eq!(
        next.counters.library_revision,
        page.counters.library_revision
    );
    assert_eq!(
        next.records[0].item.item_id.as_str(),
        (first + 60).to_string()
    );
    client
        .execute(
            "UPDATE items SET sort_title='999' WHERE id=$1",
            hiqlite::params!(first),
        )
        .await
        .expect("sort move");
    let moved = store
        .source_catalogue_page(request.clone())
        .await
        .expect("resume under current order")
        .expect("still allowed");
    assert!(moved.counters.library_revision > page.counters.library_revision);
    assert_eq!(
        moved.records[0].item.item_id.as_str(),
        (first + 60).to_string()
    );
    let results = store
        .source_catalogue_batch(
            &"b".repeat(64),
            grant,
            MetadataBatch {
                item_ids: vec![
                    SourceId::parse(&first.to_string()).expect("ID"),
                    SourceId::parse(&(first + 1000).to_string()).expect("ID"),
                ],
            },
        )
        .await
        .expect("consistent batch")
        .expect("active grant");
    assert!(results[0].record.is_some());
    assert!(results[1].record.is_none());
    store
        .share_scope(grant, 2, vec![], 1010)
        .await
        .expect("commit scope removal");
    assert!(store
        .source_catalogue_page(request)
        .await
        .expect("quorum scope revalidation")
        .is_none());
    assert!(store
        .source_catalogue_batch(
            &"b".repeat(64),
            grant,
            MetadataBatch {
                item_ids: vec![SourceId::parse(&first.to_string()).expect("ID")]
            }
        )
        .await
        .expect("batch removal")
        .expect("active narrowed grant")[0]
        .record
        .is_none());
}
