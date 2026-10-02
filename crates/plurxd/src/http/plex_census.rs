//! The durable half of the Plex façade census (C-07 §8.5).
//!
//! `plurx_plex_requests_total` is an in-process counter: it starts at zero
//! every time `plurxd` starts. §8.5's closing rule needs seven consecutive,
//! gap-free days of it on every node, and on this fleet's deploy cadence no
//! process lives a week — every node restarted within ten minutes of every
//! other on 2026-10-02 and read zero with 8–20 minutes of coverage. So the
//! question "has any Plex-family client called this node" could never be
//! closed by reading the counter, however carefully. §8.5 named the fix:
//! persist the 56 cells.
//!
//! This is that, and nothing more:
//!
//! * The 56 cumulative counts, the census start time and an honest record of
//!   every unclean stop live in one small JSON file, `plex-census.json`, in
//!   the node's data directory — beside `node.id` and the credential key, the
//!   other node-local state that must survive a restart. It is deliberately
//!   **not** a Store row, replicated or sidecar: a census of one node's
//!   requests must not be replicated (one node's answer would overwrite
//!   another's), and the node-local telemetry sidecar is reached through the
//!   Store on both backends, so persisting 57 integers there would take a
//!   schema bump on each backend and new Store methods, and would make a
//!   measure-only instrument depend on the store being healthy enough to
//!   write.
//! * Nothing is written on the request path. A request is still one relaxed
//!   atomic add on the existing cell. A background loop writes the file every
//!   [`FLUSH_INTERVAL`], and the daemon writes it once more after the HTTP
//!   server has stopped, marking the stop clean.
//! * A crash loses at most the counts recorded since the last successful
//!   write. The next start sees that the previous process never recorded a
//!   clean stop, counts it, and adds the window from that last write to the
//!   new start to `gap_seconds` — an upper bound on the uncounted stretch,
//!   since the crash instant itself is not known. The persisted counts are
//!   therefore a **lower bound** on façade usage, which is exactly what the
//!   census needs: its question is "is any cell non-zero", and a lower bound
//!   that is non-zero answers it.
//! * A missing, oversized, unreadable or corrupt file starts a new census with
//!   the reason logged; a corrupt one is kept aside as `plex-census.json.corrupt`
//!   for inspection. The daemon never refuses to start over this file.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{PlexCensus, PLEX_HANDLERS, PLEX_OUTCOMES};

pub(super) const CELLS: usize = PLEX_HANDLERS.len() * PLEX_OUTCOMES.len();
/// The census file, a direct child of the node's data directory.
pub(crate) const CENSUS_FILE: &str = "plex-census.json";
const CORRUPT_FILE: &str = "plex-census.json.corrupt";
/// How often the counts reach disk, and so the most a crash can lose.
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// A well-formed file is about 2.5 KiB; anything past this is not one.
const MAX_FILE_BYTES: u64 = 16 * 1024;
/// Cells a newer build wrote for a handler this build does not know are kept
/// and written back, so a rollback and roll-forward cannot erase a non-zero
/// count. They are never exposed (the label space stays closed), and their
/// number is bounded so the file cannot grow without limit.
const MAX_CARRIED_CELLS: usize = 64;
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CensusFile {
    version: u32,
    /// Unix seconds this census began on this node.
    started_unix_s: u64,
    /// Unix seconds of the write that produced this file.
    last_flush_unix_s: u64,
    /// True only on the write the daemon makes after its HTTP server stopped.
    stopped_cleanly: bool,
    unclean_stops: u64,
    gap_seconds: u64,
    /// `"<handler>/<outcome>"` → requests since `started_unix_s`.
    cells: BTreeMap<String, u64>,
}

fn cell_key(index: usize) -> String {
    format!(
        "{}/{}",
        PLEX_HANDLERS[index / PLEX_OUTCOMES.len()],
        PLEX_OUTCOMES[index % PLEX_OUTCOMES.len()]
    )
}

fn cell_index(key: &str) -> Option<usize> {
    let (handler, outcome) = key.split_once('/')?;
    let handler = PLEX_HANDLERS.iter().position(|known| *known == handler)?;
    let outcome = PLEX_OUTCOMES.iter().position(|known| *known == outcome)?;
    Some(handler * PLEX_OUTCOMES.len() + outcome)
}

/// How this process's census began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CensusStart {
    /// The file was read; `unclean` says the previous process never recorded
    /// a clean stop.
    Continued { unclean: bool },
    /// A new census, and why; `unusable` when a file was there but could not
    /// be used (and was kept aside), rather than simply absent.
    Fresh { reason: String, unusable: bool },
}

/// The persisted census for this node, restored once at startup.
pub(crate) struct CensusLedger {
    data_dir: PathBuf,
    started_unix_s: u64,
    unclean_stops: u64,
    gap_seconds: u64,
    /// Counts since `started_unix_s` that earlier processes recorded. This
    /// process's own counts are the live cells; the census is their sum.
    base: [u64; CELLS],
    carried: BTreeMap<String, u64>,
    last_flush_unix_s: AtomicU64,
    flush_failures: AtomicU64,
    /// Serialises writes, and is `true` once the clean-stop write landed, so
    /// a periodic write that loses the race to it cannot overwrite the clean
    /// mark with `stopped_cleanly: false`.
    finished: tokio::sync::Mutex<bool>,
}

enum Loaded {
    File(CensusFile),
    Missing,
    Unusable { reason: String, keep_aside: bool },
}

fn parse(bytes: &[u8]) -> Result<CensusFile, String> {
    let file: CensusFile =
        serde_json::from_slice(bytes).map_err(|error| format!("not a census file: {error}"))?;
    if file.version != FORMAT_VERSION {
        return Err(format!(
            "census format v{} is not the v{FORMAT_VERSION} this build reads",
            file.version
        ));
    }
    if file.cells.len() > CELLS + MAX_CARRIED_CELLS {
        return Err(format!(
            "{} cells is more than any build writes",
            file.cells.len()
        ));
    }
    Ok(file)
}

async fn load(path: &Path) -> Loaded {
    match plurx_core::fs_secure::read_bounded_regular(path, MAX_FILE_BYTES).await {
        Ok(bytes) => match parse(&bytes) {
            Ok(file) => Loaded::File(file),
            Err(reason) => Loaded::Unusable {
                reason,
                keep_aside: true,
            },
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Loaded::Missing,
        Err(error) => Loaded::Unusable {
            reason: format!("unreadable: {error}"),
            keep_aside: tokio::fs::symlink_metadata(path)
                .await
                .is_ok_and(|metadata| metadata.is_file()),
        },
    }
}

impl CensusLedger {
    /// Read the census file in `data_dir`, or begin a new census at `now`.
    /// Never fails: an unusable file is a logged new census, not a refusal.
    pub(super) async fn restore(data_dir: &Path, now_unix_s: u64) -> (Self, CensusStart) {
        let path = data_dir.join(CENSUS_FILE);
        let fresh = |reason: String, unusable: bool| {
            (
                Self {
                    data_dir: data_dir.to_owned(),
                    started_unix_s: now_unix_s,
                    unclean_stops: 0,
                    gap_seconds: 0,
                    base: [0; CELLS],
                    carried: BTreeMap::new(),
                    last_flush_unix_s: AtomicU64::new(0),
                    flush_failures: AtomicU64::new(0),
                    finished: tokio::sync::Mutex::new(false),
                },
                CensusStart::Fresh { reason, unusable },
            )
        };
        match load(&path).await {
            Loaded::File(file) => {
                let mut base = [0; CELLS];
                let mut carried = BTreeMap::new();
                for (key, count) in file.cells {
                    match cell_index(&key) {
                        Some(index) => base[index] = count,
                        None if carried.len() < MAX_CARRIED_CELLS => {
                            carried.insert(key, count);
                        }
                        None => {}
                    }
                }
                if !carried.is_empty() {
                    tracing::warn!(
                        cells = carried.len(),
                        "the Plex façade census file holds cells for handlers this build does not \
                         know; they are kept and written back but not exposed"
                    );
                }
                let unclean = !file.stopped_cleanly;
                let (unclean_stops, gap_seconds) = if unclean {
                    (
                        file.unclean_stops.saturating_add(1),
                        file.gap_seconds
                            .saturating_add(now_unix_s.saturating_sub(file.last_flush_unix_s)),
                    )
                } else {
                    (file.unclean_stops, file.gap_seconds)
                };
                if unclean {
                    tracing::warn!(
                        last_flush_unix_s = file.last_flush_unix_s,
                        now_unix_s,
                        unclean_stops,
                        gap_seconds,
                        "the previous process stopped without recording its Plex façade census; \
                         requests after its last write are lost and the window is counted as a gap"
                    );
                }
                (
                    Self {
                        data_dir: data_dir.to_owned(),
                        started_unix_s: file.started_unix_s,
                        unclean_stops,
                        gap_seconds,
                        base,
                        carried,
                        last_flush_unix_s: AtomicU64::new(file.last_flush_unix_s),
                        flush_failures: AtomicU64::new(0),
                        finished: tokio::sync::Mutex::new(false),
                    },
                    CensusStart::Continued { unclean },
                )
            }
            Loaded::Missing => fresh("no census file".to_owned(), false),
            Loaded::Unusable { reason, keep_aside } => {
                if keep_aside {
                    if let Err(error) = tokio::fs::rename(&path, data_dir.join(CORRUPT_FILE)).await
                    {
                        tracing::warn!(%error, "could not keep the unusable census file aside");
                    }
                }
                fresh(reason, true)
            }
        }
    }

    /// Say once how this census began: INFO for a continued census or a first
    /// one, WARN when a file was there and could not be used.
    pub(super) fn log_start(&self, start: &CensusStart) {
        let path = self.data_dir.join(CENSUS_FILE);
        match start {
            CensusStart::Continued { unclean } => tracing::info!(
                started_unix_s = self.started_unix_s,
                unclean_stops = self.unclean_stops,
                gap_seconds = self.gap_seconds,
                previous_stop_unclean = *unclean,
                "continuing the Plex façade census"
            ),
            CensusStart::Fresh {
                reason,
                unusable: false,
            } => tracing::info!(
                path = %path.display(),
                %reason,
                "starting a new Plex façade census"
            ),
            CensusStart::Fresh {
                reason,
                unusable: true,
            } => tracing::warn!(
                path = %path.display(),
                %reason,
                "starting a new Plex façade census: the census file was unusable and is kept aside"
            ),
        }
    }

    /// The census: what earlier processes persisted plus this process's live
    /// cells. Monotone, because both halves are.
    pub(super) fn since_census(&self, live: &[AtomicU64; CELLS]) -> [u64; CELLS] {
        std::array::from_fn(|index| {
            self.base[index].saturating_add(live[index].load(Ordering::Relaxed))
        })
    }

    /// Write the census. `stopping` marks the stop clean and is the last write
    /// this process makes; any later call is a no-op that returns `false`.
    pub(super) async fn flush(
        &self,
        live: &[AtomicU64; CELLS],
        now_unix_s: u64,
        stopping: bool,
    ) -> io::Result<bool> {
        let mut finished = self.finished.lock().await;
        if *finished {
            return Ok(false);
        }
        let mut cells = self.carried.clone();
        for (index, count) in self.since_census(live).into_iter().enumerate() {
            cells.insert(cell_key(index), count);
        }
        let file = CensusFile {
            version: FORMAT_VERSION,
            started_unix_s: self.started_unix_s,
            last_flush_unix_s: now_unix_s,
            stopped_cleanly: stopping,
            unclean_stops: self.unclean_stops,
            gap_seconds: self.gap_seconds,
            cells,
        };
        let written = match serde_json::to_vec_pretty(&file) {
            Ok(bytes) => {
                plurx_core::fs_secure::atomic_write_child(&self.data_dir, CENSUS_FILE, &bytes).await
            }
            Err(error) => Err(io::Error::other(error)),
        };
        match written {
            Ok(()) => {
                self.last_flush_unix_s.store(now_unix_s, Ordering::Relaxed);
                *finished = stopping;
                Ok(true)
            }
            Err(error) => {
                self.flush_failures.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    pub(super) fn prometheus(&self, live: &[AtomicU64; CELLS]) -> String {
        let mut out = String::from(
            "# HELP plurx_plex_requests_since_census_total Plex-compat façade requests by handler and outcome since this node's census began (plurx_plex_census_started_seconds), persisted across restarts. A lower bound: a crash loses the requests after the last write, and plurx_plex_census_gap_seconds bounds that window.\n\
             # TYPE plurx_plex_requests_since_census_total counter\n",
        );
        for (index, count) in self.since_census(live).into_iter().enumerate() {
            let handler = PLEX_HANDLERS[index / PLEX_OUTCOMES.len()];
            let outcome = PLEX_OUTCOMES[index % PLEX_OUTCOMES.len()];
            out.push_str(&format!(
                "plurx_plex_requests_since_census_total{{handler=\"{handler}\",outcome=\"{outcome}\"}} {count}\n"
            ));
        }
        out.push_str(&format!(
            "# HELP plurx_plex_census_started_seconds Unix time this node's Plex façade census began. It moves only when the census file was missing or unusable at startup.\n\
             # TYPE plurx_plex_census_started_seconds gauge\n\
             plurx_plex_census_started_seconds {}\n\
             # HELP plurx_plex_census_last_flush_seconds Unix time of the last successful census write on this node; 0 before the first.\n\
             # TYPE plurx_plex_census_last_flush_seconds gauge\n\
             plurx_plex_census_last_flush_seconds {}\n\
             # HELP plurx_plex_census_unclean_stops_total Processes on this node that stopped without recording the census since it began.\n\
             # TYPE plurx_plex_census_unclean_stops_total counter\n\
             plurx_plex_census_unclean_stops_total {}\n\
             # HELP plurx_plex_census_gap_seconds Seconds of census time whose requests may be missing: for each unclean stop, from the last census write to the next start. An upper bound, since the crash instant is not recorded.\n\
             # TYPE plurx_plex_census_gap_seconds gauge\n\
             plurx_plex_census_gap_seconds {}\n\
             # HELP plurx_plex_census_flush_failures_total Census writes that failed in this process. The counts stay in memory and the next write retries them.\n\
             # TYPE plurx_plex_census_flush_failures_total counter\n\
             plurx_plex_census_flush_failures_total {}\n",
            self.started_unix_s,
            self.last_flush_unix_s.load(Ordering::Relaxed),
            self.unclean_stops,
            self.gap_seconds,
            self.flush_failures.load(Ordering::Relaxed),
        ));
        out
    }
}

pub(crate) fn unix_now_s() -> u64 {
    u64::try_from(super::users::unix_now()).unwrap_or(0)
}

/// Writes the census every [`FLUSH_INTERVAL`] until shutdown. The clean-stop
/// write is the daemon's, after its HTTP server stops, not this loop's: this
/// loop is cancelled while requests may still be in flight.
pub(crate) async fn flush_loop(census: Arc<PlexCensus>, shutdown: CancellationToken) {
    let mut ticker =
        tokio::time::interval_at(tokio::time::Instant::now() + FLUSH_INTERVAL, FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failing = false;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticker.tick() => {}
        }
        match census.flush_durable(unix_now_s(), false).await {
            Ok(_) if failing => {
                failing = false;
                tracing::info!("Plex façade census writes recovered");
            }
            Ok(_) => {}
            Err(error) => {
                if !failing {
                    tracing::warn!(
                        %error,
                        "could not write the Plex façade census; counts stay in memory and every \
                         later write retries them"
                    );
                }
                failing = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    const SECTION_ALL: usize = 4;
    const PART: usize = 8;

    fn cell(census: &PlexCensus, handler: &str, outcome: &str) -> u64 {
        let index = cell_index(&format!("{handler}/{outcome}")).expect("known cell");
        census.since_census().expect("restored census")[index]
    }

    fn ledger(census: &PlexCensus) -> &CensusLedger {
        census.ledger.get().expect("restored census")
    }

    async fn restored(dir: &Path, now: u64) -> PlexCensus {
        restored_with_start(dir, now).await.0
    }

    async fn restored_with_start(dir: &Path, now: u64) -> (PlexCensus, CensusStart) {
        let census = PlexCensus::default();
        let start = census.restore_durable(dir, now).await;
        (census, start)
    }

    fn read_file(dir: &Path) -> CensusFile {
        parse(&std::fs::read(dir.join(CENSUS_FILE)).expect("census file")).expect("valid file")
    }

    #[test]
    fn census_cell_keys_round_trip_and_name_only_known_cells() {
        assert_eq!(PLEX_HANDLERS[SECTION_ALL], "section_all");
        assert_eq!(PLEX_HANDLERS[PART], "part");
        for index in 0..CELLS {
            assert_eq!(cell_index(&cell_key(index)), Some(index));
        }
        assert_eq!(
            cell_index("section_all/ok"),
            Some(SECTION_ALL * PLEX_OUTCOMES.len())
        );
        assert_eq!(cell_index("section_all/teapot"), None);
        assert_eq!(cell_index("future_handler/ok"), None);
        assert_eq!(cell_index("section_all"), None);
    }

    #[tokio::test]
    async fn a_plex_census_survives_a_clean_restart_and_stays_monotone() {
        let dir = crate::test_tempdir().expect("tempdir");
        let (first, start) = restored_with_start(dir.path(), 1_000).await;
        assert_eq!(
            start,
            CensusStart::Fresh {
                reason: "no census file".to_owned(),
                unusable: false,
            }
        );
        for _ in 0..3 {
            first.record(SECTION_ALL, StatusCode::OK);
        }
        first.record(PART, StatusCode::UNAUTHORIZED);
        assert!(first.flush_durable(1_100, true).await.expect("clean stop"));
        let before = first.since_census().expect("census");
        drop(first);

        let (second, start) = restored_with_start(dir.path(), 1_200).await;
        assert_eq!(start, CensusStart::Continued { unclean: false });
        let after = second.since_census().expect("census");
        assert_eq!(after, before, "a clean restart loses nothing");
        assert_eq!(cell(&second, "section_all", "ok"), 3);
        assert_eq!(cell(&second, "part", "unauthorized"), 1);
        second.record(SECTION_ALL, StatusCode::OK);
        second.record(SECTION_ALL, StatusCode::NOT_FOUND);
        let later = second.since_census().expect("census");
        assert!(later
            .iter()
            .zip(before.iter())
            .all(|(now, then)| now >= then));
        assert_eq!(cell(&second, "section_all", "ok"), 4);
        assert_eq!(cell(&second, "section_all", "not_found"), 1);
        // The process counter keeps its in-process meaning.
        let process = second.prometheus();
        assert!(process
            .contains("plurx_plex_requests_total{handler=\"section_all\",outcome=\"ok\"} 1\n"));
        assert!(process.contains(
            "plurx_plex_requests_since_census_total{handler=\"section_all\",outcome=\"ok\"} 4\n"
        ));
        let ledger = ledger(&second);
        assert_eq!(ledger.started_unix_s, 1_000);
        assert_eq!(ledger.unclean_stops, 0);
        assert_eq!(ledger.gap_seconds, 0);
    }

    #[tokio::test]
    async fn a_crash_loses_at_most_the_plex_census_counts_since_the_last_write() {
        let dir = crate::test_tempdir().expect("tempdir");
        let first = restored(dir.path(), 1_000).await;
        // Restoring writes at once, so a crash before the first periodic
        // write still has a bounded window.
        assert_eq!(read_file(dir.path()).last_flush_unix_s, 1_000);
        assert!(!read_file(dir.path()).stopped_cleanly);
        for _ in 0..4 {
            first.record(SECTION_ALL, StatusCode::OK);
        }
        assert!(first
            .flush_durable(1_060, false)
            .await
            .expect("periodic write"));
        for _ in 0..7 {
            first.record(SECTION_ALL, StatusCode::OK);
        }
        // A crash: no clean-stop write.
        drop(first);

        let (second, start) = restored_with_start(dir.path(), 1_300).await;
        assert_eq!(start, CensusStart::Continued { unclean: true });
        // Exactly the seven requests after the last write are lost: the
        // census is a lower bound, never an over-count.
        assert_eq!(cell(&second, "section_all", "ok"), 4);
        assert_eq!(ledger(&second).unclean_stops, 1);
        assert_eq!(ledger(&second).gap_seconds, 1_300 - 1_060);
        assert_eq!(ledger(&second).started_unix_s, 1_000);
        // The gap is persisted at once, so a second crash cannot forget it.
        let on_disk = read_file(dir.path());
        assert_eq!((on_disk.unclean_stops, on_disk.gap_seconds), (1, 240));
        drop(second);

        let third = restored(dir.path(), 1_310).await;
        assert_eq!(ledger(&third).unclean_stops, 2);
        assert_eq!(ledger(&third).gap_seconds, 240 + 10);
        assert!(third.flush_durable(1_400, true).await.expect("clean stop"));
        drop(third);
        let fourth = restored(dir.path(), 1_500).await;
        assert_eq!(
            ledger(&fourth).unclean_stops,
            2,
            "a clean stop is not a gap"
        );
        assert_eq!(ledger(&fourth).gap_seconds, 250);
        assert_eq!(cell(&fourth, "section_all", "ok"), 4);
    }

    #[tokio::test]
    async fn the_plex_census_clean_stop_write_is_final() {
        let dir = crate::test_tempdir().expect("tempdir");
        let census = restored(dir.path(), 1_000).await;
        census.record(SECTION_ALL, StatusCode::OK);
        assert!(census.flush_durable(1_050, true).await.expect("clean stop"));
        census.record(SECTION_ALL, StatusCode::OK);
        // A periodic write that lost the race to the clean stop must not
        // overwrite the clean mark.
        assert!(!census.flush_durable(1_060, false).await.expect("no-op"));
        let on_disk = read_file(dir.path());
        assert!(on_disk.stopped_cleanly);
        assert_eq!(on_disk.last_flush_unix_s, 1_050);
    }

    #[tokio::test]
    async fn an_unusable_plex_census_file_starts_a_new_census_without_failing() {
        let cases: [(&str, Vec<u8>); 4] = [
            ("garbage", b"{ not json".to_vec()),
            (
                "wrong version",
                br#"{"version":2,"started_unix_s":1,"last_flush_unix_s":1,"stopped_cleanly":true,"unclean_stops":0,"gap_seconds":0,"cells":{}}"#.to_vec(),
            ),
            (
                "unknown field",
                br#"{"version":1,"started_unix_s":1,"last_flush_unix_s":1,"stopped_cleanly":true,"unclean_stops":0,"gap_seconds":0,"cells":{},"extra":1}"#.to_vec(),
            ),
            ("oversized", vec![b' '; MAX_FILE_BYTES as usize + 1]),
        ];
        for (name, bytes) in cases {
            let dir = crate::test_tempdir().expect("tempdir");
            std::fs::write(dir.path().join(CENSUS_FILE), &bytes).expect("seed");
            let (census, start) = restored_with_start(dir.path(), 5_000).await;
            assert!(
                matches!(start, CensusStart::Fresh { unusable: true, .. }),
                "{name}: {start:?}"
            );
            assert_eq!(ledger(&census).started_unix_s, 5_000, "{name}");
            assert!(
                census
                    .since_census()
                    .expect("census")
                    .iter()
                    .all(|count| *count == 0),
                "{name}"
            );
            assert_eq!(
                std::fs::read(dir.path().join(CORRUPT_FILE)).expect("kept aside"),
                bytes,
                "{name}"
            );
            // The new census is written whole, and the next start reads it.
            assert_eq!(read_file(dir.path()).started_unix_s, 5_000, "{name}");
            drop(census);
            let (_again, start) = restored_with_start(dir.path(), 5_100).await;
            assert!(matches!(start, CensusStart::Continued { .. }), "{name}");
        }
    }

    #[tokio::test]
    async fn the_plex_census_exposition_is_exactly_the_known_cells() {
        let dir = crate::test_tempdir().expect("tempdir");
        // A cell a newer build wrote for a handler this build does not know.
        let mut cells = BTreeMap::new();
        cells.insert("future_handler/ok".to_owned(), 9);
        cells.insert("section_all/ok".to_owned(), 2);
        let seeded = CensusFile {
            version: FORMAT_VERSION,
            started_unix_s: 700,
            last_flush_unix_s: 900,
            stopped_cleanly: true,
            unclean_stops: 0,
            gap_seconds: 0,
            cells,
        };
        std::fs::write(
            dir.path().join(CENSUS_FILE),
            serde_json::to_vec(&seeded).expect("json"),
        )
        .expect("seed");
        let census = restored(dir.path(), 1_000).await;
        let exposition = census.prometheus();
        let lines = exposition
            .lines()
            .filter(|line| line.starts_with("plurx_plex_requests_since_census_total{"))
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), CELLS);
        let mut index = 0;
        for handler in PLEX_HANDLERS {
            for outcome in PLEX_OUTCOMES {
                let expected = if handler == "section_all" && outcome == "ok" {
                    2
                } else {
                    0
                };
                assert_eq!(
                    lines[index],
                    format!(
                        "plurx_plex_requests_since_census_total{{handler=\"{handler}\",outcome=\"{outcome}\"}} {expected}"
                    )
                );
                index += 1;
            }
        }
        assert!(!exposition.contains("future_handler"));
        for (series, value) in [
            ("plurx_plex_census_started_seconds", 700),
            ("plurx_plex_census_last_flush_seconds", 1_000),
            ("plurx_plex_census_unclean_stops_total", 0),
            ("plurx_plex_census_gap_seconds", 0),
            ("plurx_plex_census_flush_failures_total", 0),
        ] {
            assert!(
                exposition.contains(&format!("\n{series} {value}\n")),
                "{series} {value} in {exposition}"
            );
            assert!(exposition.contains(&format!("# TYPE {series} ")));
        }
        // The unknown cell is carried, not dropped.
        assert_eq!(
            read_file(dir.path()).cells.get("future_handler/ok"),
            Some(&9)
        );
        // A census that was never restored (every test router) exposes the
        // process counter alone.
        let bare = PlexCensus::default().prometheus();
        assert!(!bare.contains("plurx_plex_requests_since_census_total"));
        assert!(!bare.contains("plurx_plex_census_"));
    }
}
