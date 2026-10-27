//! Independently recognized progressive translation extension.
use crate::error::StoreError;
use std::sync::LazyLock;

pub(crate) const SCHEMA_V1: &str = include_str!("subtitle_translation.sql");
pub(crate) const RETARGET_MIGRATION: &str = include_str!("subtitle_translation_retarget.sql");
pub(crate) const CONTINUITY_MIGRATION: &str = include_str!("subtitle_translation_continuity.sql");
pub(crate) const SCHEMA_PRE_CONTINUITY: &str = concat!(
    include_str!("subtitle_translation.sql"),
    "\n-- next statement\n",
    include_str!("subtitle_translation_retarget.sql")
);
pub(crate) const SCHEMA: &str = concat!(
    include_str!("subtitle_translation.sql"),
    "\n-- next statement\n",
    include_str!("subtitle_translation_retarget.sql"),
    "\n-- next statement\n",
    include_str!("subtitle_translation_continuity.sql")
);
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'subtitle_translation_*' AND sql IS NOT NULL ORDER BY name";
#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn retarget_statements() -> Vec<String> {
    RETARGET_MIGRATION
        .split("-- next statement")
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .map(str::to_owned)
        .collect()
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn continuity_statements() -> Vec<String> {
    CONTINUITY_MIGRATION
        .split("-- next statement")
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .map(str::to_owned)
        .collect()
}
fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}
static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("translation schema fixture");
    conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("translation schema dependencies");
    conn.execute_batch(SCHEMA).expect("translation schema");
    rows(&conn).expect("translation schema shape")
});
static LEGACY_SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("legacy translation schema fixture");
    conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("legacy translation dependencies");
    conn.execute_batch(SCHEMA_V1)
        .expect("legacy translation schema");
    rows(&conn).expect("legacy translation shape")
});
static PRE_CONTINUITY_SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("pre-continuity fixture");
    conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("dependencies");
    conn.execute_batch(SCHEMA_PRE_CONTINUITY)
        .expect("pre-continuity schema");
    rows(&conn).expect("pre-continuity shape")
});
/// Exact shapes: original=0, producer-retarget=1, claim-continuity=2. Partial/unknown shapes are refused.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| {
                (
                    name.clone(),
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    if shape.is_empty() {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(2))
    } else if canonical(shape) == canonical(&PRE_CONTINUITY_SHAPE) {
        Ok(Some(1))
    } else if canonical(shape) == canonical(&LEGACY_SHAPE) {
        Ok(Some(0))
    } else {
        Err(StoreError::Migration(
            "progressive translation schema is partial or incompatible".into(),
        ))
    }
}
pub(crate) fn retarget_already_applied(conn: &rusqlite::Connection) -> Result<bool, StoreError> {
    let generation = version(&rows(conn)?)?;
    let marker: i64 = conn.query_row(
        "SELECT count(*) FROM subtitle_translation_schema WHERE singleton=1 AND version=1",
        [],
        |row| row.get(0),
    )?;
    match (generation, marker) {
        (Some(1 | 2), 1) => Ok(true),
        (Some(0), 1) => Ok(false),
        _ => Err(StoreError::Migration(
            "unrecognized progressive translation upgrade source".into(),
        )),
    }
}
pub(crate) fn continuity_already_applied(conn: &rusqlite::Connection) -> Result<bool, StoreError> {
    let marker: i64 = conn.query_row(
        "SELECT count(*) FROM subtitle_translation_schema WHERE singleton=1 AND version=1",
        [],
        |row| row.get(0),
    )?;
    if marker != 1 {
        return Err(StoreError::Migration(
            "invalid continuity upgrade marker".into(),
        ));
    }
    match version(&rows(conn)?)? {
        Some(2) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(StoreError::Migration(
            "unrecognized continuity upgrade source".into(),
        )),
    }
}
pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(2) {
        return Err(StoreError::Migration(
            "progressive translation schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM subtitle_translation_schema WHERE singleton=1 AND version=1",
        [],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(StoreError::Migration(
            "progressive translation marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn main_v115_handoff_rows_survive_appended_subtitle_migrations() {
        use crate::store::sqlite::SqliteStore;
        let directory = tempfile::tempdir().expect("owned migration fixture");
        let path = directory.path().join("store.db");
        let conn = rusqlite::Connection::open(&path).expect("historical store");
        SqliteStore::apply_migrations_for_test(&conn, 115).expect("published main schema");
        conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'migration-owner','hash',1); INSERT INTO playback_handoffs(id,code_hash,user_id,file_id,source_token_hash,created_at,expires_at) VALUES('retained-handoff','retained-code',1,17,'source-token',1,9223372036854775806); INSERT INTO playback_handoffs_tokens(token_hash,handoff_id,user_id,file_id,source_token_hash,created_at,expires_at) VALUES('retained-token','retained-handoff',1,17,'source-token',1,9223372036854775806);").expect("published handoff authority");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("published version"),
            115
        );
        drop(conn);
        let store = SqliteStore::open(&path).expect("upgrade the actual main database");
        let conn = rusqlite::Connection::open(&path).expect("read upgraded store");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("upgraded version"),
            119
        );
        assert_eq!(
            conn.query_row(
                "SELECT code_hash FROM playback_handoffs WHERE id='retained-handoff'",
                [],
                |row| row.get::<_, String>(0)
            )
            .expect("retained code"),
            "retained-code"
        );
        assert_eq!(
            conn.query_row(
                "SELECT handoff_id FROM playback_handoffs_tokens WHERE token_hash='retained-token'",
                [],
                |row| row.get::<_, String>(0)
            )
            .expect("retained token"),
            "retained-handoff"
        );
        super::verify_sqlite(&conn).expect("complete progressive schema");
        crate::store::subtitle_text_translation_schema::verify_sqlite(&conn)
            .expect("text schema appended");
        crate::store::shared_caption_schema::verify_sqlite(&conn).expect("shared schema appended");
        crate::store::playback_handoffs::verify_sqlite(&conn).expect("main schema preserved");
        drop(store);
    }
    #[test]
    fn translation_schema_install_is_atomic_and_refuses_partial_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("dependencies");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch("BEGIN").expect("transaction");
        conn.execute_batch(SCHEMA).expect("install");
        verify_sqlite(&conn).expect("complete");
        conn.execute_batch("ROLLBACK").expect("rollback");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch(SCHEMA).expect("replay");
        verify_sqlite(&conn).expect("replayed");
        conn.execute_batch("DROP TABLE subtitle_translation_read_leases")
            .expect("corrupt fixture");
        assert!(verify_sqlite(&conn).is_err());
    }
    #[test]
    fn retarget_upgrade_recognizes_only_exact_legacy_shape_and_is_transactional() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("dependencies");
        conn.execute_batch(SCHEMA_V1).expect("published v1");
        assert_eq!(
            version(&rows(&conn).expect("legacy")).expect("recognized"),
            Some(0)
        );
        conn.execute_batch("BEGIN").expect("upgrade transaction");
        conn.execute_batch(RETARGET_MIGRATION)
            .expect("same statements used by replicated backend");
        assert_eq!(
            version(&rows(&conn).expect("retarget shape")).expect("recognized retarget"),
            Some(1)
        );
        conn.execute_batch(CONTINUITY_MIGRATION)
            .expect("continuity upgrade");
        verify_sqlite(&conn).expect("current shape");
        conn.execute_batch("ROLLBACK").expect("crash before commit");
        assert_eq!(
            version(&rows(&conn).expect("rolled back")).expect("legacy retained"),
            Some(0)
        );
        conn.execute_batch(RETARGET_MIGRATION)
            .expect("committed upgrade");
        conn.execute_batch(CONTINUITY_MIGRATION)
            .expect("committed continuity upgrade");
        verify_sqlite(&conn).expect("upgraded");
        conn.execute_batch("DROP TRIGGER subtitle_translation_mutate")
            .expect("partial shape");
        assert!(version(&rows(&conn).expect("partial")).is_err());
    }
    #[test]
    fn continuity_upgrade_preserves_pre_continuity_shape_until_atomic_commit() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("dependencies");
        conn.execute_batch(SCHEMA_PRE_CONTINUITY)
            .expect("published retarget shape");
        assert_eq!(
            version(&rows(&conn).expect("shape")).expect("recognized"),
            Some(1)
        );
        assert!(!continuity_already_applied(&conn).expect("pending"));
        conn.execute_batch("BEGIN").expect("transaction");
        conn.execute_batch(CONTINUITY_MIGRATION).expect("upgrade");
        verify_sqlite(&conn).expect("complete new shape");
        conn.execute_batch("ROLLBACK").expect("crash rollback");
        assert_eq!(
            version(&rows(&conn).expect("shape")).expect("recognized old"),
            Some(1)
        );
        conn.execute_batch(CONTINUITY_MIGRATION)
            .expect("commit upgrade");
        assert!(continuity_already_applied(&conn).expect("exact replay"));
        conn.execute_batch("DROP TRIGGER subtitle_translation_capture_continuity")
            .expect("partial fixture");
        assert!(version(&rows(&conn).expect("shape")).is_err());
    }
}
