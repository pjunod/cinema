use super::*;
use plurx_core::resource_settings::{CapacityUpdate, LimitChoice, ReadState};
use plurx_core::store::{keys, CapacityInstallation};

async fn seed_planning_file(store: &dyn Store) -> i64 {
    let library = store
        .create_library(&NewLibrary {
            name: "resource planning".into(),
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
            title: "resource planning".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    store
        .upsert_file(
            item,
            "/planning/resource.mkv",
            100,
            42,
            &ProbeResult {
                raw_json: Some(r#"{"source":"exact"}"#.into()),
                ..Default::default()
            },
        )
        .await
        .expect("source")
}

#[tokio::test]
async fn planning_snapshot_binds_installed_capacity_and_preserves_automatic_rollback_cache() {
    for_each_backend(|store, backend| async move {
        let file = seed_planning_file(store.as_ref()).await;
        store
            .put_settings(&[(keys::MAX_HW_SESSIONS, "0"), (keys::SW_POOL_THREADS, "0")])
            .await
            .expect("legacy zeros");
        let before = store
            .playback_planning_snapshot(file, &[])
            .await
            .expect("snapshot")
            .expect("file");
        assert!(
            matches!(
                before.capacity.installation,
                CapacityInstallation::Ready { .. }
            ),
            "{backend}"
        );
        assert_eq!(
            before.capacity.saved_modes.hardware_sessions.state,
            ReadState::LegacyAmbiguous
        );
        assert_eq!(
            before
                .capacity
                .admission_value(&before.settings, keys::MAX_HW_SESSIONS),
            Some("0")
        );
        let revision = store
            .resource_capacity_snapshot()
            .await
            .expect("capacity")
            .expect("installed")
            .revision;
        assert!(store
            .put_resource_capacity(&CapacityUpdate {
                expected_revision: revision,
                hardware_sessions: Some(LimitChoice::Automatic),
                software_pool_threads: Some(LimitChoice::Explicit { value: 0 })
            })
            .await
            .expect("choose modes"));
        let selected = store
            .playback_planning_snapshot(file, &[])
            .await
            .expect("snapshot")
            .expect("file");
        assert_eq!(
            selected.settings[keys::MAX_HW_SESSIONS],
            "0",
            "{backend}: raw rollback cache retained"
        );
        assert!(selected
            .settings
            .contains_key(keys::MAX_HW_SESSIONS_PROVENANCE));
        assert!(selected
            .settings
            .contains_key(keys::SW_POOL_THREADS_PROVENANCE));
        assert_eq!(
            selected.capacity.saved_modes.hardware_sessions.state,
            ReadState::Automatic
        );
        assert_eq!(
            selected.capacity.verified_modes.hardware_sessions.state,
            ReadState::Automatic
        );
        assert_eq!(
            selected
                .capacity
                .admission_value(&selected.settings, keys::MAX_HW_SESSIONS),
            None
        );
        assert_eq!(
            selected
                .capacity
                .admission_value(&selected.settings, keys::SW_POOL_THREADS),
            Some("0")
        );
        assert_ne!(selected.capacity.binding, before.capacity.binding);
        assert!(selected.generation > before.generation);
        assert_eq!(selected.probe_json, before.probe_json);
        assert_eq!(selected.item_title, before.item_title);
        store
            .put_setting(keys::MAX_HW_SESSIONS, "0")
            .await
            .expect("legacy same-value writer");
        let legacy = store
            .playback_planning_snapshot(file, &[])
            .await
            .expect("snapshot")
            .expect("file");
        assert_eq!(
            legacy.capacity.verified_modes.hardware_sessions.state,
            ReadState::LegacyAmbiguous
        );
        assert_eq!(
            legacy
                .capacity
                .admission_value(&legacy.settings, keys::MAX_HW_SESSIONS),
            Some("0")
        );
        assert_ne!(legacy.capacity.binding, selected.capacity.binding);
        assert_eq!(
            selected.capacity.verified_modes.hardware_sessions.state,
            ReadState::Automatic,
            "{backend}: earlier frozen input is unchanged"
        );
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_read_step_failure_is_error_not_absence_or_partial_rows() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("remote three-voter client");
    // UNION ALL evaluates its arms in order, without an ORDER BY/materializing
    // operation. The second arm overflows at sqlite3_step, AFTER the first row
    // has been produced. No Result may contain that partial first row.
    let sql = "SELECT 7 AS value UNION ALL SELECT abs(-9223372036854775808) AS value";
    let local = rusqlite::Connection::open_in_memory().expect("SQL evaluation control");
    let mut prepared = local.prepare(sql).expect("must prepare successfully");
    let mut rows = prepared.query([]).expect("query opens");
    assert_eq!(
        rows.next()
            .expect("first step")
            .expect("first row")
            .get::<_, i64>(0)
            .expect("integer"),
        7
    );
    assert!(
        rows.next().is_err(),
        "fixture really fails during second step"
    );
    assert!(
        client
            .query_consistent(sql, hiqlite::params!())
            .await
            .is_err(),
        "owned remote read cannot return partial rows"
    );
    assert!(
        client
            .query_consistent_map::<I64Value, _>(sql, hiqlite::params!())
            .await
            .is_err(),
        "mapped remote read cannot return partial rows"
    );
    assert!(
        client
            .query_map::<I64Value, _>(sql, hiqlite::params!())
            .await
            .is_err(),
        "typed replica read cannot return partial rows"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_planning_read_downgrades_optional_schema_but_not_mandatory_errors() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    let file = seed_planning_file(&store).await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("remote contract client");
    client
        .execute(
            "UPDATE resource_settings_schema SET write_claim='unfinished'",
            hiqlite::params!(),
        )
        .await
        .expect("unsettled writer fixture");
    assert!(
        store
            .resource_capacity_snapshot()
            .await
            .expect("one scalar authority read")
            .is_none(),
        "active claim refuses typed authority"
    );
    client
        .execute(
            "UPDATE resource_settings_schema SET write_claim=NULL",
            hiqlite::params!(),
        )
        .await
        .expect("settled writer fixture");
    assert!(store
        .resource_capacity_snapshot()
        .await
        .expect("one scalar read")
        .is_some());
    client
        .execute(
            "INSERT INTO settings(key,value,updated_at) VALUES('transcode.max_hw_sessions','0',0)",
            hiqlite::params!(),
        )
        .await
        .expect("legacy cap");
    // Drop guards before their marker table so future/absent installations do
    // not poison unrelated writes. These are the actual replicated DDL path.
    for trigger in [
        "resource_settings_numeric_ai",
        "resource_settings_numeric_au",
        "resource_settings_numeric_ad",
        "resource_settings_revision_ai",
        "resource_settings_revision_au",
        "resource_settings_revision_ad",
    ] {
        client
            .execute(format!("DROP TRIGGER {trigger}"), hiqlite::params!())
            .await
            .expect("partial image fixture");
    }
    client
        .execute("DROP TABLE resource_settings_schema", hiqlite::params!())
        .await
        .expect("absent adjunct");
    assert!(store
        .resource_capacity_snapshot()
        .await
        .expect("recognized optional absence")
        .is_none());
    client.execute("INSERT INTO settings(key,value,updated_at) VALUES('transcode.max_hw_sessions.provenance',$1,0)", hiqlite::params!(r#"{"schema":1,"mode":"automatic","bound_value":"0"}"#)).await.expect("schema-less restored companion");
    let snapshot = store
        .playback_planning_snapshot(file, &[])
        .await
        .expect("one fallback full read")
        .expect("file still present");
    assert_eq!(snapshot.capacity.installation, CapacityInstallation::Absent);
    assert_eq!(
        snapshot.capacity.saved_modes.hardware_sessions.state,
        ReadState::Automatic
    );
    assert_eq!(
        snapshot.capacity.verified_modes.hardware_sessions.state,
        ReadState::LegacyAmbiguous
    );
    assert_eq!(
        snapshot
            .capacity
            .admission_value(&snapshot.settings, keys::MAX_HW_SESSIONS),
        Some("0")
    );
    client
        .execute(
            "ALTER TABLE items RENAME COLUMN title TO unrelated_title",
            hiqlite::params!(),
        )
        .await
        .expect("mandatory projection failure fixture");
    assert!(
        store.playback_planning_snapshot(file, &[]).await.is_err(),
        "mandatory source failure is not compatibility/absence"
    );
}
