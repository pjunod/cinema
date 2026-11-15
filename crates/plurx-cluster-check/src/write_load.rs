//! A back-to-back writer for the leader-shutdown load case.
//!
//! One writer per voter process inserts `(writer, seq)` rows through the
//! ordinary `Client::execute` path, so a follower's writes forward to the
//! leader exactly as a daemon's do. The case starts writers on the leader and
//! a follower, hands leadership off under them, stops them, and compares what
//! each writer saw acknowledged with the rows the cluster committed.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use hiqlite::macros::params;
use hiqlite::{Client, Row};
use tokio_util::sync::CancellationToken;

/// What one writer observed.
#[derive(Debug, Default)]
pub(crate) struct WriteLoadReport {
    pub(crate) acknowledged: u64,
    pub(crate) failed: u64,
    pub(crate) first_error: Option<String>,
    pub(crate) slowest_ms: u64,
}

struct Running {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<WriteLoadReport>,
}

/// One writer per voter process.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

pub(crate) async fn start(client: Client, writer: u64) -> Result<()> {
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS c2_handoff_load (\
               writer INTEGER NOT NULL, seq INTEGER NOT NULL, \
               PRIMARY KEY (writer, seq)) STRICT",
            params!(),
        )
        .await
        .context("create the write-load table")?;
    let stop = CancellationToken::new();
    let token = stop.clone();
    let task = tokio::spawn(async move {
        let mut report = WriteLoadReport::default();
        let writer = i64::try_from(writer).unwrap_or(i64::MAX);
        let mut seq = 0_i64;
        while !token.is_cancelled() {
            let started = Instant::now();
            // Each attempt uses a fresh sequence number, so a write that
            // failed but committed shows up as a row no writer acknowledged.
            seq += 1;
            match client
                .execute(
                    "INSERT INTO c2_handoff_load (writer, seq) VALUES ($1, $2)",
                    params!(writer, seq),
                )
                .await
            {
                Ok(_) => {
                    report.acknowledged += 1;
                    let took = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    report.slowest_ms = report.slowest_ms.max(took);
                }
                Err(error) => {
                    report.failed += 1;
                    report.first_error.get_or_insert_with(|| error.to_string());
                    // A failed write is the finding; pacing the loop keeps a
                    // leaderless stretch from flooding the report.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        report
    });
    let mut running = RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if running.is_some() {
        stop.cancel();
        bail!("this voter already runs a writer");
    }
    *running = Some(Running { stop, task });
    Ok(())
}

pub(crate) async fn stop() -> Result<WriteLoadReport> {
    let running = RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .context("this voter runs no writer")?;
    running.stop.cancel();
    running.task.await.context("join the writer")
}

struct CountRow {
    count: i64,
}

impl From<&mut Row<'_>> for CountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            count: row.get("count"),
        }
    }
}

pub(crate) async fn count(client: &Client, writer: u64) -> Result<u64> {
    let writer = i64::try_from(writer)?;
    let rows = client
        .query_consistent_map::<CountRow, _>(
            "SELECT COUNT(*) AS count FROM c2_handoff_load WHERE writer = $1",
            params!(writer),
        )
        .await?;
    let count = rows.first().map_or(0, |row| row.count);
    Ok(u64::try_from(count)?)
}
