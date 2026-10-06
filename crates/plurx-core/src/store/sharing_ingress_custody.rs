//! Shared adjunct shape only. Durable JSON is accounting, never physical proof.
pub const SCHEMA: &str = include_str!("sharing_ingress_custody_schema.sql");
pub(crate) fn declaration() -> &'static str {
    SCHEMA[SCHEMA
        .find("CREATE TABLE")
        .expect("closed custody declaration")..]
        .trim()
        .trim_end_matches(';')
}
pub fn schema_guard() -> String {
    let declaration = declaration().replace('\'', "''");
    format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sharing_ingress_custody' AND sql='{declaration}') AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name='sharing_ingress_custody')")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, SQLITE_SCHEMA_VERSION};
    #[test]
    fn sharing_ingress_custody_migration_preserves_unresolved_debt_without_route_cascade() {
        let connection = rusqlite::Connection::open_in_memory().expect("database");
        SqliteStore::apply_migrations_for_test(&connection, 92)
            .expect("actual previous SQLite migration set");
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='sharing_ingress_custody'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("previous census"),
            0
        );
        SqliteStore::apply_migrations_for_test(&connection, SQLITE_SCHEMA_VERSION)
            .expect("actual custody migration");
        let incarnation = uuid::Uuid::new_v4().to_string();
        let json = "{\"version\":1,\"sealed\":true,\"slots\":[{\"closed_confirmation\":null}],\"highwater\":[]}";
        connection
            .execute(
                "INSERT INTO sharing_ingress_custody VALUES('source',?1,?2,?3,1)",
                rusqlite::params![incarnation, "a".repeat(64), json],
            )
            .expect("unresolved diagnostic debt");
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pragma_foreign_key_list('sharing_ingress_custody')",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("foreign-key census"),
            0
        );
        connection
            .execute("DELETE FROM media_sessions", [])
            .expect("old route cleanup");
        assert_eq!(connection.query_row("SELECT custody_json FROM sharing_ingress_custody WHERE principal_kind='source' AND incarnation_id=?1",[incarnation],|row| row.get::<_,String>(0)).expect("debt preserved"),json);
        assert!(connection
            .execute(
                "UPDATE sharing_ingress_custody SET custody_json='invalid-json'",
                []
            )
            .is_err());
        assert!(connection
            .execute(
                "UPDATE sharing_ingress_custody SET custody_json=?1",
                [serde_json::json!("x".repeat(65536)).to_string()]
            )
            .is_err());
        assert!(
            connection
                .query_row(
                    &format!("SELECT CASE WHEN {} THEN 1 ELSE 0 END", schema_guard()),
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("exact adjunct")
                == 1
        );
    }
}
