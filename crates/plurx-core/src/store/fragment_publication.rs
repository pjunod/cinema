//! Keep coordination requests out of legacy fragment-head supersession.
use crate::error::StoreError;
use std::sync::LazyLock;

const TRIGGER: &str = "background_job_publish_fragment_command";
const COORDINATOR_EXCLUSION: &str =
    "                AND newer.pipeline_version NOT LIKE 'coordination-contract:%'\n";
const NEWER_TARGET: &str =
    "                AND newer.target_node_id = analysis_requests.target_node_id\n";

pub(super) const SHAPE_SQL: &str = "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' \
    AND name = 'background_job_publish_fragment_command'";

static TRIGGERS: LazyLock<(&str, String)> = LazyLock::new(|| {
    let original = super::background_jobs::SCHEMA
        .split("-- next statement\n")
        .map(str::trim)
        .find(|sql| sql.starts_with(&format!("CREATE TRIGGER IF NOT EXISTS {TRIGGER}\n")))
        .expect("legacy fragment publication trigger");
    assert_eq!(original.matches(NEWER_TARGET).count(), 1);
    let replacement = original.replacen(
        NEWER_TARGET,
        &format!("{NEWER_TARGET}{COORDINATOR_EXCLUSION}"),
        1,
    );
    (original, replacement)
});

fn canonical(sql: &str) -> String {
    sql.trim()
        .trim_end_matches(';')
        .replacen("CREATE TRIGGER IF NOT EXISTS ", "CREATE TRIGGER ", 1)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The trigger body identifies the extension. Keeping the name and schema
/// versions lets old members keep using the replicated publish command.
pub(super) fn installed(sql: &str) -> Result<bool, StoreError> {
    let actual = canonical(sql);
    if actual == canonical(&TRIGGERS.1) {
        Ok(true)
    } else if actual == canonical(TRIGGERS.0) {
        Ok(false)
    } else {
        Err(StoreError::Migration(
            "fragment publication trigger has an unexpected shape".into(),
        ))
    }
}

pub(super) fn statements() -> [String; 2] {
    [format!("DROP TRIGGER {TRIGGER}"), TRIGGERS.1.clone()]
}

pub(super) fn install_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let transaction =
        rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let sql: String = transaction.query_row(SHAPE_SQL, [], |row| row.get(1))?;
    if !installed(&sql)? {
        for statement in statements() {
            transaction.execute_batch(&statement)?;
        }
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_publication_upgrade_keeps_the_catalogue_version_and_checks_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
        super::super::sqlite::SqliteStore::migrate(&conn).expect("base schema");
        let old_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("base version");
        let shape = || {
            conn.query_row(SHAPE_SQL, [], |row| row.get::<_, String>(1))
                .expect("publication trigger")
        };
        assert!(!installed(&shape()).expect("original form"));
        install_sqlite(&conn).expect("replace trigger");
        assert!(installed(&shape()).expect("replacement form"));
        install_sqlite(&conn).expect("repeat install");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version after extension"),
            old_version
        );
        assert!(installed(&shape().replace("newer.created_at_ms", "newer.updated_at_ms")).is_err());
    }
}
