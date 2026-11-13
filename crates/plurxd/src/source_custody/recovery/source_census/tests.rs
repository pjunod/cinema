//! A dead owner's poisoned witness, its real Store custody and job source.
use super::*;
use std::os::unix::fs::PermissionsExt;

const KERNEL: &str = "linux:source-census-test";
const REBOOTED: &str = "linux:source-census-rebooted";
const WRITER_NAMESPACE: u64 = 4_026_532_885;
/// The same machine's kernel after a reboot.
/// A network superblock the daemon mounts besides the census's.
const STORAGE: u64 = 66;

struct Dead {
    _root: tempfile::TempDir,
    database: std::path::PathBuf,
    store: Arc<dyn Store>,
    witnesses: std::path::PathBuf,
    legacy: std::path::PathBuf,
    token: plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
    lease: i64,
    source: std::fs::Metadata,
    anchor: std::path::PathBuf,
}

fn private(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("private evidence");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("0600");
}

fn set_witness_namespace(path: &Path, namespace: Option<serde_json::Value>) {
    let mut evidence: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read fixture witness"))
            .expect("fixture witness JSON");
    if let Some(namespace) = namespace {
        evidence["daemon_namespace"] = namespace;
    } else {
        evidence
            .as_object_mut()
            .expect("witness object")
            .remove("daemon_namespace");
    }
    private(
        path,
        &serde_json::to_vec(&evidence).expect("serialize fixture witness"),
    );
}

async fn dead_owner() -> Dead {
    let root = tempfile::tempdir().expect("census fixture");
    let database = root.path().join("plurx.db");
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open(&database).expect("fixture Store"));
    let (_, active) = crate::background_jobs::tests::active_on(Arc::clone(&store)).await;
    let token = crate::background_jobs::tests::source_custody(&active).expect("exact custody");
    // The owner dies without its explicit join: Drop never settles custody.
    drop(active);
    let BackgroundSourceOwner::Job { token: job } = &token.owner else {
        unreachable!()
    };
    let lease = job.lease_expires_ms;
    // Bind the fixture payload's (size 100, mtime 1) source to a real file.
    let path = root.path().join("movie.mkv");
    std::fs::write(&path, [7u8; 100]).expect("source");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| {
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        })
        .expect("source mtime");
    rusqlite::Connection::open(&database)
        .expect("fixture connection")
        .execute(
            "UPDATE files SET path=?1 WHERE path='/worker-fault/movie.mkv'",
            [path.to_str().expect("utf-8 path")],
        )
        .expect("bind fixture source path");
    let source = std::fs::metadata(&path).expect("source identity");
    let recovery = root.path().join("source-custody-recovery");
    let witnesses = recovery.join("v2");
    let legacy = recovery.join("v1");
    for directory in [&recovery, &witnesses, &legacy] {
        std::fs::create_dir_all(directory).expect("recovery directory");
        // Production recovery directories are private to the worker.
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).expect("0700");
    }
    let name = format!("background-{}", token.custody_id);
    let anchor = witnesses.join(format!("{name}.anchor"));
    let nonce = uuid::Uuid::new_v4().to_string();
    private(&anchor, nonce.as_bytes());
    let metadata = std::fs::metadata(&anchor).expect("anchor identity");
    let activation = uuid::Uuid::new_v4().to_string();
    private(
        &witnesses.join(format!("{name}.armed")),
        activation.as_bytes(),
    );
    let evidence = serde_json::json!({
        "version": 2, "reader_model": "daemon_mediated_v1", "kernel_instance": KERNEL,
        "activation_id": activation, "node_id": job.node_id, "anchor_nonce": nonce,
        "anchor_device": metadata.dev(), "anchor_inode": metadata.ino(),
        "anchor_ctime": reader::anchor_ctime(&metadata),
        "daemon_namespace": {"pid_namespace": WRITER_NAMESPACE, "init": true},
        "record": Record::Background { token: token.clone() },
    });
    private(
        &witnesses.join(format!("{name}.json")),
        serde_json::to_vec(&evidence).expect("evidence").as_slice(),
    );
    // Poisoning rewrites the anchor after its witness, changing its ctime;
    // the ordinary replay can never accept it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&anchor, format!("!{}", &nonce[1..])).expect("poison anchor");
    assert_ne!(
        reader::anchor_ctime(&std::fs::metadata(&anchor).expect("anchor")),
        reader::anchor_ctime(&metadata)
    );
    Dead {
        _root: root,
        database,
        store,
        witnesses,
        legacy,
        token,
        lease,
        source,
        anchor,
    }
}

impl Dead {
    fn held(&self) -> i64 {
        rusqlite::Connection::open(&self.database)
            .expect("fixture connection")
            .query_row(
                "SELECT COUNT(*) FROM background_source_custody",
                [],
                |row| row.get(0),
            )
            .expect("custody count")
    }
    fn job(&self) -> &JobToken {
        match &self.token.owner {
            BackgroundSourceOwner::Job { token } => token,
            _ => unreachable!(),
        }
    }
    fn proof(&self, readers: i64) -> Proof {
        let anchor = std::fs::metadata(&self.anchor).expect("anchor identity");
        let job = self.job();
        let pass = |at_ms| {
            serde_json::json!({"at_ms": at_ms, "anchor_locks": 0, "silly_renames": 0,
                "path_references": 0, "superblocks": [{"device": self.source.dev(),
                "inode": self.source.ino(), "readers": readers, "writers": 0}],
                "owner_teardown": {"pid_namespace": WRITER_NAMESPACE,
                    "daemon_is_namespace_init": true, "preexisting": 0,
                    "other_daemons": 0, "foreign_storage_holders": 0,
                    "foreign_media_tools": 0, "unreadable": 0,
                    "storage_devices": [STORAGE, self.source.dev()]}})
        };
        serde_json::from_value(serde_json::json!({
            "version": 1, "model": MODEL, "kernel_instance": REBOOTED,
            "prior_boot": {"kernel_instance": KERNEL},
            "owner": {"custody_id": self.token.custody_id, "job_id": job.job_id,
                "node_id": job.node_id, "boot_id": job.boot_id, "claim_id": job.claim_id,
                "fence": job.fence},
            "anchor": {"device": anchor.dev(), "inode": anchor.ino(),
                "ctime": reader::anchor_ctime(&anchor)},
            "source": {"path": "/host/movie.mkv", "inode": self.source.ino(),
                "size": 100, "mtime": 1},
            "passes": [pass(self.lease + 1_000), pass(self.lease + 4_000)],
        }))
        .expect("proof shape")
    }
    async fn apply(&self, proof: &Proof, process_started_ms: i64) -> Result<(), String> {
        self.apply_on(REBOOTED, proof, process_started_ms).await
    }
    async fn apply_on(
        &self,
        kernel: &str,
        proof: &Proof,
        process_started_ms: i64,
    ) -> Result<(), String> {
        apply(
            &self.store,
            &self.witnesses,
            &self.legacy,
            &self.job().node_id,
            &Context {
                kernel,
                process_started_ms,
                now_ms: self.lease + 5_000,
            },
            proof,
        )
        .await
    }
    /// Retention removed the owner's job row while its custody stayed charged.
    fn forget_job(&self) {
        let connection = rusqlite::Connection::open(&self.database).expect("fixture connection");
        connection
            .execute_batch("PRAGMA foreign_keys=OFF")
            .expect("fixture pragma");
        for table in ["background_job_attempts", "background_jobs"] {
            let column = if table == "background_jobs" {
                "id"
            } else {
                "job_id"
            };
            connection
                .execute(
                    &format!("DELETE FROM {table} WHERE {column}=?1"),
                    [&self.job().job_id],
                )
                .expect("forget job");
        }
    }
}

#[tokio::test]
async fn dead_owner_source_census_releases_only_exact_quiescent_custody() {
    let dead = dead_owner().await;
    let started = dead.lease + 500;
    assert_eq!(dead.held(), 1, "dead owner's custody is still charged");
    let refused = |result: Result<(), String>, reason: &str| {
        let error = result.expect_err(reason);
        assert!(error.contains(reason), "expected {reason:?}, got {error:?}");
    };
    refused(
        dead.apply(&dead.proof(1), started).await,
        "stale, partial or nonzero",
    );
    refused(
        dead.apply(&dead.proof(0), dead.lease - 1).await,
        "exact dead-owner witness differ",
    );
    let mut incoherent = dead.proof(0);
    incoherent.source.inode += 1;
    refused(
        dead.apply(&incoherent, started).await,
        "stale, partial or nonzero",
    );
    let mut close = dead.proof(0);
    close.passes[1].at_ms = close.passes[0].at_ms + 1;
    refused(
        dead.apply(&close, started).await,
        "stale, partial or nonzero",
    );
    let mut touched = dead.proof(0);
    touched.anchor.ctime.1 ^= 1;
    refused(dead.apply(&touched, started).await, "anchor changed");
    let holder = std::fs::File::open(&dead.anchor).expect("anchor");
    holder.try_lock().expect("foreign lock holder");
    refused(dead.apply(&dead.proof(0), started).await, "anchor lock");
    drop(holder);

    let quiescent = dead.proof(0);
    dead.apply(&quiescent, started)
        .await
        .expect("exact ended-kernel proof releases");
    assert_eq!(dead.held(), 0);
    assert!(
        !dead.anchor.exists()
            && !dead
                .witnesses
                .join(format!("background-{}.json", dead.token.custody_id))
                .exists(),
        "the worker clears only its own witness"
    );
    dead.apply(&quiescent, started)
        .await
        .expect("idempotent replay");
}
#[tokio::test]
async fn namespace_and_zero_open_counts_cannot_replace_ended_kernel_proof() {
    let dead = dead_owner().await;
    let started = dead.lease + 500;
    let mut same_kernel = dead.proof(0);
    same_kernel.kernel_instance = KERNEL.into();
    same_kernel.prior_boot = None;
    let witness = dead
        .witnesses
        .join(format!("background-{}.json", dead.token.custody_id));
    let refused = |result: Result<(), String>| {
        let error = result.expect_err("same-kernel census cannot authorize settlement");
        assert!(error.contains("whole process tree stopped"), "{error}");
    };
    for namespace in [
        None,
        Some(serde_json::json!({"pid_namespace": WRITER_NAMESPACE, "init": false})),
        Some(serde_json::json!({"pid_namespace": WRITER_NAMESPACE + 1, "init": true})),
        Some(serde_json::json!({"pid_namespace": WRITER_NAMESPACE, "init": true})),
    ] {
        set_witness_namespace(&witness, namespace);
        refused(dead.apply_on(KERNEL, &same_kernel, started).await);
    }
    // Even complete-looking host coverage and zero open counts are advisory.
    for pass in &mut same_kernel.passes {
        pass._owner_teardown = Some(serde_json::json!({
            "pid_namespace": WRITER_NAMESPACE,
            "daemon_is_namespace_init": true,
            "preexisting": 0,
            "other_daemons": 0,
            "foreign_storage_holders": 0,
            "foreign_media_tools": 0,
            "unreadable": 0,
            "storage_devices": [STORAGE, dead.source.dev()]
        }));
    }
    set_witness_namespace(
        &witness,
        Some(serde_json::json!({"pid_namespace": WRITER_NAMESPACE, "init": true})),
    );
    refused(dead.apply_on(KERNEL, &same_kernel, started).await);
    assert_eq!(dead.held(), 1);
}
#[tokio::test]
async fn source_census_requires_the_exact_ended_witness_kernel() {
    let dead = dead_owner().await;
    let started = dead.lease + 500;
    let mut missing = dead.proof(0);
    missing.prior_boot = None;
    assert!(dead.apply_on(REBOOTED, &missing, started).await.is_err());
    let mut wrong = dead.proof(0);
    wrong.prior_boot = Some(PriorBoot {
        kernel_instance: "linux:another-ended-kernel".into(),
    });
    assert!(dead.apply_on(REBOOTED, &wrong, started).await.is_err());
    let mut wrong_census = dead.proof(0);
    wrong_census.kernel_instance = KERNEL.into();
    assert!(dead
        .apply_on(REBOOTED, &wrong_census, started)
        .await
        .is_err());
    assert_eq!(dead.held(), 1);
}
#[tokio::test]
async fn ended_kernel_releases_exact_job_after_row_and_source_metadata_change() {
    let dead = dead_owner().await;
    dead.forget_job();
    rusqlite::Connection::open(&dead.database)
        .expect("fixture connection")
        .execute(
            "UPDATE files SET size=101,mtime=2 WHERE path=?1",
            [dead._root.path().join("movie.mkv").to_str().expect("path")],
        )
        .expect("rescan file row");
    let sidecar = dead
        .witnesses
        .join(format!("background-{}.source", dead.token.custody_id));
    private(
        &sidecar,
        format!("{} {}", dead.source.dev(), dead.source.ino() + 99).as_bytes(),
    );
    let proof = dead.proof(0);
    dead.apply(&proof, dead.lease + 500)
        .await
        .expect("ended kernel proves the old exact owner stopped");
    assert_eq!(dead.held(), 0);
    assert!(!dead.anchor.exists());
}
/// A playback owner that died with its daemon: registered, armed and renewed
/// through the real Store API (each heartbeat advances the row's fence and
/// lease past the activation copy in its witness), never quiescent, with its
/// witness poisoned by an unmediated FFmpeg launch.
struct DeadPlayback {
    _root: tempfile::TempDir,
    database: std::path::PathBuf,
    store: Arc<dyn Store>,
    witnesses: std::path::PathBuf,
    legacy: std::path::PathBuf,
    /// The activation-time token the witness carries.
    token: ForegroundSourceToken,
    /// The Store row's renewed lease.
    lease: i64,
    source: std::fs::Metadata,
    source_path: std::path::PathBuf,
    anchor: std::path::PathBuf,
    sidecar: std::path::PathBuf,
}

const PLAYBACK_NODE: &str = "playback-census-node";

async fn dead_playback() -> DeadPlayback {
    use plurx_core::store::background_jobs_resources::{
        ForegroundSourcePolicy, ForegroundSourceProjection,
    };
    let root = tempfile::tempdir().expect("census fixture");
    let database = root.path().join("plurx.db");
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open(&database).expect("fixture Store"));
    let library_root = root.path().join("library");
    std::fs::create_dir_all(&library_root).expect("library root");
    let path = library_root.join("movie.mkv");
    std::fs::write(&path, [7u8; 100]).expect("source");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| {
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        })
        .expect("source mtime");
    let library = store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "playback census fixture".into(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec![library_root.clone()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "playback census fixture".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let file_id = store
        .upsert_file(
            item,
            path.to_str().expect("utf-8"),
            100,
            1,
            &Default::default(),
        )
        .await
        .expect("file");
    let file = store
        .get_file(file_id)
        .await
        .expect("read")
        .expect("file row");
    let registered = 1_794_000_000_000_i64;
    // Use the production request builder and its held-object override, rather
    // than a fixture-only file-row generation that production does not retain.
    let identity = crate::source_custody::SourceIdentity {
        node_id: PLAYBACK_NODE.into(),
        boot_id: uuid::Uuid::new_v4(),
    };
    let request = identity.request(
        Arc::clone(&store),
        &file,
        &uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4(),
        ForegroundSourcePolicy::Rolling,
    );
    let row_generation = request.owner.source_generation.clone();
    let mut owner = request.object_version("opened-source-object-version").owner;
    assert_ne!(owner.source_generation, row_generation);
    owner.producer_attempt = "1".into();
    let token = store
        .register_foreground_source(owner, registered, registered + 30_000)
        .await
        .expect("register")
        .expect("registered owner");
    let activation = uuid::Uuid::new_v4().to_string();
    assert!(store
        .activate_foreground_source_custody(token.clone(), activation.clone())
        .await
        .expect("activate"));
    // Heartbeats renew the row for ten minutes; each advances its fence.
    let mut current = token.clone();
    let mut lease = registered + 30_000;
    for beat in 1..=120_u64 {
        let now = registered + beat as i64 * 5_000;
        let mut projection = ForegroundSourceProjection::startup(now);
        projection.observation_version = beat;
        lease = now + 30_000;
        current = store
            .project_foreground_source(current, projection, now, lease)
            .await
            .expect("heartbeat")
            .expect("renewed owner");
    }
    assert!(current.fence > token.fence, "heartbeats advance the fence");
    let source = std::fs::metadata(&path).expect("source identity");
    let recovery = root.path().join("source-custody-recovery");
    let witnesses = recovery.join("v2");
    let legacy = recovery.join("v1");
    for directory in [&recovery, &witnesses, &legacy] {
        std::fs::create_dir_all(directory).expect("recovery directory");
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).expect("0700");
    }
    let name = format!("foreground-{}", token.owner.owner_id);
    let anchor = witnesses.join(format!("{name}.anchor"));
    let nonce = uuid::Uuid::new_v4().to_string();
    private(&anchor, nonce.as_bytes());
    let metadata = std::fs::metadata(&anchor).expect("anchor identity");
    private(
        &witnesses.join(format!("{name}.armed")),
        activation.as_bytes(),
    );
    let evidence = serde_json::json!({
        "version": 2, "reader_model": "daemon_mediated_v1", "kernel_instance": KERNEL,
        "activation_id": activation, "node_id": PLAYBACK_NODE, "anchor_nonce": nonce,
        "anchor_device": metadata.dev(), "anchor_inode": metadata.ino(),
        "anchor_ctime": reader::anchor_ctime(&metadata),
        "record": Record::Foreground { token: token.clone() },
    });
    private(
        &witnesses.join(format!("{name}.json")),
        serde_json::to_vec(&evidence).expect("evidence").as_slice(),
    );
    // The inode the path named as the owner was armed.
    let sidecar = witnesses.join(format!("{name}.source"));
    private(
        &sidecar,
        format!("{} {}", source.dev(), source.ino()).as_bytes(),
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&anchor, format!("!{}", &nonce[1..])).expect("poison anchor");
    DeadPlayback {
        _root: root,
        database,
        store,
        witnesses,
        legacy,
        token,
        lease,
        source,
        source_path: path,
        anchor,
        sidecar,
    }
}

impl DeadPlayback {
    fn held(&self) -> i64 {
        rusqlite::Connection::open(&self.database)
            .expect("fixture connection")
            .query_row(
                "SELECT COUNT(*) FROM foreground_source_permissions WHERE custody_held=1",
                [],
                |row| row.get(0),
            )
            .expect("held count")
    }
    fn proof_value(&self, readers: i64) -> serde_json::Value {
        let anchor = std::fs::metadata(&self.anchor).expect("anchor identity");
        let owner = &self.token.owner;
        let pass = |at_ms| {
            serde_json::json!({"at_ms": at_ms, "anchor_locks": 0, "silly_renames": 0,
                "path_references": 0, "superblocks": [{"device": self.source.dev(),
                "inode": self.source.ino(), "readers": readers, "writers": 0}]})
        };
        serde_json::json!({
            "version": 1, "model": MODEL, "kernel_instance": REBOOTED,
            "prior_boot": {"kernel_instance": KERNEL},
            "owner": {"custody_id": owner.owner_id, "lifetime_id": owner.lifetime_id,
                "session_incarnation": owner.session_incarnation,
                "producer_attempt": owner.producer_attempt, "node_id": owner.node_id,
                "boot_id": owner.boot_id, "fence": self.token.fence},
            "anchor": {"device": anchor.dev(), "inode": anchor.ino(),
                "ctime": reader::anchor_ctime(&anchor)},
            "source": {"path": "/host/movie.mkv", "inode": self.source.ino(),
                "size": 100, "mtime": 1},
            "passes": [pass(self.lease + 1_000), pass(self.lease + 4_000)],
        })
    }
    fn proof(&self, readers: i64) -> Proof {
        serde_json::from_value(self.proof_value(readers)).expect("proof shape")
    }
    async fn apply_as(&self, proof: &Proof, node: &str, kernel: &str) -> Result<(), String> {
        apply(
            &self.store,
            &self.witnesses,
            &self.legacy,
            node,
            &Context {
                kernel,
                process_started_ms: self.lease - 20_000,
                now_ms: self.lease + 5_000,
            },
            proof,
        )
        .await
    }
    async fn apply(&self, proof: &Proof) -> Result<(), String> {
        self.apply_as(proof, PLAYBACK_NODE, REBOOTED).await
    }
    fn set_file(&self, column: &str, value: &str) {
        rusqlite::Connection::open(&self.database)
            .expect("fixture connection")
            .execute(
                &format!("UPDATE files SET {column}={value} WHERE id=?1"),
                [self.token.owner.file_id],
            )
            .expect("change file row");
    }
}

#[tokio::test]
async fn dead_playback_owner_source_census_releases_only_exact_quiescent_custody() {
    let dead = dead_playback().await;
    assert_eq!(dead.held(), 1);
    let refused = |result: Result<(), String>, reason: &str| {
        let error = result.expect_err(reason);
        assert!(error.contains(reason), "expected {reason:?}, got {error:?}");
    };
    assert!(matches!(dead.proof(0).owner, Owner::Foreground(_)));
    let mut both = dead.proof_value(0);
    both["owner"]["job_id"] = "job".into();
    both["owner"]["claim_id"] = "claim".into();
    assert!(serde_json::from_value::<Proof>(both).is_err());
    refused(
        dead.apply(&dead.proof(1)).await,
        "stale, partial or nonzero",
    );
    let mut early = dead.proof(0);
    early.passes[0].at_ms = dead.lease - 1;
    early.passes[1].at_ms = dead.lease + 3_000;
    refused(dead.apply(&early).await, "may still be live");
    refused(
        dead.apply_as(&dead.proof(0), "another-node", REBOOTED)
            .await,
        "exact dead-owner witness differ",
    );
    let mut other = dead.proof_value(0);
    other["owner"]["session_incarnation"] = uuid::Uuid::new_v4().to_string().into();
    refused(
        dead.apply(&serde_json::from_value(other).expect("shape"))
            .await,
        "exact dead-owner witness differ",
    );
    let mut missing_boot = dead.proof(0);
    missing_boot.prior_boot = None;
    refused(
        dead.apply(&missing_boot).await,
        "exact dead-owner witness differ",
    );
    let mut touched = dead.proof(0);
    touched.anchor.ctime.1 ^= 1;
    refused(dead.apply(&touched).await, "anchor changed");
    let holder = std::fs::File::open(&dead.anchor).expect("anchor");
    holder.try_lock().expect("foreign lock holder");
    refused(dead.apply(&dead.proof(0)).await, "anchor lock");
    drop(holder);

    // A -> B -> A defeats an activation-time path stat. A held description
    // can still read B while every observation of A reports zero readers.
    let alternate = dead.source_path.with_file_name("alternate.mkv");
    let saved = dead.source_path.with_file_name("saved.mkv");
    std::fs::write(&alternate, [9u8; 100]).expect("alternate source");
    let mut actual_open = std::fs::File::open(&alternate).expect("open B");
    std::fs::rename(&dead.source_path, &saved).expect("move A");
    std::fs::rename(&alternate, &dead.source_path).expect("path names B");
    assert_ne!(
        std::fs::metadata(&dead.source_path).expect("B").ino(),
        dead.source.ino()
    );
    std::fs::rename(&dead.source_path, &alternate).expect("move B");
    std::fs::rename(&saved, &dead.source_path).expect("path names A again");
    assert_eq!(
        std::fs::metadata(&dead.source_path).expect("A").ino(),
        dead.source.ino()
    );
    let mut same_kernel = dead.proof(0);
    same_kernel.kernel_instance = KERNEL.into();
    same_kernel.prior_boot = None;
    refused(
        dead.apply_as(&same_kernel, PLAYBACK_NODE, KERNEL).await,
        "whole process tree stopped",
    );
    let mut byte = [0];
    std::io::Read::read_exact(&mut actual_open, &mut byte).expect("B remains readable");
    assert_eq!(byte, [9]);
    assert_eq!(
        dead.held(),
        1,
        "zero counts and a restored pathname grant no closure"
    );
    drop(actual_open);

    // After the exact kernel ended, the original token is the identity.
    // Neither a stale sidecar nor a new catalogue generation can veto closure.
    private(&dead.sidecar, b"obsolete pathname identity");
    dead.set_file("size", "101");
    dead.set_file("mtime", "2");
    dead.set_file("path", "'/rescanned/title.mkv'");
    let quiescent = dead.proof(0);
    dead.apply(&quiescent)
        .await
        .expect("exact ended kernel releases production-version owner");
    assert_eq!(dead.held(), 0);
    assert!(!dead.anchor.exists() && !dead.sidecar.exists());
    dead.apply(&quiescent).await.expect("idempotent replay");
}

#[tokio::test]
async fn a_census_settled_but_unreleased_playback_owner_finishes_on_replay() {
    let dead = dead_playback().await;
    // A crash after the settlement mark, before the row release.
    rusqlite::Connection::open(&dead.database)
        .expect("fixture connection")
        .execute(
            "UPDATE foreground_source_owners SET settlement_proof='joined' WHERE owner_id=?1",
            [&dead.token.owner.owner_id],
        )
        .expect("crash after settlement mark");
    dead.apply(&dead.proof(0))
        .await
        .expect("replay finishes the exact release");
    assert_eq!(dead.held(), 0);
    assert!(
        !dead.anchor.exists(),
        "the witness leaves only after release"
    );
}
