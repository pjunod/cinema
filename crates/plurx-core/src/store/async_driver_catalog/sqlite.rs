//! Persistent catalog storage. This connection never executes production SQL.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::{
    canonical, digest, number, string, CatalogError, CatalogResult, ReadLimits, Record,
    RecordBatch, RecordKey, Storage, WriteContext,
};

// Checked unsigned transport integers never silently wrap into SQLite i64.
trait SqlParameter {
    fn sql_value(&self) -> CatalogResult<SqlValue>;
}
impl SqlParameter for u64 {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        Ok(SqlValue::Integer(i64::try_from(*self).map_err(|_| {
            CatalogError::NumericDomain("SQLite integer overflow".into())
        })?))
    }
}
impl SqlParameter for i64 {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        Ok(SqlValue::Integer(*self))
    }
}
impl SqlParameter for i32 {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        Ok(SqlValue::Integer(i64::from(*self)))
    }
}
impl SqlParameter for bool {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        Ok(SqlValue::Integer(i64::from(*self)))
    }
}
impl SqlParameter for str {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        Ok(SqlValue::Text(self.into()))
    }
}
impl SqlParameter for String {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        self.as_str().sql_value()
    }
}
impl SqlParameter for [u8] {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        Ok(SqlValue::Blob(self.into()))
    }
}
impl SqlParameter for Vec<u8> {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        self.as_slice().sql_value()
    }
}
impl<T: SqlParameter> SqlParameter for Option<T> {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        self.as_ref()
            .map(SqlParameter::sql_value)
            .transpose()
            .map(|v| v.unwrap_or(SqlValue::Null))
    }
}
impl<T: SqlParameter + ?Sized> SqlParameter for &T {
    fn sql_value(&self) -> CatalogResult<SqlValue> {
        (*self).sql_value()
    }
}
macro_rules! params {($($value:expr),* $(,)?)=>{rusqlite::params_from_iter(vec![$(SqlParameter::sql_value(&$value)?),*])};}
fn row_uint(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
fn row_optional_uint(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(column)?
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

const AUX_SCHEMA:&str="
CREATE TABLE IF NOT EXISTS catalog_records (
 kind TEXT NOT NULL,key_type INTEGER NOT NULL CHECK(key_type IN (0,1)),
 key_integer INTEGER,key_text TEXT,key_json BLOB NOT NULL,payload BLOB NOT NULL,
 digest BLOB NOT NULL CHECK(length(digest)=32),created_commit INTEGER NOT NULL REFERENCES catalog_commits(sequence),
 PRIMARY KEY(kind,key_json));
CREATE INDEX IF NOT EXISTS catalog_records_order ON catalog_records(kind,key_type,key_integer,key_text);
CREATE TABLE IF NOT EXISTS catalog_identities (
 kind TEXT NOT NULL,digest BLOB NOT NULL CHECK(length(digest)=32),canonical BLOB NOT NULL,
 id INTEGER NOT NULL,PRIMARY KEY(kind,canonical),UNIQUE(kind,id));
CREATE INDEX IF NOT EXISTS catalog_identity_digest ON catalog_identities(kind,digest);
CREATE TABLE IF NOT EXISTS catalog_allocators(kind TEXT PRIMARY KEY,last_id INTEGER NOT NULL CHECK(last_id>=1));
CREATE TABLE IF NOT EXISTS catalog_control(key TEXT PRIMARY KEY,payload BLOB NOT NULL);
";

pub struct SqliteStorage {
    connection: Connection,
    path: PathBuf,
    context: Option<WriteContext>,
    pending_sequence: Option<u64>,
    reading: bool,
}
impl SqliteStorage {
    pub fn open(path: &Path, resident_bytes: u64) -> CatalogResult<Self> {
        if path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(CatalogError::Io("catalog path is a symlink".into()));
        }
        if resident_bytes < 4096 {
            return Err(CatalogError::ResourceIncomplete(
                "catalog page cache allowance".into(),
            ));
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let cache_kib = (resident_bytes / 4 / 1024).clamp(1, i32::MAX as u64);
        connection.pragma_update(None, "cache_size", -(cache_kib as i64))?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='catalog_meta')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            connection.execute_batch(include_str!(
                "../../../../../verification/async-driver-refinement/paged-catalog.sql"
            ))?;
            connection.execute_batch(AUX_SCHEMA)?;
        } else {
            let version: Option<u64> = connection
                .query_row(
                    "SELECT schema_version FROM catalog_meta WHERE singleton=1",
                    [],
                    |r| row_uint(r, 0),
                )
                .optional()?;
            if version.is_some_and(|v| v != 6) {
                return Err(CatalogError::SchemaMismatch(
                    "catalog schema version".into(),
                ));
            }
            let index: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='catalog_records')",
                [],
                |r| r.get(0),
            )?;
            if !index {
                return Err(CatalogError::SchemaMismatch(
                    "missing exact envelope index".into(),
                ));
            }
        }
        Ok(Self {
            connection,
            path: path.into(),
            context: None,
            pending_sequence: None,
            reading: false,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn writable(&self) -> CatalogResult<()> {
        if self.context.is_none() || self.reading {
            return Err(CatalogError::IncompleteExpansion(
                "no owned SQLite catalog transaction".into(),
            ));
        }
        Ok(())
    }
    fn expansion(&self) -> CatalogResult<u64> {
        self.context
            .as_ref()
            .and_then(|c| c.expansion)
            .ok_or_else(|| {
                CatalogError::IncompleteExpansion("record lacks predecessor expansion".into())
            })
    }
    fn typed_insert(&self, record: &Record, replace_private: bool) -> CatalogResult<()> {
        let v = &record.record;
        let integer = |name: &str| -> CatalogResult<SqlValue> {
            Ok(SqlValue::Integer(number(v, name)? as i64))
        };
        let text =
            |name: &str| -> CatalogResult<SqlValue> { Ok(SqlValue::Text(string(v, name)?.into())) };
        let blob = |name: &str| -> CatalogResult<SqlValue> {
            Ok(SqlValue::Blob(string(v, name)?.as_bytes().to_vec()))
        };
        let encoded = |name: &str| -> SqlValue { SqlValue::Blob(canonical(&v[name]).into_bytes()) };
        let nullable = |name: &str| -> SqlValue {
            match &v[name] {
                Value::Null => SqlValue::Null,
                Value::Number(n) => SqlValue::Integer(n.as_i64().expect("validated ID")),
                Value::String(s) => SqlValue::Text(s.clone()),
                _ => SqlValue::Blob(canonical(&v[name]).into_bytes()),
            }
        };
        let pairs: Vec<(&str, SqlValue)>;
        let table: &str;
        match record.kind.as_str() {
            "PhysicalRows" | "Episode" => {
                table = if record.kind == "PhysicalRows" {
                    "physical_rows"
                } else {
                    "episodes"
                };
                pairs = vec![
                    ("id", integer("id")?),
                    ("digest", SqlValue::Blob(hex_bytes(string(v, "sha256")?)?)),
                    ("canonical", blob("canonical_utf8")?),
                    ("utf8_bytes", integer("utf8_bytes")?),
                ];
            }
            "BoundaryObservation" => {
                table = "boundaries";
                pairs = vec![
                    ("id", integer("id")?),
                    ("physical_id", integer("physical_rows_id")?),
                    ("episode_id", integer("episode_id")?),
                    ("phase", text("phase")?),
                    ("canonical", blob("canonical_utf8")?),
                    ("omitted", blob("omitted_utf8")?),
                    ("witness", SqlValue::Blob(record.bytes())),
                ];
            }
            "Store" => {
                if replace_private {
                    self.connection.execute(
                        "UPDATE stores SET replay_parent_edge=?1 WHERE id=?2",
                        params![v["replay_parent_edge"].as_u64(), number(v, "id")?],
                    )?;
                    return Ok(());
                }
                table = "stores";
                pairs = vec![
                    ("id", integer("id")?),
                    ("physical_id", integer("physical_rows_id")?),
                    ("episode_id", integer("episode_id")?),
                    ("digest", SqlValue::Blob(hex_bytes(string(v, "sha256")?)?)),
                    ("canonical", blob("canonical_utf8")?),
                    ("initial_image_sha256", text("initial_image_id")?),
                    ("sqlite_schema_sha256", text("sqlite_schema_sha256")?),
                    ("boundary_id", integer("omitted_observation_id")?),
                    ("replay_parent_edge", nullable("replay_parent_edge")),
                    ("certification", text("certification")?),
                ];
            }
            "Product" => {
                if replace_private {
                    self.connection.execute(
                        "UPDATE products SET predecessor_edge=?1 WHERE id=?2",
                        params![v["predecessor_edge"].as_u64(), number(v, "id")?],
                    )?;
                    return Ok(());
                }
                table = "products";
                pairs = vec![
                    ("id", integer("id")?),
                    ("context", text("context")?),
                    ("store_id", integer("store_id")?),
                    ("digest", SqlValue::Blob(hex_bytes(string(v, "sha256")?)?)),
                    ("canonical", blob("canonical_utf8")?),
                    ("predecessor_edge", nullable("predecessor_edge")),
                ];
            }
            "OracleEvidence" => {
                table = "oracle_edges";
                pairs = vec![
                    ("id", integer("id")?),
                    (
                        "from_store",
                        SqlValue::Integer(number(&v["applied"], "from_store")? as i64),
                    ),
                    (
                        "to_store",
                        SqlValue::Integer(number(&v["applied"], "to_store")? as i64),
                    ),
                    (
                        "input",
                        SqlValue::Blob(string(&v["applied"], "action_utf8")?.as_bytes().to_vec()),
                    ),
                    ("applied", encoded("applied")),
                    ("production_witness", blob("production_witness_utf8")?),
                    (
                        "before_boundary",
                        SqlValue::Integer(reference_id(&v["before_boundary"])? as i64),
                    ),
                    (
                        "after_boundary",
                        SqlValue::Integer(reference_id(&v["after_boundary"])? as i64),
                    ),
                    (
                        "rollback_boundary",
                        SqlValue::Integer(reference_id(&v["rollback_boundary"])? as i64),
                    ),
                ];
            }
            "Candidate" => {
                table = "candidates";
                pairs = vec![
                    ("id", integer("id")?),
                    ("expansion_id", SqlValue::Integer(self.expansion()? as i64)),
                    ("ordinal", integer("ordinal")?),
                    ("descriptor", blob("descriptor_utf8")?),
                    ("classification", text("classification")?),
                    ("reason", nullable("reason")),
                ];
            }
            "Demand" => {
                table = "demands";
                pairs = vec![
                    ("id", integer("id")?),
                    ("candidate_id", integer("candidate")?),
                    ("ordinal", integer("ordinal")?),
                    ("from_store", integer("from_store")?),
                    ("role", text("role")?),
                    ("descriptor", SqlValue::Blob(record.bytes())),
                    ("classification", text("classification")?),
                    ("outcome_id", nullable("outcome_id")),
                    ("reason", nullable("reason")),
                ];
            }
            "ProductEdge" => {
                table = "product_edges";
                pairs = vec![
                    ("id", integer("id")?),
                    ("expansion_id", SqlValue::Integer(self.expansion()? as i64)),
                    ("candidate_id", integer("candidate")?),
                    ("from_product", integer("from_product")?),
                    ("to_product", integer("to_product")?),
                    ("descriptor", blob("branch_utf8")?),
                    ("primary_edge", nullable("primary_edge")),
                    ("resolution_edge", nullable("resolution_edge")),
                    ("read_edge", nullable("read_edge")),
                    ("cleanup_edge", nullable("cleanup_edge")),
                ];
            }
            "FairPair" => {
                self.connection.execute("INSERT INTO fair_pairs(context,from_product,to_product,nonstuttering) VALUES(?1,?2,?3,?4) ON CONFLICT(context,from_product,to_product) DO UPDATE SET nonstuttering=excluded.nonstuttering",params![string(v,"context")?,number(v,"from_product")?,number(v,"to_product")?,v["nonstuttering"].as_bool().expect("validated bool")])?;
                if replace_private {
                    self.connection.execute("DELETE FROM fair_labels WHERE context=?1 AND from_product=?2 AND to_product=?3",params![string(v,"context")?,number(v,"from_product")?,number(v,"to_product")?])?;
                }
                for label in v["labels"].as_array().expect("validated labels") {
                    self.connection.execute("INSERT INTO fair_labels(context,from_product,to_product,label) VALUES(?1,?2,?3,?4)",params![string(v,"context")?,number(v,"from_product")?,number(v,"to_product")?,label.as_str().expect("validated label")])?;
                }
                return Ok(());
            }
            "Enabledness" => {
                table = "enabledness";
                pairs = vec![
                    ("product_id", integer("product_id")?),
                    ("full_graph_labels", encoded("full_graph_labels")),
                ];
            }
            "Expansion" => {
                table = "expansions";
                pairs = vec![
                    ("id", integer("id")?),
                    ("predecessor", integer("predecessor")?),
                    ("fifo_ticket", integer("fifo_ticket")?),
                    ("unit_ordinal", integer("unit_ordinal")?),
                    ("descriptor", SqlValue::Blob(record.bytes())),
                    ("complete", SqlValue::Integer(1)),
                ];
            }
            "MonitorNode" => {
                table = "monitor_nodes";
                pairs=vec![("id",integer("id")?),("product_id",integer("product")?),("monitor",SqlValue::Blob(canonical(&json!({"seen_goal":v["seen_goal"],"capacity_ok":v["capacity_ok"],"pending_attempts":v["pending_attempts"]})).into_bytes()))];
            }
            "MonitorEdge" => {
                table = "monitor_edges";
                pairs = vec![
                    ("id", integer("id")?),
                    ("from_monitor", integer("from_monitor")?),
                    ("to_monitor", integer("to_monitor")?),
                    ("product_edge", integer("product_edge")?),
                ];
            }
            "Scc" => {
                table = "components";
                pairs = vec![
                    ("id", integer("id")?),
                    ("context", text("context")?),
                    ("property", text("property")?),
                    ("obligation", encoded("obligation")),
                    (
                        "view_key",
                        SqlValue::Blob(
                            canonical(&json!([v["context"], v["property"], v["obligation"]]))
                                .into_bytes(),
                        ),
                    ),
                    ("rank", integer("condensation_rank")?),
                    ("root", integer("monitor_root")?),
                    ("certificate", SqlValue::Blob(record.bytes())),
                ];
            }
            "Counts" | "ContextRoot" | "Outcome" => {
                table = "final_records";
                pairs =
                    vec![
                        ("kind", SqlValue::Text(record.kind.clone())),
                        (
                            "record_key",
                            SqlValue::Blob(record.key()?.canonical().into_bytes()),
                        ),
                        ("payload", SqlValue::Blob(record.bytes())),
                        (
                            "sealed_commit",
                            SqlValue::Integer(self.pending_sequence.ok_or_else(|| {
                                CatalogError::IncompleteExpansion("no commit".into())
                            })? as i64),
                        ),
                    ];
            }
            // These closed records carry no separate normative relational row.
            // Their complete immutable envelope remains in catalog_records.
            "Applied" | "InitialImage" | "Counterexample" => return Ok(()),
            _ => {
                return Err(CatalogError::CorruptRecord(
                    "controller record on semantic writer".into(),
                ))
            }
        }
        insert(&self.connection, table, &pairs)?;
        if record.kind == "Demand" {
            for dependency in v["depends_on"]
                .as_array()
                .expect("validated dependency array")
            {
                self.connection.execute(
                    "INSERT INTO demand_dependencies(demand_id,depends_on) VALUES(?1,?2)",
                    params![
                        number(v, "id")?,
                        dependency.as_u64().expect("validated dependency")
                    ],
                )?;
            }
        }
        if record.kind == "Scc" {
            let view = canonical(&json!([v["context"], v["property"], v["obligation"]]));
            for member in v["members"].as_array().expect("validated members") {
                self.connection.execute("INSERT INTO component_members(view_key,monitor_id,component_id) VALUES(?1,?2,?3)",params![view.as_bytes(),member.as_u64().expect("validated member"),number(v,"id")?])?;
            }
        }
        Ok(())
    }
}
fn insert(connection: &Connection, table: &str, pairs: &[(&str, SqlValue)]) -> CatalogResult<()> {
    let names = pairs
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = (1..=pairs.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    connection.execute(
        &format!("INSERT INTO {table}({names}) VALUES({placeholders})"),
        rusqlite::params_from_iter(pairs.iter().map(|(_, v)| v)),
    )?;
    Ok(())
}
fn reference_id(value: &Value) -> CatalogResult<u64> {
    number(value, "id")
}
fn hex_bytes(text: &str) -> CatalogResult<Vec<u8>> {
    if text.len() != 64 {
        return Err(CatalogError::CorruptRecord("SHA length".into()));
    }
    (0..64)
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|_| CatalogError::CorruptRecord("SHA hex".into()))
        })
        .collect()
}
impl Storage for SqliteStorage {
    fn begin(&mut self, context: WriteContext) -> CatalogResult<u64> {
        if self.context.is_some() || self.reading {
            return Err(CatalogError::SnapshotMismatch(
                "catalog transaction active".into(),
            ));
        }
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let next = self
                .sequence()?
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| CatalogError::NumericDomain("commit sequence exhausted".into()))?;
            self.connection.execute(
                "INSERT INTO catalog_commits(sequence,phase,unit_ordinal) VALUES(?1,?2,?3)",
                params![next, context.phase.name(), context.unit_ordinal],
            )?;
            Ok(next)
        })();
        match result {
            Ok(sequence) => {
                self.context = Some(context);
                self.pending_sequence = Some(sequence);
                Ok(sequence)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
    fn set_expansion(&mut self, expansion: u64) -> CatalogResult<()> {
        self.writable()?;
        self.context.as_mut().expect("writer").expansion = Some(expansion);
        Ok(())
    }
    fn commit(&mut self) -> CatalogResult<u64> {
        self.writable()?;
        let sequence = self.pending_sequence.expect("writer sequence");
        let binding = self
            .get_control("binding")?
            .ok_or_else(|| CatalogError::SourceMismatch("catalog lacks binding".into()))?;
        self.connection.execute("INSERT INTO catalog_meta(singleton,current_commit,schema_version,source_json,session_id,resource_plan_sha256) VALUES(1,?1,6,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET current_commit=excluded.current_commit",params![sequence,canonical(&binding["source"]).as_bytes(),string(&binding,"session_id")?,binding["resource_plan"]["record"]["plan_sha256"].as_str().ok_or_else(||CatalogError::SourceMismatch("plan digest".into()))?])?;
        self.connection.execute_batch("COMMIT")?;
        self.context.take();
        self.pending_sequence.take();
        Ok(sequence)
    }
    fn rollback(&mut self) -> CatalogResult<()> {
        self.writable()?;
        self.connection.execute_batch("ROLLBACK")?;
        self.context.take();
        self.pending_sequence.take();
        Ok(())
    }
    fn sequence(&self) -> CatalogResult<u64> {
        Ok(self
            .connection
            .query_row(
                "SELECT current_commit FROM catalog_meta WHERE singleton=1",
                [],
                |r| row_uint(r, 0),
            )
            .optional()?
            .unwrap_or(0))
    }
    fn allocate(&mut self, kind: &str) -> CatalogResult<u64> {
        self.writable()?;
        let old: u64 = self
            .connection
            .query_row(
                "SELECT last_id FROM catalog_allocators WHERE kind=?1",
                [kind],
                |r| row_uint(r, 0),
            )
            .optional()?
            .unwrap_or(0);
        let next = old
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| CatalogError::NumericDomain("global ID exhausted".into()))?;
        self.connection.execute("INSERT INTO catalog_allocators(kind,last_id) VALUES(?1,?2) ON CONFLICT(kind) DO UPDATE SET last_id=excluded.last_id",params![kind,next])?;
        Ok(next)
    }
    fn get(&self, kind: &str, key: &RecordKey) -> CatalogResult<Option<Record>> {
        let raw: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT payload FROM catalog_records WHERE kind=?1 AND key_json=?2",
                params![kind, key.canonical().as_bytes()],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|raw| {
            let record = Record::parse(&raw)?;
            if record.kind != kind || record.key()? != *key {
                return Err(CatalogError::CorruptRecord("envelope index key".into()));
            }
            Ok(record)
        })
        .transpose()
    }
    fn put(&mut self, record: &Record, replace_private: bool) -> CatalogResult<()> {
        self.writable()?;
        record.validate()?;
        let key = record.key()?;
        let old = self.get(&record.kind, &key)?;
        if let Some(old) = &old {
            if old.bytes() == record.bytes() {
                return Ok(());
            }
            if !replace_private {
                return Err(CatalogError::ConflictingOutcome(
                    "catalog immutable key".into(),
                ));
            }
        }
        self.typed_insert(record, replace_private)?;
        let (key_type, key_integer, key_text) = match &key {
            RecordKey::Numeric(id) => (0, Some(*id), None),
            RecordKey::Text(text) => (1, None, Some(text.as_str())),
        };
        self.connection.execute("INSERT INTO catalog_records(kind,key_type,key_integer,key_text,key_json,payload,digest,created_commit) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(kind,key_json) DO UPDATE SET payload=excluded.payload,digest=excluded.digest",params![record.kind,key_type,key_integer,key_text,key.canonical().as_bytes(),record.bytes(),hex_bytes(&record.sha256())?,self.pending_sequence.expect("owned sequence")])?;
        if old.is_none()
            && matches!(
                record.kind.as_str(),
                "PhysicalRows" | "Episode" | "Store" | "Product"
            )
        {
            let identity = if record.kind == "Product" {
                canonical(&json!([
                    record.record["context"],
                    record.record["canonical_utf8"]
                ]))
            } else {
                string(&record.record, "canonical_utf8")?.into()
            };
            self.connection.execute(
                "INSERT INTO catalog_identities(kind,digest,canonical,id) VALUES(?1,?2,?3,?4)",
                params![
                    record.kind,
                    hex_bytes(&digest(identity.as_bytes()))?,
                    identity.as_bytes(),
                    number(&record.record, "id")?
                ],
            )?;
        }
        Ok(())
    }
    fn find_canonical(&self, kind: &str, bytes: &str) -> CatalogResult<Option<Record>> {
        // Digest narrows candidates only. Complete canonical BLOB equality is
        // required even when two different values have the same digest.
        let id: Option<u64> = self
            .connection
            .query_row(
                "SELECT id FROM catalog_identities WHERE kind=?1 AND digest=?2 AND canonical=?3",
                params![
                    kind,
                    hex_bytes(&digest(bytes.as_bytes()))?,
                    bytes.as_bytes()
                ],
                |r| row_uint(r, 0),
            )
            .optional()?;
        id.map(|id| {
            self.get(kind, &RecordKey::Numeric(id))?
                .ok_or_else(|| CatalogError::CorruptRecord("identity index missing payload".into()))
        })
        .transpose()
    }
    fn scan(
        &self,
        kind: &str,
        after: Option<&RecordKey>,
        limits: &ReadLimits,
    ) -> CatalogResult<RecordBatch> {
        if limits.max_records == 0 || limits.max_encoded_bytes == 0 {
            return Err(CatalogError::ResourceIncomplete(
                "zero read allowance".into(),
            ));
        }
        let (after_type, after_integer, after_text) = match after {
            None => (-1, 0, String::new()),
            Some(RecordKey::Numeric(id)) => (0, *id, String::new()),
            Some(RecordKey::Text(text)) => (1, 0, text.clone()),
        };
        let mut statement=self.connection.prepare("SELECT payload FROM catalog_records WHERE kind=?1 AND (key_type>?2 OR (key_type=?2 AND ((key_type=0 AND key_integer>?3) OR (key_type=1 AND key_text>?4)))) ORDER BY key_type,key_integer,key_text LIMIT ?5")?;
        let bound = i64::try_from(limits.max_records)
            .map_err(|_| CatalogError::ResourceIncomplete("read count overflow".into()))?
            .checked_add(1)
            .ok_or_else(|| CatalogError::ResourceIncomplete("read count overflow".into()))?;
        let mut rows =
            statement.query(params![kind, after_type, after_integer, after_text, bound])?;
        let mut records = Vec::new();
        let mut bytes = 0u64;
        let mut more = false;
        while let Some(row) = rows.next()? {
            let raw: Vec<u8> = row.get(0)?;
            let size = raw.len() as u64;
            if records.len() >= limits.max_records
                || bytes
                    .checked_add(size)
                    .is_none_or(|n| n > limits.max_encoded_bytes)
            {
                if records.is_empty() {
                    return Err(CatalogError::ResourceIncomplete(
                        "indivisible catalog record exceeds read allowance".into(),
                    ));
                }
                more = true;
                break;
            }
            let record = Record::parse(&raw)?;
            if record.kind != kind {
                return Err(CatalogError::CorruptRecord("scan kind".into()));
            }
            records.push(record);
            bytes += size;
        }
        let next_after = if more {
            records.last().map(Record::key).transpose()?
        } else {
            None
        };
        Ok(RecordBatch {
            records,
            encoded_bytes: bytes,
            next_after,
        })
    }
    fn count(&self, kind: &str) -> CatalogResult<u64> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM catalog_records WHERE kind=?1",
            [kind],
            |r| row_uint(r, 0),
        )?)
    }
    fn get_control(&self, key: &str) -> CatalogResult<Option<Value>> {
        let bytes: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT payload FROM catalog_control WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        bytes
            .map(|b| serde_json::from_slice(&b).map_err(Into::into))
            .transpose()
    }
    fn set_control(&mut self, key: &str, value: &Value) -> CatalogResult<()> {
        self.writable()?;
        self.connection.execute("INSERT INTO catalog_control(key,payload) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload",params![key,canonical(value).as_bytes()])?;
        if key == "context_descriptors" {
            for context in value
                .as_array()
                .ok_or_else(|| CatalogError::CorruptRecord("context descriptor array".into()))?
            {
                self.connection.execute(
                    "INSERT INTO contexts(name,descriptor) VALUES(?1,?2)",
                    params![string(context, "id")?, canonical(context).as_bytes()],
                )?;
            }
        }
        Ok(())
    }
    fn enqueue(&mut self, product: u64) -> CatalogResult<u64> {
        self.writable()?;
        self.connection.execute(
            "INSERT INTO frontier(product_id,status) VALUES(?1,'pending')",
            params![product],
        )?;
        Ok(self.connection.last_insert_rowid() as u64)
    }
    fn oldest_pending(&self) -> CatalogResult<Option<(u64, u64)>> {
        Ok(self.connection.query_row("SELECT ticket,product_id FROM frontier WHERE status='pending' ORDER BY ticket LIMIT 1",[],|r|Ok((row_uint(r,0)?,row_uint(r,1)?))).optional()?)
    }
    fn mark_expanded(&mut self, product: u64) -> CatalogResult<()> {
        self.writable()?;
        if self.connection.execute(
            "UPDATE products SET expanded=1 WHERE id=?1 AND expanded=0",
            params![product],
        )? != 1
        {
            return Err(CatalogError::ConflictingOutcome(
                "predecessor expanded twice".into(),
            ));
        }
        if self.connection.execute(
            "UPDATE frontier SET status='expanded' WHERE product_id=?1 AND status='pending'",
            params![product],
        )? != 1
        {
            return Err(CatalogError::CorruptRecord("frontier expansion".into()));
        }
        Ok(())
    }
    fn pending(&self, context: Option<&str>) -> CatalogResult<u64> {
        Ok(self.connection.query_row("SELECT count(*) FROM frontier f JOIN products p ON p.id=f.product_id WHERE f.status='pending' AND (?1 IS NULL OR p.context=?1)",[context],|r|row_uint(r,0))?)
    }
    fn set_initial(&mut self, context: &str, product: u64) -> CatalogResult<()> {
        self.writable()?;
        if self.connection.execute(
            "UPDATE contexts SET initial_product=?1 WHERE name=?2 AND initial_product IS NULL",
            params![product, context],
        )? != 1
        {
            return Err(CatalogError::ConflictingOutcome(
                "context Init replaced/missing".into(),
            ));
        }
        Ok(())
    }
    fn initial(&self, context: &str) -> CatalogResult<Option<u64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT initial_product FROM contexts WHERE name=?1",
                [context],
                |r| row_optional_uint(r, 0),
            )
            .optional()?
            .flatten())
    }
    fn begin_read(&mut self, expected: u64) -> CatalogResult<()> {
        if self.context.is_some() || self.reading {
            return Err(CatalogError::SnapshotMismatch(
                "catalog writer/reader active".into(),
            ));
        }
        self.connection.execute_batch("BEGIN")?;
        match self.sequence() {
            Ok(sequence) if sequence == expected => {
                self.reading = true;
                Ok(())
            }
            result => {
                let _ = self.connection.execute_batch("ROLLBACK");
                match result {
                    Ok(_) => Err(CatalogError::SnapshotUnavailable(
                        "requested exact catalog high-water unavailable".into(),
                    )),
                    Err(error) => Err(error),
                }
            }
        }
    }
    fn end_read(&mut self) -> CatalogResult<()> {
        if !self.reading {
            return Err(CatalogError::SnapshotUnavailable(
                "catalog reader closed".into(),
            ));
        }
        self.connection.execute_batch("ROLLBACK")?;
        self.reading = false;
        Ok(())
    }
    fn backup(&self, path: &Path, limits: &super::BackupLimits) -> CatalogResult<()> {
        if !self.reading {
            return Err(CatalogError::SnapshotUnavailable(
                "backup lacks pinned snapshot".into(),
            ));
        }
        backup_connection(&self.connection, path, limits.max_bytes, limits.deadline)
    }
}

/// Actual SQLite backup, not a live file/WAL copy or a table reconstruction.
/// Caller owns a paused, exact read snapshot and the destination is new.
pub fn backup_connection(
    source: &Connection,
    path: &Path,
    max_bytes: u64,
    deadline: Instant,
) -> CatalogResult<()> {
    use rusqlite::backup::{Backup, StepResult};
    if path.exists() {
        return Err(CatalogError::Io("backup destination already exists".into()));
    }
    let pages: u64 = source.query_row("PRAGMA page_count", [], |r| row_uint(r, 0))?;
    let page_size: u64 = source.query_row("PRAGMA page_size", [], |r| row_uint(r, 0))?;
    if pages
        .checked_mul(page_size)
        .is_none_or(|bytes| bytes > max_bytes)
    {
        return Err(CatalogError::ResourceIncomplete(
            "catalog backup exceeds admitted bytes".into(),
        ));
    }
    let mut destination = Connection::open(path)?;
    {
        let backup = Backup::new(source, &mut destination)?;
        loop {
            if Instant::now() >= deadline {
                return Err(CatalogError::ResourceIncomplete(
                    "catalog backup deadline".into(),
                ));
            }
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => return Err(CatalogError::Io("unknown SQLite backup result".into())),
            }
            if std::fs::metadata(path)?.len() > max_bytes {
                return Err(CatalogError::ResourceIncomplete(
                    "catalog backup grew beyond admitted bytes".into(),
                ));
            }
        }
    }
    destination.execute_batch("PRAGMA journal_mode=DELETE;")?;
    destination
        .close()
        .map_err(|(_, error)| CatalogError::Io(error.to_string()))?;
    let file = std::fs::OpenOptions::new().read(true).open(path)?;
    file.sync_all()?;
    if file.metadata()?.len() > max_bytes || Instant::now() >= deadline {
        return Err(CatalogError::ResourceIncomplete(
            "catalog backup final bound".into(),
        ));
    }
    Ok(())
}
