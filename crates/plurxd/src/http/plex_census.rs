//! The durable half of the Plex façade census (C-07 §8.5, §8.7).
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
//! * The 56 cumulative counts, the census start time, the unclean-stop count
//!   and the time the node was not counting live in one small JSON file,
//!   `plex-census.json`, in the node's data directory — beside `node.id` and
//!   the credential key, the other node-local state that must survive a
//!   restart. It is deliberately **not** a Store row, replicated or sidecar: a
//!   census of one node's requests must not be replicated (one node's answer
//!   would overwrite another's), and the node-local telemetry sidecar is
//!   reached through the Store on both backends, so persisting 57 integers
//!   there would take a schema bump on each backend and new Store methods, and
//!   would make a measure-only instrument depend on the store being healthy
//!   enough to write.
//! * Nothing is written on the request path. A request is still one relaxed
//!   atomic add on the existing cell. A background loop writes the file every
//!   [`FLUSH_INTERVAL`], and the daemon writes it once more after the HTTP
//!   server has stopped — marking the stop clean only when every connection
//!   drained, since a connection still open could still be counted.
//! * **Unobserved time.** Every start adds the stretch from the file's last
//!   write to its own first trustworthy clock reading to `gap_seconds`,
//!   whether the previous process stopped cleanly or not: clean downtime, a
//!   crash window, and a whole lifetime whose writes all failed are all time
//!   this node was not counting. Census time is therefore
//!   `now − started − gap`. A crash additionally loses the counts recorded
//!   after the last write, so the persisted counts are a **lower bound** on
//!   façade usage — exactly what "is any cell non-zero" needs — and the
//!   unclean stop is counted in `unclean_stops`.
//! * **Clock floor.** A clock earlier than this build's source date
//!   ([`crate::version::BUILT_AT`]) is not trusted, and neither is a clock
//!   that cannot be read. Until the clock reaches the floor, a new census has
//!   no start time, no unobserved time is added, and nothing is written: a
//!   census started near 1970 would otherwise fake its seven days.
//! * **Unusable files.** A file that does not parse, is over the size cap,
//!   names another format version or carries an unknown field starts a new
//!   census with the reason logged at WARN. Any other read error is retried
//!   once and, if it persists, does the same. Either way the old file is kept
//!   aside as `plex-census.json.corrupt-<unix seconds>`, never overwriting an
//!   earlier one, and the daemon never refuses to start over it.
//! * **Rollback.** Cells for a handler added by a later build are carried and
//!   written back by an earlier one, so rolling back across a *new handler*
//!   keeps its count. A change of the file's format version is not: an
//!   earlier build reads a newer version as unusable and starts a new census.

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
const ASIDE_PREFIX: &str = "plex-census.json.corrupt-";
/// How often the counts reach disk, and so the most a crash can lose.
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// A well-formed file is about 2.5 KiB; anything past this is not one.
const MAX_FILE_BYTES: u64 = 16 * 1024;
/// Cells a newer build wrote for a handler this build does not know are kept
/// and written back, so rolling back across a new handler cannot erase its
/// non-zero count. They are never exposed (the label space stays closed), and
/// their number is bounded so the file cannot grow without limit.
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
    /// True only on the write the daemon makes after its HTTP server stopped
    /// with every connection drained.
    stopped_cleanly: bool,
    unclean_stops: u64,
    /// Seconds since `started_unix_s` this node was not counting.
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

/// The wall clock in unix seconds, or `None` when it cannot be read. Never 0
/// for an error: a census must not start, or count time, from a fake epoch.
pub(crate) fn unix_now_s() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// This build's source date as unix seconds: the earliest clock reading the
/// census trusts. `None` only if the stamp is malformed, which `build.rs`
/// does not produce; the census then has no floor beyond a readable clock.
pub(crate) fn build_source_floor() -> Option<u64> {
    parse_utc_stamp(crate::version::BUILT_AT)
}

/// `YYYY-MM-DDTHH:MM:SSZ` → unix seconds.
fn parse_utc_stamp(stamp: &str) -> Option<u64> {
    let bytes = stamp.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let field = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

/// How this process's census began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CensusStart {
    /// The file was read; `unclean` says the previous process never recorded
    /// a clean stop.
    Continued { unclean: bool },
    /// A new census, and why; `unusable` when something was at the census
    /// path but could not be used (and was kept aside), rather than nothing.
    Fresh { reason: String, unusable: bool },
}

/// What a write needs exclusively.
struct WriterState {
    /// `true` once the clean-stop write landed: later writes are no-ops, so a
    /// periodic write that loses the race cannot overwrite the clean mark.
    finished: bool,
    /// The previous file's last write, until the first trustworthy clock
    /// reading turns the stretch since it into unobserved time.
    unaccounted_since: Option<u64>,
}

/// The persisted census for this node, restored once at startup.
pub(crate) struct CensusLedger {
    data_dir: PathBuf,
    /// Earliest trusted clock reading (this build's source date).
    floor_unix_s: u64,
    /// 0 until the first trustworthy clock reading of a new census.
    started_unix_s: AtomicU64,
    unclean_stops: u64,
    gap_seconds: AtomicU64,
    /// Counts since the census began that earlier processes recorded. This
    /// process's own counts are the live cells; the census is their sum.
    base: [u64; CELLS],
    carried: BTreeMap<String, u64>,
    last_flush_unix_s: AtomicU64,
    flush_failures: AtomicU64,
    writer: tokio::sync::Mutex<WriterState>,
}

enum Loaded {
    File(CensusFile),
    Missing,
    /// Something is at the census path and could not be used.
    Unusable(String),
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
    // The size cap is a property of the file, not a read failure: decide it
    // before reading, so an oversized file is "unusable" and is not retried.
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await {
        if metadata.is_file() && metadata.len() > MAX_FILE_BYTES {
            return Loaded::Unusable(format!(
                "{} bytes is over the {MAX_FILE_BYTES}-byte census cap",
                metadata.len()
            ));
        }
    }
    let mut last_error = None;
    // A transient read error (EIO, EMFILE) must not reset a census on its own:
    // retry once before giving up on the file.
    for _ in 0..2 {
        match plurx_core::fs_secure::read_bounded_regular(path, MAX_FILE_BYTES).await {
            Ok(bytes) => {
                return match parse(&bytes) {
                    Ok(file) => Loaded::File(file),
                    Err(reason) => Loaded::Unusable(reason),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Loaded::Missing,
            Err(error) => last_error = Some(error),
        }
    }
    let error = last_error.map_or_else(|| "no error recorded".to_owned(), |e| e.to_string());
    Loaded::Unusable(format!("unreadable after a retry: {error}"))
}

/// Move whatever is at the census path to a name no earlier copy holds.
async fn keep_aside(data_dir: &Path, path: &Path, now_unix_s: Option<u64>) {
    let stamp = now_unix_s.map_or_else(|| "unknown-clock".to_owned(), |now| now.to_string());
    for attempt in 0..16u32 {
        let name = if attempt == 0 {
            format!("{ASIDE_PREFIX}{stamp}")
        } else {
            format!("{ASIDE_PREFIX}{stamp}-{attempt}")
        };
        let aside = data_dir.join(&name);
        match tokio::fs::symlink_metadata(&aside).await {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, aside = %aside.display(), "could not check the census aside name");
                return;
            }
        }
        if let Err(error) = tokio::fs::rename(path, &aside).await {
            tracing::warn!(%error, "could not keep the unusable census file aside");
        }
        return;
    }
    tracing::warn!("sixteen census aside names in this second are taken; the unusable file stays");
}

impl CensusLedger {
    /// Read the census file in `data_dir`, or begin a new census. Never fails:
    /// an unusable file is a logged new census, not a refusal. No unobserved
    /// time is added and no start time is set here; the first write does both
    /// at the first trustworthy clock reading.
    pub(super) async fn restore(
        data_dir: &Path,
        now_unix_s: Option<u64>,
        floor_unix_s: u64,
    ) -> (Self, CensusStart) {
        let path = data_dir.join(CENSUS_FILE);
        let ledger = |started: u64,
                      unclean_stops: u64,
                      gap: u64,
                      base: [u64; CELLS],
                      carried: BTreeMap<String, u64>,
                      last_flush: u64,
                      unaccounted_since: Option<u64>| Self {
            data_dir: data_dir.to_owned(),
            floor_unix_s,
            started_unix_s: AtomicU64::new(started),
            unclean_stops,
            gap_seconds: AtomicU64::new(gap),
            base,
            carried,
            last_flush_unix_s: AtomicU64::new(last_flush),
            flush_failures: AtomicU64::new(0),
            writer: tokio::sync::Mutex::new(WriterState {
                finished: false,
                unaccounted_since,
            }),
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
                let unclean_stops = if unclean {
                    file.unclean_stops.saturating_add(1)
                } else {
                    file.unclean_stops
                };
                if unclean {
                    tracing::warn!(
                        last_flush_unix_s = file.last_flush_unix_s,
                        unclean_stops,
                        "the previous process stopped without recording a clean Plex façade \
                         census; requests after its last write are lost"
                    );
                }
                (
                    ledger(
                        file.started_unix_s,
                        unclean_stops,
                        file.gap_seconds,
                        base,
                        carried,
                        file.last_flush_unix_s,
                        Some(file.last_flush_unix_s),
                    ),
                    CensusStart::Continued { unclean },
                )
            }
            Loaded::Missing => (
                ledger(0, 0, 0, [0; CELLS], BTreeMap::new(), 0, None),
                CensusStart::Fresh {
                    reason: "no census file".to_owned(),
                    unusable: false,
                },
            ),
            Loaded::Unusable(reason) => {
                keep_aside(data_dir, &path, now_unix_s).await;
                (
                    ledger(0, 0, 0, [0; CELLS], BTreeMap::new(), 0, None),
                    CensusStart::Fresh {
                        reason,
                        unusable: true,
                    },
                )
            }
        }
    }

    /// Say once how this census began: INFO for a continued census or a first
    /// one, WARN when something was there and could not be used.
    pub(super) fn log_start(&self, start: &CensusStart) {
        let path = self.data_dir.join(CENSUS_FILE);
        match start {
            CensusStart::Continued { unclean } => tracing::info!(
                started_unix_s = self.started_unix_s.load(Ordering::Relaxed),
                unclean_stops = self.unclean_stops,
                gap_seconds = self.gap_seconds.load(Ordering::Relaxed),
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

    /// Write the census. `clean` marks the stop clean and is the last write
    /// this process makes; any later call is a no-op that returns `false`.
    /// A clock that cannot be read or is below the build's source date writes
    /// nothing and returns `false`: the counts stay in memory.
    pub(super) async fn flush(
        &self,
        live: &[AtomicU64; CELLS],
        now_unix_s: Option<u64>,
        clean: bool,
    ) -> io::Result<bool> {
        let mut writer = self.writer.lock().await;
        if writer.finished {
            return Ok(false);
        }
        let Some(now) = now_unix_s.filter(|now| *now >= self.floor_unix_s) else {
            return Ok(false);
        };
        // The first trustworthy reading: a new census starts here, and the
        // stretch since the previous file's last write was not counted.
        if self.started_unix_s.load(Ordering::Relaxed) == 0 {
            self.started_unix_s.store(now, Ordering::Relaxed);
        }
        if let Some(since) = writer.unaccounted_since.take() {
            self.gap_seconds
                .fetch_add(now.saturating_sub(since), Ordering::Relaxed);
        }
        let mut cells = self.carried.clone();
        for (index, count) in self.since_census(live).into_iter().enumerate() {
            cells.insert(cell_key(index), count);
        }
        let file = CensusFile {
            version: FORMAT_VERSION,
            started_unix_s: self.started_unix_s.load(Ordering::Relaxed),
            last_flush_unix_s: now,
            stopped_cleanly: clean,
            unclean_stops: self.unclean_stops,
            gap_seconds: self.gap_seconds.load(Ordering::Relaxed),
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
                self.last_flush_unix_s.store(now, Ordering::Relaxed);
                writer.finished = clean;
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
            "# HELP plurx_plex_requests_since_census_total Plex-compat façade requests by handler and outcome since this node's census began (plurx_plex_census_started_seconds), persisted across restarts. A lower bound: a crash loses the requests after the last write, so it can read lower after an unclean restart; use plurx_plex_requests_total for rates.\n\
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
            "# HELP plurx_plex_census_started_seconds Unix time this node's Plex façade census began; 0 until the clock reaches this build's source date. It moves only when the census file was missing or unusable at startup.\n\
             # TYPE plurx_plex_census_started_seconds gauge\n\
             plurx_plex_census_started_seconds {}\n\
             # HELP plurx_plex_census_last_flush_seconds Unix time of the last successful census write on this node; 0 before the first.\n\
             # TYPE plurx_plex_census_last_flush_seconds gauge\n\
             plurx_plex_census_last_flush_seconds {}\n\
             # HELP plurx_plex_census_unclean_stops_total Processes on this node that stopped without a clean census write (a crash, a kill, a failed final write, or a drain that timed out) since the census began.\n\
             # TYPE plurx_plex_census_unclean_stops_total counter\n\
             plurx_plex_census_unclean_stops_total {}\n\
             # HELP plurx_plex_census_gap_seconds Seconds since the census began that this node was not counting: every stretch from a process's last census write to the next start, clean downtime and crash windows alike, including whole lifetimes whose writes all failed. Census time is now minus plurx_plex_census_started_seconds minus this.\n\
             # TYPE plurx_plex_census_gap_seconds gauge\n\
             plurx_plex_census_gap_seconds {}\n\
             # HELP plurx_plex_census_flush_failures_total Census writes that failed in this process. The counts stay in memory and the next write retries them.\n\
             # TYPE plurx_plex_census_flush_failures_total counter\n\
             plurx_plex_census_flush_failures_total {}\n",
            self.started_unix_s.load(Ordering::Relaxed),
            self.last_flush_unix_s.load(Ordering::Relaxed),
            self.unclean_stops,
            self.gap_seconds.load(Ordering::Relaxed),
            self.flush_failures.load(Ordering::Relaxed),
        ));
        out
    }
}

/// Writes the census every [`FLUSH_INTERVAL`] until shutdown. The final write
/// is the daemon's, after its HTTP server stops, not this loop's: this loop is
/// cancelled while requests may still be in flight.
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
    const DAY: u64 = 86_400;

    fn cell(census: &PlexCensus, handler: &str, outcome: &str) -> u64 {
        let index = cell_index(&format!("{handler}/{outcome}")).expect("known cell");
        census.since_census().expect("restored census")[index]
    }

    fn ledger(census: &PlexCensus) -> &CensusLedger {
        census.ledger.get().expect("restored census")
    }

    fn gap(census: &PlexCensus) -> u64 {
        ledger(census).gap_seconds.load(Ordering::Relaxed)
    }

    fn started(census: &PlexCensus) -> u64 {
        ledger(census).started_unix_s.load(Ordering::Relaxed)
    }

    async fn restored(dir: &Path, now: u64) -> PlexCensus {
        restored_with_start(dir, now).await.0
    }

    async fn restored_with_start(dir: &Path, now: u64) -> (PlexCensus, CensusStart) {
        restored_with_floor(dir, Some(now), 0).await
    }

    async fn restored_with_floor(
        dir: &Path,
        now: Option<u64>,
        floor: u64,
    ) -> (PlexCensus, CensusStart) {
        let census = PlexCensus::default();
        let start = census.restore_durable(dir, now, floor).await;
        (census, start)
    }

    fn read_file(dir: &Path) -> CensusFile {
        parse(&std::fs::read(dir.join(CENSUS_FILE)).expect("census file")).expect("valid file")
    }

    fn aside_names(dir: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(dir)
            .expect("list")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(ASIDE_PREFIX))
            .collect::<Vec<_>>();
        names.sort();
        names
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

    #[test]
    fn the_plex_census_clock_floor_is_the_build_source_date() {
        assert_eq!(parse_utc_stamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc_stamp("2026-10-02T17:52:19Z"), Some(1_790_963_539));
        assert_eq!(parse_utc_stamp("2000-02-29T00:00:00Z"), Some(951_782_400));
        for malformed in [
            "",
            "2026-10-02 17:52:19Z",
            "2026-13-02T00:00:00Z",
            "unknown",
        ] {
            assert_eq!(parse_utc_stamp(malformed), None, "{malformed}");
        }
        // `build.rs` always stamps a parseable source date, and it is after the
        // census existed, so the floor is a real date.
        let floor = build_source_floor().expect("this build's source date");
        assert!(floor >= parse_utc_stamp("2026-10-01T00:00:00Z").expect("date"));
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
        assert!(first
            .flush_durable(Some(1_100), true)
            .await
            .expect("clean stop"));
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
        assert_eq!(started(&second), 1_000);
        assert_eq!(ledger(&second).unclean_stops, 0);
        // The hundred seconds between the clean stop and this start were not
        // counted, clean or not.
        assert_eq!(gap(&second), 100);
    }

    #[tokio::test]
    async fn plex_census_downtime_after_a_clean_stop_is_unobserved_time() {
        let dir = crate::test_tempdir().expect("tempdir");
        let first = restored(dir.path(), 1_000).await;
        assert!(first
            .flush_durable(Some(1_100), true)
            .await
            .expect("clean stop"));
        drop(first);
        // Six days stopped cleanly, then one running: one day of census time,
        // not seven.
        let restart = 1_100 + 6 * DAY;
        let second = restored(dir.path(), restart).await;
        assert_eq!(
            ledger(&second).unclean_stops,
            0,
            "a clean stop is not an unclean one"
        );
        assert_eq!(gap(&second), 6 * DAY);
        assert_eq!(
            read_file(dir.path()).gap_seconds,
            6 * DAY,
            "persisted at once"
        );
        assert!(second
            .flush_durable(Some(restart + DAY), false)
            .await
            .expect("write"));
        let census_time = (restart + DAY) - started(&second) - gap(&second);
        assert_eq!(census_time, DAY + 100);
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
            .flush_durable(Some(1_060), false)
            .await
            .expect("periodic write"));
        for _ in 0..7 {
            first.record(SECTION_ALL, StatusCode::OK);
        }
        // A crash, or a lifetime whose later writes all failed: no further
        // write and no clean stop.
        drop(first);

        let (second, start) = restored_with_start(dir.path(), 1_300).await;
        assert_eq!(start, CensusStart::Continued { unclean: true });
        // Exactly the seven requests after the last write are lost: the
        // census is a lower bound, never an over-count.
        assert_eq!(cell(&second, "section_all", "ok"), 4);
        assert_eq!(ledger(&second).unclean_stops, 1);
        assert_eq!(gap(&second), 1_300 - 1_060);
        assert_eq!(started(&second), 1_000);
        // The gap is persisted at once, so a second crash cannot forget it.
        let on_disk = read_file(dir.path());
        assert_eq!((on_disk.unclean_stops, on_disk.gap_seconds), (1, 240));
        drop(second);

        let third = restored(dir.path(), 1_310).await;
        assert_eq!(ledger(&third).unclean_stops, 2);
        assert_eq!(gap(&third), 240 + 10);
        assert!(third
            .flush_durable(Some(1_400), true)
            .await
            .expect("clean stop"));
        drop(third);
        let fourth = restored(dir.path(), 1_500).await;
        assert_eq!(
            ledger(&fourth).unclean_stops,
            2,
            "a clean stop is not an unclean one"
        );
        assert_eq!(gap(&fourth), 250 + 100, "its downtime is still unobserved");
        assert_eq!(cell(&fourth, "section_all", "ok"), 4);
    }

    #[tokio::test]
    async fn the_plex_census_clean_stop_write_is_final() {
        let dir = crate::test_tempdir().expect("tempdir");
        let census = restored(dir.path(), 1_000).await;
        census.record(SECTION_ALL, StatusCode::OK);
        assert!(census
            .flush_durable(Some(1_050), true)
            .await
            .expect("clean stop"));
        census.record(SECTION_ALL, StatusCode::OK);
        // A periodic write that lost the race to the clean stop must not
        // overwrite the clean mark.
        assert!(!census
            .flush_durable(Some(1_060), false)
            .await
            .expect("no-op"));
        let on_disk = read_file(dir.path());
        assert!(on_disk.stopped_cleanly);
        assert_eq!(on_disk.last_flush_unix_s, 1_050);
    }

    #[tokio::test]
    async fn a_plex_census_trusts_no_clock_below_the_build_source_date() {
        let dir = crate::test_tempdir().expect("tempdir");
        let floor = 1_000;
        // A clock near 1970 at a fresh start: no start time, no file.
        let (census, start) = restored_with_floor(dir.path(), Some(500), floor).await;
        assert!(matches!(
            start,
            CensusStart::Fresh {
                unusable: false,
                ..
            }
        ));
        assert_eq!(started(&census), 0);
        assert!(census
            .prometheus()
            .contains("\nplurx_plex_census_started_seconds 0\n"));
        assert!(
            !dir.path().join(CENSUS_FILE).exists(),
            "no durable write below the floor"
        );
        census.record(SECTION_ALL, StatusCode::OK);
        assert!(!census
            .flush_durable(Some(999), false)
            .await
            .expect("deferred"));
        // An unreadable clock is the same, never "0".
        assert!(!census.flush_durable(None, false).await.expect("deferred"));
        assert!(!dir.path().join(CENSUS_FILE).exists());
        // The first trustworthy reading starts the census and keeps the count.
        assert!(census
            .flush_durable(Some(1_500), false)
            .await
            .expect("write"));
        let on_disk = read_file(dir.path());
        assert_eq!(on_disk.started_unix_s, 1_500);
        assert_eq!(on_disk.gap_seconds, 0);
        assert_eq!(on_disk.cells.get("section_all/ok"), Some(&1));
        assert!(census
            .flush_durable(Some(1_600), true)
            .await
            .expect("clean stop"));
        drop(census);

        // A continued census on a bad clock: the unobserved stretch is added
        // at the first trustworthy reading, not from a fake one.
        let (census, start) = restored_with_floor(dir.path(), Some(10), floor).await;
        assert_eq!(start, CensusStart::Continued { unclean: false });
        assert_eq!(gap(&census), 0);
        assert!(read_file(dir.path()).stopped_cleanly, "nothing written yet");
        assert!(!census.flush_durable(None, false).await.expect("deferred"));
        assert!(census
            .flush_durable(Some(2_000), false)
            .await
            .expect("write"));
        assert_eq!(gap(&census), 400);
        assert_eq!(started(&census), 1_500);
    }

    #[tokio::test]
    async fn an_unusable_plex_census_file_starts_a_new_census_without_failing() {
        let cases: [(&str, Vec<u8>, &str); 4] = [
            ("garbage", b"{ not json".to_vec(), "not a census file"),
            (
                "wrong version",
                br#"{"version":2,"started_unix_s":1,"last_flush_unix_s":1,"stopped_cleanly":true,"unclean_stops":0,"gap_seconds":0,"cells":{}}"#.to_vec(),
                "census format v2",
            ),
            (
                "unknown field",
                br#"{"version":1,"started_unix_s":1,"last_flush_unix_s":1,"stopped_cleanly":true,"unclean_stops":0,"gap_seconds":0,"cells":{},"extra":1}"#.to_vec(),
                "unknown field",
            ),
            (
                "oversized",
                vec![b' '; MAX_FILE_BYTES as usize + 1],
                "census cap",
            ),
        ];
        for (name, bytes, why) in cases {
            let dir = crate::test_tempdir().expect("tempdir");
            std::fs::write(dir.path().join(CENSUS_FILE), &bytes).expect("seed");
            let (census, start) = restored_with_start(dir.path(), 5_000).await;
            match &start {
                CensusStart::Fresh {
                    reason,
                    unusable: true,
                } => assert!(reason.contains(why), "{name}: {reason}"),
                other => panic!("{name}: {other:?}"),
            }
            assert_eq!(started(&census), 5_000, "{name}");
            assert!(
                census
                    .since_census()
                    .expect("census")
                    .iter()
                    .all(|count| *count == 0),
                "{name}"
            );
            assert_eq!(
                std::fs::read(dir.path().join(format!("{ASIDE_PREFIX}5000"))).expect("kept aside"),
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
    async fn plex_census_read_errors_are_retried_and_nothing_kept_aside_is_overwritten() {
        let dir = crate::test_tempdir().expect("tempdir");
        // Something at the census path that every read fails on.
        std::fs::create_dir(dir.path().join(CENSUS_FILE)).expect("seed a directory");
        let (census, start) = restored_with_start(dir.path(), 7_000).await;
        match &start {
            CensusStart::Fresh {
                reason,
                unusable: true,
            } => assert!(reason.contains("unreadable after a retry"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(dir.path().join(format!("{ASIDE_PREFIX}7000")).is_dir());
        assert_eq!(read_file(dir.path()).started_unix_s, 7_000);
        drop(census);

        // Two unusable files in the same second keep two copies.
        std::fs::write(dir.path().join(CENSUS_FILE), b"first").expect("seed");
        let _ = restored(dir.path(), 7_000).await;
        std::fs::write(dir.path().join(CENSUS_FILE), b"second").expect("seed");
        let _ = restored(dir.path(), 7_000).await;
        assert_eq!(
            aside_names(dir.path()),
            vec![
                format!("{ASIDE_PREFIX}7000"),
                format!("{ASIDE_PREFIX}7000-1"),
                format!("{ASIDE_PREFIX}7000-2"),
            ]
        );
        assert_eq!(
            std::fs::read(dir.path().join(format!("{ASIDE_PREFIX}7000-1"))).expect("read"),
            b"first"
        );
        assert_eq!(
            std::fs::read(dir.path().join(format!("{ASIDE_PREFIX}7000-2"))).expect("read"),
            b"second"
        );
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
            ("plurx_plex_census_gap_seconds", 100),
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
