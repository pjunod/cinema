//! K-05 M0: `EXPLAIN QUERY PLAN` and timings for the hot catalogue reads, on
//! the statements as executed (plan section 3.4).
//!
//! ```text
//! cargo run -p plurx-core --example catalogue_fixture --release -- fixture.db
//! query_plans capture-sqlite fixture.db sqlite.json
//! #   the Hiqlite side is captured by the ignored contract test:
//! #   K05_HIQLITE_CAPTURE=hiqlite.json cargo test -p plurx-core \
//! #     --features cluster-read-cost-validation,hiqlite-contract-tests \
//! #     --test store_contract -- --ignored k05_capture_hiqlite_statements
//! query_plans build-hiqlite fixture.db hiqlite.json hiqlite-fixture.db
//! query_plans measure fixture.db sqlite.json > sqlite.md
//! query_plans measure hiqlite-fixture.db hiqlite.json > hiqlite.md
//! ```
//!
//! `capture-sqlite` drives the real `SqliteStore` against the fixture with a
//! subscriber that keeps only the store's TRACE statement events, so the SQL
//! measured is the SQL that ran. `build-hiqlite` creates the Hiqlite
//! state-machine schema exactly as a bootstrapped three-voter cluster reported
//! it (`sqlite_master`, in creation order) and copies the fixture's rows into
//! it by column name, so both backends are planned over the same population.
//! `measure` records each plan verbatim and the median of five cold runs (a
//! fresh connection after the database file is written back and evicted from
//! the OS page cache) and five warm runs (one connection, after one untimed
//! run).

mod bench;
#[path = "calls.rs"]
mod calls;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use calls::{Planned, StatementCapture};
use plurx_core::store::SqliteStore;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What the Hiqlite capture test writes: the state machine's schema as the
/// cluster created it, and the replicated statements the calls executed.
#[derive(serde::Serialize, serde::Deserialize)]
struct Capture {
    backend: String,
    #[serde(default)]
    schema: Vec<SchemaObject>,
    statements: Vec<Planned>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SchemaObject {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["capture-sqlite", fixture, out] => capture_sqlite(Path::new(fixture), Path::new(out)),
        ["build-hiqlite", fixture, capture, out] => {
            build_hiqlite(Path::new(fixture), Path::new(capture), Path::new(out))
        }
        ["measure", database, capture] => measure(Path::new(database), Path::new(capture)),
        ["bench", fixture, reads] => bench::run(Path::new(fixture), reads.parse()?),
        _ => Err(
            "usage: query_plans capture-sqlite <fixture.db> <out.json> | \
                  build-hiqlite <fixture.db> <hiqlite.json> <out.db> | \
                  measure <db> <capture.json> | bench <fixture.db> <read-connections>"
                .into(),
        ),
    }
}

fn capture_sqlite(fixture: &Path, out: &Path) -> Result<()> {
    let capture = StatementCapture::default();
    tracing::subscriber::set_global_default(capture.clone())?;
    let store = SqliteStore::open(fixture)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let statements = runtime.block_on(calls::drive(&store, "sqlite", &capture));
    if statements.is_empty() {
        return Err("no statement events: is the store's trace_statement wired?".into());
    }
    let capture = Capture {
        backend: "sqlite".to_owned(),
        schema: Vec::new(),
        statements,
    };
    std::fs::write(out, serde_json::to_vec_pretty(&capture)?)?;
    eprintln!("captured {} statements", capture.statements.len());
    Ok(())
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info({})", quote(table)))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names)
}

fn build_hiqlite(fixture: &Path, capture: &Path, out: &Path) -> Result<()> {
    if out.exists() {
        return Err(format!("refusing to replace existing {}", out.display()).into());
    }
    let capture: Capture = serde_json::from_slice(&std::fs::read(capture)?)?;
    if capture.schema.is_empty() {
        return Err("capture carries no schema".into());
    }
    let conn = Connection::open(out)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute(
        "ATTACH DATABASE ?1 AS fx",
        [fixture.to_str().ok_or("fixture path")?],
    )?;
    let virtuals: BTreeSet<&str> = capture
        .schema
        .iter()
        .filter(|object| object.kind == "table" && object.sql.starts_with("CREATE VIRTUAL TABLE"))
        .map(|object| object.name.as_str())
        .collect();
    // FTS5 creates its own shadow tables; creating them by hand would fail.
    let shadow = |name: &str| {
        virtuals
            .iter()
            .any(|parent| name.len() > parent.len() && name.starts_with(&format!("{parent}_")))
    };
    let tx = conn.unchecked_transaction()?;
    // Ordinary tables first, then rows (with no triggers or indexes yet, so
    // the load is a plain append), then the virtual tables, their contents,
    // the indexes and the triggers.
    let mut copied = BTreeMap::new();
    for object in capture.schema.iter().filter(|object| {
        object.kind == "table" && !virtuals.contains(object.name.as_str()) && !shadow(&object.name)
    }) {
        tx.execute_batch(&object.sql)?;
        let theirs: BTreeSet<String> = columns(&tx, "fx", &object.name)?.into_iter().collect();
        if theirs.is_empty() {
            continue;
        }
        let shared: Vec<String> = columns(&tx, "main", &object.name)?
            .into_iter()
            .filter(|column| theirs.contains(column))
            .map(|column| quote(&column))
            .collect();
        if shared.is_empty() {
            continue;
        }
        let list = shared.join(",");
        let rows = tx.execute(
            &format!(
                "INSERT INTO main.{t}({list}) SELECT {list} FROM fx.{t}",
                t = quote(&object.name)
            ),
            [],
        )?;
        copied.insert(object.name.clone(), rows);
    }
    for object in capture
        .schema
        .iter()
        .filter(|object| virtuals.contains(object.name.as_str()))
    {
        tx.execute_batch(&object.sql)?;
        // A vocabulary table is a read-only view over its FTS index.
        if object.sql.contains("fts5vocab") {
            continue;
        }
        let external = object.sql.contains("content=") && !object.sql.contains("content=''");
        if external {
            tx.execute(
                &format!(
                    "INSERT INTO {t}({t}) VALUES('rebuild')",
                    t = quote(&object.name)
                ),
                [],
            )?;
        } else {
            let list = columns(&tx, "main", &object.name)?
                .iter()
                .map(|column| quote(column))
                .collect::<Vec<_>>()
                .join(",");
            let rows = tx.execute(
                &format!(
                    "INSERT INTO main.{t}(rowid,{list}) SELECT rowid,{list} FROM fx.{t}",
                    t = quote(&object.name)
                ),
                [],
            )?;
            copied.insert(object.name.clone(), rows);
        }
    }
    for kind in ["index", "trigger", "view"] {
        for object in capture.schema.iter().filter(|object| object.kind == kind) {
            tx.execute_batch(&object.sql)?;
        }
    }
    tx.commit()?;
    conn.execute_batch("DETACH DATABASE fx; PRAGMA wal_checkpoint(TRUNCATE);")?;
    for (table, rows) in copied.iter().filter(|(_, rows)| **rows > 0) {
        eprintln!("{table}: {rows} rows");
    }
    Ok(())
}

fn bind_values(params: &[Value]) -> Result<Vec<rusqlite::types::Value>> {
    params
        .iter()
        .map(|value| {
            Ok(match value {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(number) => {
                    rusqlite::types::Value::Integer(number.as_i64().ok_or("non-integer parameter")?)
                }
                Value::String(text) => rusqlite::types::Value::Text(text.clone()),
                other => return Err(format!("unsupported parameter {other}").into()),
            })
        })
        .collect()
}

/// Write back and drop the database's pages from the OS cache, so the next
/// fresh connection reads from the device.
fn evict(database: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", database.display()));
        let Ok(file) = File::open(&path) else {
            continue;
        };
        file.sync_all()?;
        drop_from_page_cache(&file, &path)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn drop_from_page_cache(file: &File, path: &Path) -> Result<()> {
    // SAFETY: a valid open descriptor; advice only, no memory is touched.
    let rc = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        return Err(format!("posix_fadvise({}) = {rc}", path.display()).into());
    }
    Ok(())
}

/// Cold runs need the kernel to drop the file's cached pages, which this tool
/// asks for with `posix_fadvise(POSIX_FADV_DONTNEED)` on Linux only. Anywhere
/// else `measure` refuses rather than report warm reads as cold ones.
#[cfg(not(target_os = "linux"))]
fn drop_from_page_cache(_file: &File, path: &Path) -> Result<()> {
    Err(format!(
        "cannot evict {} from the page cache: cold runs need Linux's posix_fadvise",
        path.display()
    )
    .into())
}

fn open_reader(database: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(conn)
}

fn run(
    conn: &Connection,
    sql: &str,
    params: &[rusqlite::types::Value],
) -> Result<(Duration, usize)> {
    let started = Instant::now();
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let mut count = 0;
    while rows.next()?.is_some() {
        count += 1;
    }
    Ok((started.elapsed(), count))
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn plan(conn: &Connection, sql: &str, params: &[rusqlite::types::Value]) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Indent by parent, the way the sqlite3 shell's `.eqp` prints the tree.
    let mut depth: BTreeMap<i64, usize> = BTreeMap::new();
    Ok(rows
        .into_iter()
        .map(|(id, parent, detail)| {
            let level = depth.get(&parent).map_or(0, |level| level + 1);
            depth.insert(id, level);
            format!("{}{detail}", "  ".repeat(level))
        })
        .collect())
}

const RUNS: usize = 5;

fn measure(database: &Path, capture: &Path) -> Result<()> {
    let capture: Capture = serde_json::from_slice(&std::fs::read(capture)?)?;
    let version: String =
        open_reader(database)?.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    println!(
        "### {} — `{}` (SQLite {version}, {RUNS} runs each)\n",
        capture.backend,
        database
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
    );
    println!("| Call | Statement | Rows | Cold median | Warm median |");
    println!("|---|---|---:|---:|---:|");
    let mut plans = Vec::new();
    let mut calls: Vec<(String, usize, Duration, Duration)> = Vec::new();
    for planned in &capture.statements {
        let params = bind_values(&planned.params)?;
        let mut cold = Vec::with_capacity(RUNS);
        let mut rows = 0;
        for _ in 0..RUNS {
            evict(database)?;
            let conn = open_reader(database)?;
            let (elapsed, count) = run(&conn, &planned.sql, &params)?;
            cold.push(elapsed);
            rows = count;
        }
        let conn = open_reader(database)?;
        run(&conn, &planned.sql, &params)?;
        let warm = (0..RUNS)
            .map(|_| run(&conn, &planned.sql, &params).map(|(elapsed, _)| elapsed))
            .collect::<Result<Vec<_>>>()?;
        let (cold, warm) = (median(cold), median(warm));
        println!(
            "| `{}` | `{}` | {rows} | {:.2} ms | {:.3} ms |",
            planned.label,
            planned.statement,
            cold.as_secs_f64() * 1e3,
            warm.as_secs_f64() * 1e3,
        );
        match calls.last_mut() {
            Some(call) if call.0 == planned.label => {
                call.1 += 1;
                call.2 += cold;
                call.3 += warm;
            }
            _ => calls.push((planned.label.clone(), 1, cold, warm)),
        }
        plans.push((planned, plan(&conn, &planned.sql, &params)?));
    }
    println!("\nPer call (sums of the statement medians above):\n");
    println!("| Call | Statements | Cold | Warm |");
    println!("|---|---:|---:|---:|");
    for (label, statements, cold, warm) in &calls {
        println!(
            "| `{label}` | {statements} | {:.2} ms | {:.3} ms |",
            cold.as_secs_f64() * 1e3,
            warm.as_secs_f64() * 1e3
        );
    }
    println!();
    for (planned, lines) in plans {
        println!("#### `{}` — `{}`\n", planned.label, planned.statement);
        println!("```sql\n{}\n```\n", planned.sql.trim());
        println!("Bound: `{}`\n", serde_json::to_string(&planned.params)?);
        println!("```text\nQUERY PLAN\n{}\n```\n", lines.join("\n"));
    }
    Ok(())
}
