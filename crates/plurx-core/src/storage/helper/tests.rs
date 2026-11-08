use super::*;

pub(super) fn build() -> HelperBuild {
    HelperBuild {
        commit: "a".repeat(40),
        tree: "b".repeat(40),
        target: "x86_64-unknown-linux-gnu".into(),
    }
}

pub(super) fn binding() -> HelperBinding {
    HelperBinding {
        daemon: build(),
        helper: describe(build()).expect("description"),
        executable_sha256: "c".repeat(64),
        bundle_sha256: "d".repeat(64),
    }
}

pub(super) fn fixture() -> (tempfile::TempDir, Config, NativeDirectoryLock) {
    let directory = tempfile::tempdir().expect("root");
    let lease = NativeDirectoryLock::try_acquire(directory.path()).expect("root lease");
    fs::create_dir(directory.path().join("hiqlite")).expect("legacy directory");
    fs::write(
        directory.path().join("hiqlite/activation.json"),
        br#"{"replicated_schema_version":81}"#,
    )
    .expect("metadata");
    fs::write(directory.path().join("node.id"), b"node\n").expect("identity");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().to_path_buf();
    (directory, config, lease)
}

#[test]
fn request_is_bound_immutable_and_does_not_retain_the_root_lease() {
    let (directory, config, lease) = fixture();
    let request = prepare(&config, &lease, binding()).expect("prepare");
    assert_eq!(
        pending(directory.path()).expect("pending"),
        Some(request.clone())
    );
    assert!(
        prepare(&config, &lease, binding()).is_err(),
        "competing operation refuses"
    );
    drop(lease);
    let lease = NativeDirectoryLock::try_acquire(directory.path())
        .expect("descriptor owns no hidden lease");
    let path = operation_path(directory.path(), &request.id, "request.json").expect("request path");
    let original = fs::read(&path).expect("request bytes");
    fs::write(&path, [original.as_slice(), b" "].concat()).expect("tamper");
    assert!(pending(directory.path()).is_err());
    assert!(validate_completed(&lease, &request).is_err());
    assert!(path.exists(), "failed validation preserves evidence");
}

#[test]
fn request_binds_metadata_key_and_root_identity_without_key_bytes() {
    let (directory, config, lease) = fixture();
    fs::write(
        config.credential_key_path(),
        b"not a secret embedded in a request",
    )
    .expect("key fixture");
    let prepared = prepare(&config, &lease, binding()).expect("prepare");
    let request = read_request(directory.path(), &prepared).expect("read");
    assert_eq!(
        request.root,
        root_identity(directory.path()).expect("identity")
    );
    assert_eq!(
        request.key,
        file_binding(&config.credential_key_path()).expect("key binding")
    );
    let bytes = fs::read_to_string(
        operation_path(directory.path(), &prepared.id, "request.json").expect("path"),
    )
    .expect("bytes");
    assert!(!bytes.contains("not a secret embedded"));
    assert_eq!(request.metadata.len(), METADATA.len());
}

#[test]
fn wrong_tuple_explicit_cluster_and_foreign_root_refuse_before_intent() {
    let (_directory, mut config, lease) = fixture();
    let mut wrong = binding();
    wrong.helper.source_cohorts.push(MigrationSourceCohort {
        auth_version: 83,
        canonical_layout_fingerprint: "e".repeat(64),
    });
    assert!(prepare(&config, &lease, wrong).is_err());
    config.storage.mode = Some(StorageMode::Cluster);
    assert!(prepare(&config, &lease, binding()).is_err());
    config.storage.mode = None;
    let other = tempfile::tempdir().expect("other root");
    config.storage.data_dir = other.path().to_path_buf();
    assert!(prepare(&config, &lease, binding()).is_err());
    assert!(pending(lease.data_dir()).expect("pending").is_none());
}

#[test]
fn refused_legacy_metadata_leaves_cluster_startup_free_of_helper_intent() {
    use serde_json::json;
    let private = json!({"replicated_schema_version": 81});
    let local = json!({"raft_id": 1, "raft_address": "127.0.0.1:1", "api_address": "127.0.0.1:2"});
    let remote = json!({"raft_id": 2, "raft_address": "10.42.0.2:1", "api_address": "10.42.0.2:2"});
    for (marker, membership) in [
        (
            json!({"replicated_schema_version": 81, "admitted_role": "voter"}),
            None,
        ),
        (json!({"replicated_schema_version": 80}), None),
        (
            private.clone(),
            Some(json!({"local": remote, "bootstrap": [remote]})),
        ),
        (
            private.clone(),
            Some(json!({"local": local, "bootstrap": [local, remote]})),
        ),
        (
            private.clone(),
            Some(json!({"local": local, "bootstrap": [local], "join_token_digest": "consumed"})),
        ),
        (
            private,
            Some(json!({"local": local, "bootstrap": [local], "role": "learner"})),
        ),
    ] {
        let (directory, config, lease) = fixture();
        let root = directory.path();
        publish_record(&root.join("hiqlite/activation.json"), &marker).expect("activation");
        if let Some(member) = &membership {
            publish_record(&root.join("membership.json"), member).expect("membership");
        }
        assert!(prepare(&config, &lease, binding()).is_err());
        assert!(pending(root)
            .expect("ordinary cluster startup gate")
            .is_none());
        assert!(!root.join("storage-migrations").exists());
        assert_eq!(
            read_record(&root.join("hiqlite/activation.json")).expect("preserved activation"),
            Some(marker)
        );
        assert_eq!(
            read_record(&root.join("membership.json")).expect("preserved membership"),
            membership
        );
    }
}

#[test]
fn strict_wire_rejects_unknown_duplicate_and_noncanonical_values() {
    let mut value = serde_json::to_value(binding()).expect("binding JSON");
    value["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<HelperBinding>(value).is_err());
    assert!(serde_json::from_str::<HelperBuild>(&format!(
        r#"{{"commit":"{}","commit":"{}","tree":"{}","target":"target"}}"#,
        "a".repeat(40),
        "b".repeat(40),
        "c".repeat(40)
    ))
    .is_err());
    assert!(canonical_id("../request").is_err());
    assert!(hex(&"A".repeat(64), 64).is_err());
    let mut wrong = build();
    wrong.target = "../../target".into();
    assert!(describe(wrong).is_err());
}

#[test]
fn oversized_metadata_is_not_read_or_replaced() {
    let (directory, config, lease) = fixture();
    fs::write(
        directory.path().join("node.id"),
        vec![b'x'; LIMIT as usize + 1],
    )
    .expect("oversized metadata");
    assert!(prepare(&config, &lease, binding()).is_err());
    assert_eq!(
        fs::metadata(directory.path().join("node.id"))
            .expect("preserved metadata")
            .len(),
        LIMIT + 1
    );
    assert!(!directory.path().join(ACTIVE).exists());
}

#[cfg(unix)]
#[test]
fn symlink_metadata_and_operation_directory_are_refused() {
    use std::os::unix::fs::symlink;
    let (directory, config, lease) = fixture();
    let outside = tempfile::tempdir().expect("outside");
    symlink(outside.path(), directory.path().join("storage-migrations")).expect("link");
    assert!(prepare(&config, &lease, binding()).is_err());
    fs::remove_file(directory.path().join("storage-migrations")).expect("remove link");
    fs::remove_file(directory.path().join("node.id")).expect("remove identity");
    symlink(
        outside.path().join("node.id"),
        directory.path().join("node.id"),
    )
    .expect("dangling link");
    assert!(prepare(&config, &lease, binding()).is_err());
    assert_eq!(
        fs::read_dir(outside.path())
            .expect("outside intact")
            .count(),
        0
    );
}

#[test]
fn exact_layout_cohorts_are_distinct_and_changed_fingerprints_refuse() {
    let cohorts = expected_source_cohorts();
    assert_eq!(
        cohorts
            .iter()
            .map(|cohort| cohort.auth_version)
            .collect::<Vec<_>>(),
        vec![81, 82]
    );
    assert_ne!(
        cohorts[0].canonical_layout_fingerprint,
        cohorts[1].canonical_layout_fingerprint
    );
    for cohort in &cohorts {
        hex(&cohort.canonical_layout_fingerprint, 64).expect("canonical layout fingerprint");
        let actual =
            crate::store::sharing_source_schema::migration_source_layout_guard(cohort.auth_version)
                .expect("actual exporter recognizer");
        assert!(actual.contains("sqlite_master"));
        assert!(actual.contains("cluster_meta"));
    }
    assert!(crate::store::sharing_source_schema::migration_source_layout_guard(83).is_none());
    let mut changed = binding();
    changed.helper.source_cohorts[0].canonical_layout_fingerprint = "f".repeat(64);
    assert!(
        validate_binding(&changed).is_err(),
        "matching auth version alone must not admit a helper"
    );
    let mut wrong_version = binding();
    wrong_version.helper.source_cohorts[0].auth_version = 82;
    assert!(
        validate_binding(&wrong_version).is_err(),
        "a layout fingerprint cannot be relabeled as another cohort"
    );
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn replicated_maintenance_refuses_legacy_and_pending_roots_before_connection() {
    use crate::cluster::migration::connect_activated_store;

    let (directory, config, lease) = fixture();
    let root = directory.path();
    let marker = fs::read(root.join("hiqlite/activation.json")).expect("original marker");
    let refusal = match connect_activated_store(&config).await {
        Err(error) => error,
        Ok(_) => panic!("legacy metadata admitted a maintenance client"),
    };
    assert!(refusal.to_string().contains("committed cluster authority"));

    let request = prepare(&config, &lease, binding()).expect("durable migration request");
    let manifest = StorageManifest {
        version: 1,
        mode: StorageMode::Cluster,
        database: "hiqlite".into(),
        instance_id: "fixture-server".into(),
        node_id: "node".into(),
        generation: uuid::Uuid::new_v4().to_string(),
        schema_version: 81,
    };
    manifest
        .publish(root)
        .expect("conflicting committed authority fixture");
    let refusal = match connect_activated_store(&config).await {
        Err(error) => error,
        Ok(_) => panic!("pending helper admitted a maintenance client"),
    };
    assert!(refusal.to_string().contains("pending standalone migration"));
    assert_eq!(pending(root).expect("preserved intent"), Some(request));
    assert_eq!(
        StorageManifest::read(root).expect("preserved existing manifest"),
        Some(manifest)
    );
    assert_eq!(
        fs::read(root.join("hiqlite/activation.json")).expect("preserved marker"),
        marker
    );
    assert!(!config.credential_key_path().exists());
    assert!(!root.join("hiqlite/telemetry.db").exists());
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn cluster_promotion_refuses_pending_helper_under_its_real_root_lease() {
    use crate::cluster::migration::native_promotion::promote_native_storage;

    let (directory, config, lease) = fixture();
    let root = directory.path();
    let request = prepare(&config, &lease, binding()).expect("durable migration request");
    let refusal = match promote_native_storage(&config, None, lease).await {
        Err(error) => error,
        Ok(_) => panic!("cluster creation overtook the pending helper"),
    };
    assert!(refusal.to_string().contains("pending standalone migration"));
    let _reacquired =
        NativeDirectoryLock::try_acquire(root).expect("refused promotion released its root");
    assert_eq!(pending(root).expect("preserved intent"), Some(request));
    assert!(!config.credential_key_path().exists());
    assert!(!root.join("storage-transition.json").exists());
    assert!(!root.join("storage-mode.json").exists());
    assert!(!root.join("hiqlite.incoming").exists());
}
