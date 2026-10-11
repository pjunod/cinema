#![forbid(unsafe_code)]
use clap::{Parser, Subcommand};
use plurx_notification_broker::{
    config::{self, Manifest},
    http::Broker,
    listener::BoundedListener,
    provider::{self, TlsTransport},
    store::Store,
    Error, Result,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
#[derive(Parser)]
#[command(about = "Cinema fixed-purpose notification broker")]
struct Cli {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    database: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create a new database. Never replaces an existing DB or creates keys.
    Init,
    /// Serve an existing database; use verified HTTPS reverse proxy externally.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8769")]
        listen: SocketAddr,
    },
    /// Offline destructive capability fence after explicit external rotation.
    RestoreFence,
}
async fn run() -> Result<()> {
    let cli = Cli::parse();
    let manifest = Manifest::load(&cli.manifest)?;
    let key = config::read_secret(&manifest.master_key_file, 32)?;
    let key: &[u8; 32] = key
        .as_slice()
        .try_into()
        .map_err(|_| Error::unavailable())?;
    let records = manifest
        .publishers
        .iter()
        .map(|value| value.record())
        .collect::<Vec<_>>();
    let now = provider::now()?;
    match cli.command {
        Command::Init => Store::initialize(
            &cli.database,
            &manifest.generation,
            key,
            config::STORAGE_REALM,
            &records,
            now,
        ),
        Command::RestoreFence => Store::restore_fence(
            &cli.database,
            &manifest.generation,
            key,
            config::STORAGE_REALM,
            &records,
            now,
        ),
        Command::Serve { listen } => {
            let store = Store::open(
                &cli.database,
                &manifest.generation,
                key,
                config::STORAGE_REALM,
            )?;
            let broker = Broker::new(
                manifest.generation,
                store,
                manifest.publishers,
                Arc::new(TlsTransport::new()?),
                now,
            )?;
            let listener = tokio::net::TcpListener::bind(listen)
                .await
                .map_err(|_| Error::unavailable())?;
            axum::serve(BoundedListener::new(listener), broker.router())
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .map_err(|_| Error::unavailable())
        }
    }
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Cinema broker: {}", error.code);
            std::process::ExitCode::FAILURE
        }
    }
}
