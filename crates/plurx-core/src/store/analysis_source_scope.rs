//! Analysis reserves only the storage domain of the root that holds its file.
//!
//! `analysis_required_resources` used to charge a fragment-index or
//! skip-marker request one `source_io:<domain>` row for every root of its
//! library, so one per-file analysis held the maintenance reader of every disk
//! the library spans. The view is replaced under its own name with a clause
//! that keeps only the root(s) containing the file's path, falling back to
//! every root when none does. The triggers that read it do so by name and need
//! no change. SQLite installs it as one migration step; hiqlite installs it as
//! an idempotent, shape-verified extension whose marker an older binary never
//! reads.
use std::sync::LazyLock;

use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("analysis_source_scope.sql");
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name IN \
    ('analysis_required_resources','analysis_source_scope_schema') \
    AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM analysis_source_scope_schema WHERE singleton=1 AND version=1";
#[cfg(test)]
const MARKER_TABLE: &str = "analysis_source_scope_schema";
const VIEW: &str = "analysis_required_resources";

// The first statement of the hiqlite install transaction: the exact original
// view and no marker must still be committed, so a concurrent winner makes the
// transaction fail before it drops anything.
#[cfg(feature = "hiqlite-store")]
pub(crate) const GUARD: &str = "SELECT CASE WHEN \
    NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='analysis_source_scope_schema') \
    AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='analysis_required_resources' AND sql=$1) \
    THEN 1 ELSE json('analysis source scope predecessor changed') END";

#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
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

/// The original view exactly as the viewer-analysis schema stores it, then the
/// installed extension. A view is not resolved against its tables until it is
/// read, so an empty catalogue measures SQLite's stored text.
type Shape = Vec<(String, String)>;
static SHAPES: LazyLock<(Shape, Shape)> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("analysis source scope fixture");
    let original = super::background_jobs::VIEWER_ANALYSIS_SCHEMA
        .split("-- next statement")
        .find(|sql| sql.contains(&format!("CREATE VIEW IF NOT EXISTS {VIEW} AS")))
        .expect("original analysis resource view");
    conn.execute_batch(original)
        .expect("original analysis resources");
    let before = rows(&conn).expect("original analysis resource shape");
    conn.execute_batch(SCHEMA).expect("analysis source scope");
    (before, rows(&conn).expect("analysis source scope shape"))
});

fn canonical(rows: &[(String, String)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(name, sql)| {
            (
                name.clone(),
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect()
}

/// `None` for the exact original view without a marker, `Some(1)` for the
/// exact extension; anything else is refused rather than repaired.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    if canonical(shape) == canonical(&SHAPES.0) {
        Ok(None)
    } else if super::source_leases::accepts_extension_shape(shape, &SHAPES.1) {
        // The source-lease expand replaces this view with its root-keyed
        // projection after the scope is installed. Recognize only the exact
        // extension, or that projection as the expand or contract left it.
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "analysis source scope schema is partial or incompatible".into(),
        ))
    }
}

/// The original view's stored SQL, which the hiqlite guard requires.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn predecessor_view() -> String {
    SHAPES
        .0
        .iter()
        .find(|(name, _)| name == VIEW)
        .expect("original analysis resource view")
        .1
        .clone()
}

// Called under the SQLite migration transaction, before any DDL.
pub(crate) fn verify_predecessor(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)?.is_some() {
        return Err(StoreError::Migration(
            "analysis source scope migration requires the exact original view".into(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "analysis source scope schema layout mismatch".into(),
        ));
    }
    let count: i64 = conn.query_row(MARKER_SQL, [], |row| row.get(0))?;
    if count != 1 {
        return Err(StoreError::Migration(
            "analysis source scope schema marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;

    /// The installed view is the original one with the root clause added to
    /// its WHERE; nothing else about it changed.
    #[test]
    fn only_the_root_clause_differs_from_the_original_view() {
        let view = |shape: &[(String, String)]| {
            shape
                .iter()
                .find(|(name, _)| name == VIEW)
                .expect("view")
                .1
                .clone()
        };
        let before = view(&SHAPES.0);
        let after = view(&SHAPES.1);
        let clause = after
            .find("\n-- A root contains the file")
            .expect("root clause");
        let original_body = before
            .split_once(&format!("CREATE VIEW {VIEW} AS"))
            .expect("original body")
            .1
            .trim_end_matches(';');
        assert!(
            after[..clause].ends_with(original_body.trim_end()),
            "the original body precedes the clause unchanged"
        );
    }

    fn fixture() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 136).expect("deployed v136");
        conn.execute_batch(
            "INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/media/a/\",\"/media/b\"]');
             INSERT INTO background_storage_domains VALUES(1,'/media/a/','disk-a'),(1,'/media/b','disk-b');
             INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','A','A'),(2,1,'movie','B','B'),(3,1,'movie','C','C'),(4,1,'movie','D','D');
             INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/media/a/x.mkv',10,1),(2,2,'/media/b/y.mkv',10,1),(3,3,'/elsewhere/z.mkv',10,1),(4,4,'/media/bb/w.mkv',10,1);",
        )
        .expect("two-root library");
        for file in 1..=4 {
            conn.execute("INSERT INTO analysis_requests(request_id,file_id,source_size,source_mtime,component,pipeline_version,video_identity,requested_generation,priority,trigger,state,fence,attempts,not_before_ms,updated_at_ms,cancel_requested,force_rebuild,target_node_id,created_at_ms) VALUES(?1,?2,10,1,'fragment_index',?3,'','generation','normal','background','queued',0,0,0,1000,0,0,'',1000)", rusqlite::params![format!("r{file}"), file, "b".repeat(64)]).expect("analysis request");
        }
        conn
    }

    fn keys(conn: &rusqlite::Connection, request: &str) -> Vec<String> {
        conn.prepare("SELECT resource_key FROM analysis_required_resources WHERE request_id=?1 ORDER BY resource_key")
            .expect("resource query")
            .query_map([request], |row| row.get::<_, String>(0))
            .expect("resource rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete resources")
    }

    #[test]
    fn upgrade_narrows_each_request_to_its_root_and_keeps_every_root_without_one() {
        let conn = fixture();
        let both = ["source_io:disk-a", "source_io:disk-b"];
        for request in ["r1", "r2", "r3", "r4"] {
            assert_eq!(keys(&conn, request), both, "the original view: {request}");
        }
        SqliteStore::apply_next_migration_for_test(&conn).expect("v137");
        SqliteStore::apply_next_migration_for_test(&conn).expect("v138");
        verify_sqlite(&conn).expect("exact extension and marker");
        assert_eq!(
            keys(&conn, "r1"),
            ["source_io:disk-a"],
            "trailing-slash root"
        );
        assert_eq!(keys(&conn, "r2"), ["source_io:disk-b"]);
        assert_eq!(
            keys(&conn, "r3"),
            both,
            "a path under no root keeps every root"
        );
        assert_eq!(
            keys(&conn, "r4"),
            both,
            "a sibling prefix is not under the root"
        );
        assert!(
            verify_predecessor(&conn).is_err(),
            "a stale installer cannot replace the extension"
        );
    }

    #[test]
    fn upgrade_refuses_a_drifted_view_without_advancing_the_ordinal() {
        let conn = fixture();
        SqliteStore::apply_next_migration_for_test(&conn).expect("v137");
        conn.execute_batch("DROP VIEW analysis_required_resources; CREATE VIEW analysis_required_resources AS SELECT request_id,'source_io' AS resource_key FROM analysis_requests;").expect("drift");
        let before = rows(&conn).expect("drifted shape");
        assert!(SqliteStore::apply_next_migration_for_test(&conn).is_err());
        assert_eq!(rows(&conn).expect("refused shape"), before);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("ordinal"),
            137
        );
    }

    #[test]
    fn a_fresh_catalogue_is_the_exact_extension_and_partial_shapes_are_refused() {
        assert_eq!(version(&SHAPES.0).expect("original"), None);
        assert_eq!(version(&SHAPES.1).expect("extension"), Some(1));
        let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
        SqliteStore::migrate(&conn).expect("migrate");
        verify_sqlite(&conn).expect("fresh catalogue carries the extension");
        let mut marker_only = SHAPES.0.clone();
        marker_only.extend(
            SHAPES
                .1
                .iter()
                .filter(|(name, _)| name == MARKER_TABLE)
                .cloned(),
        );
        assert!(
            version(&marker_only).is_err(),
            "a marker beside the old view"
        );
        let view_only = SHAPES
            .1
            .iter()
            .filter(|(name, _)| name == VIEW)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            version(&view_only).is_err(),
            "a new view without its marker"
        );
        assert!(version(&[]).is_err(), "no view at all");
    }
}
