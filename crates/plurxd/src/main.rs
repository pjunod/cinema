// Production children go through `process_control::spawn_job_owned`; see
// clippy.toml.
#![cfg_attr(test, allow(clippy::disallowed_methods))]
#![cfg_attr(plurx_dv_segment_probe, allow(dead_code))]

mod admission;
mod availability;
use plurx_core::process::bounded as bounded_process;
#[cfg(test)]
#[path = "../../plurx-core/tests/support/queue_fixture.rs"]
mod queue_fixture;
// Reuse the exact Core fixture observer. The shared fixture exports helpers
// used by different packages; this test-only include has no production path.
#[cfg(test)]
use plurx_core as observer_core;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../plurx-core/tests/fixtures/startup_observer.rs"]
mod sharing_fixture_clock;

mod artifact_integrity;
mod background_jobs;
mod backup;
mod cachekeep;
mod channel_subjects;
mod clock_offset;
mod copyseg;
mod decode_facts;
mod decoder_health;
mod delivery;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod desktop_control;
mod diagnostics;
mod diagnostics_delivery;
mod dv_disk;
#[cfg(unix)]
mod dv_segment;
mod dvpipe;
mod ffmpeg;
mod ffmpeg_support;
mod fontenv;
mod fragindex;
mod fragment_index_cluster;
mod hevc_census;
mod http;
mod https;
mod job_lease;
mod library_search;
mod live_tv;
mod live_tv_delivery;
mod logbuf;
mod macos_video;
mod manifest_cache;
mod media_pool;
mod media_sessions;
mod meter;
mod offline;
mod online_subtitles;
mod panics;
mod pgs_overlay;
mod pipeprobe;
mod playback_cache;
mod playback_control;
mod playstart;
mod process_control;
mod prodexec;
mod prodrun;
mod prodsched;
mod produce;
mod producer_spawn;
mod progress;
mod progressive;
mod reader_formats;
mod redact;
mod renditiondir;
mod rolling_output;
mod rolling_provenance;
mod schedule;
mod scratch_ledger;
mod scratch_put;
mod seam_hooks;
mod serving_fence;
mod settings_changes;
mod shared_cache;
mod sharing;
mod sharing_client;
mod sharing_connection_custody;
mod sharing_host;
#[cfg(test)]
mod sharing_protocol_fixture;
mod source_probe;
mod state;
mod store_result;
mod storeprobe;
mod subtitle_models;
mod subtitle_ranges;
mod subtitle_ride_along;
mod subtitle_source;
mod subtitle_text_translation;
mod subtitle_transcription;
mod subtitle_translation;
mod subtitles;
mod telemetry;
mod titlestore;
mod trakt;
mod transcode;
mod transcoder_optimization;
mod version;
mod vodencode;
mod vodgen;
mod vodserve;
mod waitpool;
mod wal_cli;
mod watched;
#[cfg(windows)]
mod windows_service;

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::ConnectInfo;
use axum::http::Request;
use clap::{Parser, Subcommand};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ConnectionBuilder;
use hyper_util::service::TowerToHyperService;
use plurx_core::cluster::coordination::StoreCoordinator;
use plurx_core::cluster::migration::{
    connect_activated_store, select_daemon_store_observing, SelectedBackend, StartupClockObserver,
};
use plurx_core::config::{Config, StorageConfig};
use plurx_core::domain::LibraryKind;
use plurx_core::metadata::{self, AniListClient, TmdbClient};
#[cfg(test)]
use plurx_core::store::SqliteStore;
use plurx_core::store::{keys, Store};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

use crate::job_lease::acquire_cluster_job;
use crate::state::{AppState, SystemInfo};

/// Test fixtures that exercise capability-style path walks must start below a
/// canonical root. Darwin exposes its per-user temporary directory through
/// `/var`, a system symlink to `/private/var`, which the production no-follow
/// walker intentionally refuses.
#[cfg(test)]
pub(crate) fn test_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::fs::canonicalize(std::env::temp_dir())?;
    tempfile::tempdir_in(root)
}

/// Create an executable test fixture (a script standing in for FFmpeg, a
/// tool, a probe) that this process can execute at once.
///
/// Writing it here with `std::fs::write` and then executing it races every
/// other test thread: a child forked anywhere in this process while the write
/// descriptor is open inherits it, and until that child reaches `exec` (which
/// closes it, being close-on-exec) the kernel refuses to execute the file with
/// `ETXTBSY` ("Text file busy"). On a loaded runner that window is long enough
/// to hit. So this process never opens the file for writing at all: a `/bin/sh`
/// child writes it and sets its mode, and the only writable descriptor lives
/// and dies in that child's process tree before this returns.
#[cfg(all(test, unix))]
pub(crate) fn write_test_executable(
    path: impl AsRef<std::path::Path>,
    contents: impl AsRef<[u8]>,
    mode: u32,
) {
    use std::io::Write;

    let path = path.as_ref();
    let mut writer = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod \"$2\" \"$1\"")
        .arg("write_test_executable")
        .arg(path)
        .arg(format!("{mode:o}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn a writer for {}: {error}", path.display()));
    writer
        .stdin
        .take()
        .expect("writer stdin")
        .write_all(contents.as_ref())
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    let output = writer
        .wait_with_output()
        .unwrap_or_else(|error| panic!("wait for the writer of {}: {error}", path.display()));
    assert!(
        output.status.success(),
        "writing {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(all(test, unix))]
mod test_executable_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// The race `write_test_executable` exists for, run on purpose: other
    /// threads fork continuously while fixtures are written and executed at
    /// once. Written in-process (`std::fs::write` then `set_permissions`),
    /// this fails with `ETXTBSY` within a few hundred iterations on a busy
    /// host; written by the helper, no descriptor of this process is ever
    /// writable on the fixture, so every exec succeeds.
    #[test]
    fn a_fixture_executes_at_once_while_other_threads_fork() {
        let root = crate::test_tempdir().expect("root");
        let stop = Arc::new(AtomicBool::new(false));
        let forkers: Vec<_> = (0..4)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let _ = std::process::Command::new("/bin/true").status();
                    }
                })
            })
            .collect();
        let outcome = std::panic::catch_unwind(|| {
            for index in 0..300 {
                let path = root.path().join(format!("fixture-{index}"));
                crate::write_test_executable(&path, "#!/bin/sh\nexit 7\n", 0o700);
                let status = std::process::Command::new(&path)
                    .status()
                    .unwrap_or_else(|error| panic!("fixture {index} did not execute: {error}"));
                assert_eq!(status.code(), Some(7), "fixture {index} ran its own body");
            }
        });
        stop.store(true, Ordering::Release);
        for forker in forkers {
            forker.join().expect("forker");
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn a_fixture_gets_exactly_the_mode_asked_for() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::test_tempdir().expect("root");
        for mode in [0o700, 0o755, 0o500] {
            let path = root.path().join(format!("mode-{mode:o}"));
            crate::write_test_executable(&path, "#!/bin/sh\n", mode);
            let actual = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(actual, mode, "{}", path.display());
            assert_eq!(std::fs::read(&path).expect("contents"), b"#!/bin/sh\n");
        }
    }
}

/// A scoped tracing subscriber for one test, with a global default behind it.
///
/// `tracing::subscriber::set_default` alone is not enough in a parallel test
/// binary. tracing caches an `Interest` per callsite, computed the first
/// time the callsite is hit; when exactly one dispatcher is registered,
/// tracing-core computes it from the *current thread's* default rather than
/// the registered one, and on a thread with no scoped subscriber that is
/// `NoSubscriber`, whose answer is "never". So a test that sets a scoped
/// subscriber and then walks into a callsite another thread reached first
/// finds that callsite cached as never-interesting, and its event is dropped
/// before the subscriber sees it — on a loaded machine, often enough to fail
/// the copy-argv test one run in three.
///
/// Installing a permissive global default once per process closes that:
/// with a real dispatcher as the fallback on every thread, no callsite is
/// ever cached as "never". A bare `Registry` accepts every callsite and
/// keeps nothing but span data. A scoped subscriber set afterwards still
/// wins on its own thread, exactly as before.
#[cfg(test)]
pub(crate) fn test_tracing_default<S>(subscriber: S) -> tracing::subscriber::DefaultGuard
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    static GLOBAL: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    GLOBAL.get_or_init(|| {
        // Already set by an earlier `try_init` in this process is fine: any
        // real dispatcher as the fallback is what matters.
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    });
    tracing::subscriber::set_default(subscriber)
}

#[cfg(test)]
pub(crate) fn test_temp_path(name: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    std::fs::canonicalize(std::env::temp_dir())
        .expect("canonical system temporary directory")
        .join(name)
}

#[derive(Parser)]
// `--version` carries the build stamp too: "0.1.0 (v0.1.0-14-gc0ffee)". The
// bare number is what a release *is*; the git description is what someone
// filing a bug is actually running.
#[command(
    name = "plurxd",
    version = crate::version::LONG,
    about = "plurx media server daemon"
)]
struct Cli {
    /// Print the full compiled source commit and exit without loading configuration.
    #[cfg(unix)]
    #[arg(long)]
    source_commit: bool,

    /// Path to a TOML config file (default: ./plurx.toml or
    /// /etc/plurx/plurx.toml if present).
    #[arg(long, global = true, env = "PLURX_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (the default when no subcommand is given).
    Run,
    /// Provision the node-local TLS identity used by the private sharing listener.
    Sharing {
        #[command(subcommand)]
        command: SharingCommand,
    },
    /// Install, remove, or enter the native Windows service.
    #[cfg(windows)]
    Service {
        #[command(subcommand)]
        command: WindowsServiceCommand,
    },
    /// Probe a running local server's /readyz and exit 0/1 (container
    /// health checks: no curl needed in the image).
    Healthcheck,
    /// Print the exact fragment-index engine identity used by this binary.
    MediaRuntimeIdentity,
    /// Advertise a bridge-networked server on the host's Bonjour interfaces.
    /// This is intended for the discovery companion container; the HTTP server
    /// remains attached to its normal Docker networks.
    Advertise {
        /// Base URL where the server is published on the Docker host.
        #[arg(
            long,
            env = "PLURX_DISCOVERY_SERVER_URL",
            default_value = "http://127.0.0.1:32400"
        )]
        server: String,
    },
    /// Refuse the legacy direct-store password reset. An authenticated
    /// daemon-owned recovery control path is required before this can safely
    /// mutate credentials beside cache-only cluster recovery reads.
    ResetPassword {
        /// Username whose password to reset.
        username: String,
        /// Retained for CLI compatibility; the disabled command refuses
        /// before reading, validating, or applying this value.
        #[arg(long)]
        password: Option<String>,
    },
    /// Force provider metadata and artwork through the activated store. Useful
    /// after an artwork-quality upgrade; runs beside the server and requires
    /// it running, because it shares that daemon's voter.
    RefreshMetadata {
        /// Refresh one library id instead of every provider-backed library.
        #[arg(long)]
        library: Option<i64>,
    },
    /// Inspect or prepare a running replicated cluster through its admin API.
    Cluster {
        #[command(subcommand)]
        command: ClusterCommand,
    },
    /// Verify or restore a portable cluster archive while the daemon is stopped.
    Restore {
        /// Existing portable backup directory.
        #[arg(long)]
        archive: PathBuf,
        /// Fresh data directory to populate. Not used with --verify.
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Verify checksums, integrity, identity, and credential key without writes.
        #[arg(long)]
        verify: bool,
        /// Rewrite an absolute path prefix, in OLD=NEW form. May be repeated.
        #[arg(long)]
        remap: Vec<String>,
        /// Credential key from a separate secret store instead of the archived key.
        #[arg(long)]
        credential_key: Option<PathBuf>,
        /// Host or IP the restored first voter advertises to later joiners.
        #[arg(long)]
        advertise_host: Option<String>,
    },
    /// Inspect or back up a stopped local Raft WAL. No source mutation is implemented.
    Wal {
        #[command(subcommand)]
        command: crate::wal_cli::WalCommand,
    },
    /// Panic on purpose and exit, so a release binary's backtrace can be read.
    ///
    /// Hidden: it exists to prove the release profile's line tables
    /// symbolicate (docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md
    /// §3.5 and §5.6, which name it `--diagnostic-panic`; a subcommand is the
    /// shape this CLI gives every other action), and for an operator to check
    /// the same on a deployed image with `RUST_BACKTRACE=1 plurxd
    /// diagnostic-panic`. It touches no storage and no network.
    #[command(hide = true)]
    DiagnosticPanic,
}

#[derive(Subcommand)]
enum SharingCommand {
    /// Fixed host-installer compatibility probe; no credentials or mutations.
    HostCapabilities,
    /// Create an owner-only node key and certificate; refuse existing identity files.
    InitTls {
        #[arg(long)]
        key_directory: PathBuf,
    },
}

#[cfg(windows)]
#[derive(Subcommand)]
enum WindowsServiceCommand {
    /// Install and start plurx as an automatic LocalSystem service.
    Install,
    /// Stop and remove the installed plurx service.
    Uninstall,
    /// SCM-only entry point installed by `service install`.
    #[command(hide = true)]
    Run,
}

#[derive(Subcommand)]
enum ClusterCommand {
    /// Build one portable, checksummed cluster backup on the running voter.
    Backup {
        #[arg(long, default_value = "http://127.0.0.1:32400")]
        server: String,
        /// Owner-only file containing the admin bearer token.
        #[arg(long, env = "PLURX_ADMIN_TOKEN_FILE")]
        token_file: Option<PathBuf>,
        /// Existing absolute destination directory; overrides the saved setting.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Print the same versioned aggregate and rollout verdict as the Cluster tab.
    Status {
        #[arg(long, default_value = "http://127.0.0.1:32400")]
        server: String,
        /// Owner-only file containing the admin bearer token.
        #[arg(long, env = "PLURX_ADMIN_TOKEN_FILE")]
        token_file: Option<PathBuf>,
        /// Emit the versioned JSON response instead of the compact table.
        #[arg(long)]
        json: bool,
    },
    /// Download a bounded, redacted cluster evidence archive.
    SupportBundle {
        #[arg(long, default_value = "http://127.0.0.1:32400")]
        server: String,
        /// Owner-only file containing the admin bearer token.
        #[arg(long, env = "PLURX_ADMIN_TOKEN_FILE")]
        token_file: Option<PathBuf>,
        /// New archive path. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
    /// Expireably block new mutable media on one exact safe candidate and wait for drain.
    PrepareRestart {
        #[arg(long, default_value = "http://127.0.0.1:32400")]
        server: String,
        #[arg(long, env = "PLURX_ADMIN_TOKEN_FILE")]
        token_file: Option<PathBuf>,
        /// Must name the backend at --server. Omit to use its local node id.
        #[arg(long)]
        node_id: Option<String>,
    },
    /// Cancel an unexpired process-local restart preparation.
    CancelRestart {
        #[arg(long, default_value = "http://127.0.0.1:32400")]
        server: String,
        #[arg(long, env = "PLURX_ADMIN_TOKEN_FILE")]
        token_file: Option<PathBuf>,
        /// Must name the backend at --server. Omit to use its local node id.
        #[arg(long)]
        node_id: Option<String>,
    },
}

#[derive(Debug)]
struct CliExit {
    code: i32,
    message: String,
}

impl std::fmt::Display for CliExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliExit {}

fn cli_exit(code: i32, message: impl Into<String>) -> anyhow::Error {
    CliExit {
        code,
        message: message.into(),
    }
    .into()
}

#[cfg(not(plurx_dv_segment_probe))]
fn main() -> anyhow::Result<()> {
    if let Some(result) = decode_facts::dispatch_probe_bootstrap() {
        result.context("launching namespace-bound probe")?;
        anyhow::bail!("probe exec unexpectedly returned");
    }
    daemon_main()
}

// Source-only validation entry: compile the real production graph without
// unrelated unit bodies. No public command, server listener or qualification.
#[cfg(all(unix, plurx_dv_segment_probe))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dv_segment::physical_probe().await;
    Ok(())
}

#[cfg(all(not(unix), plurx_dv_segment_probe))]
compile_error!("DV segment physical probe requires Unix descriptor custody");

#[tokio::main]
async fn daemon_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    #[cfg(unix)]
    if cli.source_commit {
        println!("{}", crate::version::SOURCE_COMMIT);
        return Ok(());
    }
    let config = Config::load(cli.config.as_deref()).context("loading configuration")?;
    if let Err(error) = dispatch(
        cli.command.unwrap_or(Command::Run),
        config,
        cli.config.as_deref(),
    )
    .await
    {
        let code = error
            .downcast_ref::<CliExit>()
            .map_or(1, |error| error.code);
        eprintln!("{error:#}");
        std::process::exit(code);
    }
    Ok(())
}

/// The deliberate panic behind `plurxd diagnostic-panic`. Its own function, and
/// `#[inline(never)]`, so the frame a reader looks for in the backtrace has
/// this name and this file's line number rather than being folded into
/// `dispatch`.
#[inline(never)]
fn diagnostic_panic() -> ! {
    panic!("deliberate panic requested by `plurxd diagnostic-panic`");
}

/// Route a parsed command, separated from `main` so every subcommand but the
/// server itself is reachable without a process launch.
async fn dispatch(
    command: Command,
    mut config: Config,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    #[cfg(not(windows))]
    let _ = config_path;
    match &command {
        Command::Run => {
            canonicalize_storage_roots(&mut config)?;
        }
        #[cfg(windows)]
        Command::Service { .. } => {}
        Command::ResetPassword { .. } => {
            canonicalize_configured_dir(&mut config.storage.data_dir, "data")?;
        }
        Command::RefreshMetadata { .. } => {}
        Command::Healthcheck
        | Command::MediaRuntimeIdentity
        | Command::Advertise { .. }
        | Command::Cluster { .. }
        | Command::Restore { .. }
        | Command::Wal { .. }
        | Command::Sharing { .. }
        | Command::DiagnosticPanic => {}
    }
    match command {
        Command::Run => run(config).await,
        Command::Sharing {
            command: SharingCommand::HostCapabilities,
        } => {
            println!("{{\"schema_version\":1,\"managed_host_egress_guard\":true}}");
            Ok(())
        }
        Command::Sharing {
            command: SharingCommand::InitTls { key_directory },
        } => {
            let now = crate::state::clock_ms() / 1000;
            let node = plurx_core::sharing_tls::NodeTls::initialize(&key_directory, now)?;
            println!("SPKI SHA-256: {}", node.spki_sha256);
            println!(
                "Certificate expires at Unix second {}",
                node.expires_at_seconds
            );
            Ok(())
        }
        #[cfg(windows)]
        Command::Service { command } => match command {
            WindowsServiceCommand::Install => {
                windows_service::install(config_path).map_err(Into::into)
            }
            WindowsServiceCommand::Uninstall => windows_service::uninstall().map_err(Into::into),
            WindowsServiceCommand::Run => {
                windows_service::dispatch_service(config).map_err(Into::into)
            }
        },
        Command::DiagnosticPanic => diagnostic_panic(),
        Command::Healthcheck => {
            // One terse line either way — this output lands in `docker inspect`.
            if let Err(error) = healthcheck(&config) {
                eprintln!("unhealthy: {error:#}");
                std::process::exit(1);
            }
            Ok(())
        }
        Command::MediaRuntimeIdentity => {
            println!(
                "{}",
                serde_json::to_string(&media_runtime_identity().await)?
            );
            Ok(())
        }
        Command::Advertise { server } => advertise(&server).await,
        Command::ResetPassword { username, password } => {
            reset_password(&config, &username, password).await
        }
        Command::RefreshMetadata { library } => refresh_metadata(&mut config, library).await,
        Command::Cluster { command } => cluster_command(command).await,
        Command::Restore {
            archive,
            data_dir,
            verify,
            remap,
            credential_key,
            advertise_host,
        } => {
            if verify {
                if data_dir.is_some() || !remap.is_empty() || advertise_host.is_some() {
                    anyhow::bail!(
                        "--verify cannot be combined with --data-dir, --remap, or --advertise-host"
                    );
                }
                let report = plurx_core::cluster::migration::verify_cluster_backup_archive(
                    &archive,
                    credential_key.as_deref(),
                )?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                Ok(())
            } else {
                let data_dir = data_dir.ok_or_else(|| {
                    anyhow::anyhow!("--data-dir is required unless --verify is used")
                })?;
                config.storage.data_dir = data_dir;
                if let Some(host) = advertise_host {
                    config.cluster.advertise_host = host;
                }
                let remaps = parse_restore_remaps(&remap)?;
                let report = plurx_core::cluster::migration::restore_cluster_backup_archive(
                    &archive,
                    &config,
                    &remaps,
                    credential_key.as_deref(),
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                eprintln!(
                    "restore created a fenced first voter; before directing clients here, \
                     rejoin every surviving old member with a join token issued by this voter: \
                     stop the node, create a new empty data directory owned by the service user, \
                     copy plurx.toml and nothing else into it, move the old directory aside as \
                     a retired copy and put the new one at the configured path, write a fresh \
                     join token where join_token_file points, and start the node"
                );
                Ok(())
            }
        }
        Command::Wal { command } => crate::wal_cli::run(command, &config).await,
    }
}

#[derive(Debug, serde::Serialize)]
struct MediaRuntimeIdentity {
    architecture: &'static str,
    application_sha: Option<&'static str>,
    runtime_image_digest: Option<String>,
    package_manifest_sha256: Option<String>,
    engine_digest: String,
    engine_current: bool,
}

/// The index cache key's actual FFmpeg identity, not a lookalike package or
/// version hash. The image build writes the package manifest alongside the
/// shipped tools; a local source build reports that field as unavailable.
async fn media_runtime_identity() -> MediaRuntimeIdentity {
    let manifest = std::fs::read("/usr/share/doc/plurx/media-runtime-packages.txt")
        .ok()
        .map(|bytes| hex::encode(Sha256::digest(bytes)));
    MediaRuntimeIdentity {
        architecture: std::env::consts::ARCH,
        application_sha: option_env!("PLURX_BUILD_SHA").filter(|sha| !sha.is_empty()),
        runtime_image_digest: std::env::var("PLURX_MEDIA_RUNTIME_DIGEST")
            .ok()
            .filter(|digest| !digest.is_empty()),
        package_manifest_sha256: manifest,
        engine_digest: crate::ffmpeg::fragment_index_engine_digest().await,
        engine_current: crate::ffmpeg::fragment_index_engine_is_current().await,
    }
}

fn parse_restore_remaps(values: &[String]) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    values
        .iter()
        .map(|value| {
            let (from, to) = value
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("restore remap {value:?} must be OLD=NEW"))?;
            if from.is_empty() || to.is_empty() {
                anyhow::bail!("restore remap {value:?} must have non-empty OLD and NEW paths");
            }
            Ok((PathBuf::from(from), PathBuf::from(to)))
        })
        .collect()
}

async fn cluster_command(command: ClusterCommand) -> anyhow::Result<()> {
    match command {
        ClusterCommand::Backup {
            server,
            token_file,
            output,
        } => cluster_backup_command(&server, token_file.as_deref(), output.as_deref()).await,
        ClusterCommand::Status {
            server,
            token_file,
            json,
        } => cluster_status_command(&server, token_file.as_deref(), json).await,
        ClusterCommand::SupportBundle {
            server,
            token_file,
            output,
        } => cluster_support_bundle_command(&server, token_file.as_deref(), &output).await,
        ClusterCommand::PrepareRestart {
            server,
            token_file,
            node_id,
        } => {
            cluster_prepare_restart_command(&server, token_file.as_deref(), node_id.as_deref())
                .await
        }
        ClusterCommand::CancelRestart {
            server,
            token_file,
            node_id,
        } => {
            cluster_cancel_restart_command(&server, token_file.as_deref(), node_id.as_deref()).await
        }
    }
}

async fn cluster_backup_command(
    server: &str,
    token_file: Option<&std::path::Path>,
    output: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "output": output }))
        .map_err(|_| cli_exit(2, "cluster backup request could not be encoded"))?;
    let response = cluster_api_request_method(
        server,
        token_file,
        "/api/v1/cluster/backups",
        reqwest::Method::POST,
        Some(body),
    )
    .await?;
    let body = bounded_cli_response(response, 1024 * 1024).await?;
    let response: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| cli_exit(3, "cluster backup returned an invalid response"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response)
            .map_err(|_| cli_exit(3, "cluster backup response could not be encoded"))?
    );
    Ok(())
}

async fn cluster_status_command(
    server: &str,
    token_file: Option<&std::path::Path>,
    json: bool,
) -> anyhow::Result<()> {
    let status = fetch_cluster_aggregate(server, token_file).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status)
                .map_err(|_| cli_exit(3, "cluster status could not be encoded"))?
        );
    } else {
        print_cluster_status(&status);
    }
    if status.verdict.safe_to_restart_one {
        return Ok(());
    }
    let incomplete = status
        .nodes
        .iter()
        .any(|row| row.observation != crate::http::cluster_operations::ObservationState::Answered);
    Err(cli_exit(
        if incomplete { 3 } else { 1 },
        if incomplete {
            "cluster status is incomplete because one or more required peers were not observed"
        } else {
            "cluster status was collected, but restarting a voter is not safe"
        },
    ))
}

async fn fetch_cluster_aggregate(
    server: &str,
    token_file: Option<&std::path::Path>,
) -> anyhow::Result<crate::http::cluster_operations::ClusterOperationsAggregate> {
    let response = cluster_api_request(server, token_file, "/api/v1/cluster/status").await?;
    let body = bounded_cli_response(response, 1024 * 1024).await?;
    serde_json::from_slice(&body).map_err(|_| {
        cli_exit(
            3,
            "cluster status returned an invalid or incompatible response",
        )
    })
}

fn print_cluster_status(status: &crate::http::cluster_operations::ClusterOperationsAggregate) {
    println!(
        "{:<20} {:<12} {:<18} {:<23} {:<18} MEDIA",
        "NODE", "PROCESS", "SERVING", "RAFT", "WAL"
    );
    for row in &status.nodes {
        let node = row.membership.hostname.as_str();
        if let Some(local) = row.status.as_ref() {
            let serving = if local.serving.ready {
                "ready".to_owned()
            } else {
                format!("fenced:{:?}", local.serving.reason)
            };
            let raft = format!(
                "term {} lag {}",
                local
                    .raft
                    .current_term
                    .map_or_else(|| "?".to_owned(), |value| value.to_string()),
                local
                    .raft
                    .apply_lag_entries
                    .map_or_else(|| "?".to_owned(), |value| value.to_string())
            );
            let wal = local.wal.snapshot.as_ref().map_or_else(
                || "unavailable".to_owned(),
                |wal| format!("{:?} durable {:?}", wal.state, wal.last_durable_index),
            );
            println!(
                "{:<20} {:<12} {:<18} {:<23} {:<18} {} sessions",
                truncate_table(node, 20),
                "live",
                truncate_table(&serving, 18),
                truncate_table(&raft, 23),
                truncate_table(&wal, 18),
                local.media.local_active_sessions,
            );
        } else {
            println!(
                "{:<20} {:<12} {:<18} {:<23} {:<18} unknown",
                truncate_table(node, 20),
                format!("{:?}", row.observation).to_ascii_lowercase(),
                "unknown",
                "unknown",
                "unknown",
            );
        }
    }
    println!();
    println!(
        "safe_to_restart_one: {}",
        status.verdict.safe_to_restart_one
    );
    if let Some(candidate) = status.verdict.candidate_node_id.as_deref() {
        println!("candidate_node_id: {candidate}");
    }
    for blocker in &status.verdict.blockers {
        println!(
            "BLOCKER {}{}: {}",
            blocker.code,
            blocker
                .node_id
                .as_deref()
                .map_or_else(String::new, |node| format!(" [{node}]")),
            blocker.message
        );
    }
    for warning in &status.verdict.warnings {
        println!("WARNING {}: {}", warning.code, warning.message);
    }
}

fn truncate_table(value: &str, width: usize) -> String {
    let mut text = value.chars().take(width).collect::<String>();
    if value.chars().count() > width && width > 1 {
        text.pop();
        text.push('…');
    }
    text
}

async fn cluster_support_bundle_command(
    server: &str,
    token_file: Option<&std::path::Path>,
    output: &std::path::Path,
) -> anyhow::Result<()> {
    let response =
        cluster_api_request(server, token_file, "/api/v1/cluster/support-bundle").await?;
    let body = bounded_cli_response(response, 2 * 1024 * 1024).await?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(output).map_err(|error| {
        cli_exit(
            2,
            format!(
                "refusing to overwrite or create support bundle {}: {error}",
                output.display()
            ),
        )
    })?;
    file.write_all(&body)
        .and_then(|()| file.sync_all())
        .map_err(|error| cli_exit(2, format!("writing {}: {error}", output.display())))?;
    println!("wrote redacted support bundle {}", output.display());
    Ok(())
}

async fn cluster_prepare_restart_command(
    server: &str,
    token_file: Option<&std::path::Path>,
    requested_node_id: Option<&str>,
) -> anyhow::Result<()> {
    let preflight = fetch_cluster_aggregate(server, token_file).await?;
    if !preflight.verdict.safe_to_restart_one {
        print_cluster_status(&preflight);
        return Err(cli_exit(
            1,
            "restart preparation refused because the rollout verdict is unsafe",
        ));
    }
    let local_node_id = preflight.membership.local_node_id.as_str();
    let node_id = requested_node_id.unwrap_or(local_node_id);
    if node_id != local_node_id {
        return Err(cli_exit(
            2,
            "--node-id must name the backend at --server; connect directly to the target node",
        ));
    }
    if preflight.verdict.candidate_node_id.as_deref() != Some(node_id) {
        return Err(cli_exit(
            1,
            "this backend is not the current safe restart candidate; connect to the named candidate",
        ));
    }
    let path = format!(
        "/api/v1/cluster/nodes/{}/restart-preparation",
        percent_encode_path_segment(node_id)
    );
    let response = cluster_api_request_method(
        server,
        token_file,
        &path,
        reqwest::Method::POST,
        Some(br#"{"expires_in_seconds":900}"#.to_vec()),
    )
    .await?;
    let body = bounded_cli_response(response, 64 * 1024).await?;
    let mut preparation = serde_json::from_slice::<
        crate::http::cluster_operations::RestartPreparationResponse,
    >(&body)
    .map_err(|_| cli_exit(3, "restart preparation returned an invalid response"))?;
    loop {
        println!(
            "{}: {} owned sessions, {} admissions in flight, drained={}",
            node_id,
            preparation.media.local_active_sessions,
            preparation.media.admissions_in_flight,
            preparation.media.drained
        );
        if !preparation.media.new_admissions_blocked {
            return Err(cli_exit(
                1,
                "restart preparation expired or was canceled before the node drained",
            ));
        }
        if preparation.media.drained {
            println!("Direct-play connections remain a proxy-side check.");
            println!("Run exactly one supervisor command on {node_id}:");
            for command in &preparation.restart_commands {
                println!("  {:<16} {}", command.supervisor, command.command);
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let aggregate = fetch_cluster_aggregate(server, token_file).await?;
        let Some(local) = aggregate
            .nodes
            .iter()
            .find(|row| row.membership.node_id == node_id)
            .and_then(|row| row.status.as_ref())
        else {
            return Err(cli_exit(3, "the prepared node could no longer be observed"));
        };
        preparation.media = local.media.clone();
        preparation.restart_commands = local.media.restart_commands.clone();
    }
}

async fn cluster_cancel_restart_command(
    server: &str,
    token_file: Option<&std::path::Path>,
    requested_node_id: Option<&str>,
) -> anyhow::Result<()> {
    let status = fetch_cluster_aggregate(server, token_file).await?;
    let local_node_id = status.membership.local_node_id.as_str();
    let node_id = requested_node_id.unwrap_or(local_node_id);
    if node_id != local_node_id {
        return Err(cli_exit(
            2,
            "--node-id must name the backend at --server; connect directly to the target node",
        ));
    }
    let path = format!(
        "/api/v1/cluster/nodes/{}/restart-preparation",
        percent_encode_path_segment(node_id)
    );
    let response =
        cluster_api_request_method(server, token_file, &path, reqwest::Method::DELETE, None)
            .await?;
    let body = bounded_cli_response(response, 64 * 1024).await?;
    let canceled = serde_json::from_slice::<
        crate::http::cluster_operations::RestartPreparationResponse,
    >(&body)
    .map_err(|_| cli_exit(3, "restart cancellation returned an invalid response"))?;
    println!(
        "restart preparation canceled on {}; new admissions are enabled",
        canceled.node_id
    );
    Ok(())
}

fn percent_encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![char::from(byte)]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

async fn cluster_api_request(
    server: &str,
    token_file: Option<&std::path::Path>,
    path: &str,
) -> anyhow::Result<reqwest::Response> {
    cluster_api_request_method(server, token_file, path, reqwest::Method::GET, None).await
}

async fn cluster_api_request_method(
    server: &str,
    token_file: Option<&std::path::Path>,
    path: &str,
    method: reqwest::Method,
    body: Option<Vec<u8>>,
) -> anyhow::Result<reqwest::Response> {
    let token = read_owner_only_token(token_file)?;
    let mut base = reqwest::Url::parse(server)
        .map_err(|_| cli_exit(2, "--server must be an absolute http(s) origin"))?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || !matches!(base.path(), "" | "/")
    {
        return Err(cli_exit(2, "--server must be an absolute http(s) origin"));
    }
    base.set_path(path);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| cli_exit(2, "could not initialize the cluster HTTP client"))?;
    let mut request = client.request(method, base).bearer_auth(token);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let response = request.send().await.map_err(|_| {
        cli_exit(
            3,
            "cluster status is incomplete because the server is unreachable",
        )
    })?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    Err(cli_exit(
        if matches!(status.as_u16(), 401 | 403 | 404) {
            2
        } else {
            3
        },
        format!("cluster API refused the request with HTTP {status}"),
    ))
}

fn read_owner_only_token(token_file: Option<&std::path::Path>) -> anyhow::Result<String> {
    let path = token_file.ok_or_else(|| {
        cli_exit(
            2,
            "--token-file (or PLURX_ADMIN_TOKEN_FILE) is required; tokens are never accepted on the command line",
        )
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| cli_exit(2, format!("reading token file {}: {error}", path.display())))?;
    let metadata = file.metadata().map_err(|error| {
        cli_exit(
            2,
            format!("inspecting token file {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(cli_exit(
            2,
            "the token file must be a small regular file, not a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(cli_exit(
                2,
                "the token file must be owned by this user and readable only by its owner (mode 0600)",
            ));
        }
    }
    let mut token = String::new();
    file.take(16 * 1024 + 1)
        .read_to_string(&mut token)
        .map_err(|error| cli_exit(2, format!("reading token file {}: {error}", path.display())))?;
    if token.len() > 16 * 1024 {
        return Err(cli_exit(2, "the token file exceeds the 16 KiB bound"));
    }
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(cli_exit(
            2,
            "the token file does not contain one valid bearer token",
        ));
    }
    Ok(token.to_owned())
}

async fn bounded_cli_response(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| cli_exit(3, "cluster response ended before it was complete"))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(cli_exit(
                3,
                "cluster response exceeded the bounded size limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Resolve a configured relocation symlink once, before any store or cache
/// path is derived. Capability-style I/O deliberately rejects symlink
/// components; anchoring every daemon path to this canonical trusted root
/// preserves the common "data directory on another disk" deployment without
/// reopening per-request path traversal races.
fn canonicalize_configured_dir(path: &mut PathBuf, label: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(&*path)
        .with_context(|| format!("creating configured {label} directory {}", path.display()))?;
    *path = std::fs::canonicalize(&*path).with_context(|| {
        format!(
            "canonicalizing configured {label} directory {}",
            path.display()
        )
    })?;
    Ok(())
}

/// Resolve the optional shared-cache mount only when it is presently
/// available. A missing or failed shared mount must retain #538's node-local
/// fallback instead of preventing the daemon from serving.
fn canonicalize_available_shared_cache(config: &mut Config) {
    let path = &config.cluster.shared_cache_dir;
    if path.as_os_str().is_empty() {
        return;
    }
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return;
    };
    if std::fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
        config.cluster.shared_cache_dir = canonical;
    }
}

fn existing_shared_cache_roots(config: &Config) -> Vec<(&'static str, &std::path::Path)> {
    let path = config.cluster.shared_cache_dir.as_path();
    if !path.as_os_str().is_empty()
        && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
    {
        vec![("verified shared cache root", path)]
    } else {
        Vec::new()
    }
}

const TRANSCODE_SCRATCH_MARKER: &str = ".plurx-transcode-scratch";
const TRANSCODE_SCRATCH_MARKER_VERSION: &str = "plurx-transcode-scratch-v1";

fn canonicalize_storage_roots(config: &mut Config) -> anyhow::Result<crate::state::Dirs> {
    canonicalize_persistent_roots(config)?;
    canonicalize_available_shared_cache(config);
    if !config.storage.transcode_dir.as_os_str().is_empty() {
        canonicalize_configured_dir(&mut config.storage.transcode_dir, "transcode scratch")?;
    }
    // This dispatch preflight runs before `select_daemon_store` owns the
    // authoritative daemon lock. It may create/resolve roots and reject unsafe
    // overlap, but must not clear a live daemon's scratch.
    let shared_cache = existing_shared_cache_roots(config);
    let (dirs, _) = prepare_storage_dirs(&config.storage, false, &shared_cache, &[])?;
    if let Some((shared_label, shared_root)) = shared_cache.first().copied() {
        ensure_configured_roots_disjoint(
            "authoritative data root",
            &config.storage.data_dir,
            shared_label,
            shared_root,
        )?;
        if !config.storage.cache_dir.as_os_str().is_empty() {
            ensure_configured_roots_disjoint(
                "configured persistent cache root",
                &config.storage.cache_dir,
                shared_label,
                shared_root,
            )?;
        }
    }
    canonicalize_and_validate_credential_key(config, &dirs)?;
    Ok(dirs)
}

fn scratch_marker_contents(storage: &StorageConfig) -> String {
    format!(
        "{TRANSCODE_SCRATCH_MARKER_VERSION}\nowner={:?}\n",
        storage.data_dir.as_os_str()
    )
}

fn claim_explicit_scratch(
    storage: &StorageConfig,
    scratch: &std::path::Path,
    protected: &[plurx_core::fs_secure::FileIdentity],
) -> anyhow::Result<()> {
    let expected = scratch_marker_contents(storage);
    plurx_core::fs_secure::claim_and_clear_owned_scratch_with_protected_blocking(
        scratch,
        TRANSCODE_SCRATCH_MARKER,
        expected.as_bytes(),
        protected,
    )
    .with_context(|| {
        format!(
            "claiming and securely clearing transcode scratch {}",
            scratch.display()
        )
    })
}

fn canonicalized_managed_dir(path: &std::path::Path, label: &str) -> anyhow::Result<PathBuf> {
    let mut path = path.to_owned();
    canonicalize_configured_dir(&mut path, label)?;
    Ok(path)
}

fn ensure_configured_roots_disjoint(
    first_label: &str,
    first: &std::path::Path,
    second_label: &str,
    second: &std::path::Path,
) -> anyhow::Result<()> {
    let first_identity = plurx_core::fs_secure::directory_identity_nofollow_blocking(first)
        .with_context(|| format!("opening {first_label} identity {}", first.display()))?;
    let second_identity = plurx_core::fs_secure::directory_identity_nofollow_blocking(second)
        .with_context(|| format!("opening {second_label} identity {}", second.display()))?;
    let source_alias =
        plurx_core::fs_secure::directory_source_ancestry_alias_blocking(first, second)
            .with_context(|| {
                format!(
                    "proving {first_label} {} is source-disjoint from {second_label} {}",
                    first.display(),
                    second.display()
                )
            })?;
    anyhow::ensure!(
        !first.starts_with(second)
            && !second.starts_with(first)
            && !first_identity.same_inode(second_identity)
            && !source_alias,
        "{first_label} {} overlaps {second_label} {}; configured storage roots must be disjoint",
        first.display(),
        second.display()
    );
    Ok(())
}

fn ensure_distinct_storage_identities(
    scratch: &std::path::Path,
    persistent: &[(&str, &std::path::Path)],
    allow_scratch_inside_authority: bool,
) -> anyhow::Result<Vec<plurx_core::fs_secure::FileIdentity>> {
    let scratch_identity = plurx_core::fs_secure::directory_identity_nofollow_blocking(scratch)
        .with_context(|| format!("opening transcode scratch identity {}", scratch.display()))?;
    let mut identities = Vec::with_capacity(persistent.len());
    for (label, path) in persistent {
        let identity = plurx_core::fs_secure::directory_identity_nofollow_blocking(path)
            .with_context(|| format!("opening {label} identity {}", path.display()))?;
        anyhow::ensure!(
            !identity.same_inode(scratch_identity),
            "transcode scratch {} aliases {label} {}; cleanup roots must have distinct filesystem identities",
            scratch.display(),
            path.display()
        );
        anyhow::ensure!(
            !identities
                .iter()
                .any(|prior: &plurx_core::fs_secure::FileIdentity| prior.same_inode(identity)),
            "{label} {} aliases another authoritative or persistent storage root",
            path.display()
        );
        identities.push(identity);
    }
    let scratch_ancestors =
        plurx_core::fs_secure::directory_ancestor_identities_nofollow_blocking(scratch)
            .with_context(|| format!("walking transcode scratch ancestry {}", scratch.display()))?;
    for (index, ((label, path), identity)) in persistent.iter().zip(&identities).enumerate() {
        let ancestors =
            plurx_core::fs_secure::directory_ancestor_identities_nofollow_blocking(path)
                .with_context(|| format!("walking {label} ancestry {}", path.display()))?;
        anyhow::ensure!(
            !ancestors
                .iter()
                .any(|ancestor| ancestor.same_inode(scratch_identity)),
            "{label} {} is inside the transcode scratch identity {}; cleanup roots must be disjoint",
            path.display(),
            scratch.display()
        );
        if index != 0 || !allow_scratch_inside_authority {
            anyhow::ensure!(
                !scratch_ancestors
                    .iter()
                    .any(|ancestor| ancestor.same_inode(*identity)),
                "transcode scratch {} is inside {label} identity {}; cleanup roots must be disjoint",
                scratch.display(),
                path.display()
            );
        }
        if index != 0 || !allow_scratch_inside_authority {
            anyhow::ensure!(
                !plurx_core::fs_secure::directory_source_ancestry_alias_blocking(scratch, path)
                    .with_context(|| {
                        format!(
                            "proving transcode scratch {} is source-disjoint from {label} {}",
                            scratch.display(),
                            path.display()
                        )
                    })?,
                "transcode scratch {} and {label} {} overlap through filesystem source ancestry; bind-mounted cleanup roots must be disjoint",
                scratch.display(),
                path.display()
            );
        }
    }
    let data_ancestors =
        plurx_core::fs_secure::directory_ancestor_identities_nofollow_blocking(persistent[0].1)
            .with_context(|| {
                format!(
                    "walking authoritative data ancestry {}",
                    persistent[0].1.display()
                )
            })?;
    for ((label, path), identity) in persistent.iter().zip(&identities).skip(1) {
        anyhow::ensure!(
            !data_ancestors
                .iter()
                .any(|ancestor| ancestor.same_inode(*identity)),
            "authoritative data root {} is inside {label} identity {}; database and cache ownership must be disjoint",
            persistent[0].1.display(),
            path.display()
        );
    }
    Ok(identities)
}

fn prepare_storage_dirs(
    storage: &StorageConfig,
    clear_scratch: bool,
    extra_persistent: &[(&str, &std::path::Path)],
    extra_protected: &[plurx_core::fs_secure::FileIdentity],
) -> anyhow::Result<(crate::state::Dirs, Vec<plurx_core::fs_secure::FileIdentity>)> {
    let configured = configured_dirs(storage);
    let artwork = canonicalized_managed_dir(&configured.artwork, "artwork cache")?;
    let cache = canonicalized_managed_dir(&configured.cache, "transcode cache")?;
    let subs = canonicalized_managed_dir(&configured.subs, "subtitle cache")?;
    let runtime_cache =
        canonicalized_managed_dir(&configured.runtime_cache, "ffmpeg runtime cache")?;
    let renditions = canonicalized_managed_dir(&configured.renditions, "VOD rendition cache")?;
    let explicit_scratch = !storage.transcode_dir.as_os_str().is_empty();
    let transcode_identity = canonicalized_managed_dir(&configured.transcode, "transcode scratch")?;
    if !explicit_scratch {
        let metadata = std::fs::symlink_metadata(&configured.transcode).with_context(|| {
            format!(
                "inspecting legacy transcode scratch {}",
                configured.transcode.display()
            )
        })?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "legacy transcode scratch {} is a symlink; configure an explicit empty scratch root so plurx can bind and verify ownership",
            configured.transcode.display()
        );
    }

    anyhow::ensure!(
        !storage.data_dir.starts_with(&transcode_identity),
        "transcode scratch {} contains authoritative data root {}; choose a child or separate directory",
        transcode_identity.display(),
        storage.data_dir.display()
    );
    for persistent in [&artwork, &cache, &subs, &runtime_cache, &renditions] {
        anyhow::ensure!(
            !storage.data_dir.starts_with(persistent),
            "authoritative data root {} is inside managed persistent cache path {}; database and cache ownership must be disjoint",
            storage.data_dir.display(),
            persistent.display()
        );
        anyhow::ensure!(
            !persistent.starts_with(&transcode_identity),
            "transcode scratch {} contains persistent cache path {}; startup would erase durable bytes",
            transcode_identity.display(),
            persistent.display()
        );
        anyhow::ensure!(
            !transcode_identity.starts_with(persistent),
            "transcode scratch {} is inside persistent cache path {}; cleanup and cache sweeping must use disjoint roots",
            transcode_identity.display(),
            persistent.display()
        );
    }
    let mut persistent = vec![
        ("authoritative data root", storage.data_dir.as_path()),
        ("artwork cache", artwork.as_path()),
        ("transcode cache", cache.as_path()),
        ("subtitle cache", subs.as_path()),
        ("ffmpeg runtime cache", runtime_cache.as_path()),
        ("VOD rendition cache", renditions.as_path()),
    ];
    if !storage.cache_dir.as_os_str().is_empty() {
        persistent.push((
            "configured persistent cache root",
            storage.cache_dir.as_path(),
        ));
    }
    persistent.extend_from_slice(extra_persistent);
    #[cfg(windows)]
    {
        plurx_core::fs_secure::require_mutable_volume_blocking(&transcode_identity).with_context(
            || {
                format!(
                    "checking mutable Windows volume for transcode scratch {}",
                    transcode_identity.display()
                )
            },
        )?;
        for (label, path) in &persistent {
            plurx_core::fs_secure::require_mutable_volume_blocking(path).with_context(|| {
                format!(
                    "checking mutable Windows volume for {label} {}",
                    path.display()
                )
            })?;
        }
    }
    let mut protected =
        ensure_distinct_storage_identities(&transcode_identity, &persistent, !explicit_scratch)?;
    protected.extend_from_slice(extra_protected);
    if explicit_scratch && clear_scratch {
        claim_explicit_scratch(storage, &transcode_identity, &protected)?;
    }
    Ok((
        crate::state::Dirs {
            artwork,
            transcode: if explicit_scratch {
                transcode_identity
            } else {
                configured.transcode
            },
            cache,
            subs,
            runtime_cache,
            renditions,
        },
        protected,
    ))
}

fn canonicalize_persistent_roots(config: &mut Config) -> anyhow::Result<()> {
    canonicalize_configured_dir(&mut config.storage.data_dir, "data")?;
    if !config.storage.cache_dir.as_os_str().is_empty() {
        canonicalize_configured_dir(&mut config.storage.cache_dir, "persistent cache")?;
    }
    Ok(())
}

fn canonicalize_and_validate_credential_key(
    config: &mut Config,
    dirs: &crate::state::Dirs,
) -> anyhow::Result<()> {
    if !config.cluster.credential_key_file.as_os_str().is_empty() {
        let configured = config.cluster.credential_key_file.clone();
        let name = configured
            .file_name()
            .context("configured credential key path has no filename")?;
        let parent = configured
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let parent = std::fs::canonicalize(parent).with_context(|| {
            format!(
                "canonicalizing configured credential key parent {}",
                parent.display()
            )
        })?;
        config.cluster.credential_key_file = parent.join(name);
    }
    let key = config.cluster.credential_key_path(&config.storage.data_dir);
    let mut forbidden = vec![
        ("transcode scratch", dirs.transcode.as_path()),
        ("artwork cache", dirs.artwork.as_path()),
        ("transcode cache", dirs.cache.as_path()),
        ("subtitle cache", dirs.subs.as_path()),
    ];
    if !config.storage.cache_dir.as_os_str().is_empty() {
        forbidden.push((
            "configured persistent cache root",
            config.storage.cache_dir.as_path(),
        ));
    }
    if !config.cluster.shared_cache_dir.as_os_str().is_empty() {
        forbidden.push((
            "verified shared cache root",
            config.cluster.shared_cache_dir.as_path(),
        ));
    }
    for (label, root) in forbidden {
        anyhow::ensure!(
            !key.starts_with(root),
            "authoritative credential key {} is inside {label} {}; choose durable storage outside all managed cache and scratch roots",
            key.display(),
            root.display()
        );
    }
    Ok(())
}

/// Re-fetch provider-backed metadata through the activated replicated store.
/// The command refuses a legacy-only data directory because only `run` owns
/// the one-time import and activation sequence.
async fn refresh_metadata(config: &mut Config, library_id: Option<i64>) -> anyhow::Result<()> {
    // Maintenance must resolve and validate exactly the same storage topology
    // as daemon startup. This remains non-destructive: scratch cleanup is
    // reserved for the lock-owning `run` path.
    let dirs = canonicalize_storage_roots(config)?;
    let store = connect_activated_store(config).await.context(
        "connecting to the activated store: an unmigrated data directory needs \
             `plurxd run` to import it once, and an activated one needs that daemon \
             running because maintenance commands share its voter rather than \
             opening a second store",
    )?;
    let cluster_id = store.instance_id().await?;
    let identity = plurx_core::cluster::initialize_identity(&config.storage.data_dir, &cluster_id)?;
    let artwork_dir = dirs.artwork;
    std::fs::create_dir_all(&artwork_dir)?;
    // A maintenance command runs the same cluster-wide singleton pass a
    // scheduled job would, so it is subject to the same rule. This process has
    // no Raft handle of its own, so the durable admission record in
    // membership.json is what it has; it can only refuse, never grant.
    let authority = crate::job_lease::AdmittedRoleJobAuthority::new(
        plurx_core::cluster::migration::local_cluster_role(&config.storage.data_dir)?,
    );
    refresh_metadata_with_store(
        store,
        &artwork_dir,
        library_id,
        &identity.node_id,
        &authority,
    )
    .await
}

/// What an operator sees when `plurxd refresh-metadata` cannot take the
/// cluster-wide artwork lease.
///
/// A named constant rather than an inline literal because the sentence is the
/// deliverable — it is the only thing the operator gets — and it shipped once
/// with two runs of fourteen spaces in it, from continuation lines that had
/// lost their trailing `\`. Nothing could assert on it while it was inline, so
/// nothing did; [`the_cluster_refusals_read_as_sentences`] asserts on it here.
const ARTWORK_PASS_UNAVAILABLE: &str =
    "the artwork provider pass is not available on this node: it is either active on \
     another cluster node, or this node was admitted as a learner and never runs \
     cluster-wide provider work";

async fn refresh_metadata_with_store(
    store: Arc<dyn Store>,
    artwork_dir: &std::path::Path,
    library_id: Option<i64>,
    node_id: &str,
    authority: &dyn plurx_core::cluster::coordination::ClusterJobAuthority,
) -> anyhow::Result<()> {
    let libraries = store.list_libraries().await?;

    if let Some(id) = library_id {
        anyhow::ensure!(
            libraries.iter().any(|library| library.id == id),
            "no library with id {id}"
        );
    }

    let tmdb_key = store.get_setting(keys::TMDB_API_KEY).await?;
    let coordinator = StoreCoordinator::new(Arc::clone(&store), node_id.to_owned())?;
    let Some(lease) =
        acquire_cluster_job(&coordinator, authority, "provider:artwork".to_owned()).await?
    else {
        anyhow::bail!(ARTWORK_PASS_UNAVAILABLE);
    };
    let lost = lease.loss_token();
    let publisher = lease.publisher(store.as_ref());
    let result: anyhow::Result<()> = tokio::select! {
        result = async {
            for library in libraries
                .into_iter()
                .filter(|library| library_id.is_none_or(|id| library.id == id))
            {
                match library.kind {
                    LibraryKind::Books | LibraryKind::Home | LibraryKind::Recordings => {
                        println!("{}: skipped provider artwork", library.name);
                    }
                    LibraryKind::Shows if library.anime => {
                        let report = metadata::enrich_anime_library_with_publication(
                            &publisher,
                            &AniListClient::new(),
                            artwork_dir,
                            library.id,
                            true,
                            None,
                            None,
                        )
                        .await;
                        println!("{}: {}", library.name, serde_json::to_string(&report)?);
                    }
                    LibraryKind::Movies | LibraryKind::Shows => {
                        let key = tmdb_key
                            .as_deref()
                            .filter(|key| !key.is_empty())
                            .context("TMDB API key is not configured")?;
                        let report = metadata::enrich_library_with_publication(
                            &publisher,
                            &TmdbClient::new(key),
                            artwork_dir,
                            Some(library.id),
                            true,
                            None,
                        )
                        .await;
                        println!("{}: {}", library.name, serde_json::to_string(&report)?);
                    }
                }
            }
            Ok(())
        } => result,
        () = lost.cancelled() => Err(anyhow::anyhow!(
            "artwork provider pass stopped after losing its cluster lease"
        )),
    };
    drop(publisher);
    let _ = lease.release().await;
    result
}

const RESET_PASSWORD_REQUIRES_DAEMON_CONTROL: &str =
    "password reset is unavailable: a direct replicated Store write cannot invalidate the \
     running daemons' cache-only admin proofs; use an authenticated admin session to reset \
     another user, and do not bypass this refusal with a direct database write";

/// The former console path connected directly to the replicated Store beside
/// the daemon. That write could commit without invalidating any process-local
/// recovery proof, so fail before opening the Store until a daemon-owned,
/// authenticated local control endpoint exists.
async fn reset_password(
    _config: &Config,
    _username: &str,
    _password: Option<String>,
) -> anyhow::Result<()> {
    anyhow::bail!(RESET_PASSWORD_REQUIRES_DAEMON_CONTROL)
}

async fn run(config: Config) -> anyhow::Result<()> {
    let logs = init_logging();
    // After the subscriber and before the first task: a panic reported before
    // there is anywhere to report it to reaches only stderr.
    panics::install_panic_hook();
    report_open_file_limit(plurx_core::process::rlimit::raise_open_file_limit());
    // Signal streams must exist before store activation, system probing, or
    // any listener can make this process externally reachable. Installing them
    // is only half of it: nothing polls that future until `serve` first polls
    // its graceful-drain argument, so watching the streams from here is what
    // lets the startup stages below answer a signal instead of finishing every
    // one of them first.
    let shutdown = ShutdownWatch::observing(shutdown_signal());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let desktop = desktop_control::DesktopControl::from_environment()?;

    let observation = Arc::new(StartupObservationHttp::new(config.server.bind));
    let mut selected =
        match select_daemon_store_observing(&config, Some(observation.as_ref())).await {
            Ok(selected) => selected,
            Err(error) => {
                observation.stop_and_drain().await;
                return Err(error).with_context(|| {
                    format!("selecting store in {}", config.storage.data_dir.display())
                });
            }
        };
    // Which backend is serving is not otherwise observable. A recovery boot
    // binds and serves normally on unreplicated SQLite, so a persistently
    // failing activation otherwise looks only like a flapping container, with
    // nothing saying which boots were replicated and which were not.
    match selected.backend {
        SelectedBackend::Replicated => {
            tracing::info!(target: "plurx::cluster", "durable state: replicated store");
        }
        SelectedBackend::SqliteRecovery => tracing::warn!(target: "plurx::cluster",
            "durable state: unreplicated SQLite, recovering from an interrupted activation; \
             this boot is not replicated and the next restart retries the import"
        ),
    }
    trigger_shutdown_registration_failpoint("after-store-activation");

    let serving = async {
        // `select_daemon_store` renames directories and fsyncs activation
        // markers in a fixed order, so it is the one startup stage that has to
        // finish rather than be cancelled midway. Answer the signal at the
        // boundary just after it instead: no listener exists yet, so stopping
        // here is indistinguishable from never having been asked to start.
        if shutdown.is_signalled() {
            tracing::info!("shutdown signal received during store activation; not serving");
            return Ok(());
        }
        let source_layout_ready = Box::pin(selected.prepare_source_schema_before_serving())
            .await
            .context("qualifying Source schema before serving")?;
        if !source_layout_ready {
            tracing::info!(target:"plurx::sharing","Source schema readiness pending; Local serving continues");
        }
        let store = Arc::clone(&selected.store);
        let replication = selected.replication_monitor();
        let membership = selected.membership_manager();
        let catalogue = selected.catalogue_reader();
        let dirs = create_dirs_for_config(&config)?;
        // Probing only measures ffmpeg and the host, so cancelling it leaves
        // nothing half-written. Racing it is what keeps `docker stop` during a
        // slow probe from waiting out its grace period while this process
        // finishes measuring hardware it is about to stop using.
        let probed = tokio::select! {
            biased;
            () = shutdown.clone().signalled() => None,
            probed = probe_system(&config, &store, &selected.identity.node_id, &dirs.transcode, &dirs.runtime_cache) => Some(probed?),
        };
        let Some((encoder_caps, system)) = probed else {
            tracing::info!("shutdown signal received during system probing; not serving");
            return Ok(());
        };
        let parts = Boot {
            source_activation_at_boot: Some(source_layout_ready),
            store,
            replication,
            membership,
            catalogue,
            identity: selected.identity.clone(),
            credential_key: Arc::clone(&selected.credential_key),
            backup_client: selected.local_client(),
            dirs,
            encoder_caps,
            system,
            logs,
        };
        boot_observing(
            config,
            parts,
            start_mdns_advertiser,
            shutdown.signalled(),
            Some(Arc::clone(&observation)),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            desktop.clone(),
        )
        .await
    };
    let result = serving.await;
    observation.stop_and_drain().await;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let native_commit = if let Some(desktop) = &desktop {
        desktop.is_committed().await
    } else {
        false
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let native_commit = false;
    let shutdown = if native_commit {
        match tokio::time::timeout(Duration::from_secs(30), selected.shutdown()).await {
            Ok(result) => result.context("stopping local Hiqlite voter"),
            Err(_) => Err(anyhow::anyhow!(
                "local Hiqlite store close timed out during committed native shutdown"
            )),
        }
    } else {
        selected
            .shutdown()
            .await
            .context("stopping local Hiqlite voter")
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(desktop) = desktop {
        desktop.finish(shutdown.is_ok()).await?;
    }
    result.and(shutdown)
}

/// What a measured node hands to the server it is about to become.
struct Boot {
    source_activation_at_boot: Option<bool>,
    store: Arc<dyn plurx_core::store::Store>,
    catalogue: plurx_core::store::CatalogueReader,
    replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    membership: plurx_core::cluster::membership::MembershipManager,
    identity: plurx_core::cluster::ClusterIdentity,
    /// Resolved before the store is handed on, so a node that cannot open its
    /// existing Trakt rows fails here rather than at the first sync.
    credential_key: Arc<plurx_core::secrets::CredentialKey>,
    backup_client: Option<hiqlite::Client>,
    dirs: crate::state::Dirs,
    encoder_caps: plurx_core::transcode::EncoderCaps,
    system: SystemInfo,
    logs: logbuf::LogBuffers,
}

/// One public socket owner spanning clock-only observation and normal HTTP.
/// Routing is selected per request, so already-open keepalive connections
/// cannot retain the pending router after activation.
struct StartupObservationHttp {
    bind: SocketAddr,
    router: Arc<std::sync::RwLock<axum::Router>>,
    tasks: tokio::sync::Mutex<Option<StartupObservationTasks>>,
    stop: tokio_util::sync::CancellationToken,
}

struct StartupObservationTasks {
    server: tokio::task::JoinHandle<anyhow::Result<HttpDrain>>,
    observer: crate::clock_offset::ClockObserver,
    probe: Option<tokio::task::JoinHandle<()>>,
    probe_stop: tokio_util::sync::CancellationToken,
}

impl StartupObservationHttp {
    fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            router: Arc::new(std::sync::RwLock::new(axum::Router::new())),
            tasks: tokio::sync::Mutex::new(None),
            stop: tokio_util::sync::CancellationToken::new(),
        }
    }

    async fn stop_probe(&self) {
        let mut tasks = self.tasks.lock().await;
        if let Some(tasks) = tasks.as_mut() {
            tasks.probe_stop.cancel();
            if let Some(mut probe) = tasks.probe.take() {
                if tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, &mut probe)
                    .await
                    .is_err()
                {
                    probe.abort();
                    let _ = probe.await;
                }
            }
        }
    }

    async fn observer(&self) -> Option<crate::clock_offset::ClockObserver> {
        self.tasks
            .lock()
            .await
            .as_ref()
            .map(|tasks| tasks.observer.clone())
    }

    async fn stop_and_drain(&self) {
        self.stop.cancel();
        self.stop_probe().await;
        if let Some(mut tasks) = self.tasks.lock().await.take() {
            if tokio::time::timeout(
                SHUTDOWN_DRAIN_TIMEOUT + Duration::from_secs(1),
                &mut tasks.server,
            )
            .await
            .is_err()
            {
                tasks.server.abort();
                let _ = tasks.server.await;
            }
        }
    }

    async fn serve_normal(
        &self,
        app: axum::Router,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> anyhow::Result<HttpDrain> {
        *self
            .router
            .write()
            .map_err(|_| anyhow::anyhow!("startup router ownership poisoned"))? = app;
        let mut tasks = self
            .tasks
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow::anyhow!("startup observation socket was not installed"))?;
        tokio::pin!(shutdown);
        let result = tokio::select! {
            result = &mut tasks.server => result.context("startup HTTP task").and_then(|result| result),
            () = &mut shutdown => {
                self.stop.cancel();
                (&mut tasks.server).await.context("startup HTTP task").and_then(|result| result)
            }
        };
        self.stop.cancel();
        tasks.probe_stop.cancel();
        if let Some(mut probe) = tasks.probe.take() {
            if tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, &mut probe)
                .await
                .is_err()
            {
                probe.abort();
                let _ = probe.await;
            }
        }
        result
    }
}

/// Seconds a client is told to wait before asking a still-starting server again.
const STARTUP_PENDING_RETRY_SECS: u64 = 2;

/// The page a browser gets while the server is still starting: it says so and
/// reloads itself, so a tab opened during startup lands in the app on its own.
const STARTUP_PENDING_HTML: &str = concat!(
    "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
    "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
    "<meta http-equiv=\"refresh\" content=\"3\"><title>Starting…</title>",
    "<style>body{margin:0;min-height:100vh;display:grid;place-items:center;",
    "background:#111;color:#ddd;font:16px/1.5 system-ui,sans-serif}",
    "main{max-width:32em;padding:24px;text-align:center}h1{font-size:22px;margin:0 0 8px}",
    "p{margin:0;color:#aaa}</style></head><body><main role=\"status\">",
    "<h1>The server is starting</h1>",
    "<p>This page reloads by itself when it is ready.</p></main></body></html>",
);

/// Every route but the clock observation while the store is still coming up.
///
/// The listener is bound before the store is selected so a joining learner can
/// answer clock observation, which leaves a window — seconds on a standalone
/// server, as long as a snapshot transfer on a cluster node — in which the
/// socket accepts connections the application cannot serve yet. Axum's default
/// fallback answered those with an empty 404: a browser saw a blank page,
/// `/readyz` said "not found" rather than "not ready", and a client probing
/// `/api/v1/server` was told there was no plurx server here. A 503 with
/// Retry-After is the truthful answer, and it carries the typed code every
/// client already dispatches on.
async fn startup_pending(headers: axum::http::HeaderMap) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    let browser = headers
        .get(header::ACCEPT)
        .and_then(|accept| accept.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));
    let response = if browser {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            STARTUP_PENDING_HTML,
        )
            .into_response()
    } else {
        http::error::ApiError::TypedRetry {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "server_starting",
            message: "the server is starting; retry shortly".into(),
            retry_after_seconds: STARTUP_PENDING_RETRY_SECS,
        }
        .into_response()
    };
    startup_pending_headers(response)
}

/// `/readyz` keeps its own contract while pending: plain text, 503, and a
/// one-word reason (API.md §22.1), so status tables that print the body show
/// `starting` rather than a JSON object.
async fn startup_pending_readyz() -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    startup_pending_headers(
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "starting\n",
        )
            .into_response(),
    )
}

fn startup_pending_headers(mut response: axum::response::Response) -> axum::response::Response {
    use axum::http::{header, HeaderValue};
    let headers = response.headers_mut();
    headers.insert(
        header::RETRY_AFTER,
        HeaderValue::from(STARTUP_PENDING_RETRY_SECS),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl Drop for StartupObservationHttp {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl StartupClockObserver for StartupObservationHttp {
    fn start(
        &self,
        membership: plurx_core::cluster::membership::MembershipManager,
        node_id: String,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(), plurx_core::error::StoreError>> + Send + '_,
        >,
    > {
        Box::pin(async move {
            let mut tasks = self.tasks.lock().await;
            if tasks.is_some() {
                return Err(plurx_core::error::StoreError::Database(
                    "clock observation socket already owned".into(),
                ));
            }
            let listener = bind_listener(self.bind)
                .await
                .map_err(|error| plurx_core::error::StoreError::Database(error.to_string()))?;
            let observer = crate::clock_offset::ClockObserver::new(membership.clone());
            let pending = axum::Router::new()
                .route(
                    http::internal_clock::PATH,
                    axum::routing::get(http::internal_clock::observation_snapshot),
                )
                .route("/readyz", axum::routing::get(startup_pending_readyz))
                .with_state(http::internal_clock::ObservationContext {
                    membership: membership.clone(),
                    node_id,
                    observer: observer.clone(),
                })
                .fallback(startup_pending);
            *self.router.write().map_err(|_| {
                plurx_core::error::StoreError::Database("startup router ownership poisoned".into())
            })? = pending;
            let router = Arc::clone(&self.router);
            let dispatch = axum::Router::new().fallback(axum::routing::any(
                move |request: axum::http::Request<axum::body::Body>| {
                    let router = Arc::clone(&router);
                    async move {
                        let app = router
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        match tower::ServiceExt::oneshot(app, request).await {
                            Ok(response) => response,
                            Err(never) => match never {},
                        }
                    }
                },
            ));
            let stop = self.stop.clone();
            let server = tokio::spawn(serve_http(
                listener,
                dispatch,
                async move { stop.cancelled().await },
                HTTP_TIMEOUTS,
            ));
            let probe_stop = self.stop.child_token();
            let probe_owner = observer.clone();
            let probe_shutdown = probe_stop.clone();
            let probe = tokio::spawn(async move { probe_owner.run(probe_shutdown).await });
            *tasks = Some(StartupObservationTasks {
                server,
                observer,
                probe: Some(probe),
                probe_stop,
            });
            Ok(())
        })
    }
}

/// Everything from a measured node to a served, drained shutdown.
///
/// Separated from [`run`] at exactly the line where startup stops reaching
/// outside the process: above it are the store and the ffmpeg probes, below it
/// is wiring. Taking the shutdown future and the advertiser rather than
/// installing a signal handler and registering on the LAN is what lets the
/// whole sequence run in a test, on a temporary directory and a loopback port.
#[cfg(test)]
async fn boot(
    config: Config,
    parts: Boot,
    advertiser: MdnsAdvertiser,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    boot_observing(
        config,
        parts,
        advertiser,
        shutdown,
        None,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        None,
    )
    .await
}

async fn boot_observing(
    mut config: Config,
    parts: Boot,
    advertiser: MdnsAdvertiser,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    observation: Option<Arc<StartupObservationHttp>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))] desktop: Option<
        Arc<desktop_control::DesktopControl>,
    >,
) -> anyhow::Result<()> {
    let observation = match observation {
        Some(owner) if owner.tasks.lock().await.is_some() => Some(owner),
        _ => None,
    };
    let prepared_https = https::Prepared::open(&config).await?;
    let Boot {
        source_activation_at_boot,
        store,
        replication,
        membership,
        catalogue,
        identity,
        credential_key,
        backup_client,
        dirs,
        encoder_caps,
        system,
        logs,
    } = parts;
    // `server.name` predates clustering as a node-local config value. Seed it
    // once so an existing installation keeps its name on upgrade, then use the
    // replicated winner on every voter. A joining node's config can therefore
    // never rename the logical server accidentally.
    config.server.name = store
        .get_or_init_setting(keys::SERVER_NAME, &config.server.name)
        .await
        .context("initializing replicated server name")?;
    // "Sign-ins expire" defaults to on; this is the moment it takes effect on
    // an upgraded server, and no device's idle window starts before it.
    let expiry_since =
        http::users::start_token_expiry_clock(store.as_ref(), http::users::unix_now())
            .await
            .context("starting the sign-in expiry clock")?;
    tracing::debug!(expiry_since, "sign-in expiry clock");
    log_startup(&config, &identity);

    let purpose_master = Arc::clone(&credential_key);
    let instance_id = identity.cluster_id;
    let node_id = identity.node_id;
    let mut state = build_state(
        &config,
        node_id.clone(),
        instance_id.clone(),
        credential_key,
        backup_client,
        replication,
        membership,
        catalogue,
        store,
        dirs,
        encoder_caps,
        system,
        logs,
    );
    state.https = Some(Arc::new(https::Control::new(
        node_id.clone(),
        config.clone(),
        prepared_https
            .as_ref()
            .map(|prepared| prepared.identity.clone()),
    )?));
    state
        .sharing
        .observe_source_activation_at_boot(source_activation_at_boot);
    if let Some(owner) = &observation {
        let observer = owner.observer().await.ok_or_else(|| {
            anyhow::anyhow!("startup observation owner disappeared before activation")
        })?;
        anyhow::ensure!(
            observer.belongs_to(&state.membership),
            "foreign startup clock owner"
        );
        state.clock_observer = observer;
    }
    state
        .membership
        .set_ingress_custody_boot(Some(state.sharing.accepted_drivers.boot_id()));
    state
        .membership
        .publish_ingress_custody_boot()
        .await
        .context("publishing actual accepted-driver custody boot")?;
    state
        .membership
        .prepare_purpose_master(purpose_master)
        .await
        .context("qualifying coordinated sharing purpose master")?;
    match tokio::time::timeout(
        Duration::from_secs(5),
        state.membership.coordinate_purpose_keys(),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(code = error.code(), "sharing purpose factory is pending"),
        Err(_) => tracing::warn!(code = "deadline", "sharing purpose factory is pending"),
    }

    // The stored request is not an ffmpeg contract. Exercise the exact
    // production rate-control arguments against this boot's real drivers and
    // publish only the effective result before any session can start.
    state.transcode.initialize_rate_control().await?;
    state.live_tv.start_caption_probe();
    // Which artifact identity this node plans into, before anything can plan,
    // and the only time it is decided. It is part of every cache key the node
    // computes, so moving it on a live node would move the key space under
    // work already running; the request is stored when written and read here.
    state.transcode.publish_artifact_qualification().await;
    // Recovery authorization changes no artifact identity, but it still has
    // to be published before the listener accepts the first session.
    state.transcode.publish_automatic_decoder_recovery().await;
    state.transcode.publish_macos_video_processing().await;
    // One bounded telemetry writer owns all node-local event persistence.
    // Register it before the listener can accept the first producer.
    crate::telemetry::initialize(Arc::clone(&state.store))
        .await
        .context("seed playback telemetry settings")?;
    // The Plex façade census continues across restarts (C-07 §8.5). Restored
    // before the listener accepts; an unusable file is a new census, never a
    // refusal to start.
    state
        .plex_census
        .restore_durable(
            &config.storage.data_dir,
            http::plex_census::unix_now_s(),
            http::plex_census::build_source_floor().unwrap_or(0),
        )
        .await;
    let plex_census = Arc::clone(&state.plex_census);
    let background_loops = BackgroundLoopGuard::new();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let native_managed = desktop.is_some();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let native_managed = false;
    tokio::spawn(sharing_host::run(
        state.clone(),
        background_loops.token(),
        native_managed,
    ));
    spawn_background_loops(&state, background_loops.token());

    let progress = Arc::clone(&state.progress);
    let telemetry_store = Arc::clone(&state.store);
    let leave_shutdown = state.shutdown.clone();
    let live_tv_shutdown = Arc::clone(&state.live_tv);
    let serving_shutdown = state.serving.clone();
    let jellyfin_standard_port = Arc::clone(&state.jellyfin_standard_port);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let desktop_server = if let Some(desktop) = &desktop {
        let bind = config.server.bind;
        let ip = match bind.ip() {
            std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            }
            std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            }
            ip => ip,
        };
        Some(
            desktop
                .serve(
                    state.clone(),
                    format!("http://{}", SocketAddr::new(ip, bind.port())),
                )
                .await?,
        )
    } else {
        None
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let desktop_shutdown = desktop.clone();
    let app = http::router(state);
    tokio::task::spawn_blocking(http::web::warm_static_assets)
        .await
        .context("precompute embedded web assets")?;
    let listener = if observation.is_none() {
        Some(bind_listener(config.server.bind).await?)
    } else {
        None
    };
    let public_listener_stop = tokio_util::sync::CancellationToken::new();
    let https_serving = https::Serving::start(
        prepared_https,
        app.clone(),
        public_listener_stop.clone(),
        leave_shutdown.clone(),
    );
    #[cfg(windows)]
    crate::windows_service::report_listener_ready()?;
    trigger_shutdown_registration_failpoint("after-listener-bind");
    let discovery_node_id =
        (!config.cluster.advertise_host.trim().is_empty()).then_some(node_id.as_str());
    let mdns = start_discovery(&config, &instance_id, discovery_node_id, advertiser);
    let jellyfin = JellyfinStandardListener::open(
        config.server.jellyfin_bind(),
        &app,
        &jellyfin_standard_port,
    )
    .await;
    let jellyfin_stop = jellyfin.stop_token();

    let shutdown = async move {
        tokio::select! {
            () = shutdown => {}
            () = leave_shutdown.cancelled() => {
                tracing::info!("local shutdown requested, draining");
            }
        }
        // One drain token for both causes: owners that must hand work back
        // before exit (shared session retirement) observe `state.shutdown`,
        // and a signal is as much a drain as a committed leave.
        leave_shutdown.cancel();
        // Stop new media starts before waiting for tuner/FFmpeg ownership to
        // settle. The HTTP server remains alive during this short phase so
        // existing close/resource requests can finish normally.
        let expires = crate::media_sessions::unix_ms()
            .saturating_add(10_000)
            .try_into()
            .unwrap_or(u64::MAX);
        let _ = serving_shutdown
            .begin_restart_preparation_until(expires)
            .await;
        crate::telemetry::drain_for_shutdown(&telemetry_store).await;
        if let Err(error) = live_tv_shutdown.shutdown().await {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if let Some(desktop) = &desktop_shutdown {
                desktop.record_auxiliary_failure();
            }
            tracing::warn!(%error, "Live TV shutdown could not confirm complete cleanup");
        }
        // The main listener stops accepting when this future completes;
        // the Jellyfin standard port stops at the same moment.
        public_listener_stop.cancel();
        jellyfin_stop.cancel();
    };
    let served = if let Some(owner) = observation {
        serve_observed(
            owner,
            app,
            jellyfin,
            https_serving,
            progress,
            mdns,
            shutdown,
        )
        .await
    } else {
        serve(
            listener.expect("normal startup owns its listener"),
            app,
            jellyfin,
            https_serving,
            progress,
            mdns,
            shutdown,
        )
        .await
    };
    // The last census write. It is marked clean only when the HTTP server
    // stopped with every connection drained: then no façade request can still
    // be counted, and the clean mark tells the next start that nothing was
    // lost. A drain that timed out leaves connections that may still finish
    // and be counted after this write, and a serving error says nothing about
    // them, so both write an unclean final record and the next start counts an
    // unclean stop. A failed write is read the same way, which is honest too.
    let clean = matches!(served, Ok(HttpDrain::Complete));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(server) = desktop_server {
            if let Err(error) = server.finish().await {
                if let Some(desktop) = &desktop {
                    desktop.record_auxiliary_failure();
                }
                tracing::warn!(%error, "native IPC did not drain cleanly");
            }
        }
        if let Some(desktop) = &desktop {
            desktop.record_http_drain(clean);
        }
    }
    if let Err(error) = plex_census
        .flush_durable(http::plex_census::unix_now_s(), clean)
        .await
    {
        tracing::warn!(%error, "could not record the Plex façade census at shutdown");
    }
    served.map(|_| ())
}

/// How a Bonjour record gets published. A parameter rather than a direct call
/// because registering one puts a service on the local network, which a test
/// must not do to check that discovery is best-effort.
type MdnsAdvertiser =
    fn(&str, &str, &str, Option<&str>, SocketAddr, &str) -> anyhow::Result<mdns_sd::ServiceDaemon>;

/// Widen the inherited open-file limit and say what it now is.
///
/// `#[tokio::main]` builds the runtime before any of `main`'s body runs, so
/// there is no "before the runtime starts" to run this in. What matters is
/// that the limit is consulted when a descriptor is opened, and this runs
/// before the store, the system probe and every listener open theirs.
///
/// A limit that cannot be raised is reported and is not fatal: the daemon
/// served on the inherited soft limit before this call existed, and refusing
/// to boot over a resource limit would be a worse failure than the
/// exhaustion it guards against.
fn report_open_file_limit(
    outcome: std::io::Result<Option<plurx_core::process::rlimit::OpenFileLimit>>,
) {
    match outcome {
        Ok(Some(limit)) => tracing::info!(
            "open files: soft {} -> {} (hard {})",
            limit.soft_before,
            limit.soft_after,
            limit.hard
        ),
        // A platform without POSIX resource limits, which is the Windows
        // service. There is no number to report.
        Ok(None) => {}
        Err(error) => tracing::warn!(
            "raising the soft open-file limit failed ({error}); continuing on the inherited limit"
        ),
    }
}

/// Console logging plus separate bounded rings for general and cluster events.
///
/// Cluster INFO/DEBUG detail belongs on Settings → Cluster and is omitted from
/// both the general ring and stdout. WARN/ERROR still reach stdout so a broken
/// voter remains visible to service supervisors before an admin can sign in.
/// The EnvFilter remains global, so `PLURX_LOG` governs every sink.
fn init_logging() -> logbuf::LogBuffers {
    use tracing_subscriber::util::SubscriberInitExt;

    let logs = logbuf::LogBuffers::default();
    logging_subscriber(
        &logs,
        EnvFilter::try_from_env("PLURX_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        LogFormat::from_env(),
        std::io::stdout().is_terminal(),
        std::io::stdout,
    )
    .try_init()
    .ok();
    logs
}

/// How the console sink is formatted.
///
/// `PLURX_LOG_FORMAT=json|text`, default text. An environment variable rather
/// than a replicated setting, beside `PLURX_LOG`, because it describes how
/// *this process's* stdout is consumed: a node whose journald is scraped by a
/// log pipeline needs JSON and its neighbour may not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    fn from_env() -> Self {
        Self::parse(std::env::var("PLURX_LOG_FORMAT").ok().as_deref())
    }

    /// Anything that is not `json` is text, including an unset variable and a
    /// misspelling. A log format is not worth refusing a boot over.
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some(value) if value.trim().eq_ignore_ascii_case("json") => Self::Json,
            _ => Self::Text,
        }
    }
}

/// The subscriber `init_logging` installs, built separately so its format and
/// its ANSI decision can be exercised against a captured writer rather than
/// against the process's real stdout.
///
/// The filter is a parameter rather than read from `PLURX_LOG` in here: a test
/// that has to reach into the environment to decide whether its own subscriber
/// will emit anything is a test that fails when a neighbouring test sets the
/// same variable, which is exactly what happened.
///
/// `with_ansi` is set explicitly because `tracing-subscriber` does not check
/// for a terminal: `fmt_layer.rs` turns ANSI on whenever the `ansi` feature is
/// compiled in and `NO_COLOR` is unset, with no TTY detection anywhere, so a
/// systemd unit or a container gets escape bytes in its journal unless the
/// operator knows to set `NO_COLOR`. Under `json` it is forced off regardless,
/// because an escape sequence inside a JSON string field is not a colour, it
/// is a parse hazard.
fn logging_subscriber<W>(
    logs: &logbuf::LogBuffers,
    filter: EnvFilter,
    format: LogFormat,
    ansi: bool,
    writer: W,
) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    use tracing_subscriber::filter::filter_fn;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    // Boxed, and built separately per arm. `fmt::Layer` carries the subscriber
    // type it will be layered onto, so one shared builder reused by both arms
    // would tie the text and JSON layers to the same position in the stack and
    // neither would compile.
    let console: Box<dyn Layer<_> + Send + Sync> = match format {
        LogFormat::Text => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(ansi)
                .with_filter(filter_fn(console_target)),
        ),
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .json()
                // Never, whatever the terminal says: an escape sequence inside
                // a JSON string field is not a colour, it is a parse hazard.
                .with_ansi(false)
                .with_filter(filter_fn(console_target)),
        ),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(
            logbuf::BufferLayer(Arc::clone(&logs.general)).with_filter(filter_fn(|metadata| {
                !logbuf::is_cluster_target(metadata.target())
            })),
        )
        .with(
            logbuf::BufferLayer(Arc::clone(&logs.cluster)).with_filter(filter_fn(|metadata| {
                logbuf::is_cluster_target(metadata.target())
            })),
        )
}

/// Cluster INFO/DEBUG detail belongs on Settings -> Cluster and is kept off
/// stdout; WARN and ERROR still reach it so a broken voter stays visible to a
/// service supervisor before an admin can sign in.
fn console_target(metadata: &tracing::Metadata<'_>) -> bool {
    !logbuf::is_cluster_target(metadata.target()) || *metadata.level() <= tracing::Level::WARN
}

/// The one line that says what is running and where its data is.
///
/// Both identities are named because they are different things that a cluster
/// makes it possible to confuse: `instance_id` is the logical server clients
/// bind to, `node_id` is the process that owns the local bytes.
fn log_startup(config: &Config, identity: &plurx_core::cluster::ClusterIdentity) {
    tracing::info!(
        version = crate::version::SEMVER,
        build = crate::version::BUILD,
        server_name = %config.server.name,
        instance_id = %identity.cluster_id,
        node_id = %identity.node_id,
        data_dir = %config.storage.data_dir.display(),
        "plurxd starting"
    );
    if crate::version::BUILD == "unknown" {
        // Two fleet nodes shipped "unknown" on 2026-08-17 because a bare
        // `docker compose up -d --build` skips the stamp; "did my deploy
        // land?" then has no answer from inside the product. The image cannot
        // derive the commit itself (`.git` is outside the build context by
        // design), so the deploy path must pass it: `make docker-up`, or
        // PLURX_BUILD_REF="$(git describe --tags --always --dirty)" in the
        // compose environment.
        tracing::warn!(
            "running an UNSTAMPED build — deploys from this image are \
             unattributable; build with `make docker-up` (or set \
             PLURX_BUILD_REF) so the System page can say which commit this is"
        );
    }
}

/// Bind the HTTP listener, naming the address when it cannot be had — the
/// common boot failure is a port already in use, and "Address already in use"
/// alone does not say which one.
async fn bind_listener(bind: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    Ok(listener)
}

/// Start LAN discovery for a server other devices can actually reach.
///
/// A loopback-only bind is common in proxy deployments, and publishing it would
/// produce a tempting setup button in every client on the network that can
/// never connect — so nothing is advertised at all in that case. Both
/// protocols are best-effort past that point: failing to join multicast must
/// not take down the HTTP server.
fn start_discovery(
    config: &Config,
    instance_id: &str,
    node_id: Option<&str>,
    advertiser: MdnsAdvertiser,
) -> Option<mdns_sd::ServiceDaemon> {
    let lan_discovery = !config.server.bind.ip().is_loopback();
    if let Some(gdm_port) = gdm_responder_port(lan_discovery, config.server.bind) {
        // GDM keeps direct-connect Plex clients working. Bonjour is the native
        // plurx contract used by the Apple apps.
        spawn_gdm_responder(
            config,
            instance_id.to_owned(),
            node_id.map(str::to_owned),
            gdm_port,
        );
    }
    start_bonjour(config, instance_id, node_id, advertiser, lan_discovery)
}

/// The native plurx discovery contract, which the Apple clients browse for.
///
/// Best-effort: a node that cannot register still serves HTTP, and clients
/// reach it by address instead. Compose deployments turn the in-process
/// advertiser off and run `plurxd advertise` in a host-network companion,
/// because multicast cannot cross a bridge.
fn start_bonjour(
    config: &Config,
    instance_id: &str,
    node_id: Option<&str>,
    advertiser: MdnsAdvertiser,
    lan_discovery: bool,
) -> Option<mdns_sd::ServiceDaemon> {
    if !lan_discovery {
        return None;
    }
    if !mdns_advertising_enabled() {
        tracing::info!("in-process Bonjour advertising disabled");
        return None;
    }
    let name = discovery_name(
        &config.server.name,
        system_hostname().as_deref(),
        primary_lan_address().as_ref(),
    );
    let instance_name = instance_label(&name, node_id);
    match advertiser(
        instance_id,
        &name.label,
        &instance_name,
        node_id,
        config.server.bind,
        crate::version::SEMVER,
    ) {
        Ok(daemon) => Some(daemon),
        Err(error) => {
            tracing::warn!(%error, "Bonjour discovery advertiser unavailable");
            None
        }
    }
}

/// Answer Plex clients' GDM searches for as long as the server runs.
///
/// Best-effort by construction: a box where the multicast join is refused
/// still serves HTTP, and the only trace is one warning.
fn spawn_gdm_responder(
    config: &Config,
    instance_id: String,
    node_id: Option<String>,
    gdm_port: u16,
) {
    let name = config.server.name.clone();
    let http_port = config.server.bind.port();
    tokio::spawn(async move {
        if let Err(e) = gdm_responder(instance_id, node_id, name, http_port, gdm_port).await {
            tracing::warn!(error = %e, "GDM discovery responder unavailable");
        }
    });
}

/// Lay out the configured storage roots, and clear exactly the one directory
/// that must not survive a restart.
///
/// Which of these persists is load-bearing. The session scratch is cleared at
/// every boot, because a half-written segment from a killed process is worse
/// than no segment. Finished transcodes, extracted subtitles, ffmpeg runtime
/// data, and persistent VOD renditions are explicit siblings so each path is
/// canonicalized and fenced before scratch cleanup. In particular, never
/// derive siblings from the resolved target of a legacy transcode-cache
/// symlink: that silently moves paths that were not configured to move.
fn configured_dirs(storage: &StorageConfig) -> crate::state::Dirs {
    let (artwork, cache_root) = if storage.cache_dir.as_os_str().is_empty() {
        (
            storage.data_dir.join("artwork"),
            storage.data_dir.join("cache"),
        )
    } else {
        (storage.cache_dir.join("artwork"), storage.cache_dir.clone())
    };
    let transcode = if storage.transcode_dir.as_os_str().is_empty() {
        storage.data_dir.join("transcode")
    } else {
        storage.transcode_dir.clone()
    };
    crate::state::Dirs {
        artwork,
        transcode,
        cache: cache_root.join("transcode"),
        subs: cache_root.join("subs"),
        runtime_cache: cache_root.join("runtime"),
        renditions: cache_root.join("renditions"),
    }
}

/// Warn when `storage.cache_dir` leaves the previous cache stranded.
///
/// Setting it switches wholesale from `<data_dir>/{artwork,cache/{transcode,
/// subs,runtime,renditions}}` to the same children under `<cache_dir>`.
/// Nothing migrates what is already there and nothing sweeps it afterwards,
/// so the old bytes stop being read without becoming reclaimable — including
/// completed offline packages and admitted VOD renditions.
/// This warns rather than refuses: the new root is the operator's explicit
/// choice, and a warning is what tells them the old one still costs disk.
fn warn_if_cache_dir_orphans_legacy_bytes(storage: &StorageConfig, dirs: &crate::state::Dirs) {
    if storage.cache_dir.as_os_str().is_empty() {
        return;
    }
    let active = [
        dirs.artwork.as_path(),
        dirs.cache.as_path(),
        dirs.subs.as_path(),
        dirs.runtime_cache.as_path(),
        dirs.renditions.as_path(),
    ];
    let legacy = [
        ("artwork", storage.data_dir.join("artwork")),
        (
            "finished transcodes",
            storage.data_dir.join("cache").join("transcode"),
        ),
        (
            "extracted subtitles",
            storage.data_dir.join("cache").join("subs"),
        ),
        (
            "ffmpeg runtime cache",
            storage.data_dir.join("cache").join("runtime"),
        ),
        (
            "admitted VOD renditions",
            storage.data_dir.join("cache").join("renditions"),
        ),
    ];
    let stranded: Vec<String> = legacy
        .iter()
        .filter(|(_, path)| {
            // A cache_dir pointed back inside data_dir can make a legacy path
            // and its replacement the same directory; that is not orphaned.
            !active.iter().any(|configured| configured == path)
                && std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
        })
        .map(|(label, path)| format!("{label} in {}", path.display()))
        .collect();
    if stranded.is_empty() {
        return;
    }
    tracing::warn!(
        cache_dir = %storage.cache_dir.display(),
        data_dir = %storage.data_dir.display(),
        stranded = %stranded.join(", "),
        "storage.cache_dir moved the node-local caches, but the previous cache under \
         data_dir still holds entries. They are no longer read or swept, including any \
         completed offline packages and admitted VOD renditions; move persistent entries \
         under the new cache root and delete or regenerate the old ffmpeg runtime cache"
    );
}

#[cfg(test)]
fn create_dirs_for_storage(storage: &StorageConfig) -> anyhow::Result<crate::state::Dirs> {
    create_dirs_for_storage_with_protected(storage, &[], &[])
}

fn create_dirs_for_config(config: &Config) -> anyhow::Result<crate::state::Dirs> {
    let key_path = config.cluster.credential_key_path(&config.storage.data_dir);
    let key_identity = plurx_core::fs_secure::regular_file_identity_nofollow_blocking(&key_path)
        .with_context(|| {
            format!(
                "opening authoritative credential key identity {} after store selection",
                key_path.display()
            )
        })?;
    let shared_cache = existing_shared_cache_roots(config);
    create_dirs_for_storage_with_protected(&config.storage, &shared_cache, &[key_identity])
}

fn create_dirs_for_storage_with_protected(
    storage: &StorageConfig,
    extra_persistent: &[(&str, &std::path::Path)],
    extra_protected: &[plurx_core::fs_secure::FileIdentity],
) -> anyhow::Result<crate::state::Dirs> {
    let mut normalized = storage.clone();
    canonicalize_configured_dir(&mut normalized.data_dir, "data")?;
    #[cfg(windows)]
    plurx_core::fs_secure::harden_private_path_blocking(&normalized.data_dir, true).with_context(
        || {
            format!(
                "protecting managed service data {} for owner and SYSTEM",
                normalized.data_dir.display()
            )
        },
    )?;
    if !normalized.cache_dir.as_os_str().is_empty() {
        canonicalize_configured_dir(&mut normalized.cache_dir, "persistent cache")?;
    }
    if !normalized.transcode_dir.as_os_str().is_empty() {
        canonicalize_configured_dir(&mut normalized.transcode_dir, "transcode scratch")?;
    }
    // `run` calls this only after `select_daemon_store` has acquired and
    // retained the data-directory daemon lock, so this is the single startup
    // point authorized to clear scratch.
    let (dirs, protected) =
        prepare_storage_dirs(&normalized, true, extra_persistent, extra_protected)?;
    let crate::state::Dirs {
        artwork,
        transcode,
        cache,
        subs,
        runtime_cache,
        renditions,
    } = dirs;
    // Explicit scratch was claimed and cleared by `prepare_storage_dirs`
    // through one retained no-follow descriptor. Legacy scratch has no marker,
    // but receives the same descriptor-relative, rename-resistant cleanup.
    if normalized.transcode_dir.as_os_str().is_empty() {
        plurx_core::fs_secure::clear_scratch_with_protected_blocking(&transcode, &protected)
            .with_context(|| {
                format!(
                    "securely clearing legacy transcode scratch {}",
                    transcode.display()
                )
            })?;
    }
    let dirs = crate::state::Dirs {
        artwork,
        transcode,
        cache,
        subs,
        runtime_cache,
        renditions,
    };
    warn_if_cache_dir_orphans_legacy_bytes(&normalized, &dirs);
    Ok(dirs)
}

#[cfg(test)]
fn create_dirs(data_dir: &std::path::Path) -> anyhow::Result<crate::state::Dirs> {
    create_dirs_for_storage(&StorageConfig {
        data_dir: data_dir.to_owned(),
        ..Default::default()
    })
}

/// Measure this node's encoders and tone-map graph, and record what was found.
///
/// The subprocess shell of startup: everything here that decides something —
/// which preference the probe runs under, what an env override means — is a
/// tested function it calls.
async fn probe_system(
    config: &Config,
    store: &Arc<dyn plurx_core::store::Store>,
    node_id: &str,
    transcode_dir: &std::path::Path,
    runtime_cache: &std::path::Path,
) -> anyhow::Result<(plurx_core::transcode::EncoderCaps, SystemInfo)> {
    let ffmpeg = crate::ffmpeg::ffmpeg_bin();
    let configured_ffprobe = crate::ffmpeg::ffprobe_bin();
    let configured_bound_ffprobe = crate::ffmpeg::bound_ffprobe_bin();
    let decode_probe_identity = match crate::decode_facts::DecodeProbeIdentity::discover(
        &configured_bound_ffprobe,
    )
    .await
    {
        Ok(identity) => Some(identity),
        Err(error) => {
            tracing::warn!(%error, "FFprobe identity is unavailable; explicit decoder facts remain unavailable");
            None
        }
    };
    let ffprobe = configured_ffprobe;
    // Detect available hardware encoders once at startup.
    let encoder_caps = plurx_core::transcode::detect_encoders(&ffmpeg).await;
    let decoders = plurx_core::transcode::detect_video_decoders(&ffmpeg).await;
    // Which decoder this build actually selects for each of those families.
    // Read only by qualified planning, but measured unconditionally: it is a
    // handful of fractional-second probes, and an operator deciding whether to
    // enable qualification needs to see what this node measured before they
    // decide, not after.
    let measured_decoders = plurx_core::transcode::decoder_inventory::measure_selected_decoders(
        &ffmpeg,
        &decoders,
        transcode_dir,
    )
    .await;
    for (codec, backend, decoder) in measured_decoders.measured_paths() {
        tracing::info!(%codec, backend = backend.name(), %decoder, "measured the decoder this ffmpeg selects");
    }
    // The Dolby Vision renderer's CPU debit reads this. Warm it off the boot
    // path: until it is proved, a play is debited the software shape, and a
    // failure is retried on later need (`has_hevc_main10_vaapi_decode`).
    drop(tokio::spawn(async {
        let hevc_main10_vaapi_decode = crate::ffmpeg::has_hevc_main10_vaapi_decode().await;
        tracing::info!(
            hevc_main10_vaapi_decode,
            "measured VA-API HEVC Main10 decode"
        );
    }));

    let hwaccel_pref = resolve_hwaccel_pref(store).await?;
    let hwaccel_override = store
        .get_setting(&crate::state::node_hwaccel_key(node_id))
        .await?;
    let hwaccel_pref = hwaccel_override.clone().unwrap_or(hwaccel_pref);
    seed_switch_settings(store).await?;
    // Representative comparisons run only on an explicit idle-node request.
    // A saved receipt is reusable only for this executable/fixture/driver set.
    let optimization = match transcoder_optimization::load(store, node_id, &ffmpeg, &encoder_caps)
        .await
    {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(%error, "could not load representative transcode comparison; using validated hardware order");
            None
        }
    };
    let probe_pref = optimization
        .as_ref()
        .filter(|_| hwaccel_pref == "auto" || hwaccel_pref.is_empty())
        .and_then(|report| report.fastest(&encoder_caps))
        .map(|encoder| encoder.family_name().to_owned())
        .unwrap_or_else(|| probe_preference(&hwaccel_pref));
    let encoder_selected = encoder_caps.choose(&probe_pref).label().to_owned();
    // Which tone-map graph this node may use. After encoder detection, because
    // a graph is only worth probing if it can feed the encoder that won. Costs
    // a few seconds on a box with a GPU worth testing and nothing at all on one
    // without.
    let tone_map = pipeprobe::probe(
        transcode_dir,
        runtime_cache,
        encoder_caps.choose(&probe_pref),
    )
    .await;
    // Measure this node's own FFmpeg once, and decide from it whether any
    // retained contract covers the binary that is about to run. A node with no
    // covering contract still reads every child's stderr; it simply reads it
    // without a grammar, which is the honest state for a build nobody has
    // qualified — and it is the state the deployed fleet is in today, because
    // FFmpeg 5.1.9 reports decode failures without naming a stream.
    install_decoder_diagnostic_policy(&ffmpeg).await;
    let measured = Measured {
        ffmpeg_version: ffmpeg_version(&ffmpeg).await,
        ffprobe_build_digest: decode_probe_identity
            .as_ref()
            .map(|identity| identity.build_digest().to_owned()),
        decode_probe_identity,
        pacing: crate::ffmpeg::pacing_caps().await,
        dovi_rpu: crate::ffmpeg::has_dovi_rpu().await,
        dovi_reshape: crate::ffmpeg::has_dovi_reshape().await,
        dovi_passthrough: crate::ffmpeg::has_dovi_passthrough().await,
        hdr10_passthrough: crate::ffmpeg::has_hdr10_passthrough().await,
        hdr10_passthrough_qsv: if encoder_caps.qsv {
            crate::ffmpeg::has_hdr10_passthrough_qsv().await
        } else {
            false
        },
        hdr10_passthrough_vaapi: if encoder_caps.vaapi {
            crate::ffmpeg::has_hdr10_passthrough_vaapi(encoder_caps.vaapi_hdr_policy).await
        } else {
            false
        },
        dovi_passthrough_qsv: if encoder_caps.qsv {
            crate::ffmpeg::has_dovi_passthrough_with(plurx_core::transcode::Encoder::Qsv).await
        } else {
            false
        },
        encoder_selected,
        decoders,
        measured_decoders,
        tone_map,
        dv_disk: crate::dv_disk::probe_capabilities().await,
    };
    let mut system = system_info(
        config,
        ffmpeg,
        ffprobe,
        hwaccel_pref,
        encoder_caps.clone(),
        measured,
    );
    system.hwaccel_override = hwaccel_override;
    system.transcoder_optimization = optimization;
    let support = crate::ffmpeg_support::report(&system).await;
    if !support.package_problems.is_empty()
        || support.rows.iter().any(|row| row.required && !row.included)
    {
        tracing::error!(
            executable = %support.executable,
            package_problems = ?support.package_problems,
            missing = ?support.rows.iter().filter(|row| row.required && !row.included).map(|row| (&row.label, &row.missing)).collect::<Vec<_>>(),
            "FFmpeg does not satisfy this node's media support contract; see Settings > System"
        );
    }
    Ok((encoder_caps, system))
}

/// What the boot probes learned about this machine's ffmpeg.
struct Measured {
    ffmpeg_version: Option<String>,
    ffprobe_build_digest: Option<String>,
    decode_probe_identity: Option<crate::decode_facts::DecodeProbeIdentity>,
    pacing: crate::ffmpeg::PacingCaps,
    dovi_rpu: bool,
    dovi_reshape: bool,
    dovi_passthrough: bool,
    dovi_passthrough_qsv: bool,
    hdr10_passthrough: bool,
    hdr10_passthrough_qsv: bool,
    hdr10_passthrough_vaapi: bool,
    encoder_selected: String,
    decoders: Vec<String>,
    measured_decoders: plurx_core::transcode::decoder_inventory::MeasuredDecoders,
    tone_map: pipeprobe::PipelineReport,
    dv_disk: crate::dv_disk::DvDiskCapabilities,
}

/// Assemble what the settings page and every session read.
///
/// Separated from the probing so the record itself is checkable: the tone-map
/// report and the encoder here are what a session consults, and a `SystemInfo`
/// whose `encoder_selected` disagreed with its `encoders` would be a settings
/// page describing a node nobody is running.
fn system_info(
    config: &Config,
    ffmpeg: String,
    ffprobe: String,
    hwaccel_pref: String,
    encoders: plurx_core::transcode::EncoderCaps,
    measured: Measured,
) -> SystemInfo {
    SystemInfo {
        data_dir: config.storage.data_dir.display().to_string(),
        ffmpeg_version: measured.ffmpeg_version,
        ffprobe_build_digest: measured.ffprobe_build_digest,
        decode_probe_identity: measured.decode_probe_identity,
        ffmpeg,
        ffprobe,
        hwaccel_pref,
        hwaccel_override: None,
        transcoder_optimization: None,
        encoders,
        decoders: measured.decoders,
        measured_decoders: measured.measured_decoders,
        encoder_selected: measured.encoder_selected,
        tone_map: measured.tone_map,
        pacing: measured.pacing,
        dovi_rpu: measured.dovi_rpu,
        dovi_reshape: measured.dovi_reshape,
        dovi_passthrough: measured.dovi_passthrough,
        dovi_passthrough_qsv: measured.dovi_passthrough_qsv,
        hdr10_passthrough: measured.hdr10_passthrough,
        hdr10_passthrough_qsv: measured.hdr10_passthrough_qsv,
        hdr10_passthrough_vaapi: measured.hdr10_passthrough_vaapi,
        dv_disk: measured.dv_disk,
        // Not measured: the conversion is plurx's own code, so the only
        // question is whether an operator has turned it off. The default is
        // on — a Profile 7 title that reaches a Dolby Vision client as HDR10
        // is the thing this exists to stop, so it should not need enabling.
        //
        // The boot record only. What the daemon actually converts is the
        // replicated `playback.dolby_vision_convert` setting, read per
        // request; this variable seeds it once on a node that has never been
        // told either way, and `stored_switch` case-folds both the same way.
        dolby_vision_convert: dv_convert_enabled(std::env::var("PLURX_DV_CONVERT").ok().as_deref()),
    }
}

/// Whether the Profile 7 → 8.1 conversion runs, from the raw environment
/// value.
///
/// Separated from `system_info` so it is testable without setting a process
/// environment variable — which the test harness runs threads in, so a test
/// that set one would decide the answer for whatever else was running.
fn dv_convert_enabled(value: Option<&str>) -> bool {
    !value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// Carry a deployment's `PLURX_PGS_OVERLAY` and `PLURX_DV_CONVERT` across to
/// the settings that replaced them, once.
///
/// Two properties, and both matter.
///
/// **Seed-when-absent, not env-wins.** `resolve_hwaccel_pref` below lets the
/// variable win on every boot, because that one is a hint about this machine's
/// hardware. These two are switches the product now owns: if a stale compose
/// file re-asserted itself at every restart, turning one off in Settings would
/// appear to work and then quietly undo itself on the next deploy, which is
/// worse than the boot-time gate it replaces.
///
/// **First writer wins, and on a cluster that is one node deciding for the
/// fleet.** These settings are replicated; the variables were per-node. So a
/// three-node cluster where one node still carries `PLURX_DV_CONVERT=0` will
/// take that node's answer for every node, if it is the first to upgrade —
/// which is why the write is `put_setting_if_absent` rather than a read
/// followed by a put. The atomic form is what makes "first" mean the first
/// durable commit rather than the first node to *look*: an operator changing
/// the switch on node B while node A restarts would otherwise have their
/// change silently reverted by A's compose file. It does not make one node
/// deciding for the fleet correct — it makes it bounded, visible in Settings
/// immediately after, and impossible to repeat on the next restart.
async fn seed_switch_settings(store: &Arc<dyn plurx_core::store::Store>) -> anyhow::Result<()> {
    if let Ok(value) = std::env::var("PLURX_PGS_OVERLAY") {
        let on = plurx_core::store::stored_switch(Some(value.as_str()), false);
        store
            .put_setting_if_absent(keys::PGS_OVERLAY, if on { "1" } else { "0" })
            .await?;
    }
    // Only an explicit off is worth recording. This switch defaults on, so a
    // node whose variable was unset or said on already answers the way the
    // absent setting answers, and writing that down would make a stored value
    // where the operator never expressed one — and, on a cluster, would let a
    // node that was never configured claim the key first.
    if let Ok(value) = std::env::var("PLURX_DV_CONVERT") {
        if !plurx_core::store::stored_switch(Some(value.as_str()), true) {
            store.put_setting_if_absent(keys::DV_CONVERT, "0").await?;
        }
    }
    Ok(())
}

/// The encoder preference this boot runs under.
///
/// `PLURX_HWACCEL` (documented since Phase 2, previously read by nothing) seeds
/// the stored setting at boot: env wins over the setting, and then the setting
/// is the single source both this probe and the admin UI read, so the two can
/// never disagree about what the node was told to use.
async fn resolve_hwaccel_pref(store: &Arc<dyn plurx_core::store::Store>) -> anyhow::Result<String> {
    if let Some(pref) = hwaccel_override(std::env::var("PLURX_HWACCEL").ok()) {
        store.put_setting(keys::HWACCEL, &pref).await?;
    }
    Ok(store
        .get_setting(keys::HWACCEL)
        .await?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "auto".to_owned()))
}

/// What an `PLURX_HWACCEL` value seeds the stored setting with, if anything.
///
/// An empty value is what Compose produces for an unset variable, and storing
/// it would overwrite an operator's chosen encoder with "no preference" on
/// every restart.
fn hwaccel_override(raw: Option<String>) -> Option<String> {
    raw.filter(|v| !v.is_empty())
        .map(|v| v.to_lowercase().trim().to_owned())
}

/// The string the encoder chooser is asked with. "auto" is the absence of a
/// preference, not a preference named "auto" — passed through literally it
/// would match no encoder and silently select the software fallback.
fn probe_preference(hwaccel_pref: &str) -> String {
    if hwaccel_pref == "auto" {
        String::new()
    } else {
        hwaccel_pref.to_owned()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_state(
    config: &Config,
    node_id: String,
    cluster_id: String,
    credential_key: Arc<plurx_core::secrets::CredentialKey>,
    backup_client: Option<hiqlite::Client>,
    replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    membership: plurx_core::cluster::membership::MembershipManager,
    catalogue: plurx_core::store::CatalogueReader,
    store: Arc<dyn plurx_core::store::Store>,
    dirs: crate::state::Dirs,
    encoder_caps: plurx_core::transcode::EncoderCaps,
    system: SystemInfo,
    logs: logbuf::LogBuffers,
) -> AppState {
    let state = AppState::new_configured(
        crate::state::AppConfig {
            sharing_network: config.sharing.clone(),
            server_name: config.server.name.clone(),
            node_id,
            cluster_advertisement: !config.cluster.advertise_host.trim().is_empty(),
            trusted_proxies: config.server.trusted_proxies.clone(),
            scan_prune_percent: config.storage.scan_prune_percent,
            credential_key,
            replication,
            membership,
            cluster_id,
            shared_cache_dir: config.cluster.shared_cache_dir.clone(),
            shared_cache_id: config.cluster.shared_cache_id.clone(),
            catalogue,
            snapshot_recovery_budgets: crate::state::SnapshotRecoveryBudgets {
                chunk_secs: config.cluster.snapshot_chunk_timeout_secs,
                transfer_secs: config.cluster.snapshot_transfer_timeout_secs,
                install_secs: config.cluster.install_snapshot_timeout_secs,
            },
            data_dir: config.storage.data_dir.clone(),
            credential_key_path: config.cluster.credential_key_path(&config.storage.data_dir),
            backup_client,
        },
        store,
        dirs,
        encoder_caps,
        system,
        logs,
    );
    crate::telemetry::diagnostics(Arc::clone(&state.store))
        .configure_publisher(config.support.clone());
    state
}

/// Every loop that has to keep running whether or not a request arrives.
///
/// A retry scheduled two minutes out has no request to wake it, and a Curator
/// that is down must not stall anything a viewer is waiting on — so each of
/// these owns its own timing rather than riding on traffic.
struct BackgroundLoopGuard {
    shutdown: tokio_util::sync::CancellationToken,
}

impl BackgroundLoopGuard {
    fn new() -> Self {
        Self {
            shutdown: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn token(&self) -> tokio_util::sync::CancellationToken {
        self.shutdown.clone()
    }
}

impl Drop for BackgroundLoopGuard {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Start the daemon's background loops.
///
/// Three of these are cluster-wide work and take a live eligibility check on
/// every pass, so a node carrying no vote runs none of them and a promoted one
/// starts without a restart: the scheduler (which dispatches every leased
/// singleton job), the Trakt two-way sync, and the watched outbox. All three
/// mutate replicated state on behalf of the whole server.
///
/// Everything else here is node-local by construction and must keep running on
/// a learner. The store-metrics and replication loops describe *this* process —
/// a learner's lag is precisely what an operator needs to see. The membership
/// heartbeat is how a node stays in the roster and keeps its protocol
/// capability proof current; a learner that stopped heartbeating would read as
/// unreachable. The offline source probe answers questions about this node's
/// own files and has to run everywhere for any node's removal to be provable.
/// The serving-fence, media-pool, shared-cache, transcode, artwork-
/// materialization and storage-probe loops act on bytes and hardware this
/// process owns.
///
/// Two of them are not node-local, and saying they were was wrong.
/// `maintain_media_sessions` ends *any* node's expired sessions and prunes
/// `job_leases`; `expire_offline_packages` expires the whole cluster's. Both
/// are idempotent sweeps over rows that have already passed a deadline, both
/// reach the same answer whoever runs them, and every voter already runs them
/// concurrently today — so a learner running them too changes nothing. They
/// stay ungated because a cluster where only voters swept would leave expired
/// sessions and packages alive whenever the sweeping voter was down, and
/// because gating them would buy nothing: a sweep is not a singleton.
///
/// The offline-package loop is the exception inside the exception, and it is
/// gated: its expiry sweep is one of the two above, but claiming the next
/// queued package is not a sweep at all — it pops from a cluster-wide queue
/// and binds the work to this node. See `offline::prepare_loop`.
fn spawn_background_loops(
    state: &AppState,
    background_shutdown: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(http::remote::invitations::worker::run(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(Arc::clone(&state.sharing).run(state.clone(), background_shutdown.clone()));
    tokio::spawn(Arc::clone(&state.sharing).claim_loop(state.clone(), background_shutdown.clone()));
    // Crash recovery for orphaned shared-playback receiver routes; ends on drain.
    tokio::spawn(
        crate::http::shared_receiver_playback::receiver_recovery_loop(
            state.clone(),
            background_shutdown.clone(),
        ),
    );
    tokio::spawn(crate::clock_offset::run(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(http::file_grants::prune_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(http::playback_handoffs::prune_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(subtitle_transcription::run(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(http::subtitle_downloads::automatic_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(state.clone().store_metrics_loop());
    tokio::spawn(http::plex_census::flush_loop(
        Arc::clone(&state.plex_census),
        background_shutdown.clone(),
    ));
    tokio::spawn(Arc::clone(&state.backup).schedule_loop(background_shutdown.clone()));
    tokio::spawn(
        crate::http::cluster_operations::membership_status_cache_loop(
            state.clone(),
            background_shutdown.clone(),
        ),
    );
    tokio::spawn(
        crate::http::internal_auth_revocation::cache_admin_revocation_activation_loop(
            state.clone(),
            background_shutdown.clone(),
        ),
    );
    tokio::spawn(crate::http::cluster_operations::peer_status_cache_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    let replication = state.replication.clone();
    tokio::spawn(replication.passive_metrics_loop(background_shutdown.clone().cancelled_owned()));
    tokio::spawn(
        state
            .serving
            .clone()
            .monitor_loop(background_shutdown.clone()),
    );
    tokio::spawn(
        std::sync::Arc::clone(&state.transcode).serving_fence_loop(state.serving.subscribe()),
    );
    state.membership.register_media_relay_acquisition();
    tokio::spawn(state.membership.clone().heartbeat_loop());
    tokio::spawn(std::sync::Arc::clone(&state.media_pool).poll_loop());
    tokio::spawn(
        std::sync::Arc::clone(&state.media_pool)
            .root_readability_loop(std::sync::Arc::clone(&state.store)),
    );
    tokio::spawn(std::sync::Arc::clone(&state.shared_cache).run(background_shutdown.clone()));
    tokio::spawn(std::sync::Arc::clone(&state.playback_cache).run(background_shutdown.clone()));
    // Fragment indexes are node-local for both storage backends. Validate one
    // bounded page here on every node; cluster leadership is neither required
    // nor sufficient to cover another node's sidecar.
    tokio::spawn(
        std::sync::Arc::clone(&state.jobs)
            .fragment_index_validation_loop(background_shutdown.clone()),
    );
    // Once per process, off the startup path: the fragment-index pass keeps
    // PGS tracks only after this proves the configured ffmpeg behaves as the
    // design measured. Bounded by its own budget and by shutdown; every child
    // it runs is killed on drop and reaped before it returns.
    tokio::spawn(crate::subtitle_ride_along::startup_self_test(
        state.runtime_cache_dir.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::media_sessions::lease_loop(state.clone()));
    tokio::spawn(crate::media_sessions::takeover_loop(state.clone()));
    tokio::spawn(crate::media_sessions::maintenance_loop(state.clone()));
    tokio::spawn(crate::artifact_integrity::run(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::source_probe::run(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::library_search::semantic::worker(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::library_search::worker(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::channel_subjects::worker(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::http::library_channels::reconcile_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    // Answers "can you read this package's source?" while a peer is being
    // removed. Every node has to be listening for its own removal to be
    // possible, so this runs whether or not a removal is in progress.
    tokio::spawn(state.membership.clone().offline_source_probe_loop());
    // Artwork filenames are replicated facts, but their bytes are node-local.
    // Reconcile them independently of traffic so every voter is ready to
    // become the public leader before a failover actually happens.
    tokio::spawn(crate::http::images::materialize_loop(state.clone()));
    tokio::spawn(std::sync::Arc::clone(&state.transcode).rate_control_refresh_loop());
    // Compatibility probing stays off startup and session creation. A restart
    // owns one generation; an explicit admin reprobe uses this same owner.
    tokio::spawn({
        let manager = Arc::clone(&state.transcode);
        let cancelled = background_shutdown.clone();
        async move {
            manager.reprobe_video_processing(&cancelled).await;
        }
    });
    tokio::spawn(std::sync::Arc::clone(&state.transcode).scratch_space_loop());
    tokio::spawn(
        std::sync::Arc::clone(&state.live_tv).scratch_sweep_loop(background_shutdown.clone()),
    );
    tokio::spawn(
        std::sync::Arc::clone(&state.live_tv).settings_observer_loop(background_shutdown.clone()),
    );
    tokio::spawn(
        std::sync::Arc::clone(&state.live_tv)
            .guide_refresh_loop(state.serving.subscribe(), background_shutdown.clone()),
    );
    // Running Live TV survives a loss of serving authority inside the session
    // grace, as rolling and progressive playback do; this loop decides when a
    // loss has outlasted it.
    tokio::spawn(
        state
            .live_tv
            .authority()
            .serving_fence_loop(state.serving.subscribe()),
    );
    // One queue, one worker: the recording and reminder loops enqueue and
    // never await the network, so an unreachable endpoint cannot delay a
    // capture starting or stopping.
    let (dvr_events, dvr_event_queue) = crate::live_tv::webhook::channel();
    tokio::spawn(crate::live_tv::webhook::webhook_worker(
        std::sync::Arc::clone(&state.live_tv),
        dvr_event_queue,
        background_shutdown.clone(),
    ));
    tokio::spawn(
        std::sync::Arc::clone(&state.live_tv)
            .dvr_loop(dvr_events.clone(), background_shutdown.clone()),
    );
    tokio::spawn(crate::live_tv::dvr::reminder_loop(
        std::sync::Arc::clone(&state.live_tv),
        dvr_events,
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::http::live_tv::guide_replica_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::live_tv::dvr::dvr_library_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    tokio::spawn(crate::live_tv::dvr::dvr_progress_loop(
        std::sync::Arc::clone(&state.live_tv),
        background_shutdown.clone(),
    ));
    // Reap idle transcode sessions in the background.
    tokio::spawn(std::sync::Arc::clone(&state.transcode).reap_loop());
    tokio::spawn(std::sync::Arc::clone(&state.transcode).vod_maintain_loop());
    // Post-start complete-output queue publication; VOD create only hands off.
    tokio::spawn(std::sync::Arc::clone(&state.transcode).output_enqueue_loop(background_shutdown));
    // The expiry sweep in here is a cluster-wide idempotent sweep and stays
    // ungated; claiming the next queued package is not, and takes the same
    // live membership check the other cluster-wide loops do.
    tokio::spawn(
        std::sync::Arc::clone(&state.offline).run(std::sync::Arc::new(state.membership.clone())),
    );

    // What the libraries' storage reads at. Deliberately after the listener
    // would come up rather than inline with the encoder and tone-map probes:
    // those measure local hardware in milliseconds, this one may be talking to
    // a NAS that is asleep, on a link that is down, or behind a mount that
    // will block for thirty seconds before admitting it. None of that should
    // delay a server answering requests, and all of it is worth knowing.
    tokio::spawn(crate::http::system::probe_storage(state.clone(), 0.0));

    // Scheduled jobs: library scans, metadata refreshes, probe retries, and
    // transcode-cache cleanup. Every interval defaults to off, so this loop
    // does nothing until someone sets one in Settings.
    tokio::spawn(
        std::sync::Arc::clone(&state.jobs).schedule_loop(std::sync::Arc::clone(&state.transcode)),
    );

    tokio::spawn(
        std::sync::Arc::clone(&state.jobs)
            .background_work_loop(std::sync::Arc::clone(&state.transcode)),
    );

    tokio::spawn(crate::http::images::durable_artwork_loop(state.clone()));
    tokio::spawn(crate::http::transcode_copies::run(state.clone()));

    // Trakt: hourly (and on-demand) two-way sync + the scrobble-pause sweep.
    tokio::spawn(
        std::sync::Arc::clone(&state.trakt)
            .sync_loop(std::sync::Arc::new(state.membership.clone())),
    );
    tokio::spawn(std::sync::Arc::clone(&state.trakt).sweep_loop());
    // The watched outbox. Its own loop because a retry scheduled two minutes
    // out has no request to wake it, and a Curator that is down must not stall
    // anything a viewer is waiting on.
    match plurx_core::cluster::coordination::StoreCoordinator::new(
        std::sync::Arc::clone(&state.store),
        state.node_id.clone(),
    ) {
        Ok(coordinator) => {
            tokio::spawn(std::sync::Arc::clone(&state.watched).run(
                crate::watched::DrainAuthority {
                    coordinator,
                    authority: std::sync::Arc::new(state.membership.clone()),
                    policy: crate::watched::WatchedLeasePolicy::PRODUCTION,
                },
            ));
        }
        Err(error) => {
            tracing::error!(%error, "the watched outbox drain cannot start without a lease owner id");
        }
    }
}

/// Which port the GDM responder should answer on, or `None` when it must not
/// run at all. A loopback bind is never advertised: publishing it would produce
/// a setup button in every client that can never connect.
fn gdm_responder_port(lan_discovery: bool, bind: SocketAddr) -> Option<u16> {
    if !lan_discovery {
        tracing::info!(%bind, "LAN discovery disabled for loopback bind");
        return None;
    }
    match gdm_bind_port(std::env::var("PLURX_GDM_PORT").ok().as_deref()) {
        Ok(port) => Some(port),
        Err(error) => {
            tracing::warn!(%error, "GDM discovery responder disabled");
            None
        }
    }
}

/// Serve until `shutdown` fires, then drain — for a bounded time.
///
/// Taking the shutdown future rather than installing the signal handler itself
/// is what makes the drain testable, and the drain is the part worth testing. A
/// playback response is a stream: a paced remux holds its connection open for a
/// quarter of the film's runtime, and an HLS playlist poll is only ever moments
/// from the next one. Waiting for all of them means never finishing, so `docker
/// stop` spends its ten-second grace period achieving nothing and then SIGKILLs
/// us — which surfaces as exit 137, indistinguishable at a glance from an
/// out-of-memory kill. Bounding the drain turns that into an orderly exit 0
/// with a line in the log saying what was still open.
async fn serve(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    jellyfin: JellyfinStandardListener,
    https: https::Serving,
    progress: Arc<crate::progress::ProgressCoalescer>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<HttpDrain> {
    // The shutdown future stops both listeners at once; the standard port
    // finishes draining before progress is flushed, like the main one.
    let main = serve_http(listener, app, shutdown, HTTP_TIMEOUTS).await;
    let tls = https.drain().await;
    let jellyfin = jellyfin.drain().await;
    let cleanup = drain_serving_state(progress, mdns).await;
    main.and_then(|main| tls.map(|tls| main.and(tls).and(jellyfin)))
        .and_then(|drain| cleanup.map(|()| drain))
}

/// The observation socket transfers its router per request, but shares the
/// same TLS/Jellyfin/progress ownership order as ordinary startup.
async fn serve_observed(
    owner: Arc<StartupObservationHttp>,
    app: axum::Router,
    jellyfin: JellyfinStandardListener,
    https: https::Serving,
    progress: Arc<crate::progress::ProgressCoalescer>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<HttpDrain> {
    let main = owner.serve_normal(app, shutdown).await;
    let tls = https.drain().await;
    let jellyfin = jellyfin.drain().await;
    let cleanup = drain_serving_state(progress, mdns).await;
    main.and_then(|main| tls.map(|tls| main.and(tls).and(jellyfin)))
        .and_then(|drain| cleanup.map(|()| drain))
}

/// How `serve_http`'s shutdown drain ended. Only `Complete` proves that no
/// request can still be running in this process; the Plex census marks its
/// last write clean on exactly that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpDrain {
    /// Every open connection finished inside the drain window.
    Complete,
    /// The window passed with connections still open; they were abandoned.
    TimedOut,
}

impl HttpDrain {
    /// A process drained only if every listener it served did.
    fn and(self, other: Self) -> Self {
        if self == Self::Complete && other == Self::Complete {
            Self::Complete
        } else {
            Self::TimedOut
        }
    }
}

/// Jellyfin's standard port, served beside the main listener.
///
/// Jellyfin clients given only a host assume this port at the server root, so
/// the facade answers there too: `http::jellyfin_standard_port_app` maps every
/// request under the facade's mount, and nothing else is reachable on it.
/// Opening it is best-effort, like discovery: a host where the port is taken
/// (a real Jellyfin still running, say) keeps serving on its own port, and
/// Settings -> Developer says why bare-host clients cannot connect. It stops
/// accepting at the same moment as the main listener and drains before
/// playback progress is flushed, so a late progress report through it is not
/// lost.
#[derive(Default)]
struct JellyfinStandardListener {
    stop: tokio_util::sync::CancellationToken,
    task: Option<tokio::task::JoinHandle<anyhow::Result<HttpDrain>>>,
}

impl JellyfinStandardListener {
    async fn open(
        address: Option<SocketAddr>,
        app: &axum::Router,
        status: &http::JellyfinStandardPortStatus,
    ) -> Self {
        let mut listener = Self::default();
        let Some(address) = address else {
            status.record(http::JellyfinStandardPort::Off);
            return listener;
        };
        match tokio::net::TcpListener::bind(address).await {
            Ok(socket) => {
                let bound = socket.local_addr().unwrap_or(address);
                tracing::info!(addr = %bound, "listening for Jellyfin clients at the server root");
                status.record(http::JellyfinStandardPort::Listening(bound));
                let stop = listener.stop.clone();
                listener.task = Some(tokio::spawn(serve_http(
                    socket,
                    http::jellyfin_standard_port_app(app.clone()),
                    async move { stop.cancelled().await },
                    HTTP_TIMEOUTS,
                )));
            }
            Err(error) => {
                tracing::warn!(
                    addr = %address,
                    %error,
                    "Jellyfin standard port unavailable; Jellyfin clients need the /jellyfin address on the main port"
                );
                status.record(http::JellyfinStandardPort::Unavailable {
                    address,
                    error: error.to_string(),
                });
            }
        }
        listener
    }

    fn stop_token(&self) -> tokio_util::sync::CancellationToken {
        self.stop.clone()
    }

    /// Stop accepting, if the shutdown future has not already, and wait for
    /// open connections to finish inside `serve_http`'s drain window.
    async fn drain(mut self) -> HttpDrain {
        self.stop.cancel();
        let Some(task) = self.task.take() else {
            return HttpDrain::Complete;
        };
        match task.await {
            Ok(Ok(drain)) => drain,
            Ok(Err(error)) => {
                tracing::warn!(%error, "Jellyfin standard-port listener stopped with an error");
                HttpDrain::TimedOut
            }
            Err(error) => {
                tracing::warn!(%error, "Jellyfin standard-port listener task failed");
                HttpDrain::TimedOut
            }
        }
    }
}

impl Drop for JellyfinStandardListener {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn drain_serving_state(
    progress: Arc<crate::progress::ProgressCoalescer>,
    mdns: Option<mdns_sd::ServiceDaemon>,
) -> anyhow::Result<()> {
    match tokio::time::timeout(PROGRESS_DRAIN_TIMEOUT, progress.drain()).await {
        Ok(Ok(flushed)) if flushed > 0 => {
            tracing::info!(
                flushed,
                "flushed coalesced playback progress during shutdown"
            );
        }
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "could not flush coalesced playback progress during shutdown");
        }
        Err(_) => {
            tracing::warn!(
                after = ?PROGRESS_DRAIN_TIMEOUT,
                "timed out flushing coalesced playback progress during shutdown"
            );
        }
    }
    if let Some(mdns) = mdns {
        if let Err(error) = mdns.shutdown() {
            tracing::warn!(%error, "Bonjour discovery shutdown failed");
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct HttpTimeouts {
    header_read: Duration,
    h2_keepalive_interval: Duration,
    h2_keepalive_timeout: Duration,
    accept_error_backoff: Duration,
    shutdown_drain: Duration,
}

const HTTP_TIMEOUTS: HttpTimeouts = HttpTimeouts {
    header_read: HEADER_READ_TIMEOUT,
    h2_keepalive_interval: H2_KEEPALIVE_INTERVAL,
    h2_keepalive_timeout: H2_KEEPALIVE_TIMEOUT,
    accept_error_backoff: Duration::from_secs(1),
    shutdown_drain: SHUTDOWN_DRAIN_TIMEOUT,
};

trait HttpAcceptor {
    type Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static;

    async fn accept(&self) -> std::io::Result<(Self::Stream, SocketAddr)>;
}

impl HttpAcceptor for tokio::net::TcpListener {
    type Stream = tokio::net::TcpStream;

    async fn accept(&self) -> std::io::Result<(Self::Stream, SocketAddr)> {
        let (stream, remote) = tokio::net::TcpListener::accept(self).await?;
        disable_nagle(&stream, remote);
        Ok((stream, remote))
    }
}
impl HttpAcceptor for plurx_core::sharing_tls::SharingTlsListener {
    type Stream = plurx_core::sharing_tls::SharingTlsStream;
    async fn accept(&self) -> std::io::Result<(Self::Stream, SocketAddr)> {
        plurx_core::sharing_tls::SharingTlsListener::accept(self).await
    }
}

/// Send every write as soon as it is made: set `TCP_NODELAY` on an accepted
/// HTTP connection.
///
/// With Nagle's algorithm on, the kernel holds back a write smaller than one
/// segment while earlier data on the connection is still unacknowledged. A
/// media body is a run of large writes that usually ends in a short one, and
/// the peer acknowledges lazily (delayed ACK, 40 ms minimum on Linux), so that
/// last short write can wait one delayed-ACK interval before it leaves. How
/// often a body ends that way depends on write timing, which the media read
/// size changes: measured on loopback, the 128 KiB and 256 KiB reads put most
/// HLS segment fetches into a ~50 ms mode that the 4 KiB read only reached in
/// its tail (docs/streaming/MEDIA-BODY-BUFFERS.md §5.1.1, Decision 6).
///
/// The cost is more, smaller packets on HLS bodies. The HLS pump hands the
/// body one `MEDIA_BODY_ACK_GRANULARITY` (4 KiB) piece and waits for it to be
/// taken before splitting the next, so hyper usually finds the body pending
/// and flushes after each piece: an HLS body leaves as a run of roughly 4 KiB
/// writes. With Nagle on, the kernel merged those writes' sub-segment tails;
/// with it off, each write is sent at once. On a 1500-byte-MTU link a 4 KiB
/// write is two full segments and a 1200-byte tail, about 6% more packets
/// (and ACKs) than full segments would need. Direct play and ranges are not
/// affected: they hand hyper whole `MEDIA_BODY_READ_BUFFER` reads. The
/// post-deploy check (§6) watches packet rate; batching the pump's
/// acknowledgements (M2) is what would coalesce these writes.
///
/// Failing to set the option is not a reason to refuse the connection; it is
/// only slower.
fn disable_nagle(stream: &tokio::net::TcpStream, remote: SocketAddr) {
    if let Err(error) = stream.set_nodelay(true) {
        tracing::debug!(%error, %remote, "could not set TCP_NODELAY on an accepted connection");
    }
}

/// Sharing content monitors may close this accepted transport even when Hyper
/// is blocked writing a body. HTTP/2 cancellation closes all multiplexed streams
/// on that connection; ordinary handlers never cancel this token.
#[derive(Clone)]
pub(crate) struct SharingConnectionCancellation(
    pub(crate) tokio_util::sync::CancellationToken,
    std::sync::Arc<SharingConnectionMonitors>,
    tokio_util::sync::CancellationToken,
    tokio_util::sync::CancellationToken,
);
/// Read-only observer of actual accepted writer closure; it cannot request it.
#[derive(Clone)]
pub(crate) struct SharingConnectionClosure(tokio_util::sync::CancellationToken);
impl SharingConnectionClosure {
    pub(crate) async fn wait(&self) {
        self.0.cancelled().await;
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.0.is_cancelled()
    }
}
struct SharingConnectionMonitors(std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>);
impl Drop for SharingConnectionMonitors {
    fn drop(&mut self) {
        for monitor in self.0.get_mut().expect("sharing monitor owner").drain(..) {
            monitor.abort();
        }
    }
}
impl SharingConnectionCancellation {
    fn new() -> Self {
        Self(
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::new(SharingConnectionMonitors(std::sync::Mutex::new(Vec::new()))),
            tokio_util::sync::CancellationToken::new(),
            tokio_util::sync::CancellationToken::new(),
        )
    }
    /// Stop accepting new streams, while completing existing accepted writers.
    /// Only actual connection closure supplies the settlement observation.
    pub(crate) fn drain_token(&self) -> tokio_util::sync::CancellationToken {
        self.3.clone()
    }
    /// Opaque weak ownership identity for deduplicating per-actor monitors.
    /// Holding it cannot keep the connection or monitor owner alive.
    pub(crate) fn ownership_key(&self) -> std::sync::Weak<dyn Send + Sync> {
        let identity: std::sync::Arc<dyn Send + Sync> = self.1.clone();
        std::sync::Arc::downgrade(&identity)
    }
    /// Completes only after the accepted Hyper connection future is dropped.
    pub(crate) fn closed(&self) -> SharingConnectionClosure {
        SharingConnectionClosure(self.2.clone())
    }
    /// After the actual writer has closed, let accepted-driver-owned closure
    /// acknowledgements finish their finite exchange before dropping owners.
    async fn join_monitors(&self) {
        // Keep pending handles in their owner even if daemon shutdown cancels
        // this join future. The owner's Drop can then abort remaining work.
        futures_util::future::poll_fn(|cx| {
            let mut tasks = self.1 .0.lock().expect("sharing monitor owner");
            tasks.retain_mut(|task| {
                std::future::Future::poll(std::pin::Pin::new(task), cx).is_pending()
            });
            if tasks.is_empty() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
    }
    pub(crate) fn monitor(
        &self,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), ()> {
        let mut monitors = self.1 .0.lock().expect("sharing monitor owner");
        monitors.retain(|monitor| !monitor.is_finished());
        if monitors.len() >= 32 || self.0.is_cancelled() || self.2.is_cancelled() {
            return Err(());
        }
        monitors.push(tokio::spawn(future));
        Ok(())
    }
}

async fn serve_http<A: HttpAcceptor>(
    listener: A,
    app: axum::Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    timeouts: HttpTimeouts,
) -> anyhow::Result<HttpDrain> {
    let (drain_started, drain_signal) = tokio::sync::oneshot::channel();
    let mut builder = ConnectionBuilder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(timeouts.header_read)
        .keep_alive(true);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(Some(timeouts.h2_keepalive_interval))
        .keep_alive_timeout(timeouts.h2_keepalive_timeout)
        // axum's server enables CONNECT for HTTP/2 websocket upgrades. Keep
        // that behavior while taking ownership of the connection builder.
        .enable_connect_protocol();

    let connection_shutdown = tokio_util::sync::CancellationToken::new();
    let mut connections = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            () = &mut shutdown => break,
        };
        let (stream, remote) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                // Accept errors are listener-local, not evidence that the
                // bound socket is permanently unusable. In particular,
                // descriptor and buffer exhaustion are expected to clear.
                // Backing off keeps a persistent condition from turning the
                // loop into a CPU/log spinner while preserving availability.
                tracing::error!(%error, "HTTP listener accept failed; retrying");
                tokio::select! {
                    () = &mut shutdown => break,
                    () = tokio::time::sleep(timeouts.accept_error_backoff) => continue,
                }
            }
        };
        // Connect info is load-bearing for the network-priors handlers in
        // `http::network`, which extract the peer address. The lower-level
        // hyper loop has to insert it explicitly because axum's IncomingStream
        // is private to `axum::serve`.
        let connection_cancel = SharingConnectionCancellation::new();
        let request_cancel = connection_cancel.clone();
        let service = tower::ServiceBuilder::new()
            .map_request(move |mut request: Request<Incoming>| {
                request.extensions_mut().insert(ConnectInfo(remote));
                request.extensions_mut().insert(request_cancel.clone());
                request.map(axum::body::Body::new)
            })
            .service(app.clone());
        let connection = builder
            .serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service))
            .into_owned();
        let shutdown_connection = connection_shutdown.clone();
        // Reap completed accepted drivers on each admission; the JoinSet owns
        // every remaining writer through daemon drain.
        while connections.try_join_next().is_some() {}
        connections.spawn(async move {
            let _cancel_on_close = connection_cancel.0.clone().drop_guard();
            // Declared before the owned connection so unwinding also drops the
            // actual writer before signalling closure to capacity monitors.
            let _closed_after_writer = connection_cancel.2.clone().drop_guard();
            let mut connection = Box::pin(connection);
            tokio::select! {
                () = async {
                    tokio::select! {
                        () = connection_cancel.3.cancelled() => {},
                        () = shutdown_connection.cancelled() => {},
                    }
                } => {
                    connection.as_mut().graceful_shutdown();
                    tokio::select! {
                        () = connection_cancel.0.cancelled() => {},
                        result = &mut connection => {
                            if let Err(error) = result {
                                tracing::debug!(%error, %remote, "HTTP connection drain ended with an error");
                            }
                        }
                    }
                },
                ()=connection_cancel.0.cancelled()=>{},
                result=&mut connection=> {if let Err(error)=result {tracing::debug!(%error,%remote,"HTTP connection closed with an error");}},
            }
            drop(connection);
            connection_cancel.2.cancel();
            connection_cancel.join_monitors().await;
        });
    }

    // Cancel owned pending TLS handshakes before waiting on active bodies.
    // This also releases the listening socket immediately on shutdown.
    drop(listener);
    let _ = drain_started.send(());
    connection_shutdown.cancel();
    let connections_drained = async { while connections.join_next().await.is_some() {} };
    tokio::pin!(connections_drained);
    let drain = tokio::select! {
        () = &mut connections_drained => {
            tracing::info!("shutdown complete");
            HttpDrain::Complete
        }
        // Only starts counting once the signal has actually arrived: if the
        // channel never fires, this branch stays pending and the server runs.
        _ = async {
            let _ = drain_signal.await;
            tokio::time::sleep(timeouts.shutdown_drain).await;
        } => {
            tracing::warn!(
                after = ?timeouts.shutdown_drain,
                "drain timed out with connections still open; exiting anyway"
            );
            HttpDrain::TimedOut
        }
    };
    Ok(drain)
}

/// How long to wait for open connections to finish after a shutdown signal.
/// Comfortably inside Docker's default ten-second stop grace period, so the
/// process gets to choose its own exit rather than being killed mid-drain.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Leaves headroom inside Docker's ten-second stop window after HTTP drain.
const PROGRESS_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
/// A request head on the LAN should arrive in milliseconds. Fifteen seconds
/// permits a bad Wi-Fi hop without letting a silent socket live forever.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(15);
/// HTTP/2 has no next-request head to time while idle, so ping it explicitly.
const H2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
/// A peer that misses one ping gets another interval to answer before close.
const H2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Native plurx discovery contract. The Apple clients declare this exact type
/// in `NSBonjourServices`, so changing it is a protocol change, not a rename.
const MDNS_SERVICE_TYPE: &str = "_plurx._tcp.local.";

/// The normal server advertises directly when it runs on a LAN interface.
/// Docker deployments turn this off and run `plurxd advertise` in a tiny
/// host-network companion instead, because multicast cannot cross a bridge.
fn mdns_advertising_enabled() -> bool {
    std::env::var("PLURX_MDNS_ADVERTISE")
        .ok()
        .map(|value| mdns_advertising_value_enabled(&value))
        .unwrap_or(true)
}

fn mdns_advertising_value_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// The name a picker shows for this server, and whether that name already
/// tells this node apart from the other nodes of the same cluster.
///
/// Distinctness is the only reason a cluster node ever falls back to its node
/// id. Three machines serving one logical server all report the same
/// `server.name`, so a picker built from that alone would offer three
/// identical rows and DNS-SD would rename two of them. A label taken from the
/// machine hostname, or one carrying this node's own LAN address, is already
/// unique on the network — and far more use to a person than a UUID prefix.
///
/// Both of those are properties of the *machine*, not of the node, so this
/// assumes the shipped topology of one node per host (`deploy/docker-compose.yml`
/// fixes `container_name`). Two nodes on one host would compute the same label
/// and DNS-SD would rename the second; their host records stay distinct
/// because those are derived from the node id.
struct DiscoveryName {
    label: String,
    distinct: bool,
}

/// A fresh Compose deployment is named `plurx`, which is useful branding but
/// useless when three machines appear in one picker. Use the machine hostname
/// for that default only; an operator's explicit server name remains theirs.
fn discovery_name(
    configured: &str,
    hostname: Option<&str>,
    address: Option<&IpAddr>,
) -> DiscoveryName {
    let configured = configured.trim();
    let machine = hostname.and_then(normalized_hostname);
    let (label, names_the_machine) = if plurx_core::config::is_default_server_name(configured) {
        match machine {
            Some(host) => (host, true),
            None => (plurx_core::config::DEFAULT_SERVER_NAME.to_owned(), false),
        }
    } else {
        (configured.to_owned(), false)
    };
    let Some(address) = address else {
        return DiscoveryName {
            label,
            distinct: names_the_machine,
        };
    };
    let identified = format!("{label} · {address}");
    // A DNS-SD service instance is one DNS label (63 bytes). A long custom
    // name remains more useful than a registration failure merely to add IP.
    if identified.len() <= 63 {
        DiscoveryName {
            label: identified,
            distinct: true,
        }
    } else {
        DiscoveryName {
            label,
            distinct: names_the_machine,
        }
    }
}

/// The DNS-SD instance name this node publishes: its display name, given a
/// node-id suffix only when the display name cannot tell two peers apart.
///
/// The full node id stays in the TXT record and the host record either way, so
/// nothing that resolves a node loses information; this is only what a person
/// reads in a picker.
fn instance_label(name: &DiscoveryName, node_id: Option<&str>) -> String {
    match node_id {
        Some(node_id) if !name.distinct => {
            clustered_instance_name(&name.label, &node_label_suffix(node_id))
        }
        _ => truncated_label(&name.label),
    }
}

/// The bounded, DNS-safe fragment of an identity that names one node's host
/// record — and, when nothing else can distinguish it, its instance too.
fn node_label_suffix(identity: &str) -> String {
    let suffix: String = identity
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    if suffix.is_empty() {
        "server".to_owned()
    } else {
        suffix
    }
}

fn normalized_hostname(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('.');
    let without_local = trimmed.strip_suffix(".local").unwrap_or(trimmed).trim();
    (!without_local.is_empty() && !without_local.eq_ignore_ascii_case("localhost"))
        .then(|| without_local.to_owned())
}

/// Ask the routing table which local IPv4 address carries mDNS. UDP `connect`
/// chooses an interface without sending a packet, so this works while the
/// server is still offline and avoids accidentally showing a Docker bridge IP.
fn primary_lan_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("224.0.0.251:5353").ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_loopback() && !address.is_unspecified()).then_some(address)
}

/// The machine name the picker shows: the same resolution the cluster roster
/// and `/api/v1/server` use, so a picker row and Settings name a node alike.
fn system_hostname() -> Option<String> {
    plurx_core::cluster::membership::system_short_hostname()
}

#[derive(Deserialize)]
struct DiscoveryServerInfo {
    name: String,
    version: String,
    instance_id: String,
    node_id: String,
    cluster_advertisement: bool,
}

/// Fetch identity from the running server, then publish that identity through
/// the host's mDNS interfaces. Keeping this process separate is what allows
/// the main container to stay on an external Compose network such as `media`.
async fn advertise(server: &str) -> anyhow::Result<()> {
    init_companion_logging();
    advertise_with(
        server,
        start_mdns_advertiser,
        Duration::from_secs(2),
        shutdown_signal(),
    )
    .await
}

async fn advertise_with(
    server: &str,
    advertiser: MdnsAdvertiser,
    retry_after: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let (base, port, info_url) = discovery_endpoints(server)?;
    let client = discovery_client()?;
    tokio::pin!(shutdown);

    let Some(info) =
        await_discovery_identity(&client, &base, info_url, retry_after, &mut shutdown).await
    else {
        return Ok(());
    };

    let host_name = system_hostname();
    let lan_address = primary_lan_address();
    let node_id = info.cluster_advertisement.then_some(info.node_id.as_str());
    let name = discovery_name(&info.name, host_name.as_deref(), lan_address.as_ref());
    let instance_name = instance_label(&name, node_id);
    let daemon = advertiser(
        &info.instance_id,
        &name.label,
        &instance_name,
        node_id,
        SocketAddr::from(([0, 0, 0, 0], port)),
        &info.version,
    )?;
    tracing::info!(server = %base, "host Bonjour discovery companion ready");
    (&mut shutdown).await;
    daemon
        .shutdown()
        .context("shutting down host Bonjour advertiser")?;
    Ok(())
}

/// Console logging for the discovery companion. It has no admin UI behind it,
/// so there is no in-memory ring to feed — only the container's stdout.
fn init_companion_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("PLURX_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init()
        .ok();
}

/// The HTTP client the companion asks the server with.
///
/// Bounded, because the companion's whole loop is "ask again in two seconds":
/// a request with no timeout against a half-open connection would hang the
/// retry loop forever and the server would never be advertised.
fn discovery_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .context("building discovery HTTP client")
}

/// The base URL, the port to advertise, and where the server identity lives.
///
/// The port is taken from the URL rather than assumed, because the whole
/// purpose of this companion is a server published on the *host* at whatever
/// port the operator mapped — advertising the container's port would produce a
/// Bonjour record no client on the LAN can connect to.
fn discovery_endpoints(server: &str) -> anyhow::Result<(reqwest::Url, u16, reqwest::Url)> {
    let base = reqwest::Url::parse(server).context("parsing discovery server URL")?;
    anyhow::ensure!(
        matches!(base.scheme(), "http" | "https"),
        "discovery server URL must use http or https"
    );
    let port = base
        .port_or_known_default()
        .context("discovery server URL has no port")?;
    let info_url = base
        .join("/api/v1/server")
        .context("building discovery server-info URL")?;
    Ok((base, port, info_url))
}

/// Wait for the server to come up and tell us who it is, or for the process to
/// be asked to stop.
///
/// The companion container starts beside the server, not after it, so a refused
/// connection is the normal first few seconds rather than a failure. It retries
/// indefinitely — there is nothing else for it to do — but logs the first
/// attempt and then only every fifteenth, so a server that never appears leaves
/// a trail without filling the log with one line every two seconds.
async fn await_discovery_identity(
    client: &reqwest::Client,
    base: &reqwest::Url,
    info_url: reqwest::Url,
    retry_after: Duration,
    shutdown: &mut (impl std::future::Future<Output = ()> + Unpin),
) -> Option<DiscoveryServerInfo> {
    let mut failed_attempts = 0_u64;
    loop {
        match fetch_discovery_server_info(client, info_url.clone()).await {
            Ok(info) => return Some(info),
            Err(error) => {
                failed_attempts += 1;
                if should_log_wait(failed_attempts) {
                    tracing::warn!(
                        %error,
                        server = %base,
                        "waiting for server before advertising Bonjour"
                    );
                }
                tokio::select! {
                    _ = &mut *shutdown => return None,
                    _ = tokio::time::sleep(retry_after) => {}
                }
            }
        }
    }
}

/// The first failure, then one line every thirty seconds. A server that never
/// appears has to leave a trail; it must not fill the log while doing so.
fn should_log_wait(failed_attempts: u64) -> bool {
    failed_attempts == 1 || failed_attempts.is_multiple_of(15)
}

async fn fetch_discovery_server_info(
    client: &reqwest::Client,
    url: reqwest::Url,
) -> anyhow::Result<DiscoveryServerInfo> {
    client
        .get(url)
        .send()
        .await
        .context("requesting server identity")?
        .error_for_status()
        .context("server identity request failed")?
        .json()
        .await
        .context("decoding server identity")
}

/// The two `ServiceDaemon` calls a registration makes. Naming them is what lets
/// a test decide a registration — and each way one fails — against a fake,
/// instead of publishing a real Bonjour record on the local network.
trait MdnsRegistrar {
    /// The daemon's event stream. It ends when the daemon is gone.
    fn monitor(&self) -> anyhow::Result<Box<dyn Iterator<Item = mdns_sd::DaemonEvent> + Send>>;

    /// Publish one record.
    fn register(&self, service: mdns_sd::ServiceInfo) -> anyhow::Result<()>;
}

impl MdnsRegistrar for mdns_sd::ServiceDaemon {
    fn monitor(&self) -> anyhow::Result<Box<dyn Iterator<Item = mdns_sd::DaemonEvent> + Send>> {
        let events = mdns_sd::ServiceDaemon::monitor(self).context("monitoring mDNS daemon")?;
        Ok(Box::new(std::iter::from_fn(move || events.recv().ok())))
    }

    fn register(&self, service: mdns_sd::ServiceInfo) -> anyhow::Result<()> {
        mdns_sd::ServiceDaemon::register(self, service).context("registering plurx Bonjour service")
    }
}

/// Drain a daemon's events, logging the ones that report a failure.
///
/// This is where an unusable interface or a socket the kernel refused actually
/// surfaces: `mdns-sd` opens its multicast sockets on the daemon thread, so
/// `register` can return `Ok` for a record that never reaches the network.
fn log_mdns_events(events: impl Iterator<Item = mdns_sd::DaemonEvent>) {
    for event in events {
        if let mdns_sd::DaemonEvent::Error(error) = event {
            tracing::warn!(%error, "Bonjour discovery daemon error");
        }
    }
}

/// Put one record on a daemon and leave its error monitor draining.
///
/// Every decision `start_mdns_advertiser` makes is here: a daemon that cannot
/// be monitored, a record that cannot be built, and a registration the daemon
/// refuses are each an error the caller reports and carries on from. Monitoring
/// is established before the record exists, so a failure the registration
/// provokes cannot be missed.
fn register_advertiser(
    daemon: &impl MdnsRegistrar,
    instance_id: &str,
    name: &str,
    instance_name: &str,
    node_id: Option<&str>,
    bind: SocketAddr,
    version: &str,
) -> anyhow::Result<()> {
    let monitor = daemon.monitor()?;
    std::thread::Builder::new()
        .name("plurx-mdns-monitor".to_owned())
        .spawn(move || log_mdns_events(monitor))
        .context("starting mDNS monitor")?;

    let service = mdns_service_info(instance_id, name, instance_name, node_id, bind, version)?;
    daemon.register(service)?;
    tracing::info!(
        service = MDNS_SERVICE_TYPE,
        server_name = name,
        instance_name,
        port = bind.port(),
        "Bonjour discovery advertiser registered"
    );
    Ok(())
}

/// Start the local DNS-SD advertiser and keep its daemon handle alive for the
/// lifetime of the HTTP server. Address auto-detection is correct for the
/// default wildcard bind; an explicit bind advertises only that address.
fn start_mdns_advertiser(
    instance_id: &str,
    name: &str,
    instance_name: &str,
    node_id: Option<&str>,
    bind: SocketAddr,
    version: &str,
) -> anyhow::Result<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new().context("starting mDNS daemon")?;
    register_advertiser(
        &daemon,
        instance_id,
        name,
        instance_name,
        node_id,
        bind,
        version,
    )?;
    Ok(daemon)
}

/// Build one testable DNS-SD record. Cluster hosts derive from `node.id`; the
/// logical name remains visible while the full node id stays in TXT metadata.
fn mdns_service_info(
    instance_id: &str,
    name: &str,
    instance_name: &str,
    node_id: Option<&str>,
    bind: SocketAddr,
    version: &str,
) -> anyhow::Result<mdns_sd::ServiceInfo> {
    let host_suffix = node_label_suffix(node_id.unwrap_or(instance_id));
    let hostname = format!("plurx-{host_suffix}.local.");
    let logical_name = if name.trim().is_empty() {
        plurx_core::config::DEFAULT_SERVER_NAME
    } else {
        name
    };
    // A caller that hands over nothing usable still gets a registrable
    // record: an empty instance label is a DNS-SD failure, not an unnamed
    // server. An over-long one is *not* cut here — the bound belongs to
    // whoever chooses the label, and `instance_label` applies it. Past 255
    // bytes `ServiceInfo::new` refuses outright; between 63 and 255 the
    // record builds and the daemon drops the label when it writes the
    // packet, which surfaces only as a warning from the monitor thread. That
    // is the reason both production callers bound the label before this
    // point, and the reason to leave this one honest rather than papering
    // over a caller that did not.
    let instance_name = if instance_name.trim().is_empty() {
        logical_name
    } else {
        instance_name.trim()
    };
    let mut properties = vec![
        ("id", instance_id),
        ("name", logical_name),
        ("version", version),
        ("api", "/api/v1"),
    ];
    if let Some(node_id) = node_id {
        properties.push(("node_id", node_id));
    }
    let addresses = if bind.ip().is_unspecified() {
        String::new()
    } else {
        bind.ip().to_string()
    };
    let service = mdns_sd::ServiceInfo::new(
        MDNS_SERVICE_TYPE,
        instance_name,
        &hostname,
        addresses.as_str(),
        bind.port(),
        properties.as_slice(),
    )?;
    Ok(if bind.ip().is_unspecified() {
        service.enable_addr_auto()
    } else {
        service
    })
}

fn clustered_instance_name(logical_name: &str, node_suffix: &str) -> String {
    let suffix = format!(" · {node_suffix}");
    let name = truncate_to_bytes(logical_name, DNS_LABEL_BYTES.saturating_sub(suffix.len()));
    if name.is_empty() {
        node_suffix.to_owned()
    } else {
        format!("{name}{suffix}")
    }
}

/// One DNS-SD service instance is a single DNS label: 63 bytes, and a
/// registration that overflows it fails outright.
const DNS_LABEL_BYTES: usize = 63;

/// The instance label as it can actually be published.
fn truncated_label(value: &str) -> String {
    truncate_to_bytes(value.trim(), DNS_LABEL_BYTES)
}

/// Cut to a byte budget on a character boundary — the label is bounded in
/// bytes, and a multi-byte name cut mid-character is not a name at all.
fn truncate_to_bytes(value: &str, budget: usize) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() + ch.len_utf8() > budget {
            break;
        }
        out.push(ch);
    }
    out
}

/// Resolve and install this process's diagnostic policy.
///
/// Every failure is silence rather than a refusal to start: a node that cannot
/// hash its own FFmpeg loses automatic decoder actions and loses nothing else,
/// and turning a diagnostic capability into an availability requirement would
/// trade a cache problem for an outage.
async fn install_decoder_diagnostic_policy(ffmpeg: &str) {
    let contracts = match crate::decoder_health::DiagnosticContract::load(
        crate::decoder_health::RETAINED_DIAGNOSTIC_CONTRACTS,
    ) {
        Ok(contracts) => contracts,
        Err(error) => {
            tracing::warn!("retained diagnostic contracts are unreadable: {error}");
            Vec::new()
        }
    };
    let build = crate::decoder_health::MeasuredBuild::measure(ffmpeg).await;
    if build.is_none() {
        tracing::info!(
            "ffmpeg build identity could not be measured; diagnostics are observation only"
        );
    }
    let policy = crate::decoder_health::DiagnosticPolicy::new(build, contracts);
    let qualified = policy.measured_build().is_some();
    if !crate::decoder_health::install_diagnostic_policy(policy) {
        tracing::warn!("a decoder diagnostic policy was already installed for this process");
        return;
    }
    tracing::info!(
        measured_build = qualified,
        "decoder diagnostic policy installed"
    );
}

/// First line of `ffmpeg -version` (e.g. "ffmpeg version 6.1.1 …"), if the
/// binary runs at all. Purely informational, for the settings page.
async fn ffmpeg_version(bin: &str) -> Option<String> {
    let out = crate::bounded_process::output(
        bin,
        &["-version"],
        Duration::from_secs(5),
        64 * 1024,
        crate::process_control::ChildWork::background("ffmpeg version probe"),
    )
    .await
    .ok()?;
    if !out.status.success() {
        return None;
    }
    first_version_line(&out.stdout)
}

/// The banner's first line, or nothing.
///
/// `None` rather than an empty string when the binary printed nothing: the
/// settings page renders this verbatim, and a blank version reads as "ffmpeg
/// reported no version" when what happened is that nothing reported at all.
fn first_version_line(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
}

/// GDM discovery responder: answers Plex clients' multicast `M-SEARCH` on the
/// LAN (docs/CLIENTS.md §3). Multicast is TTL-scoped to the local network, so
/// this never answers WAN queries (avoids GDM/SSDP reflection abuse).
async fn gdm_responder(
    instance_id: String,
    node_id: Option<String>,
    name: String,
    http_port: u16,
    gdm_port: u16,
) -> anyhow::Result<()> {
    let socket = gdm_socket(gdm_port).await?;
    gdm_serve(&socket, &instance_id, node_id.as_deref(), &name, http_port).await
}

/// Bind the GDM port and join the group Plex clients search on.
///
/// Multicast is TTL-scoped to the local network, so this can only ever be
/// reached from the LAN — which is what keeps a GDM responder from becoming
/// the reflection amplifier that the protocol has historically been abused as.
async fn gdm_socket(gdm_port: u16) -> anyhow::Result<tokio::net::UdpSocket> {
    use std::net::Ipv4Addr;

    let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, gdm_port))
        .await
        .context("binding GDM port")?;
    socket
        .join_multicast_v4(
            plurx_compat_plex::gdm::GDM_MULTICAST_ADDR.parse()?,
            Ipv4Addr::UNSPECIFIED,
        )
        .context("joining GDM multicast group")?;
    tracing::info!(port = gdm_port, "GDM discovery responder listening");
    Ok(socket)
}

/// Answer GDM searches on an already-bound socket, forever.
///
/// Split from the socket setup so the *protocol* half is reachable without
/// joining a multicast group: what matters here is that only a real `M-SEARCH`
/// draws a reply. Answering anything else would turn the server into an
/// amplification reflector for whatever else arrives on the port.
async fn gdm_serve(
    socket: &tokio::net::UdpSocket,
    instance_id: &str,
    node_id: Option<&str>,
    name: &str,
    http_port: u16,
) -> anyhow::Result<()> {
    // Plex clients parse what GDM advertises, so this stays bare semver — the
    // git build stamp would not survive their version comparisons.
    let version = crate::version::SEMVER;
    let mut buf = [0u8; 1024];
    loop {
        let (n, addr) = socket.recv_from(&mut buf).await?;
        if plurx_compat_plex::gdm::is_search(&buf[..n]) {
            let resp = plurx_compat_plex::gdm::response_for(
                &plurx_compat_plex::gdm::Advertisement {
                    instance_id,
                    name,
                    node_id,
                },
                version,
                http_port,
            );
            if let Err(e) = socket.send_to(&resp, addr).await {
                tracing::warn!(error = %e, "GDM response send failed");
            }
        }
    }
}

fn gdm_bind_port(value: Option<&str>) -> anyhow::Result<u16> {
    let Some(value) = value else {
        return Ok(plurx_compat_plex::gdm::GDM_PORT);
    };
    value
        .parse::<u16>()
        .with_context(|| format!("invalid PLURX_GDM_PORT {value:?}"))
}

/// Records a terminate or interrupt signal from the moment startup begins,
/// rather than from the moment something first polls the drain.
///
/// [`shutdown_signal`] installs the OS handlers before this process can become
/// reachable, but installing is not observing. Nothing polls the returned
/// future until `serve` first polls its graceful-shutdown argument, so a signal
/// arriving during store activation or hardware probing stayed buffered in the
/// stream while every remaining startup stage ran to completion. An operator
/// stopping a container midway through activation got no drain and no early
/// exit — just a daemon that kept starting until the grace period ran out and
/// the runtime killed it. Driving the streams from a task started here means
/// the signal is recorded when it arrives, and startup can stop at its next
/// cancel-safe boundary.
///
/// This does not weaken the registration invariant: the handlers are still
/// installed by [`shutdown_signal`] before the future is handed over, so the
/// window between reachability and registration stays closed.
#[derive(Clone)]
struct ShutdownWatch(tokio::sync::watch::Receiver<bool>);

impl ShutdownWatch {
    /// Takes an already-installed signal future and starts polling it now.
    fn observing(signal: impl std::future::Future<Output = ()> + Send + 'static) -> Self {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            signal.await;
            // Every receiver being gone means the process is already past the
            // point where a recorded signal could change anything.
            let _ = sender.send(true);
        });
        Self(receiver)
    }

    /// Whether a signal has already been observed. For the startup boundaries
    /// that must not be cancelled mid-stage.
    fn is_signalled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once a signal has been observed, including one that arrived
    /// before this future was first polled.
    async fn signalled(mut self) {
        while !*self.0.borrow_and_update() {
            if self.0.changed().await.is_err() {
                // The driver only ends after sending, so a closed channel with
                // nothing recorded means no signal is ever coming. Never
                // resolving matches what an unsignalled process should do.
                std::future::pending::<()>().await;
            }
        }
    }
}

fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    // Install both Unix streams before returning the future. The caller binds
    // the listener before `serve` first polls its graceful-shutdown future, so
    // creating either stream inside that future leaves a small interval where
    // the process is externally reachable but the signal still has its default
    // terminating action.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler");
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("installing SIGINT handler");

    async move {
        #[cfg(unix)]
        let ctrl_c = async move {
            interrupt.recv().await;
        };
        #[cfg(not(unix))]
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("installing Ctrl-C handler");
        };
        #[cfg(unix)]
        let terminate = async move {
            terminate.recv().await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();
        #[cfg(windows)]
        let service_stop = async {
            if let Some(token) = crate::windows_service::stop_token() {
                token.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        #[cfg(not(windows))]
        let service_stop = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
            _ = service_stop => {},
        }
        tracing::info!("shutdown signal received, draining");
    }
}

/// Raise SIGTERM at a named startup boundary in the shipped binary, so two
/// otherwise unobservable windows can be tested rather than argued about.
///
/// `after-listener-bind` hits the sub-millisecond gap between the listener
/// becoming reachable and `serve` first polling the drain: with lazy
/// registration the signal takes its default action and the process dies from
/// signal 15, with eager registration it drains and exits 0.
///
/// `after-store-activation` hits the much wider window before `boot` runs at
/// all: without a watcher observing the installed streams the signal only sits
/// buffered, so the daemon probes hardware and binds a listener anyway and
/// `plurxd starting` appears in its log. With one, startup stops before `boot`.
#[cfg(unix)]
fn trigger_shutdown_registration_failpoint(stage: &str) {
    if matches!(
        std::env::var("PLURX_SHUTDOWN_REGISTRATION_FAILPOINT").as_deref(),
        Ok(configured) if configured == stage
    ) {
        // SAFETY: this path is opt-in test instrumentation, and
        // `shutdown_signal` installed the process handler before startup.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
    }
}

#[cfg(not(unix))]
fn trigger_shutdown_registration_failpoint(_stage: &str) {}

/// Minimal HTTP/1.0 probe over std TcpStream — deliberately dependency-free.
fn healthcheck(config: &Config) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    let addr = SocketAddr::from(([127, 0, 0, 1], config.server.bind.port()));
    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .with_context(|| format!("connecting to {addr}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /readyz HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status_line = response.lines().next().unwrap_or_default();
    if status_line.starts_with("HTTP/1.0 200") || status_line.starts_with("HTTP/1.1 200") {
        println!("healthy");
        Ok(())
    } else {
        anyhow::bail!("unhealthy: {status_line:?}");
    }
}

#[cfg(test)]
mod startup_tests;
