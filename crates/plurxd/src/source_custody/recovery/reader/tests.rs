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
async fn reader_recovery_refuses_unarmed_unmediated_foreign_kernel_and_replaced_inode() {
    for mode in [
        "unarmed",
        "unmediated",
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
                "unmediated" | "prepared_descriptor" | "retry_descriptor"
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
                assert!(
                    info.st_dev as u64 != expected_device || info.st_ino != expected_inode,
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
    let mut file = std::fs::File::open(source).expect("daemon original descriptor");
    let metadata = file.metadata().expect("physical identity");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("daemon source bridge");
    let address = listener.local_addr().expect("bridge address");
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
    let output = runtime.block_on(plurx_core::process::source_lifetime::mediated(async {
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
    }));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    bridge.join().expect("physical source bridge joined");
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
