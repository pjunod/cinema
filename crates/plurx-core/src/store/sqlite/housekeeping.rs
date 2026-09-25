//! Connection health for the standalone SQLite store (K-05 section 3.9).
//!
//! Two things live here. A closure that panics while it holds a connection
//! poisons that connection's mutex, and before this every later call on the
//! slot failed with "mutex poisoned" until the process restarted; now the
//! next caller takes the connection back, proves it is usable, and reopens
//! it from the database path if it is not. And the database file is checked
//! once at open, so a corrupt one refuses startup with its recovery named
//! instead of failing somewhere later, while a check too slow for boot
//! starts the daemon with a warning and runs in full in the background.
//! Both report through fixed-cardinality counters on `/metrics`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::StoreError;

/// Which slot a connection sits in; a writer is validated for the pragmas
/// only it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pool {
    Writer,
    Read,
}

impl Pool {
    const ALL: [Self; 2] = [Self::Writer, Self::Read];

    fn label(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Read => "read",
        }
    }
}

/// What recovering a poisoned connection came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    /// The connection passed validation and was put back into service.
    Validated,
    /// It failed validation and a fresh connection replaced it.
    Reopened,
    /// It failed validation and could not be replaced; the slot stays
    /// poisoned so the next caller tries again.
    Failed,
}

impl Recovery {
    const ALL: [Self; 3] = [Self::Validated, Self::Reopened, Self::Failed];

    fn label(self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Reopened => "reopened",
            Self::Failed => "failed",
        }
    }
}

static RECOVERIES: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

fn recovery_slot(pool: Pool, outcome: Recovery) -> &'static AtomicU64 {
    let pool = Pool::ALL
        .iter()
        .position(|value| *value == pool)
        .unwrap_or(0);
    let outcome = Recovery::ALL
        .iter()
        .position(|value| *value == outcome)
        .unwrap_or(0);
    &RECOVERIES[pool * Recovery::ALL.len() + outcome]
}

#[cfg(test)]
pub(super) fn recoveries(pool: Pool, outcome: Recovery) -> u64 {
    recovery_slot(pool, outcome).load(Ordering::Relaxed)
}

/// Where an integrity check ran and what it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Integrity {
    BootOk,
    BootCorrupt,
    /// The boot check did not finish inside its budget; the full check runs
    /// in the background instead.
    BootDeferred,
    BackgroundOk,
    BackgroundCorrupt,
    /// The background check could not run at all (open or I/O failure).
    BackgroundError,
}

impl Integrity {
    const ALL: [Self; 6] = [
        Self::BootOk,
        Self::BootCorrupt,
        Self::BootDeferred,
        Self::BackgroundOk,
        Self::BackgroundCorrupt,
        Self::BackgroundError,
    ];

    fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::BootOk => ("boot", "ok"),
            Self::BootCorrupt => ("boot", "corrupt"),
            Self::BootDeferred => ("boot", "deferred"),
            Self::BackgroundOk => ("background", "ok"),
            Self::BackgroundCorrupt => ("background", "corrupt"),
            Self::BackgroundError => ("background", "error"),
        }
    }
}

static INTEGRITY: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

fn record_integrity(outcome: Integrity) {
    let index = Integrity::ALL
        .iter()
        .position(|value| *value == outcome)
        .unwrap_or(0);
    INTEGRITY[index].fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
pub(super) fn integrity_checks(outcome: Integrity) -> u64 {
    let index = Integrity::ALL
        .iter()
        .position(|value| *value == outcome)
        .unwrap_or(0);
    INTEGRITY[index].load(Ordering::Relaxed)
}

/// The counters above as Prometheus text. Every series is always present,
/// at zero on a healthy node and on a clustered one (which does not use
/// this store for its catalogue).
pub fn prometheus_sqlite_health() -> String {
    use std::fmt::Write;

    let mut out = String::from(
        "# HELP plurx_sqlite_connection_recoveries_total Standalone SQLite connections taken back after a panicking closure poisoned them, by pool and outcome.\n\
         # TYPE plurx_sqlite_connection_recoveries_total counter\n",
    );
    for pool in Pool::ALL {
        for outcome in Recovery::ALL {
            let _ = writeln!(
                out,
                "plurx_sqlite_connection_recoveries_total{{pool=\"{}\",outcome=\"{}\"}} {}",
                pool.label(),
                outcome.label(),
                recovery_slot(pool, outcome).load(Ordering::Relaxed)
            );
        }
    }
    out.push_str(
        "# HELP plurx_sqlite_integrity_checks_total Standalone SQLite database integrity checks, by phase and outcome.\n\
         # TYPE plurx_sqlite_integrity_checks_total counter\n",
    );
    for (index, outcome) in Integrity::ALL.into_iter().enumerate() {
        let (phase, result) = outcome.labels();
        let _ = writeln!(
            out,
            "plurx_sqlite_integrity_checks_total{{phase=\"{phase}\",outcome=\"{result}\"}} {}",
            INTEGRITY[index].load(Ordering::Relaxed)
        );
    }
    out
}

/// The per-connection settings every writer connection carries. Applied at
/// open and again to a replacement, so a reopened writer is the connection
/// the store was opened with.
pub(super) fn configure_writer(conn: &Connection) -> Result<(), StoreError> {
    // WAL for concurrent-reader friendliness on real files; in-memory
    // databases report their own journal mode, which is fine.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

pub(super) fn open_reader(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(conn)
}

fn reopen(pool: Pool, path: &Path) -> Result<Connection, StoreError> {
    match pool {
        Pool::Writer => {
            let conn = Connection::open(path)?;
            configure_writer(&conn)?;
            Ok(conn)
        }
        Pool::Read => open_reader(path),
    }
}

/// Is this connection fit to serve the next caller? A closure that panicked
/// mid-transaction left it open (rusqlite's guards roll back on drop, but a
/// raw `BEGIN` has no guard), so that is rolled back first; then the
/// connection must answer a query, its database must pass `quick_check(1)`,
/// and a writer must still enforce foreign keys, which a migration turns
/// off while it runs.
fn validate(conn: &Connection, pool: Pool) -> Result<(), String> {
    if !conn.is_autocommit() {
        conn.execute_batch("ROLLBACK")
            .map_err(|error| format!("rollback of the open transaction failed: {error}"))?;
    }
    let one: i64 = conn
        .query_row("SELECT 1", [], |row| row.get(0))
        .map_err(|error| format!("SELECT 1 failed: {error}"))?;
    if one != 1 {
        return Err(format!("SELECT 1 returned {one}"));
    }
    let check: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| format!("quick_check failed: {error}"))?;
    if check != "ok" {
        return Err(format!("quick_check reported {check}"));
    }
    if pool == Pool::Writer {
        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .map_err(|error| format!("reading foreign_keys failed: {error}"))?;
        if foreign_keys != 1 {
            return Err("foreign_keys is off".to_owned());
        }
    }
    Ok(())
}

/// Lock a connection slot, taking it back if a panic poisoned it.
///
/// `path` is the database file, or `None` for an in-memory store, whose one
/// connection cannot be replaced by opening another (that would be a
/// different, empty database): there a connection that fails validation is
/// `Failed` and the slot stays poisoned.
pub(super) fn lock_or_recover<'a>(
    slot: &'a Mutex<Connection>,
    pool: Pool,
    path: Option<&Path>,
) -> Result<MutexGuard<'a, Connection>, StoreError> {
    let poisoned = match slot.lock() {
        Ok(guard) => return Ok(guard),
        Err(poisoned) => poisoned,
    };
    let mut guard = poisoned.into_inner();
    let outcome = match validate(&guard, pool) {
        Ok(()) => Recovery::Validated,
        Err(reason) => {
            tracing::warn!(pool = pool.label(), %reason, "poisoned sqlite connection failed validation");
            match path.map(|path| reopen(pool, path)) {
                Some(Ok(fresh)) => {
                    *guard = fresh;
                    Recovery::Reopened
                }
                Some(Err(error)) => {
                    tracing::error!(pool = pool.label(), %error, "reopening a poisoned sqlite connection failed");
                    Recovery::Failed
                }
                None => Recovery::Failed,
            }
        }
    };
    recovery_slot(pool, outcome).fetch_add(1, Ordering::Relaxed);
    if outcome == Recovery::Failed {
        return Err(StoreError::Task(format!(
            "sqlite {} connection was poisoned and could not be recovered",
            pool.label()
        )));
    }
    tracing::warn!(
        pool = pool.label(),
        outcome = outcome.label(),
        "recovered a sqlite connection poisoned by a panic"
    );
    slot.clear_poison();
    Ok(guard)
}

/// How long the boot integrity check may hold up startup.
pub(super) const BOOT_CHECK_BUDGET: Duration = Duration::from_secs(30);

/// Run `f` on `conn`, interrupting it once `budget` has passed. Returns
/// `None` if it was interrupted.
fn within_budget<T>(
    conn: &Connection,
    budget: Duration,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<Option<T>> {
    let interrupt = conn.get_interrupt_handle();
    let finished = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let fired = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let finished = Arc::clone(&finished);
        let fired = Arc::clone(&fired);
        std::thread::spawn(move || {
            let (lock, condvar) = &*finished;
            let done = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (done, timeout) = condvar
                .wait_timeout_while(done, budget, |done| !*done)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if timeout.timed_out() && !*done {
                fired.store(true, Ordering::SeqCst);
                interrupt.interrupt();
            }
        })
    };
    let result = f(conn);
    {
        let (lock, condvar) = &*finished;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        condvar.notify_all();
    }
    let _ = watchdog.join();
    match result {
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::OperationInterrupted
                && fired.load(Ordering::SeqCst) =>
        {
            Ok(None)
        }
        other => other.map(Some),
    }
}

/// The refusal a corrupt database produces, naming the file, what the check
/// found first, and both ways back.
fn corruption_refusal(path: &Path, finding: &str) -> StoreError {
    StoreError::Database(format!(
        "SQLite database {} failed its integrity check: {}. plurx will not start on it. \
         On a node that was activated into a cluster, restore the newest backup \
         artefact (docs/cluster/CLUSTER-BACKUP-AND-RESTORE.md); on a standalone \
         server, stop plurx and salvage it with `sqlite3 {} .recover | sqlite3 <new file>`, \
         then put the new file in its place.",
        path.display(),
        finding.lines().next().unwrap_or(finding),
        path.display()
    ))
}

/// The boot integrity check: `quick_check(1)` on the freshly opened writer,
/// within `budget`. `ok` continues; a finding refuses startup with the
/// recovery named; running out of budget continues with a warning and
/// hands the full check to the background (see `background_integrity_check`).
pub(super) fn boot_integrity_check(
    conn: &Connection,
    path: &Path,
    budget: Duration,
) -> Result<(), StoreError> {
    let checked = within_budget(conn, budget, |conn| {
        conn.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
    });
    match checked {
        Ok(Some(finding)) if finding == "ok" => {
            record_integrity(Integrity::BootOk);
            Ok(())
        }
        Ok(Some(finding)) => {
            record_integrity(Integrity::BootCorrupt);
            Err(corruption_refusal(path, &finding))
        }
        Ok(None) => {
            record_integrity(Integrity::BootDeferred);
            tracing::warn!(
                path = %path.display(),
                budget_secs = budget.as_secs(),
                "the boot integrity check did not finish in its budget; starting, and running \
                 the full integrity check in the background at the top of the next hour"
            );
            background_integrity_check(path.to_owned());
            Ok(())
        }
        // SQLITE_CORRUPT or SQLITE_NOTADB surface as errors, not findings.
        Err(rusqlite::Error::SqliteFailure(error, message))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            record_integrity(Integrity::BootCorrupt);
            Err(corruption_refusal(
                path,
                message.as_deref().unwrap_or(&error.to_string()),
            ))
        }
        Err(error) => Err(error.into()),
    }
}

/// The full `integrity_check` a deferred boot check owes, on its own
/// read-only connection (WAL readers do not block the writer), starting at
/// the top of the next wall-clock hour so it does not add to the load of
/// the startup that just ran long.
fn background_integrity_check(path: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("sqlite-integrity".to_owned())
        .spawn(move || {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            std::thread::sleep(Duration::from_secs(3_600 - now % 3_600));
            let finding = open_reader(&path).and_then(|conn| {
                Ok(conn.query_row("PRAGMA integrity_check(1)", [], |row| row.get::<_, String>(0))?)
            });
            match finding {
                Ok(finding) if finding == "ok" => {
                    record_integrity(Integrity::BackgroundOk);
                    tracing::info!(path = %path.display(), "background integrity check passed");
                }
                Ok(finding) => {
                    record_integrity(Integrity::BackgroundCorrupt);
                    tracing::error!(
                        path = %path.display(),
                        %finding,
                        "background integrity check found corruption; see the recovery in \
                         OPERATIONS.md before restarting"
                    );
                }
                Err(error) => {
                    record_integrity(Integrity::BackgroundError);
                    tracing::error!(path = %path.display(), %error, "background integrity check could not run");
                }
            }
        });
    if let Err(error) = spawned {
        record_integrity(Integrity::BackgroundError);
        tracing::error!(%error, "could not start the background integrity check");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::{SettingsStore, SqliteStore};

    /// K-05 M6: a closure that panics on the writer, mid-transaction, leaves
    /// the next call successful: the connection is taken back, its open
    /// transaction rolled back, and the recovery counted as validated.
    #[tokio::test]
    async fn a_panicking_writer_closure_is_validated_and_rolled_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));
        let before = recoveries(Pool::Writer, Recovery::Validated);
        let panicked = store
            .with_conn(|conn| -> Result<(), StoreError> {
                conn.execute_batch(
                    "BEGIN; INSERT INTO settings(key, value) VALUES('half-written', 'x');",
                )?;
                panic!("a closure bug, mid-transaction");
            })
            .await;
        assert!(panicked.is_err(), "the panicking call itself fails");
        store
            .put_setting("after", "ok")
            .await
            .expect("the next write succeeds");
        assert_eq!(
            store.get_setting("half-written").await.expect("read"),
            None,
            "the panicked transaction was rolled back"
        );
        assert!(recoveries(Pool::Writer, Recovery::Validated) > before);
    }

    /// A panic that left the writer without foreign keys (a migration turns
    /// them off while it runs) fails validation, and the slot gets a fresh,
    /// fully configured connection instead.
    #[tokio::test]
    async fn a_writer_left_without_foreign_keys_is_reopened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));
        let before = recoveries(Pool::Writer, Recovery::Reopened);
        let _ = store
            .with_conn(|conn| -> Result<(), StoreError> {
                conn.pragma_update(None, "foreign_keys", "OFF")?;
                panic!("a closure bug after turning foreign keys off");
            })
            .await;
        let foreign_keys: i64 = store
            .with_conn(|conn| Ok(conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?))
            .await
            .expect("the next call succeeds");
        assert_eq!(
            foreign_keys, 1,
            "the replacement carries the writer's pragmas"
        );
        assert!(recoveries(Pool::Writer, Recovery::Reopened) > before);
    }

    /// The read pool recovers the same way: after a panic on each read
    /// connection, reads keep working.
    #[tokio::test]
    async fn a_panicking_read_closure_leaves_reads_working() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));
        store.put_setting("k", "v").await.expect("put");
        let before = recoveries(Pool::Read, Recovery::Validated);
        for _ in 0..4 {
            let _ = store
                .with_read(|_conn| -> Result<(), StoreError> { panic!("a read closure bug") })
                .await;
        }
        for _ in 0..4 {
            assert_eq!(
                store.get_setting("k").await.expect("read"),
                Some("v".to_owned())
            );
        }
        assert!(recoveries(Pool::Read, Recovery::Validated) > before);
    }

    /// An in-memory store cannot replace its only connection, so a failed
    /// validation is reported instead of silently swapping in an empty
    /// database.
    #[tokio::test]
    async fn an_in_memory_writer_that_fails_validation_is_not_replaced() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let before = recoveries(Pool::Writer, Recovery::Failed);
        let _ = store
            .with_conn(|conn| -> Result<(), StoreError> {
                conn.pragma_update(None, "foreign_keys", "OFF")?;
                panic!("a closure bug after turning foreign keys off");
            })
            .await;
        let error = store
            .get_setting("k")
            .await
            .expect_err("the slot stays poisoned");
        assert!(
            error.to_string().contains("could not be recovered"),
            "{error}"
        );
        assert!(recoveries(Pool::Writer, Recovery::Failed) > before);
    }

    /// A truncated database refuses to open, and the refusal names the file
    /// and both recoveries.
    #[test]
    fn a_truncated_database_refuses_startup_with_the_recovery_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.db");
        {
            let store = SqliteStore::open(&path).expect("open");
            drop(store);
        }
        {
            let conn = Connection::open(&path).expect("raw");
            conn.execute_batch(
                "PRAGMA journal_mode = DELETE;
                 CREATE TABLE filler(x BLOB);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200)
                 INSERT INTO filler SELECT randomblob(4096) FROM n;",
            )
            .expect("fill");
        }
        let length = std::fs::metadata(&path).expect("stat").len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for truncation")
            .set_len(length / 2)
            .expect("truncate");
        let before = integrity_checks(Integrity::BootCorrupt);
        let error = match SqliteStore::open(&path) {
            Ok(_) => panic!("a truncated database must not open"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("failed its integrity check"), "{error}");
        assert!(error.contains("CLUSTER-BACKUP-AND-RESTORE.md"), "{error}");
        assert!(error.contains(".recover"), "{error}");
        assert!(integrity_checks(Integrity::BootCorrupt) > before);
    }

    /// A check that outlives its budget is interrupted and reported as
    /// deferred, not as corruption and not as a hang.
    #[test]
    fn a_check_past_its_budget_is_interrupted_not_failed() {
        let conn = Connection::open_in_memory().expect("open");
        let started = std::time::Instant::now();
        let finished = within_budget(&conn, Duration::from_millis(50), |conn| {
            conn.query_row(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                 SELECT COUNT(*) FROM n",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .expect("an interruption is not an error");
        assert!(finished.is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
        let quick = within_budget(&conn, Duration::from_secs(30), |conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
        })
        .expect("query");
        assert_eq!(
            quick,
            Some(1),
            "a query inside its budget is not interrupted"
        );
    }

    #[test]
    fn health_exposition_has_the_fixed_series() {
        let exposition = prometheus_sqlite_health();
        assert_eq!(
            exposition
                .lines()
                .filter(|line| line.starts_with("plurx_sqlite_connection_recoveries_total{"))
                .count(),
            6
        );
        assert_eq!(
            exposition
                .lines()
                .filter(|line| line.starts_with("plurx_sqlite_integrity_checks_total{"))
                .count(),
            6
        );
    }
}
