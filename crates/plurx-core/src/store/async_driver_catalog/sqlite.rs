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
pub(super) fn row_uint(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
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
CREATE TABLE IF NOT EXISTS catalog_record_counts(kind TEXT PRIMARY KEY,record_count INTEGER NOT NULL CHECK(record_count>=0));
";

pub struct SqliteStorage {
    connection: Connection,
    path: PathBuf,
    context: Option<WriteContext>,
    pending_sequence: Option<u64>,
    reading: bool,
    read_limit: u64,
    reservation: Option<super::ResourceLimits>,
    /// Child/parent view of the limits for a read-only restored attachment.
    attached: Option<super::ResourceLimits>,
}
impl SqliteStorage {
    pub fn open(path: &Path, resident_bytes: u64) -> CatalogResult<Self> {
        Self::open_inner(path, resident_bytes, None)
    }
    pub fn open_reserved(
        path: &Path,
        resident_bytes: u64,
        limits: &super::ResourceLimits,
    ) -> CatalogResult<Self> {
        Self::open_inner(path, resident_bytes, Some(limits))
    }
    fn open_inner(
        path: &Path,
        resident_bytes: u64,
        limits: Option<&super::ResourceLimits>,
    ) -> CatalogResult<Self> {
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
        if let Some(limits) = limits {
            Self::prospective_family(limits)?;
        }
        let connection = Connection::open(path)?;
        if let Some(limits) = limits {
            Self::enforce_page_cap(&connection, limits.family("catalog_main")?.database_bytes)?;
        }
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA temp_store=FILE;",
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
            read_limit: resident_bytes / 4,
            reservation: limits.cloned(),
            attached: None,
        })
    }
    /// Attach an already-restored catalog read-only. `attached_bytes` is the
    /// size of the reservation the parent already holds for this file (None for
    /// the parent/fixture view): no second main allocation is created, the
    /// effective page ceiling is checked and no pragma that mutates is run.
    pub fn open_readonly(
        path: &Path,
        resident_bytes: u64,
        limits: super::ResourceLimits,
        attached_bytes: Option<u64>,
    ) -> CatalogResult<Self> {
        let metadata = path.symlink_metadata()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(CatalogError::Io(
                "restore catalog is not a regular file".into(),
            ));
        }
        if attached_bytes.is_some_and(|held| metadata.len() > held) {
            return Err(CatalogError::ResourceIncomplete(
                "restored catalog exceeds its attached reservation".into(),
            ));
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            if Path::new(&name).symlink_metadata().is_ok() {
                return Err(CatalogError::ResourceIncomplete(
                    "restored catalog has unaccounted companion files".into(),
                ));
            }
        }
        let connection = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA query_only=ON; PRAGMA cache_size=-4096;",
        )?;
        Ok(Self {
            connection,
            path: path.into(),
            context: None,
            pending_sequence: None,
            reading: false,
            read_limit: resident_bytes / 4,
            reservation: None,
            attached: Some(limits),
        })
    }
    /// Manual operational measurement, never a behavioral/proof result.
    /// The real empty harness open path installs DDL but inserts no metadata.
    pub fn empty_geometry(&self) -> CatalogResult<Value> {
        if self.context.is_some() || self.reading || self.sequence()? != 0 {
            return Err(CatalogError::CorruptRecord(
                "calibration requires empty unbound harness".into(),
            ));
        }
        let normative =
            include_str!("../../../../../verification/async-driver-refinement/paged-catalog.sql");
        let mut preimage = normative.as_bytes().to_vec();
        preimage.push(0);
        preimage.extend_from_slice(AUX_SCHEMA.as_bytes());
        let mut rows = serde_json::Map::new();
        let mut statement = self
            .connection
            .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")?;
        let mut tables = statement.query([])?;
        while let Some(table) = tables.next()? {
            let name: String = table.get(0)?;
            let quoted = name.replace('"', "\"\"");
            let count = self.connection.query_row(
                &format!("SELECT count(*) FROM \"{quoted}\""),
                [],
                |row| row_uint(row, 0),
            )?;
            if count != 0 {
                return Err(CatalogError::CorruptRecord(
                    "calibration database contains rows".into(),
                ));
            }
            rows.insert(name, json!(count));
        }
        let scalar = |sql: &str| -> CatalogResult<u64> {
            Ok(self.connection.query_row(sql, [], |row| row_uint(row, 0))?)
        };
        let mut objects = Vec::new();
        let mut table_names = Vec::new();
        let mut index_names = Vec::new();
        let mut schema=self.connection.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE type IN ('table','index') ORDER BY type,name,tbl_name")?;
        for row in schema.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })? {
            let (kind, name, table, sql) = row?;
            if kind == "table" {
                table_names.push(name.clone());
            } else {
                index_names.push(name.clone());
            }
            let normalized = sql.map(|sql| {
                sql.replace("IF NOT EXISTS ", "")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_end_matches(';')
                    .to_owned()
            });
            objects.push(json!([kind, name, table, normalized]));
        }
        let geometry = json!({"database_role":"rust_catalog","sqlite_source_id":self.connection.query_row("SELECT sqlite_source_id()",[],|row|row.get::<_,String>(0))?,
            "schema_sha256":digest(canonical(&json!(objects)).as_bytes()),"page_size":scalar("PRAGMA page_size")?,"page_count":scalar("PRAGMA page_count")?,"tables":table_names,"indexes":index_names});
        Ok(
            json!({"schema_version":1,"engine":"rust_rusqlite","source_path":"verification/async-driver-refinement/paged-catalog.sql","acceptance":false,
            "sqlite_version":rusqlite::version(),"sqlite_source_id":self.connection.query_row("SELECT sqlite_source_id()",[],|row|row.get::<_,String>(0))?,
            "ddl_sha256":digest(&preimage),"source_sha256":digest(normative.as_bytes()),"auxiliary_ddl_sha256":digest(AUX_SCHEMA.as_bytes()),
            "compiled_sources":super::compiled_catalog_sources(),"geometry":geometry,"page_size":scalar("PRAGMA page_size")?,"page_count":scalar("PRAGMA page_count")?,"freelist_count":scalar("PRAGMA freelist_count")?,
            "tables":scalar("SELECT count(*) FROM sqlite_schema WHERE type='table'")?,"indexes":scalar("SELECT count(*) FROM sqlite_schema WHERE type='index'")?,"empty_table_rows":rows}),
        )
    }
    fn prospective_family(limits: &super::ResourceLimits) -> CatalogResult<()> {
        let family = limits.family("catalog_main")?;
        let journal = family
            .database_bytes
            .checked_mul(2)
            .and_then(|v| v.checked_add(1024 * 1024));
        let temp = family.database_bytes.checked_mul(2);
        if journal.is_none_or(|bytes| bytes > family.journal_bytes)
            || temp.is_none_or(|bytes| bytes > family.temp_bytes)
        {
            return Err(CatalogError::ResourceIncomplete(
                "catalog prospective journal/temp reservation exhausted".into(),
            ));
        }
        Ok(())
    }
    fn enforce_page_cap(connection: &Connection, maximum: u64) -> CatalogResult<()> {
        let page_size = connection.query_row("PRAGMA page_size", [], |row| row_uint(row, 0))?;
        let pages = maximum / page_size;
        let current = connection.query_row("PRAGMA page_count", [], |row| row_uint(row, 0))?;
        if pages == 0 || pages > i64::MAX as u64 || current > pages {
            return Err(CatalogError::ResourceIncomplete(
                "catalog effective page ceiling below existing database".into(),
            ));
        }
        let effective =
            connection.query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                row_uint(row, 0)
            })?;
        if effective != pages {
            return Err(CatalogError::ResourceIncomplete(
                "ineffective catalog max_page_count".into(),
            ));
        }
        Ok(())
    }
    fn check_readonly_cap(&self, limits: &super::ResourceLimits) -> CatalogResult<()> {
        // Size and page ceiling only: an immutable attachment writes no journal
        // and owns no second main file; ancillary work needs its own grant.
        let size = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row_uint(row, 0))?;
        let pages = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row_uint(row, 0))?;
        if pages > limits.family("catalog_main")?.database_bytes / size {
            return Err(CatalogError::ResourceIncomplete(
                "immutable restored catalog exceeds effective main cap".into(),
            ));
        }
        Ok(())
    }
    pub fn reserve_operational_storage(
        &mut self,
        limits: &super::ResourceLimits,
    ) -> CatalogResult<()> {
        Self::prospective_family(limits)?;
        Self::enforce_page_cap(
            &self.connection,
            limits.family("catalog_main")?.database_bytes,
        )?;
        self.reservation = Some(limits.clone());
        self.check_storage()
    }
    fn check_storage(&self) -> CatalogResult<()> {
        let Some(limits) = &self.reservation else {
            return Ok(());
        };
        Self::prospective_family(limits)?;
        let maximum = limits.family("catalog_main")?.database_bytes;
        let pages = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row_uint(row, 0))?;
        let size = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row_uint(row, 0))?;
        let effective = self
            .connection
            .query_row("PRAGMA max_page_count", [], |row| row_uint(row, 0))?;
        if effective != maximum / size {
            return Err(CatalogError::ResourceIncomplete(
                "catalog max_page_count changed".into(),
            ));
        }
        if pages.checked_mul(size).is_none_or(|bytes| bytes > maximum) {
            return Err(CatalogError::ResourceIncomplete(
                "catalog data/index pages exceed immutable reservation".into(),
            ));
        }
        let mut wal = 0u64;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = self.path.as_os_str().to_os_string();
            name.push(suffix);
            match Path::new(&name).symlink_metadata() {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(CatalogError::Io("catalog journal symlink".into()));
                    }
                    wal = wal.checked_add(metadata.len()).ok_or_else(|| {
                        CatalogError::ResourceIncomplete("catalog WAL reservation overflow".into())
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if wal > limits.family("catalog_main")?.journal_bytes {
            return Err(CatalogError::ResourceIncomplete(
                "catalog WAL/shared-memory exceeds immutable reservation".into(),
            ));
        }
        Ok(())
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
        self.typed_record(record, replace_private, false)
    }
    fn typed_record(
        &self,
        record: &Record,
        replace_private: bool,
        verify: bool,
    ) -> CatalogResult<()> {
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
                    (
                        "expansion_id",
                        SqlValue::Integer(if verify {
                            let predecessor = if record.kind == "Candidate" {
                                number(v, "predecessor")?
                            } else {
                                number(v, "from_product")?
                            };
                            self.connection.query_row(
                                "SELECT id FROM expansions WHERE predecessor=?1",
                                params![predecessor],
                                |row| row_uint(row, 0),
                            )?
                        } else {
                            self.expansion()?
                        } as i64),
                    ),
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
                    (
                        "expansion_id",
                        SqlValue::Integer(if verify {
                            let predecessor = if record.kind == "Candidate" {
                                number(v, "predecessor")?
                            } else {
                                number(v, "from_product")?
                            };
                            self.connection.query_row(
                                "SELECT id FROM expansions WHERE predecessor=?1",
                                params![predecessor],
                                |row| row_uint(row, 0),
                            )?
                        } else {
                            self.expansion()?
                        } as i64),
                    ),
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
                if verify {
                    let matches:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM fair_pairs WHERE context=?1 AND from_product=?2 AND to_product=?3 AND nonstuttering=?4)",params![string(v,"context")?,number(v,"from_product")?,number(v,"to_product")?,v["nonstuttering"].as_bool().expect("validated bool")],|row|row.get(0))?;
                    if !matches {
                        return Err(CatalogError::CorruptRecord(
                            "typed fair pair differs".into(),
                        ));
                    }
                    let mut statement=self.connection.prepare("SELECT label FROM fair_labels WHERE context=?1 AND from_product=?2 AND to_product=?3 ORDER BY label")?;
                    let mut rows = statement.query(params![
                        string(v, "context")?,
                        number(v, "from_product")?,
                        number(v, "to_product")?
                    ])?;
                    let mut labels = v["labels"]
                        .as_array()
                        .expect("validated labels")
                        .iter()
                        .map(|label| label.as_str().expect("validated label"))
                        .collect::<Vec<_>>();
                    labels.sort_unstable();
                    for label in labels {
                        if rows
                            .next()?
                            .map(|row| row.get::<_, String>(0))
                            .transpose()?
                            .as_deref()
                            != Some(label)
                        {
                            return Err(CatalogError::CorruptRecord(
                                "typed fair labels differ".into(),
                            ));
                        }
                    }
                    if rows.next()?.is_some() {
                        return Err(CatalogError::CorruptRecord("extra typed fair label".into()));
                    }
                    return Ok(());
                }
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
                pairs = vec![
                    ("kind", SqlValue::Text(record.kind.clone())),
                    (
                        "record_key",
                        SqlValue::Blob(record.key()?.canonical().into_bytes()),
                    ),
                    ("payload", SqlValue::Blob(record.bytes())),
                    (
                        "sealed_commit",
                        SqlValue::Integer(if verify {
                            self.connection.query_row("SELECT created_commit FROM catalog_records WHERE kind=?1 AND key_json=?2",params![record.kind,record.key()?.canonical().as_bytes()],|row|row_uint(row,0))?
                        } else {
                            self.pending_sequence.ok_or_else(|| {
                                CatalogError::IncompleteExpansion("no commit".into())
                            })?
                        } as i64),
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
        if verify {
            let predicate = pairs
                .iter()
                .enumerate()
                .map(|(index, (name, _))| format!("{name} IS ?{}", index + 1))
                .collect::<Vec<_>>()
                .join(" AND ");
            let matches: u64 = self.connection.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {predicate}"),
                rusqlite::params_from_iter(pairs.iter().map(|(_, value)| value)),
                |row| row_uint(row, 0),
            )?;
            if matches != 1 {
                return Err(CatalogError::CorruptRecord(format!(
                    "typed {table} differs from exact envelope"
                )));
            }
        } else {
            insert(&self.connection, table, &pairs)?;
        }

        if record.kind == "Demand" {
            if verify {
                let count: u64 = self.connection.query_row(
                    "SELECT count(*) FROM demand_dependencies WHERE demand_id=?1",
                    params![number(v, "id")?],
                    |row| row_uint(row, 0),
                )?;
                if count
                    != v["depends_on"]
                        .as_array()
                        .expect("validated dependencies")
                        .len() as u64
                {
                    return Err(CatalogError::CorruptRecord(
                        "dependency count differs".into(),
                    ));
                }
            }
            for dependency in v["depends_on"]
                .as_array()
                .expect("validated dependency array")
            {
                if verify {
                    let found:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM demand_dependencies WHERE demand_id=?1 AND depends_on=?2)",params![number(v,"id")?,dependency.as_u64().expect("validated dependency")],|row|row.get(0))?;
                    if !found {
                        return Err(CatalogError::CorruptRecord(
                            "typed dependency differs".into(),
                        ));
                    }
                    continue;
                }
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
            if verify {
                let count: u64 = self.connection.query_row(
                    "SELECT count(*) FROM component_members WHERE component_id=?1",
                    params![number(v, "id")?],
                    |row| row_uint(row, 0),
                )?;
                if count != v["members"].as_array().expect("validated members").len() as u64 {
                    return Err(CatalogError::CorruptRecord(
                        "component member count differs".into(),
                    ));
                }
            }
            for member in v["members"].as_array().expect("validated members") {
                if verify {
                    let found:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM component_members WHERE view_key=?1 AND monitor_id=?2 AND component_id=?3)",params![view.as_bytes(),member.as_u64().expect("validated member"),number(v,"id")?],|row|row.get(0))?;
                    if !found {
                        return Err(CatalogError::CorruptRecord(
                            "typed SCC membership differs".into(),
                        ));
                    }
                    continue;
                }
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
        self.check_storage()?;
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
        // Flush uncommitted dirty pages before the quota observation. The
        // transaction is still rollbackable; no predecessor is visible yet.
        self.connection.cache_flush()?;
        self.check_storage()?;
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
                "SELECT length(payload),payload FROM catalog_records WHERE kind=?1 AND key_json=?2",
                params![kind, key.canonical().as_bytes()],
                |r| {
                    let length = row_uint(r, 0)?;
                    if length > self.read_limit {
                        return Err(rusqlite::Error::InvalidParameterName(
                            "bounded catalog record read".into(),
                        ));
                    }
                    r.get(1)
                },
            )
            .optional()?;
        raw.map(|raw| {
            let record = Record::parse_bounded(&raw, self.read_limit.saturating_mul(4))?;
            if record.kind != kind || record.key()? != *key {
                return Err(CatalogError::CorruptRecord("envelope index key".into()));
            }
            Ok(record)
        })
        .transpose()
    }
    fn put(&mut self, record: &Record, replace_private: bool) -> CatalogResult<()> {
        self.writable()?;
        self.check_storage()?;
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
        if old.is_none() {
            self.connection.execute("INSERT INTO catalog_record_counts(kind,record_count) VALUES(?1,1) ON CONFLICT(kind) DO UPDATE SET record_count=record_count+1",[record.kind.as_str()])?;
        }
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
        after_transaction: u64,
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
        let mut statement=self.connection.prepare("SELECT length(payload),payload FROM catalog_records WHERE kind=?1 AND created_commit>?6 AND (key_type>?2 OR (key_type=?2 AND ((key_type=0 AND key_integer>?3) OR (key_type=1 AND key_text>?4)))) ORDER BY key_type,key_integer,key_text LIMIT ?5")?;
        let bound = i64::try_from(limits.max_records)
            .map_err(|_| CatalogError::ResourceIncomplete("read count overflow".into()))?
            .checked_add(1)
            .ok_or_else(|| CatalogError::ResourceIncomplete("read count overflow".into()))?;
        let mut rows = statement.query(params![
            kind,
            after_type,
            after_integer,
            after_text,
            bound,
            after_transaction
        ])?;
        let mut records = Vec::new();
        let mut bytes = 0u64;
        let mut more = false;
        while let Some(row) = rows.next()? {
            let size = row_uint(row, 0)?;
            if records.len() >= limits.max_records
                || bytes
                    .checked_add(size)
                    .is_none_or(|n| n > limits.max_encoded_bytes.min(self.read_limit))
            {
                if records.is_empty() {
                    return Err(CatalogError::ResourceIncomplete(
                        "indivisible catalog record exceeds read allowance".into(),
                    ));
                }
                more = true;
                break;
            }
            let raw: Vec<u8> = row.get(1)?;
            let record = Record::parse_bounded(&raw, self.read_limit.saturating_mul(4))?;
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
        Ok(self
            .connection
            .query_row(
                "SELECT record_count FROM catalog_record_counts WHERE kind=?1",
                [kind],
                |row| row_uint(row, 0),
            )
            .optional()?
            .unwrap_or(0))
    }
    fn oracle_edge_for_input(&self, from: u64, input: &str) -> CatalogResult<Option<u64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id FROM oracle_edges WHERE from_store=?1 AND input=?2",
                params![from, input.as_bytes()],
                |r| row_uint(r, 0),
            )
            .optional()?)
    }
    fn get_control(&self, key: &str) -> CatalogResult<Option<Value>> {
        let bytes: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT length(payload),payload FROM catalog_control WHERE key=?1",
                [key],
                |r| {
                    if row_uint(r, 0)? > self.read_limit {
                        return Err(rusqlite::Error::InvalidParameterName(
                            "bounded catalog control read".into(),
                        ));
                    }
                    r.get(1)
                },
            )
            .optional()?;
        bytes
            .map(|b| {
                super::decoded_json_reservation(&b, self.read_limit.saturating_mul(4))?;
                super::strict_json(&b)
            })
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
    fn save_checkpoint(
        &mut self,
        checkpoint: &Record,
        work_unit: &Record,
        session: &Record,
    ) -> CatalogResult<()> {
        if self.context.is_some() || self.reading {
            return Err(CatalogError::SnapshotMismatch(
                "checkpoint bookkeeping requires paused writers/readers".into(),
            ));
        }
        let c = &checkpoint.record;
        let w = &work_unit.record;
        if number(c, "committed_transaction")? != self.sequence()? {
            return Err(CatalogError::SnapshotMismatch(
                "checkpoint high-water".into(),
            ));
        }
        self.check_storage()?;
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            self.connection.execute("INSERT INTO work_units(ordinal,expected_prior_checkpoint,stage,progress,registered_cases,source_json,status) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![number(w,"unit_ordinal")?,w["expected_prior_checkpoint"].as_str(),string(w,"stage")?,canonical(&w["progress"]).as_bytes(),canonical(&w["registered_cases"]).as_bytes(),canonical(&w["source"]).as_bytes(),string(w,"status")?])?;
            self.connection.execute("INSERT INTO checkpoints(sequence,digest,unit_ordinal,descriptor,previous_digest,page_root,transaction_high_water,oldest_pending_ticket,counts) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![number(c,"sequence")?,checkpoint.sha256(),number(c,"unit_ordinal")?,checkpoint.bytes(),c["previous_checkpoint"].as_str(),string(c,"page_root")?,number(c,"committed_transaction")?,c["oldest_pending_ticket"].as_u64(),canonical(&c["counts"]).as_bytes()])?;
            for (key, value) in [
                (
                    format!("checkpoint:{}", checkpoint.sha256()),
                    serde_json::to_value(checkpoint)?,
                ),
                (
                    "latest_checkpoint".into(),
                    serde_json::to_value(checkpoint)?,
                ),
                (
                    format!("work_unit:{}", number(w, "unit_ordinal")?),
                    serde_json::to_value(work_unit)?,
                ),
            ] {
                self.connection.execute("INSERT INTO catalog_control(key,payload) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload",params![key,canonical(&value).as_bytes()])?;
            }
            let binding = self
                .get_control("binding")?
                .ok_or_else(|| CatalogError::SourceMismatch("checkpoint binding".into()))?;
            let plan = &binding["resource_plan"];
            self.connection.execute("INSERT INTO resource_plans(digest,descriptor,admission) VALUES(?1,?2,'admitted') ON CONFLICT(digest) DO NOTHING",params![string(c,"resource_plan_sha256")?,canonical(plan).as_bytes()])?;
            let v = &session.record;
            self.connection.execute("INSERT INTO sessions(id,source_json,initial_image_sha256,resource_plan_sha256,phase,checkpoint_digest,next_unit,descriptor) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET phase=excluded.phase,checkpoint_digest=excluded.checkpoint_digest,next_unit=excluded.next_unit,descriptor=excluded.descriptor",params![string(v,"session_id")?,canonical(&v["source"]).as_bytes(),string(v,"initial_image_id")?,string(v,"resource_plan_sha256")?,string(v,"phase")?,checkpoint.sha256(),number(v,"next_unit")?,session.bytes()])?;
            self.connection.execute("INSERT INTO catalog_control(key,payload) VALUES('session',?1) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload",params![session.bytes()])?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                if let Err(error) = self
                    .connection
                    .cache_flush()
                    .map_err(CatalogError::from)
                    .and_then(|()| self.check_storage())
                {
                    let _ = self.connection.execute_batch("ROLLBACK");
                    return Err(error);
                }
                self.connection.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
    fn save_export(&mut self, record: &Record) -> CatalogResult<()> {
        if self.reading || self.context.is_some() {
            return Err(CatalogError::SnapshotMismatch(
                "export writer/read snapshot active".into(),
            ));
        }
        let key = format!("export:{}:{}", record.kind, record.key()?.canonical());
        if let Some(old) = self.get_control(&key)? {
            if old != serde_json::to_value(record)? {
                return Err(CatalogError::ConflictingOutcome(
                    "immutable export changed".into(),
                ));
            }
            return Ok(());
        }
        self.check_storage()?;
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            self.connection.execute(
                "INSERT INTO catalog_control(key,payload) VALUES(?1,?2)",
                params![key, record.bytes()],
            )?;
            let count_key = format!("export_count:{}", record.kind);
            let old = self
                .get_control(&count_key)?
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            self.connection.execute("INSERT INTO catalog_control(key,payload) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload",params![count_key,canonical(&json!(old.checked_add(1).ok_or_else(||CatalogError::NumericDomain("export count".into()))?)).as_bytes()])?;
            if record.kind == "Page" {
                let v = &record.record;
                self.connection.execute("INSERT INTO pages(id,kind,first_key,last_key,record_count,bytes,descriptor,previous_checkpoint,transaction_high_water) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![string(v,"page_id")?,string(v,"kind")?,canonical(&v["first_key"]).as_bytes(),canonical(&v["last_key"]).as_bytes(),number(v,"record_count")?,number(v,"utf8_bytes")?,record.bytes(),v["previous_checkpoint"].as_str(),number(v,"transaction_high_water")?])?;
                for dependency in v["dependencies"]
                    .as_array()
                    .expect("validated page dependencies")
                {
                    self.connection.execute("INSERT INTO page_dependencies(page_id,target_kind,target_id,sha256) VALUES(?1,?2,?3,?4)",params![string(v,"page_id")?,string(dependency,"kind")?,canonical(&dependency["id"]),string(dependency,"sha256")?])?;
                }
                self.connection.execute("INSERT INTO catalog_control(key,payload) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload",params![format!("last_page:{}",string(v,"kind")?),canonical(&v["page_id"]).as_bytes()])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if let Err(error) = self
                    .connection
                    .cache_flush()
                    .map_err(CatalogError::from)
                    .and_then(|()| self.check_storage())
                {
                    let _ = self.connection.execute_batch("ROLLBACK");
                    return Err(error);
                }
                self.connection.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
    fn check_private(&self, checkpoint: &Record) -> CatalogResult<()> {
        if self.context.is_some() || self.pending_sequence.is_some() {
            return Err(CatalogError::SnapshotMismatch(
                "restore contains an active predecessor".into(),
            ));
        }
        let sequence = self.sequence()?;
        if number(&checkpoint.record, "committed_transaction")? != sequence {
            return Err(CatalogError::SnapshotMismatch(
                "restore commit high-water".into(),
            ));
        }
        // Schema equality is checked against a schema-only database. No semantic
        // rows, IDs, FIFO state, or production image are reconstructed here.
        let expected = Connection::open_in_memory()?;
        expected.execute_batch(include_str!(
            "../../../../../verification/async-driver-refinement/paged-catalog.sql"
        ))?;
        expected.execute_batch(AUX_SCHEMA)?;
        let sql = "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name";
        let mut a = self.connection.prepare(sql)?;
        let mut b = expected.prepare(sql)?;
        let mut actual = a.query([])?;
        let mut wanted = b.query([])?;
        loop {
            match (actual.next()?, wanted.next()?) {
                (None, None) => break,
                (Some(actual), Some(wanted)) => {
                    for column in 0..4 {
                        if actual.get::<_, Option<String>>(column)?
                            != wanted.get::<_, Option<String>>(column)?
                        {
                            return Err(CatalogError::SchemaMismatch(
                                "private catalog schema differs".into(),
                            ));
                        }
                    }
                }
                _ => {
                    return Err(CatalogError::SchemaMismatch(
                        "private catalog schema inventory differs".into(),
                    ))
                }
            }
        }
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(CatalogError::CorruptRecord(
                "catalog integrity check".into(),
            ));
        }
        if self
            .connection
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
        {
            return Err(CatalogError::CorruptRecord(
                "catalog foreign key check".into(),
            ));
        }
        let binding = self
            .get_control("binding")?
            .ok_or_else(|| CatalogError::SourceMismatch("restore binding absent".into()))?;
        let plan: Record = serde_json::from_value(binding["resource_plan"].clone())?;
        // A read-only attachment spends the child's narrowed view, never the
        // plan's whole policy; the parent/fixture view derives it from the plan.
        let limits = match &self.attached {
            Some(view) => view.clone(),
            None => super::ResourceLimits::from_plan(&plan)?,
        };
        self.check_readonly_cap(&limits)?;
        let meta:(Vec<u8>,String,String)=self.connection.query_row("SELECT source_json,session_id,resource_plan_sha256 FROM catalog_meta WHERE singleton=1",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        if meta.0 != canonical(&binding["source"]).as_bytes()
            || meta.1 != string(&binding, "session_id")?
            || meta.2 != plan.sha256()
            || checkpoint.record["source"] != binding["source"]
            || checkpoint.record["session_id"] != binding["session_id"]
            || checkpoint.record["resource_plan_sha256"] != plan.sha256()
        {
            return Err(CatalogError::SourceMismatch(
                "restore private identity metadata".into(),
            ));
        }
        let invalid_checks=[
            ("commit sequence", "SELECT (SELECT count(*) FROM catalog_commits)!=(SELECT max(sequence) FROM catalog_commits) OR (SELECT max(sequence) FROM catalog_commits)!=(SELECT current_commit FROM catalog_meta)"),
            ("record counters", "SELECT EXISTS(SELECT 1 FROM catalog_record_counts c WHERE c.record_count!=(SELECT count(*) FROM catalog_records r WHERE r.kind=c.kind)) OR EXISTS(SELECT 1 FROM catalog_records r WHERE NOT EXISTS(SELECT 1 FROM catalog_record_counts c WHERE c.kind=r.kind))"),
            ("allocator", "SELECT EXISTS(SELECT 1 FROM catalog_allocators a WHERE a.last_id!=(SELECT max(key_integer) FROM catalog_records r WHERE r.kind=a.kind AND r.key_type=0)) OR EXISTS(SELECT 1 FROM catalog_records r WHERE r.key_type=0 AND r.kind NOT IN ('Enabledness') AND NOT EXISTS(SELECT 1 FROM catalog_allocators a WHERE a.kind=r.kind))"),
            ("identity cardinality", "SELECT (SELECT count(*) FROM catalog_identities)!=(SELECT count(*) FROM catalog_records WHERE kind IN ('PhysicalRows','Episode','Store','Product'))"),
            ("FIFO membership", "SELECT (SELECT count(*) FROM products)!=(SELECT count(*) FROM frontier) OR EXISTS(SELECT 1 FROM products p JOIN frontier f ON f.product_id=p.id WHERE (p.expanded=1)!=(f.status='expanded') OR (p.expanded=1)!=EXISTS(SELECT 1 FROM expansions e WHERE e.predecessor=p.id AND e.fifo_ticket=f.ticket))"),
            ("FIFO order", "SELECT EXISTS(SELECT 1 FROM frontier WHERE ticket!=product_id) OR EXISTS(SELECT 1 FROM frontier p JOIN frontier e ON e.status='expanded' AND p.status='pending' AND p.ticket<e.ticket)"),
            ("initial contexts", "SELECT count(*)!=7 OR count(initial_product)!=7 FROM contexts"),
            ("creating Store witness", "SELECT EXISTS(SELECT 1 FROM stores s WHERE (s.id=1 AND (s.certification!='actual_init_checked' OR s.replay_parent_edge IS NOT NULL)) OR (s.id!=1 AND (s.certification!='novel_actual_state_checked' OR s.replay_parent_edge IS NULL OR NOT EXISTS(SELECT 1 FROM oracle_edges e WHERE e.id=s.replay_parent_edge AND e.to_store=s.id AND e.from_store<s.id))))"),
            ("Product discovery witness", "SELECT EXISTS(SELECT 1 FROM products p WHERE (EXISTS(SELECT 1 FROM contexts c WHERE c.initial_product=p.id) AND p.predecessor_edge IS NOT NULL) OR (NOT EXISTS(SELECT 1 FROM contexts c WHERE c.initial_product=p.id) AND (p.predecessor_edge IS NULL OR NOT EXISTS(SELECT 1 FROM product_edges e WHERE e.id=p.predecessor_edge AND e.to_product=p.id AND e.from_product<p.id))))"),
            ("boundary lifetime", "SELECT (SELECT count(*) FROM boundaries)!=1+3*(SELECT count(*) FROM oracle_edges)"),
            ("SQLite allocator", "SELECT EXISTS(SELECT 1 FROM sqlite_sequence s WHERE s.seq!=CASE s.name WHEN 'physical_rows' THEN (SELECT max(id) FROM physical_rows) WHEN 'episodes' THEN (SELECT max(id) FROM episodes) WHEN 'boundaries' THEN (SELECT max(id) FROM boundaries) WHEN 'stores' THEN (SELECT max(id) FROM stores) WHEN 'oracle_edges' THEN (SELECT max(id) FROM oracle_edges) WHEN 'products' THEN (SELECT max(id) FROM products) WHEN 'frontier' THEN (SELECT max(ticket) FROM frontier) WHEN 'expansions' THEN (SELECT max(id) FROM expansions) WHEN 'candidates' THEN (SELECT max(id) FROM candidates) WHEN 'demands' THEN (SELECT max(id) FROM demands) WHEN 'product_edges' THEN (SELECT max(id) FROM product_edges) WHEN 'monitor_nodes' THEN (SELECT max(id) FROM monitor_nodes) WHEN 'monitor_edges' THEN (SELECT max(id) FROM monitor_edges) WHEN 'components' THEN (SELECT max(id) FROM components) ELSE -1 END)"),
        ];
        for (name, sql) in invalid_checks {
            if self
                .connection
                .query_row(sql, [], |row| row.get::<_, bool>(0))?
            {
                return Err(CatalogError::CorruptRecord(format!(
                    "private {name} invariant"
                )));
            }
        }
        // Stream one bounded envelope at a time; the full catalog is never
        // collected in a map or accepted on digest equality alone.
        let mut statement=self.connection.prepare("SELECT kind,key_type,key_integer,key_text,key_json,length(payload),payload,digest,created_commit FROM catalog_records ORDER BY kind,key_type,key_integer,key_text")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(0)?;
            let allowance = limits.allowance(&kind)?;
            let length = row_uint(row, 5)?;
            if length > allowance.max_encoded_bytes || length > limits.resident_bytes / 4 {
                return Err(CatalogError::ResourceIncomplete(
                    "restore envelope decoded reservation".into(),
                ));
            }
            let bytes: Vec<u8> = row.get(6)?;
            let record = Record::parse_bounded(&bytes, limits.resident_bytes)?;
            let key = record.key()?;
            let expected_key = match row.get::<_, i64>(1)? {
                0 => RecordKey::Numeric(row_uint(row, 2)?),
                1 => RecordKey::Text(row.get(3)?),
                _ => return Err(CatalogError::CorruptRecord("private key type".into())),
            };
            if record.kind != kind
                || key != expected_key
                || row.get::<_, Vec<u8>>(4)? != key.canonical().as_bytes()
                || record.bytes() != bytes
                || row.get::<_, Vec<u8>>(7)? != hex_bytes(&record.sha256())?
                || row_uint(row, 8)? > sequence
            {
                return Err(CatalogError::CorruptRecord(
                    "private exact envelope index".into(),
                ));
            }
            self.typed_record(&record, false, true)?;
            if matches!(
                kind.as_str(),
                "PhysicalRows" | "Episode" | "Store" | "Product"
            ) {
                let identity = if kind == "Product" {
                    canonical(&json!([
                        record.record["context"],
                        record.record["canonical_utf8"]
                    ]))
                } else {
                    string(&record.record, "canonical_utf8")?.into()
                };
                let found:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM catalog_identities WHERE kind=?1 AND digest=?2 AND canonical=?3 AND id=?4)",params![kind,hex_bytes(&digest(identity.as_bytes()))?,identity.as_bytes(),number(&record.record,"id")?],|row|row.get(0))?;
                if !found {
                    return Err(CatalogError::CorruptRecord(
                        "private full identity index differs".into(),
                    ));
                }
            }
        }
        for (kind, table) in [
            ("PhysicalRows", "physical_rows"),
            ("Episode", "episodes"),
            ("BoundaryObservation", "boundaries"),
            ("Store", "stores"),
            ("Product", "products"),
            ("OracleEvidence", "oracle_edges"),
            ("Candidate", "candidates"),
            ("Demand", "demands"),
            ("ProductEdge", "product_edges"),
            ("FairPair", "fair_pairs"),
            ("Enabledness", "enabledness"),
            ("Expansion", "expansions"),
            ("MonitorNode", "monitor_nodes"),
            ("MonitorEdge", "monitor_edges"),
            ("Scc", "components"),
        ] {
            let actual: u64 =
                self.connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row_uint(row, 0)
                    })?;
            if actual != self.count(kind)? {
                return Err(CatalogError::CorruptRecord(format!(
                    "extra private {table} rows"
                )));
            }
            if actual > limits.allowance(kind)?.records {
                return Err(CatalogError::ResourceIncomplete(format!(
                    "restored {kind} lifetime allowance"
                )));
            }
        }
        let current_unit = self
            .get_control(&format!(
                "work_unit:{}",
                number(&checkpoint.record, "unit_ordinal")?
            ))?
            .ok_or_else(|| CatalogError::CorruptRecord("checkpoint private unit missing".into()))?;
        let current_unit: Record = serde_json::from_value(current_unit)?;
        current_unit.validate()?;
        let actual_session = self
            .get_control("session")?
            .ok_or_else(|| CatalogError::CorruptRecord("current Session missing".into()))?;
        let phase = match actual_session["record"]["phase"].as_str() {
            Some("resource_blocked") => super::SessionCheckpointPhase::ResourceBlocked,
            Some("closed_unqualified") => super::SessionCheckpointPhase::ClosedUnqualified,
            Some("checkpointed_incomplete") => {
                super::SessionCheckpointPhase::CheckpointedIncomplete
            }
            _ => return Err(CatalogError::CorruptRecord("producer Session phase".into())),
        };
        let session = super::checkpoint_session(checkpoint, &current_unit, phase)?;
        if self.get_control("session")? != Some(serde_json::to_value(&session)?)
            || self.get_control("latest_checkpoint")? != Some(serde_json::to_value(checkpoint)?)
        {
            return Err(CatalogError::CorruptRecord(
                "current controller/session bookkeeping differs".into(),
            ));
        }
        let plans: u64 =
            self.connection
                .query_row("SELECT count(*) FROM resource_plans", [], |row| {
                    row_uint(row, 0)
                })?;
        let sessions: u64 =
            self.connection
                .query_row("SELECT count(*) FROM sessions", [], |row| row_uint(row, 0))?;
        let actual_plan: Vec<u8> = self.connection.query_row(
            "SELECT descriptor FROM resource_plans WHERE digest=?1 AND admission='admitted'",
            params![plan.sha256()],
            |row| row.get(0),
        )?;
        let actual_session:Vec<u8>=self.connection.query_row("SELECT descriptor FROM sessions WHERE id=?1 AND source_json=?2 AND initial_image_sha256=?3 AND resource_plan_sha256=?4 AND checkpoint_digest=?5 AND next_unit=?6 AND phase=?7",params![string(&session.record,"session_id")?,canonical(&session.record["source"]).as_bytes(),string(&session.record,"initial_image_id")?,plan.sha256(),checkpoint.sha256(),number(&session.record,"next_unit")?,string(&session.record,"phase")?],|row|row.get(0))?;
        if plans != 1
            || sessions != 1
            || actual_plan != plan.bytes()
            || actual_session != session.bytes()
        {
            return Err(CatalogError::CorruptRecord(
                "private admitted plan/session row differs".into(),
            ));
        }
        let mut exports=self.connection.prepare("SELECT key,length(payload),payload FROM catalog_control WHERE key LIKE 'export:%' ORDER BY key")?;
        let mut exported = exports.query([])?;
        let mut export_counts = std::collections::BTreeMap::<String, u64>::new();
        while let Some(row) = exported.next()? {
            if row_uint(row, 1)? > limits.resident_bytes / 4 {
                return Err(CatalogError::ResourceIncomplete(
                    "restored export decoded reservation".into(),
                ));
            }
            let bytes: Vec<u8> = row.get(2)?;
            let record = Record::parse_bounded(&bytes, limits.resident_bytes)?;
            if !matches!(record.kind.as_str(), "Page" | "ManifestNode")
                || record.record["source"] != binding["source"]
                || record.record["session_id"] != binding["session_id"]
                || row.get::<_, String>(0)?
                    != format!("export:{}:{}", record.kind, record.key()?.canonical())
                || record.bytes() != bytes
                || bytes.len() as u64 > limits.allowance(&record.kind)?.max_encoded_bytes
            {
                return Err(CatalogError::CorruptRecord(
                    "private exported envelope binding".into(),
                ));
            }
            *export_counts.entry(record.kind.clone()).or_default() += 1;
            if record.kind == "Page" {
                let v = &record.record;
                let matches:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM pages WHERE id=?1 AND kind=?2 AND first_key=?3 AND last_key=?4 AND record_count=?5 AND bytes=?6 AND descriptor=?7 AND previous_checkpoint IS ?8 AND transaction_high_water=?9)",params![string(v,"page_id")?,string(v,"kind")?,canonical(&v["first_key"]).as_bytes(),canonical(&v["last_key"]).as_bytes(),number(v,"record_count")?,number(v,"utf8_bytes")?,bytes,v["previous_checkpoint"].as_str(),number(v,"transaction_high_water")?],|row|row.get(0))?;
                if !matches || number(v, "transaction_high_water")? > sequence {
                    return Err(CatalogError::CorruptRecord(
                        "private page index differs".into(),
                    ));
                }
                let count: u64 = self.connection.query_row(
                    "SELECT count(*) FROM page_dependencies WHERE page_id=?1",
                    params![string(v, "page_id")?],
                    |row| row_uint(row, 0),
                )?;
                if count
                    != v["dependencies"]
                        .as_array()
                        .expect("validated dependencies")
                        .len() as u64
                {
                    return Err(CatalogError::CorruptRecord(
                        "private page dependency count differs".into(),
                    ));
                }
                for dependency in v["dependencies"]
                    .as_array()
                    .expect("validated dependencies")
                {
                    let found:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM page_dependencies WHERE page_id=?1 AND target_kind=?2 AND target_id=?3 AND sha256=?4)",params![string(v,"page_id")?,string(dependency,"kind")?,canonical(&dependency["id"]),string(dependency,"sha256")?],|row|row.get(0))?;
                    if !found {
                        return Err(CatalogError::CorruptRecord(
                            "private page dependency differs".into(),
                        ));
                    }
                }
            } else {
                for child in record.record["children"]
                    .as_array()
                    .expect("validated manifest children")
                {
                    let child_kind = string(child, "kind")?;
                    let child_key: RecordKey = serde_json::from_value(child["id"].clone())?;
                    let target = self
                        .get_control(&format!("export:{child_kind}:{}", child_key.canonical()))?
                        .ok_or_else(|| {
                            CatalogError::CorruptRecord("private manifest child missing".into())
                        })?;
                    let target: Record = serde_json::from_value(target)?;
                    target.validate()?;
                    let level = number(&record.record, "level")?;
                    if target.sha256() != string(child, "sha256")?
                        || target.key() != Ok(child_key)
                        || target.kind != child_kind
                        || (level == 0 && child_kind != "Page")
                        || (level > 0
                            && (child_kind != "ManifestNode"
                                || number(&target.record, "level")?.checked_add(1) != Some(level)))
                    {
                        return Err(CatalogError::CorruptRecord(
                            "private manifest child identity/level differs".into(),
                        ));
                    }
                }
            }
        }
        for kind in ["Page", "ManifestNode"] {
            let count = export_counts.get(kind).copied().unwrap_or(0);
            let indexed = self
                .get_control(&format!("export_count:{kind}"))?
                .map(|value| {
                    value.as_u64().ok_or_else(|| {
                        CatalogError::CorruptRecord("private export counter type".into())
                    })
                })
                .transpose()?
                .unwrap_or(0);
            if count != indexed || count > limits.allowance(kind)?.records {
                return Err(CatalogError::CorruptRecord(
                    "private lifetime export counter differs".into(),
                ));
            }
        }
        let page_count: u64 =
            self.connection
                .query_row("SELECT count(*) FROM pages", [], |row| row_uint(row, 0))?;
        if page_count != export_counts.get("Page").copied().unwrap_or(0) {
            return Err(CatalogError::CorruptRecord(
                "extra private page rows".into(),
            ));
        }
        let finals: u64 =
            self.connection
                .query_row("SELECT count(*) FROM final_records", [], |row| {
                    row_uint(row, 0)
                })?;
        if finals != self.count("Counts")? + self.count("ContextRoot")? + self.count("Outcome")? {
            return Err(CatalogError::CorruptRecord(
                "extra final singleton rows".into(),
            ));
        }
        Ok(())
    }
    fn backup(&self, path: &Path, limits: &super::BackupLimits) -> CatalogResult<()> {
        if !self.reading {
            return Err(CatalogError::SnapshotUnavailable(
                "backup lacks pinned snapshot".into(),
            ));
        }
        self.check_storage()?;
        let maximum = if let Some(policy) = &self.reservation {
            limits
                .max_bytes
                .min(policy.family("catalog_main")?.database_bytes)
                .min(policy.pool("snapshot_spool_bytes")?)
        } else {
            return Err(CatalogError::ResourceIncomplete(
                "backup lacks physical reservation".into(),
            ));
        };
        backup_connection(&self.connection, path, maximum, limits.deadline)
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

/// Physical-cap mutations: the named catalog_main database ceiling, journal/temp
/// reservations and read-only restored attachment. Logical record quotas are
/// deliberately left untouched in every case.
#[cfg(test)]
mod physical_cap_tests {
    use super::*;
    use crate::store::async_driver_catalog::{
        fixture_configuration_shaped, FamilyLimits, ResourceLimits,
    };

    const PAGE: u64 = 4096;

    fn shaped(
        path: &Path,
        database_bytes: u64,
        journal_extra: i64,
        temp_extra: i64,
    ) -> ResourceLimits {
        let journal = (2 * database_bytes + 1024 * 1024)
            .checked_add_signed(journal_extra)
            .expect("journal");
        let temp = (2 * database_bytes)
            .checked_add_signed(temp_extra)
            .expect("temp");
        let config = fixture_configuration_shaped(
            path.to_path_buf(),
            FamilyLimits {
                database_bytes,
                main_bytes: database_bytes.max(1024 * 1024),
                journal_bytes: journal,
                temp_bytes: temp,
            },
        );
        ResourceLimits::from_plan(&config.resource_plan).expect("fixture physical limits")
    }
    fn pragma(storage: &SqliteStorage, name: &str) -> u64 {
        storage
            .connection
            .query_row(&format!("PRAGMA {name}"), [], |row| row_uint(row, 0))
            .expect("pragma")
    }

    #[test]
    fn database_ceiling_rounds_down_to_the_measured_page_size_and_is_effective() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let path = directory.path().join("catalog.sqlite");
        let limits = shaped(&path, 400 * PAGE + 17, 0, 0);
        let storage =
            SqliteStorage::open_reserved(&path, 4 * 1024 * 1024, &limits).expect("capped open");
        assert_eq!(pragma(&storage, "page_size"), PAGE);
        assert_eq!(
            pragma(&storage, "max_page_count"),
            400,
            "floor(capacity / measured page size), not a logical record estimate"
        );
        storage
            .check_storage()
            .expect("empty schema fits and the pragma is intact");
        storage
            .connection
            .execute_batch("PRAGMA max_page_count=999999")
            .expect("tamper");
        assert!(
            storage.check_storage().is_err(),
            "a changed physical cap is detected"
        );
    }

    #[test]
    fn restored_database_beyond_the_effective_ceiling_is_refused_unopened() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let path = directory.path().join("catalog.sqlite");
        let big = Connection::open(&path).expect("restored image");
        big.execute_batch("CREATE TABLE t(x BLOB); INSERT INTO t VALUES(zeroblob(400000));")
            .expect("fill");
        drop(big);
        let limits = shaped(&path, 3 * PAGE, 0, 0);
        assert!(
            matches!(
                SqliteStorage::open_reserved(&path, 4 * 1024 * 1024, &limits),
                Err(CatalogError::ResourceIncomplete(_))
            ),
            "an existing database already beyond max_page_count must not open"
        );
    }

    #[test]
    fn journal_and_temp_reservations_must_cover_the_ceiling_exactly() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let ok = shaped(&directory.path().join("a.sqlite"), 400 * PAGE, 0, 0);
        SqliteStorage::open_reserved(&directory.path().join("a.sqlite"), 4 * 1024 * 1024, &ok)
            .expect("equal reservations proceed");
        let journal = shaped(&directory.path().join("b.sqlite"), 400 * PAGE, -1, 0);
        assert!(
            matches!(
                SqliteStorage::open_reserved(
                    &directory.path().join("b.sqlite"),
                    4 * 1024 * 1024,
                    &journal
                ),
                Err(CatalogError::ResourceIncomplete(_))
            ),
            "journal one byte short"
        );
        let temp = shaped(&directory.path().join("c.sqlite"), 400 * PAGE, 0, -1);
        assert!(
            matches!(
                SqliteStorage::open_reserved(
                    &directory.path().join("c.sqlite"),
                    4 * 1024 * 1024,
                    &temp
                ),
                Err(CatalogError::ResourceIncomplete(_))
            ),
            "FILE temp one byte short"
        );
    }

    #[test]
    fn exhausting_the_page_cap_is_an_incomplete_disposition_and_rolls_back() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let path = directory.path().join("catalog.sqlite");
        let limits = shaped(&path, 400 * PAGE, 0, 0);
        let storage =
            SqliteStorage::open_reserved(&path, 4 * 1024 * 1024, &limits).expect("capped open");
        storage
            .connection
            .execute_batch("CREATE TABLE scratch(x BLOB)")
            .expect("scratch table");
        let error = storage
            .connection
            .execute("INSERT INTO scratch VALUES(zeroblob(4000000))", [])
            .expect_err("the enforced cap must stop the write");
        assert!(matches!(
            CatalogError::from(error),
            CatalogError::ResourceIncomplete(_)
        ));
        let rows: u64 = storage
            .connection
            .query_row("SELECT count(*) FROM scratch", [], |row| row_uint(row, 0))
            .expect("count");
        assert_eq!(rows, 0, "the failed statement published nothing");
        storage
            .check_storage()
            .expect("cap and prior state intact after rollback");
    }

    #[test]
    fn readonly_attachment_checks_the_held_reservation_without_mutating_anything() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let path = directory.path().join("restored.sqlite");
        let image = Connection::open(&path).expect("restored image");
        image
            .execute_batch("CREATE TABLE t(x BLOB); INSERT INTO t VALUES(zeroblob(40000));")
            .expect("fill");
        drop(image);
        let bytes = std::fs::metadata(&path).expect("size").len();
        let limits = shaped(&path, 400 * PAGE, 0, 0);
        let before = std::fs::read(&path).expect("bytes before");
        let held =
            SqliteStorage::open_readonly(&path, 4 * 1024 * 1024, limits.clone(), Some(bytes))
                .expect("attached to its held reservation");
        held.check_readonly_cap(&limits)
            .expect("within the effective ceiling");
        assert!(
            held.reservation.is_none(),
            "an attachment is never a second writable main allocation"
        );
        drop(held);
        assert!(
            matches!(
                SqliteStorage::open_readonly(
                    &path,
                    4 * 1024 * 1024,
                    limits.clone(),
                    Some(bytes - 1)
                ),
                Err(CatalogError::ResourceIncomplete(_))
            ),
            "attached file larger than the reservation held for it"
        );
        let tight = shaped(&path, 3 * PAGE, 0, 0);
        let over = SqliteStorage::open_readonly(&path, 4 * 1024 * 1024, tight.clone(), Some(bytes))
            .expect("opens read-only");
        assert!(
            over.check_readonly_cap(&tight).is_err(),
            "restored database beyond the effective page ceiling"
        );
        drop(over);
        std::fs::write(directory.path().join("restored.sqlite-journal"), b"x")
            .expect("stray companion");
        assert!(
            SqliteStorage::open_readonly(&path, 4 * 1024 * 1024, limits, Some(bytes)).is_err(),
            "unaccounted companion file"
        );
        assert_eq!(
            std::fs::read(&path).expect("bytes after"),
            before,
            "validation never writes the attached bytes"
        );
    }
}
