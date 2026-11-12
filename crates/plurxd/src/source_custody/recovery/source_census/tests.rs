//! A dead owner's poisoned witness, its real Store custody and job source.
use super::*;
use std::os::unix::fs::PermissionsExt;

const KERNEL: &str = "linux:source-census-test";
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
                "inode": self.source.ino(), "readers": readers, "writers": 0}]})
        };
        serde_json::from_value(serde_json::json!({
            "version": 1, "model": MODEL, "kernel_instance": KERNEL,
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
        self.apply_in(proof, process_started_ms, None).await
    }
    async fn apply_in(
        &self,
        proof: &Proof,
        process_started_ms: i64,
        namespace_init: Option<u64>,
    ) -> Result<(), String> {
        apply(
            &self.store,
            &self.witnesses,
            &self.legacy,
            &self.job().node_id,
            &Context {
                kernel: KERNEL,
                process_started_ms,
                now_ms: self.lease + 5_000,
                namespace_init,
                storage_devices: namespace_init.map(|_| vec![STORAGE, self.source.dev()]),
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
    // Any open file description on any counted superblock refuses.
    refused(
        dead.apply(&dead.proof(1), started).await,
        "stale, partial or nonzero",
    );
    // A lease that had not ended before this process started may be ours.
    refused(
        dead.apply(&dead.proof(0), dead.lease - 1).await,
        "exact dead-owner witness differ",
    );
    // A census of another inode is not this job's source.
    let mut foreign = dead.proof(0);
    foreign.source.inode += 1;
    for pass in &mut foreign.passes {
        pass.superblocks[0].inode += 1;
    }
    refused(
        dead.apply(&foreign, started).await,
        "differs from the job's exact source",
    );
    // Passes too close together refuse.
    let mut close = dead.proof(0);
    close.passes[1].at_ms = close.passes[0].at_ms + 1;
    refused(
        dead.apply(&close, started).await,
        "stale, partial or nonzero",
    );
    // The anchor must be unchanged since the census observed it.
    let mut touched = dead.proof(0);
    touched.anchor.ctime.1 ^= 1;
    refused(dead.apply(&touched, started).await, "anchor changed");
    // The daemon's own anchor observation: another lock holder refuses.
    let holder = std::fs::File::open(&dead.anchor).expect("anchor");
    holder.try_lock().expect("foreign lock holder");
    refused(
        dead.apply(&dead.proof(0), started).await,
        "still holds the anchor lock",
    );
    drop(holder);
    // The owner recorded the inode it opened; a path naming another refuses.
    let sidecar = dead
        .witnesses
        .join(format!("background-{}.source", dead.token.custody_id));
    private(
        &sidecar,
        format!("{} {}", dead.source.dev(), dead.source.ino() + 1).as_bytes(),
    );
    refused(
        dead.apply(&dead.proof(0), started).await,
        "not the inode the owner opened",
    );
    std::fs::remove_file(&sidecar).expect("remove sidecar");
    private(
        &sidecar,
        format!("{} {}", dead.source.dev(), dead.source.ino()).as_bytes(),
    );
    assert_eq!(dead.held(), 1, "every refusal keeps the custody");

    let quiescent = dead.proof(0);
    dead.apply(&quiescent, started)
        .await
        .expect("exact quiescent census releases");
    assert_eq!(dead.held(), 0);
    assert!(
        !sidecar.exists(),
        "the source identity leaves with its witness"
    );
    assert!(
        !dead.anchor.exists()
            && !dead
                .witnesses
                .join(format!("background-{}.json", dead.token.custody_id))
                .exists(),
        "the worker clears only its own witness"
    );
    // Replay after release is a no-op until the operator removes the proof.
    dead.apply(&quiescent, started)
        .await
        .expect("idempotent replay");
}

#[tokio::test]
async fn a_forgotten_job_releases_only_with_the_owners_process_tree_proved_gone() {
    const NAMESPACE: u64 = 4_026_532_885;
    let dead = dead_owner().await;
    let started = dead.lease + 500;
    dead.forget_job();
    let refused = |result: Result<(), String>, reason: &str| {
        let error = result.expect_err(reason);
        assert!(error.contains(reason), "expected {reason:?}, got {error:?}");
    };
    let with_daemons = |namespace: u64, init: bool, preexisting: u32, others: u32, foreign: u32| {
        let mut proof = dead.proof(0);
        for pass in &mut proof.passes {
            pass.owner_teardown = Some(Teardown {
                pid_namespace: namespace,
                daemon_is_namespace_init: init,
                preexisting,
                other_daemons: others,
                foreign_storage_holders: foreign,
                foreign_media_tools: 0,
                unreadable: 0,
                storage_devices: vec![STORAGE, dead.source.dev()],
            });
        }
        proof
    };
    let with = |namespace: u64, init: bool, preexisting: u32, foreign: u32| {
        with_daemons(namespace, init, preexisting, 0, foreign)
    };
    // Without the job, the exact source binding is gone: a census alone refuses.
    refused(
        dead.apply_in(&dead.proof(0), started, Some(NAMESPACE))
            .await,
        "census job unavailable",
    );
    // A daemon that is not its namespace's init cannot rely on teardown.
    refused(
        dead.apply_in(&with(NAMESPACE, true, 0, 0), started, None)
            .await,
        "census job unavailable",
    );
    // The helper must have observed this daemon's own namespace.
    refused(
        dead.apply_in(&with(NAMESPACE + 1, true, 0, 0), started, Some(NAMESPACE))
            .await,
        "census job unavailable",
    );
    refused(
        dead.apply_in(&with(NAMESPACE, false, 0, 0), started, Some(NAMESPACE))
            .await,
        "census job unavailable",
    );
    // A process older than the owner's death, or a foreign storage holder, refuses.
    refused(
        dead.apply_in(&with(NAMESPACE, true, 1, 0), started, Some(NAMESPACE))
            .await,
        "census job unavailable",
    );
    refused(
        dead.apply_in(&with(NAMESPACE, true, 0, 1), started, Some(NAMESPACE))
            .await,
        "census job unavailable",
    );
    // Another `plurxd run` anywhere on the host refuses.
    refused(
        dead.apply_in(
            &with_daemons(NAMESPACE, true, 0, 1, 0),
            started,
            Some(NAMESPACE),
        )
        .await,
        "census job unavailable",
    );
    // An idle FFmpeg or plurx helper outside the namespace refuses.
    let mut idle = with(NAMESPACE, true, 0, 0);
    idle.passes[1]
        .owner_teardown
        .as_mut()
        .expect("teardown")
        .foreign_media_tools = 1;
    refused(
        dead.apply_in(&idle, started, Some(NAMESPACE)).await,
        "census job unavailable",
    );
    // A process the helper could not read is never assumed harmless.
    let mut unread = with(NAMESPACE, true, 0, 0);
    unread.passes[0]
        .owner_teardown
        .as_mut()
        .expect("teardown")
        .unreadable = 1;
    refused(
        dead.apply_in(&unread, started, Some(NAMESPACE)).await,
        "census job unavailable",
    );
    // The helper must have scanned every network superblock this daemon mounts.
    let mut narrow = with(NAMESPACE, true, 0, 0);
    for pass in &mut narrow.passes {
        pass.owner_teardown
            .as_mut()
            .expect("teardown")
            .storage_devices = vec![dead.source.dev()];
    }
    refused(
        dead.apply_in(&narrow, started, Some(NAMESPACE)).await,
        "census job unavailable",
    );
    // A census of a file outside every storage this daemon serves refuses.
    refused(
        apply(
            &dead.store,
            &dead.witnesses,
            &dead.legacy,
            &dead.job().node_id,
            &Context {
                kernel: KERNEL,
                process_started_ms: started,
                now_ms: dead.lease + 5_000,
                namespace_init: Some(NAMESPACE),
                storage_devices: Some(vec![STORAGE]),
            },
            &with(NAMESPACE, true, 0, 0),
        )
        .await,
        "census job unavailable",
    );
    // A census older than this daemon observed some other daemon.
    refused(
        dead.apply_in(
            &with(NAMESPACE, true, 0, 0),
            dead.lease + 2_000,
            Some(NAMESPACE),
        )
        .await,
        "census job unavailable",
    );
    // One pass without teardown evidence refuses.
    let mut partial = with(NAMESPACE, true, 0, 0);
    partial.passes[1].owner_teardown = None;
    refused(
        dead.apply_in(&partial, started, Some(NAMESPACE)).await,
        "census job unavailable",
    );
    // The owner's own record of the inode it opened still binds.
    let sidecar = dead
        .witnesses
        .join(format!("background-{}.source", dead.token.custody_id));
    private(
        &sidecar,
        format!("{} {}", dead.source.dev(), dead.source.ino() + 1).as_bytes(),
    );
    refused(
        dead.apply_in(&with(NAMESPACE, true, 0, 0), started, Some(NAMESPACE))
            .await,
        "not the inode the owner opened",
    );
    std::fs::remove_file(&sidecar).expect("remove sidecar");
    assert_eq!(dead.held(), 1, "every refusal keeps the custody");

    dead.apply_in(&with(NAMESPACE, true, 0, 0), started, Some(NAMESPACE))
        .await
        .expect("a proved teardown releases");
    assert_eq!(dead.held(), 0);
    assert!(!dead.anchor.exists(), "the worker clears its own witness");
}
