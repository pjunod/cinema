use super::*;
use plurx_core::domain::{LibraryKind, NewLibrary};
use plurx_core::store::background_jobs_resources::{
    ForegroundSourceOwner, ForegroundSourcePolicy, StorageDomainMapping,
};

const PATH: &str = "/api/v1/cluster/work/storage-domains";

#[tokio::test]
async fn storage_domains_save_reports_pending_readers_and_rejects_stale_roots_as_input() {
    let (app, state) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/storage-domains".into()],
            anime: false,
        })
        .await
        .expect("library");
    // A synthetic held foreground registration is sufficient here: this route
    // observes the Store's budget, and never infers physical reader closure.
    let owner = ForegroundSourceOwner {
        owner_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "http-domains".into(),
        session_incarnation: uuid::Uuid::new_v4().to_string(),
        producer_attempt: "reader".into(),
        node_id: "reader-node".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        file_id: 1,
        source_size: 1,
        source_mtime: 1,
        source_generation: "http-source".into(),
        policy: ForegroundSourcePolicy::Rolling,
    };
    let mapping = |domain: &str| {
        vec![StorageDomainMapping {
            library_id: library.id,
            root_path: "/storage-domains".into(),
            domain_id: domain.into(),
        }]
    };
    let (status, first) = call(&app, put(PATH, Some(&admin), json!(mapping("nas-a")))).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["applied"], true);
    // Register an owner through an actual catalogue file so derivation uses
    // the same library-wide roots as a production playback owner.
    let item = state
        .store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "Domain movie".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let file_id = state
        .store
        .upsert_file(
            item,
            "/storage-domains/movie.mkv",
            1,
            1,
            &plurx_core::domain::ProbeResult::default(),
        )
        .await
        .expect("file");
    let now = crate::state::clock_ms();
    let held = state
        .store
        .register_foreground_source(
            ForegroundSourceOwner { file_id, ..owner },
            now,
            now + 60_000,
        )
        .await
        .expect("registration")
        .expect("held reader");
    let (status, saved) = call(&app, put(PATH, Some(&admin), json!(mapping("nas-b")))).await;
    assert_eq!(status, StatusCode::OK, "busy save must succeed: {saved}");
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["applied"], false);
    assert_eq!(saved["pending"]["state"], "pending");
    assert_eq!(saved["waiting_total"], 1);
    assert_eq!(saved["waiting_on"][0]["reason"], "old_domain_reader");
    // Invalid roots are a 400 input error, never a 409 busy-state refusal.
    let (status, _) = call(
        &app,
        put(
            PATH,
            Some(&admin),
            json!([{
                "library_id":library.id,"root_path":"/removed","domain_id":"nas-b"
            }]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, current) = call(&app, get(PATH, Some(&admin))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["pending"]["state"], "pending");
    assert_eq!(current["pending"]["mappings"], json!(mapping("nas-b")));
    assert_eq!(current["mappings"], json!(mapping("nas-a")));
    assert_eq!(
        call(&app, get(PATH, None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(state
        .store
        .release_foreground_source(held, now + 1)
        .await
        .expect("release"));
    state
        .store
        .apply_pending_storage_domains(now + 1)
        .await
        .expect("apply after release");
    let (status, applied) = call(&app, get(PATH, Some(&admin))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(applied["pending"]["state"], "applied");
    assert_eq!(applied["mappings"], json!(mapping("nas-b")));
}
