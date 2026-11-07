//! This executable performs no serving and launches no subprocesses.
//! All authority, census, conversion and durable receipt checks stay in Core.
use clap::{Parser, Subcommand};
use plurx_core::storage::helper::{self, HelperBuild};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "plurx-storage-migrate", version)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bounded protocol description; does not open a data directory.
    Describe,
    /// Explicit operator migration for a verified source/external installation.
    /// Managed daemon startup never uses this unsigned bootstrap path.
    MigratePrivate {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        expected_commit: String,
        #[arg(long)]
        expected_tree: String,
        #[arg(long)]
        expected_executable_sha256: String,
    },
    /// Resume exactly the already prepared, root-bound operation.
    Run {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        request: String,
        #[arg(long)]
        request_sha256: String,
    },
}

fn build() -> HelperBuild {
    HelperBuild {
        commit: env!("PLURX_HELPER_COMMIT").to_owned(),
        tree: env!("PLURX_HELPER_TREE").to_owned(),
        target: env!("PLURX_HELPER_TARGET").to_owned(),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let result = match Arguments::parse().command {
        Command::Describe => helper::describe(build()).and_then(|description| {
            serde_json::to_vec(&description)
                .map_err(|error| plurx_core::error::StoreError::Task(error.to_string()))
        }),
        Command::MigratePrivate {
            config,
            expected_commit,
            expected_tree,
            expected_executable_sha256,
        } => migrate_private(
            &config,
            &expected_commit,
            &expected_tree,
            &expected_executable_sha256,
        )
        .await
        .and_then(|receipt| {
            serde_json::to_vec(&receipt)
                .map_err(|error| plurx_core::error::StoreError::Task(error.to_string()))
        }),
        Command::Run {
            root,
            key,
            request,
            request_sha256,
        } => run_prepared(&root, &key, &request, &request_sha256)
            .await
            .and_then(|receipt| {
                serde_json::to_vec(&receipt)
                    .map_err(|error| plurx_core::error::StoreError::Task(error.to_string()))
            }),
    };
    match result {
        Ok(bytes) if bytes.len() <= 65_536 => {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            if stdout
                .write_all(&bytes)
                .and_then(|_| stdout.write_all(b"\n"))
                .is_ok()
            {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Ok(_) => {
            eprintln!("migration result exceeds protocol bound");
            std::process::ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
async fn migrate_private(
    config_path: &std::path::Path,
    expected_commit: &str,
    expected_tree: &str,
    expected_sha256: &str,
) -> Result<helper::HelperResult, plurx_core::error::StoreError> {
    use plurx_core::{config::Config, error::StoreError, storage::NativeDirectoryLock};
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let refused = |message: &str| StoreError::Migration(message.to_owned());
    let build = build();
    let description = helper::describe(build.clone())?;
    if build.commit != expected_commit || build.tree != expected_tree {
        return Err(refused(
            "manual helper does not match the operator's verified source commit/tree",
        ));
    }
    let executable = std::env::current_exe().map_err(|error| refused(&error.to_string()))?;
    let mut file = plurx_core::fs_secure::open_read_nofollow_blocking(&executable)
        .map_err(|error| refused(&error.to_string()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| refused(&error.to_string()))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected_sha256 {
        return Err(refused(
            "manual helper does not match the operator's verified executable SHA256",
        ));
    }
    let config = Config::load(Some(config_path)).map_err(|error| refused(&error.to_string()))?;
    let lease = NativeDirectoryLock::acquire(&config.storage.data_dir).await?;
    let _supervisor = helper::lock_supervisor(lease.data_dir())?;
    let binding = helper::HelperBinding {
        daemon: build.clone(),
        helper: description,
        executable_sha256: expected_sha256.to_owned(),
        // This explicit bootstrap takes the verified raw executable as its
        // artifact. Managed selection instead binds the signed archive digest.
        bundle_sha256: expected_sha256.to_owned(),
    };
    let request = match helper::pending(lease.data_dir())? {
        // Explicit operator trust is in the verified executable, not a newly
        // invented archive. Resume the original request and its archive hash.
        Some(request)
            if request.binding.daemon == binding.daemon
                && request.binding.helper == binding.helper
                && request.binding.executable_sha256 == binding.executable_sha256 =>
        {
            request
        }
        Some(_) => {
            return Err(refused(
                "pending migration requires its original verified helper",
            ))
        }
        None => helper::prepare(&config, &lease, binding)?,
    };
    let root = lease.data_dir().to_owned();
    drop(lease);
    let result = helper::run(
        &root,
        &config.credential_key_path(),
        &request.id,
        &request.sha256,
        build,
    )
    .await?;
    let lease = NativeDirectoryLock::acquire(&root).await?;
    let path = plurx_core::storage::mode::owned_path(
        &root,
        &std::path::Path::new("storage-migrations")
            .join(&request.id)
            .join("result.json"),
    )?;
    let recorded: helper::HelperResult = plurx_core::storage::mode::read_record(&path)?
        .ok_or_else(|| refused("manual helper lost its completion receipt"))?;
    if result != recorded {
        return Err(refused(
            "manual helper result differs from its durable receipt",
        ));
    }
    helper::validate_completed(&lease, &request)
}

#[cfg(not(unix))]
async fn migrate_private(
    _config_path: &std::path::Path,
    _expected_commit: &str,
    _expected_tree: &str,
    _expected_sha256: &str,
) -> Result<helper::HelperResult, plurx_core::error::StoreError> {
    Err(plurx_core::error::StoreError::Migration(
        "manual private migration is supported on macOS and Linux".into(),
    ))
}

async fn run_prepared(
    root: &std::path::Path,
    key: &std::path::Path,
    request: &str,
    request_sha256: &str,
) -> Result<helper::HelperResult, plurx_core::error::StoreError> {
    #[cfg(unix)]
    {
        helper::run(root, key, request, request_sha256, build()).await
    }
    #[cfg(not(unix))]
    {
        let _ = (root, key, request, request_sha256);
        Err(plurx_core::error::StoreError::Migration(
            "the managed migration executable is supported on macOS and Linux".into(),
        ))
    }
}
