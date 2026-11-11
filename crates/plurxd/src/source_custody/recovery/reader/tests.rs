//! Actual subprocess/kernel locks exercise the same code on native Mac/Linux.
use super::*;
use crate::source_custody::recovery::tests::{Fixture, NODE};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const FIXTURE: &str = "source_custody::recovery::reader::tests::reader_process_fixture";
const CONFIG: &str = "PLURX_TEST_READER_CONFIG";
static REJECT_NEXT_SPAWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn observe_retry(mediated: bool) -> std::io::Result<()> {
    if REJECT_NEXT_SPAWN.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    observe_children(mediated)
}

#[derive(Serialize, Deserialize)]
struct Configuration {
    directory: PathBuf,
    source: PathBuf,
    record: Record,
    activation_id: String,
    mode: String,
}

struct OwnedChild {
    child: Child,
    output: Receiver<String>,
}
impl OwnedChild {
    #[allow(clippy::disallowed_methods)] // Isolated test supervisor owns/reaps its exact child.
    fn start(path: &Path) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", FIXTURE, "--nocapture"])
            .env(CONFIG, path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("real reader supervisor");
        let stdout = child.stdout.take().expect("reader stdout");
        let stderr = child.stderr.take().expect("reader stderr");
        let (sender, output) = mpsc::channel();
        let errors = sender.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().take(128) {
                if errors
                    .send(format!("stderr: {}", line.expect("reader stderr line")))
                    .is_err()
                {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().take(128) {
                if sender.send(line.expect("reader protocol line")).is_err() {
                    break;
                }
            }
        });
        Self { child, output }
    }
    fn wait_for(&self, expected: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut observed = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let line = self.output.recv_timeout(remaining).unwrap_or_else(|error| {
                panic!("reader protocol {expected} failed: {error}; observed={observed:?}")
            });
            if line.contains(expected) {
                break;
            }
            observed.push(line);
        }
    }
    fn send(&mut self, value: &str) {
        let input = self.child.stdin.as_mut().expect("reader input");
        writeln!(input, "{value}")
            .and_then(|()| input.flush())
            .expect("reader control");
    }
    fn exit(&mut self) {
        self.send("exit");
        assert!(self.child.wait().expect("reader process exit").success());
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn count(fixture: &Fixture, record: &Record) -> i64 {
    let table = match record {
        Record::Foreground { .. } => "foreground_source_owners",
        _ => "background_source_custody",
    };
    fixture
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("exact family count")
}

fn preread(fixture: &Fixture, record: &Record) {
    match record {
        Record::Foreground { token } => {
            fixture.connection().execute("UPDATE foreground_source_owners SET settlement_proof='no_reader',activation_id=NULL WHERE owner_id=?1", [&token.owner.owner_id]).expect("foreground pre-read fixture");
        }
        Record::Background { token } => {
            fixture.connection().execute("UPDATE background_source_custody SET settlement_proof='no_reader',activation_id=NULL WHERE custody_id=?1", [&token.custody_id]).expect("background pre-read fixture");
        }
    }
}

async fn activate(fixture: &Fixture, record: &Record, nonce: &str) -> bool {
    match record {
        Record::Foreground { token } => fixture
            .store
            .activate_foreground_source_custody(token.clone(), nonce.into())
            .await
            .expect("foreground CAS"),
        Record::Background { token } => fixture
            .store
            .activate_background_source_custody(token.clone(), nonce.into())
            .await
            .expect("background CAS"),
    }
}

fn configuration(fixture: &Fixture, record: &Record, mode: &str) -> (PathBuf, Configuration) {
    let source = fixture.root.path().join("physical-source.bin");
    std::fs::write(&source, vec![42u8; 4096]).expect("real physical source");
    let config = Configuration {
        directory: fixture.dir.with_file_name("v2"),
        source,
        record: record.clone(),
        activation_id: uuid::Uuid::new_v4().to_string(),
        mode: mode.into(),
    };
    let path = fixture.root.path().join("reader-config.json");
    std::fs::write(&path, serde_json::to_vec(&config).expect("fixture config"))
        .expect("private config");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("private config mode");
    (path, config)
}

#[tokio::test]
async fn mediated_reader_crash_recovers_same_kernel_on_native_mac_and_linux() {
    assert!(kernel_instance().is_some(), "real platform kernel provider");
    for family in ["foreground", "job", "analysis", "lease", "offline"] {
        let fixture = Fixture::new();
        configure_backend(&fixture.store, fixture.root.path(), true)
            .expect("authoritative context");
        let record = if family == "foreground" {
            fixture.foreground()
        } else {
            fixture.background(family)
        };
        preread(&fixture, &record);
        // Empty legacy host identity must not be guessed or rewritten.
        write_active_at(&fixture.dir, record.clone(), None).expect("legacy evidence");
        let (path, config) = configuration(&fixture, &record, "mediated");
        let mut child = OwnedChild::start(&path);
        child.wait_for("READER_PREPARED");
        assert!(activate(&fixture, &record, &config.activation_id).await);
        child.send("arm");
        child.wait_for("READER_ARMED");
        replay(&fixture.store, NODE)
            .await
            .expect("live reader observation");
        assert_eq!(
            count(&fixture, &record),
            1,
            "{family}: live lock remains charged"
        );
        child.send("cancel");
        child.wait_for("REQUEST_CANCELLED");
        replay(&fixture.store, NODE)
            .await
            .expect("cancelled future is not a join");
        assert_eq!(
            count(&fixture, &record),
            1,
            "{family}: registry retains actual lifetime"
        );
        child.exit();
        replay(&fixture.store, NODE)
            .await
            .expect("kernel-confirmed lifetime end");
        assert_eq!(
            count(&fixture, &record),
            0,
            "{family}: exact old owner recovered"
        );
        assert!(!config.directory.join(record.name().expect("name")).exists());
        replay(&fixture.store, NODE)
            .await
            .expect("idempotent retry");
    }
}

#[tokio::test]
async fn brokered_cache_parts_keep_reader_recovery_but_raw_fallbacks_remain_fenced() {
    for mode in [
        "brokered_cache",
        "cache_input_only",
        "cache_output_only",
        "cache_raw_subtitle",
        "cache_embedded_subtitle",
    ] {
        let fixture = Fixture::new();
        configure_backend(&fixture.store, fixture.root.path(), true)
            .expect("authoritative context");
        let record = fixture.background("job");
        preread(&fixture, &record);
        write_active_at(&fixture.dir, record.clone(), None).expect("legacy receipt");
        let (path, config) = configuration(&fixture, &record, mode);
        let mut child = OwnedChild::start(&path);
        child.wait_for("READER_PREPARED");
        assert!(activate(&fixture, &record, &config.activation_id).await);
        child.send("arm");
        child.wait_for("READER_ARMED");
        replay(&fixture.store, NODE)
            .await
            .expect("live reader remains owned");
        assert_eq!(
            count(&fixture, &record),
            1,
            "{mode}: live daemon lock is required"
        );
        child.send("cancel");
        child.wait_for("REQUEST_CANCELLED");
        replay(&fixture.store, NODE)
            .await
            .expect("cancellation is not a join");
        assert_eq!(
            count(&fixture, &record),
            1,
            "{mode}: cancellation cannot erase custody"
        );
        child.exit();
        replay(&fixture.store, NODE)
            .await
            .expect("replay exact lifetime witness");
        assert_eq!(
            count(&fixture, &record),
            if mode == "brokered_cache" { 0 } else { 1 },
            "{mode}: only the real HTTP input/output child retains mediated proof"
        );
        assert_eq!(
            config
                .directory
                .join(record.name().expect("record name"))
                .exists(),
            mode != "brokered_cache"
        );
    }
}

#[tokio::test]
async fn reader_recovery_refuses_unarmed_unmediated_foreign_kernel_and_replaced_inode() {
    for mode in [
        "unarmed",
        "unmediated",
        "declared_raw",
        "prepared_descriptor",
        "retry_descriptor",
        "foreign_kernel",
        "replaced_inode",
    ] {
        eprintln!("reader refusal case={mode}");
        let fixture = Fixture::new();
        let record = fixture.background("job");
        preread(&fixture, &record);
        let (path, config) = configuration(
            &fixture,
            &record,
            if matches!(
                mode,
                "unmediated" | "declared_raw" | "prepared_descriptor" | "retry_descriptor"
            ) {
                mode
            } else {
                "mediated"
            },
        );
        let mut child = OwnedChild::start(&path);
        child.wait_for("READER_PREPARED");
        if mode != "unarmed" {
            assert!(activate(&fixture, &record, &config.activation_id).await);
            child.send("arm");
            child.wait_for("READER_ARMED");
        }
        child.exit();
        let name = record.name().expect("name");
        if mode == "foreign_kernel" {
            let evidence_path = config.directory.join(&name);
            let mut evidence: Evidence = read_private_json(&evidence_path).expect("proof");
            evidence.kernel_instance = format!("other:{}", uuid::Uuid::new_v4());
            std::fs::write(
                &evidence_path,
                serde_json::to_vec(&evidence).expect("foreign proof"),
            )
            .expect("copied locking domain fixture");
        } else if mode == "replaced_inode" {
            let anchor = config.directory.join(name.replace(".json", ".anchor"));
            let nonce = std::fs::read(&anchor).expect("anchor nonce");
            std::fs::rename(&anchor, anchor.with_extension("preserved"))
                .expect("preserve old inode");
            std::fs::write(&anchor, nonce).expect("same bytes different kernel inode");
            std::fs::set_permissions(anchor, std::fs::Permissions::from_mode(0o600))
                .expect("private replacement");
        }
        replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
            .await
            .expect("unproved reader stays held");
        assert_eq!(count(&fixture, &record), 1, "{mode}");
        assert!(
            config.directory.join(name).exists(),
            "{mode}: evidence preserved"
        );
    }
}

#[tokio::test]
async fn reader_recovery_refuses_original_bytes_restored_in_place_after_raw_launch() {
    let fixture = Fixture::new();
    let record = fixture.background("job");
    preread(&fixture, &record);
    let (path, config) = configuration(&fixture, &record, "unmediated");
    let mut child = OwnedChild::start(&path);
    child.wait_for("READER_PREPARED");
    let proof = config.directory.join(record.name().expect("name"));
    let anchor = proof.with_extension("anchor");
    let original = std::fs::read(&anchor).expect("backup before raw launch");
    let original_stat = anchor.metadata().expect("original anchor identity");
    assert!(activate(&fixture, &record, &config.activation_id).await);
    child.send("arm");
    child.wait_for("READER_ARMED");
    assert_ne!(
        std::fs::read(&anchor).expect("invalidated anchor"),
        original
    );
    child.exit();

    let mut restore = std::fs::OpenOptions::new()
        .write(true)
        .open(&anchor)
        .expect("existing anchor");
    restore
        .write_all(&original)
        .expect("restore original bytes in place");
    restore.sync_all().expect("durable restore");
    let restored = restore.metadata().expect("restored anchor identity");
    assert_eq!(
        (restored.dev(), restored.ino()),
        (original_stat.dev(), original_stat.ino())
    );
    assert_eq!(std::fs::read(&anchor).expect("restored bytes"), original);
    replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
        .await
        .expect("replay observations");
    assert_eq!(
        count(&fixture, &record),
        1,
        "restored raw-owner proof cannot release custody"
    );
    assert!(proof.exists());
}

#[tokio::test]
async fn reader_inventory_admits_and_replays_by_owner_count_among_sidecars() {
    let fixture = Fixture::new();
    let record = fixture.background("job");
    preread(&fixture, &record);
    let (path, config) = configuration(&fixture, &record, "mediated");
    std::fs::create_dir_all(&config.directory).expect("fixture inventory");
    for n in 0..MAX_FILES {
        std::fs::File::create(config.directory.join(format!("orphan-{n}.anchor")))
            .expect("retained sidecar");
    }
    let mut child = OwnedChild::start(&path);
    child.wait_for("READER_PREPARED");
    assert!(activate(&fixture, &record, &config.activation_id).await);
    child.send("arm");
    child.wait_for("READER_ARMED");
    child.exit();
    replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
        .await
        .expect("replay beyond a single sidecar per owner");
    assert_eq!(
        count(&fixture, &record),
        0,
        "release the proven exact owner"
    );
    assert!(!config.directory.join(record.name().expect("name")).exists());

    // Fill all logical slots without changing the total number of entries.
    for n in 0..MAX_FILES {
        let old = config.directory.join(format!("orphan-{n}.anchor"));
        std::fs::rename(&old, old.with_extension("json")).expect("occupied owner slot");
    }
    let other = fixture.background("analysis");
    preread(&fixture, &other);
    let candidate = config.directory.join(other.name().expect("other name"));
    let result = prepare_at(
        &config.directory,
        other.clone(),
        &uuid::Uuid::new_v4().to_string(),
    );
    assert!(matches!(result, Err(error) if error.contains("no capacity")));
    assert!(!candidate.exists());
    assert!(!candidate.with_extension("anchor").exists());
    assert_eq!(count(&fixture, &other), 1);
}

#[tokio::test]
async fn reader_release_failure_keeps_exact_proof_for_retry() {
    let fixture = Fixture::new();
    let record = fixture.background("analysis");
    preread(&fixture, &record);
    let (path, config) = configuration(&fixture, &record, "mediated");
    let mut child = OwnedChild::start(&path);
    child.wait_for("READER_PREPARED");
    assert!(activate(&fixture, &record, &config.activation_id).await);
    child.send("arm");
    child.wait_for("READER_ARMED");
    child.exit();
    fixture.connection().execute_batch("CREATE TRIGGER reject_reader_release BEFORE DELETE ON background_source_custody BEGIN SELECT RAISE(FAIL,'injected release acknowledgement failure'); END;").expect("inject real Store release failure");
    assert!(
        replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
            .await
            .is_err()
    );
    assert_eq!(count(&fixture, &record), 1);
    assert!(config.directory.join(record.name().expect("name")).exists());
    fixture
        .connection()
        .execute_batch("DROP TRIGGER reject_reader_release;")
        .expect("restore Store");
    replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
        .await
        .expect("retry acknowledged exact owner");
    assert_eq!(count(&fixture, &record), 0);
}

#[tokio::test]
async fn active_owner_rejection_preserves_original_activation_and_unarmed_candidate() {
    let fixture = Fixture::new();
    let record = fixture.background("job");
    let (path, config) = configuration(&fixture, &record, "refuse_existing");
    let previous_activation: String = fixture
        .connection()
        .query_row(
            "SELECT activation_id FROM background_source_custody",
            [],
            |row| row.get(0),
        )
        .expect("old activation nonce");
    let mut child = OwnedChild::start(&path);
    child.wait_for("READER_REFUSED");
    child.exit();
    replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
        .await
        .expect("unarmed candidate cannot settle old active owner");
    assert_eq!(count(&fixture, &record), 1);
    let activation: Option<String> = fixture
        .connection()
        .query_row(
            "SELECT activation_id FROM background_source_custody",
            [],
            |row| row.get(0),
        )
        .expect("old activation");
    assert_eq!(activation.as_deref(), Some(previous_activation.as_str()));
    let evidence_path = config
        .directory
        .join(record.name().expect("candidate name"));
    let candidate: Evidence =
        read_private_json(&evidence_path).expect("pre-read candidate was actually created");
    assert_ne!(candidate.activation_id, previous_activation);
    assert!(
        !evidence_path.with_extension("armed").exists(),
        "failed Store CAS must never arm the candidate"
    );
}

#[tokio::test]
async fn mediated_packet_probe_withholds_original_source_descriptor() {
    let fixture = Fixture::new();
    let record = fixture.background("job");
    preread(&fixture, &record);
    let (path, config) = configuration(&fixture, &record, "packet");
    let mut child = OwnedChild::start(&path);
    child.wait_for("READER_PREPARED");
    assert!(activate(&fixture, &record, &config.activation_id).await);
    child.send("arm");
    child.wait_for("READER_ARMED");
    child.exit();
    replay_at(&fixture.store, &config.directory, &fixture.dir, NODE)
        .await
        .expect("URL-only packet helper retained reader proof");
    assert_eq!(count(&fixture, &record), 0);
}

#[test]
fn reader_process_fixture() {
    if std::env::var("PLURX_TEST_READER_CONSUMER").is_ok() {
        let expected_device: u64 = std::env::var("PLURX_TEST_SOURCE_DEVICE")
            .expect("device")
            .parse()
            .expect("device number");
        let expected_inode: u64 = std::env::var("PLURX_TEST_SOURCE_INODE")
            .expect("inode")
            .parse()
            .expect("inode number");
        for descriptor in 0..128 {
            let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: fstat neither owns nor closes the descriptor; success initializes info.
            if unsafe { libc::fstat(descriptor, info.as_mut_ptr()) } == 0 {
                let info = unsafe { info.assume_init() };
                #[cfg(target_os = "linux")]
                let actual_device = info.st_dev;
                #[cfg(target_os = "macos")]
                let actual_device = info.st_dev as u64;
                assert!(
                    actual_device != expected_device || info.st_ino != expected_inode,
                    "original source descriptor escaped into mediated child"
                );
            }
        }
        let address = std::env::var("PLURX_TEST_READER_ADDRESS").expect("HTTP address");
        let mut stream = std::net::TcpStream::connect(address).expect("mediated source connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bounded HTTP read");
        stream
            .write_all(b"GET /source HTTP/1.0\r\n\r\n")
            .expect("HTTP request");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).expect("source response");
        assert!(bytes.ends_with(&vec![42; 4096]));
        if let Ok(address) = std::env::var("PLURX_TEST_READER_OUTPUT_ADDRESS") {
            assert!(
                std::env::var(CONFIG).unwrap_or_default().is_empty(),
                "private original-source configuration cannot escape to the brokered child"
            );
            let mut stream =
                std::net::TcpStream::connect(address).expect("mediated output connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bounded PUT response");
            stream
                .write_all(b"PUT /output HTTP/1.0\r\nContent-Length: 4096\r\n\r\n")
                .expect("output headers");
            stream
                .write_all(&vec![42u8; 4096])
                .expect("brokered output bytes");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("output acknowledged");
            assert!(response.starts_with(b"HTTP/1.0 200 OK"));
        }
        return;
    }
    let Ok(path) = std::env::var(CONFIG) else {
        return;
    };
    let config: Configuration =
        serde_json::from_slice(&std::fs::read(path).expect("private fixture config"))
            .expect("reader configuration");
    if config.mode == "refuse_existing" {
        let root = config
            .directory
            .parent()
            .expect("recovery root")
            .parent()
            .expect("data root");
        let store: Arc<dyn Store> = Arc::new(
            plurx_core::store::SqliteStore::open(&root.join("plurx.db"))
                .expect("old authoritative state"),
        );
        configure_backend(&store, root, true).expect("exact Store context");
        crate::source_custody::configure_settlement_journal(root);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("activation runtime");
        let Record::Background { token } = config.record.clone() else {
            panic!("background fixture");
        };
        assert!(runtime
            .block_on(crate::source_custody::activate_background_with_journal(
                &store, token
            ))
            .is_err());
        println!("READER_REFUSED");
        std::io::stdout().flush().expect("protocol flush");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).expect("exit control");
        assert_eq!(line.trim(), "exit");
        return;
    }
    plurx_core::process::source_lifetime::install_observer(if config.mode == "retry_descriptor" {
        observe_retry
    } else {
        observe_children
    })
    .expect("common child observer");
    let mut prepared = if matches!(
        config.mode.as_str(),
        "prepared_descriptor" | "retry_descriptor"
    ) {
        let source =
            std::fs::File::open(&config.source).expect("preconstructed original capability");
        let mut command = tokio::process::Command::new("/bin/cat");
        command.arg("/dev/fd/3");
        crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source, 3)]);
        Some((command, source))
    } else {
        None
    };
    let witness = prepare_at(
        &config.directory,
        config.record.clone(),
        &config.activation_id,
    )
    .expect("pre-read witness")
    .expect("supported local filesystem");
    println!("READER_PREPARED");
    std::io::stdout().flush().expect("protocol flush");
    let mut witness = Some(witness);
    let mut input = BufReader::new(std::io::stdin());
    loop {
        let mut line = String::new();
        assert!(input.read_line(&mut line).expect("reader command") != 0);
        match line.trim() {
            "arm" => {
                witness
                    .as_ref()
                    .expect("live witness")
                    .arm(&config.activation_id)
                    .expect("Store CAS acknowledged nonce");
                if config.mode == "mediated" {
                    run_mediated_child(&config.source);
                }
                if config.mode == "packet" {
                    run_packet_probe(&config);
                }
                if config.mode == "brokered_cache" {
                    run_mediated_child_inner(&config.source, true);
                }
                if config.mode.starts_with("cache_") {
                    run_raw_cache_child(&config.source, &config.mode);
                }
                if config.mode == "mediated" {
                    // A cancelled request can leave a detached read actor with
                    // an original capability. Its process still owns the lock.
                    let source = config.source.clone();
                    let (ready, opened) = mpsc::channel();
                    std::thread::spawn(move || {
                        let mut file =
                            std::fs::File::open(source).expect("detached physical reader");
                        ready.send(()).expect("reader started");
                        std::thread::park();
                        let mut byte = [0];
                        let _ = file.read(&mut byte);
                    });
                    opened
                        .recv_timeout(Duration::from_secs(5))
                        .expect("real detached reader owns descriptor");
                }
                if let Some((mut command, _source)) = prepared.take() {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("prepared command runtime");
                    if config.mode == "retry_descriptor" {
                        REJECT_NEXT_SPAWN.store(true, std::sync::atomic::Ordering::SeqCst);
                        let error = runtime
                            .block_on(plurx_core::process::source_lifetime::mediated(
                                plurx_core::process::bounded::output_command_clean_env(
                                    &mut command,
                                    Duration::from_secs(5),
                                    8192,
                                    plurx_core::process::ChildWork::background(
                                        "refused prepared descriptor fixture",
                                    ),
                                ),
                            ))
                            .expect_err("first admission is explicitly refused");
                        assert_eq!(error.raw_os_error(), Some(libc::EIO));
                        assert!(
                            plurx_core::process::source_lifetime::has_descriptor_handoff(
                                command.as_std()
                            ),
                            "a reusable command retains raw-FD classification after refusal"
                        );
                    }
                    let output = runtime
                        .block_on(plurx_core::process::source_lifetime::mediated(
                            plurx_core::process::bounded::output_command_clean_env(
                                &mut command,
                                Duration::from_secs(5),
                                8192,
                                plurx_core::process::ChildWork::background(
                                    "prepared raw descriptor fixture",
                                ),
                            ),
                        ))
                        .expect("prepared descriptor command");
                    assert!(output.status.success());
                    assert_eq!(output.stdout, vec![42; 4096]);
                    assert!(
                        plurx_core::process::source_lifetime::has_descriptor_handoff(
                            command.as_std()
                        ),
                        "classification survives a successful reusable spawn too"
                    );
                }
                if config.mode == "declared_raw" {
                    let Record::Background { token } = &config.record else {
                        panic!("declared raw background owner");
                    };
                    let output = declared_background_spawn(token, || {
                        plurx_core::process::output_job_owned_blocking(
                            std::process::Command::new("/bin/cat").arg(&config.source),
                            plurx_core::process::ChildWork::background(
                                "declared raw crash fixture",
                            ),
                        )
                    })
                    .expect("actual declared raw helper");
                    assert!(output.status.success());
                    assert_eq!(output.stdout, vec![42; 4096]);
                }
                if config.mode == "unmediated" {
                    let output = plurx_core::process::output_job_owned_blocking(
                        std::process::Command::new("/bin/cat").arg(&config.source),
                        plurx_core::process::ChildWork::background("unmediated source fixture"),
                    )
                    .expect("actual unmediated helper");
                    assert!(output.status.success());
                    assert_eq!(output.stdout, vec![42; 4096]);
                }
                println!("READER_ARMED");
            }
            "cancel" => {
                drop(witness.take());
                println!("REQUEST_CANCELLED");
            }
            "exit" => std::process::exit(0),
            _ => panic!("unknown reader command"),
        }
        std::io::stdout().flush().expect("protocol flush");
    }
}

fn run_mediated_child(source: &Path) {
    run_mediated_child_inner(source, false);
}

fn run_mediated_child_inner(source: &Path, cache_part: bool) {
    let mut file = std::fs::File::open(source).expect("daemon original descriptor");
    let metadata = file.metadata().expect("physical identity");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("daemon source bridge");
    let address = listener.local_addr().expect("bridge address");
    let output_listener =
        cache_part.then(|| std::net::TcpListener::bind("127.0.0.1:0").expect("output broker"));
    let output_address = output_listener
        .as_ref()
        .map(|listener| listener.local_addr().expect("output address"));
    let sink = output_listener.map(|listener| {
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("output consumer");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bounded output request");
            let mut bytes = Vec::new();
            let mut block = [0u8; 8192];
            let header_end = loop {
                let n = stream.read(&mut block).expect("output request");
                assert!(n > 0 && bytes.len() + n <= 16384, "bounded PUT request");
                bytes.extend_from_slice(&block[..n]);
                if let Some(at) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            while bytes.len() < header_end + 4096 {
                let n = stream.read(&mut block).expect("output body");
                assert!(n > 0 && bytes.len() + n <= header_end + 4096);
                bytes.extend_from_slice(&block[..n]);
            }
            assert!(bytes.starts_with(b"PUT /output HTTP/1.0\r\n"));
            assert_eq!(&bytes[header_end..], vec![42u8; 4096].as_slice());
            stream
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n")
                .expect("output response");
        })
    });
    let bridge = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("source consumer");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bounded request");
        let mut request = [0u8; 4096];
        assert!(stream.read(&mut request).expect("request") > 0);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .expect("actual daemon file read");
        write!(
            stream,
            "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n",
            bytes.len()
        )
        .expect("response");
        stream.write_all(&bytes).expect("daemon-mediated bytes");
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    let output = if cache_part {
        use std::os::fd::AsRawFd;
        let original = std::fs::File::open(source).expect("held source capability");
        let directory = std::fs::File::open(source.parent().expect("source parent"))
            .expect("held output capability");
        let executable = std::env::current_exe().expect("consumer executable");
        let source_device = metadata.dev().to_string();
        let source_inode = metadata.ino().to_string();
        let address_string = address.to_string();
        let output_string = output_address.expect("real output broker").to_string();
        let input_url = format!("http://{address}/source");
        let output_url = format!("http://{output_string}/output");
        let env = [
            ("PLURX_TEST_READER_CONSUMER", std::ffi::OsStr::new("1")),
            (CONFIG, std::ffi::OsStr::new("")),
            (
                "PLURX_TEST_SOURCE_DEVICE",
                std::ffi::OsStr::new(&source_device),
            ),
            (
                "PLURX_TEST_SOURCE_INODE",
                std::ffi::OsStr::new(&source_inode),
            ),
            (
                "PLURX_TEST_READER_ADDRESS",
                std::ffi::OsStr::new(&address_string),
            ),
            (
                "PLURX_TEST_READER_OUTPUT_ADDRESS",
                std::ffi::OsStr::new(&output_string),
            ),
        ];
        runtime.block_on(async {
            let spawned = crate::producer_spawn::brokered_cache_part(
                Some(&input_url),
                Some(&output_url),
                false,
                crate::producer_spawn::Descriptors::from_raw_fds(
                    Some(original.as_raw_fd()),
                    Some(directory.as_raw_fd()),
                    None,
                    true,
                ),
                |descriptors| {
                    crate::producer_spawn::spawn(
                        &executable,
                        &["--exact".into(), FIXTURE.into(), "--nocapture".into()],
                        crate::producer_spawn::SpawnOptions {
                            runtime_cache: source.parent().expect("scratch root"),
                            progress: crate::producer_spawn::Progress::None,
                            descriptors,
                            env: &env,
                            work: plurx_core::process::ChildWork::background(
                                "brokered cache producer fixture",
                            ),
                        },
                    )
                },
            )
            .expect("same production brokered cache spawn");
            joined_producer_output(spawned).await
        })
    } else {
        runtime.block_on(plurx_core::process::source_lifetime::mediated(async {
            let mut command =
                tokio::process::Command::new(std::env::current_exe().expect("test binary"));
            command
                .args(["--exact", FIXTURE, "--nocapture"])
                .env_clear()
                .env("PLURX_TEST_READER_CONSUMER", "1")
                .env("PLURX_TEST_SOURCE_DEVICE", metadata.dev().to_string())
                .env("PLURX_TEST_SOURCE_INODE", metadata.ino().to_string())
                .env("PLURX_TEST_READER_ADDRESS", address.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let (child, _owner) = plurx_core::process::spawn_job_owned(
                &mut command,
                plurx_core::process::ChildWork::background("mediated source fixture"),
            )
            .expect("audited HTTP-only child");
            child
                .wait_with_output()
                .await
                .expect("join HTTP-only child")
        }))
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    bridge.join().expect("physical source bridge joined");
    if let Some(sink) = sink {
        sink.join().expect("physical output broker joined");
    }
}

async fn joined_producer_output(
    mut spawned: crate::producer_spawn::Spawned,
) -> std::process::Output {
    use tokio::io::AsyncReadExt;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let (status, out, err) = tokio::join!(
        spawned.child.wait(),
        spawned.stdout.read_to_end(&mut stdout),
        spawned.stderr.read_to_end(&mut stderr)
    );
    out.expect("producer stdout");
    err.expect("producer stderr");
    std::process::Output {
        status: status.expect("join real cache producer"),
        stdout,
        stderr,
    }
}

fn run_raw_cache_child(source: &Path, mode: &str) {
    use std::os::fd::AsRawFd;
    let original = std::fs::File::open(source).expect("raw source");
    let directory =
        std::fs::File::open(source.parent().expect("parent")).expect("raw output directory");
    let descriptors = crate::producer_spawn::Descriptors::from_raw_fds(
        Some(original.as_raw_fd()),
        Some(directory.as_raw_fd()),
        (mode == "cache_raw_subtitle").then_some(original.as_raw_fd()),
        true,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("raw cache runtime");
    let output = runtime.block_on(async {
        let child = crate::producer_spawn::brokered_cache_part(
            (mode != "cache_output_only").then_some("http://127.0.0.1:1/source"),
            (mode != "cache_input_only").then_some("http://127.0.0.1:1/output"),
            mode == "cache_embedded_subtitle",
            descriptors,
            |descriptors| {
                crate::producer_spawn::spawn(
                    Path::new("/bin/cat"),
                    &["/dev/fd/3".into()],
                    crate::producer_spawn::SpawnOptions {
                        runtime_cache: source.parent().expect("scratch root"),
                        progress: crate::producer_spawn::Progress::None,
                        descriptors,
                        env: &[],
                        work: plurx_core::process::ChildWork::background(
                            "raw cache producer fixture",
                        ),
                    },
                )
            },
        )
        .expect("raw cache producer");
        joined_producer_output(child).await
    });
    assert!(output.status.success());
    assert_eq!(output.stdout, vec![42u8; 4096]);
}

fn run_packet_probe(config: &Configuration) {
    let root = config
        .directory
        .parent()
        .expect("recovery root")
        .parent()
        .expect("data root");
    let source = std::fs::File::open(&config.source).expect("held source");
    let metadata = source.metadata().expect("source identity");
    let reporter = root.join("fd-check-ffprobe");
    std::fs::write(&reporter, format!("#!/usr/bin/env python3\nimport os,sys\ntry:\n s=os.fstat(3)\nexcept OSError:\n pass\nelse:\n if s.st_dev=={} and s.st_ino=={}: sys.exit(97)\nprint('{{\"packets\":[]}}')\n", metadata.dev(), metadata.ino())).expect("actual packet FD inspection tool");
    std::fs::set_permissions(&reporter, std::fs::Permissions::from_mode(0o700))
        .expect("executable FD inspector");
    // This fixture is a separately re-executed process running one test.
    std::env::set_var("PLURX_FFPROBE", &reporter);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("packet runtime");
    runtime.block_on(async {
        let store: Arc<dyn Store> = Arc::new(
            plurx_core::store::SqliteStore::open(&root.join("plurx.db")).expect("fixture Store"),
        );
        let Record::Background { token } = &config.record else {
            panic!("background owner");
        };
        let budget = crate::background_jobs::SourceReadBudget::new(
            store,
            token.owner.clone(),
            tokio_util::sync::CancellationToken::new(),
        );
        let output = crate::background_jobs::budgeted(
            budget,
            crate::ffmpeg::held_source_packet_probe_json(
                &source,
                crate::ffmpeg::HeldPacketProbeRequest {
                    stream_index: 0,
                    start_seconds: None,
                    packet_limit: Some(2),
                },
                Duration::from_secs(5),
                4096,
            ),
        )
        .await
        .expect("packet helper cannot inherit original descriptor");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).expect("packet JSON"),
            serde_json::json!({"packets":[]})
        );
    });
}

/// A planning error calls `finish` while an enrolled blocking read, an original
/// File clone and a declared raw child are still alive. Only the completion
/// ledger may publish the joined proof, after each physically retires.
/// Unenrolled jobs keep the explicit legacy join; a vetoed ledger never falls
/// back to it; an unknown launch waits for the children launched since; a
/// non-authoritative backend still settles.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn finish_during_blocked_enrolled_read_leaves_settlement_to_the_ledger() {
    use std::sync::Arc;
    fn held(fixture: &Fixture) -> i64 {
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM background_source_custody",
                [],
                |row| row.get(0),
            )
            .expect("custody count")
    }
    async fn started() -> (Fixture, crate::background_jobs::ActiveBackgroundJob) {
        let fixture = Fixture::new();
        super::super::configure_backend(&fixture.store, fixture.root.path(), true)
            .expect("exact backend");
        let (_, active) =
            crate::background_jobs::tests::active_on(Arc::clone(&fixture.store)).await;
        assert_eq!(held(&fixture), 1, "job owns its exact source custody");
        (fixture, active)
    }
    fn original(fixture: &Fixture) -> Arc<std::fs::File> {
        let path = fixture.root.path().join("actual-source");
        std::fs::write(&path, vec![42; 4096]).expect("source");
        Arc::new(std::fs::File::open(path).expect("open original"))
    }
    // The actor polls every 25ms; give it ample time to (wrongly) settle.
    async fn quiet() {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Unenrolled: the explicit finish is still the join.
    let (fixture, active) = started().await;
    active.finish().await;
    assert_eq!(held(&fixture), 0, "legacy explicit join settles");

    // Enrolled planning error: finish returns while the read is blocked.
    let (fixture, active) = started().await;
    let budget = active.fence().source_budget();
    let source = original(&fixture);
    let operation = budget
        .enroll_preparation(Arc::clone(&source))
        .expect("enrollment")
        .expect("Linux completion ledger");
    let retirement = budget
        .raw_retirement()
        .expect("registration")
        .expect("declared raw child");
    let surviving_original = Arc::clone(&source);
    let (input, parked, gate) =
        crate::source_input::SourceInput::completion_read_fixture(&source, &budget).await;
    let url = input.url().to_owned();
    let reading = tokio::spawn(async move {
        match reqwest::get(url).await {
            Ok(response) => response.bytes().await.map(|_| ()),
            Err(error) => Err(error),
        }
    });
    parked.await.expect("real blocking reader owns InputState");
    // The planner gave up: its operation and handles are gone, the read is not.
    drop((operation, source, input));
    tokio::time::timeout(Duration::from_secs(5), active.finish())
        .await
        .expect("finish never waits on a blocked physical read");
    quiet().await;
    assert_eq!(held(&fixture), 1, "blocked read keeps custody after finish");
    gate.release();
    let _ = reading.await.expect("HTTP waiter joined");
    quiet().await;
    assert_eq!(
        held(&fixture),
        1,
        "original File clone is a real capability"
    );
    drop(surviving_original);
    quiet().await;
    assert_eq!(held(&fixture), 1, "declared raw retirement is outstanding");
    retirement.joined();
    tokio::time::timeout(Duration::from_secs(5), async {
        while held(&fixture) != 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("ledger settles after every physical retirement");

    // A vetoed ledger owns settlement and refuses it; finish must not fall
    // back to the legacy join.
    let (fixture, active) = started().await;
    let budget = active.fence().source_budget();
    let operation = budget
        .enroll_preparation(original(&fixture))
        .expect("enrollment")
        .expect("Linux completion ledger");
    budget.untracked_source_capability();
    drop(operation);
    active.finish().await;
    quiet().await;
    assert_eq!(held(&fixture), 1, "unknown escape vetoes joined proof");

    // Another owner's (or any) unmediated launch is not a veto. It holds
    // this custody exactly until every daemon descendant started since then
    // has exited: the only processes that could have received the source.
    // SAFETY: prctl with integer arguments; the fixture process adopts orphans
    // exactly as PID 1 does in the container.
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
    let (fixture, active) = started().await;
    let budget = active.fence().source_budget();
    let operation = budget
        .enroll_preparation(original(&fixture))
        .expect("enrollment")
        .expect("Linux completion ledger");
    observe_children(false).expect("foreign unmediated launch");
    #[allow(clippy::disallowed_methods)] // The fixture owns and reaps this exact child.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "read line"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("child launched after the escape");
    drop(operation);
    active.finish().await;
    quiet().await;
    assert_eq!(held(&fixture), 1, "a live post-escape child keeps custody");
    drop(child.stdin.take());
    assert!(child.wait().expect("reap child").success());
    tokio::time::timeout(Duration::from_secs(5), async {
        while held(&fixture) != 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("custody settles once that child has exited");

    // A backend without witnesses (non-replicated Store) still settles once
    // every holder and every child launched since the owner started is gone.
    let (store, _, active) = crate::background_jobs::tests::active().await;
    let token = crate::background_jobs::tests::source_custody(&active).expect("custody");
    let budget = active.fence().source_budget();
    let operation = budget
        .enroll_preparation(original(&fixture))
        .expect("enrollment")
        .expect("Linux completion ledger");
    drop(operation);
    active.finish().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while store
            .resolve_background_source_custody(token.clone())
            .await
            .expect("resolve")
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("unwitnessed enrolled custody settles");
}

/// Real File/InputState/read-worker ownership, not recovery-anchor Arc counts.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn live_completion_waits_for_read_original_clones_and_explicit_raw_retirement() {
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    async fn started() -> (
        Fixture,
        Record,
        Arc<crate::background_jobs::SourceReadBudget>,
        CancellationToken,
        Arc<std::fs::File>,
    ) {
        let fixture = Fixture::new();
        let record = fixture.background("analysis");
        preread(&fixture, &record);
        super::super::configure_backend(&fixture.store, fixture.root.path(), true)
            .expect("exact backend");
        let Record::Background { token } = &record else {
            unreachable!()
        };
        crate::source_custody::activate_background_with_journal(&fixture.store, token.clone())
            .await
            .expect("active exact custody");
        let cancel = CancellationToken::new();
        let budget = crate::background_jobs::SourceReadBudget::with_completion(
            Arc::clone(&fixture.store),
            token,
            cancel.clone(),
            cancel.clone(),
        );
        let path = fixture.root.path().join("actual-source");
        std::fs::write(&path, vec![42; 4096]).expect("source");
        let source = Arc::new(std::fs::File::open(path).expect("open original"));
        (fixture, record, budget, cancel, source)
    }
    async fn released(fixture: &Fixture, record: &Record) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while count(fixture, record) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("independent owner settles after all physical closures");
    }
    // Before enrollment cancellation cannot manufacture an empty-reader proof.
    let (fixture, record, budget, cancel, source) = started().await;
    cancel.cancel();
    assert!(budget.enroll_preparation(Arc::clone(&source)).is_err());
    assert!(budget.source_holder().is_err());
    assert_eq!(count(&fixture, &record), 1);
    drop((budget, source));

    // The HTTP waiter may disappear while its blocking physical read remains.
    let (fixture, record, budget, cancel, source) = started().await;
    let operation = budget
        .enroll_preparation(Arc::clone(&source))
        .expect("enrollment");
    let surviving_original = Arc::clone(&source);
    let (input, parked, gate) =
        crate::source_input::SourceInput::completion_read_fixture(&source, &budget).await;
    let url = input.url().to_owned();
    let reading = tokio::spawn(async move {
        match reqwest::get(url).await {
            Ok(response) => response.bytes().await.map(|_| ()),
            Err(error) => Err(error),
        }
    });
    parked.await.expect("real blocking reader owns InputState");
    cancel.cancel();
    drop(operation);
    drop(source);
    drop(input);
    assert_eq!(count(&fixture, &record), 1, "live read must stay charged");
    assert!(
        budget.source_holder().is_err(),
        "closed registration cannot add readers"
    );
    gate.release();
    let _ = reading.await.expect("HTTP waiter joined");
    assert_eq!(
        count(&fixture, &record),
        1,
        "original Arc clone is a real capability"
    );
    drop(surviving_original);
    released(&fixture, &record).await;

    // Declared raw children still poison crash recovery. The LIVE actor needs
    // the explicit child+worker retirement, never a guard Drop notification.
    let (fixture, record, budget, cancel, source) = started().await;
    let operation = budget
        .enroll_preparation(Arc::clone(&source))
        .expect("enrollment");
    let retirement = budget
        .raw_retirement()
        .expect("registration")
        .expect("tracked raw child");
    let spawned = retirement
        .spawn(|| {
            crate::producer_spawn::spawn(
                Path::new("/bin/sh"),
                &["-c".into(), "dd bs=1 count=1 <&3 >/dev/null".into()],
                crate::producer_spawn::SpawnOptions {
                    runtime_cache: fixture.root.path(),
                    progress: crate::producer_spawn::Progress::None,
                    descriptors: crate::producer_spawn::Descriptors::from_files(
                        Some(&source),
                        None,
                        None,
                        false,
                    ),
                    env: &[],
                    work: plurx_core::process::ChildWork::background(
                        "declared raw retirement fixture",
                    ),
                },
            )
        })
        .expect("real source-bearing child launch");
    let output = joined_producer_output(spawned).await;
    assert!(
        output.status.success(),
        "child consumed its real original FD"
    );
    // A separate actual source worker remains owned until explicit join.
    let worker_source = source.try_clone().expect("declared worker descriptor");
    let (worker_release, worker_wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        use std::os::unix::fs::FileExt;
        worker_wait.recv().expect("worker release");
        worker_source
            .read_at(&mut [0; 1], 0)
            .expect("physical source read");
    });
    let directory = fixture.dir.with_file_name("v2");
    assert_eq!(
        live_background_escape(
            &directory,
            match &record {
                Record::Background { token } => token,
                _ => unreachable!(),
            }
        ),
        Some(None),
        "declared raw launch is no unknown escape"
    );
    replay_at(&fixture.store, &directory, &fixture.dir, NODE)
        .await
        .expect("invalidated replay remains conservative");
    assert_eq!(count(&fixture, &record), 1);
    cancel.cancel();
    drop((operation, source));
    assert_eq!(
        count(&fixture, &record),
        1,
        "declared raw retirement is still outstanding"
    );
    worker_release.send(()).expect("release actual read worker");
    worker.join().expect("actual source worker joined");
    retirement.joined();
    released(&fixture, &record).await;

    // A dropped/unjoined declaration vetoes live proof. An unknown escape
    // instead waits for every descendant launched since it (none here). Neither
    // can turn an invalidated restart record into joined proof.
    // SAFETY: prctl with integer arguments, as the container's PID 1 adopts.
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
    for unknown in [false, true] {
        let (fixture, record, budget, cancel, source) = started().await;
        let operation = budget
            .enroll_preparation(Arc::clone(&source))
            .expect("enrollment");
        let retirement = budget
            .raw_retirement()
            .expect("registration")
            .expect("tracked raw child");
        retirement
            .spawn(|| observe_children(false))
            .expect("declared raw launch");
        if unknown {
            observe_children(false).expect("unknown raw escape");
            retirement.joined();
        } else {
            drop(retirement);
        }
        cancel.cancel();
        drop((operation, source));
        replay_at(
            &fixture.store,
            &fixture.dir.with_file_name("v2"),
            &fixture.dir,
            NODE,
        )
        .await
        .expect("raw restart replay");
        if unknown {
            released(&fixture, &record).await;
        } else {
            quiet_for_veto().await;
            assert_eq!(count(&fixture, &record), 1, "unjoined declaration vetoes");
        }
    }
}

async fn quiet_for_veto() {
    tokio::time::sleep(Duration::from_millis(250)).await;
}
