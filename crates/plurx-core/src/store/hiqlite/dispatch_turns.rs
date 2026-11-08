//! Replicated installation of the shared dispatch-turn schema.
use super::{admit_schema_migration, HiqliteAuthStore, RemoteShapeRow, SchemaMigrationAdmission};
use crate::error::StoreError;
use crate::store::background_job_turns::SCHEMA;
use hiqlite::macros::params;

const CATALOG: &str =
    "SELECT name, sql FROM sqlite_master WHERE name GLOB 'dispatch_turn_*' ORDER BY name";

fn expected_catalog() -> Result<Vec<(String, String)>, StoreError> {
    let connection = rusqlite::Connection::open_in_memory()?;
    // SQLite requires the subject of an external trigger to exist. Only the
    // dispatch objects themselves belong to this extension's shape.
    connection.execute_batch(
        "CREATE TABLE background_jobs(id TEXT);
         CREATE TABLE analysis_requests(request_id TEXT);
         CREATE TABLE background_source_custody(custody_id TEXT);",
    )?;
    connection.execute_batch(SCHEMA)?;
    let mut objects = connection.prepare(CATALOG)?;
    let rows = objects
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn installed_matches(
    installed: &[RemoteShapeRow],
    expected: &[(String, String)],
) -> Result<bool, StoreError> {
    if installed.is_empty() {
        return Ok(false);
    }
    if installed.len() != expected.len()
        || installed
            .iter()
            .zip(expected)
            .any(|(row, (name, sql))| row.name != *name || row.sql != *sql)
    {
        return Err(StoreError::Migration(
            "dispatch turn schema has an incompatible or partial shape".into(),
        ));
    }
    Ok(true)
}

impl HiqliteAuthStore {
    async fn dispatch_turns_installed(
        &self,
        expected: &[(String, String)],
    ) -> Result<bool, StoreError> {
        let installed = self
            .client()
            // authority: installation and lost-ack reconciliation require the committed dispatch catalog.
            .query_consistent_map::<RemoteShapeRow, _>(CATALOG, params!())
            .await?;
        installed_matches(&installed, expected)
    }

    pub(super) async fn install_dispatch_turns(
        &self,
        admission: SchemaMigrationAdmission<'_>,
    ) -> Result<(), StoreError> {
        let expected = expected_catalog()?;
        if self.dispatch_turns_installed(&expected).await? {
            return Ok(());
        }
        let mut statements = vec![(
            "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM sqlite_master WHERE name GLOB 'dispatch_turn_*') THEN 1 ELSE json('dispatch turn schema raced') END".to_owned(),
            params!(),
        )];
        statements.extend(
            SCHEMA
                .split("-- next statement\n")
                .filter(|sql| !sql.trim().is_empty())
                .map(|sql| (sql.to_owned(), params!())),
        );
        admit_schema_migration(admission)?;
        let attempt = self.schema_migration_transaction(statements).await;
        if self.dispatch_turns_installed(&expected).await? {
            return Ok(());
        }
        attempt?;
        Err(StoreError::Migration(
            "dispatch turn schema installation did not settle".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;

    #[test]
    fn dispatch_turn_upgrade_agrees_with_replicated_catalog_and_rejects_partial_shape() {
        let db = rusqlite::Connection::open_in_memory().expect("database");
        SqliteStore::apply_migrations_for_test(&db, 123).expect("predecessor");
        let expected = expected_catalog().expect("canonical extension");
        assert!(!expected.is_empty());
        let installed = || {
            db.prepare(CATALOG)
                .expect("catalog")
                .query_map([], |row| {
                    Ok(RemoteShapeRow {
                        name: row.get(0)?,
                        sql: row.get(1)?,
                    })
                })
                .expect("objects")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("object shape")
        };
        assert!(!installed_matches(&installed(), &expected).expect("extension absent"));
        SqliteStore::apply_next_migration_for_test(&db).expect("upgrade predecessor");
        let mut rows = installed();
        assert!(installed_matches(&rows, &expected).expect("v124 matches the replicated schema"));
        rows[0].sql.push_str(" /* incompatible */");
        assert!(installed_matches(&rows, &expected).is_err());
        rows.remove(0);
        if !rows.is_empty() {
            assert!(installed_matches(&rows, &expected).is_err());
        }
    }
}
