//! Durable record of each encoded-output publication command.
//!
//! Encoded publication reuses the copy-output `publish_copy_output` command,
//! whose trigger settles the job and its waiters and then deletes the command
//! row: commands are transient. A worker that lost its publication await
//! must still read back the exact original command
//! ([`super::background_jobs::BackgroundJobStore::encoded_output_publication`]),
//! so this extension copies every positive encoded publication command into
//! its own table, in the same statement. The first positive command for a
//! claim is kept (an exact reconcile never replaces it), and a pruned job
//! takes its records with it.
//!
//! Like the adaptive source scope, the catalogue is installed after the
//! ordinal migrations on both backends, all objects or none.
use crate::error::StoreError;

pub(crate) const CATALOG_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'encoded_output_publications*' AND sql IS NOT NULL ORDER BY name";

pub(crate) fn objects() -> Vec<(String, String)> {
    let mut objects = include_str!("encoded_output_publications.sql")
        .split("-- next statement")
        .map(|sql| sql.trim().trim_end_matches(';'))
        .map(|sql| {
            let name = sql.split_whitespace().nth(2).expect("static SQL object");
            (name.to_owned(), sql.to_owned())
        })
        .collect::<Vec<_>>();
    objects.sort_by(|left, right| left.0.cmp(&right.0));
    objects
}

pub(crate) fn catalog_is_exact(rows: &[(String, String)]) -> bool {
    rows == objects()
}

pub(crate) fn install_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    let rows = {
        let mut stmt = tx.prepare(CATALOG_SQL)?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if rows.is_empty() {
        for (_, sql) in objects() {
            tx.execute_batch(&sql)?;
        }
    } else if !catalog_is_exact(&rows) {
        return Err(StoreError::Migration(
            "encoded output publication catalogue is partial or incompatible".into(),
        ));
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(conn: &rusqlite::Connection) -> Vec<(String, String)> {
        let mut stmt = conn.prepare(CATALOG_SQL).expect("catalogue");
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("catalogue rows");
        rows.collect::<Result<Vec<_>, _>>().expect("catalogue rows")
    }

    #[test]
    fn the_publication_catalogue_installs_once_and_refuses_a_partial_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("database");
        conn.execute_batch(
            "CREATE TABLE background_jobs(id TEXT PRIMARY KEY, kind TEXT);
             CREATE TABLE background_job_commands(id TEXT PRIMARY KEY, operation TEXT,
                 request_json TEXT, result_json TEXT);",
        )
        .expect("subjects");
        install_sqlite(&conn).expect("fresh install");
        assert_eq!(objects().len(), 4);
        assert!(catalog_is_exact(&catalog(&conn)));
        install_sqlite(&conn).expect("an exact catalogue is left alone");
        assert!(catalog_is_exact(&catalog(&conn)));
        conn.execute_batch("DROP TRIGGER encoded_output_publications_retire")
            .expect("partial fixture");
        assert!(matches!(
            install_sqlite(&conn),
            Err(StoreError::Migration(_))
        ));
    }

    #[test]
    fn only_positive_encoded_commands_are_kept_and_the_first_one_wins() {
        let conn = rusqlite::Connection::open_in_memory().expect("database");
        conn.execute_batch(
            "CREATE TABLE background_jobs(id TEXT PRIMARY KEY, kind TEXT);
             CREATE TABLE background_job_commands(id TEXT PRIMARY KEY, operation TEXT,
                 request_json TEXT, result_json TEXT);
             -- The transient command contract the copy-output trigger keeps.
             CREATE TRIGGER transient AFTER INSERT ON background_job_commands
             BEGIN DELETE FROM background_job_commands WHERE id=NEW.id; END;
             INSERT INTO background_jobs VALUES('encoded','encoded_output_prepare'),
                 ('copy','copy_output_prepare');",
        )
        .expect("subjects");
        install_sqlite(&conn).expect("install");
        let publish = |claim: &str, job: &str, outcome: &str, request: &str| {
            conn.execute(
                "INSERT INTO background_job_commands VALUES(?1,'publish_copy_output',?2,
                     json_object('outcome',?3,'job_id',?4,'result_ref','artifact'))",
                rusqlite::params![claim, request, outcome, job],
            )
            .expect("command");
        };
        publish("lost", "encoded", "lost_ownership", "{\"n\":0}");
        publish("copy", "copy", "published", "{\"n\":1}");
        publish("claim", "encoded", "published", "{\"n\":2}");
        publish("claim", "encoded", "already_published", "{\"n\":3}");
        let kept: Vec<(String, String, String)> = conn
            .prepare("SELECT claim_id, job_id, request_json FROM encoded_output_publications")
            .expect("records")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("records")
            .collect::<Result<_, _>>()
            .expect("records");
        assert_eq!(
            kept,
            [("claim".into(), "encoded".into(), "{\"n\":2}".into())]
        );
        let commands: i64 = conn
            .query_row("SELECT count(*) FROM background_job_commands", [], |row| {
                row.get(0)
            })
            .expect("commands");
        assert_eq!(commands, 0, "the command rows themselves stay transient");
        conn.execute("DELETE FROM background_jobs WHERE id='encoded'", [])
            .expect("prune");
        let left: i64 = conn
            .query_row(
                "SELECT count(*) FROM encoded_output_publications",
                [],
                |row| row.get(0),
            )
            .expect("records");
        assert_eq!(left, 0, "a pruned job takes its publication record with it");
    }
}
