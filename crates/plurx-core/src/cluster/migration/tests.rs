#[cfg(feature = "hiqlite-store")]
use crate as observer_core;
#[cfg(feature = "hiqlite-store")]
mod startup_observer {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/startup_observer.rs"
    ));
}
use super::*;

#[cfg(feature = "hiqlite-store")]
#[path = "membership_api.rs"]
mod membership_api;

#[cfg(feature = "hiqlite-store")]
fn portable_snapshot_fixture(root: &Path, cluster_id: &str) -> PathBuf {
    let active = root.join(HIQLITE_ACTIVE_DIRNAME);
    std::fs::create_dir_all(&active).expect("active fixture");
    write_activation_marker(
        &active,
        &ActivationMarker {
            marker_version: ACTIVATION_MARKER_VERSION,
            cluster_id: cluster_id.to_owned(),
            source_backup_sha256: "a".repeat(64),
            source_schema_version: AUTH_SCHEMA_VERSION,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 3,
            table_hashes: vec![SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 1,
                sha256: "b".repeat(64),
            }],
            admitted_role: Some(ClusterRole::Voter),
        },
    )
    .expect("activation marker fixture");
    let snapshot = root.join("published-snapshot");
    // Build the current production schema. A hand-written subset made the
    // restore test pass while omitting exactly the durable coordination
    // tables the restore contract promises to clear.
    drop(SqliteStore::open(&snapshot).expect("production snapshot schema"));
    let connection = Connection::open(&snapshot).expect("snapshot fixture");
    connection
        .execute_batch(
            "CREATE TABLE _metadata (key TEXT PRIMARY KEY, data BLOB NOT NULL);
                 INSERT INTO settings(key,value) VALUES('instance.id', 'cluster-backup-test')
                   ON CONFLICT(key) DO UPDATE SET value=excluded.value;
                 INSERT INTO users(id,username,password_hash,is_admin)
                   VALUES(1,'restore-admin','hash',1);
                 INSERT INTO libraries(id,name,kind,paths)
                   VALUES(1,'Movies','movies','[\"/srv/media\"]');
                 INSERT INTO library_roots(library_id,fingerprint) VALUES(1, 'old-root');
                 INSERT INTO items(id,library_id,kind,title,sort_title)
                   VALUES(1,1,'movie','Movie','Movie');
                 INSERT INTO files(id,item_id,path,size,mtime)
                   VALUES(1,1,'/srv/media/movie.mkv',1,1);
                 INSERT INTO dvr_recordings(
                   id,origin,channel_id,guide_number,channel_name,airing_start,
                   airing_end,capture_start,capture_end,title,state,path,
                   created_at_ms,updated_at_ms)
                   VALUES('recording-1','manual','channel-1','1','One',1,2,1,2,
                          'News','done','/srv/media/dvr/news.ts',1,1);
                 INSERT INTO media_session_requests(
                   user_id,request_id,request_fingerprint,playback_id,state,
                   claim_expires_at_ms,incarnation_id,owner_node_id,updated_at_ms)
                   VALUES(1,'request-1','fingerprint','playback-1','resolved',9,
                          'incarnation-1','old-node',1);
                 INSERT INTO media_sessions(
                   incarnation_id,session_id,user_id,playback_id,
                   request_fingerprint,owner_node_id,owner_epoch,
                   lease_expires_at_ms,state,recipe_json,response_json,updated_at_ms)
                   VALUES('incarnation-1','session-1',1,'playback-1','fingerprint',
                          'old-node',1,9,'active','{}','{}',1);
                 INSERT INTO media_playback_pointers(
                   user_id,playback_id,current_incarnation_id,updated_at_ms)
                   VALUES(1,'playback-1','incarnation-1',1);
                 INSERT INTO media_session_preparations(
                   user_id,playback_id,staged_incarnation_id,
                   expected_predecessor_incarnation_id,deadline_ms,
                   created_at_ms,updated_at_ms)
                   VALUES(1,'playback-2','incarnation-2','incarnation-1',9,1,1);
                 INSERT INTO media_session_terminal_acks(
                   incarnation_id,session_id,owner_node_id,owner_epoch,
                   client_instance_id,sequence,request_fingerprint,response_json,
                   expires_at_ms,updated_at_ms)
                   VALUES('incarnation-3','session-3','old-node',1,'client-1',1,
                          'fingerprint','{}',9,1);
                 INSERT INTO media_session_producer_recovery(
                   user_id,playback_id,recovery_epoch,failed_incarnation_id,
                   failed_producer_attempt,decision_sequence,failed_plan_digest,
                   alternate_plan_digest,state,created_at_ms,updated_at_ms)
                   VALUES(1,'playback-1','epoch-1','incarnation-1',1,1,
                          lower(hex(zeroblob(32))),lower(hex(zeroblob(32))),
                          'exhausted',1,1);",
        )
        .expect("snapshot schema");
    let metadata = hiqlite::StateMachineData {
        last_applied_log_id: Some(openraft::LogId::new(
            openraft::CommittedLeaderId::new(3, 1),
            7,
        )),
        ..Default::default()
    };
    let bytes = bincode::serde::encode_to_vec(&metadata, bincode::config::legacy())
        .expect("snapshot metadata");
    connection
        .execute("INSERT INTO _metadata(key,data) VALUES('meta',?1)", [bytes])
        .expect("metadata row");
    drop(connection);
    snapshot
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn cluster_backup_open_descriptor_survives_unlink_and_records_exact_cut() {
    let data = tempfile::tempdir().expect("data fixture");
    let destination = tempfile::tempdir().expect("destination fixture");
    let snapshot = portable_snapshot_fixture(data.path(), "cluster-backup-test");
    let descriptor = File::open(&snapshot).expect("open snapshot before cleanup");
    std::fs::remove_file(&snapshot).expect("supersede snapshot");

    let (artifact, manifest) = build_cluster_backup_artifact(
        descriptor,
        destination.path(),
        data.path(),
        None,
        "node-a",
        None,
        "test-build",
        7,
    )
    .expect("portable artefact from held descriptor");

    assert_eq!(manifest.raft_cut.term, 3);
    assert_eq!(manifest.raft_cut.index, 7);
    let connection = Connection::open(artifact.join(SQLITE_FILENAME)).expect("artefact image");
    let metadata_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_metadata'",
            (),
            |row| row.get(0),
        )
        .expect("metadata table count");
    assert_eq!(metadata_tables, 1);
    let metadata_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM _metadata", (), |row| row.get(0))
        .expect("metadata row count");
    assert_eq!(metadata_rows, 0);
    verify_cluster_backup_archive(&artifact, None).expect("verified artefact");
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn cluster_backup_refuses_a_declared_missing_key_without_publishing() {
    let data = tempfile::tempdir().expect("data fixture");
    let destination = tempfile::tempdir().expect("destination fixture");
    let snapshot = portable_snapshot_fixture(data.path(), "cluster-backup-test");
    let missing = data.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);

    let error = build_cluster_backup_artifact(
        File::open(&snapshot).expect("snapshot"),
        destination.path(),
        data.path(),
        Some(&missing),
        "node-a",
        Some("deadbeef"),
        "test-build",
        7,
    )
    .expect_err("a declared key must be copied or the build must fail");

    assert!(error.to_string().contains("credential key file"));
    assert_eq!(
        std::fs::read_dir(destination.path())
            .expect("empty destination")
            .count(),
        0,
        "a failed key copy must publish neither staging nor archive"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn cluster_backup_integrity_failure_cleans_staging_and_publishes_nothing() {
    let data = tempfile::tempdir().expect("data fixture");
    let destination = tempfile::tempdir().expect("destination fixture");
    let snapshot = portable_snapshot_fixture(data.path(), "cluster-backup-test");
    let connection = Connection::open(&snapshot).expect("corruptible snapshot");
    connection
        .execute_batch(
            "PRAGMA writable_schema=ON;
                 UPDATE sqlite_master SET rootpage=2147483647
                   WHERE type='index' AND name='idx_items_library_kind';
                 PRAGMA writable_schema=OFF;",
        )
        .expect("poison an index root page");
    drop(connection);

    build_cluster_backup_artifact(
        File::open(&snapshot).expect("snapshot"),
        destination.path(),
        data.path(),
        None,
        "node-a",
        None,
        "test-build",
        7,
    )
    .expect_err("integrity failure must refuse the artefact");

    assert_eq!(
        std::fs::read_dir(destination.path())
            .expect("empty destination")
            .count(),
        0,
        "integrity failure must clean staging and never publish"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn sharing_purpose_backup_verification_censuses_retired_keys_and_opens_original_aad() {
    let archive = tempfile::tempdir().expect("archive");
    let connection = Connection::open_in_memory().expect("offline image");
    connection.execute_batch("CREATE TABLE sharing_identity(singleton INTEGER NOT NULL PRIMARY KEY,server_id TEXT NOT NULL,catalogue_epoch TEXT NOT NULL,created_at_ms INTEGER NOT NULL)").expect("empty retired identity");
    for schema in [
        crate::store::sharing_purpose_keys::SHARING_PURPOSE_KEYS_SCHEMA,
        crate::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
        crate::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
    ] {
        connection
            .execute_batch(schema)
            .expect("candidate installed image");
    }
    let key_path = archive.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
    let master = CredentialKey::load_or_create(&key_path).expect("fixture master");
    let identity = crate::sharing::SharingIdentity {
        server_id: uuid::Uuid::new_v4(),
        catalogue_epoch: uuid::Uuid::new_v4(),
        created_at_ms: 1,
    };
    let source = crate::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
        &master,
        identity.clone(),
    )
    .expect("retired Source");
    let receiver =
        crate::sharing_file_locators::FileLocatorKey::generate_sealed(&master, &identity)
            .expect("retired B");
    for (purpose, envelope) in [("catalogue_revision", &source), ("file_locator", &receiver)] {
        connection
            .execute(
                "INSERT INTO sharing_purpose_key_archive VALUES(?1,?2,?3,?4)",
                rusqlite::params![
                    purpose,
                    identity.server_id.to_string(),
                    identity.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("retired archive");
    }
    connection
        .execute(
            "INSERT INTO sharing_purpose_key_installation VALUES(1,?1,?2,?3,'restore_pending',2,1)",
            rusqlite::params![
                identity.server_id.to_string(),
                identity.catalogue_epoch.to_string(),
                master.id()
            ],
        )
        .expect("explicit restore disposition");
    connection
        .execute(
            "INSERT INTO sharing_purpose_transaction_guard VALUES(1,1)",
            [],
        )
        .expect("closed guard");
    verify_backup_key(&connection, archive.path(), None, None)
        .expect("all retired purpose material opens");
    let retained = archive.path().join("retained-key");
    std::fs::rename(&key_path, &retained).expect("missing-key fixture");
    assert!(
        verify_backup_key(&connection, archive.path(), None, None).is_err(),
        "purpose-only image cannot be mistaken for no sealed rows"
    );
    assert!(!key_path.exists(), "verification never mints a replacement");
    verify_backup_key(&connection, archive.path(), Some(&retained), None)
        .expect("explicit correct key");
    let wrong = archive.path().join("wrong-key");
    CredentialKey::load_or_create(&wrong).expect("wrong fixture master");
    assert!(verify_backup_key(&connection, archive.path(), Some(&wrong), None).is_err());
    let foreign = crate::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
        &master,
        crate::sharing::SharingIdentity {
            server_id: uuid::Uuid::new_v4(),
            ..identity.clone()
        },
    )
    .expect("same key foreign AAD");
    connection
        .execute(
            "UPDATE sharing_purpose_key_archive SET envelope=?1 WHERE purpose='catalogue_revision'",
            [foreign.as_stored()],
        )
        .expect("AAD corruption fixture");
    assert!(
        verify_backup_key(&connection, archive.path(), Some(&retained), None).is_err(),
        "matching key ID is insufficient without authenticated open"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn restore_verify_missing_key_is_read_only_and_wrong_key_is_refused() {
    let data = tempfile::tempdir().expect("data fixture");
    let destination = tempfile::tempdir().expect("destination fixture");
    let snapshot = portable_snapshot_fixture(data.path(), "cluster-backup-test");
    let source_key_path = data.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
    let source_key = CredentialKey::load_or_create(&source_key_path).expect("source key");
    let (artifact, _) = build_cluster_backup_artifact(
        File::open(&snapshot).expect("snapshot"),
        destination.path(),
        data.path(),
        Some(&source_key_path),
        "node-a",
        Some(source_key.id()),
        "test-build",
        7,
    )
    .expect("keyed artefact");

    let wrong_key_path = data.path().join("wrong-credentials.key");
    let wrong_key = CredentialKey::load_or_create(&wrong_key_path).expect("wrong key");
    assert_ne!(wrong_key.id(), source_key.id());
    let wrong = verify_cluster_backup_archive(&artifact, Some(&wrong_key_path))
        .expect_err("wrong override key must be refused");
    assert!(wrong.to_string().contains("manifest names credential key"));

    std::fs::remove_file(artifact.join(crate::secrets::CREDENTIAL_KEY_FILENAME))
        .expect("simulate missing archived key");
    let before = std::fs::read_dir(&artifact)
        .expect("archive members before verify")
        .map(|entry| {
            let entry = entry.expect("archive member");
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).expect("archive member bytes"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let missing = verify_cluster_backup_archive(&artifact, None)
        .expect_err("missing declared key must be refused");
    assert!(missing.to_string().contains("credentials.key is missing"));
    let after = std::fs::read_dir(&artifact)
        .expect("archive members after verify")
        .map(|entry| {
            let entry = entry.expect("archive member");
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).expect("archive member bytes"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(after, before, "verify must be byte-for-byte read-only");
    assert!(!artifact
        .join(crate::secrets::CREDENTIAL_KEY_FILENAME)
        .exists());
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn restore_archive_newer_schema_refuses_before_creating_the_target() {
    let data = tempfile::tempdir().expect("data fixture");
    let destination = tempfile::tempdir().expect("destination fixture");
    let snapshot = portable_snapshot_fixture(data.path(), "cluster-backup-test");
    let (artifact, _) = build_cluster_backup_artifact(
        File::open(&snapshot).expect("snapshot"),
        destination.path(),
        data.path(),
        None,
        "node-a",
        None,
        "test-build",
        7,
    )
    .expect("newer-schema artefact");
    let manifest_path = artifact.join("manifest.json");
    let mut manifest: ClusterBackupManifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("manifest bytes"))
            .expect("manifest");
    manifest.replicated_schema_version = AUTH_SCHEMA_VERSION + 1;
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("newer manifest");
    manifest_bytes.push(b'\n');
    std::fs::write(&manifest_path, manifest_bytes).expect("replace manifest");
    let sums = format!(
        "{}  manifest.json\n{}  {SQLITE_FILENAME}\n",
        sha256_file(&manifest_path).expect("manifest hash"),
        manifest.image_sha256
    );
    std::fs::write(artifact.join("SHA256SUMS"), sums).expect("replace checksums");
    let target_parent = tempfile::tempdir().expect("target parent");
    let target = target_parent.path().join("restored");
    let config = membership_test_config(&target);

    let error = restore_cluster_backup_archive(&artifact, &config, &[], None)
        .await
        .expect_err("newer schema must be refused");

    assert!(error.to_string().contains("only knows"));
    assert!(!target.exists(), "schema refusal must not create a target");
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn restore_archive_remaps_paths_resets_fingerprint_and_mints_fence() {
    let data = tempfile::tempdir().expect("builder fixture");
    let destination = tempfile::tempdir().expect("destination fixture");
    let snapshot = portable_snapshot_fixture(data.path(), "cluster-backup-test");
    let (artifact, _) = build_cluster_backup_artifact(
        File::open(&snapshot).expect("snapshot"),
        destination.path(),
        data.path(),
        None,
        "old-node",
        None,
        "test-build",
        7,
    )
    .expect("portable artefact");
    let target_parent = tempfile::tempdir().expect("target parent");
    let target = target_parent.path().join("restored");
    let config = membership_test_config(&target);

    let report = restore_cluster_backup_archive(
        &artifact,
        &config,
        &[(PathBuf::from("/srv/media"), PathBuf::from("/mnt/nas/media"))],
        None,
    )
    .await
    .expect("offline restore");

    assert_eq!(report.cluster_id, "cluster-backup-test");
    assert_eq!(report.remapped_files, 1);
    assert_eq!(report.remapped_libraries, 1);
    assert_eq!(report.remapped_dvr_recordings, 1);
    assert_ne!(report.node_id.as_deref(), Some("old-node"));
    for secret in [
        RAFT_SECRET_FILENAME,
        API_SECRET_FILENAME,
        ACTIVITY_SIGNING_KEY_FILENAME,
    ] {
        assert!(target.join(secret).is_file(), "missing fresh {secret}");
    }
    let restored = Connection::open(
        target
            .join(HIQLITE_ACTIVE_DIRNAME)
            .join("state_machine/db")
            .join(HIQLITE_DATABASE_FILENAME),
    )
    .expect("restored image");
    let path: String = restored
        .query_row("SELECT path FROM files WHERE id=1", (), |row| row.get(0))
        .expect("restored file path");
    assert_eq!(path, "/mnt/nas/media/movie.mkv");
    let roots: i64 = restored
        .query_row("SELECT COUNT(*) FROM library_roots", (), |row| row.get(0))
        .expect("root count");
    assert_eq!(roots, 0);
    let dvr_path: String = restored
        .query_row(
            "SELECT path FROM dvr_recordings WHERE id='recording-1'",
            (),
            |row| row.get(0),
        )
        .expect("restored DVR path");
    assert_eq!(dvr_path, "/mnt/nas/media/dvr/news.ts");
    for table in [
        "media_session_requests",
        "media_session_preparations",
        "media_session_terminal_acks",
        "media_session_producer_recovery",
        "media_playback_pointers",
        "media_sessions",
    ] {
        let count: i64 = restored
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), (), |row| {
                row.get(0)
            })
            .unwrap_or_else(|error| panic!("count restored {table}: {error}"));
        assert_eq!(count, 0, "restore retained dead coordination in {table}");
    }
    let generation: String = restored
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [crate::store::keys::CLUSTER_RESTORE_GENERATION],
            |row| row.get(0),
        )
        .expect("restore generation");
    assert_eq!(generation, report.source_built_at_unix_ms.to_string());
}

// These two restore tests may reacquire only their initial source endpoints.
// A reaped process, rather than a dropped Tokio runtime, also closes the
// vendor's detached SQLite/WAL OS threads after pre-Client bind failures.
#[cfg(feature = "hiqlite-store")]
mod restore_endpoints {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::Instant;

    const CHILD: &str = "PLURX_RESTORE_ENDPOINT_CHILD";
    const COLLISION: i32 = 75;
    const ATTEMPTS: usize = 5;
    const LOG_LIMIT: u64 = 8 * 1024 * 1024;
    const REPORT_LIMIT: u64 = 16 * 1024;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub(super) enum Probe {
        Actual,
        FirstCollision,
        AlwaysCollision,
        Unknown,
        WrongEndpoint,
        MissingReport,
        AcquiredThenRefuse,
        AcquiredPanic,
        Timeout,
        OutputLimit,
        WrongChild,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Context {
        case: String,
        nonce: String,
        attempt: usize,
        directory: PathBuf,
        state: PathBuf,
        sharing: bool,
        probe: Probe,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Report {
        context: Context,
        pid: u32,
        stage: String,
        source: PathBuf,
        raft: std::net::SocketAddr,
        api: std::net::SocketAddr,
        error: Option<String>,
    }

    #[derive(Debug, Serialize)]
    struct Attempt {
        number: usize,
        pid: u32,
        directory: PathBuf,
        status: String,
        acquired: bool,
        collision: Option<Report>,
        error: Option<String>,
    }

    #[derive(Debug, Serialize)]
    pub(super) struct Run {
        pub(super) success: bool,
        pub(super) error: String,
        pub(super) attempts: usize,
        pub(super) directories: Vec<PathBuf>,
        archive: PathBuf,
    }

    struct OwnedChild(Option<Child>);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                child.wait().expect("reap owned restore candidate");
            }
        }
    }

    fn json_file(path: &Path, value: &impl Serialize) {
        let bytes = serde_json::to_vec(value).expect("encode owned restore report");
        assert!(bytes.len() as u64 <= REPORT_LIMIT);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create immutable restore report");
        file.write_all(&bytes).expect("write restore report");
    }

    fn context(expected: &str) -> Option<Context> {
        let encoded = std::env::var_os(CHILD)?;
        let encoded = encoded.to_str().expect("UTF-8 child context");
        assert!(encoded.len() as u64 <= REPORT_LIMIT);
        let context: Context = serde_json::from_str(encoded).expect("exact child context");
        assert_eq!(
            context.case,
            format!("cluster::migration::tests::{expected}")
        );
        assert!((1..=ATTEMPTS).contains(&context.attempt));
        uuid::Uuid::parse_str(&context.nonce).expect("owned child nonce");
        assert_eq!(context.state, context.directory.join("state"));
        assert!(context.directory.is_absolute());
        assert_eq!(
            context.directory.canonicalize().expect("owned candidate"),
            context.directory
        );
        assert_eq!(
            std::fs::read(context.directory.join("context.json")).expect("sealed child context"),
            encoded.as_bytes()
        );
        Some(context)
    }

    fn current_context() -> Context {
        let encoded = std::env::var(CHILD).expect("source setup runs only in owned child");
        serde_json::from_str(&encoded).expect("validated current restore context")
    }

    pub(super) fn source_directory() -> tempfile::TempDir {
        tempfile::tempdir_in(current_context().state).expect("owned source data directory")
    }

    // StoreError intentionally has string payloads. Recognize only the two
    // exact production bind contexts, this candidate's addresses and rollback
    // path, and this platform's actual AddrInUse rendering. Never substring-
    // match an arbitrary database, census, I/O, permission or timeout error.
    fn bind_collision(
        error: &StoreError,
        source: &Path,
        raft: std::net::SocketAddr,
        api: std::net::SocketAddr,
    ) -> bool {
        let StoreError::Migration(message) = error else {
            return false;
        };
        let suffix = format!(
            "; SQLite source {} remains available; rollback command: plurxd run",
            source.join(SQLITE_FILENAME).display()
        );
        let Some(message) = message.strip_suffix(&suffix) else {
            return false;
        };
        let occupied = std::io::Error::from_raw_os_error(libc::EADDRINUSE).to_string();
        [("raft", "Raft", raft), ("cluster API", "API", api)].into_iter().any(|(preflight, vendor, address)| {
            message == format!("database error: starting Hiqlite voter: Connect: bind {vendor} listener {address}: {occupied}")
                || message == format!("schema migration failed: the {preflight} port {address} is not available: {occupied}. Another plurxd on this host is using it — cluster ports are host-wide, so a second data directory needs its own raft_bind and api_bind")
        })
    }

    fn report(config: &Config, stage: &str, error: Option<String>) {
        let context = current_context();
        let path = context.directory.join(format!("{stage}.json"));
        json_file(
            &path,
            &Report {
                context,
                pid: std::process::id(),
                stage: stage.into(),
                source: config.storage.data_dir.clone(),
                raft: config.cluster.raft_bind,
                api: config.cluster.api_bind,
                error,
            },
        );
    }

    pub(super) async fn source_voter(config: &Config) -> SelectedStore {
        match startup_observer::select_applied_singleton(config).await {
            Ok(source) => {
                report(config, "acquired", None);
                source
            }
            Err(error)
                if bind_collision(
                    &error,
                    &config.storage.data_dir,
                    config.cluster.raft_bind,
                    config.cluster.api_bind,
                ) =>
            {
                let StoreError::Migration(message) = error else {
                    unreachable!("matched migration variant")
                };
                report(config, "refused", Some(message));
                std::process::exit(COLLISION);
            }
            Err(error) => panic!("source voter: {error:?}"),
        }
    }

    fn read_report(context: &Context, pid: u32, stage: &str) -> Result<Report, String> {
        let path = context.directory.join(format!("{stage}.json"));
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("missing {stage} report: {error}"))?;
        if !metadata.is_file() || metadata.len() > REPORT_LIMIT {
            return Err(format!("invalid {stage} report file"));
        }
        let report: Report =
            serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if report.context != *context
            || report.pid != pid
            || report.stage != stage
            || !report.source.starts_with(&context.state)
            || report.source == context.state
            || report
                .source
                .components()
                .any(|part| part == std::path::Component::ParentDir)
            || !report.raft.ip().is_loopback()
            || !report.api.ip().is_loopback()
            || report.raft.port() == 0
            || report.api.port() == 0
            || report.raft == report.api
        {
            return Err(format!("invalid {stage} report identity"));
        }
        Ok(report)
    }

    fn completed(context: &Context) {
        let mut acquired = read_report(context, std::process::id(), "acquired")
            .expect("actual source acquisition");
        acquired.stage = "completed".into();
        json_file(&context.directory.join("completed.json"), &acquired);
    }

    fn sizes(directory: &Path) -> u64 {
        ["stdout.log", "stderr.log"]
            .into_iter()
            .map(|name| {
                std::fs::metadata(directory.join(name))
                    .expect("owned child log")
                    .len()
            })
            .sum()
    }

    pub(super) fn failure_tail(directory: &Path) -> String {
        let mut output = String::new();
        for name in ["stdout.log", "stderr.log"] {
            let label = format!("--- restore child {name} tail ---\n");
            let mut file = std::fs::File::open(directory.join(name)).expect("owned failure log");
            let bytes = file.metadata().expect("failure log size").len().min(8192);
            file.seek(SeekFrom::End(-(bytes as i64)))
                .expect("seek bounded failure tail");
            let mut tail = Vec::new();
            file.take(bytes)
                .read_to_end(&mut tail)
                .expect("read bounded failure tail");
            let text = String::from_utf8_lossy(&tail);
            let mut end = text.len().min(8192 - label.len() - 1);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            output.push_str(&label);
            output.push_str(&text[..end]);
            output.push('\n');
        }
        output
    }

    fn wait(
        child: &mut OwnedChild,
        directory: &Path,
        started: Instant,
        timeout: Duration,
        limit: u64,
    ) -> Result<ExitStatus, String> {
        loop {
            let status = child
                .0
                .as_mut()
                .expect("owned child")
                .try_wait()
                .map_err(|error| error.to_string())?;
            // Check even a fast-exited child: neither exit0 nor reserved exit
            // can bypass the finite time and combined output admission bounds.
            let elapsed = started.elapsed();
            let bytes = sizes(directory);
            if let Some(status) = status {
                child
                    .0
                    .take()
                    .expect("exited child")
                    .wait()
                    .expect("reap exited child");
                if elapsed > timeout {
                    return Err("child deadline exceeded".into());
                }
                if bytes > limit {
                    return Err("child output limit exceeded".into());
                }
                return Ok(status);
            }
            if elapsed > timeout {
                return Err("child deadline exceeded".into());
            }
            if bytes > limit {
                return Err("child output limit exceeded".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn attempt(context: &Context, timeout: Duration, limit: u64) -> Attempt {
        let mut child_context = context.clone();
        if context.probe == Probe::WrongChild {
            child_context.case.push_str("-wrong");
        }
        let encoded = serde_json::to_string(&child_context).expect("encode candidate context");
        json_file(&context.directory.join("context.json"), &child_context);
        let stdout =
            std::fs::File::create(context.directory.join("stdout.log")).expect("owned stdout");
        let stderr =
            std::fs::File::create(context.directory.join("stderr.log")).expect("owned stderr");
        let started = Instant::now();
        let child = Command::new(std::env::current_exe().expect("current test binary"))
            .args(["--exact", &context.case, "--nocapture", "--test-threads=1"])
            .env(CHILD, encoded)
            .env("TMPDIR", &context.state)
            .env("TMP", &context.state)
            .env("TEMP", &context.state)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("owned restore child");
        let pid = child.id();
        let mut child = OwnedChild(Some(child));
        let status = wait(&mut child, &context.directory, started, timeout, limit);
        drop(child); // All outcomes reap before report validation or another candidate.
        let acquired = context.directory.join("acquired.json").exists();
        let mut record = Attempt {
            number: context.attempt,
            pid,
            directory: context.directory.clone(),
            status: format!("{status:?}"),
            acquired,
            collision: None,
            error: None,
        };
        record.error = match status {
            Err(error) => Some(error),
            Ok(status) if status.success() => read_report(context, pid, "acquired")
                .and_then(|acquired| {
                    let complete = read_report(context, pid, "completed")?;
                    if context.directory.join("refused.json").exists()
                        || acquired.error.is_some()
                        || complete.error.is_some()
                        || acquired.source != complete.source
                        || acquired.raft != complete.raft
                        || acquired.api != complete.api
                    {
                        Err("inconsistent completed body reports".into())
                    } else {
                        Ok(())
                    }
                })
                .err(),
            Ok(status)
                if status.code() == Some(COLLISION)
                    && !acquired
                    && !context.directory.join("completed.json").exists() =>
            {
                match read_report(context, pid, "refused") {
                    Ok(report)
                        if report.error.as_ref().is_some_and(|message| {
                            bind_collision(
                                &StoreError::Migration(message.clone()),
                                &report.source,
                                report.raft,
                                report.api,
                            )
                        }) =>
                    {
                        record.collision = Some(report);
                        None
                    }
                    _ => Some("reserved exit without exact source bind refusal".into()),
                }
            }
            Ok(_) if acquired => Some("child failed after source acquisition; no retry".into()),
            Ok(_) => Some("child failed outside recognized source acquisition".into()),
        };
        json_file(&context.directory.join("attempt.json"), &record);
        // Direct OS stdio survives ordinary passing libtest capture. Keep raw
        // bounded logs separately; print only structured candidate diagnostics.
        let diagnostic = serde_json::to_vec(&record).expect("encode acquisition diagnostic");
        let mut output = std::io::stdout().lock();
        output
            .write_all(b"RESTORE_ENDPOINT_ATTEMPT ")
            .expect("visible diagnostic");
        output.write_all(&diagnostic).expect("visible diagnostic");
        output.write_all(b"\n").expect("visible diagnostic");
        if record.error.is_some() {
            output
                .write_all(failure_tail(&context.directory).as_bytes())
                .expect("visible bounded original child failure");
        }
        std::fs::remove_dir_all(&context.state).expect("remove reaped candidate state");
        record
    }

    pub(super) fn run(
        case: &str,
        sharing: bool,
        probe: Probe,
        timeout: Duration,
        limit: u64,
    ) -> Run {
        assert!(
            std::env::var_os(CHILD).is_none(),
            "child cannot recursively allocate candidates"
        );
        let archive = tempfile::Builder::new()
            .prefix("plurx-restore-endpoints-")
            .tempdir()
            .expect("owned attempt archive")
            .keep()
            .canonicalize()
            .expect("canonical attempt archive");
        let mut result = Run {
            success: false,
            error: String::new(),
            attempts: 0,
            directories: Vec::new(),
            archive: archive.clone(),
        };
        for number in 1..=ATTEMPTS {
            let directory = archive.join(format!("attempt-{number}"));
            let state = directory.join("state");
            std::fs::create_dir_all(&state).expect("fresh candidate state");
            let context = Context {
                case: format!("cluster::migration::tests::{case}"),
                nonce: uuid::Uuid::now_v7().to_string(),
                attempt: number,
                directory: directory.clone(),
                state,
                sharing,
                probe: probe.clone(),
            };
            let record = attempt(&context, timeout, limit);
            result.attempts = number;
            result.directories.push(directory);
            if let Some(error) = record.error {
                result.error = error;
                break;
            }
            if record.collision.is_some() {
                result.error = format!("exhausted {ATTEMPTS} source endpoint candidates");
                continue;
            }
            result.success = true;
            result.error.clear();
            break;
        }
        json_file(&archive.join("summary.json"), &result);
        result
    }

    pub(super) fn actual(case: &str, sharing: bool) {
        if let Some(context) = context(case) {
            assert_eq!(context.probe, Probe::Actual);
            assert_eq!(context.sharing, sharing);
            if sharing {
                startup_observer::run_full_hiqlite_fixture(
                    "restore-sharing-census",
                    restore_archive_sharing_observation_fixture,
                );
            } else {
                startup_observer::run_full_hiqlite_fixture(
                    "k06-r1-restore",
                    restore_archive_observation_fixture,
                );
            }
            completed(&context);
            return;
        }
        let run = run(
            case,
            sharing,
            Probe::Actual,
            Duration::from_secs(180),
            LOG_LIMIT,
        );
        assert!(
            run.success,
            "restore observation failed without assertion retry: {run:?}"
        );
    }

    pub(super) fn probe_child(case: &str) -> bool {
        let Some(context) = context(case) else {
            return false;
        };
        assert_ne!(context.probe, Probe::Actual);
        if context.probe == Probe::MissingReport {
            std::process::exit(COLLISION);
        }
        if context.probe == Probe::Timeout {
            std::thread::sleep(Duration::from_secs(1));
            return true;
        }
        if context.probe == Probe::OutputLimit {
            std::io::stdout()
                .write_all(&[b'x'; 2048])
                .expect("controlled output");
            return true;
        }
        let candidate = context.clone();
        startup_observer::run_full_hiqlite_fixture(
            "restore-endpoint-control",
            move || async move {
                let context = candidate;
                install_default_crypto_provider();
                let source = source_directory();
                let config = membership_test_config(source.path());
                if matches!(context.probe, Probe::Unknown | Probe::WrongEndpoint) {
                    let occupied = std::io::Error::from_raw_os_error(libc::EADDRINUSE);
                    let address = if context.probe == Probe::WrongEndpoint {
                        config.cluster.api_bind
                    } else {
                        config.cluster.raft_bind
                    };
                    let message = format!(
                        "database error: starting Hiqlite voter: Connect: bind Raft listener {address}: {occupied}; SQLite source {} remains available; rollback command: plurxd run",
                        source.path().join(SQLITE_FILENAME).display()
                    );
                    report(
                        &config,
                        "refused",
                        Some(if context.probe == Probe::Unknown {
                            format!("census failed: {message}")
                        } else {
                            message
                        }),
                    );
                    std::process::exit(COLLISION);
                }
                let _collider = (context.probe == Probe::AlwaysCollision
                    || (context.probe == Probe::FirstCollision && context.attempt == 1))
                    .then(|| {
                        std::net::TcpListener::bind(config.cluster.raft_bind)
                            .expect("controlled real source collision")
                    });
                drop(
                    SqliteStore::open(&source.path().join(SQLITE_FILENAME))
                        .expect("controlled legacy source"),
                );
                let selected = source_voter(&config).await;
                if context.probe == Probe::AcquiredPanic {
                    panic!("RESTORE_POST_ACQUISITION_FAILURE_SENTINEL");
                }
                if context.probe == Probe::AcquiredThenRefuse {
                    let message = format!(
                        "database error: starting Hiqlite voter: Connect: bind Raft listener {}: {}; SQLite source {} remains available; rollback command: plurxd run",
                        config.cluster.raft_bind,
                        std::io::Error::from_raw_os_error(libc::EADDRINUSE),
                        source.path().join(SQLITE_FILENAME).display()
                    );
                    report(&config, "refused", Some(message));
                    std::process::exit(COLLISION);
                }
                selected.shutdown().await.expect("control source shutdown");
            },
        );
        completed(&context);
        true
    }
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn restore_source_endpoints_reacquire_only_before_the_body_and_exhaust_finitely() {
    const CASE: &str =
        "restore_source_endpoints_reacquire_only_before_the_body_and_exhaust_finitely";
    if restore_endpoints::probe_child(CASE) {
        return;
    }
    let acquired = restore_endpoints::run(
        CASE,
        false,
        restore_endpoints::Probe::FirstCollision,
        Duration::from_secs(180),
        8 * 1024 * 1024,
    );
    assert!(acquired.success, "{acquired:?}");
    assert_eq!(acquired.attempts, 2);
    assert_eq!(
        acquired
            .directories
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    for directory in &acquired.directories {
        assert!(!directory.join("state").exists());
    }
    assert!(!acquired.directories[0].join("acquired.json").exists());
    assert!(acquired.directories[1].join("completed.json").exists());
    let exhausted = restore_endpoints::run(
        CASE,
        false,
        restore_endpoints::Probe::AlwaysCollision,
        Duration::from_secs(180),
        8 * 1024 * 1024,
    );
    assert!(!exhausted.success);
    assert_eq!(exhausted.attempts, 5);
    assert!(exhausted.error.contains("exhausted"));
    assert!(exhausted
        .directories
        .iter()
        .all(|path| !path.join("acquired.json").exists() && !path.join("state").exists()));
    assert!(!exhausted.directories[4]
        .with_file_name("attempt-6")
        .exists());
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn restore_source_endpoints_refuse_unknown_wrong_endpoint_and_post_acquisition_retries() {
    const CASE: &str =
        "restore_source_endpoints_refuse_unknown_wrong_endpoint_and_post_acquisition_retries";
    if restore_endpoints::probe_child(CASE) {
        return;
    }
    for probe in [
        restore_endpoints::Probe::Unknown,
        restore_endpoints::Probe::WrongEndpoint,
        restore_endpoints::Probe::MissingReport,
        restore_endpoints::Probe::AcquiredThenRefuse,
        restore_endpoints::Probe::AcquiredPanic,
        restore_endpoints::Probe::WrongChild,
    ] {
        let acquired_failure = matches!(
            probe,
            restore_endpoints::Probe::AcquiredThenRefuse | restore_endpoints::Probe::AcquiredPanic
        );
        let panic_failure = probe == restore_endpoints::Probe::AcquiredPanic;
        let run = restore_endpoints::run(
            CASE,
            false,
            probe,
            Duration::from_secs(180),
            8 * 1024 * 1024,
        );
        assert!(!run.success, "{run:?}");
        assert_eq!(run.attempts, 1, "{run:?}");
        assert!(!run.directories[0].join("state").exists());
        if acquired_failure {
            assert!(run.error.contains("after source acquisition"), "{run:?}");
            assert!(run.directories[0].join("acquired.json").exists());
            assert!(!run.directories[0].join("completed.json").exists());
        }
        if panic_failure {
            let tail = restore_endpoints::failure_tail(&run.directories[0]);
            assert!(tail.contains("RESTORE_POST_ACQUISITION_FAILURE_SENTINEL"));
            assert!(tail.len() <= 16 * 1024);
        }
    }
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn restore_source_endpoints_deadline_and_fast_output_failure_are_not_retried() {
    const CASE: &str = "restore_source_endpoints_deadline_and_fast_output_failure_are_not_retried";
    if restore_endpoints::probe_child(CASE) {
        return;
    }
    for (probe, timeout, limit, expected) in [
        (
            restore_endpoints::Probe::Timeout,
            Duration::from_millis(100),
            8 * 1024 * 1024,
            "deadline",
        ),
        (
            restore_endpoints::Probe::OutputLimit,
            Duration::from_secs(180),
            1024,
            "output limit",
        ),
    ] {
        let run = restore_endpoints::run(CASE, false, probe, timeout, limit);
        assert!(!run.success);
        assert_eq!(run.attempts, 1);
        assert!(run.error.contains(expected), "{run:?}");
        assert!(!run.directories[0].join("state").exists());
    }
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn restore_archive_from_real_snapshot_starts_as_the_only_voter() {
    restore_endpoints::actual(
        "restore_archive_from_real_snapshot_starts_as_the_only_voter",
        false,
    );
}

#[cfg(feature = "hiqlite-store")]
async fn restore_archive_observation_fixture() {
    restore_archive_observation_fixture_with_sharing(false).await;
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn restore_archive_with_retired_purpose_keys_admits_new_node_before_key_census() {
    restore_endpoints::actual(
        "restore_archive_with_retired_purpose_keys_admits_new_node_before_key_census",
        true,
    );
}

#[cfg(feature = "hiqlite-store")]
async fn restore_archive_sharing_observation_fixture() {
    restore_archive_observation_fixture_with_sharing(true).await;
}

#[cfg(feature = "hiqlite-store")]
async fn restore_archive_observation_fixture_with_sharing(sharing: bool) {
    install_default_crypto_provider();

    let source_dir = restore_endpoints::source_directory();
    let source_config = membership_test_config(source_dir.path());
    drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("legacy source store"));
    let mut source = restore_endpoints::source_voter(&source_config).await;
    if sharing {
        source
            .store
            .put_setting("sharing_enabled", "1")
            .await
            .expect("actual enabled setting");
        assert!(source
            .prepare_source_schema_before_serving()
            .await
            .expect("actual Source and purpose activation"));
    }
    let client = source.local_client().expect("source local client");
    let applied = client
        .trigger_db_snapshot()
        .await
        .expect("trigger snapshot");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let metrics = client.metrics_db().await.expect("snapshot metrics");
            if metrics
                .snapshot
                .as_ref()
                .is_some_and(|log| log.index >= applied)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot publication deadline");
    let snapshots = source_dir
        .path()
        .join(HIQLITE_ACTIVE_DIRNAME)
        .join("state_machine/snapshots");
    let snapshot_id = std::fs::read_to_string(snapshots.join("current")).expect("snapshot pointer");
    let destination = tempfile::tempdir().expect("backup destination");
    let key_path = source_dir
        .path()
        .join(crate::secrets::CREDENTIAL_KEY_FILENAME);
    let (artifact, _) = build_cluster_backup_artifact(
        File::open(snapshots.join(snapshot_id.trim())).expect("published snapshot"),
        destination.path(),
        source_dir.path(),
        Some(&key_path),
        &source.identity.node_id,
        Some(source.credential_key.id()),
        "test-build",
        applied,
    )
    .expect("portable artefact from real snapshot");
    source.shutdown().await.expect("source voter shutdown");
    drop(source);

    let target_parent = tempfile::tempdir().expect("target parent");
    let target = target_parent.path().join("restored");
    let target_config = membership_test_config(&target);
    let report = restore_cluster_backup_archive(&artifact, &target_config, &[], None)
        .await
        .expect("restore real snapshot");
    if sharing {
        let connection = Connection::open(
            target
                .join(HIQLITE_ACTIVE_DIRNAME)
                .join("state_machine/db")
                .join(HIQLITE_DATABASE_FILENAME),
        )
        .expect("offline restored projection");
        let observation: (String, i64, i64) = connection
            .query_row(
                "SELECT node_id,raft_id,last_seen_at FROM cluster_nodes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("only restored observation identity");
        assert_eq!(Some(observation.0.as_str()), report.node_id.as_deref());
        assert_eq!(
            (observation.1, observation.2),
            (super::super::SINGLE_VOTER_RAFT_ID as i64, 0)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cluster_node_capabilities",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("no fabricated capabilities"),
            0
        );
        assert!(connection.execute("INSERT INTO cluster_nodes(node_id,raft_id,raft_address,api_address,last_seen_at,removed_at,role) VALUES('unproved-node',99,'raft','api',0,NULL,'voter')", []).is_err(), "canonical join admission must still refuse arbitrary nodes");
        for expected in super::super::membership::sharing_member_admission_guard_schema()
            .into_iter()
            .filter(|sql| sql.contains("_node_insert_guard "))
        {
            let name = expected.split_whitespace().nth(5).expect("canonical name");
            let actual: String = connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name=?1",
                    [name],
                    |row| row.get(0),
                )
                .expect("restored canonical guard");
            assert_eq!(actual, expected.replace(" IF NOT EXISTS", ""));
        }
    }
    let restored = startup_observer::select_applied_singleton(&target_config)
        .await
        .expect("restored target starts");
    let status = restored
        .membership_manager()
        .status()
        .await
        .expect("restored membership projection");
    assert_eq!(status.nodes.len(), 1);
    assert_eq!(
        status.local_node_id,
        report.node_id.expect("restored node id")
    );
    assert!(status.nodes[0].is_voter);
    assert!(status.nodes[0].is_leader);
    if sharing {
        assert_eq!(
            restored
                .store
                .get_setting("sharing_enabled")
                .await
                .expect("fenced setting")
                .as_deref(),
            Some("false")
        );
        assert_eq!(
            restored
                .store
                .verify_sharing_purpose_material(&restored.credential_key)
                .await
                .expect("strict retired-key census"),
            crate::store::sharing_purpose_keys::PurposeKeyInstallation::NotReady
        );
        let connection = Connection::open(
            target
                .join(HIQLITE_ACTIVE_DIRNAME)
                .join("state_machine/db")
                .join(HIQLITE_DATABASE_FILENAME),
        )
        .expect("actual reopened image");
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM sharing_identity", [], |row| row
                    .get::<_, i64>(0))
                .expect("removed sharing identity"),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sharing_purpose_key_archive",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("retained purpose pair"),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sharing_purpose_census_intents",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("completed exact census"),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cluster_nodes WHERE node_id=?1 AND removed_at IS NULL",
                    [&restored.identity.node_id],
                    |row| row.get::<_, i64>(0)
                )
                .expect("actual newly admitted node"),
            1
        );
    }
    restored.shutdown().await.expect("restored voter shutdown");
}

/// Every table and row the restored image holds, read through the local
/// replica (never the leader) so a joiner is judged on what it applied.
#[cfg(feature = "hiqlite-store")]
async fn local_tables_and_marker(client: &Client) -> (BTreeSet<String>, Option<String>) {
    struct Text(Option<String>);
    impl From<&mut hiqlite::Row<'_>> for Text {
        fn from(row: &mut hiqlite::Row<'_>) -> Self {
            Self(row.get("text"))
        }
    }
    let tables = client
        .query_map::<Text, _>(
            "SELECT name AS text FROM sqlite_master WHERE type = 'table'",
            hiqlite::params!(),
        )
        .await
        .map(|rows| rows.into_iter().filter_map(|row| row.0).collect())
        .unwrap_or_default();
    let marker = client
        .query_map::<Text, _>(
            "SELECT value AS text FROM settings WHERE key = 'restore.join.marker'",
            hiqlite::params!(),
        )
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row.0);
    (tables, marker)
}

/// Drill F4 (2026-10-11): a restored first voter installed its image under
/// a brand-new Raft log and published no snapshot, so OpenRaft replicated
/// a joiner from that log's first entry and never sent it the image. The
/// joiner became a voter with 32 of 163 tables and no `users`. The
/// restored voter must publish a transferable snapshot before it serves,
/// and a voter joining it must hold every table and the restored rows.
#[cfg(feature = "hiqlite-store")]
#[test]
fn restored_first_voter_seeds_a_joining_voter_with_the_restored_image() {
    startup_observer::run_plural_fixture("f4-restore-join", restored_voter_join_fixture);
}

#[cfg(feature = "hiqlite-store")]
async fn restored_voter_join_fixture() {
    install_default_crypto_provider();
    let observed = startup_observer::MeasuredPeers::default();

    let source_dir = tempfile::tempdir().expect("source data dir");
    let source_config = membership_test_config(source_dir.path());
    drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("legacy source store"));
    let source = startup_observer::select_applied_singleton(&source_config)
        .await
        .expect("source voter");
    source
        .store
        .put_setting("restore.join.marker", "restored")
        .await
        .expect("restored marker row");
    let client = source.local_client().expect("source local client");
    let applied = client
        .trigger_db_snapshot()
        .await
        .expect("trigger snapshot");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let metrics = client.metrics_db().await.expect("snapshot metrics");
            if metrics
                .snapshot
                .as_ref()
                .is_some_and(|log| log.index >= applied)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot publication deadline");
    let snapshots = source_dir
        .path()
        .join(HIQLITE_ACTIVE_DIRNAME)
        .join("state_machine/snapshots");
    let snapshot_id = std::fs::read_to_string(snapshots.join("current")).expect("snapshot pointer");
    let destination = tempfile::tempdir().expect("backup destination");
    let key_path = source_dir
        .path()
        .join(crate::secrets::CREDENTIAL_KEY_FILENAME);
    let (artifact, _) = build_cluster_backup_artifact(
        File::open(snapshots.join(snapshot_id.trim())).expect("published snapshot"),
        destination.path(),
        source_dir.path(),
        Some(&key_path),
        &source.identity.node_id,
        Some(source.credential_key.id()),
        "test-build",
        applied,
    )
    .expect("portable artefact from real snapshot");
    source.shutdown().await.expect("source voter shutdown");
    drop(source);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("restored coordinator listener");
    let coordinator_addr = listener.local_addr().expect("coordinator address");
    let target_parent = tempfile::tempdir().expect("target parent");
    let target = target_parent.path().join("restored");
    let mut target_config = membership_test_config(&target);
    target_config.server.bind = coordinator_addr;
    target_config.cluster.join_url = format!("http://{coordinator_addr}");
    restore_cluster_backup_archive(&artifact, &target_config, &[], None)
        .await
        .expect("restore real snapshot");
    let restored = observed
        .select(&target_config, false)
        .await
        .expect("restored voter starts");
    let restored_client = restored.local_client().expect("restored client");
    let metrics = restored_client
        .metrics_db()
        .await
        .expect("restored metrics");
    assert!(
        metrics.purged.is_some() && metrics.snapshot.is_some(),
        "the restored voter opened without a transferable snapshot: snapshot {:?}, purged {:?}",
        metrics.snapshot,
        metrics.purged
    );
    assert!(
        !super::super::membership::restored_image_lacks_join_snapshot(&restored_client)
            .await
            .expect("restored admission check")
    );
    let (restored_tables, restored_marker) = local_tables_and_marker(&restored_client).await;
    assert_eq!(restored_marker.as_deref(), Some("restored"));
    assert!(restored_tables.contains("users"));

    let coordinator = restored.membership_manager();
    let app = Router::new()
        .route("/api/v1/cluster/join/redeem", post(redeem_join_for_test))
        .route(
            "/api/v1/cluster/join/finalize",
            post(finalize_join_for_test),
        )
        .with_state(coordinator.clone())
        .merge(startup_observer::MeasuredPeers::route(coordinator.clone()));
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve restored join coordinator");
    });

    let issued = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("a restored voter with a transferable snapshot issues join tokens");
    let joining_dir = tempfile::tempdir().expect("joining data dir");
    let token_path = joining_dir.path().join("join.token");
    std::fs::write(&token_path, format!("{}\n", issued.token)).expect("joining token file");
    let mut joining_config = membership_test_config(joining_dir.path());
    joining_config.server.bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("joining clock bind");
    joining_config.cluster.join_token_file = token_path;
    // Without a transferable snapshot the joiner never finishes opening
    // a store whose schema it was never sent; bound that instead of
    // hanging the suite.
    let joined = tokio::time::timeout(
        Duration::from_secs(90),
        observed.select(&joining_config, true),
    )
    .await
    .expect("the joiner never finished joining the restored cluster")
    .expect("join the restored cluster");
    let joined_client = joined.local_client().expect("joined client");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (joined_tables, joined_marker) = loop {
        let observed = local_tables_and_marker(&joined_client).await;
        if (observed.0 == restored_tables && observed.1.is_some())
            || tokio::time::Instant::now() >= deadline
        {
            break observed;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        joined_tables.contains("users"),
        "the joiner became a member without the restored image: {} of {} tables",
        joined_tables.len(),
        restored_tables.len()
    );
    assert_eq!(joined_tables, restored_tables);
    assert_eq!(joined_marker.as_deref(), Some("restored"));

    http_task.abort();
    joined.shutdown().await.expect("drain joined voter");
    restored.shutdown().await.expect("drain restored voter");
}

/// The join-admission half of F4, as defence in depth behind the startup
/// snapshot: while a node's Raft log still begins at a restore, neither
/// issuance nor redemption admits a node, and both resume once a snapshot
/// has purged the log beneath it. A cluster without a restore generation
/// is never refused, snapshot or not.
#[cfg(feature = "hiqlite-store")]
#[test]
fn join_admission_refuses_a_restored_image_until_a_snapshot_covers_it() {
    startup_observer::run_full_hiqlite_fixture(
        "f4-restore-admission",
        restored_join_admission_fixture,
    );
}

#[cfg(feature = "hiqlite-store")]
async fn restored_join_admission_fixture() {
    install_default_crypto_provider();
    let source_dir = tempfile::tempdir().expect("source data dir");
    let mut config = membership_test_config(source_dir.path());
    // No advertised address: no readdress, so no snapshot at open.
    config.cluster.advertise_host.clear();
    drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let source = startup_observer::select_applied_singleton(&config)
        .await
        .expect("activate source voter");
    let client = source.local_client().expect("source client");
    assert!(
        client
            .metrics_db()
            .await
            .expect("source metrics")
            .purged
            .is_none(),
        "fixture precondition: a fresh voter's log still begins at its first entry"
    );
    let coordinator = source.membership_manager();
    let issued = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("an unrestored cluster is never refused for lacking a snapshot");

    source
        .store
        .put_setting(crate::store::keys::CLUSTER_RESTORE_GENERATION, "1")
        .await
        .expect("simulate a restored lineage");
    assert!(matches!(
        coordinator.issue_token(Duration::from_secs(120)).await,
        Err(crate::cluster::membership::MembershipError::RestoredImageNotTransferable)
    ));
    let joining_dir = tempfile::tempdir().expect("joining data dir");
    let joining_config = membership_test_config(joining_dir.path());
    let staged = configured_local_peer(&joining_config, issued.raft_id).expect("staged peer");
    let request = RedeemJoinRequest {
        token_digest: join_token_digest(&issued.token),
        raft_id: issued.raft_id,
        node_id: uuid::Uuid::now_v7().to_string(),
        hostname: "restored-joiner".to_owned(),
        raft_address: staged.raft_address,
        api_address: staged.api_address,
        http_base: configured_artwork_url(&joining_config).expect("joining origin"),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
        protocol_min: crate::store::AUTH_PROTOCOL_MIN,
        protocol_max: crate::store::AUTH_PROTOCOL_MAX,
        sharing: Default::default(),
        live_tv_v1: true,
    };
    assert!(matches!(
        coordinator.redeem(&request).await,
        Err(crate::cluster::membership::MembershipError::RestoredImageNotTransferable)
    ));

    let applied = client
        .trigger_db_snapshot()
        .await
        .expect("trigger snapshot");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let metrics = client.metrics_db().await.expect("snapshot metrics");
            if metrics.purged.is_some()
                && metrics
                    .snapshot
                    .as_ref()
                    .is_some_and(|log| log.index >= applied)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot and purge deadline");
    coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("a snapshot covering the restored image re-admits joins");
    source.shutdown().await.expect("drain source voter");
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn an_accepted_join_that_never_answers_is_reported_as_ambiguous() {
    use axum::routing::post;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("join mock listener");
    let address = listener.local_addr().expect("join mock address");
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/join",
            post(|| async { std::future::pending::<axum::http::StatusCode>().await }),
        );
        let _ = axum::serve(listener, app).await;
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(50))
        .timeout(Duration::from_millis(50))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("bounded test client");
    let started = std::time::Instant::now();

    let error = post_join_url(
        &client,
        format!("http://{address}/join"),
        &serde_json::json!({ "token_digest": "accepted" }),
    )
    .await
    .expect_err("the missing response is ambiguous");

    assert!(matches!(error, StoreError::Migration(_)));
    assert!(error.to_string().contains("join_request_ambiguous"));
    assert!(error.to_string().contains("re-run the same staged join"));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn joined_store_open_retries_only_the_explicit_quorum_gap() {
    assert_eq!(REPLICATED_LEADER_RECOVERY_BUDGET, Duration::from_secs(16));
    assert!(join_store_open_quorum_unavailable(&StoreError::Database(
        "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
    )));

    for terminal in [
        StoreError::Database("replicated store operation timed out".to_owned()),
        StoreError::Database("CheckIsLeaderError: leader changed".to_owned()),
        StoreError::Database("not enough for a quorum".to_owned()),
        StoreError::Migration("cluster compatibility marker is missing".to_owned()),
    ] {
        assert!(
            !join_store_open_quorum_unavailable(&terminal),
            "unexpected retry for {terminal}"
        );
    }
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn join_url_preserves_reverse_proxy_prefix_but_peer_url_requires_an_origin() {
    let mut config = Config::default();
    config.cluster.join_url = "https://cluster.example/plurx/".to_owned();
    assert_eq!(
        configured_join_url(&config).expect("path-prefixed join coordinator"),
        "https://cluster.example/plurx"
    );

    config.cluster.artwork_url = config.cluster.join_url.clone();
    assert!(
        configured_artwork_url(&config).is_err(),
        "peer routes must not inherit a reverse-proxy path prefix"
    );

    for invalid in [
        "https://user:secret@cluster.example/plurx",
        "https://cluster.example/plurx?redirect=elsewhere",
        "file:///tmp/plurx.sock",
    ] {
        config.cluster.join_url = invalid.to_owned();
        assert!(configured_join_url(&config).is_err(), "accepted {invalid}");
    }
}
use crate::store::{SettingsStore, SqliteStore};

#[cfg(feature = "hiqlite-store")]
use axum::extract::State;
#[cfg(feature = "hiqlite-store")]
use axum::http::StatusCode;
#[cfg(feature = "hiqlite-store")]
use axum::response::{IntoResponse, Response};
#[cfg(feature = "hiqlite-store")]
use axum::routing::post;
#[cfg(feature = "hiqlite-store")]
use axum::{Json, Router};
#[cfg(feature = "hiqlite-store")]
use serde_json::json;

/// The claim a data-directory refusal is entitled to make. Nothing that did
/// not observe a *different* live process may print it.
#[cfg(feature = "hiqlite-store")]
const SECOND_DAEMON_VERDICT: &str = "another plurxd process already owns the data directory";

/// Raft ids are durable identities, not positions in the bootstrap vector.
/// A live token can reserve id 2 while id 3 joins first, so Hiqlite must
/// accept the unique sparse roster rather than requiring a duplicate pad.
#[cfg(feature = "hiqlite-store")]
#[test]
fn hiqlite_accepts_a_unique_sparse_node_roster() {
    let peers = [
        ClusterPeer {
            raft_id: 1,
            raft_address: "127.0.0.1:32401".to_owned(),
            api_address: "127.0.0.1:32402".to_owned(),
        },
        ClusterPeer {
            raft_id: 3,
            raft_address: "127.0.0.1:32501".to_owned(),
            api_address: "127.0.0.1:32502".to_owned(),
        },
    ];
    let nodes = hiqlite_nodes_for_voter(&peers, 3).expect("build sparse connection roster");
    let config = NodeConfig {
        node_id: 3,
        nodes,
        secret_raft: "0123456789abcdef".to_owned(),
        secret_api: "fedcba9876543210".to_owned(),
        ..NodeConfig::default()
    };

    config
        .is_valid()
        .expect("sparse Raft ids must be selected by id");
    assert_eq!(
        config.nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
        vec![1, 3],
        "the compatibility path must not duplicate a connection target"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn production_timing_admits_recovery_after_upgrade_and_clean_rolling_restarts() {
    let mut config = Config::default();
    config.cluster.read_pool_size = 16;
    let defaults = production_hiqlite_defaults(&config);
    assert_eq!(defaults.read_pool_size, 16);
    assert_eq!(defaults.wal_size, HIQLITE_WAL_SIZE_BYTES);
    assert_eq!(defaults.raft_config.heartbeat_interval, 800);
    assert_eq!(defaults.raft_config.election_timeout_min, 2_400);
    assert_eq!(defaults.raft_config.election_timeout_max, 4_000);
    assert!(
        defaults.raft_config.election_timeout_min >= defaults.raft_config.heartbeat_interval * 3
    );
    assert!(
        defaults.raft_config.heartbeat_interval * 3 / 2 < 1_500,
        "a new leader must beat the previous release's election floor"
    );
    let vote_soft_ttl = defaults.raft_config.election_timeout_min * 3 / 4;
    assert!(
        u128::from(defaults.raft_config.election_timeout_max + vote_soft_ttl)
            < hiqlite::LEADER_DISCOVERY_TIMEOUT.as_millis(),
        "detached leader discovery must outlive the longest election and vote round"
    );
    assert_eq!(
        hiqlite::LEADER_DISCOVERY_TIMEOUT + hiqlite::LEADER_STREAM_HANDOFF_TIMEOUT,
        hiqlite::LEADER_RETRY_RECOVERY_TIMEOUT,
        "the recovery bound must include discovery and an acknowledged replacement stream"
    );
    assert!(
        hiqlite::LEADER_STREAM_CONNECT_TIMEOUT < hiqlite::LEADER_STREAM_HANDOFF_TIMEOUT,
        "the end-to-end handoff must leave time around one connection attempt"
    );
    assert!(
        hiqlite::LEADER_RETRY_RECOVERY_TIMEOUT
            + Duration::from_millis(defaults.raft_config.heartbeat_interval)
            < REPLICATED_LEADER_RECOVERY_BUDGET,
        "leader discovery, stream reconnect, and one AppendEntries round must fit the recovery budget"
    );
    assert_eq!(defaults.raft_config.max_in_snapshot_log_to_keep, 1);
    assert_eq!(defaults.raft_config.purge_batch_size, 1);
    assert_eq!(defaults.raft_config.install_snapshot_timeout, 120_000);
    assert_eq!(defaults.snapshot_chunk_timeout, Duration::from_secs(30));
    assert_eq!(
        defaults.snapshot_transfer_timeout,
        Duration::from_secs(1_200)
    );
    assert_eq!(
        snapshot_catchup_timeout(
            config.cluster.snapshot_transfer_timeout_secs,
            config.cluster.install_snapshot_timeout_secs,
        ),
        Duration::from_secs(1_365)
    );
    assert!(
        format!("{:?}", defaults.raft_config.snapshot_policy).contains("10000"),
        "snapshot policy must stay at 10,000 entries"
    );
    assert_eq!(
        format!("{:?}", defaults.wal_sync),
        format!("{:?}", NodeConfig::default().wal_sync),
        "read-pool tuning must not change immediate-async WAL sync"
    );

    config.cluster.snapshot_chunk_timeout_secs = 45;
    config.cluster.snapshot_transfer_timeout_secs = 900;
    config.cluster.install_snapshot_timeout_secs = 300;
    let tuned = production_hiqlite_defaults(&config);
    assert_eq!(tuned.raft_config.install_snapshot_timeout, 300_000);
    assert_eq!(tuned.snapshot_chunk_timeout, Duration::from_secs(45));
    assert_eq!(tuned.snapshot_transfer_timeout, Duration::from_secs(900));
    assert_eq!(
        snapshot_catchup_timeout(
            config.cluster.snapshot_transfer_timeout_secs,
            config.cluster.install_snapshot_timeout_secs,
        ),
        Duration::from_secs(1_245)
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn production_hiqlite_snapshot_cadence_preserves_default_and_retention() {
    let mut config = Config::default();
    for entries in [10_000, 1_000, 200_000] {
        config.cluster.logs_until_snapshot = entries;
        let defaults = production_hiqlite_defaults(&config);
        assert_eq!(
            defaults.raft_config.snapshot_policy,
            hiqlite::SnapshotPolicy::LogsSinceLast(entries)
        );
        assert_eq!(defaults.raft_config.max_in_snapshot_log_to_keep, 1);
        assert_eq!(defaults.wal_size, HIQLITE_WAL_SIZE_BYTES);
    }
    let legacy = production_hiqlite_defaults_with_read_pool(4);
    assert_eq!(
        legacy.raft_config.snapshot_policy,
        hiqlite::SnapshotPolicy::LogsSinceLast(10_000)
    );
}

/// Once the active voter exists, every later initialization error must
/// drain it before returning. Otherwise the process exits with Hiqlite's
/// state-machine crash sentinel present and the next boot destroys the
/// local state machine to rebuild it from peers.
#[cfg(feature = "hiqlite-store")]
#[test]
fn a_post_start_initialization_failure_leaves_no_crash_sentinel() {
    // The owned 8 MiB stack convention the other full-startup tests use:
    // a whole daemon selection is deeper than a 2 MiB debug test thread.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build post-start failure runtime")
                .block_on(Box::pin(async {
                    install_default_crypto_provider();

                    let dir = tempfile::tempdir().expect("post-start failure data dir");
                    let config = membership_test_config(dir.path());
                    drop(
                        SqliteStore::open(&dir.path().join(SQLITE_FILENAME))
                            .expect("source SQLite"),
                    );
                    write_private_file(
                        &dir.path().join(ACTIVITY_SIGNING_KEY_FILENAME),
                        b"not-hexadecimal\n",
                    )
                    .expect("write invalid activity key");

                    // Boxed like production's `open_active_store`: the full selection
                    // state machine is deeper than a 2 MiB debug test thread allows.
                    let error = Box::pin(select_daemon_store(&config))
                        .await
                        .err()
                        .expect("the invalid post-start key must refuse activation")
                        .to_string();
                    assert!(
                        error.contains(ACTIVITY_SIGNING_KEY_FILENAME)
                            && error.contains("is malformed"),
                        "the refusal must occur after the voter starts: {error}"
                    );
                    assert!(
                        !dir.path()
                            .join(HIQLITE_ACTIVE_DIRNAME)
                            .join("state_machine")
                            .join("lock")
                            .exists(),
                        "a handled initialization error must not look like a process crash"
                    );
                }));
        })
        .expect("spawn post-start failure thread")
        .join()
        .expect("post-start failure thread");
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn activation_marker_accepts_only_the_supported_upgrade_window() {
    let marker = |replicated_schema_version| ActivationMarker {
        marker_version: ACTIVATION_MARKER_VERSION,
        cluster_id: "cluster-a".to_owned(),
        source_backup_sha256: "0".repeat(64),
        source_schema_version: 20,
        replicated_schema_version,
        imported_rows: 0,
        table_hashes: vec![SqliteImportTableDigest {
            table: "settings".to_owned(),
            row_count: 0,
            sha256: "0".repeat(64),
        }],
        admitted_role: None,
    };
    marker(AUTH_SCHEMA_MIGRATION_SOURCE)
        .validate()
        .expect("v5 marker must reach the complete daemon migration chain");
    marker(AUTH_SCHEMA_MIGRATION_SOURCE + 1)
        .validate()
        .expect("v6 marker must reach the book-facts migration");
    marker(AUTH_SCHEMA_VERSION)
        .validate()
        .expect("current marker");
    for unsupported in [AUTH_SCHEMA_MIGRATION_SOURCE - 1, AUTH_SCHEMA_VERSION + 1] {
        let error = marker(unsupported)
            .validate()
            .expect_err("unsupported marker must fail before voter startup");
        assert!(error.to_string().contains("incomplete"), "{error}");
    }

    // Version 0 has never been a valid writer-produced marker format.
    {
        let zero = ActivationMarker {
            marker_version: 0,
            cluster_id: "cluster-a".to_owned(),
            source_backup_sha256: "0".repeat(64),
            source_schema_version: 20,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 0,
            table_hashes: vec![SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 0,
                sha256: "0".repeat(64),
            }],
            admitted_role: None,
        };
        let error = zero
            .validate()
            .expect_err("marker version 0 must fail closed");
        assert!(error.to_string().contains("unsupported"), "{error}");
    }
}

/// A predecessor still closing its handle is not a second daemon.
///
/// Regression for #374. `acquire_daemon_lock` took the advisory lock
/// non-blockingly, so
/// `daemon_join_refuses_occupied_and_expired_targets_then_resumes_finalization`
/// re-activating the same data directory printed [`SECOND_DAEMON_VERDICT`]
/// whenever the previous activation's handle had not finished dropping.
/// Unloaded the drop won that race; under gate-parallel CI load it did not,
/// and `rust-gate` failed on diffs that cannot reach this code — a load
/// artifact reported as a production-grade double-start.
///
/// Exercises the real production entry point and its real window, because
/// the window is the correction. Reverting to a single non-blocking attempt
/// fails here immediately.
#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn a_still_closing_predecessor_lock_is_not_reported_as_a_second_daemon() {
    let data = tempfile::tempdir().expect("daemon lock dir");
    let holder = acquire_daemon_lock(data.path())
        .await
        .expect("the first owner takes the lock");

    // Well inside DAEMON_LOCK_ACQUIRE_WINDOW, and the whole point of it:
    // the incoming owner cannot know this handle is about to close.
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        drop(holder);
    });

    let acquired = acquire_daemon_lock(data.path()).await;
    release.await.expect("predecessor release task");
    let error = match acquired {
        Ok(_lock) => return,
        Err(error) => error,
    };
    panic!(
        "a predecessor that was still closing its handle was refused: {error}. The \
             data-directory lock must wait out a departing owner rather than read a \
             not-yet-closed handle as a second running daemon"
    );
}

/// The refusal survives, and it does not conflate the two conditions.
///
/// The owner here is this test process, so the lock is genuinely held for
/// the whole window and must still be refused — but calling that "another
/// plurxd process" would send a reader hunting a process that does not
/// exist. A short explicit window keeps this from costing five seconds;
/// the production window is exercised by the regression above.
#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn a_lock_held_for_the_whole_window_is_refused_without_blaming_another_process() {
    let data = tempfile::tempdir().expect("daemon lock dir");
    let _holder = acquire_daemon_lock(data.path())
        .await
        .expect("the owner takes the lock");
    assert_eq!(
        read_daemon_lock_holder(&data.path().join(DAEMON_LOCK_FILENAME)),
        DaemonLockHolder::ThisProcess(std::process::id()),
        "an owner must record itself so a contender's refusal can name it"
    );

    let message = acquire_daemon_lock_within(data.path(), Duration::from_millis(200))
        .await
        .expect_err("a lock held for the whole window must still be refused")
        .to_string();
    assert!(
        !message.contains(SECOND_DAEMON_VERDICT),
        "our own still-open handle was reported as a second daemon: {message}"
    );
    assert!(
        message.contains(&format!("pid {}", std::process::id())),
        "a refusal must name the recorded holder: {message}"
    );
}

/// A genuinely different owner keeps the operator-facing verdict.
///
/// `crates/plurxd/tests/cluster_activation.rs` proves this end to end with
/// two real processes; this pins the text and the classification so the
/// #374 correction cannot quietly stop calling a real double-start what it
/// is. An unrecorded owner is treated the same way: the lock is held and
/// nothing says it is ours.
#[cfg(feature = "hiqlite-store")]
#[test]
fn a_different_owner_is_still_reported_as_a_second_daemon() {
    let data = tempfile::tempdir().expect("daemon lock dir");
    let path = data.path().join(DAEMON_LOCK_FILENAME);

    let foreign = std::process::id().wrapping_add(1);
    std::fs::write(&path, format!("{foreign}\n")).expect("record a foreign owner");
    assert_eq!(
        read_daemon_lock_holder(&path),
        DaemonLockHolder::OtherProcess(foreign)
    );
    std::fs::write(&path, "not-a-pid").expect("record an unusable owner");
    assert_eq!(
        read_daemon_lock_holder(&path),
        DaemonLockHolder::Unidentified
    );
    assert_eq!(
        read_daemon_lock_holder(&data.path().join("absent.lock")),
        DaemonLockHolder::Unidentified,
        "a missing record must degrade to a message, never to a startup error"
    );

    for holder in [
        DaemonLockHolder::OtherProcess(foreign),
        DaemonLockHolder::Unidentified,
    ] {
        let message =
            daemon_lock_conflict(data.path(), &holder, DAEMON_LOCK_ACQUIRE_WINDOW).to_string();
        assert!(
            message.contains(SECOND_DAEMON_VERDICT),
            "{holder:?} must keep the double-start verdict: {message}"
        );
    }
}

/// Staging import never needs a remotely reachable address. Active M3
/// voters and maintenance clients use persisted membership addresses.
#[cfg(feature = "hiqlite-store")]
#[test]
fn staging_fallback_addresses_are_always_loopback() {
    let configured_v4: SocketAddr = "192.0.2.40:32401".parse().expect("IPv4 bind");
    assert_eq!(
        local_client_address(configured_v4),
        "127.0.0.1:32401".parse().expect("IPv4 loopback")
    );

    let configured_v6: SocketAddr = "[2001:db8::40]:32402".parse().expect("IPv6 bind");
    assert_eq!(
        local_client_address(configured_v6),
        "[::1]:32402".parse().expect("IPv6 loopback")
    );
}

/// K-06 finding 3. Vendor startup (join and its snapshot catch-up), the
/// health wait and the startup catch-up must not spend the 45-second
/// membership admission phase. The phase starts after the health wait,
/// and the deadline promotion/activation honour is installed once, after
/// the catch-up, with only what the committed-member wait left. Vendor
/// startup is nonetheless BOUNDED, by its own budget (review P2): the
/// vendor's join retries and learner wait never end by themselves.
#[cfg(feature = "hiqlite-store")]
#[test]
fn k06_vendor_start_and_catchup_do_not_spend_the_admission_phase() {
    assert_eq!(
        startup_admission_remaining(MEMBERSHIP_ADMISSION_TIMEOUT, Duration::from_secs(5)),
        Duration::from_secs(40)
    );
    assert_eq!(
        startup_admission_remaining(MEMBERSHIP_ADMISSION_TIMEOUT, Duration::from_secs(90)),
        Duration::ZERO,
        "an overspent phase is never replenished"
    );
    let source = include_str!("../migration.rs");
    let start = source
        .split_once("async fn start_voter(")
        .expect("start_voter")
        .1
        .split_once("fn startup_wait_log_is_due(")
        .expect("start_voter boundary")
        .0;
    let vendor_start = start
        .find("hiqlite::start_node_for_clock_observation(")
        .expect("observation start");
    let health = start
        .find("client.wait_until_healthy_db()")
        .expect("health wait");
    let phase_start = start
        .find("let admission_started = tokio::time::Instant::now();")
        .expect("phase start");
    let catchup = start
        .find("let catchup_deadline = tokio::time::Instant::now() + catchup_timeout;")
        .expect("catch-up deadline");
    let install = start
        .find(".install_startup_deadline(")
        .expect("deadline install");
    assert!(vendor_start < health && health < phase_start && phase_start < catchup);
    assert!(
        catchup < install,
        "the catch-up must not spend the installed phase"
    );
    assert!(
        !start[..health].contains("MEMBERSHIP_ADMISSION_TIMEOUT"),
        "vendor startup is not bounded by the admission phase"
    );
    assert!(!source.contains(concat!("with_startup", "_deadline(")));
    // Its own budget: one admission plus one snapshot catch-up, computed
    // before the vendor future and wrapping both start entry points.
    assert_eq!(
        vendor_start_timeout(600, 300),
        MEMBERSHIP_ADMISSION_TIMEOUT + snapshot_catchup_timeout(600, 300)
    );
    assert_eq!(
        vendor_start_timeout(600, 300),
        Duration::from_secs(45 + 600 + 300 + 45)
    );
    let budget = start
        .find("let vendor_timeout = vendor_start_timeout(")
        .expect("vendor start budget");
    let bounded = start
        .find("tokio::time::timeout(vendor_timeout, vendor_start)")
        .expect("vendor start is bounded");
    let normal_start = start
        .find("hiqlite::start_node_with_membership_admission(")
        .expect("normal start");
    assert!(budget < vendor_start && vendor_start < bounded);
    assert!(normal_start < bounded && bounded < health);
    assert_eq!(
        start.matches("hiqlite::start_node_").count(),
        2,
        "no vendor start outside the bounded future"
    );
    let expiry = &start[bounded..health];
    assert!(
        expiry.contains("did not finish starting") && expiry.contains("return Err("),
        "an expired vendor start fails startup with a clear error"
    );
}

/// A readdressed voter has application rows that are older than its new
/// Raft log. Clearing the recovery record before snapshot publication
/// would make the next learner start from an empty database while still
/// reporting an applied membership index.
/// The join snapshot of a restored (or readdressed) image copies the whole
/// image and may wait for disk space first. It runs after every step the
/// startup activation deadline bounds, so no revalidation of that deadline
/// follows it, and its own backstop exceeds the generic health budget and
/// grows with the image.
#[cfg(feature = "hiqlite-store")]
#[test]
fn join_snapshot_runs_outside_the_startup_deadline_on_an_image_sized_budget() {
    let source = include_str!("../migration.rs");
    let open = source
        .split_once("async fn open_active_store_with_key")
        .expect("active store open")
        .1
        .split_once("/// Resolve the key")
        .expect("active store boundary")
        .0;
    let census = open
        .find("open_active_credential_key(")
        .expect("credential census");
    let snapshot = open
        .find("materialize_join_snapshot(&client")
        .expect("join snapshot");
    assert!(
        snapshot > census,
        "the join snapshot must follow the census"
    );
    assert!(
        !open[snapshot..].contains(".revalidate()"),
        "nothing after the join snapshot may spend the startup deadline"
    );
    assert!(join_snapshot_budget(0) > HIQLITE_HEALTH_TIMEOUT);
    let gib = 1024 * 1024 * 1024;
    assert!(
        join_snapshot_budget(20 * gib)
            >= join_snapshot_budget(0) + Duration::from_secs(20 * gib / (8 * 1024 * 1024))
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn readdress_completion_waits_for_a_transferable_database_snapshot() {
    let source = include_str!("../migration.rs");
    let open = source
        .split_once("async fn open_active_store_with_key")
        .expect("active store open")
        .1
        .split_once("/// Resolve the key")
        .expect("active store boundary")
        .0;
    assert!(
        open.contains("read_readdress_record(&config.storage.data_dir)?.is_some()")
            && open.contains("materialize_join_snapshot(&client"),
        "an in-progress readdress must publish a snapshot before open succeeds"
    );
    let selection = source
        .split_once("pub async fn select_daemon_store")
        .expect("daemon selection")
        .1
        .split_once("fn install_default_crypto_provider")
        .expect("daemon selection boundary")
        .0;
    assert!(
        selection.contains("let selected = Box::pin(open_active_store(")
            && selection.contains("finish_readdress(&config.storage.data_dir)?"),
        "readdress cleanup must remain after the snapshot-owning active-store open"
    );
    let hiqlite_management = include_str!("../../../../../vendor/hiqlite/src/client/mgmt.rs");
    assert!(
        hiqlite_management.contains(".client_write(QueryWrite::RTT)")
            && hiqlite_management.contains("state.raft_db.raft.trigger().snapshot().await?"),
        "the local client snapshot hook must anchor writer metadata before the real OpenRaft trigger"
    );
}

/// K-08 §3.6 / M3: the provider this crate installs is `ring`, suites and
/// key-exchange groups alike, so no hybrid post-quantum group is offered.
/// `prefer-post-quantum` only ever reordered the `aws-lc-rs` provider's
/// groups; this pins that the provider the process negotiates with is not
/// that one, which is the premise that made dropping the feature neutral.
#[cfg(feature = "hiqlite-store")]
#[test]
fn installed_provider_is_ring() {
    install_default_crypto_provider();
    let installed = rustls::crypto::CryptoProvider::get_default()
        .expect("install_default_crypto_provider leaves a process provider");
    let ring = rustls::crypto::ring::default_provider();
    let suites = |provider: &rustls::crypto::CryptoProvider| {
        provider
            .cipher_suites
            .iter()
            .map(|suite| suite.suite())
            .collect::<Vec<_>>()
    };
    let groups = |provider: &rustls::crypto::CryptoProvider| {
        provider
            .kx_groups
            .iter()
            .map(|group| group.name())
            .collect::<Vec<_>>()
    };
    assert_eq!(suites(installed), suites(&ring));
    assert_eq!(groups(installed), groups(&ring));
    assert!(
        !groups(installed).contains(&rustls::NamedGroup::X25519MLKEM768),
        "the installed provider offers a hybrid post-quantum group, so it is \
             not ring and dropping prefer-post-quantum was not behaviour-neutral"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn maintenance_uses_the_persisted_local_api_address() {
    let data = tempfile::tempdir().expect("maintenance address dir");
    let mut config = Config::default();
    config.storage.data_dir = data.path().to_owned();
    let identity = super::super::ClusterIdentity {
        cluster_id: "cluster-a".to_owned(),
        node_id: "node-a".to_owned(),
        raft_id: 1,
    };
    write_local_membership(
        data.path(),
        &LocalMembership {
            version: 1,
            cluster_id: identity.cluster_id.clone(),
            node_id: identity.node_id.clone(),
            raft_id: identity.raft_id,
            local: ClusterPeer {
                raft_id: 1,
                raft_address: "192.0.2.40:32401".to_owned(),
                api_address: "192.0.2.40:32402".to_owned(),
            },
            bootstrap: Vec::new(),
            join_token_digest: None,
            role: ClusterRole::Voter,
        },
    )
    .expect("write local membership");
    assert_eq!(
        local_voter_api_address(&config, &identity).expect("maintenance address"),
        "192.0.2.40:32402"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn public_cleartext_cluster_listeners_are_refused() {
    let public: SocketAddr = "0.0.0.0:32401".parse().expect("public bind");
    let error = validate_cluster_listener("raft", public, false)
        .expect_err("public cleartext must be refused");
    assert!(error.to_string().contains("refusing public cleartext raft"));
    validate_cluster_listener("raft", public, true).expect("public TLS is allowed");
    validate_cluster_listener("raft", "127.0.0.1:32401".parse().expect("loopback"), false)
        .expect("loopback cleartext is allowed for staging");
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn default_voters_stay_loopback_and_joined_port_drift_is_refused() {
    let config = Config::default();
    assert!(should_force_loopback(&config, None));

    let mut enabled = config.clone();
    enabled.cluster.advertise_host = "plurx-a.lan".to_owned();
    assert!(!should_force_loopback(&enabled, None));

    let membership = LocalMembership {
        version: 1,
        cluster_id: "cluster-a".to_owned(),
        node_id: "node-a".to_owned(),
        raft_id: 1,
        local: ClusterPeer {
            raft_id: 1,
            raft_address: "plurx-a.lan:32401".to_owned(),
            api_address: "plurx-a.lan:32402".to_owned(),
        },
        bootstrap: Vec::new(),
        join_token_digest: None,
        role: ClusterRole::Voter,
    };
    let mut drifted = enabled;
    drifted.cluster.api_bind.set_port(32502);
    let error = configured_or_persisted_local_peer(&drifted, &membership, false)
        .expect_err("joined listener port drift must fail closed");
    assert!(error.to_string().contains("listener port drift"), "{error}");
}

/// Build an activated data directory whose persisted membership is
/// `membership`, so readdress decisions can be exercised without a store.
#[cfg(feature = "hiqlite-store")]
fn activated_dir_for_readdress(membership: &LocalMembership) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("activated data dir");
    let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
    std::fs::create_dir_all(&active).expect("active target");
    write_activation_marker(
        &active,
        &ActivationMarker {
            marker_version: ACTIVATION_MARKER_VERSION,
            cluster_id: membership.cluster_id.clone(),
            source_backup_sha256: "0".repeat(64),
            source_schema_version: AUTH_SCHEMA_VERSION,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 0,
            table_hashes: vec![SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 0,
                sha256: "0".repeat(64),
            }],
            admitted_role: Some(membership.role),
        },
    )
    .expect("activation marker");
    std::fs::write(
        dir.path().join("node.id"),
        format!("{}\n", membership.node_id),
    )
    .expect("node identity");
    write_local_membership(dir.path(), membership).expect("persisted membership");
    dir
}

/// A loopback-literal `advertise_host` is a legitimate configuration - two
/// daemons on one host - and must reach a fixed point. Deciding on
/// loopback-ness instead of on "differs from configured" never settles, so
/// a sole voter rebuilt its state machine on every boot and a joined
/// follower refused to start at all.
#[cfg(feature = "hiqlite-store")]
#[test]
fn readdress_is_a_no_op_once_the_committed_address_matches_configuration() {
    for (raft_id, bootstrap_raft_id) in [(1_u64, 1_u64), (2, 1)] {
        let dir = tempfile::tempdir().expect("port probe dir");
        let config_probe = membership_test_config(dir.path());
        let raft_port = config_probe.cluster.raft_bind.port();
        let api_port = config_probe.cluster.api_bind.port();
        let local = ClusterPeer {
            raft_id,
            raft_address: format!("127.0.0.1:{raft_port}"),
            api_address: format!("127.0.0.1:{api_port}"),
        };
        let membership = LocalMembership {
            version: 1,
            cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            raft_id,
            local: local.clone(),
            // A joined follower carries the coordinator's bootstrap list,
            // which is exactly what the old peer-existence guard rejected.
            bootstrap: vec![ClusterPeer {
                raft_id: bootstrap_raft_id,
                raft_address: format!("127.0.0.1:{raft_port}"),
                api_address: format!("127.0.0.1:{api_port}"),
            }],
            join_token_digest: None,
            role: ClusterRole::Voter,
        };
        let activated = activated_dir_for_readdress(&membership);
        let mut config = membership_test_config(activated.path());
        config.cluster.advertise_host = "127.0.0.1".to_owned();
        config.cluster.raft_bind = format!("127.0.0.1:{raft_port}").parse().expect("Raft bind");
        config.cluster.api_bind = format!("127.0.0.1:{api_port}")
            .parse()
            .expect("cluster API bind");

        readdress_single_voter_if_needed(&config)
            .unwrap_or_else(|error| panic!("raft_id {raft_id} must boot unchanged: {error}"));

        assert_eq!(
            read_local_membership(activated.path())
                .expect("read membership")
                .map(|membership| membership.local),
            Some(local),
            "raft_id {raft_id} committed address was rewritten"
        );
        assert!(
            !activated
                .path()
                .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
                .exists()
                && !activated.path().join(HIQLITE_INCOMING_DIRNAME).exists(),
            "raft_id {raft_id} rebuilt its state machine with nothing to repair"
        );
    }
}

/// `membership.json` records the bootstrap list once and is never
/// refreshed, so a coordinator that has admitted a peer still reads as a
/// lone voter there. Deciding sole-voter-ness from that file let the
/// readdress rebuild one-node Raft metadata that silently ejected the peer.
#[cfg(feature = "hiqlite-store")]
#[test]
fn readdress_refuses_once_a_peer_is_admitted_even_if_local_membership_is_stale() {
    let membership = LocalMembership {
        version: 1,
        cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
        raft_id: 1,
        local: ClusterPeer {
            raft_id: 1,
            raft_address: "127.0.0.1:32401".to_owned(),
            api_address: "127.0.0.1:32402".to_owned(),
        },
        // Written before the peer joined, and never updated since.
        bootstrap: vec![ClusterPeer {
            raft_id: 1,
            raft_address: "127.0.0.1:32401".to_owned(),
            api_address: "127.0.0.1:32402".to_owned(),
        }],
        join_token_digest: None,
        role: ClusterRole::Voter,
    };
    let activated = activated_dir_for_readdress(&membership);
    let database = activated
        .path()
        .join(HIQLITE_ACTIVE_DIRNAME)
        .join("state_machine")
        .join("db")
        .join(HIQLITE_DATABASE_FILENAME);
    std::fs::create_dir_all(database.parent().expect("state machine dir"))
        .expect("state machine dir");
    let connection = Connection::open(&database).expect("state machine");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                 raft_address TEXT NOT NULL, api_address TEXT NOT NULL, \
                 last_seen_at INTEGER NOT NULL, removed_at INTEGER);\
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 1, '127.0.0.1:32401', '127.0.0.1:32402', 1, NULL),\
                   ('node-b', 2, '127.0.0.1:32411', '127.0.0.1:32412', 1, NULL);",
        )
        .expect("admitted peer record");
    drop(connection);

    let mut config = membership_test_config(activated.path());
    config.cluster.advertise_host = "plurx-a.lan".to_owned();
    config.cluster.raft_bind = "127.0.0.1:32401".parse().expect("Raft bind");
    config.cluster.api_bind = "127.0.0.1:32402".parse().expect("cluster API bind");

    let error = readdress_single_voter_if_needed(&config)
        .expect_err("readdress must refuse once a peer is admitted");
    assert!(
        error
            .to_string()
            .contains("cannot change after another voter or learner exists"),
        "{error}"
    );
    assert!(
        !activated
            .path()
            .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
            .exists()
            && !activated.path().join(HIQLITE_INCOMING_DIRNAME).exists(),
        "refused readdress still touched the active target"
    );
}

/// A removed node is tombstoned rather than deleted, so it must stop
/// blocking a sole survivor's readdress while still reserving its Raft id.
#[cfg(feature = "hiqlite-store")]
#[test]
fn admitted_peer_count_ignores_tombstones_and_missing_schema() {
    let dir = tempfile::tempdir().expect("state machine dir");
    let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
    let database = active
        .join("state_machine")
        .join("db")
        .join(HIQLITE_DATABASE_FILENAME);
    std::fs::create_dir_all(database.parent().expect("db dir")).expect("db dir");

    // A store activated before the membership schema existed never admitted
    // a peer, and an absent state machine cannot contradict that either.
    assert_eq!(admitted_peer_count(&active, 1).expect("absent database"), 0);
    let connection = Connection::open(&database).expect("state machine");
    assert_eq!(admitted_peer_count(&active, 1).expect("absent table"), 0);
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                 raft_address TEXT NOT NULL, api_address TEXT NOT NULL, \
                 last_seen_at INTEGER NOT NULL, removed_at INTEGER);\
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 1, 'a:1', 'a:2', 1, NULL),\
                   ('node-b', 2, 'b:1', 'b:2', 1, 99);",
        )
        .expect("tombstoned peer");
    drop(connection);

    assert_eq!(admitted_peer_count(&active, 1).expect("tombstoned peer"), 0);
    assert_eq!(admitted_peer_count(&active, 2).expect("live peer"), 1);
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn readdress_directory_swap_recovers_both_crash_boundaries() {
    let membership = LocalMembership {
        version: 1,
        cluster_id: "cluster-a".to_owned(),
        node_id: "node-a".to_owned(),
        raft_id: 1,
        local: ClusterPeer {
            raft_id: 1,
            raft_address: "plurx-a.lan:32401".to_owned(),
            api_address: "plurx-a.lan:32402".to_owned(),
        },
        bootstrap: Vec::new(),
        join_token_digest: None,
        role: ClusterRole::Voter,
    };

    let between = tempfile::tempdir().expect("between-renames recovery dir");
    std::fs::create_dir(between.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME)).expect("old target");
    std::fs::create_dir(between.path().join(HIQLITE_INCOMING_DIRNAME)).expect("prepared target");
    write_readdress_record(between.path(), &membership).expect("readdress marker");
    recover_interrupted_readdress(between.path()).expect("restore old target");
    assert!(between.path().join(HIQLITE_ACTIVE_DIRNAME).exists());
    assert!(!between
        .path()
        .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
        .exists());
    assert!(!between.path().join(HIQLITE_INCOMING_DIRNAME).exists());
    assert!(!between
        .path()
        .join(HIQLITE_READDRESS_MARKER_FILENAME)
        .exists());

    let after = tempfile::tempdir().expect("after-renames recovery dir");
    std::fs::create_dir(after.path().join(HIQLITE_ACTIVE_DIRNAME)).expect("new target");
    std::fs::create_dir(after.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME))
        .expect("old recovery target");
    write_readdress_record(after.path(), &membership).expect("readdress marker");
    recover_interrupted_readdress(after.path()).expect("publish new membership");
    assert_eq!(
        read_local_membership(after.path()).expect("read membership"),
        Some(membership)
    );
    assert!(after.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME).exists());
    finish_readdress(after.path()).expect("finish readdress cleanup");
    assert!(!after.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME).exists());
    assert!(!after
        .path()
        .join(HIQLITE_READDRESS_MARKER_FILENAME)
        .exists());
}

#[cfg(feature = "hiqlite-store")]
fn free_test_port() -> u16 {
    static USED_PORTS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<u16>>> =
        std::sync::OnceLock::new();

    loop {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test port");
        let port = listener.local_addr().expect("test port address").port();
        let inserted = USED_PORTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
            .lock()
            .expect("test port registry")
            .insert(port);
        if inserted {
            return port;
        }
    }
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn membership_test_configs_do_not_reuse_released_ephemeral_ports() {
    let dir = tempfile::tempdir().expect("unique membership port dir");
    let mut ports = std::collections::BTreeSet::new();
    for _ in 0..64 {
        let config = membership_test_config(dir.path());
        assert!(ports.insert(config.cluster.raft_bind.port()));
        assert!(ports.insert(config.cluster.api_bind.port()));
    }
    assert_eq!(ports.len(), 128);
}

#[cfg(feature = "hiqlite-store")]
fn membership_test_config(data_dir: &Path) -> Config {
    let mut config = Config::default();
    config.storage.data_dir = data_dir.to_owned();
    config.cluster.raft_bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("Raft bind");
    config.cluster.api_bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("cluster API bind");
    // `localhost` is reachable on this machine but is not parsed as a
    // loopback SocketAddr. Tests can therefore distinguish committed
    // advertised membership from the old hard-coded 127.0.0.1 record.
    config.cluster.advertise_host = "localhost".to_owned();
    config
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn k06_post_client_cancellation_retains_actual_daemon_lock_until_tls_drain() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build cleanup control runtime")
                .block_on(Box::pin(async {
                    let root = tempfile::tempdir().expect("create owned fixture root");
                    let config = membership_test_config(root.path());
                    let identity = super::super::initialize_identity(
                        root.path(),
                        &uuid::Uuid::now_v7().to_string(),
                    )
                    .expect("initialize actual identity");
                    let secrets = load_or_create_secrets(root.path()).expect("load secrets");
                    let daemon_lock = acquire_daemon_lock(root.path())
                        .await
                        .expect("lock directory");
                    let retained_lock = daemon_lock.try_clone().expect("duplicate owned lock");
                    let target = root.path().join(HIQLITE_ACTIVE_DIRNAME);
                    std::fs::create_dir(&target).expect("create active directory");
                    let (client, _, pending) = Box::pin(start_voter(
                        &config,
                        &target,
                        &secrets,
                        &identity,
                        None,
                        true,
                        Some(retained_lock),
                        true,
                        true,
                    ))
                    .await
                    .expect("start actual staged client");
                    let pending = pending.expect("guard transferred from actual Client receipt");
                    let membership = client
                        .local_db_raft_metrics()
                        .expect("actual local Raft metrics")
                        .membership_snapshot();
                    let (ready, entered) = tokio::sync::oneshot::channel();
                    let requester = tokio::spawn(async move {
                        let _pending = pending;
                        let _ = ready.send(());
                        std::future::pending::<()>().await;
                    });
                    entered.await.expect("requester owns pending client");
                    drop(daemon_lock);
                    assert!(
                        acquire_daemon_lock_within(root.path(), Duration::from_millis(20))
                            .await
                            .is_err(),
                        "duplicate owned lock must exclude another daemon"
                    );
                    requester.abort();
                    assert!(requester
                        .await
                        .expect_err("cancelled requester")
                        .is_cancelled());
                    let replacement_lock = tokio::time::timeout(Duration::from_secs(15), async {
                        loop {
                            if let Ok(lock) =
                                acquire_daemon_lock_within(root.path(), Duration::from_millis(20))
                                    .await
                            {
                                break lock;
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    })
                    .await
                    .expect("actual cleanup must release the directory lock after drainage");
                    assert!(
                        !client
                            .local_db_raft_metrics()
                            .expect("local metrics")
                            .snapshot()
                            .running
                    );
                    assert_eq!(
                        client
                            .local_db_raft_metrics()
                            .expect("terminal local metrics")
                            .membership_snapshot()
                            .members,
                        membership.members,
                        "cancelled startup must not automatically remove membership"
                    );
                    let _raft = std::net::TcpListener::bind(config.cluster.raft_bind)
                        .expect("rebind drained Raft port");
                    let _api = std::net::TcpListener::bind(config.cluster.api_bind)
                        .expect("rebind drained API port");
                    assert!(!client.has_retained_startup_resources());
                    drop(replacement_lock);
                }));
        })
        .expect("spawn owned cleanup control thread")
        .join()
        .expect("cleanup control thread completes");
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn k06_post_client_uncertain_cleanup_keeps_actual_lock_without_runtime_or_after_abort() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build actual Client runtime");
            let root = tempfile::tempdir().expect("owned uncertain-cleanup root");
            let config = membership_test_config(root.path());
            let identity =
                super::super::initialize_identity(root.path(), &uuid::Uuid::now_v7().to_string())
                    .expect("actual identity");
            let secrets = load_or_create_secrets(root.path()).expect("actual secrets");
            let target = root.path().join(HIQLITE_ACTIVE_DIRNAME);
            std::fs::create_dir(&target).expect("active directory");
            let (client, _, _) = runtime
                .block_on(Box::pin(start_voter(
                    &config, &target, &secrets, &identity, None, true, None, true, true,
                )))
                .expect("actual staged Client");
            let make_owned = |name: &str| {
                let path = root.path().join(name);
                let lock = File::create(&path).expect("create owned lock");
                lock.lock().expect("lock actual file");
                let cloned = lock.try_clone().expect("clone actual file lock");
                drop(lock);
                let probe = File::open(path).expect("independent lock probe");
                ((client.clone(), cloned), probe)
            };

            // No entered runtime: Drop must not panic or release the pair.
            assert!(tokio::runtime::Handle::try_current().is_err());
            let (owned, probe) = make_owned("outside-runtime.lock");
            drop(PendingStartupClient { owned: Some(owned) });
            assert!(probe.try_lock().is_err(), "missing runtime is not terminal");

            // This real cleanup future is cancelled before its first poll.
            // The auxiliary runtime is current-thread, so scheduling is exact.
            let auxiliary = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("unpolled cleanup runtime");
            let (owned, probe) = make_owned("aborted-drain.lock");
            let task = auxiliary.spawn(RetainedStartupResources { owned: Some(owned) }.drain());
            task.abort();
            assert!(auxiliary
                .block_on(task)
                .expect_err("aborted cleanup")
                .is_cancelled());
            assert!(
                probe.try_lock().is_err(),
                "aborted drain retains actual lock"
            );

            let (owned, probe) = make_owned("runtime-teardown.lock");
            let _task = auxiliary.spawn(RetainedStartupResources { owned: Some(owned) }.drain());
            drop(auxiliary);
            assert!(
                probe.try_lock().is_err(),
                "runtime teardown is not terminal"
            );

            let (owned, probe) = make_owned("successful-handoff.lock");
            let mut pending = PendingStartupClient { owned: Some(owned) };
            pending.handoff();
            drop(pending);
            probe
                .try_lock()
                .expect("successful handoff disarms failure guard");
            runtime
                .block_on(client.shutdown_retained_startup())
                .expect("actual fixture Client terminal drainage");
        })
        .expect("spawn owned uncertain-cleanup control")
        .join()
        .expect("uncertain-cleanup control completes");
}

#[cfg(feature = "hiqlite-store")]
fn membership_http_error(error: super::super::membership::MembershipError) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "code": error.code(), "message": error.to_string() })),
    )
        .into_response()
}

#[cfg(feature = "hiqlite-store")]
async fn redeem_join_for_test(
    State(manager): State<MembershipManager>,
    Json(request): Json<RedeemJoinRequest>,
) -> Response {
    match manager.redeem(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => membership_http_error(error),
    }
}

#[cfg(feature = "hiqlite-store")]
async fn redeem_learner_join_for_test(
    State(manager): State<MembershipManager>,
    Json(request): Json<RedeemJoinRequest>,
) -> Response {
    match manager.redeem_learner(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => membership_http_error(error),
    }
}

#[cfg(feature = "hiqlite-store")]
async fn finalize_join_for_test(
    State(manager): State<MembershipManager>,
    Json(request): Json<FinalizeJoinRequest>,
) -> Response {
    match manager.finalize(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => membership_http_error(error),
    }
}

#[cfg(feature = "hiqlite-store")]
async fn finalize_learner_join_for_test(
    State(manager): State<MembershipManager>,
    Json(request): Json<FinalizeJoinRequest>,
) -> Response {
    match manager.finalize_learner(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => membership_http_error(error),
    }
}

/// Exercise the operator's actual daemon join entry point, including the
/// token file, public redeem/finalize wire, local membership state, and
/// fully-TLS voter startup. The in-process membership harness alone cannot
/// cover any of these pre-store decisions.
/// 496 bytes with every branch boxed (debug, 2026-09-22). With the join,
/// reopen and finalize branches awaited inline it measured 9,984, so the
/// limit leaves room for ordinary growth and still refuses a branch
/// awaited inline again.
#[cfg(feature = "hiqlite-store")]
const SELECT_DAEMON_STORE_FUTURE_LIMIT: usize = 2048;

/// `select_daemon_store` is awaited inline by daemon startup and by the
/// join tests, so its future lives on the caller's stack. Its branches are
/// boxed to keep it small; awaiting one inline again summed every branch's
/// state machine into it and overflowed a 2 MiB debug test thread.
#[cfg(feature = "hiqlite-store")]
#[test]
fn select_daemon_store_future_stays_small() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let config = membership_test_config(data_dir.path());
    let future = select_daemon_store(&config);
    let size = std::mem::size_of_val(&future);
    drop(future);
    println!("select_daemon_store future: {size} bytes");
    assert!(
        size <= SELECT_DAEMON_STORE_FUTURE_LIMIT,
        "select_daemon_store's future grew to {size} bytes; box the branch that grew it"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn actual_live_source_activation_three_running_voters_preserves_local_renewal() {
    install_default_crypto_provider();
    let source_dir = tempfile::tempdir().expect("Source node");
    let second_dir = tempfile::tempdir().expect("second node");
    let third_dir = tempfile::tempdir().expect("third node");
    let make_runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("actual voter runtime")
    };
    let before = make_runtime();
    before.block_on(async {
        let observed = startup_observer::MeasuredPeers::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("join listener");
        let addr = listener.local_addr().expect("join address");
        let mut config = membership_test_config(source_dir.path());
        config.server.bind = addr;
        config.cluster.join_url = format!("http://{addr}");
        drop(
            SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME))
                .expect("baseline SQLite"),
        );
        let source = observed
            .select(&config, false)
            .await
            .expect("actual first voter");
        let manager = source.membership_manager();
        let app = Router::new()
            .route("/api/v1/cluster/join/redeem", post(redeem_join_for_test))
            .route(
                "/api/v1/cluster/join/finalize",
                post(finalize_join_for_test),
            )
            .with_state(manager.clone())
            .merge(startup_observer::MeasuredPeers::route(manager.clone()));
        let http_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("join gateway");
        });
        let mut members = vec![source];
        for directory in [&second_dir, &third_dir] {
            let token = manager
                .issue_token(Duration::from_secs(120))
                .await
                .expect("actual join invitation");
            let path = directory.path().join("join.token");
            std::fs::write(&path, format!("{}\n", token.token)).expect("isolated token file");
            let mut joined_config = membership_test_config(directory.path());
            joined_config.server.bind = format!("127.0.0.1:{}", free_test_port())
                .parse()
                .expect("distinct node HTTP origin");
            joined_config.cluster.join_token_file = path;
            members.push(
                observed
                    .select(&joined_config, true)
                    .await
                    .expect("actual joined voter"),
            );
        }
        let client = members[0].local_client().expect("actual Source client");
        assert_eq!(
            client
                .metrics_db()
                .await
                .expect("real roster")
                .membership_config
                .voter_ids()
                .count(),
            3
        );
        members[0]
            .store
            .put_setting("sharing_enabled", "true")
            .await
            .expect("replicated saved choice");
        for member in &members {
            assert_eq!(
                HiqliteAuthStore::committed_schema_version_for_client(
                    &member.local_client().expect("actual client")
                )
                .await
                .expect("baseline metadata"),
                AUTH_SCHEMA_BASELINE_VERSION
            );
        }
        for member in &members {
            member
                .membership_manager()
                .prepare_purpose_master(Arc::clone(&member.credential_key))
                .await
                .expect("actual selected master proof for each running member");
        }
        let owner=members[0].identity.node_id.clone();
        let fixture=include_str!("../../../tests/fixtures/session-principal-local.sql").replace("'node'",&format!("'{}'",owner.replace('\'',"''"))).replace("'live'","'00000000-0000-0000-0000-000000000001'").replace("'live-session'","'00000000-0000-0000-0000-000000000002'").replace("'staged'","'00000000-0000-0000-0000-000000000003'").replace("'ended-session'","'00000000-0000-0000-0000-000000000005'").replace("'session:live'","'session:00000000-0000-0000-0000-000000000001'");
        let statements=fixture.split(';').map(str::trim).filter(|sql|!sql.is_empty()).map(|sql|(sql.to_owned(),hiqlite::params!())).collect::<Vec<_>>();
        client.txn(statements).await.expect("live Local route before activation").into_iter().collect::<Result<Vec<_>,_>>().expect("retained route committed");
        client.execute("UPDATE media_session_requests SET state='resolved' WHERE incarnation_id='00000000-0000-0000-0000-000000000001'",hiqlite::params!()).await.expect("already playing Local request");
        client.execute("UPDATE media_sessions SET lease_expires_at_ms=9223372036854775000 WHERE incarnation_id='00000000-0000-0000-0000-000000000001'",hiqlite::params!()).await.expect("live producer ownership");
        client.execute("UPDATE job_leases SET expires_at_ms=9223372036854775000 WHERE resource='session:00000000-0000-0000-0000-000000000001'",hiqlite::params!()).await.expect("live physical fence");
        for member in &members {
            let route=member.store.media_session_route_by_incarnation("00000000-0000-0000-0000-000000000001").await.expect("prime each same Store legacy projection").expect("Local route before migration");
            assert_eq!(route.owner_node_id,owner);
            assert_eq!(route.owner_epoch,2);
        }
        // The same three selected stores keep their live Raft identities.
        // Publish compatible dispatch sequentially, as a rolling deployment
        // does; no voter is shut down or reopened to install the layout.
        for (index,member) in members.iter_mut().enumerate() {
            let ready=member.prepare_source_schema_before_serving().await.expect("bounded compatibility publication");
            if index<2 {
                assert!(!ready,"partial compatible roster must stay pending");
                assert_eq!(HiqliteAuthStore::committed_schema_version_for_client(&client).await.expect("unchanged baseline"),AUTH_SCHEMA_BASELINE_VERSION);
            } else { assert!(ready,"final running member completes live installation"); }
        }
        // No Local read has run since the DDL. Every original Store still
        // holds its primed81 dispatch hint: this no-pre-read writer must
        // cross the actual Raft assertion rollback certificate bridge.
        for (index,member) in members.iter().enumerate() {
            let playback=format!("after-live-install-{index}");
            let desired=member.store.record_desired_selection(&crate::playback_principal::PlaybackPrincipal::LocalUser{user_id:1},&playback,&"a".repeat(64),"{}",1_000).await.expect("certified stale81 proposal regenerates as principal82");
            assert_eq!(desired.revision,1,"only one desired mutation committed");
            let rows=client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT json_array(owner_key,principal_kind,user_id,revision) AS payload FROM media_playback_desired WHERE playback_id=$1",hiqlite::params!(playback)).await.expect("principal-safe regenerated row");
            assert_eq!(rows[0].payload,r#"["local:1","local",1,1]"#);
        }
        let renewal=crate::domain::MediaSessionRenewal { incarnation_id:"00000000-0000-0000-0000-000000000001".to_owned(),owner_epoch:2,produced_playable_through_ms:10_000,fetched_through_ms:8_000,media_sequence:4 };
        for (index,member) in members.iter().enumerate() {
            assert!(member.membership_manager().source_layout_ready().await.expect("each running voter verifies layout"));
            let renewed=member.store.renew_media_sessions(&owner,std::slice::from_ref(&renewal),1_000+index as i64,9223372036854775001+index as i64).await.expect("ordinary Local writer survives live transition");
            assert_eq!(renewed,vec!["00000000-0000-0000-0000-000000000001".to_owned()]);
            let route=member.store.media_session_route_by_incarnation("00000000-0000-0000-0000-000000000001").await.expect("projection changes without process restart").expect("retained live route");
            assert_eq!(route.owner_node_id,owner);
            assert_eq!(route.owner_epoch,2);
        }
        let rows=client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT json_array(fence,revision,expires_at_ms) AS payload FROM job_leases WHERE resource='session:00000000-0000-0000-0000-000000000001'",hiqlite::params!()).await.expect("same physical fence after renewals");
        assert_eq!(rows[0].payload,"[2,6,9223372036854775003]");
        for member in &members { member.shutdown().await.expect("shutdown only after live qualification"); }
        http_task.abort();
    });
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_source_schema_coordinator_accepts_api_numeric_enablement() {
    install_default_crypto_provider();
    for (saved, expected) in [("1", true), ("0", false), ("invalid", false)] {
        let directory = tempfile::tempdir().expect("API switch startup data");
        let baseline = SqliteStore::open(&directory.path().join(SQLITE_FILENAME))
            .expect("actual baseline SQLite");
        baseline
            .put_setting("sharing_enabled", saved)
            .await
            .expect("persist the public settings API representation");
        drop(baseline);
        let mut selected =
            startup_observer::select_applied_singleton(&membership_test_config(directory.path()))
                .await
                .expect("actual admitted voter and authenticated startup clock");
        assert_eq!(
            selected
                .prepare_source_schema_before_serving()
                .await
                .expect("actual startup coordinator"),
            expected,
            "canonical saved choice {saved}"
        );
        assert_eq!(
            HiqliteAuthStore::committed_schema_version_for_client(
                &selected.local_client().expect("actual client"),
            )
            .await
            .expect("actual committed schema"),
            if expected {
                AUTH_SCHEMA_VERSION
            } else {
                AUTH_SCHEMA_BASELINE_VERSION
            }
        );
        assert_eq!(
            selected
                .store
                .get_setting("sharing_enabled")
                .await
                .expect("saved choice remains unchanged")
                .as_deref(),
            Some(saved)
        );
        selected
            .shutdown()
            .await
            .expect("bounded actual voter shutdown");
    }
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_source_schema_coordinator_keeps_complete_legacy_guard_local_only_without_repair() {
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("legacy guard data");
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("legacy SQLite"));
    let mut selected =
        startup_observer::select_applied_singleton(&membership_test_config(directory.path()))
            .await
            .expect("actual admitted voter");
    let client = selected.local_client().expect("actual client");
    for sql in super::super::membership::sharing_member_admission_guard_schema()
        .into_iter()
        .filter(|sql| !sql.contains("cluster_sharing_purpose_keys_"))
        .map(|sql| sql.replace(",'sharing_purpose_keys_v1'", ""))
    {
        client
            .execute(sql, hiqlite::params!())
            .await
            .expect("known complete legacy guard fixture");
    }
    let query = "SELECT json_array(type,name,sql) AS payload FROM sqlite_master WHERE name GLOB 'cluster_sharing_*' ORDER BY name";
    let before = client
        .query_consistent_map::<SourceStartupPayloadRow, _>(query, hiqlite::params!())
        .await
        .expect("legacy schema snapshot")
        .into_iter()
        .map(|row| row.payload)
        .collect::<Vec<_>>();
    selected
        .store
        .put_setting("sharing_enabled", "true")
        .await
        .expect("saved choice");
    assert!(!selected
        .prepare_source_schema_before_serving()
        .await
        .expect("recognized Local-only pending state"));
    let after = client
        .query_consistent_map::<SourceStartupPayloadRow, _>(query, hiqlite::params!())
        .await
        .expect("unchanged snapshot")
        .into_iter()
        .map(|row| row.payload)
        .collect::<Vec<_>>();
    assert_eq!(before, after, "no ALTER or repair");
    assert_eq!(
        selected
            .store
            .get_setting("sharing_enabled")
            .await
            .expect("saved choice")
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("legacy metadata"),
        AUTH_SCHEMA_BASELINE_VERSION
    );
    assert!(client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT name AS payload FROM sqlite_master WHERE name IN('sharing_catalogue_keys','sharing_file_locator_keys','sharing_source_boot_intents','sharing_source_schema_installation')",hiqlite::params!()).await.expect("no mint or rebuild").is_empty());
    selected
        .store
        .put_setting("sharing_enabled", "false")
        .await
        .expect("disabled choice preserved");
    client
        .execute(
            "DROP TRIGGER cluster_sharing_generation_intent_update",
            hiqlite::params!(),
        )
        .await
        .expect("test-only partial guard");
    selected.shutdown().await.expect("settled shutdown");
    drop(client);
    drop(selected);
    let mut restart =
        startup_observer::select_applied_singleton(&membership_test_config(directory.path()))
            .await
            .expect("master census remains readable");
    assert!(
        restart
            .prepare_source_schema_before_serving()
            .await
            .is_err(),
        "unknown/partial guard does not become safe merely because Sharing is disabled"
    );
    let client = restart.local_client().expect("actual restarted client");
    assert!(client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT name AS payload FROM sqlite_master WHERE name='cluster_sharing_generation_intent_update'",hiqlite::params!()).await.expect("no repair census").is_empty());
    assert_eq!(
        restart
            .store
            .get_setting("sharing_enabled")
            .await
            .expect("disabled choice")
            .as_deref(),
        Some("false")
    );
    restart.shutdown().await.expect("settled partial fixture");
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_source_schema_coordinator_preserves_live_local_lease_without_boot_intents() {
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("Local data");
    let config = membership_test_config(directory.path());
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("legacy SQLite"));
    let mut selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("actual one voter");
    selected
        .store
        .put_setting("sharing_enabled", "true")
        .await
        .expect("saved choice");
    let client = selected.local_client().expect("actual client");
    client.execute("INSERT INTO job_leases VALUES('retained-local-worker','actual-local-owner',7,9,9223372036854775807,1)",hiqlite::params!()).await.expect("live Local lease");
    assert!(selected
        .prepare_source_schema_before_serving()
        .await
        .expect("compatible live startup retains Local ownership"));
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("legacy marker"),
        crate::store::sharing_source_schema::SOURCE_SCHEMA_VERSION
    );
    let rows=client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT json_array(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) AS payload FROM job_leases WHERE resource='retained-local-worker'",hiqlite::params!()).await.expect("retained ownership");
    assert!(
        matches!(rows.as_slice(),[row] if row.payload=="[\"retained-local-worker\",\"actual-local-owner\",7,9,9223372036854775807,1]")
    );
    let rows = client
        .query_consistent_map::<SourceStartupPayloadRow, _>(
            "SELECT CAST(count(*) AS TEXT) AS payload FROM sharing_source_boot_intents",
            hiqlite::params!(),
        )
        .await
        .expect("confirmed intent release");
    assert!(matches!(rows.as_slice(),[row] if row.payload=="0"));
    assert_eq!(
        selected
            .store
            .get_setting("sharing_enabled")
            .await
            .expect("saved choice")
            .as_deref(),
        Some("true")
    );
    assert_eq!(client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT name AS payload FROM sqlite_master WHERE name='sharing_source_session_bindings'",hiqlite::params!()).await.expect("complete installation").len(),1);
    selected
        .store
        .put_setting("sharing_enabled", "false")
        .await
        .expect("operator disables optional sharing");
    client.execute("INSERT INTO sharing_source_boot_intents SELECT node_id,raft_id,$1,$2,(SELECT generation FROM cluster_sharing_membership_generation WHERE singleton=1) FROM cluster_nodes WHERE removed_at IS NULL",hiqlite::params!(uuid::Uuid::new_v4().to_string(),selected.credential_key.sharing_purpose_master_fingerprint())).await.expect("retained crash startup intent fixture");
    selected.shutdown().await.expect("settled shutdown");
    drop(client);
    drop(selected);
    let mut restart =
        startup_observer::select_applied_singleton(&membership_test_config(directory.path()))
            .await
            .expect("actual disabled-choice restart");
    assert!(restart
        .prepare_source_schema_before_serving()
        .await
        .expect("withdraw stale process receipt before Local serving"));
    let client = restart.local_client().expect("actual restart client");
    let rows = client
        .query_consistent_map::<SourceStartupPayloadRow, _>(
            "SELECT CAST(count(*) AS TEXT) AS payload FROM sharing_source_boot_intents",
            hiqlite::params!(),
        )
        .await
        .expect("old process receipt retired");
    assert!(matches!(rows.as_slice(),[row] if row.payload=="0"));
    assert_eq!(
        restart
            .store
            .get_setting("sharing_enabled")
            .await
            .expect("saved disabled choice")
            .as_deref(),
        Some("false")
    );
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("unchanged legacy"),
        crate::store::sharing_source_schema::SOURCE_SCHEMA_VERSION
    );
    restart.shutdown().await.expect("settled restart");
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_live_source_activation_preserves_local_ownership_and_requires_dispatch_proof() {
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("live Source data");
    let config = membership_test_config(directory.path());
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("legacy SQLite"));
    let mut selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("actual voter");
    assert!(!selected
        .prepare_source_schema_before_serving()
        .await
        .expect("disabled startup"));
    let client = selected.local_client().expect("actual replicated client");
    let fixture = include_str!("../../../tests/fixtures/session-principal-local.sql");
    let statements = fixture
        .split(';')
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .map(|sql| (sql.to_owned(), hiqlite::params!()))
        .collect::<Vec<_>>();
    client
        .txn(statements)
        .await
        .expect("retain active Local work")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture committed");
    client.execute("UPDATE media_sessions SET lease_expires_at_ms=9223372036854775000 WHERE incarnation_id='live'",hiqlite::params!()).await.expect("retain live producer lease");
    client
        .execute(
            "UPDATE job_leases SET expires_at_ms=9223372036854775000 WHERE resource='session:live'",
            hiqlite::params!(),
        )
        .await
        .expect("retain live worker lease");
    selected
        .store
        .put_setting("sharing_enabled", "1")
        .await
        .expect("live enable");
    let membership = selected.membership_manager();
    membership
        .coordinate_purpose_keys()
        .await
        .expect("current factory");
    client
        .execute(
            "DELETE FROM cluster_node_capabilities WHERE capability=$1",
            hiqlite::params!(super::super::membership::SHARING_SOURCE_LIVE_DISPATCH_CAPABILITY),
        )
        .await
        .expect("mixed binary floor");
    assert!(!membership
        .coordinate_source_schema_live()
        .await
        .expect("missing dispatch remains pending"));
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("baseline"),
        AUTH_SCHEMA_BASELINE_VERSION
    );
    membership
        .prepare_purpose_master(Arc::clone(&selected.credential_key))
        .await
        .expect("force actual compatible heartbeat after capability removal");
    // A retained capability from the previous process heartbeat is not proof
    // that the current admitted process has installed compatible dispatch.
    client
        .execute(
            "UPDATE cluster_nodes SET last_seen_at=last_seen_at+1 WHERE removed_at IS NULL",
            hiqlite::params!(),
        )
        .await
        .expect("older binary heartbeat withdraws current coupling");
    assert!(!membership
        .coordinate_source_schema_live()
        .await
        .expect("stale prior capability refuses migration"));
    membership
        .prepare_purpose_master(Arc::clone(&selected.credential_key))
        .await
        .expect("force fresh compatible process proof without heartbeat coalescing");
    assert!(membership
        .coordinate_source_schema_live()
        .await
        .expect("install without stopping producer"));
    let rows=client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT json_array(incarnation_id,owner_node_id,owner_epoch,lease_expires_at_ms,state,recovery_epoch,drain_deadline_ms,owner_key,principal_kind) AS payload FROM media_sessions WHERE incarnation_id='live'",hiqlite::params!()).await.expect("retained route");
    assert_eq!(
        rows[0].payload,
        r#"["live","node",2,9223372036854775000,"active","epoch",8000,"local:1","local"]"#
    );
    let rows=client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT json_array(fence,revision,expires_at_ms) AS payload FROM job_leases WHERE resource='session:live'",hiqlite::params!()).await.expect("retained physical fence");
    assert_eq!(rows[0].payload, "[2,3,9223372036854775000]");
    let rows=client.query_consistent_map::<SourceStartupPayloadRow,_>("SELECT json_array((SELECT count(*) FROM media_session_preparations),(SELECT count(*) FROM sharing_relay_upstream),(SELECT count(*) FROM sharing_delivery_grants),(SELECT count(*) FROM media_session_requests WHERE state='starting')) AS payload",hiqlite::params!()).await.expect("retained child and starting work");
    assert_eq!(rows[0].payload, "[1,1,1,1]");
    assert!(membership
        .source_layout_ready()
        .await
        .expect("exact current evidence"));
    assert_eq!(
        read_activation_marker(&directory.path().join(HIQLITE_ACTIVE_DIRNAME))
            .expect("refreshed receipt")
            .replicated_schema_version,
        crate::store::sharing_source_schema::SOURCE_SCHEMA_VERSION
    );
    assert!(membership
        .coordinate_source_schema_live()
        .await
        .expect("idempotent existing82"));
    selected.shutdown().await.expect("actual voter shutdown");
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_source_schema_coordinator_one_voter_installs_and_restarts_without_repair() {
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("Source coordinator data");
    let config = membership_test_config(directory.path());
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let mut selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("actual admitted one voter");
    selected
        .store
        .put_setting("sharing_enabled", "true")
        .await
        .expect("preserve explicit choice");
    let client = selected.local_client().expect("actual replicated client");
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("baseline marker"),
        AUTH_SCHEMA_BASELINE_VERSION
    );
    assert!(selected
        .prepare_source_schema_before_serving()
        .await
        .expect("actual selected master and boot factory"));
    assert!(
        selected
            .prepare_source_schema_before_serving()
            .await
            .is_err(),
        "startup authority is single use"
    );
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("installed marker"),
        AUTH_SCHEMA_VERSION
    );
    assert_eq!(
        read_activation_marker(&directory.path().join(HIQLITE_ACTIVE_DIRNAME))
            .expect("durable activation marker")
            .replicated_schema_version,
        AUTH_SCHEMA_VERSION
    );
    let marker = selected
        .store
        .get_setting("sharing_enabled")
        .await
        .expect("choice");
    assert_eq!(marker.as_deref(), Some("true"));
    selected.shutdown().await.expect("settled voter shutdown");
    drop(client);
    drop(selected);
    let restart_config = membership_test_config(directory.path());
    let mut restarted = startup_observer::select_applied_singleton(&restart_config)
        .await
        .expect("exact installed shape reopens");
    assert!(restarted
        .prepare_source_schema_before_serving()
        .await
        .expect("restart verifies winner"));
    let client = restarted.local_client().expect("restarted voter");
    client
        .execute(
            "DROP INDEX sharing_catalogue_binary_order",
            hiqlite::params!(),
        )
        .await
        .expect("test-only partial installed layout");
    restarted
        .shutdown()
        .await
        .expect("shutdown malformed fixture");
    drop(client);
    drop(restarted);
    assert!(
        startup_observer::select_applied_singleton(&membership_test_config(directory.path()))
            .await
            .is_err(),
        "v71 missing index is refused without read repair"
    );
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_purpose_coordinator_one_voter_preserves_keys_across_restart() {
    use crate::store::sharing_purpose_keys::PurposeKeyInstallation;
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("purpose coordinator data");
    let config = membership_test_config(directory.path());
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("actual one voter selection");
    assert_eq!(selected.backend, SelectedBackend::Replicated);
    let key = Arc::clone(&selected.credential_key);
    selected
        .local_client
        .as_ref()
        .expect("actual voter")
        .execute(
            "UPDATE cluster_nodes SET role=NULL WHERE node_id=$1",
            hiqlite::params!(selected.identity.node_id.as_str()),
        )
        .await
        .expect("legacy nullable voter role remains an actual Raft voter");
    selected
        .membership
        .prepare_purpose_master(Arc::clone(&key))
        .await
        .expect("actual selected master proof");
    selected
        .membership
        .coordinate_purpose_keys()
        .await
        .expect("disabled choice preserved");
    assert_eq!(
        selected
            .store
            .verify_sharing_purpose_material(&key)
            .await
            .expect("read-only absent keys"),
        PurposeKeyInstallation::NotReady
    );
    selected
        .store
        .put_setting("sharing_enabled", "true")
        .await
        .expect("explicit saved choice");
    selected
        .membership
        .coordinate_purpose_keys()
        .await
        .expect("actual first-install coordinator");
    assert_eq!(
        selected
            .store
            .verify_sharing_purpose_material(&key)
            .await
            .expect("installed keys open"),
        PurposeKeyInstallation::Ready
    );
    #[derive(Debug, PartialEq, Eq)]
    struct PurposeEnvelope {
        envelope: String,
    }
    impl From<&mut hiqlite::Row<'_>> for PurposeEnvelope {
        fn from(row: &mut hiqlite::Row<'_>) -> Self {
            Self {
                envelope: row.get("envelope"),
            }
        }
    }
    let ciphertext_sql = "SELECT revision_envelope AS envelope FROM sharing_catalogue_keys UNION ALL SELECT locator_envelope AS envelope FROM sharing_file_locator_keys ORDER BY envelope";
    let before = selected
        .local_client
        .as_ref()
        .expect("actual client")
        .query_consistent_map::<PurposeEnvelope, _>(ciphertext_sql, hiqlite::params!())
        .await
        .expect("current active ciphertexts");
    let wrong = Arc::new(CredentialKey::from_bytes([233; 32]));
    assert!(
        selected
            .membership
            .prepare_purpose_master(wrong)
            .await
            .is_err(),
        "arbitrary master cannot replace actual selected proof"
    );
    selected.shutdown().await.expect("durable shutdown");
    drop(selected);
    // Hiqlite's known TLS listener shutdown defect retains the old bind;
    // exercise the real supported sole-voter readdress/restart path.
    let restart_config = membership_test_config(directory.path());
    let restarted = startup_observer::select_applied_singleton(&restart_config)
        .await
        .expect("actual voter restart");
    assert_eq!(
        restarted
            .credential_key
            .sharing_purpose_master_fingerprint(),
        key.sharing_purpose_master_fingerprint()
    );
    restarted
        .membership
        .prepare_purpose_master(Arc::clone(&restarted.credential_key))
        .await
        .expect("restarted actual master proof");
    restarted
        .membership
        .coordinate_purpose_keys()
        .await
        .expect("restart preserves winner");
    assert_eq!(
        restarted
            .local_client
            .as_ref()
            .expect("actual restarted client")
            .query_consistent_map::<PurposeEnvelope, _>(ciphertext_sql, hiqlite::params!())
            .await
            .expect("preserved active ciphertexts"),
        before
    );
    assert_eq!(
        restarted
            .store
            .get_setting("sharing_enabled")
            .await
            .expect("saved choice"),
        Some("true".to_owned())
    );
    restarted.shutdown().await.expect("restart shutdown");
}

#[cfg(feature = "hiqlite-store")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_redeem_rejoin_and_promotion_refuse_missing_installed_sharing_capabilities() {
    use crate::cluster::membership::{
        sharing_member_admission_guard_schema, MembershipError, SharingJoinCapabilities,
    };
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("candidate member source");
    let config = membership_test_config(directory.path());
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("legacy source store"));
    let selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("actual source voter");
    let client = selected.local_client.as_ref().expect("local voter client");
    for sql in sharing_member_admission_guard_schema() {
        client
            .execute(sql, hiqlite::params!())
            .await
            .expect("install test-only candidate membership guards");
    }
    selected
        .membership
        .activate_learner_protocol()
        .await
        .expect("activate existing learner protocol for real redeem path");
    let make_request = |issued: &crate::cluster::membership::IssuedJoinToken| RedeemJoinRequest {
        token_digest: join_token_digest(&issued.token),
        raft_id: issued.raft_id,
        node_id: uuid::Uuid::new_v4().to_string(),
        hostname: "candidate-joiner".to_owned(),
        raft_address: "127.0.0.1:32401".to_owned(),
        api_address: "127.0.0.1:32402".to_owned(),
        http_base: String::new(),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
        protocol_min: crate::store::AUTH_PROTOCOL_MIN,
        protocol_max: crate::store::AUTH_PROTOCOL_MAX,
        live_tv_v1: true,
        sharing: SharingJoinCapabilities::default(),
    };
    let legacy_token = selected
        .membership
        .issue_learner_token(Duration::from_secs(120))
        .await
        .expect("legacy compatible learner token");
    let legacy = make_request(&legacy_token);
    selected
        .membership
        .redeem_learner(&legacy)
        .await
        .expect("uninstalled cluster preserves missing sharing declarations");
    client
        .execute(
            "CREATE TABLE item_identity_watermark(malformed TEXT)",
            hiqlite::params!(),
        )
        .await
        .expect("partial allocator marker");
    assert!(
        matches!(
            selected.membership.redeem_learner(&legacy).await,
            Err(MembershipError::Incompatible)
        ),
        "even an existing staged rejoin must explicitly implement installed allocator semantics"
    );
    assert!(
        matches!(
            selected.membership.promote_learner(&legacy.node_id).await,
            Err(MembershipError::Incompatible)
        ),
        "actual promotion entry refuses older target before membership mutation"
    );
    let compatible_token = selected
        .membership
        .issue_learner_token(Duration::from_secs(120))
        .await
        .expect("compatible token");
    let mut compatible = make_request(&compatible_token);
    compatible.raft_address = "127.0.0.1:32403".to_owned();
    compatible.api_address = "127.0.0.1:32404".to_owned();
    compatible.sharing.catalogue_item_identity = true;
    client
        .execute(
            "ALTER TABLE media_session_requests ADD COLUMN owner_key TEXT",
            hiqlite::params!(),
        )
        .await
        .expect("partial principal owner column");
    assert!(
        matches!(
            selected.membership.redeem_learner(&compatible).await,
            Err(MembershipError::Incompatible)
        ),
        "allocator claim is independent of installed principal columns"
    );
    compatible.sharing.session_principal = true;
    selected
        .membership
        .redeem_learner(&compatible)
        .await
        .expect("actual compatible redemption uses fresh token-bound intents");
    selected
        .membership
        .redeem_learner(&compatible)
        .await
        .expect("compatible identity-bound retry remains resumable");
    compatible.sharing = SharingJoinCapabilities::default();
    assert!(
        matches!(
            selected.membership.redeem_learner(&compatible).await,
            Err(MembershipError::Incompatible)
        ),
        "a retry cannot inherit an earlier request's declarations"
    );
    selected
        .shutdown()
        .await
        .expect("stop actual candidate source voter");
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn daemon_join_refuses_occupied_and_expired_targets_then_resumes_finalization() {
    startup_observer::run_plural_fixture("k06-r1-voter-join", daemon_join_observation_fixture);
}

#[cfg(feature = "hiqlite-store")]
async fn daemon_join_observation_fixture() {
    let observed = startup_observer::MeasuredPeers::default();
    install_default_crypto_provider();

    let source_dir = tempfile::tempdir().expect("source data dir");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("join coordinator listener");
    let coordinator_addr = listener.local_addr().expect("coordinator address");
    let mut source_config = membership_test_config(source_dir.path());
    source_config.cluster.advertise_host.clear();
    source_config.server.bind = coordinator_addr;
    source_config.cluster.join_url = format!("http://{coordinator_addr}");
    drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let source = startup_observer::select_applied_singleton(&source_config)
        .await
        .expect("activate source voter");
    let cluster_id = source.identity.cluster_id.clone();
    source
        .store
        .put_setting("membership.readdress", "preserved")
        .await
        .expect("write state before readdress");
    let original_metrics = source
        .local_client
        .as_ref()
        .expect("source client")
        .metrics_db()
        .await
        .expect("source membership");
    let original = original_metrics
        .membership_config
        .nodes()
        .find(|(raft_id, _)| **raft_id == 1)
        .map(|(_, node)| node.clone())
        .expect("source voter record");
    assert!(original.addr_api.starts_with("127.0.0.1:"));
    source.shutdown().await.expect("stop loopback source");
    drop(source);

    // This is the actual upgrade path: an already-running one-voter install
    // opts into membership later. Its committed Raft address, not only
    // membership.json, must become remotely usable without losing data.
    // A real daemon restart drops the old runtime and frees these ports;
    // this in-process test uses fresh ports because Hiqlite 0.14 leaves its
    // TLS listener task alive until the test runtime itself exits.
    source_config.cluster.raft_bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("readdressed Raft bind");
    source_config.cluster.api_bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("readdressed cluster API bind");
    source_config.cluster.advertise_host = "localhost".to_owned();
    let source = observed
        .select(&source_config, false)
        .await
        .expect("readdress existing source voter");
    assert_eq!(source.identity.cluster_id, cluster_id);
    assert_eq!(
        source
            .store
            .get_setting("membership.readdress")
            .await
            .expect("read state after readdress")
            .as_deref(),
        Some("preserved")
    );
    let readdressed_metrics = source
        .local_client
        .as_ref()
        .expect("readdressed source client")
        .metrics_db()
        .await
        .expect("readdressed membership");
    let readdressed = readdressed_metrics
        .membership_config
        .nodes()
        .find(|(raft_id, _)| **raft_id == 1)
        .map(|(_, node)| node.clone())
        .expect("readdressed voter record");
    assert!(
        readdressed.addr_api.starts_with("localhost:"),
        "committed membership remained loopback: {readdressed:?}"
    );
    assert!(!source_dir
        .path()
        .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
        .exists());
    assert!(!source_dir
        .path()
        .join(HIQLITE_READDRESS_MARKER_FILENAME)
        .exists());
    let coordinator = source.membership_manager();
    let source_client = source.local_client.clone().expect("source client");
    let app = Router::new()
        .route("/api/v1/cluster/join/redeem", post(redeem_join_for_test))
        .route(
            "/api/v1/cluster/learner/join/redeem",
            post(redeem_learner_join_for_test),
        )
        .route(
            "/api/v1/cluster/join/finalize",
            post(finalize_join_for_test),
        )
        .route(
            "/api/v1/cluster/learner/join/finalize",
            post(finalize_learner_join_for_test),
        )
        .with_state(coordinator.clone())
        .merge(startup_observer::MeasuredPeers::route(coordinator.clone()));
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve join coordinator");
    });

    let occupied = tempfile::tempdir().expect("occupied join dir");
    let occupied_token = occupied.path().join("join.token");
    std::fs::write(&occupied_token, "unused\n").expect("occupied token file");
    File::create(occupied.path().join(SQLITE_FILENAME)).expect("occupied SQLite path");
    let mut occupied_config = membership_test_config(occupied.path());
    occupied_config.cluster.join_token_file = occupied_token;
    let occupied_error = select_daemon_store(&occupied_config)
        .await
        .err()
        .expect("existing SQLite must refuse daemon join")
        .to_string();
    assert!(occupied_error.contains("refusing to join with existing local database"));

    let expired = coordinator
        .issue_token(Duration::from_secs(1))
        .await
        .expect("issue expiring token");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let expired_dir = tempfile::tempdir().expect("expired join dir");
    let expired_path = expired_dir.path().join("join.token");
    std::fs::write(&expired_path, format!("{}\n", expired.token)).expect("expired token file");
    let mut expired_config = membership_test_config(expired_dir.path());
    expired_config.cluster.join_token_file = expired_path;
    let expired_error = select_daemon_store(&expired_config)
        .await
        .err()
        .expect("expired token must refuse daemon join")
        .to_string();
    assert!(
        expired_error.contains("join_token_expired"),
        "{expired_error}"
    );
    assert!(!expired_dir.path().join(HIQLITE_ACTIVE_DIRNAME).exists());

    // A still-live token may reserve the next Raft id while another
    // machine starts first. OpenRaft permits that sparse membership, and
    // the production daemon must not confuse the assigned id with the
    // length of its bootstrap vector (Hiqlite 0.14 does exactly that
    // without the compatibility adapter in `start_voter`).
    let held_gap = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("reserve the preceding Raft id");
    let issued = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("issue daemon join token");
    assert_eq!(
        issued.raft_id,
        held_gap.raft_id + 1,
        "daemon join fixture must exercise a sparse assigned id"
    );
    let joining_dir = tempfile::tempdir().expect("joining data dir");
    let token_path = joining_dir.path().join("join.token");
    std::fs::write(&token_path, format!("{}\n", issued.token)).expect("joining token file");
    let mut joining_config = membership_test_config(joining_dir.path());
    joining_config.server.bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("joining clock bind");
    joining_config.cluster.join_token_file = token_path.clone();
    let staged_identity =
        crate::cluster::initialize_join_identity(joining_dir.path(), &cluster_id, issued.raft_id)
            .expect("stage the joining node identity");
    let staged_local =
        configured_local_peer(&joining_config, issued.raft_id).expect("configure the staged peer");
    let issued_digest = join_token_digest(&issued.token);
    let staged_node_id = staged_identity.node_id.clone();
    let redeem_request = RedeemJoinRequest {
        token_digest: issued_digest.clone(),
        raft_id: issued.raft_id,
        node_id: staged_node_id.clone(),
        hostname: "joining-test-node".to_owned(),
        raft_address: staged_local.raft_address,
        api_address: staged_local.api_address,
        http_base: configured_artwork_url(&joining_config)
            .expect("derive the staged node artwork origin"),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
        protocol_min: crate::store::AUTH_PROTOCOL_MIN,
        protocol_max: crate::store::AUTH_PROTOCOL_MAX,
        sharing: Default::default(),
        live_tv_v1: true,
    };
    coordinator
        .redeem(&redeem_request)
        .await
        .expect("reserve the token to the staged node before its failed start");
    source_client
        .execute(
            "UPDATE cluster_join_tokens SET expires_at = 0 \
                 WHERE token_hash = $1 AND state = 'redeeming'",
            hiqlite::params!(issued_digest.as_str()),
        )
        .await
        .expect("expire the identity-bound reservation deterministically");
    // Treat the first successful redemption as an ambiguous transport
    // outcome: the same staged identity must be able to repeat it after
    // the reservation's original TTL, rather than losing the node to an
    // orphaned `redeeming` record.
    coordinator
        .redeem(&redeem_request)
        .await
        .expect("repeat the expired identity-bound redemption");
    let joined = observed
        .select(&joining_config, true)
        .await
        .expect("resume an expired identity-bound join through daemon store selection");
    assert_eq!(joined.identity.cluster_id, cluster_id);
    assert_eq!(joined.identity.raft_id, issued.raft_id);
    assert!(
        !token_path.exists(),
        "successful finalization removes the token"
    );
    let local = read_local_membership(joining_dir.path())
        .expect("read joined local membership")
        .expect("joined node persists local membership");
    assert_eq!(
        local.join_token_digest.as_deref(),
        Some(issued_digest.as_str())
    );
    let finalize_request = FinalizeJoinRequest {
        token_digest: issued_digest.clone(),
        raft_id: issued.raft_id,
        node_id: staged_node_id,
    };
    coordinator
        .finalize(&finalize_request)
        .await
        .expect("repeat finalization after the daemon lost its response");
    coordinator
        .finalize(&finalize_request)
        .await
        .expect("repeated finalization remains idempotent");

    // A crash after finalization but before unlink leaves exactly this
    // shape: active target + membership.json + the original token. The
    // retry is idempotent and removes it on the next pass.
    std::fs::write(&token_path, format!("{}\n", issued.token))
        .expect("restore interrupted-finalization token");
    finalize_pending_join(&joining_config, &joined)
        .await
        .expect("resume finalized join");
    assert!(!token_path.exists());

    // An unrelated copied token is not this node's pending state. It is
    // ignored rather than taking a healthy voter offline.
    let foreign = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("issue foreign token");
    std::fs::write(&token_path, format!("{}\n", foreign.token)).expect("foreign token file");
    finalize_pending_join(&joining_config, &joined)
        .await
        .expect("foreign token is a warning, not a startup failure");
    assert!(
        token_path.exists(),
        "foreign token remains for operator inspection"
    );

    // An operator staging two machines, or replacing a token they lost,
    // must not be made to wait out the first token's TTL. Allocation walks
    // past ids that live tokens are holding instead of recomputing the one
    // id an outstanding token already reserved.
    let concurrent = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("second token while the first is still outstanding");
    assert_ne!(
        concurrent.raft_id, foreign.raft_id,
        "concurrent tokens reserved the same Raft id"
    );

    // Once voter state is durable, a coordinator outage may delay token
    // consumption but must not take the healthy joined daemon offline.
    std::fs::write(&token_path, format!("{}\n", issued.token))
        .expect("restore own token before coordinator outage");
    http_task.abort();
    finalize_pending_join_best_effort(&joining_config, &joined).await;
    assert!(
        token_path.exists(),
        "pending finalization keeps the identity-bound token for retry"
    );

    let local_client = joined.local_client.clone().expect("joined local client");
    joined.shutdown().await.expect("drain joined voter");
    let post_shutdown = tokio::time::timeout(
        Duration::from_secs(2),
        local_client.execute(
            "CREATE TABLE IF NOT EXISTS shutdown_must_have_closed_writers (id INTEGER)",
            hiqlite::macros::params!(),
        ),
    )
    .await;
    assert!(
        !matches!(post_shutdown, Ok(Ok(_))),
        "shutdown returned while the replicated writer still accepted work"
    );

    source.shutdown().await.expect("drain source voter");
}

#[tokio::test]
async fn backup_includes_committed_wal_state_and_removes_abandoned_incoming() {
    let data = tempfile::tempdir().expect("data dir");
    let source_path = data.path().join(SQLITE_FILENAME);
    let store = SqliteStore::open(&source_path).expect("source store");
    store
        .put_setting("migration.wal", "committed")
        .await
        .expect("write source");

    let incoming = data.path().join(HIQLITE_INCOMING_DIRNAME);
    std::fs::create_dir_all(incoming.join("partial")).expect("incoming tree");
    std::fs::write(incoming.join("partial/state"), b"not complete").expect("incoming state");

    let prepared = prepare_sqlite_import(data.path()).expect("prepare import");
    assert!(!incoming.exists(), "abandoned target was removed");
    assert_eq!(prepared.schema_version, SQLITE_SCHEMA_VERSION);
    assert_eq!(
        prepared.backup_sha256,
        sha256_file(&prepared.backup_path).expect("hash published backup")
    );

    let backup = Connection::open_with_flags(
        &prepared.backup_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open backup");
    let value: String = backup
        .query_row(
            "SELECT value FROM settings WHERE key = 'migration.wal'",
            [],
            |row| row.get(0),
        )
        .expect("backed-up setting");
    assert_eq!(value, "committed");
    let journal_mode: String = backup
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read backup journal mode");
    assert_eq!(journal_mode, "delete");
}

#[test]
fn future_schema_refusal_changes_nothing() {
    let data = tempfile::tempdir().expect("data dir");
    let source_path = data.path().join(SQLITE_FILENAME);
    let source = Connection::open(&source_path).expect("source");
    source
        .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION + 1)
        .expect("future version");
    drop(source);
    let incoming = data.path().join(HIQLITE_INCOMING_DIRNAME);
    std::fs::create_dir(&incoming).expect("incoming");
    std::fs::write(incoming.join("state"), b"preserve").expect("state");

    let error = prepare_sqlite_import(data.path()).expect_err("future schema refused");
    assert!(error.to_string().contains("refusing clustering import"));
    assert!(incoming.join("state").exists());
    assert!(!data.path().join(MIGRATION_DIRNAME).exists());
}

#[tokio::test]
async fn content_addressed_backups_are_reused_and_old_backups_are_kept() {
    let data = tempfile::tempdir().expect("data dir");
    let source_path = data.path().join(SQLITE_FILENAME);
    let store = SqliteStore::open(&source_path).expect("source store");
    let migration_dir = data.path().join(MIGRATION_DIRNAME);
    std::fs::create_dir(&migration_dir).expect("migration dir");
    let abandoned = migration_dir.join(format!(
        ".{SQLITE_FILENAME}.{}.incoming",
        uuid::Uuid::new_v4()
    ));
    let abandoned_wal = PathBuf::from(format!("{}-wal", abandoned.display()));
    let abandoned_shm = PathBuf::from(format!("{}-shm", abandoned.display()));
    std::fs::write(&abandoned, b"partial backup").expect("abandoned backup");
    std::fs::write(&abandoned_wal, b"partial wal").expect("abandoned backup wal");
    std::fs::write(&abandoned_shm, b"partial shm").expect("abandoned backup shm");
    let first = prepare_sqlite_import(data.path()).expect("first backup");
    assert!(!abandoned.exists(), "abandoned backup temp was removed");
    assert!(!abandoned_wal.exists(), "abandoned backup WAL was removed");
    assert!(!abandoned_shm.exists(), "abandoned backup SHM was removed");
    let repeated = prepare_sqlite_import(data.path()).expect("repeated backup");
    assert_eq!(first, repeated);

    store
        .put_setting("migration.changed", "yes")
        .await
        .expect("change source");
    let changed = prepare_sqlite_import(data.path()).expect("changed backup");
    assert_ne!(changed.backup_path, first.backup_path);
    assert!(first.backup_path.exists(), "original backup is retained");
    assert!(changed.backup_path.exists(), "new backup is retained");
    assert!(
        std::fs::read_dir(data.path().join(MIGRATION_DIRNAME))
            .expect("migration dir")
            .all(|entry| {
                let entry = entry.expect("migration entry");
                !is_backup_temp_artifact(&entry.file_name().to_string_lossy())
            }),
        "no temporary backup remains"
    );
}

/// A failing activation must not fill the data volume with database copies.
///
/// Content addressing alone does not bound this: each failed attempt leaves
/// a recovery boot that serves traffic and mutates the source, so the next
/// retry addresses different content and writes another full-size copy.
#[tokio::test]
async fn repeated_activation_attempts_bound_retained_source_backups() {
    let data = tempfile::tempdir().expect("data dir");
    let source_path = data.path().join(SQLITE_FILENAME);
    let store = SqliteStore::open(&source_path).expect("source store");

    let mut published = Vec::new();
    // Comfortably past the retention window, each with different content,
    // standing in for the mutation a recovery boot makes between retries.
    for attempt in 0..MIGRATION_BACKUP_RETENTION + 3 {
        store
            .put_setting("migration.attempt", &attempt.to_string())
            .await
            .expect("change source between attempts");
        published.push(
            prepare_sqlite_import(data.path())
                .expect("attempt backup")
                .backup_path,
        );
    }

    let retained: Vec<_> = std::fs::read_dir(data.path().join(MIGRATION_DIRNAME))
        .expect("migration dir")
        .map(|entry| entry.expect("migration entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .collect();
    assert_eq!(
        retained.len(),
        MIGRATION_BACKUP_RETENTION,
        "retained backups: {retained:?}"
    );
    // The window keeps the newest, and above all the one this attempt is
    // about to import — deleting that would break the activation it serves.
    for recent in published.iter().rev().take(MIGRATION_BACKUP_RETENTION) {
        assert!(retained.contains(recent), "{recent:?} must be retained");
    }
    for old in published
        .iter()
        .take(published.len() - MIGRATION_BACKUP_RETENTION)
    {
        assert!(!old.exists(), "{old:?} must have been pruned");
    }
}

#[test]
fn corrupt_content_addressed_backup_is_refused() {
    let data = tempfile::tempdir().expect("data dir");
    let source_path = data.path().join(SQLITE_FILENAME);
    SqliteStore::open(&source_path).expect("source store");

    let prepared = prepare_sqlite_import(data.path()).expect("first backup");
    std::fs::write(&prepared.backup_path, b"not a SQLite backup").expect("poison published backup");

    let error = prepare_sqlite_import(data.path()).expect_err("corrupt backup refused");
    assert!(
        error
            .to_string()
            .contains("does not match its content-addressed name"),
        "unexpected error: {error}"
    );
}

/// The upgrade contract, on a real replicated voter.
///
/// Everything here happens with no operator action beyond installing this
/// binary: the cluster must stay on the pre-P6 protocol, must keep
/// admitting the previous release, and must move only when activation is
/// asked for. The refusal that comes after activation is the point of the
/// whole guard, so it is asserted on the message an operator would read.
#[cfg(feature = "hiqlite-store")]
#[test]
fn a_deployed_binary_widens_no_range_until_activation_is_asked_for() {
    startup_observer::run_full_hiqlite_fixture(
        "k06-r1-protocol-range",
        deployed_binary_observation_fixture,
    );
}

#[cfg(feature = "hiqlite-store")]
async fn deployed_binary_observation_fixture() {
    install_default_crypto_provider();

    let dir = tempfile::tempdir().expect("protocol activation data dir");
    let config = membership_test_config(dir.path());
    drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("activate a one-voter cluster on this binary");
    let membership = selected.membership_manager();
    let client = selected
        .local_client
        .as_ref()
        .expect("voter client")
        .clone();

    // The binary that shipped before P6: it implements exactly protocol 4.
    let previous_release = crate::store::ClusterCompatibility {
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_min: crate::store::AUTH_PROTOCOL_VERSION,
        protocol_max: crate::store::AUTH_PROTOCOL_VERSION,
    };

    // 1. Installing this binary changed nothing.
    assert_eq!(
        membership
            .active_protocol_range()
            .await
            .expect("read the active range"),
        (
            crate::store::AUTH_PROTOCOL_MIN,
            crate::store::AUTH_PROTOCOL_MIN
        ),
        "a bootstrapped cluster must not activate protocol 5 by itself"
    );
    HiqliteAuthStore::preflight_voter(&client, previous_release)
        .await
        .expect("the previous release must still be able to join this cluster");
    HiqliteAuthStore::preflight_voter(&client, crate::store::ClusterCompatibility::CURRENT)
        .await
        .expect("and so must this one");

    // A learner's preflight against the same unactivated cluster is
    // refused, and the refusal is not dressed as a database problem. It
    // reached operators as `schema migration failed: learner_protocol_
    // inactive: …`, which sent them to look at the schema when the answer
    // was "run the activation command".
    let learner_refusal = HiqliteAuthStore::preflight_role(
        &client,
        crate::store::ClusterCompatibility::CURRENT,
        true,
    )
    .await
    .expect_err("an unactivated cluster must refuse a learner's preflight")
    .to_string();
    assert!(
        learner_refusal.contains("learner_protocol_inactive"),
        "{learner_refusal}"
    );
    assert!(
        !learner_refusal.contains("migration"),
        "a protocol refusal must not arrive as a migration failure: {learner_refusal}"
    );

    // The node's own heartbeat is the proof; nothing else writes it.
    let status = membership
        .protocol_status()
        .await
        .expect("read the protocol status");
    assert!(
        status.learner_protocol_pending.is_empty(),
        "the running binary proved the learner protocol by heartbeating: {status:?}"
    );
    assert!(!status.learner_protocol_active);

    // An unactivated cluster still admits a joiner that predates the range
    // fields entirely. This is the other half of "zero operator action":
    // the coordinator was upgraded, and a not-yet-upgraded node can still
    // join it. Any refusal here is addressing, never the protocol gate.
    let unactivated_token = membership
        .issue_token(Duration::from_secs(120))
        .await
        .expect("issue a join token before activation");
    let pre_range_joiner = RedeemJoinRequest {
        token_digest: join_token_digest(&unactivated_token.token),
        raft_id: unactivated_token.raft_id,
        node_id: "pre-range-joiner".to_owned(),
        hostname: "pre-range-joiner".to_owned(),
        raft_address: "127.0.0.1:3".to_owned(),
        api_address: "127.0.0.1:4".to_owned(),
        http_base: String::new(),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
        protocol_min: 0,
        protocol_max: 0,
        sharing: Default::default(),
        live_tv_v1: false,
    };
    assert_ne!(
        membership
            .redeem(&pre_range_joiner)
            .await
            .err()
            .map(|error| error.code().to_owned())
            .unwrap_or_default(),
        "join_incompatible",
        "an unactivated cluster must not refuse a joiner that predates the range"
    );

    // And that admitted joiner now holds activation, which is exactly the
    // point: it redeemed on a binary that predates protocol 5, it is
    // mid-join, and narrowing the range past it would leave a node that
    // counts for quorum and can never open its store again.
    match membership.activate_learner_protocol().await {
        Err(super::super::membership::MembershipError::JoinInFlight(nodes)) => {
            assert_eq!(nodes, vec!["pre-range-joiner".to_owned()]);
        }
        other => panic!("a join in flight must hold activation: {other:?}"),
    }
    assert_eq!(
        membership
            .active_protocol_range()
            .await
            .expect("read the range"),
        (
            crate::store::AUTH_PROTOCOL_MIN,
            crate::store::AUTH_PROTOCOL_MIN
        ),
        "a refused activation must not move the range"
    );
    // The operator's exit: this joiner has no process behind it and is
    // never going to finish, so remove it.
    client
        .execute(
            "UPDATE cluster_nodes SET removed_at = $1 WHERE node_id = $2",
            hiqlite::params!(1_i64, "pre-range-joiner"),
        )
        .await
        .expect("tombstone the abandoned joiner");

    // 2. Activation is explicit, and only then does the range move.
    let activated = membership
        .activate_learner_protocol()
        .await
        .expect("activate the learner protocol");
    assert!(
        activated.changed,
        "the first activation commits: {activated:?}"
    );
    assert!(activated.protocol.learner_protocol_active);
    assert_eq!(
        membership
            .active_protocol_range()
            .await
            .expect("read the activated range"),
        (
            crate::store::AUTH_PROTOCOL_MAX,
            crate::store::AUTH_PROTOCOL_MAX
        )
    );

    // 3. Idempotent: a retried request after a timeout is not an error and
    //    is not a second write.
    let again = membership
        .activate_learner_protocol()
        .await
        .expect("activating an already-activated cluster succeeds");
    assert!(!again.changed, "activation must not write twice: {again:?}");

    // 4. The previous release can no longer boot here, and is told why.
    let refusal = HiqliteAuthStore::preflight_voter(&client, previous_release)
        .await
        .expect_err("an activated cluster must refuse the previous release")
        .to_string();
    assert!(refusal.contains("protocol 5"), "{refusal}");
    assert!(refusal.contains("too old"), "{refusal}");
    HiqliteAuthStore::preflight_voter(&client, crate::store::ClusterCompatibility::CURRENT)
        .await
        .expect("this binary keeps working across activation");

    // The admission gate agrees with the boot guard rather than comparing
    // against a constant of its own.
    let issued = membership
        .issue_token(Duration::from_secs(120))
        .await
        .expect("issue a join token for the activated cluster");
    let old_joiner = RedeemJoinRequest {
        token_digest: join_token_digest(&issued.token),
        raft_id: issued.raft_id,
        node_id: "old-binary-joiner".to_owned(),
        hostname: "old-binary-joiner".to_owned(),
        raft_address: "127.0.0.1:1".to_owned(),
        api_address: "127.0.0.1:2".to_owned(),
        http_base: String::new(),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
        // A joiner that predates the range sends no range fields.
        protocol_min: 0,
        protocol_max: 0,
        sharing: Default::default(),
        live_tv_v1: false,
    };
    assert_eq!(
        membership
            .redeem(&old_joiner)
            .await
            .expect_err("an activated cluster must refuse a protocol-4-only joiner")
            .code(),
        "join_incompatible"
    );
    let new_joiner = RedeemJoinRequest {
        protocol_min: crate::store::AUTH_PROTOCOL_MIN,
        protocol_max: crate::store::AUTH_PROTOCOL_MAX,
        sharing: Default::default(),
        live_tv_v1: true,
        ..old_joiner
    };
    // This one clears the protocol gate; it fails later, on addressing,
    // which is exactly what proves the protocol check let it through.
    assert_ne!(
        membership
            .redeem(&new_joiner)
            .await
            .err()
            .map(|error| error.code().to_owned())
            .unwrap_or_default(),
        "join_incompatible",
        "a range-declaring joiner must clear the activated protocol gate"
    );

    // 5. The reversible rollback, and its idempotence.
    let rolled_back = membership
        .deactivate_learner_protocol()
        .await
        .expect("deactivate with no learner present");
    assert!(rolled_back.changed);
    assert!(!rolled_back.protocol.learner_protocol_active);
    let noop = membership
        .deactivate_learner_protocol()
        .await
        .expect("deactivating an unactivated cluster succeeds");
    assert!(!noop.changed, "deactivation must not write twice: {noop:?}");
    HiqliteAuthStore::preflight_voter(&client, previous_release)
        .await
        .expect("rollback restores the previous release's ability to boot");

    selected.shutdown().await.expect("stop the voter");
}

/// The replicated schema marker, read through the leader.
#[cfg(feature = "hiqlite-store")]
async fn replicated_schema_version(client: &Client) -> i64 {
    client
        .query_consistent_map::<SchemaVersionRow, _>(
            "SELECT schema_version FROM cluster_meta WHERE singleton = 1",
            hiqlite::params!(),
        )
        .await
        .expect("read the replicated schema marker")
        .first()
        .expect("cluster_meta has exactly one row")
        .schema_version
}

#[cfg(feature = "hiqlite-store")]
struct SourceStartupPayloadRow {
    payload: String,
}
#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for SourceStartupPayloadRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            payload: row.get("payload"),
        }
    }
}

#[cfg(feature = "hiqlite-store")]
struct SchemaVersionRow {
    schema_version: i64,
}

#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for SchemaVersionRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            schema_version: row.get("schema_version"),
        }
    }
}

/// One replicated membership row's durable admission role.
#[cfg(feature = "hiqlite-store")]
#[derive(Debug)]
struct NodeRoleRow {
    node_id: String,
    role: Option<String>,
}

#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for NodeRoleRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            node_id: row.get("node_id"),
            role: row.get("role"),
        }
    }
}

/// A token framing this build does not implement must leave the data
/// directory exactly as it found it.
///
/// The assertion is on the filesystem rather than on the return value on
/// purpose: the refusal is only worth anything if this node is not holding
/// the cluster's raft, API, and credential secrets afterwards, and has not
/// staged an identity a later boot could resume. `plxjoin:v3` stands in for
/// any future protocol — including, from an older build's side, `plxjoin:v2`
/// itself.
#[cfg(feature = "hiqlite-store")]
#[tokio::test]
async fn an_unsupported_join_token_is_refused_before_any_secret_is_written() {
    let joining = tempfile::tempdir().expect("joining data dir");
    let token_path = joining.path().join("join.token");
    // Well-formed framing, unknown version: 32-byte key, 24-byte nonce and
    // a body, so nothing but the version can be what refuses it.
    let token = format!("plxjoin:v3:{}:{}", "ab".repeat(32), "cd".repeat(64));
    std::fs::write(&token_path, format!("{token}\n")).expect("write token file");
    let mut config = membership_test_config(joining.path());
    config.cluster.join_token_file = token_path.clone();

    let error = select_daemon_store(&config)
        .await
        .err()
        .expect("an unsupported token framing must refuse the join")
        .to_string();
    assert!(error.contains("join_token_invalid"), "{error}");

    for leftover in [
        joining.path().join(RAFT_SECRET_FILENAME),
        joining.path().join(API_SECRET_FILENAME),
        config.cluster.credential_key_path(joining.path()),
        joining.path().join(crate::cluster::NODE_ID_FILENAME),
        joining.path().join(LOCAL_MEMBERSHIP_FILENAME),
        joining.path().join(HIQLITE_ACTIVE_DIRNAME),
    ] {
        assert!(
            !leftover.exists(),
            "a refused join left {} behind",
            leftover.display()
        );
    }
    assert!(
        token_path.exists(),
        "the operator's token stays put for inspection"
    );
}

/// The previous release's `membership.json` version check, transcribed.
///
/// A build that predates the learner role has no idea it must not campaign,
/// so it has to refuse a learner's record rather than read it as a voter's.
/// That is the desired behaviour, which means it is worth a test rather
/// than a comment.
#[cfg(feature = "hiqlite-store")]
fn the_previous_release_accepts(record: &LocalMembership) -> bool {
    record.version == 1
        && !record.cluster_id.trim().is_empty()
        && !record.node_id.trim().is_empty()
        && record.raft_id != 0
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn a_learner_membership_record_fails_closed_on_an_older_binary() {
    let peer = ClusterPeer {
        raft_id: 2,
        raft_address: "localhost:32401".to_owned(),
        api_address: "localhost:32402".to_owned(),
    };
    let voter = LocalMembership {
        version: local_membership_version(ClusterRole::Voter),
        cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
        raft_id: 2,
        local: peer.clone(),
        bootstrap: vec![peer.clone()],
        join_token_digest: None,
        role: ClusterRole::Voter,
    };
    let learner = LocalMembership {
        version: local_membership_version(ClusterRole::Learner),
        role: ClusterRole::Learner,
        ..voter.clone()
    };

    // Installing this binary does not rewrite a voter's record, so rolling
    // that node back to the previous release still works.
    assert!(the_previous_release_accepts(&voter));
    assert!(!the_previous_release_accepts(&learner));

    // And this build round-trips both, including the v1 record's implied
    // role — which is the v1-to-v2 read path.
    let dir = tempfile::tempdir().expect("membership dir");
    write_local_membership(dir.path(), &voter).expect("write a voter record");
    assert_eq!(
        read_local_membership(dir.path()).expect("read it back"),
        Some(voter.clone())
    );
    let raw = std::fs::read_to_string(dir.path().join(LOCAL_MEMBERSHIP_FILENAME))
        .expect("read the raw record");
    assert!(raw.contains("\"version\": 1"), "{raw}");

    write_local_membership(dir.path(), &learner).expect("write a learner record");
    assert_eq!(
        read_local_membership(dir.path()).expect("read it back"),
        Some(learner)
    );

    // A record whose version and role disagree is an edited file, not a
    // version this build has to interpret.
    write_local_membership(
        dir.path(),
        &LocalMembership {
            version: 1,
            role: ClusterRole::Learner,
            ..voter
        },
    )
    .expect("write an inconsistent record");
    assert!(
        read_local_membership(dir.path()).is_err(),
        "a v1 record must not be allowed to claim the learner role"
    );
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn a_promoted_learner_persists_a_crash_safe_downgrade_readable_voter_role() {
    let dir = tempfile::tempdir().expect("promoted data dir");
    let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
    std::fs::create_dir_all(&active).expect("active target");
    let peer = ClusterPeer {
        raft_id: 4,
        raft_address: "localhost:33401".to_owned(),
        api_address: "localhost:33402".to_owned(),
    };
    let mut membership = LocalMembership {
        version: local_membership_version(ClusterRole::Learner),
        cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        node_id: "44444444-4444-4444-8444-444444444444".to_owned(),
        raft_id: 4,
        local: peer.clone(),
        bootstrap: vec![peer],
        join_token_digest: Some("a".repeat(64)),
        role: ClusterRole::Learner,
    };
    let mut marker = ActivationMarker {
        marker_version: ACTIVATION_MARKER_VERSION,
        cluster_id: membership.cluster_id.clone(),
        source_backup_sha256: "0".repeat(64),
        source_schema_version: AUTH_SCHEMA_VERSION,
        replicated_schema_version: AUTH_SCHEMA_VERSION,
        imported_rows: 0,
        table_hashes: vec![SqliteImportTableDigest {
            table: "settings".to_owned(),
            row_count: 0,
            sha256: "0".repeat(64),
        }],
        admitted_role: Some(ClusterRole::Learner),
    };
    write_local_membership(dir.path(), &membership).expect("learner membership");
    write_activation_marker(&active, &marker).expect("learner activation marker");

    persist_promoted_voter_role(dir.path()).expect("persist promoted role");
    let promoted = read_local_membership(dir.path())
        .expect("read promoted membership")
        .expect("membership exists");
    assert_eq!(promoted.role, ClusterRole::Voter);
    assert_eq!(promoted.version, 1);
    assert!(the_previous_release_accepts(&promoted));
    assert_eq!(
        read_activation_marker(&active)
            .expect("read promoted marker")
            .admitted_role,
        Some(ClusterRole::Voter)
    );

    // Recreate the only crash interval: membership was fsynced as voter,
    // activation.json still says learner. The next boot's reconciliation
    // completes the marker and remains idempotent.
    membership.role = ClusterRole::Voter;
    membership.version = local_membership_version(ClusterRole::Voter);
    marker.admitted_role = Some(ClusterRole::Learner);
    write_local_membership(dir.path(), &membership).expect("interrupted voter record");
    write_activation_marker(&active, &marker).expect("stale learner marker");
    persist_promoted_voter_role(dir.path()).expect("resume interrupted promotion");
    persist_promoted_voter_role(dir.path()).expect("idempotent retry");
    assert_eq!(
        read_activation_marker(&active)
            .expect("read resumed marker")
            .admitted_role,
        Some(ClusterRole::Voter)
    );
}

/// The admitted-role record grants cluster-wide job authority to a process
/// that cannot read committed membership, so a *missing* record must not
/// be read as "voter" on a clustered directory.
///
/// Two different absences: a directory that was never clustered has no
/// record because there is nothing to record, and that is the ordinary
/// single-node server which owns every job. A directory with an activation
/// marker and no record is a cluster node whose one durable statement of
/// what it was admitted as has been lost — and if that node was a learner,
/// answering "voter" hands it `provider:artwork`.
///
/// Gated like everything it calls. Without this, `cargo test -p plurx-core
/// --lib` does not compile — the same default-feature blind spot that hid
/// `local_cluster_role`'s own missing gate, one layer up.
#[cfg(feature = "hiqlite-store")]
#[test]
fn a_clustered_directory_with_no_admitted_role_record_refuses_instead_of_defaulting() {
    let dir = tempfile::tempdir().expect("data dir");

    // Never clustered: no marker, no record, and the single-node answer.
    assert_eq!(
        local_cluster_role(dir.path()).expect("an unclustered directory answers"),
        ClusterRole::Voter
    );

    // Clustered, record present: the record answers.
    let peer = ClusterPeer {
        raft_id: 2,
        raft_address: "localhost:32401".to_owned(),
        api_address: "localhost:32402".to_owned(),
    };
    let learner = LocalMembership {
        version: local_membership_version(ClusterRole::Learner),
        cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
        raft_id: 2,
        local: peer.clone(),
        bootstrap: vec![peer],
        join_token_digest: None,
        role: ClusterRole::Learner,
    };
    write_local_membership(dir.path(), &learner).expect("write a learner record");
    let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
    std::fs::create_dir_all(&active).expect("create the active directory");
    std::fs::write(active.join(ACTIVATION_MARKER_FILENAME), b"{}")
        .expect("write an activation marker");
    assert_eq!(
        local_cluster_role(dir.path()).expect("a clustered directory answers from its record"),
        ClusterRole::Learner
    );

    // Clustered, record gone: refuse rather than grant.
    std::fs::remove_file(dir.path().join(LOCAL_MEMBERSHIP_FILENAME))
        .expect("delete the learner's record");
    let refused = local_cluster_role(dir.path())
        .expect_err("a clustered directory with no admitted-role record must refuse")
        .to_string();
    assert!(
        refused.contains(LOCAL_MEMBERSHIP_FILENAME),
        "the refusal must name the missing file: {refused}"
    );
}

/// A joined role is durable before `activation.json` becomes the commit
/// bit, and a retry after a crash at that boundary resumes the same role.
#[cfg(feature = "hiqlite-store")]
#[test]
fn join_activation_crash_after_membership_never_restarts_a_learner_as_a_voter() {
    let dir = tempfile::tempdir().expect("joined data dir");
    let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
    std::fs::create_dir_all(&active).expect("active target");
    let peer = ClusterPeer {
        raft_id: 4,
        raft_address: "localhost:33401".to_owned(),
        api_address: "localhost:33402".to_owned(),
    };
    let membership = LocalMembership {
        version: local_membership_version(ClusterRole::Learner),
        cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        node_id: "44444444-4444-4444-8444-444444444444".to_owned(),
        raft_id: 4,
        local: peer.clone(),
        bootstrap: vec![peer],
        join_token_digest: Some("a".repeat(64)),
        role: ClusterRole::Learner,
    };
    let marker = ActivationMarker {
        marker_version: ACTIVATION_MARKER_VERSION,
        cluster_id: membership.cluster_id.clone(),
        source_backup_sha256: "0".repeat(64),
        source_schema_version: AUTH_SCHEMA_VERSION,
        replicated_schema_version: AUTH_SCHEMA_VERSION,
        imported_rows: 0,
        table_hashes: vec![SqliteImportTableDigest {
            table: "settings".to_owned(),
            row_count: 0,
            sha256: "0".repeat(64),
        }],
        admitted_role: Some(ClusterRole::Learner),
    };

    let interrupted = publish_join_activation(
        dir.path(),
        &active,
        &marker,
        &membership,
        JoinActivationFailpoint::AfterMembership,
    )
    .expect_err("failpoint interrupts before activation marker");
    assert!(interrupted.to_string().contains("failpoint"));
    assert_eq!(
        read_local_membership(dir.path())
            .expect("read durable membership")
            .expect("membership was published")
            .role,
        ClusterRole::Learner
    );
    assert!(!active.join(ACTIVATION_MARKER_FILENAME).exists());

    publish_join_activation(
        dir.path(),
        &active,
        &marker,
        &membership,
        JoinActivationFailpoint::None,
    )
    .expect("retry publishes the marker");
    let resumed_marker = read_activation_marker(&active).expect("read activation marker");
    assert_eq!(
        active_store_role(&resumed_marker, Some(&membership), dir.path())
            .expect("resume the admitted role"),
        ClusterRole::Learner
    );

    std::fs::remove_file(dir.path().join(LOCAL_MEMBERSHIP_FILENAME))
        .expect("simulate later membership loss");
    assert!(
        active_store_role(&resumed_marker, None, dir.path()).is_err(),
        "an activated learner with a missing role record must fail closed"
    );
}

/// The whole learner admission path against a real replicated cluster.
///
/// A learner is only useful if it is provably harmless, so this asserts the
/// harm it must not do rather than only that it started: it carries no
/// vote, its own membership handle refuses it leader-singleton work, its
/// durable role survives a restart, and while it exists the cluster refuses
/// to deactivate the protocol that admitted it.
#[cfg(feature = "hiqlite-store")]
#[test]
fn a_learner_joins_only_after_activation_and_never_gains_a_vote() {
    startup_observer::run_plural_fixture("k06-r1-learner-join", learner_join_observation_fixture);
}

#[cfg(feature = "hiqlite-store")]
async fn learner_join_observation_fixture() {
    let observed = startup_observer::MeasuredPeers::default();
    install_default_crypto_provider();

    let source_dir = tempfile::tempdir().expect("source data dir");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("join coordinator listener");
    let coordinator_addr = listener.local_addr().expect("coordinator address");
    let mut source_config = membership_test_config(source_dir.path());
    source_config.server.bind = coordinator_addr;
    source_config.cluster.join_url = format!("http://{coordinator_addr}");
    drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let source = observed
        .select(&source_config, false)
        .await
        .expect("activate the source voter");
    let coordinator = source.membership_manager();
    let source_client = source.local_client.clone().expect("source client");
    let app = Router::new()
        .route("/api/v1/cluster/join/redeem", post(redeem_join_for_test))
        .route(
            "/api/v1/cluster/learner/join/redeem",
            post(redeem_learner_join_for_test),
        )
        .route(
            "/api/v1/cluster/join/finalize",
            post(finalize_join_for_test),
        )
        .route(
            "/api/v1/cluster/learner/join/finalize",
            post(finalize_learner_join_for_test),
        )
        .with_state(coordinator.clone())
        .merge(startup_observer::MeasuredPeers::route(coordinator.clone()));
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve join coordinator");
    });

    // 1. Before activation there is no learner protocol to admit anyone
    //    with, and the refusal happens at issuance — an operator finds out
    //    before they have copied a token to another machine.
    assert_eq!(
        coordinator
            .issue_learner_token(Duration::from_secs(120))
            .await
            .expect_err("an unactivated cluster cannot mint a learner token")
            .code(),
        "learner_protocol_inactive"
    );
    // A voter token is still perfectly ordinary, and still v1.
    let voter_token = coordinator
        .issue_token(Duration::from_secs(120))
        .await
        .expect("issue a voter token on an unactivated cluster");
    assert!(voter_token.token.starts_with("plxjoin:v1:"));

    // 2. Activation is the explicit step that changes this.
    assert!(
        coordinator
            .activate_learner_protocol()
            .await
            .expect("activate the learner protocol")
            .changed
    );
    let learner_token = coordinator
        .issue_learner_token(Duration::from_secs(120))
        .await
        .expect("issue a learner token on an activated cluster");
    assert!(learner_token.token.starts_with("plxjoin:v2:"));

    // A pre-P6 coordinator knows only the voter route and its old SQL
    // transition. Replicated state must reject that transition even if
    // the old HTTP decoder would otherwise ignore a caller's learner
    // field and proceed with the stored digest.
    let learner_digest = join_token_digest(&learner_token.token);
    let old_coordinator = source_client
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                 WHERE token_hash = $2 AND state = 'issued'",
            hiqlite::params!("old-coordinator", learner_digest.as_str()),
        )
        .await
        .expect_err("the voter-path transition must reject a learner token");
    assert!(
        old_coordinator
            .to_string()
            .contains("learner token requires v2 admission"),
        "unexpected old-coordinator refusal: {old_coordinator}"
    );

    // 3. The join itself, through the operator's real entry point.
    let learner_dir = tempfile::tempdir().expect("learner data dir");
    let token_path = learner_dir.path().join("join.token");
    std::fs::write(&token_path, format!("{}\n", learner_token.token)).expect("learner token file");
    let mut learner_config = membership_test_config(learner_dir.path());
    learner_config.server.bind = format!("127.0.0.1:{}", free_test_port())
        .parse()
        .expect("learner clock bind");
    learner_config.cluster.join_token_file = token_path.clone();
    let learner = observed
        .select(&learner_config, true)
        .await
        .expect("admit the learner");
    assert_eq!(learner.identity.raft_id, learner_token.raft_id);
    assert!(
        !token_path.exists(),
        "a finalized learner join consumes its token"
    );

    // 4. It is a committed member and it carries no vote. This is the
    //    property Raft itself enforces: a non-voter cannot campaign.
    let metrics = source_client
        .metrics_db()
        .await
        .expect("read committed membership");
    assert!(
        metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == learner_token.raft_id),
        "the learner must be a committed member"
    );
    assert!(
        !metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == learner_token.raft_id),
        "the learner must not have acquired a vote"
    );
    assert!(
        metrics.membership_config.voter_ids().count() == 1,
        "admitting a learner must not change quorum size"
    );

    // 5. Its own membership handle refuses it every leader-singleton job,
    //    from live committed membership rather than a boot flag.
    let learner_membership = learner.membership_manager();
    assert!(!learner_membership
        .local_node_is_committed_voter()
        .await
        .expect("read the learner's committed role"));
    assert!(!learner_membership.may_run_cluster_jobs().await);
    assert!(
        coordinator.may_run_cluster_jobs().await,
        "the voter is unaffected"
    );

    // 6. The durable role is in replicated membership, where deactivation
    //    can see it.
    let roles = source_client
        .query_map::<NodeRoleRow, _>(
            "SELECT node_id, role FROM cluster_nodes ORDER BY raft_id",
            hiqlite::params!(),
        )
        .await
        .expect("read the durable roles");
    let learner_rows = roles
        .iter()
        .filter(|row| row.role.as_deref() == Some("learner"))
        .map(|row| row.node_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        learner_rows,
        vec![learner.identity.node_id.clone()],
        "exactly the joined node is durably a learner: {roles:?}"
    );
    match coordinator.deactivate_learner_protocol().await {
        Err(super::super::membership::MembershipError::LearnerProtocolInUse {
            admitted,
            non_voting,
        }) => {
            assert!(
                admitted.contains(&learner.identity.node_id),
                "the refusal must name the learner it protects: {admitted:?}"
            );
            assert_eq!(
                non_voting,
                vec![learner.identity.raft_id.to_string()],
                "and must label the Raft-membership roster as its own claim"
            );
        }
        other => panic!("deactivation must be refused while a learner exists: {other:?}"),
    }
    assert_eq!(
        coordinator
            .active_protocol_range()
            .await
            .expect("read the range"),
        (
            crate::store::AUTH_LEARNER_PROTOCOL,
            crate::store::AUTH_LEARNER_PROTOCOL
        ),
        "a refused deactivation must not move the range"
    );

    // 6b. And the removal endpoint an operator reaches for next says the
    //     same thing. It used to answer "not found" for this node — a 404
    //     for a row the roster is printing, which reads as "you typed the
    //     wrong id" rather than "this release cannot remove it". The
    //     refusal has to name the node and be its own code.
    let refused_removal = coordinator
        .remove_voter(&learner.identity.node_id)
        .await
        .expect_err("this release has no removal path for a member with no vote");
    assert_eq!(
        refused_removal.code(),
        "cluster_non_voter_removal_unsupported",
        "a learner the roster lists must not be reported as missing: {refused_removal}"
    );
    assert!(
        refused_removal
            .to_string()
            .contains(&learner.identity.node_id),
        "the refusal must name the node the operator asked about: {refused_removal}"
    );
    assert_eq!(
        coordinator
            .remove_voter("a-node-id-that-was-never-in-this-cluster")
            .await
            .expect_err("an unknown node is still not found")
            .code(),
        "cluster_node_not_found",
        "and the genuine 404 must keep answering 404"
    );

    // 7. A learner never advances replicated schema. Rewind the marker
    //    one step and make the learner's own boot call — `open`, not
    //    `open_or_migrate` — against its own client: it must refuse and
    //    leave the marker exactly where it found it. What a voter's boot
    //    call does with the same marker is covered by the schema
    //    migration tests in `store::hiqlite`; what matters here is that
    //    the learner is not the node that does it.
    let learner_client = learner.local_client.clone().expect("learner client");
    // 80 and 81 remain readable compatibility layouts; 79 requires migration.
    let previous_schema = crate::store::AUTH_SCHEMA_BASELINE_VERSION - 2;
    source_client
        .execute(
            "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
            hiqlite::params!(previous_schema),
        )
        .await
        .expect("rewind the replicated schema marker");
    let refused = HiqliteAuthStore::open(
        learner_client.clone(),
        &learner_dir.path().join("learner-migration-probe.db"),
    )
    .await
    .err()
    .expect("a learner must refuse a schema it does not implement")
    .to_string();
    assert!(refused.contains("schema"), "{refused}");
    assert_eq!(
        replicated_schema_version(&source_client).await,
        previous_schema,
        "a learner must not advance replicated schema"
    );
    source_client
        .execute(
            "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
            hiqlite::params!(crate::store::AUTH_SCHEMA_BASELINE_VERSION),
        )
        .await
        .expect("restore the replicated schema marker");

    // 7. The role is durable across a restart: membership.json carries it,
    //    and the restarted process must not wait for a promotion it was
    //    admitted specifically not to receive.
    let persisted = read_local_membership(learner_dir.path())
        .expect("read the learner record")
        .expect("a joined node persists one");
    assert_eq!(persisted.role, ClusterRole::Learner);
    assert_eq!(persisted.version, 2);
    learner.shutdown().await.expect("drain the learner");
    observed.stop_node(&learner.identity.node_id).await;
    drop(learner);

    // The record a restart reads, read back through the same function both
    // boot paths use to resume a role.
    let resumed = read_local_membership(learner_dir.path())
        .expect("read the record a restart would resume from")
        .expect("still present after shutdown");
    assert_eq!(boot_cluster_role(Some(&resumed)), ClusterRole::Learner);

    // Real shutdown drains the vendor listeners; restart observes the
    // actual cluster again without promoting the durable learner role.
    let restart = observed
        .select(&learner_config, true)
        .await
        .expect("restart the durable learner with real observation");
    assert_eq!(restart.identity.raft_id, learner_token.raft_id);
    let restarted_membership = restart.membership_manager();
    assert!(!restarted_membership
        .local_node_is_committed_voter()
        .await
        .expect("read restarted learner role"));
    assert!(!restarted_membership.may_run_cluster_jobs().await);
    assert_eq!(
        read_local_membership(learner_dir.path())
            .expect("read restarted learner record")
            .expect("restarted learner retains durable membership")
            .role,
        ClusterRole::Learner
    );
    restart.shutdown().await.expect("drain restarted learner");
    observed.stop_node(&restart.identity.node_id).await;

    http_task.abort();
    source.shutdown().await.expect("drain the source voter");
}

/// A learner never proposes a replicated schema migration.
///
/// The boot path picks how to open the store from the role this node was
/// admitted with, and swapping the learner arm to `open_or_migrate`
/// survived everywhere — including the separate-process harness, which
/// never sets up a learner joining a cluster whose replicated schema is
/// behind. Nothing looked at the one thing that distinguishes the arms.
///
/// So construct exactly that: rewind the cluster's replicated schema
/// marker, then open the store both ways against the same live cluster.
/// The learner has to refuse and leave the marker where it found it; the
/// voter has to advance it. Either arm collapsing onto the other fails
/// one of the two halves.
#[cfg(feature = "hiqlite-store")]
#[test]
fn a_learner_refuses_a_behind_schema_that_a_voter_migrates() {
    startup_observer::run_full_hiqlite_fixture(
        "k06-r1-schema-role",
        learner_schema_observation_fixture,
    );
}

#[cfg(feature = "hiqlite-store")]
async fn learner_schema_observation_fixture() {
    install_default_crypto_provider();

    let dir = tempfile::tempdir().expect("schema role data dir");
    let config = membership_test_config(dir.path());
    drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("activate a one-voter cluster");
    let client = selected
        .local_client
        .as_ref()
        .expect("voter client")
        .clone();

    let current = replicated_schema_version(&client).await;
    assert_eq!(current, crate::store::AUTH_SCHEMA_BASELINE_VERSION);
    // Both baseline 81 and sharing 80 are readable; 79 needs migration.
    let behind = current - 2;
    client
        .execute(
            "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
            hiqlite::params!(behind),
        )
        .await
        .expect("rewind the replicated schema marker");

    // The learner arm: refuse, and change nothing. The refusal is asserted
    // in full, because that is what distinguishes the arms — swapping this
    // to `open_or_migrate` also fails here, but on a migration step rather
    // than on the compatibility check, and an operator reading "duplicate
    // column name" learns nothing about what their learner is waiting for.
    let refused = open_store_for_role(
        ClusterRole::Learner,
        client.clone(),
        &dir.path().join("learner-probe.db"),
    )
    .await
    .err()
    .expect("a learner must refuse a replicated schema it does not implement")
    .to_string();
    assert!(
        refused.contains(&format!(
            "cluster schema {behind} is incompatible with voter schema {}",
            crate::store::AUTH_SCHEMA_VERSION
        )),
        "the learner must refuse on the compatibility check, not inside a migration: \
             {refused}"
    );
    assert_eq!(
        replicated_schema_version(&client).await,
        behind,
        "a learner must not advance replicated schema"
    );

    // And the refusal was about the schema, not about learners: put the
    // marker back and the same arm opens the same cluster.
    client
        .execute(
            "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
            hiqlite::params!(crate::store::AUTH_SCHEMA_BASELINE_VERSION),
        )
        .await
        .expect("restore the replicated schema marker");
    drop(
        open_store_for_role(
            ClusterRole::Learner,
            client.clone(),
            &dir.path().join("learner-probe.db"),
        )
        .await
        .expect("a learner opens a schema it does implement"),
    );

    // The voter arm is the one that may migrate, and it opens the same
    // cluster through `open_or_migrate`.
    // Reconstruct the preceding schema, rather than rewinding only its
    // marker: migrations 80–81 create these tables and must actually execute.
    for table in [
        "sharing_ingress_custody",
        "sharing_delivery_grants",
        "sharing_relay_upstream",
        "sharing_endpoint_manifest",
        "sharing_catalogue_revisions",
        "sharing_import_rotations",
        "sharing_rotations",
        "sharing_watch",
        "sharing_assignments",
        "sharing_viewers",
        "sharing_imports",
        "sharing_export_libraries",
        "sharing_exports",
        "sharing_invitations",
        "sharing_identity",
    ] {
        client
            .execute(format!("DROP TABLE {table}"), hiqlite::params!())
            .await
            .expect("remove the test fixture's migration-80/81 tables");
    }
    client
        .execute(
            "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
            hiqlite::params!(behind),
        )
        .await
        .expect("restore the behind marker before the voter migration");
    drop(
        open_store_for_role(
            ClusterRole::Voter,
            client.clone(),
            &dir.path().join("voter-probe.db"),
        )
        .await
        .expect("a voter opens the cluster it is responsible for migrating"),
    );
    assert_eq!(
        replicated_schema_version(&client).await,
        crate::store::AUTH_SCHEMA_BASELINE_VERSION
    );

    selected.shutdown().await.expect("stop the voter");
}

/// Activation is decided on the binary each voter is running *now*.
///
/// Both refusals below are constructed against a live replicated cluster:
/// one voter whose capability row was never written, and one whose row
/// survives from a build that was rolled back. The second is the case the
/// heartbeat coupling exists for — the row is present and looks like a
/// proof until it is compared with that node's current heartbeat.
#[cfg(feature = "hiqlite-store")]
#[test]
fn activation_refuses_a_voter_whose_running_binary_is_unproven() {
    startup_observer::run_full_hiqlite_fixture("k06-r1-capability", capability_observation_fixture);
}

#[cfg(feature = "hiqlite-store")]
async fn capability_observation_fixture() {
    install_default_crypto_provider();

    let dir = tempfile::tempdir().expect("unproven voter data dir");
    let config = membership_test_config(dir.path());
    drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("activate a one-voter cluster");
    let membership = selected.membership_manager();
    let client = selected
        .local_client
        .as_ref()
        .expect("voter client")
        .clone();

    // A second node that has heartbeated but never proved the capability,
    // standing in for a voter still running the previous release. Its
    // heartbeats are *current* on purpose: activation also refuses for a
    // node that has gone silent, and this test is about the capability
    // rule, not that one.
    let beat = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    client
        .execute(
            "INSERT INTO cluster_nodes \
                 (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL)",
            hiqlite::params!("unproven-voter", 77_i64, "127.0.0.1:1", "127.0.0.1:2", beat),
        )
        .await
        .expect("seed a voter running an older binary");
    client
        .execute(
            "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 VALUES ($1, $2, $3)",
            hiqlite::params!("unproven-voter", "membership_removal_attempt_refs_v1", beat),
        )
        .await
        .expect("that binary does write the capability it knows about");

    match membership.activate_learner_protocol().await {
        Err(super::super::membership::MembershipError::LearnerProtocolUpgradeRequired(nodes)) => {
            assert_eq!(nodes, vec!["unproven-voter".to_owned()]);
        }
        other => panic!("an unproven voter must block activation: {other:?}"),
    }
    assert_eq!(
        membership
            .active_protocol_range()
            .await
            .expect("read the range"),
        (
            crate::store::AUTH_PROTOCOL_MIN,
            crate::store::AUTH_PROTOCOL_MIN
        ),
        "a refused activation must not move the range"
    );

    // Now the deliberate stale case: the node proves the capability, then
    // heartbeats again on a binary that does not know about it. The row is
    // still there; only its timestamp says it is no longer current.
    client
        .execute(
            "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 VALUES ($1, $2, $3)",
            hiqlite::params!("unproven-voter", "learner_protocol_v5", beat),
        )
        .await
        .expect("the node proves the learner protocol once");
    assert!(
        membership
            .protocol_status()
            .await
            .expect("status after the proof")
            .learner_protocol_pending
            .is_empty(),
        "the cluster is briefly ready"
    );
    client
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = $1 WHERE node_id = $2",
            hiqlite::params!(beat + 1, "unproven-voter"),
        )
        .await
        .expect("the rolled-back binary heartbeats");

    match membership.activate_learner_protocol().await {
        Err(super::super::membership::MembershipError::LearnerProtocolUpgradeRequired(nodes)) => {
            assert_eq!(
                nodes,
                vec!["unproven-voter".to_owned()],
                "a capability older than its node's heartbeat is not a current proof"
            );
        }
        other => panic!("a stale capability must block activation: {other:?}"),
    }
    assert_eq!(
        membership
            .active_protocol_range()
            .await
            .expect("read the range"),
        (
            crate::store::AUTH_PROTOCOL_MIN,
            crate::store::AUTH_PROTOCOL_MIN
        )
    );

    selected.shutdown().await.expect("stop the voter");
}
