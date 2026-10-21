//! Explicit initialized-schema fixtures. Cold constructors remain unchanged.
use super::*;
use rusqlite::backup::{Backup, StepResult};

// No caller receives this connection. After initialization query_only also
// rejects writes, so copying it cannot contend with a producer or a test.
static TEMPLATE: Mutex<Option<Connection>> = Mutex::new(None);

impl SqliteStore {
    /// A private writable fixture copied from the process's pristine schema.
    /// Migration, first-start and backfill tests must use the cold constructors.
    #[doc(hidden)]
    pub fn fixture_in_memory() -> Result<Self, StoreError> {
        let mut template = TEMPLATE
            .lock()
            .map_err(|_| StoreError::Task("SQLite fixture template lock poisoned".into()))?;
        if template.is_none() {
            let connection = Connection::open_in_memory()?;
            housekeeping::configure_writer(&connection)?;
            Self::migrate(&connection)?;
            Self::backfill_hdr_format(&connection)?;
            // These values belong to each fresh instance, not the shared schema.
            // init below recreates both settings and their creation timestamps.
            connection.execute(
                "DELETE FROM settings WHERE key IN ('instance.id', 'hdr_format_backfilled_v1')",
                [],
            )?;
            connection.pragma_update(None, "query_only", true)?;
            *template = Some(connection);
        }
        let mut connection = Connection::open_in_memory()?;
        {
            let backup = Backup::new(
                template.as_ref().expect("initialized template"),
                &mut connection,
            )?;
            // One finite copy, never run_to_completion's retry loop. The source
            // is immutable and the destination private; Busy/Locked/More refuse.
            match backup.step(-1)? {
                StepResult::Done => {}
                result => {
                    return Err(StoreError::Task(format!(
                        "SQLite fixture backup did not complete: {result:?}"
                    )));
                }
            }
        }
        drop(template);
        // Reapply connection-local pragmas, regenerate dynamic settings, run
        // current-schema verification, and construct entirely fresh Arc state.
        Self::init(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value;
    use std::collections::HashSet;

    fn quoted(name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn rows(connection: &Connection, sql: &str) -> Vec<Vec<Value>> {
        let mut statement = connection.prepare(sql).expect("snapshot statement");
        let columns = statement.column_count();
        let mapped = statement
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .expect("snapshot query");
        mapped
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("snapshot rows")
    }

    fn schema(connection: &Connection) -> Vec<Vec<Value>> {
        rows(
            connection,
            "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name",
        )
    }

    fn data(connection: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
        let tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .expect("table inventory")
            .query_map([], |row| row.get(0))
            .expect("table names")
            .collect::<rusqlite::Result<_>>()
            .expect("all table names");
        tables
            .into_iter()
            .map(|table| {
                let name = quoted(&table);
                let projection = if connection
                    .prepare(&format!("SELECT rowid, * FROM {name}"))
                    .is_ok()
                {
                    "rowid, *"
                } else {
                    "*" // WITHOUT ROWID tables retain their primary-key columns.
                };
                let filter = if table == "settings" {
                    " WHERE key NOT IN ('instance.id', 'hdr_format_backfilled_v1')"
                } else {
                    ""
                };
                let select = format!("SELECT {projection} FROM {name}{filter}");
                let columns = connection
                    .prepare(&select)
                    .expect("table projection")
                    .column_count();
                let order = (1..=columns)
                    .map(|column| column.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                (
                    table,
                    rows(connection, &format!("{select} ORDER BY {order}")),
                )
            })
            .collect()
    }

    fn setting(connection: &Connection, key: &str) -> (String, i64) {
        connection
            .query_row(
                "SELECT value, updated_at FROM settings WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("dynamic setting")
    }

    fn seed_item(connection: &Connection, title: &str) {
        connection
            .execute_batch(
                "INSERT INTO libraries(id,name,kind,paths) VALUES(1,'Fixture','movies','[]');",
            )
            .expect("private library");
        connection
            .execute(
                "INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie',?1,?1)",
                [title],
            )
            .expect("private item");
    }

    #[test]
    fn fixture_schema_and_all_table_data_match_a_cold_store() {
        let cold = SqliteStore::open_in_memory().expect("real cold initialization");
        let fixture = SqliteStore::fixture_in_memory().expect("schema fixture");
        let cold = cold.conn.lock().expect("cold connection");
        let fixture = fixture.conn.lock().expect("private fixture connection");
        assert_eq!(schema(&fixture), schema(&cold));
        assert_eq!(data(&fixture), data(&cold));
        assert_eq!(
            fixture
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("version"),
            SQLITE_SCHEMA_VERSION
        );
        assert!(rows(&fixture, "PRAGMA foreign_key_check").is_empty());
    }

    #[test]
    fn fixture_instances_refresh_dynamic_settings_pragmas_and_owner_state() {
        let clock = Connection::open_in_memory().expect("clock connection");
        let before: i64 = clock
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .expect("before");
        let first = SqliteStore::fixture_in_memory().expect("first fixture");
        let second = SqliteStore::fixture_in_memory().expect("second fixture");
        let after: i64 = clock
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .expect("after");
        assert!(!Arc::ptr_eq(&first.conn, &second.conn));
        assert!(!Arc::ptr_eq(&first.token_activity, &second.token_activity));
        assert!(first.path.is_none() && second.path.is_none());
        assert!(first.reads.is_none() && second.reads.is_none());
        #[cfg(feature = "fixtures")]
        {
            assert!(!Arc::ptr_eq(&first.settings_reads, &second.settings_reads));
            first.note_settings_read(&["fixture.proof"]);
            assert_eq!(
                first.take_settings_reads(),
                vec![vec!["fixture.proof".to_owned()]]
            );
            assert!(second.take_settings_reads().is_empty());
        }
        let first = first.conn.lock().expect("first connection");
        let second = second.conn.lock().expect("second connection");
        let first_id = setting(&first, "instance.id");
        let second_id = setting(&second, "instance.id");
        uuid::Uuid::parse_str(&first_id.0).expect("fresh UUID");
        uuid::Uuid::parse_str(&second_id.0).expect("other fresh UUID");
        assert_ne!(first_id.0, second_id.0);
        for connection in [&*first, &*second] {
            for key in ["instance.id", "hdr_format_backfilled_v1"] {
                let (value, updated_at) = setting(connection, key);
                assert!((before..=after).contains(&updated_at));
                if key == "hdr_format_backfilled_v1" {
                    assert_eq!(value, "1");
                }
            }
            for (pragma, expected) in [
                ("foreign_keys", 1),
                ("busy_timeout", 5000),
                ("synchronous", 1),
                ("query_only", 0),
            ] {
                assert_eq!(
                    connection
                        .pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))
                        .expect("connection pragma"),
                    expected
                );
            }
            assert_eq!(
                connection
                    .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                    .expect("memory journal"),
                "memory"
            );
        }
        let template = TEMPLATE.lock().expect("immutable template");
        let template = template.as_ref().expect("initialized template");
        assert!(rows(
            template,
            "SELECT key FROM settings WHERE key IN ('instance.id','hdr_format_backfilled_v1')"
        )
        .is_empty());
        assert!(
            template.execute("DELETE FROM settings", []).is_err(),
            "template is read-only"
        );
    }

    #[test]
    fn fixture_mutation_rollback_delete_and_fts_are_private() {
        let first = SqliteStore::fixture_in_memory().expect("first fixture");
        let second = SqliteStore::fixture_in_memory().expect("second fixture");
        let first = first.conn.lock().expect("first connection");
        let second = second.conn.lock().expect("second connection");
        let pristine = data(&second);
        seed_item(&first, "FixtureAlpha");
        assert_eq!(
            rows(
                &first,
                "SELECT rowid FROM items_fts WHERE items_fts MATCH 'FixtureAlpha'"
            ),
            vec![vec![Value::Integer(1)]]
        );
        first
            .execute_batch("BEGIN; UPDATE items SET title='FixtureRollback' WHERE id=1; ROLLBACK;")
            .expect("real rollback");
        assert_eq!(
            rows(
                &first,
                "SELECT rowid FROM items_fts WHERE items_fts MATCH 'FixtureAlpha'"
            ),
            vec![vec![Value::Integer(1)]]
        );
        assert!(rows(
            &first,
            "SELECT rowid FROM items_fts WHERE items_fts MATCH 'FixtureRollback'"
        )
        .is_empty());
        first
            .execute("UPDATE items SET title='FixtureBeta' WHERE id=1", [])
            .expect("real update trigger");
        assert!(rows(
            &first,
            "SELECT rowid FROM items_fts WHERE items_fts MATCH 'FixtureAlpha'"
        )
        .is_empty());
        assert_eq!(
            rows(
                &first,
                "SELECT rowid FROM items_fts WHERE items_fts MATCH 'FixtureBeta'"
            ),
            vec![vec![Value::Integer(1)]]
        );
        first
            .execute("DELETE FROM items WHERE id=1", [])
            .expect("real delete trigger");
        assert!(rows(
            &first,
            "SELECT rowid FROM items_fts WHERE items_fts MATCH 'FixtureBeta'"
        )
        .is_empty());
        first
            .execute(
                "INSERT INTO settings(key,value) VALUES('fixture.private','changed')",
                [],
            )
            .expect("private setting");
        first
            .pragma_update(None, "foreign_keys", false)
            .expect("private pragma");
        assert_eq!(data(&second), pristine);
        assert_eq!(
            second
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                .expect("other pragma"),
            1
        );
        let third = SqliteStore::fixture_in_memory().expect("later pristine clone");
        assert_eq!(
            data(&third.conn.lock().expect("third connection")),
            pristine
        );
    }

    #[test]
    fn fixture_parallel_connections_keep_identical_primary_keys_private() {
        let results = std::thread::scope(|scope| {
            (0..8)
                .map(|index| {
                    scope.spawn(move || {
                        let fixture = SqliteStore::fixture_in_memory().expect("parallel fixture");
                        let connection = fixture.conn.lock().expect("private parallel connection");
                        let title = format!("FixtureParallel{index}");
                        seed_item(&connection, &title);
                        let item: String = connection
                            .query_row("SELECT title FROM items WHERE id=1", [], |row| row.get(0))
                            .expect("own row");
                        assert_eq!(item, title);
                        assert_eq!(
                            rows(&connection, "SELECT COUNT(*) FROM items"),
                            vec![vec![Value::Integer(1)]]
                        );
                        setting(&connection, "instance.id").0
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|worker| worker.join().expect("parallel worker"))
                .collect::<Vec<_>>()
        });
        assert_eq!(results.into_iter().collect::<HashSet<_>>().len(), 8);
    }

    #[test]
    fn cold_file_hdr_backfill_and_reopen_still_execute_the_real_path() {
        let root = tempfile::tempdir().expect("cold file root");
        let path = root.path().join("cold.sqlite");
        {
            let connection = Connection::open(&path).expect("raw cold file");
            // The full real migration chain runs here, without the HDR backfill.
            SqliteStore::migrate(&connection).expect("cold migrations");
            seed_item(&connection, "ColdBackfill");
            connection.execute_batch(r#"INSERT INTO files(id,item_id,path,size,mtime,probe_json)
                VALUES(1,1,'/fixture.mkv',100,1,'{"streams":[{"codec_type":"video","codec_name":"hevc","color_transfer":"smpte2084"}]}');"#).expect("pre-backfill probe row");
            assert!(rows(
                &connection,
                "SELECT key FROM settings WHERE key='hdr_format_backfilled_v1'"
            )
            .is_empty());
        }
        let instance_id = {
            let store = SqliteStore::open(&path).expect("real file initialization/backfill");
            let connection = store.conn.lock().expect("cold file connection");
            assert_eq!(
                rows(&connection, "SELECT hdr_format FROM files WHERE id=1"),
                vec![vec![Value::Text("HDR10".into())]]
            );
            assert_eq!(setting(&connection, "hdr_format_backfilled_v1").0, "1");
            let id = setting(&connection, "instance.id").0;
            connection
                .execute(
                    "UPDATE files SET hdr_format='retained-proof' WHERE id=1",
                    [],
                )
                .expect("proof flag prevents another pass");
            id
        };
        let reopened = SqliteStore::open(&path).expect("real cold reopen");
        let connection = reopened.conn.lock().expect("reopened file");
        assert_eq!(setting(&connection, "instance.id").0, instance_id);
        assert_eq!(setting(&connection, "hdr_format_backfilled_v1").0, "1");
        assert_eq!(
            rows(&connection, "SELECT hdr_format FROM files WHERE id=1"),
            vec![vec![Value::Text("retained-proof".into())]]
        );
    }
}
