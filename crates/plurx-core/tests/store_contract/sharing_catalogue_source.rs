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
    let identity = store.sharing_identity(1000).await.expect("source identity");
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
    let library_id = SourceId::parse(&libraries[0].to_string()).expect("ID");
    let item_id = SourceId::parse(&first.to_string()).expect("ID");
    let authority = || {
        store.source_content_authorized(
            grant,
            identity.server_id,
            identity.catalogue_epoch,
            std::slice::from_ref(&library_id),
            &[],
        )
    };
    assert!(authority().await.expect("current library authority"));
    assert!(!store
        .source_content_authorized(
            grant,
            Uuid::new_v4(),
            identity.catalogue_epoch,
            std::slice::from_ref(&library_id),
            &[]
        )
        .await
        .expect("source identity mismatch"));
    let items = [(library_id.clone(), item_id)];
    assert!(store
        .source_content_authorized(
            grant,
            identity.server_id,
            identity.catalogue_epoch,
            &[],
            &items
        )
        .await
        .expect("current item authority"));
    client
        .execute(
            "UPDATE items SET library_id=$1 WHERE id=$2",
            hiqlite::params!(libraries[1], first),
        )
        .await
        .expect("move out of effective scope");
    assert!(!store
        .source_content_authorized(
            grant,
            identity.server_id,
            identity.catalogue_epoch,
            &[],
            &items
        )
        .await
        .expect("current item move refuses"));
    client
        .execute(
            "UPDATE items SET library_id=$1 WHERE id=$2",
            hiqlite::params!(libraries[0], first),
        )
        .await
        .expect("restore fixture item");
    client
        .execute(
            "UPDATE item_identity_watermark SET importing=1 WHERE singleton=1",
            hiqlite::params!(),
        )
        .await
        .expect("internal import fixture");
    assert!(
        authority().await.is_err(),
        "accepted content refuses import mode"
    );
    client
        .execute(
            "UPDATE item_identity_watermark SET importing=0 WHERE singleton=1",
            hiqlite::params!(),
        )
        .await
        .expect("restore qualified layout");
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
    assert!(!authority().await.expect("accepted library revoked"));
    assert!(!store
        .source_content_authorized(
            grant,
            identity.server_id,
            identity.catalogue_epoch,
            &[],
            &items
        )
        .await
        .expect("accepted item revoked"));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_catalogue_revision_key_census_three_voters_refuses_partial_and_foreign_state() {
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
    .expect("candidate client");
    assert_eq!(
        store
            .sharing_sealed_census()
            .await
            .expect("legacy absence")
            .sealed_rows(),
        0
    );
    let identity = store.sharing_identity(1000).await.expect("source identity");
    let key = plurx_core::secrets::CredentialKey::from_bytes([31; 32]);
    let envelope = key
        .seal_sharing(
            plurx_core::secrets::SharingSecretPurpose::CatalogueRevision,
            identity.server_id,
            identity.catalogue_epoch,
            "synthetic-stable-purpose-key",
        )
        .expect("sealed purpose key");
    client
        .execute(
            plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            hiqlite::params!(),
        )
        .await
        .expect("candidate table only");
    client
        .execute(
            "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)",
            hiqlite::params!(
                identity.server_id.to_string(),
                identity.catalogue_epoch.to_string(),
                envelope.as_stored()
            ),
        )
        .await
        .expect("fixture key seed");
    assert_eq!(
        store
            .sharing_sealed_census()
            .await
            .expect("consistent purpose census")
            .sealed_rows(),
        1
    );
    client
        .execute(
            "UPDATE sharing_catalogue_keys SET server_id=$1",
            hiqlite::params!(Uuid::new_v4().to_string()),
        )
        .await
        .expect("foreign source corruption");
    assert!(store.sharing_sealed_census().await.is_err());
    client
        .execute(
            "UPDATE sharing_catalogue_keys SET server_id=$1,revision_envelope=$2",
            hiqlite::params!(identity.server_id.to_string(), "x".repeat(4097)),
        )
        .await
        .expect("oversized corruption");
    assert!(store.sharing_sealed_census().await.is_err());
    for sql in ["DROP TABLE sharing_catalogue_keys",
        "CREATE TABLE sharing_catalogue_keys(singleton INTEGER,server_id TEXT,catalogue_epoch TEXT)"] {
        client.execute(sql,hiqlite::params!()).await.expect("partial table fixture");
    }
    assert!(store.sharing_sealed_census().await.is_err());
    for sql in ["DROP TABLE sharing_catalogue_keys",
        "CREATE TABLE sharing_catalogue_keys(singleton INTEGER NOT NULL PRIMARY KEY,server_id TEXT NOT NULL,catalogue_epoch TEXT NOT NULL,revision_envelope TEXT NOT NULL)"] {
        client.execute(sql,hiqlite::params!()).await.expect("excess shape fixture");
    }
    client
        .execute(
            "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3),(2,$1,$2,$3)",
            hiqlite::params!(
                identity.server_id.to_string(),
                identity.catalogue_epoch.to_string(),
                envelope.as_stored()
            ),
        )
        .await
        .expect("excess row fixture");
    assert!(store.sharing_sealed_census().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_catalogue_file_witness_three_voters_binds_current_file_and_refuses_capacity() {
    use plurx_core::{
        secrets::CredentialKey,
        sharing_catalogue_details::CatalogueRevisionKey,
        store::{sharing_catalogue_details::SourceDetailsRead, SharingSourceDetailsStore},
    };
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
    .expect("snapshot client");
    let identity = store.sharing_identity(1000).await.expect("source identity");
    let library = store
        .create_library(&NewLibrary {
            name: "Source files".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/synthetic")],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    let grant = Uuid::new_v4();
    let invitation = Uuid::new_v4();
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![library],
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
            recipient_name: "synthetic".into(),
            credential_hash: "b".repeat(64),
            now_ms: 1001,
        })
        .await
        .expect("claim");
    store.approve_share(grant, 1, 1002).await.expect("approve");
    for result in client
        .txn(
            candidate_statements()
                .into_iter()
                .chain(candidate_item_identity_statements())
                .map(|sql| (sql, hiqlite::params!()))
                .collect::<Vec<_>>(),
        )
        .await
        .expect("candidate layout")
    {
        result.expect("layout statement");
    }
    client.execute("INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) VALUES(9007199254740993,$1,'movie','Source movie','movie',1000,1000)",hiqlite::params!(library)).await.expect("durable movie");
    client.execute("INSERT INTO files(id,item_id,path,size,mtime,probe_json,scanned_at) VALUES(9223372036854775807,9007199254740993,'/private/synthetic.mkv',20,1000,'synthetic exact probe',1000)",hiqlite::params!()).await.expect("file fixture");
    let sealing = CredentialKey::from_bytes([17; 32]);
    let envelope =
        CatalogueRevisionKey::generate_sealed(&sealing, identity.clone()).expect("purpose key");
    let key = CatalogueRevisionKey::open(&sealing, identity.clone(), &envelope)
        .expect("purpose material");
    client
        .execute("UPDATE files SET probe_json='{}'", hiqlite::params!())
        .await
        .expect("closed valid probe");
    let item = SourceId::parse("9007199254740993").expect("item");
    let SourceDetailsRead::Authorized(details) = store
        .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
        .await
        .expect("consistent quorum details")
    else {
        panic!("details authorized")
    };
    assert_eq!(details.files.len(), 1);
    let facts = details.files[0]
        .playable_file(&key)
        .expect("closed replicated facts");
    assert_eq!(facts.file_id.as_str(), "9223372036854775807");
    assert!(!serde_json::to_string(&facts)
        .expect("wire facts")
        .contains("/private/"));
    let tuples = vec![(details.record.item.library_id, item.clone(), facts.file_id)];
    assert!(store
        .source_content_files_authorized(
            grant,
            identity.server_id,
            identity.catalogue_epoch,
            &tuples
        )
        .await
        .expect("quorum file tuples"));
    client.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<64) INSERT INTO files(id,item_id,path,size,mtime,scanned_at) SELECT x,9007199254740993,'/synthetic/'||x,20,1000,1000 FROM n",hiqlite::params!()).await.expect("65 files");
    assert!(matches!(
        store
            .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
            .await
            .expect("quorum file cap"),
        SourceDetailsRead::Capacity
    ));
    client
        .execute("DELETE FROM files WHERE id=64", hiqlite::params!())
        .await
        .expect("64 files");
    let SourceDetailsRead::Authorized(details) = store
        .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
        .await
        .expect("64 files admitted")
    else {
        panic!("bounded details")
    };
    assert_eq!(details.files.len(), 64);
    client
        .execute(
            "UPDATE files SET probe_json=$1 WHERE id<=8",
            hiqlite::params!(serde_json::to_string(&"x".repeat(1048574)).expect("private bound")),
        )
        .await
        .expect("aggregate fixture");
    assert!(matches!(
        store
            .source_item_details_snapshot(&"b".repeat(64), grant, item)
            .await
            .expect("quorum aggregate bound"),
        SourceDetailsRead::Capacity
    ));
    client
        .execute(
            "DELETE FROM files WHERE id<9223372036854775807",
            hiqlite::params!(),
        )
        .await
        .expect("restore bounded file");
    let credential_hash = "b".repeat(64);
    let read = || {
        store.source_item_file_witness(
            &credential_hash,
            grant,
            SourceId::parse("9007199254740993").expect("item"),
            SourceId::parse("9223372036854775807").expect("file"),
        )
    };
    let initial = match read().await.expect("current quorum snapshot") {
        SourceDetailsRead::Authorized(w) => key.file_revision(&w).expect("revision"),
        _ => panic!("authorized file expected"),
    };
    client
        .execute(
            "UPDATE files SET path='/private/changed.mkv'",
            hiqlite::params!(),
        )
        .await
        .expect("file replacement");
    let changed = match read().await.expect("fresh file snapshot") {
        SourceDetailsRead::Authorized(w) => key.file_revision(&w).expect("new revision"),
        _ => panic!("current file expected"),
    };
    assert_ne!(initial, changed);
    client
        .execute(
            "UPDATE files SET probe_json=$1,downloaded_subtitles='malformed private JSON'",
            hiqlite::params!("x".repeat(1048577)),
        )
        .await
        .expect("oversized private snapshot");
    assert!(matches!(
        read().await.expect("quorum capacity short circuit"),
        SourceDetailsRead::Capacity
    ));
    store
        .share_scope(grant, 2, vec![], 1010)
        .await
        .expect("scope revoke");
    assert!(matches!(
        read().await.expect("current scope refuses"),
        SourceDetailsRead::Unavailable
    ));
}
