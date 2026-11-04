//! Protocol tests use injected boot identities. The Linux process fixture
//! separately proves that daemon death on the same real boot is insufficient.
use super::*;
use plurx_core::cluster::coordination::Lease;
use plurx_core::domain::{LibraryKind, NewLibrary};
use plurx_core::store::background_jobs::{
    ClaimJob, ClaimOutcome, EnqueueOutcome, JobKind, JobToken,
};
use plurx_core::store::background_jobs_library::{
    LibraryTrigger, LibraryWorkInput, NewLibraryWork,
};
use plurx_core::store::background_jobs_resources::{ForegroundSourceOwner, ForegroundSourcePolicy};

const NODE: &str = "recovery-node";
const OLD_BOOT: &str = "11111111-1111-4111-8111-111111111111";
const NEW_BOOT: &str = "22222222-2222-4222-8222-222222222222";

fn identity(boot_id: &str) -> Identity {
    Identity {
        machine_id: "a".repeat(32),
        boot_id: boot_id.into(),
    }
}

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    dir: PathBuf,
    store: Arc<dyn Store>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("recovery fixture directory");
        let database = root.path().join("plurx.db");
        let store = Arc::new(
            plurx_core::store::SqliteStore::open(&database).expect("recovery fixture store"),
        );
        let dir = root.path().join("source-custody-recovery/v1");
        Self {
            root,
            database,
            dir,
            store,
        }
    }

    fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.database).expect("fixture connection")
    }

    fn count(&self, table: &str) -> i64 {
        assert!(matches!(
            table,
            "background_source_custody" | "foreground_source_owners"
        ));
        self.connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("custody count")
    }

    fn background(&self, kind: &str) -> Record {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner, owner_kind, work_id, key_parts) = match kind {
            "job" => {
                let token = JobToken {
                    job_id: id.clone(),
                    node_id: NODE.into(),
                    boot_id: OLD_BOOT.into(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    fence: 3,
                    revision: 4,
                    lease_expires_ms: 1_000,
                };
                let key_parts = vec![
                    "job".into(),
                    id.clone(),
                    NODE.into(),
                    token.boot_id.clone(),
                    token.claim_id.clone(),
                    "3".into(),
                ];
                (BackgroundSourceOwner::Job { token }, "job", id, key_parts)
            }
            "analysis" => (
                BackgroundSourceOwner::Analysis {
                    request_id: id.clone(),
                    node_id: NODE.into(),
                    fence: 3,
                },
                "analysis",
                id.clone(),
                vec!["analysis".into(), id, NODE.into(), "3".into()],
            ),
            "lease" => {
                let lease = Lease {
                    resource: format!("subtitle-read-attest:{NODE}:0"),
                    owner_node_id: NODE.into(),
                    fence: 3,
                    revision: 4,
                    expires_at_unix_ms: 1_000,
                };
                let parts = vec![
                    "read_attestation".into(),
                    lease.resource.clone(),
                    NODE.into(),
                    "3".into(),
                    "12".into(),
                    "1234".into(),
                    "5678".into(),
                ];
                (
                    BackgroundSourceOwner::ReadAttestation {
                        lease: lease.clone(),
                        file_id: 12,
                        source_size: 1234,
                        source_mtime: 5678,
                    },
                    "lease",
                    lease.resource,
                    parts,
                )
            }
            "offline" => (
                BackgroundSourceOwner::Offline {
                    package_id: id.clone(),
                    node_id: NODE.into(),
                    claim_generation: 3,
                },
                "offline",
                id.clone(),
                vec!["offline".into(), id, NODE.into(), "3".into()],
            ),
            _ => panic!("unknown fixture owner"),
        };
        let token = BackgroundSourceCustodyToken {
            custody_id: uuid::Uuid::new_v4().to_string(),
            owner,
        };
        // Seed the persisted immutable owner key using the public fingerprint
        // encoding. No job/analysis/offline history exists to recover through.
        self.connection()
            .execute(
                "INSERT INTO background_source_custody
             (custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,
              phase,started_at_ms,updated_at_ms,lease_expires_at_ms,settlement_proof,activation_id)
             VALUES (?1,?2,?3,'[\"source_io\"]',?4,?5,3,'active',1,1,1000,NULL,?6)",
                rusqlite::params![
                    token.custody_id,
                    plurx_core::segplan::argv_fingerprint(&key_parts),
                    serde_json::to_string(
                        &serde_json::to_value(&token.owner).expect("owner value"),
                    )
                    .expect("canonical owner JSON"),
                    owner_kind,
                    work_id,
                    uuid::Uuid::new_v4().to_string(),
                ],
            )
            .expect("orphan background custody");
        Record::Background { token }
    }

    fn foreground(&self) -> Record {
        let token = ForegroundSourceToken {
            owner: ForegroundSourceOwner {
                owner_id: uuid::Uuid::new_v4().to_string(),
                lifetime_id: "lifetime".into(),
                session_incarnation: uuid::Uuid::new_v4().to_string(),
                producer_attempt: "producer".into(),
                node_id: NODE.into(),
                boot_id: OLD_BOOT.into(),
                file_id: 12,
                source_size: 1234,
                source_mtime: 5678,
                source_generation: "fixture-source-generation".into(),
                policy: ForegroundSourcePolicy::Rolling,
            },
            fence: 9,
            lease_expires_at_ms: 1_000,
        };
        self.connection()
            .execute(
                "INSERT INTO foreground_source_owners
             (owner_id,identity_json,resource_keys_json,fence,phase,projection_json,
              started_at_ms,updated_at_ms,lease_expires_at_ms,settlement_proof,activation_id)
             VALUES (?1,?2,'[\"source_io\"]',9,'startup',?3,1,1,1000,NULL,?4)",
                rusqlite::params![
                    token.owner.owner_id,
                    serde_json::to_string(
                        &serde_json::to_value(&token.owner).expect("owner value")
                    )
                    .expect("canonical owner JSON"),
                    serde_json::to_string(&ForegroundSourceProjection::startup(1))
                        .expect("projection"),
                    uuid::Uuid::new_v4().to_string(),
                ],
            )
            .expect("orphan foreground custody");
        Record::Foreground { token }
    }

    fn envelope(&self, records: Vec<Record>) -> Envelope {
        Envelope {
            version: 1,
            machine_id: identity(OLD_BOOT).machine_id,
            boot_id: OLD_BOOT.into(),
            node_id: NODE.into(),
            records,
        }
    }

    fn write(&self, envelope: &Envelope) -> PathBuf {
        private_directory(&self.dir).expect("private recovery directory");
        let path = self
            .dir
            .join(format!("legacy-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, serde_json::to_vec(envelope).expect("envelope JSON"))
            .expect("write recovery evidence");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("private evidence");
        }
        path
    }
}

#[tokio::test]
async fn reboot_recovery_settles_foreground_and_every_background_owner_without_history() {
    let fixture = Fixture::new();
    let mut records = vec![fixture.foreground()];
    for kind in ["job", "analysis", "lease", "offline"] {
        records.push(fixture.background(kind));
    }
    let unrelated = fixture.background("job");
    let history_count: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM background_jobs", [], |row| row.get(0))
        .expect("fixture has no original job history");
    assert_eq!(history_count, 0);
    let envelope = fixture.envelope(records);
    let path = fixture.write(&envelope);
    replay_at(&fixture.store, &fixture.dir, &identity(NEW_BOOT), NODE)
        .await
        .expect("verified reboot replay");
    assert!(!path.exists(), "acknowledged recovery evidence is removed");
    assert_eq!(fixture.count("foreground_source_owners"), 0);
    assert_eq!(fixture.count("background_source_custody"), 1);
    let Record::Background { token } = unrelated else {
        unreachable!()
    };
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT custody_id FROM background_source_custody",
                [],
                |row| row.get::<_, String>(0)
            )
            .expect("unrelated owner retained"),
        token.custody_id
    );
    replay_at(&fixture.store, &fixture.dir, &identity(NEW_BOOT), NODE)
        .await
        .expect("idempotent replay");
    assert_eq!(fixture.count("background_source_custody"), 1);
}

#[tokio::test]
async fn recovery_refuses_same_boot_other_machine_node_future_version_and_corrupt_evidence() {
    for case in [
        "same-boot",
        "other-machine",
        "other-node",
        "record-node",
        "future",
        "corrupt",
    ] {
        let fixture = Fixture::new();
        let record = fixture.background("job");
        let mut envelope = fixture.envelope(vec![record]);
        let mut current = identity(NEW_BOOT);
        match case {
            "same-boot" => current.boot_id = OLD_BOOT.into(),
            "other-machine" => current.machine_id = "b".repeat(32),
            "other-node" => envelope.node_id = "other-node".into(),
            "record-node" => {
                let Record::Background { token } = &mut envelope.records[0] else {
                    unreachable!()
                };
                let BackgroundSourceOwner::Job { token } = &mut token.owner else {
                    unreachable!()
                };
                token.node_id = "other-node".into();
            }
            "future" => envelope.version = 2,
            "corrupt" => {}
            _ => unreachable!(),
        }
        let path = fixture.write(&envelope);
        if case == "corrupt" {
            std::fs::write(&path, b"{invalid").expect("corrupt fixture");
        }
        replay_at(&fixture.store, &fixture.dir, &current, NODE)
            .await
            .expect("refusal is retained");
        assert_eq!(fixture.count("background_source_custody"), 1, "{case}");
        assert!(path.exists(), "{case}");
    }
}

#[tokio::test]
async fn recovery_old_identity_never_releases_replacement_custody() {
    let fixture = Fixture::new();
    let old_background = fixture.background("analysis");
    let old_foreground = fixture.foreground();
    let envelope = fixture.envelope(vec![old_background.clone(), old_foreground.clone()]);
    let Record::Background { token } = old_background else {
        unreachable!()
    };
    fixture.connection().execute(
        "UPDATE background_source_custody SET owner_key='replacement-immutable-owner' WHERE custody_id=?1",
        [token.custody_id],
    ).expect("replace background owner");
    let Record::Foreground { token } = old_foreground else {
        unreachable!()
    };
    let mut replacement = token.owner.clone();
    replacement.session_incarnation = uuid::Uuid::new_v4().to_string();
    fixture
        .connection()
        .execute(
            "UPDATE foreground_source_owners SET identity_json=?1 WHERE owner_id=?2",
            rusqlite::params![
                serde_json::to_string(
                    &serde_json::to_value(&replacement).expect("replacement owner value"),
                )
                .expect("canonical replacement owner JSON"),
                token.owner.owner_id
            ],
        )
        .expect("replace foreground owner");
    fixture.write(&envelope);
    replay_at(&fixture.store, &fixture.dir, &identity(NEW_BOOT), NODE)
        .await
        .expect("old owner no longer matches");
    assert_eq!(fixture.count("background_source_custody"), 1);
    assert_eq!(fixture.count("foreground_source_owners"), 1);
    let unsettled: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM background_source_custody WHERE settlement_proof IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("replacement proof");
    assert_eq!(unsettled, 1);
}

#[tokio::test]
async fn reboot_recovery_unblocks_an_already_accepted_manual_library_scan() {
    let fixture = Fixture::new();
    let library = fixture
        .store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![fixture.root.path().join("media")],
            anime: false,
        })
        .await
        .expect("library");
    let EnqueueOutcome::Accepted { job_id, .. } = fixture
        .store
        .enqueue_library_work(NewLibraryWork {
            request_id: uuid::Uuid::new_v4().to_string(),
            library_id: library.id,
            input: LibraryWorkInput::Full {
                refresh: false,
                trigger: LibraryTrigger::Manual,
            },
            now_ms: 10_000,
        })
        .await
        .expect("accepted scan")
    else {
        panic!("expected durable scan acceptance")
    };
    let record = fixture.background("job");
    let path = fixture.write(&fixture.envelope(vec![record]));
    let revision = fixture
        .store
        .background_job(&job_id)
        .await
        .expect("accepted row")
        .expect("durable job")
        .revision;
    let claim = || ClaimJob {
        job_id: job_id.clone(),
        expected_revision: revision,
        node_id: NODE.into(),
        boot_id: NEW_BOOT.into(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::LibraryScan,
        payload_version: 1,
        now_ms: 10_000,
        dispatched_at_ms: 10_000,
    };
    assert!(matches!(
        fixture
            .store
            .claim_job(claim())
            .await
            .expect("blocked claim"),
        ClaimOutcome::Contended
    ));
    replay_at(&fixture.store, &fixture.dir, &identity(NEW_BOOT), NODE)
        .await
        .expect("verified physical reboot replay");
    assert!(!path.exists());
    let ClaimOutcome::Claimed { job } = fixture.store.claim_job(claim()).await.expect("scan claim")
    else {
        panic!("accepted scan must run after abandoned physical custody settles")
    };
    assert_eq!(job.id, job_id);
    assert_eq!(job.fence, 1);
}

#[test]
fn active_recovery_preserves_prior_boot_and_refuses_conflicting_evidence() {
    let root = tempfile::tempdir().expect("journal fixture");
    let dir = root.path().join("source-custody-recovery/v1");
    let token = BackgroundSourceCustodyToken {
        custody_id: uuid::Uuid::new_v4().to_string(),
        owner: BackgroundSourceOwner::Analysis {
            request_id: uuid::Uuid::new_v4().to_string(),
            node_id: NODE.into(),
            fence: 3,
        },
    };
    let record = Record::Background { token };
    write_active_at(&dir, record.clone(), Some(identity(OLD_BOOT))).expect("active evidence");
    write_active_at(&dir, record.clone(), Some(identity(NEW_BOOT))).expect("idempotent activation");
    assert_eq!(
        read(&dir.join(record.name().expect("name")))
            .expect("read evidence")
            .boot_id,
        OLD_BOOT
    );
    let mut conflicting = record.clone();
    let Record::Background { token } = &mut conflicting else {
        unreachable!()
    };
    let BackgroundSourceOwner::Analysis { fence, .. } = &mut token.owner else {
        unreachable!()
    };
    *fence += 1;
    assert!(write_active_at(&dir, conflicting, Some(identity(NEW_BOOT))).is_err());
    assert_eq!(
        read(&dir.join(record.name().expect("name")))
            .expect("prior evidence")
            .boot_id,
        OLD_BOOT
    );
}

#[tokio::test]
async fn renewed_foreground_and_job_tokens_deduplicate_and_clear_exact_active_evidence() {
    let fixture = Fixture::new();
    for original in [fixture.foreground(), fixture.background("job")] {
        write_active_at(&fixture.dir, original.clone(), Some(identity(OLD_BOOT)))
            .expect("first activation evidence");
        let path = fixture.dir.join(original.name().expect("evidence name"));
        let mut renewed = original.clone();
        match &mut renewed {
            Record::Foreground { token } => {
                token.fence += 5;
                token.lease_expires_at_ms += 20_000;
            }
            Record::Background { token } => {
                let BackgroundSourceOwner::Job { token } = &mut token.owner else {
                    unreachable!()
                };
                token.revision += 5;
                token.lease_expires_ms += 20_000;
            }
        }
        write_active_at(&fixture.dir, renewed.clone(), Some(identity(NEW_BOOT)))
            .expect("renewal retains exact physical owner");
        assert_eq!(read(&path).expect("prior boot evidence").boot_id, OLD_BOOT);
        clear_at(&fixture.dir, &renewed).expect("normal joined owner clears its active evidence");
        assert!(!path.exists());
        clear_at(&fixture.dir, &renewed).expect("acknowledged cleanup is idempotent");
    }
}

#[tokio::test]
async fn replacement_foreground_incarnation_or_job_claim_cannot_clear_active_evidence() {
    let fixture = Fixture::new();
    for original in [fixture.foreground(), fixture.background("job")] {
        write_active_at(&fixture.dir, original.clone(), Some(identity(OLD_BOOT)))
            .expect("original activation evidence");
        let path = fixture.dir.join(original.name().expect("evidence name"));
        let mut replacement = original.clone();
        match &mut replacement {
            Record::Foreground { token } => {
                token.owner.session_incarnation = uuid::Uuid::new_v4().to_string();
            }
            Record::Background { token } => {
                let BackgroundSourceOwner::Job { token } = &mut token.owner else {
                    unreachable!()
                };
                token.claim_id = uuid::Uuid::new_v4().to_string();
                token.fence += 1;
            }
        }
        assert!(
            write_active_at(&fixture.dir, replacement.clone(), Some(identity(NEW_BOOT))).is_err()
        );
        assert!(clear_at(&fixture.dir, &replacement).is_err());
        assert_eq!(read(&path).expect("original retained").boot_id, OLD_BOOT);
        clear_at(&fixture.dir, &original).expect("original exact owner may clear its own evidence");
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn missing_physical_identity_and_oversized_or_excessive_evidence_retain_custody() {
    for case in ["identity", "oversized", "records"] {
        let fixture = Fixture::new();
        let record = fixture.background("job");
        let path = match case {
            "identity" => {
                write_active_at(&fixture.dir, record.clone(), None)
                    .expect("unsupported identity is retained");
                fixture.dir.join(record.name().expect("name"))
            }
            "oversized" => {
                let path = fixture.write(&fixture.envelope(vec![record]));
                std::fs::write(&path, vec![b' '; MAX_BYTES as usize + 1])
                    .expect("oversized evidence");
                path
            }
            "records" => fixture.write(&fixture.envelope(vec![record; MAX_RECORDS + 1])),
            _ => unreachable!(),
        };
        replay_at(&fixture.store, &fixture.dir, &identity(NEW_BOOT), NODE)
            .await
            .expect("unproved evidence retained");
        assert!(path.exists(), "{case}");
        assert_eq!(fixture.count("background_source_custody"), 1, "{case}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_recovery_evidence_never_releases_custody() {
    let fixture = Fixture::new();
    let record = fixture.background("analysis");
    let path = fixture.write(&fixture.envelope(vec![record]));
    let target = fixture.root.path().join("outside-evidence");
    std::fs::rename(&path, &target).expect("move real evidence");
    std::os::unix::fs::symlink(&target, &path).expect("symlink fixture");
    replay_at(&fixture.store, &fixture.dir, &identity(NEW_BOOT), NODE)
        .await
        .expect("untrusted evidence retained");
    assert_eq!(fixture.count("background_source_custody"), 1);
    assert!(path
        .symlink_metadata()
        .expect("retained link")
        .file_type()
        .is_symlink());
    assert!(target.exists());
}

#[cfg(target_os = "linux")]
const PROCESS_FIXTURE: &str = "source_custody::recovery::tests::recovery_process_fixture";

#[cfg(target_os = "linux")]
#[test]
fn dead_daemon_with_live_reader_cannot_release_same_real_boot_custody() {
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", PROCESS_FIXTURE, "--nocapture"])
        .env("PLURX_RECOVERY_PROCESS_ROLE", "supervisor")
        .output()
        .expect("isolated real-process supervisor");
    assert!(
        output.status.success(),
        "supervisor failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
struct OwnedProcess {
    child: std::process::Child,
    joined: bool,
}
#[cfg(target_os = "linux")]
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.joined {
            return;
        }
        let group = i32::try_from(self.child.id()).expect("fixture group fits pid_t");
        // The daemon's unreaped PID reserves this fresh group, including on a
        // failure before the reader identity/cleanup guard was constructed.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = self.child.wait();
        // Subreaper adoption makes the deliberately orphaned group children
        // ours to reap after killing the daemon and every fixture descendant.
        unsafe {
            loop {
                if libc::waitpid(-group, std::ptr::null_mut(), 0) == -1
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                {
                    break;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
struct AdoptedReader(libc::pid_t);
#[cfg(target_os = "linux")]
impl Drop for AdoptedReader {
    fn drop(&mut self) {
        // The isolated supervisor is a subreaper. Once its daemon child was
        // joined, this exact descendant is its unreaped child, so its PID
        // cannot be reused before this kill/wait pair.
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
            loop {
                if libc::waitpid(self.0, std::ptr::null_mut(), 0) != -1
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                {
                    break;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_process_fixture() {
    use std::io::{Read as _, Seek as _};
    use std::os::unix::process::CommandExt as _;
    use std::time::{Duration, Instant};
    let Ok(role) = std::env::var("PLURX_RECOVERY_PROCESS_ROLE") else {
        return;
    };
    if role == "reader" {
        let root = PathBuf::from(std::env::var_os("PLURX_RECOVERY_PROCESS_ROOT").expect("root"));
        let mut source = std::fs::File::open(root.join("source.bin")).expect("held source");
        loop {
            source.read_exact(&mut [0_u8; 1]).expect("source read");
            source.rewind().expect("source rewind");
            std::fs::write(root.join("input-observed"), b"reading").expect("source marker");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if role == "daemon" {
        let root = PathBuf::from(std::env::var_os("PLURX_RECOVERY_PROCESS_ROOT").expect("root"));
        let envelope = read(&root.join("prepared.json")).expect("prepared exact owner");
        write_active_at(
            &root.join("source-custody-recovery/v1"),
            envelope.records[0].clone(),
            Some(Identity {
                machine_id: envelope.machine_id,
                boot_id: envelope.boot_id,
            }),
        )
        .expect("durable active evidence before input");
        // Deliberately bypass the production parent-death launcher in this
        // hostile test fixture, proving that an escaped descendant prevents
        // recovery from using mere daemon death as a physical-stop proof.
        let reader = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", PROCESS_FIXTURE, "--nocapture"])
            .env("PLURX_RECOVERY_PROCESS_ROLE", "reader")
            .env("PLURX_RECOVERY_PROCESS_ROOT", &root)
            .spawn()
            .expect("held descendant");
        std::fs::write(root.join("reader.pid.tmp"), reader.id().to_string()).expect("reader PID");
        std::fs::rename(root.join("reader.pid.tmp"), root.join("reader.pid"))
            .expect("publish complete reader identity");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    assert_eq!(role, "supervisor");
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) },
        0,
        "isolate child ownership"
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let fixture = Fixture::new();
            let record = fixture.background("job");
            let mut real_identity = identity(OLD_BOOT);
            real_identity.boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .expect("actual kernel boot UUID")
                .trim()
                .to_owned();
            let envelope = Envelope {
                version: 1,
                machine_id: real_identity.machine_id.clone(),
                boot_id: real_identity.boot_id.clone(),
                node_id: NODE.into(),
                records: vec![record.clone()],
            };
            std::fs::write(
                fixture.root.path().join("prepared.json"),
                serde_json::to_vec(&envelope).expect("prepared"),
            )
            .expect("prepare child ownership");
            std::fs::write(fixture.root.path().join("source.bin"), [42_u8; 4096])
                .expect("source fixture");
            let daemon =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", PROCESS_FIXTURE, "--nocapture"])
                    .env("PLURX_RECOVERY_PROCESS_ROLE", "daemon")
                    .env("PLURX_RECOVERY_PROCESS_ROOT", fixture.root.path())
                    .process_group(0)
                    .spawn()
                    .expect("daemon fixture");
            let mut daemon = OwnedProcess {
                child: daemon,
                joined: false,
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while !fixture.root.path().join("input-observed").exists()
                || !fixture.root.path().join("reader.pid").exists()
            {
                assert!(Instant::now() < deadline, "reader did not begin input");
                std::thread::sleep(Duration::from_millis(10));
            }
            let reader_pid: libc::pid_t =
                std::fs::read_to_string(fixture.root.path().join("reader.pid"))
                    .expect("reader identity")
                    .parse()
                    .expect("reader PID");
            daemon.child.kill().expect("kill daemon before settlement");
            daemon.child.wait().expect("join daemon");
            daemon.joined = true;
            let reader = AdoptedReader(reader_pid);
            assert_eq!(
                unsafe { libc::kill(reader.0, 0) },
                0,
                "reader survived daemon death"
            );
            replay_at(&fixture.store, &fixture.dir, &real_identity, NODE)
                .await
                .expect("same-boot refusal");
            assert_eq!(fixture.count("background_source_custody"), 1);
            assert!(fixture.dir.join(record.name().expect("name")).exists());
            drop(reader);
        });
}
