//! Hiqlite bridge. All mutations share the SQLite backend's exact SQL.

use async_trait::async_trait;
use hiqlite::macros::params;

use super::background_jobs::{QueueSql, SCHEMA};
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use crate::error::StoreError;

struct JsonRow(String);

struct ProducerPayloadShape {
    columns: i64,
    current_columns: i64,
    table_sql: String,
}

impl From<&mut hiqlite::Row<'_>> for ProducerPayloadShape {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            columns: row.get("columns"),
            current_columns: row.get("current_columns"),
            table_sql: row.get("table_sql"),
        }
    }
}

fn integrity_schema_for_shape(shape: &ProducerPayloadShape) -> Result<&'static str, StoreError> {
    let schema = super::background_jobs_integrity::SCHEMA;
    let (alter, remainder) = schema.split_once("-- next statement\n").ok_or_else(|| {
        StoreError::Migration("producer payload migration has no statement boundary".into())
    })?;
    // Let SQLite generate the exact owned table definition. Text in a comment,
    // string, or another identifier can never substitute for the real CHECK.
    let expected = rusqlite::Connection::open_in_memory()?;
    let base = super::background_jobs_transcode::SCHEMA
        .split_once("-- next statement\n")
        .ok_or_else(|| StoreError::Migration("artifact table has no statement boundary".into()))?
        .0;
    expected.execute_batch(base)?;
    let table_sql = || {
        expected.query_row(
            "SELECT sql FROM sqlite_master WHERE name='background_transcode_artifacts'",
            [],
            |row| row.get::<_, String>(0),
        )
    };
    if shape.columns == 0 && shape.current_columns == 0 && shape.table_sql == table_sql()? {
        return Ok(schema);
    }
    expected.execute_batch(alter)?;
    if shape.columns == 1 && shape.current_columns == 1 && shape.table_sql == table_sql()? {
        return Ok(remainder);
    }
    Err(StoreError::Migration(
        "existing producer payload column has incompatible shape".into(),
    ))
}

impl From<&mut hiqlite::Row<'_>> for JsonRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(row.get("result_json"))
    }
}

struct ManagedShapeRow(String, String);
impl From<&mut hiqlite::Row<'_>> for ManagedShapeRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(row.get("name"), row.get("sql"))
    }
}

// Only bootstrap's single coordinator installs baseline schema. Even a delayed
// retry must not replace an adjunct view installed after its shape observation.
fn probe_bootstrap_statements(shape: &[(String, String)]) -> Result<Vec<String>, StoreError> {
    const ABSENT_GUARD: &str = "INSERT INTO cluster_meta(singleton) SELECT NULL WHERE EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('subtitle_managed_schema','subtitle_model_receipts','subtitle_managed_source_changed','subtitle_managed_source_deleted','subtitle_managed_config_write'))";
    let schema = super::background_jobs_probe::SCHEMA;
    let managed = shape
        .iter()
        .any(|(name, _)| name != "background_job_required_resources");
    let (schema, guard) = if managed {
        if super::subtitle_managed_schema::version(shape)? != Some(1) {
            return Err(StoreError::Migration(
                "bootstrap requires an exact managed extension".into(),
            ));
        }
        // Keep the probe index and publish trigger. The exact closed suffix
        // replaces a predecessor view and must never touch an installed adjunct.
        let (prefix, _) = schema
            .split_once("-- next statement\nDROP VIEW IF EXISTS background_job_required_resources;")
            .ok_or_else(|| StoreError::Migration("probe view boundary changed".into()))?;
        (prefix, None)
    } else {
        // The write-time assertion shares the same transaction as DROP/CREATE.
        // A concurrent completed or partial install therefore refuses the retry
        // before any view can be replaced, rather than relying on this read.
        (schema, Some(ABSENT_GUARD))
    };
    validate_sql(schema)?;
    Ok(guard
        .into_iter()
        .map(str::to_owned)
        .chain(
            schema
                .split("-- next statement")
                .map(str::trim)
                .filter(|sql| !sql.is_empty())
                .map(str::to_owned),
        )
        .collect())
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    validate_sql(SCHEMA)?;
    for result in timeout_store(client.batch(SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_domain::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_domain::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_library::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_library::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_resources::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_resources::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_subtitle::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_subtitle::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_subtitle::RECONCILE_SCHEMA)?;
    for result in
        timeout_store(client.batch(super::background_jobs_subtitle::RECONCILE_SCHEMA)).await?
    {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_provider::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_provider::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_artwork::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_artwork::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_transcode::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_transcode::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_embeddings::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_embeddings::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    let managed_shape = timeout_store(
        client
            // authority: installed adjunct shape decides whether bootstrap may replace its predecessor view.
            .query_consistent_map::<ManagedShapeRow, _>(
                super::subtitle_managed_schema::SHAPE_SQL,
                params!(),
            ),
    )
    .await?
    .into_iter()
    .map(|row| (row.0, row.1))
    .collect::<Vec<_>>();
    let probe = probe_bootstrap_statements(&managed_shape)?
        .into_iter()
        .map(|sql| (sql, params!()))
        .collect::<Vec<_>>();
    for result in timeout_store(client.txn(probe)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_integrity::SCHEMA)?;
    // authority: committed column constraints decide whether bootstrap ALTER still applies.
    let shape = timeout_store(client.query_consistent_map::<ProducerPayloadShape, _>(
        "SELECT (SELECT count(*) FROM pragma_table_xinfo('background_transcode_artifacts') WHERE name='producer_payload') AS columns, \
         (SELECT count(*) FROM pragma_table_xinfo('background_transcode_artifacts') WHERE name='producer_payload' AND type='TEXT' AND \"notnull\"=0 AND dflt_value IS NULL AND pk=0 AND hidden=0) AS current_columns, \
         COALESCE((SELECT sql FROM sqlite_master WHERE type='table' AND name='background_transcode_artifacts'),'') AS table_sql",
        params!(),
    )).await?;
    let [shape] = shape.as_slice() else {
        return Err(StoreError::Migration(
            "producer payload shape returned no unique row".into(),
        ));
    };
    for result in timeout_store(client.batch(integrity_schema_for_shape(shape)?)).await? {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs_predictions::SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs_predictions::SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    // Last, as the v62→v63 upgrade applies it, so a fresh cluster and an
    // upgraded one load the replaced triggers in the same order.
    validate_sql(super::background_jobs::RETENTION_SCHEMA)?;
    for result in timeout_store(client.batch(super::background_jobs::RETENTION_SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    // v64 after v63, as the upgrade chain applies them.
    validate_sql(super::background_jobs::RECEIPT_PRESSURE_SCHEMA)?;
    for result in
        timeout_store(client.batch(super::background_jobs::RECEIPT_PRESSURE_SCHEMA)).await?
    {
        result.map_err(database_error)?;
    }
    // v65 follows v64 on both fresh and upgraded clusters.
    validate_sql(super::background_jobs::VIEWER_ANALYSIS_SCHEMA)?;
    for result in
        timeout_store(client.batch(super::background_jobs::VIEWER_ANALYSIS_SCHEMA)).await?
    {
        result.map_err(database_error)?;
    }
    validate_sql(super::background_jobs::PREPARATION_INDEX_SCHEMA)?;
    timeout_store(client.execute(super::background_jobs::PREPARATION_INDEX_SCHEMA, params!()))
        .await?;
    // The fresh schema receives the same replaced source guards as the upgrade chain.
    validate_sql(super::background_jobs::COPY_OUTPUT_SCHEMA)?;
    let statements: Vec<(String, hiqlite::Params)> = super::background_jobs::COPY_OUTPUT_SCHEMA
        .split("-- next statement\n")
        .map(|sql| (sql.to_owned(), params!()))
        .collect();
    for result in timeout_store(client.txn(statements)).await? {
        result.map_err(database_error)?;
    }
    // Match encoded preparation routing and source guards on fresh clusters.
    validate_sql(super::background_jobs::ENCODED_OUTPUT_SCHEMA)?;
    let statements: Vec<(String, hiqlite::Params)> = super::background_jobs::ENCODED_OUTPUT_SCHEMA
        .split("-- next statement\n")
        .map(|sql| (sql.to_owned(), params!()))
        .collect();
    for result in timeout_store(client.txn(statements)).await? {
        result.map_err(database_error)?;
    }
    Ok(())
}

pub(super) async fn seal_legacy(client: &super::hiqlite::TimedClient) -> Result<(), StoreError> {
    let statements: Vec<(String, hiqlite::Params)> = SCHEMA
        .split("-- next statement\n")
        .filter(|sql| {
            sql.trim_start()
                .starts_with("INSERT INTO background_job_legacy")
                || sql
                    .trim_start()
                    .starts_with("INSERT INTO background_job_migration")
                || sql
                    .trim_start()
                    .starts_with("DELETE FROM background_job_legacy WHERE kind")
        })
        .map(|sql| (sql.to_owned(), params!()))
        .collect();
    for result in client.txn(statements).await? {
        result.map_err(database_error)?;
    }
    Ok(())
}

#[async_trait]
impl QueueSql for HiqliteAuthStore {
    async fn queue_transaction(&self, statements: Vec<(String, String)>) -> Result<(), StoreError> {
        let statements: Vec<(String, hiqlite::Params)> = statements
            .into_iter()
            .map(|(sql, request)| (sql, params!(request)))
            .collect();
        for result in self.client().txn(statements).await? {
            result.map_err(database_error)?;
        }
        Ok(())
    }

    async fn queue_sql(
        &self,
        sql: String,
        request: String,
        mutation: bool,
        authoritative: bool,
    ) -> Result<Vec<String>, StoreError> {
        if mutation {
            self.client()
                .execute_returning_map::<_, JsonRow>(sql, params!(request))
                .await?
                .into_iter()
                .map(|row| row.map(|row| row.0).map_err(database_error))
                .collect()
        } else {
            let rows = if authoritative {
                self.client()
                    // authority: claim reconciliation must observe committed ownership.
                    .query_consistent_map::<JsonRow, _>(sql, params!(request))
                    .await?
            } else {
                self.client()
                    .query_map::<JsonRow, _>(sql, params!(request))
                    .await?
            };
            Ok(rows.into_iter().map(|row| row.0).collect())
        }
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use crate::store::SqliteStore;

    fn shape(conn: &rusqlite::Connection) -> Vec<(String, String)> {
        conn.prepare(super::super::subtitle_managed_schema::SHAPE_SQL)
            .expect("shape query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete shape")
    }

    #[test]
    fn bootstrap_probe_preserves_managed_view_and_refuses_partial_extension() {
        let conn = rusqlite::Connection::open_in_memory().expect("bootstrap fixture");
        SqliteStore::apply_migrations_for_test(&conn, 111).expect("actual current SQLite schema");
        let before = shape(&conn);
        let statements = probe_bootstrap_statements(&before).expect("exact installed extension");
        assert_eq!(
            statements.len(),
            2,
            "retain both probe index and publish trigger"
        );
        for sql in statements {
            conn.execute_batch(&sql).expect("idempotent probe prefix");
        }
        assert_eq!(
            shape(&conn),
            before,
            "managed resource obligations remain exact"
        );
        conn.execute_batch("DROP TRIGGER subtitle_managed_source_deleted")
            .expect("partial extension witness");
        assert!(
            probe_bootstrap_statements(&shape(&conn)).is_err(),
            "partial extension cannot be silently repaired"
        );
    }

    #[test]
    fn bootstrap_probe_absence_guard_refuses_a_concurrent_managed_install() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("bootstrap race fixture");
        SqliteStore::apply_migrations_for_test(&conn, 110).expect("actual predecessor schema");
        let plan = probe_bootstrap_statements(&shape(&conn)).expect("observed absent extension");
        conn.execute_batch("CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),schema_version INTEGER NOT NULL); INSERT INTO cluster_meta VALUES(1,81);").expect("actual singleton refusal constraints");
        conn.execute_batch(super::super::subtitle_managed_schema::SCHEMA)
            .expect("another installer wins after the observation");
        let installed = shape(&conn);
        let transaction = conn.transaction().expect("owned bootstrap transaction");
        let refusal = transaction
            .execute(&plan[0], [])
            .expect_err("write-time guard must refuse before DROP VIEW");
        assert!(
            matches!(refusal, rusqlite::Error::SqliteFailure(error, _) if error.code == rusqlite::ErrorCode::ConstraintViolation),
            "only the actual constraint refusal proves the write-time fence"
        );
        transaction.rollback().expect("rollback refused retry");
        assert_eq!(shape(&conn), installed, "concurrent winner remains intact");
    }
}
