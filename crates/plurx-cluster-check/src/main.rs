//! Entry point for the separate-process cluster harness.
//!
//! Every mode lives in the crate's library so the harness itself is reachable
//! from tests. This file stays a wrapper on purpose: `cargo run -p
//! plurx-cluster-check -- check` behaves exactly as it did when the same code
//! was a single binary target.

// A harness that starts plurxd processes on purpose; it is never a
// daemon child, so the launcher rule in clippy.toml does not apply.
#![allow(clippy::disallowed_methods)]

use anyhow::Result;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    match plurx_cluster_check::run(std::env::args().collect::<Vec<_>>()).await {
        Ok(()) => Ok(()),
        Err(error) if plurx_cluster_check::is_port_collision(&error) => {
            eprintln!("{error:#}");
            std::process::exit(plurx_cluster_check::BIND_FAILURE_EXIT);
        }
        Err(error) => Err(error),
    }
}
