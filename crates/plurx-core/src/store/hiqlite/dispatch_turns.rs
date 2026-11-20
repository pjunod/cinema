//! Replicated installation of the shared dispatch-turn schema.
use super::{admit_schema_migration, HiqliteAuthStore, RemoteShapeRow, SchemaMigrationAdmission};
use crate::error::StoreError;
use crate::store::background_job_turns::{expected_catalog, CATALOG, LIVE_TITLE_MIGRATION, SCHEMA};
use hiqlite::macros::params;

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
    async fn dispatch_turns_catalog(&self) -> Result<Vec<RemoteShapeRow>, StoreError> {
        self
            .client()
            // authority: installation and lost-ack reconciliation require the committed dispatch catalog.
            .query_consistent_map::<RemoteShapeRow, _>(
                "dispatch_turns_installed",
                CATALOG,
                params!(),
            )
            .await
    }

    pub(super) async fn install_dispatch_turns(
        &self,
        admission: SchemaMigrationAdmission<'_>,
    ) -> Result<(), StoreError> {
        let expected = expected_catalog(true)?;
        let installed = self.dispatch_turns_catalog().await?;
        let (predecessor, migrations) = if installed.is_empty() {
            (Vec::new(), vec![SCHEMA, LIVE_TITLE_MIGRATION])
        } else {
            let original = expected_catalog(false)?;
            if installed_matches(&installed, &expected).is_ok() {
                return Ok(());
            }
            installed_matches(&installed, &original)?;
            (original, vec![LIVE_TITLE_MIGRATION])
        };
        // Compare the whole observed layout again inside the replicated writer.
        // A concurrent winner is recognized only after reading its complete catalog.
        let predecessor = serde_json::to_string(&predecessor).expect("schema string pairs serialize");
        let mut statements = vec![(
            format!("SELECT CASE WHEN (SELECT json_group_array(json_array(name,sql)) FROM ({CATALOG}))=$1 THEN 1 ELSE json('dispatch turn schema raced') END"),
            params!(predecessor),
        )];
        for migration in migrations {
            statements.extend(
                migration.split("-- next statement\n")
                    .filter(|sql| !sql.trim().is_empty())
                    .map(|sql| (sql.to_owned(), params!())),
            );
        }
        admit_schema_migration(admission)?;
        let attempt = self.schema_migration_transaction(statements).await;
        if installed_matches(&self.dispatch_turns_catalog().await?, &expected).unwrap_or(false) {
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
        let expected = expected_catalog(false).expect("canonical extension");
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
