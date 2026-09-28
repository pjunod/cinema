// K-05 section 3.3: the read-pool bench. The Home request shape (token
// authentication, the library previews, the page's watch map and rollups,
// Continue Watching and Next Up) runs 256 times across 32 concurrent
// workers against the catalogue fixture, while a scan-shaped writer
// upserts 50 files a second through the same store. One process per pool
// size, so the peak RSS it reports is that configuration's alone.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use plurx_core::domain::ProbeResult;
use plurx_core::store::{MediaStore, SqliteStore, UserStore, WatchStore};

const WORKERS: usize = 32;
const ITERATIONS: usize = 256;
const WRITES_PER_SECOND: u64 = 50;
/// Every show's first item id in the fixture (`calls.rs` explains the ids).
const FIRST_SHOW: i64 = 20_001;
const SHOW_STRIDE: i64 = 105;

fn percentile(samples: &mut [Duration], percentile: f64) -> Duration {
    samples.sort();
    let rank = ((samples.len() as f64) * percentile).ceil() as usize;
    samples[rank.clamp(1, samples.len()) - 1]
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

async fn home(
    store: &SqliteStore,
    user: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    store
        .authenticate_token(&format!("fixture-token-{user}"))
        .await?;
    let pages = store.home_preview_pages(24).await?;
    let ids: Vec<i64> = pages
        .iter()
        .flat_map(|page| page.items.iter().map(|item| item.id))
        .collect();
    store.watch_map(user, &ids).await?;
    let shows: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| *id >= FIRST_SHOW && (id - FIRST_SHOW) % SHOW_STRIDE == 0)
        .collect();
    store.watch_rollups(user, &shows).await?;
    store.continue_watching(user, 24).await?;
    store.next_up(user, 24).await?;
    Ok(())
}

pub fn run(fixture: &Path, reads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let store = Arc::new(SqliteStore::open_with_read_connections(fixture, reads)?);
        // One warm-up pass, untimed, so every configuration starts warm.
        home(&store, 1).await.map_err(|error| error.to_string())?;

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let store = Arc::clone(&store);
            let stop = Arc::clone(&stop);
            tokio::spawn(async move {
                let mut latencies = Vec::new();
                let mut tick =
                    tokio::time::interval(Duration::from_millis(1_000 / WRITES_PER_SECOND));
                let mut n = 0_i64;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or(0);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    tick.tick().await;
                    let started = Instant::now();
                    store
                        .upsert_file(
                            1 + n % 20_000,
                            &format!("/bench/{stamp}/{n:08}.mkv"),
                            1_000_000,
                            0,
                            &ProbeResult::default(),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    latencies.push(started.elapsed());
                    n += 1;
                }
                Ok::<_, String>(latencies)
            })
        };

        let started = Instant::now();
        let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let workers = (0..WORKERS)
            .map(|worker| {
                let store = Arc::clone(&store);
                let next = Arc::clone(&next);
                tokio::spawn(async move {
                    let mut latencies = Vec::new();
                    while next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < ITERATIONS {
                        let user = 1 + (worker as i64) % 5;
                        let began = Instant::now();
                        home(&store, user)
                            .await
                            .map_err(|error| error.to_string())?;
                        latencies.push(began.elapsed());
                    }
                    Ok::<_, String>(latencies)
                })
            })
            .collect::<Vec<_>>();
        let mut reads_ms = Vec::with_capacity(ITERATIONS);
        for worker in workers {
            reads_ms.extend(worker.await??);
        }
        let wall = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut writes = writer.await??;
        println!(
            "| {reads} | {} | {:.1} | {:.1} | {:.1} | {} | {:.1} | {:.1} | {} |",
            reads_ms.len(),
            percentile(&mut reads_ms, 0.50).as_secs_f64() * 1e3,
            percentile(&mut reads_ms, 0.95).as_secs_f64() * 1e3,
            wall.as_secs_f64(),
            writes.len(),
            percentile(&mut writes, 0.50).as_secs_f64() * 1e3,
            percentile(&mut writes, 0.99).as_secs_f64() * 1e3,
            peak_rss_kib() / 1024,
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
