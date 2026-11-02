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

// Read and validate before touching the managed extension. A marker alone is
// never evidence of a complete installation; both canonical SQL and its row
// are required. This is also the strict read-back after a lost/stale proposal.
async fn managed_bootstrap_layout(
    client: &hiqlite::Client,
) -> Result<(Option<i64>, String), StoreError> {
    let shape = {
        let mut rows = timeout_store(
            // authority: committed schema SQL decides whether bootstrap may replace the legacy view or must preserve the installed managed extension.
            client.query_consistent(super::subtitle_managed_schema::SHAPE_SQL, params!()),
        )
        .await?;
        rows.iter_mut()
            .map(|row| (row.get::<String>("name"), row.get::<String>("sql")))
            .collect::<Vec<_>>()
    };
    if shape.is_empty() {
        return Ok((None, String::new()));
    }
    let version = super::subtitle_managed_schema::bootstrap_version(&shape)?;
    if let Some(version) = version {
        // authority: canonical DDL cannot substitute for the committed singleton version row when deciding that another installation already won.
        let mut marker = timeout_store(client.query_consistent(
            "SELECT count(*) AS count FROM subtitle_managed_schema WHERE singleton=1 AND version=$1",
            params!(version),
        ))
        .await?;
        if marker.len() != 1 || marker[0].get::<i64>("count") != 1 {
            return Err(StoreError::Migration(
                "managed subtitle schema marker missing".into(),
            ));
        }
    }
    let view = shape
        .iter()
        .find(|(name, _)| name == "background_job_required_resources")
        .ok_or_else(|| StoreError::Migration("managed subtitle resource view missing".into()))?
        .1
        .clone();
    Ok((version, view))
}

async fn install_probe_and_managed_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    let (version, observed_view) = managed_bootstrap_layout(client).await?;
    if matches!(version, Some(1..=3)) {
        // Reinstall only idempotent probe objects; never replace a managed view.
        let prefix = super::background_jobs_probe::SCHEMA
            .split_once("-- next statement\nDROP VIEW")
            .ok_or_else(|| StoreError::Migration("probe view boundary missing".into()))?
            .0;
        for result in timeout_store(client.batch(prefix)).await? {
            result.map_err(database_error)?;
        }
        return Ok(());
    }
    let mut statements = vec![(
        super::subtitle_managed_schema::BOOTSTRAP_GUARD.to_owned(),
        params!(observed_view),
    )];
    statements.extend(
        super::background_jobs_probe::SCHEMA
            .split("-- next statement")
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .map(|sql| (sql.to_owned(), params!())),
    );
    statements.extend(
        super::subtitle_managed_schema::statements()
            .into_iter()
            .map(|sql| (sql, params!())),
    );
    // One guarded schema transaction owns both replacements. A stale legacy
    // observer cannot drop the view after a concurrent managed install commits.
    let attempt = timeout_store(client.txn(statements))
        .await
        .and_then(|results| {
            results
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(database_error)
        });
    let (settled, _) = managed_bootstrap_layout(client).await?;
    if matches!(settled, Some(1..=3)) {
        return Ok(());
    }
    attempt?;
    Err(StoreError::Migration(
        "managed subtitle bootstrap did not settle".into(),
    ))
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    // Refuse a pre-existing partial extension before any base CREATE IF NOT
    // EXISTS could recreate its missing view. Empty fresh bootstrap is allowed;
    // the guarded installer re-observes the exact predecessor after base DDL.
    managed_bootstrap_layout(client).await?;
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
    validate_sql(super::background_jobs_probe::SCHEMA)?;
    install_probe_and_managed_schema(client).await?;
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
