//! What each title generation's M7 evaluation ended with, and under which M7
//! identity and fitted F+ table.
//!
//! One row per Dolby Vision title demand revision. The title job's
//! publication writes it in the same command that stores the aggregate and
//! settles the job, so a title job that succeeded with an M7 outcome always
//! has its row. The idle reopen sweep writes `pending` when it reopens a
//! title: its loop-breaker, so a title reopens at most once per (identity,
//! fitted table) whatever the reopened job does. A row dies with its demand
//! (reprobe, source change, scanner rotation, file delete).
//!
//! Retirement is a trigger rather than a foreign-key cascade. Both stores do
//! run with `foreign_keys=ON`, but SQLite migrations and restores run with it
//! off, and this schema's own demand cleanup (`dv_title_jobs_file_delete`)
//! already uses explicit triggers for the same reason; a trigger also cannot
//! abort a publication the way a failed foreign-key insert would.

use super::background_jobs::{decode, digest, encode, QueueSql};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE dv_m7_title_outcomes_v1 (
  revision INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL CHECK (file_id > 0),
  identity TEXT NOT NULL
    CHECK (length(identity)=64 AND identity NOT GLOB '*[^0-9a-f]*'),
  fit_table TEXT NOT NULL
    CHECK (fit_table='' OR (length(fit_table)=64 AND fit_table NOT GLOB '*[^0-9a-f]*')),
  outcome TEXT NOT NULL
    CHECK (outcome IN ('pending','not_applicable','result','unsupported','inconclusive')),
  gate TEXT CHECK (gate IS NULL OR length(CAST(gate AS BLOB)) BETWEEN 1 AND 1024),
  at_ms INTEGER NOT NULL CHECK (at_ms >= 0)
) STRICT;
-- next statement
CREATE TRIGGER dv_m7_title_outcomes_v1_retire
AFTER DELETE ON dv_title_jobs_demand
BEGIN
 DELETE FROM dv_m7_title_outcomes_v1 WHERE revision=OLD.revision;
END;
-- next statement
CREATE TRIGGER dv_m7_title_outcomes_v1_publish
AFTER INSERT ON background_job_commands
WHEN NEW.operation='publish_dv_title' AND json_extract(NEW.result_json,'$.outcome')='published'
 AND json_extract(NEW.result_json,'$.job_id') IS NOT NULL
 AND json_type(NEW.request_json,'$.m7')='object'
BEGIN
 INSERT INTO dv_m7_title_outcomes_v1(revision,file_id,identity,fit_table,outcome,gate,at_ms)
 SELECT d.revision,d.file_id,json_extract(NEW.request_json,'$.m7.identity'),
  COALESCE((SELECT t.curve_digest FROM background_dv_refit_titles t
   WHERE t.file_id=f.id AND t.superseded_at_ms IS NULL AND t.verdict='fitted'
    AND t.fit_version=json_extract(NEW.request_json,'$.m7.fit_version')
    AND t.source_size=f.size AND t.source_mtime=f.mtime),''),
  json_extract(NEW.request_json,'$.m7.outcome'),json_extract(NEW.request_json,'$.m7.gate'),
  json_extract(NEW.request_json,'$.now_ms')
 FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
 WHERE d.revision=json_extract(NEW.request_json,'$.output.probe_revision')
  AND d.file_id=json_extract(NEW.request_json,'$.output.file_id')
 ON CONFLICT(revision) DO UPDATE SET file_id=excluded.file_id,identity=excluded.identity,
  fit_table=excluded.fit_table,outcome=excluded.outcome,gate=excluded.gate,at_ms=excluded.at_ms;
END;
"#;

/// The fitted table a title's M7 evaluation reads now: the file's unsuperseded
/// `fitted` F+ table under the fit version `$1.fit_version`, for the source its
/// row still describes; `''` when there is none. `f` is the file row. The
/// publication trigger records the same expression (with the request's fit
/// version), so the sweep compares like with like.
pub(super) const CURRENT_FIT_TABLE: &str =
    "COALESCE((SELECT t.curve_digest FROM background_dv_refit_titles t
  WHERE t.file_id=f.id AND t.superseded_at_ms IS NULL AND t.verdict='fitted'
   AND t.fit_version=json_extract($1,'$.fit_version')
   AND t.source_size=f.size AND t.source_mtime=f.mtime),'')";

pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'dv_m7_title_outcomes_v1*' AND sql IS NOT NULL ORDER BY name";

/// The objects the extension's triggers fire on.
const DEPENDENCIES: &str = "CREATE TABLE dv_title_jobs_demand(revision INTEGER PRIMARY KEY,file_id INTEGER); \
     CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT);";

#[cfg(any(feature = "hiqlite-store", test))]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn canonical(rows: &[(String, String)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(name, sql)| {
            (
                name.clone(),
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect()
}

static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("M7 outcome schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("M7 outcome schema dependencies");
    conn.execute_batch(SCHEMA).expect("M7 outcome schema");
    canonical(&rows(&conn).expect("M7 outcome schema shape"))
});

/// Whether `shape` (rows of [`SHAPE_SQL`]) is the whole schema; an
/// incomplete or different one refuses.
pub(crate) fn has_schema(shape: &[(String, String)]) -> Result<bool, StoreError> {
    if shape.is_empty() {
        Ok(false)
    } else if canonical(shape) == *SHAPE {
        Ok(true)
    } else {
        Err(StoreError::Migration(
            "M7 title outcome schema partial or incompatible".into(),
        ))
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if !has_schema(&rows(conn)?)? {
        return Err(StoreError::Migration(
            "M7 title outcome schema missing".into(),
        ));
    }
    Ok(())
}

/// How an M7 evaluation ended, as recorded with the title's publication.
/// `pending` is the sweep's own mark and never published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum M7OutcomeKind {
    /// The sweep reopened the title; its job has not recorded an outcome.
    Pending,
    /// Not an M7 title (no P8.1 conversion, not Profile 7, not FEL). Never reopened.
    NotApplicable,
    /// A historical, repetition alias, or measured/published result. Never reopened.
    Result,
    /// A gate refused the title. Reopened when the identity or fitted table changes.
    Unsupported,
    /// The experiment ran out of attempts or conflicts with a frozen one.
    /// Terminal; only the administrator's re-evaluation reopens it.
    Inconclusive,
}

/// Longest stored gate, in bytes.
pub const MAX_GATE_BYTES: usize = 1024;

/// The M7 outcome a title job publishes with its aggregate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct M7TitleOutcome {
    /// The M7 identity (64 lowercase hex) that evaluated the title.
    pub identity: String,
    /// The fit version whose fitted table the record names.
    pub fit_version: String,
    pub outcome: M7OutcomeKind,
    /// Why an `unsupported` or `inconclusive` evaluation ended.
    pub gate: Option<String>,
}

impl M7TitleOutcome {
    /// A gate cut to [`MAX_GATE_BYTES`] on a character boundary.
    pub fn bounded_gate(gate: &str) -> String {
        let mut end = gate.len().min(MAX_GATE_BYTES);
        while !gate.is_char_boundary(end) {
            end -= 1;
        }
        gate[..end].to_owned()
    }

    pub(super) fn validate(&self) -> Result<(), StoreError> {
        let gated = matches!(
            self.outcome,
            M7OutcomeKind::Unsupported | M7OutcomeKind::Inconclusive
        );
        if !digest(&self.identity)
            || !super::dv_refit_jobs::valid_fit_version(&self.fit_version)
            || self.outcome == M7OutcomeKind::Pending
            || gated != self.gate.is_some()
            || self
                .gate
                .as_ref()
                .is_some_and(|gate| gate.is_empty() || gate.len() > MAX_GATE_BYTES)
        {
            return Err(StoreError::Task("invalid M7 title outcome".into()));
        }
        Ok(())
    }
}

/// The stored outcome of a file's current title generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M7TitleOutcomeRecord {
    pub revision: i64,
    pub file_id: i64,
    pub identity: String,
    pub fit_table: String,
    pub outcome: M7OutcomeKind,
    pub gate: Option<String>,
    pub at_ms: i64,
}

/// The outcome recorded for `file_id`'s current demand revision, if any.
pub(super) async fn outcome<T: QueueSql>(
    store: &T,
    file_id: i64,
) -> Result<Option<M7TitleOutcomeRecord>, StoreError> {
    if file_id <= 0 {
        return Err(StoreError::Task("invalid M7 outcome lookup".into()));
    }
    let rows = store
        .queue_sql(
            "SELECT json_object('revision',o.revision,'file_id',o.file_id,'identity',o.identity,
 'fit_table',o.fit_table,'outcome',o.outcome,'gate',o.gate,'at_ms',o.at_ms) AS result_json
FROM dv_title_jobs_demand d JOIN dv_m7_title_outcomes_v1 o ON o.revision=d.revision
WHERE d.file_id=json_extract($1,'$.file')"
                .into(),
            encode(&serde_json::json!({"file": file_id}))?,
            false,
            true,
        )
        .await?;
    rows.first().map(|row| decode(row)).transpose()
}

/// Forget the M7 outcomes of `item_id`'s files' current title generations,
/// so the idle sweep reopens them. Returns how many were forgotten.
pub(super) async fn clear_item<T: QueueSql>(store: &T, item_id: i64) -> Result<u64, StoreError> {
    if item_id <= 0 {
        return Err(StoreError::Task("invalid M7 re-evaluation".into()));
    }
    let rows = store
        .queue_sql(
            "DELETE FROM dv_m7_title_outcomes_v1 WHERE revision IN (SELECT d.revision
 FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id WHERE f.item_id=json_extract($1,'$.item'))
RETURNING 'true' AS result_json"
                .into(),
            encode(&serde_json::json!({"item": item_id}))?,
            true,
            true,
        )
        .await?;
    Ok(rows.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_schema_refuses_a_partial_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch(DEPENDENCIES).expect("dependencies");
        assert!(!has_schema(&rows(&conn).expect("empty")).expect("absent"));
        for sql in statements() {
            conn.execute_batch(&sql).expect("statement");
        }
        verify_sqlite(&conn).expect("complete");
        // A record dies with its demand row, and only its own.
        let identity = "a".repeat(64);
        conn.execute_batch(&format!(
            "INSERT INTO dv_title_jobs_demand VALUES(1,10),(2,11);
             INSERT INTO dv_m7_title_outcomes_v1 VALUES
              (1,10,'{identity}','','result',NULL,1),
              (2,11,'{identity}','','pending',NULL,1);
             DELETE FROM dv_title_jobs_demand WHERE revision=1;"
        ))
        .expect("retire");
        let left: Vec<i64> = conn
            .prepare("SELECT revision FROM dv_m7_title_outcomes_v1")
            .expect("query")
            .query_map([], |row| row.get(0))
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("revisions");
        assert_eq!(left, vec![2]);
        conn.execute_batch("DROP TRIGGER dv_m7_title_outcomes_v1_retire")
            .expect("partial fixture");
        assert!(verify_sqlite(&conn).is_err());
    }

    #[test]
    fn a_published_outcome_names_its_gate_only_when_gated() {
        let outcome = |kind, gate: Option<&str>| M7TitleOutcome {
            identity: "a".repeat(64),
            fit_version: "fit-v1".into(),
            outcome: kind,
            gate: gate.map(str::to_owned),
        };
        assert!(outcome(M7OutcomeKind::Result, None).validate().is_ok());
        assert!(outcome(M7OutcomeKind::Unsupported, Some("gate"))
            .validate()
            .is_ok());
        assert!(outcome(M7OutcomeKind::Unsupported, None)
            .validate()
            .is_err());
        assert!(outcome(M7OutcomeKind::Result, Some("gate"))
            .validate()
            .is_err());
        assert!(outcome(M7OutcomeKind::Pending, None).validate().is_err());
        let long = "é".repeat(MAX_GATE_BYTES);
        let bounded = M7TitleOutcome::bounded_gate(&long);
        assert!(bounded.len() <= MAX_GATE_BYTES && long.starts_with(&bounded));
        assert!(outcome(M7OutcomeKind::Inconclusive, Some(&bounded))
            .validate()
            .is_ok());
        assert!(outcome(M7OutcomeKind::Inconclusive, Some(&long))
            .validate()
            .is_err());
    }
}
