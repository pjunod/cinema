use super::*;

struct Fixture {
    root: tempfile::TempDir,
    manifest: StorageManifest,
    incoming: PathBuf,
    receipt: PathBuf,
    source: VerifiedArtifact,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("storage handoff fixture");
    fs::create_dir(root.path().join("hiqlite")).expect("storage handoff fixture");
    fs::write(
        root.path().join("hiqlite/original-state"),
        b"retained source",
    )
    .expect("storage handoff fixture");
    fs::write(root.path().join("node.id"), b"node\n").expect("storage handoff fixture");
    publish_record(
        &root.path().join(LEGACY_HANDOFF),
        &serde_json::json!({
            "cluster_id":"server", "source_backup_sha256":"a".repeat(64)
        }),
    )
    .expect("storage handoff fixture");
    let legacy = Connection::open(root.path().join("plurx.db")).expect("storage handoff fixture");
    legacy.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT); INSERT INTO settings VALUES('instance.id','server'); CREATE TABLE old_data(value TEXT); INSERT INTO old_data VALUES('rollback only');").expect("storage handoff fixture");
    drop(legacy);
    fs::create_dir(root.path().join("incoming")).expect("storage handoff fixture");
    let incoming: PathBuf = "incoming/native.sqlite".into();
    let receipt: PathBuf = "incoming/receipt.json".into();
    fs::write(root.path().join(&receipt), b"{\"verified\":true}").expect("storage handoff fixture");
    fs::write(
        root.path().join("incoming/source.sqlite"),
        b"immutable old cut",
    )
    .expect("storage handoff fixture");
    let source = VerifiedArtifact {
        path: "incoming/source.sqlite".into(),
        sha256: hash_regular(&root.path().join("incoming/source.sqlite"))
            .expect("storage handoff fixture"),
    };
    let manifest = StorageManifest {
        version: 1,
        mode: StorageMode::Sqlite,
        database: "plurx.db".into(),
        instance_id: "server".into(),
        node_id: "node".into(),
        generation: uuid::Uuid::new_v4().to_string(),
        schema_version: crate::store::SQLITE_SCHEMA_VERSION,
    };
    let native = Connection::open(root.path().join(&incoming)).expect("storage handoff fixture");
    native.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT); INSERT INTO settings VALUES('instance.id','server'); CREATE TABLE storage_identity(singleton INTEGER PRIMARY KEY,instance_id TEXT,node_id TEXT,generation TEXT); CREATE TABLE user_changes(value TEXT);").expect("storage handoff fixture");
    native
        .pragma_update(None, "user_version", crate::store::SQLITE_SCHEMA_VERSION)
        .expect("current native fixture version");
    native
        .execute(
            "INSERT INTO storage_identity VALUES(1,?1,?2,?3)",
            rusqlite::params![manifest.instance_id, manifest.node_id, manifest.generation],
        )
        .expect("storage handoff fixture");
    drop(native);
    Fixture {
        root,
        manifest,
        incoming,
        receipt,
        source,
    }
}

fn prepare_fixture(fixture: &Fixture) -> NativeHandoff {
    prepare_native_handoff_with_source(
        fixture.root.path(),
        fixture.manifest.clone(),
        fixture.incoming.clone(),
        "retired".into(),
        fixture.receipt.clone(),
        fixture.source.clone(),
    )
    .expect("storage handoff fixture")
}

fn stage_renames(fixture: &Fixture) {
    let root = fixture.root.path();
    fs::create_dir(root.join("retired")).expect("storage handoff fixture");
    fs::rename(root.join("hiqlite"), root.join("retired/hiqlite"))
        .expect("storage handoff fixture");
    fs::rename(root.join("plurx.db"), root.join("retired/legacy-plurx.db"))
        .expect("storage handoff fixture");
    fs::rename(root.join(&fixture.incoming), root.join("plurx.db"))
        .expect("storage handoff fixture");
}

#[test]
fn crash_after_source_retirement_resumes_without_reopening_stale_root_data() {
    let fixture = fixture();
    prepare_fixture(&fixture);
    let root = fixture.root.path();
    fs::create_dir(root.join("retired")).expect("storage handoff fixture");
    fs::rename(root.join("hiqlite"), root.join("retired/hiqlite"))
        .expect("storage handoff fixture");
    assert_eq!(
        recover_native_handoff(root).expect("storage handoff fixture"),
        Some(fixture.manifest.clone())
    );
    fixture
        .manifest
        .validate_sqlite(root)
        .expect("storage handoff fixture");
    assert!(root.join("retired/hiqlite/original-state").is_file());
    assert!(root.join("retired/legacy-plurx.db").is_file());
    assert!(root.join(LEGACY_HANDOFF).is_file());
    assert!(!root.join(TRANSITION_FILENAME).exists());
}

#[test]
fn crash_after_native_target_move_finishes_the_original_verified_generation() {
    let fixture = fixture();
    prepare_fixture(&fixture);
    stage_renames(&fixture);
    assert_eq!(
        recover_native_handoff(fixture.root.path()).expect("storage handoff fixture"),
        Some(fixture.manifest.clone())
    );
    assert_eq!(
        StorageManifest::read(fixture.root.path()).expect("storage handoff fixture"),
        Some(fixture.manifest)
    );
}

#[test]
fn postpublication_recovery_preserves_native_writes_instead_of_comparing_initial_hash() {
    let fixture = fixture();
    let handoff = prepare_fixture(&fixture);
    stage_renames(&fixture);
    fixture
        .manifest
        .publish(fixture.root.path())
        .expect("storage handoff fixture");
    let database =
        Connection::open(fixture.root.path().join("plurx.db")).expect("storage handoff fixture");
    database
        .execute(
            "INSERT INTO user_changes VALUES('accepted after native publication')",
            [],
        )
        .expect("storage handoff fixture");
    drop(database);
    assert_ne!(
        hash_regular(&fixture.root.path().join("plurx.db")).expect("storage handoff fixture"),
        handoff.target_sha256
    );
    assert_eq!(
        recover_native_handoff(fixture.root.path()).expect("storage handoff fixture"),
        Some(fixture.manifest)
    );
    let database =
        Connection::open(fixture.root.path().join("plurx.db")).expect("storage handoff fixture");
    assert_eq!(
        database
            .query_row("SELECT value FROM user_changes", [], |row| row
                .get::<_, String>(0))
            .expect("storage handoff fixture"),
        "accepted after native publication"
    );
}

#[test]
fn malformed_and_unknown_transition_operations_fail_closed() {
    let fixture = fixture();
    fs::write(
        fixture.root.path().join(TRANSITION_FILENAME),
        b"{\"operation\":\"unknown\"}",
    )
    .expect("storage handoff fixture");
    assert!(recover_native_handoff(fixture.root.path()).is_err());
    assert!(fixture.root.path().join("hiqlite/original-state").is_file());
    fs::remove_file(fixture.root.path().join(TRANSITION_FILENAME))
        .expect("storage handoff fixture");
    let mut handoff = prepare_fixture(&fixture);
    handoff.operation = "discard_source".into();
    publish_record(&fixture.root.path().join(TRANSITION_FILENAME), &handoff)
        .expect("storage handoff fixture");
    assert!(recover_native_handoff(fixture.root.path()).is_err());
    assert!(fixture.root.path().join("hiqlite/original-state").is_file());
}

#[test]
fn simultaneous_active_and_retired_sources_are_ambiguous() {
    let fixture = fixture();
    prepare_fixture(&fixture);
    fs::create_dir_all(fixture.root.path().join("retired/hiqlite"))
        .expect("storage handoff fixture");
    assert!(recover_native_handoff(fixture.root.path()).is_err());
    assert!(fixture.root.path().join("hiqlite/original-state").is_file());
    assert!(fixture.root.path().join(&fixture.incoming).is_file());
}

#[test]
fn receipt_source_and_target_hash_changes_refuse_before_retiring_the_source() {
    for artifact in ["receipt", "source", "target"] {
        let fixture = fixture();
        prepare_fixture(&fixture);
        match artifact {
            "receipt" => fs::write(
                fixture.root.path().join(&fixture.receipt),
                b"changed receipt",
            )
            .expect("storage handoff fixture"),
            "source" => fs::write(
                fixture.root.path().join(&fixture.source.path),
                b"changed old cut",
            )
            .expect("storage handoff fixture"),
            _ => {
                let connection = Connection::open(fixture.root.path().join(&fixture.incoming))
                    .expect("storage handoff fixture");
                connection
                    .execute(
                        "INSERT INTO user_changes VALUES('unexpected write before publication')",
                        [],
                    )
                    .expect("storage handoff fixture");
            }
        }
        assert!(
            recover_native_handoff(fixture.root.path()).is_err(),
            "{artifact}"
        );
        assert!(fixture.root.path().join("hiqlite/original-state").is_file());
    }
}

#[test]
fn changed_incoming_binding_and_missing_legacy_breadcrumb_refuse_preparation() {
    let fixture = fixture();
    let connection = Connection::open(fixture.root.path().join(&fixture.incoming))
        .expect("storage handoff fixture");
    connection
        .execute("UPDATE storage_identity SET node_id='different-node'", [])
        .expect("storage handoff fixture");
    drop(connection);
    assert!(prepare_native_handoff(
        fixture.root.path(),
        fixture.manifest.clone(),
        fixture.incoming.clone(),
        "retired".into(),
        fixture.receipt.clone()
    )
    .is_err());
    assert!(!fixture.root.path().join(TRANSITION_FILENAME).exists());
    assert!(fixture.root.path().join("hiqlite").is_dir());
}

#[test]
fn recovery_precedes_old_manifest_path_validation_after_source_retirement() {
    let fixture = fixture();
    let root = fixture.root.path();
    let old = StorageManifest {
        version: 1,
        mode: StorageMode::Cluster,
        database: "hiqlite".into(),
        instance_id: fixture.manifest.instance_id.clone(),
        node_id: fixture.manifest.node_id.clone(),
        generation: uuid::Uuid::new_v4().to_string(),
        schema_version: 82,
    };
    old.publish(root).expect("storage handoff fixture");
    prepare_fixture(&fixture);
    fs::create_dir(root.join("retired")).expect("storage handoff fixture");
    fs::rename(root.join("hiqlite"), root.join("retired/hiqlite"))
        .expect("storage handoff fixture");
    assert!(
        StorageManifest::read(root).is_err(),
        "the old active path has deliberately moved"
    );
    assert_eq!(
        recover_native_handoff(root).expect("storage handoff fixture"),
        Some(fixture.manifest.clone())
    );
    assert_eq!(
        StorageManifest::read(root).expect("storage handoff fixture"),
        Some(fixture.manifest)
    );
}

#[test]
fn removing_legacy_refusal_breadcrumb_cannot_enable_a_silent_rollback() {
    let fixture = fixture();
    prepare_fixture(&fixture);
    fs::remove_file(fixture.root.path().join(LEGACY_HANDOFF)).expect("storage handoff fixture");
    assert!(recover_native_handoff(fixture.root.path()).is_err());
    assert!(fixture.root.path().join("hiqlite/original-state").is_file());
    assert!(fixture.root.path().join(&fixture.incoming).is_file());
}
