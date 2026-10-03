//! The frozen rebuild is exercised before installing it in either backend.
//! These SQL-shape probes do not qualify a live mixed-version cluster.
use crate::store::{SqliteStore, MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA, SQLITE_SCHEMA_VERSION};
use rusqlite::{params, Connection};

const OWNER_TABLES: [&str; 7] = [
    "media_session_requests",
    "media_playback_pointers",
    "media_sessions",
    "media_session_preparations",
    "media_playback_desired",
    "media_session_producer_recovery",
    "library_channel_session_recipes",
];

fn current_database() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    SqliteStore::apply_migrations_for_test(&conn, SQLITE_SCHEMA_VERSION)
        .expect("current migrations");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enforce deployed child foreign keys during the candidate transaction");
    conn.execute_batch(include_str!(
        "../../tests/fixtures/session-principal-local.sql"
    ))
    .expect("populate all ownership and retention tables");
    conn
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .expect("columns")
        .query_map([], |row| row.get(1))
        .expect("read columns")
        .collect::<Result<_, _>>()
        .expect("decode columns")
}
fn snapshot(conn: &Connection, table: &str, cols: &[String]) -> Vec<String> {
    let projection = cols
        .iter()
        .map(|col| format!("'{col}', {col}"))
        .collect::<Vec<_>>()
        .join(", ");
    conn.prepare(&format!(
        "SELECT json_object({projection}) FROM {table} ORDER BY 1"
    ))
    .expect("snapshot")
    .query_map([], |row| row.get(0))
    .expect("read snapshot")
    .collect::<Result<_, _>>()
    .expect("decode snapshot")
}
fn rebuild(conn: &Connection) {
    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("begin rebuild");
    conn.execute_batch(MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA)
        .expect("rebuild all tables");
    conn.execute_batch("COMMIT").expect("commit rebuild");
}
fn insert_shared_request(
    conn: &Connection,
    owner: &str,
    grant: &str,
    viewer: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO media_session_requests
      (owner_key, principal_kind, user_id, share_grant_id, share_viewer_key,
       request_id, request_fingerprint, playback_id, state, claim_expires_at_ms,
       incarnation_id, owner_node_id, updated_at_ms)
      VALUES (?1, 'sharing', NULL, ?2, ?3, 'request', 'fingerprint', 'playback',
              'starting', 9000, 'shared-live', 'node', 10)",
        params![owner, grant, viewer],
    )
}

#[test]
fn sharing_principal_rebuild_preserves_every_old_column_and_retention_row() {
    for enforce_foreign_keys in [false, true] {
        let conn = current_database();
        conn.pragma_update(None, "foreign_keys", enforce_foreign_keys)
            .expect("exercise the SQLite runner and replicated FK settings");
        let tables = OWNER_TABLES.into_iter().chain([
            "job_leases",
            "media_session_terminal_acks",
            "sharing_relay_upstream",
            "sharing_delivery_grants",
        ]);
        let before = tables
            .map(|table| {
                let cols = columns(&conn, table);
                let rows = snapshot(&conn, table, &cols);
                (table, cols, rows)
            })
            .collect::<Vec<_>>();
        rebuild(&conn);
        let dangling: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .expect("child foreign key integrity");
        assert_eq!(dangling, 0);
        let leftovers: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('media_session_principal_relay_backup', 'media_session_principal_delivery_backup')", [], |r| r.get(0)).expect("temporary rebuild tables");
        assert_eq!(
            leftovers, 0,
            "the atomic migration leaves no authority backup tables"
        );
        for (table, cols, rows) in before {
            assert_eq!(
                snapshot(&conn, table, &cols),
                rows,
                "{table}: old data is retained"
            );
        }
        for table in OWNER_TABLES {
            let foreign_keys: i64 = conn
                .query_row(
                    &format!("SELECT count(*) FROM pragma_foreign_key_list('{table}')"),
                    [],
                    |r| r.get(0),
                )
                .expect("foreign keys");
            assert_eq!(
                foreign_keys, 0,
                "{table}: retained rows must remain FK-free"
            );
            let invalid: i64 = conn.query_row(&format!(
            "SELECT count(*) FROM {table} WHERE owner_key != 'local:' || CAST(user_id AS TEXT)
              OR principal_kind != 'local' OR share_grant_id IS NOT NULL OR share_viewer_key IS NOT NULL"
        ), [], |r| r.get(0)).expect("local backfill");
            assert_eq!(invalid, 0, "{table}: all projections backfilled");
        }
        conn.execute("DELETE FROM users WHERE id = 1", [])
            .expect("owner deletion");
        assert_eq!(conn.query_row("SELECT state || ':' || terminal_reason || ':' || lease_expires_at_ms FROM media_sessions WHERE incarnation_id='live'", [], |r| r.get::<_, String>(0)).expect("deleted owner fenced"), "ended:deleted:0");
        assert_eq!(
            conn.query_row(
                "SELECT expires_at_ms FROM job_leases WHERE resource='session:live'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("deleted owner lease fenced"),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT state FROM sharing_delivery_grants WHERE incarnation_id='live'",
                [],
                |r| r.get::<_, String>(0)
            )
            .expect("deleted owner delivery fenced"),
            "revoked"
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM media_playback_pointers WHERE owner_key='local:1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("deleted pointer"),
            0
        );
        conn.execute("INSERT INTO users(id,username,password_hash,is_admin,created_at) VALUES(1,'replacement','hash',0,2)", []).expect("reuse numeric ID");
        assert_eq!(conn.query_row("SELECT count(*) FROM media_sessions WHERE user_id=1 AND state IN ('starting','active')", [], |r| r.get::<_, i64>(0)).expect("old authority remains retired"), 0);
        assert_eq!(
            conn.query_row("SELECT count(*) FROM media_sessions", [], |r| r
                .get::<_, i64>(0))
                .expect("sessions"),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM media_session_terminal_acks",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("acks"),
            1
        );
    }
}

#[test]
fn sharing_principal_rebuild_fences_legacy_writes_and_rolls_back_atomically() {
    let conn = current_database();
    let old_insert = "INSERT INTO media_session_requests
      (user_id, request_id, request_fingerprint, playback_id, state, claim_expires_at_ms,
       incarnation_id, updated_at_ms) VALUES (1, 'legacy', 'fp', 'legacy', 'starting', 9000, 'legacy', 10)";
    conn.execute_batch("BEGIN IMMEDIATE").expect("begin");
    conn.execute_batch(MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA)
        .expect("candidate rebuild");
    let error = conn
        .execute(old_insert, [])
        .expect_err("old writer cannot omit owner_key");
    assert!(error
        .to_string()
        .contains("NOT NULL constraint failed: media_session_requests.owner_key"));
    conn.execute_batch("ROLLBACK")
        .expect("rollback complete rebuild");
    conn.execute(old_insert, [])
        .expect("original writer works after rollback");
    rebuild(&conn);
    // Before admitting remote rows, named-column legacy local reads still work.
    assert_eq!(
        conn.query_row(
            "SELECT user_id FROM media_sessions WHERE incarnation_id = 'live'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("legacy read"),
        1
    );
    let error = conn.execute(&format!("{old_insert} ON CONFLICT(user_id, request_id) DO UPDATE SET updated_at_ms = excluded.updated_at_ms"), [])
        .expect_err("old conflict target cannot address new principal PK");
    assert!(error
        .to_string()
        .contains("ON CONFLICT clause does not match"));
    assert_eq!(
        columns(&conn, "media_session_requests")
            .iter()
            .filter(|c| c.as_str() == "owner_key")
            .count(),
        1
    );
}

#[test]
fn sharing_principal_rebuild_isolates_viewer_keys_and_existing_fence_triggers() {
    let conn = current_database();
    rebuild(&conn);
    let grant = "00000000-0000-4000-a000-000000000001".to_owned();
    let viewer = "d".repeat(64);
    let owner = format!("share:{grant}:{viewer}");
    insert_shared_request(&conn, &owner, &grant, &viewer)
        .expect("sharing owner with same request id");
    assert!(insert_shared_request(&conn, "local:1", &grant, &viewer).is_err());
    assert!(insert_shared_request(
        &conn,
        &format!("share:{grant}:{}", "D".repeat(64)),
        &grant,
        &"D".repeat(64)
    )
    .is_err());
    assert!(insert_shared_request(
        &conn,
        &format!("share:{}:{viewer}", grant.to_uppercase()),
        &grant.to_uppercase(),
        &viewer
    )
    .is_err());
    assert!(
        conn.execute(
            "UPDATE media_session_requests SET user_id = 1 WHERE owner_key = ?1",
            [&owner]
        )
        .is_err(),
        "sharing projection cannot adopt a local user"
    );
    assert!(
        conn.execute(
            "UPDATE media_session_requests SET owner_key = 'local:01' WHERE owner_key = 'local:1'",
            []
        )
        .is_err(),
        "local owner key cannot use a decimal alias"
    );
    conn.execute(
        "INSERT INTO library_channel_session_recipes
       (owner_key, principal_kind, share_grant_id, share_viewer_key, request_id,
        incarnation_id, recipe_json, created_at_ms)
       VALUES (?1, 'sharing', ?2, ?3, 'request', 'shared-live', '{}', 10)",
        params![owner, grant, viewer],
    )
    .expect("shared recipe");
    conn.execute(
        "INSERT INTO media_playback_desired
       (owner_key, principal_kind, share_grant_id, share_viewer_key, playback_id,
        revision, digest, canonical_form, updated_at_ms)
       VALUES (?1, 'sharing', ?2, ?3, 'playback', 5, ?4, '{}', 10)",
        params![owner, grant, viewer, "a".repeat(64)],
    )
    .expect("shared desired");
    let pointer = "INSERT INTO media_playback_pointers
       (owner_key, principal_kind, share_grant_id, share_viewer_key, playback_id,
        current_incarnation_id, updated_at_ms, desired_revision)
       VALUES (?1, 'sharing', ?2, ?3, 'playback', 'shared-live', 10, ?4)";
    assert!(
        conn.execute(pointer, params![owner, grant, viewer, 1])
            .is_err(),
        "local revision cannot admit sharing pointer"
    );
    conn.execute(pointer, params![owner, grant, viewer, 5])
        .expect("correct shared revision");
    assert!(
        conn.execute(
            "UPDATE media_playback_pointers SET desired_revision = 2 WHERE owner_key = 'local:1'",
            []
        )
        .is_err(),
        "local update fence retained"
    );
    conn.execute(
        "UPDATE media_sessions SET owner_epoch = 3 WHERE incarnation_id = 'live'",
        [],
    )
    .expect("drain trigger returns IGNORE");
    assert_eq!(
        conn.query_row(
            "SELECT owner_epoch FROM media_sessions WHERE incarnation_id = 'live'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("epoch"),
        2
    );
    conn.execute(
        "DELETE FROM media_session_requests WHERE owner_key = ?1",
        [&owner],
    )
    .expect("delete shared request");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM library_channel_session_recipes WHERE owner_key = 'local:1'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("local recipe retained"),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM library_channel_session_recipes WHERE owner_key = ?1",
            [&owner],
            |r| r.get::<_, i64>(0)
        )
        .expect("shared recipe deleted"),
        0
    );
    conn.execute(
        "DELETE FROM media_session_requests WHERE owner_key = 'local:1'",
        [],
    )
    .expect("delete local request");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM library_channel_session_recipes",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("recipes"),
        0
    );
}

#[test]
fn sharing_principal_rebuild_owner_deletion_and_revocation_retire_only_matching_authority() {
    let conn = current_database();
    conn.execute_batch(MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA)
        .expect("candidate rebuild");
    let grant_a = "00000000-0000-4000-a000-000000000001";
    let grant_b = "00000000-0000-4000-a000-000000000002";
    conn.execute_batch(include_str!(
        "../../tests/fixtures/session-principal-sharing.sql"
    ))
    .expect("two grant fixture");
    conn.execute("DELETE FROM users WHERE id=1", [])
        .expect("delete local owner");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM media_sessions WHERE principal_kind='sharing' AND state='active'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("sharing unaffected"),
        2
    );
    conn.execute(
        "UPDATE sharing_exports SET state='revoked' WHERE id=?1",
        [&grant_a],
    )
    .expect("revoke source grant");
    assert_eq!(conn.query_row("SELECT state || ':' || terminal_reason || ':' || lease_expires_at_ms FROM media_sessions WHERE incarnation_id=?1", [&grant_a], |r| r.get::<_,String>(0)).expect("revoked source fenced"), "ended:revoked:0");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM media_sessions WHERE principal_kind='sharing' AND state='active'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("other grant unaffected"),
        1
    );
    conn.execute("DELETE FROM sharing_exports WHERE id=?1", [&grant_b])
        .expect("delete active grant");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM media_sessions WHERE principal_kind='sharing'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("terminal sharing rows retained"),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM media_sessions WHERE principal_kind='sharing' AND state!='ended'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("all deleted authority retired"),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM media_session_terminal_acks",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("unrelated terminal ack retained"),
        1
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM media_playback_pointers", [], |r| r
            .get::<_, i64>(0))
            .expect("pointers retired"),
        0
    );
}

#[test]
fn sharing_principal_rebuild_refuses_zero_and_negative_local_owners_in_every_family() {
    let conn = current_database();
    rebuild(&conn);
    for table in OWNER_TABLES {
        let present: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE principal_kind='local'"),
                [],
                |row| row.get(0),
            )
            .expect("populated owner fixture");
        assert!(
            present > 0,
            "{table}: test exercises a real populated ownership row"
        );
        for invalid_id in [0_i64, -1] {
            let result = conn.execute(
                &format!(
                    "UPDATE {table} SET user_id=?1, owner_key=?2 WHERE principal_kind='local'"
                ),
                params![invalid_id, format!("local:{invalid_id}")],
            );
            let error = result.expect_err("invalid local owner must fail in the database");
            assert!(
                error.to_string().contains("CHECK constraint failed"),
                "{table}: unexpected refusal {error}"
            );
        }
    }
}
