use super::*;

fn request_fixture() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("fixture");
    conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE users(id INTEGER PRIMARY KEY); INSERT INTO users VALUES(7); CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY,schema_version INTEGER NOT NULL); INSERT INTO cluster_meta VALUES(1,81)").expect("base");
    conn.execute_batch(hiqlite::Error::SOURCE_LAYOUT_GUARD_DDL)
        .expect("dispatch guard");
    type Object = (String, String, String, Vec<Option<String>>);
    let objects: Vec<Object> =
        serde_json::from_str(include_str!("../sharing_source_legacy_schema.json"))
            .expect("frozen predecessor");
    let definition = objects
        .iter()
        .find(|(kind, name, _, _)| kind == "table" && name == "media_session_requests")
        .expect("request definition")
        .3
        .iter()
        .flatten()
        .next()
        .expect("request SQL");
    conn.execute_batch(definition).expect("legacy requests");
    conn
}

fn migrate_requests(conn: &mut rusqlite::Connection) {
    let txn = conn.transaction().expect("migration");
    let mut started = false;
    for sql in super::super::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA.split("-- next statement") {
        let sql = sql.trim();
        if sql.starts_with("CREATE TABLE media_session_requests_principal_new") {
            started = true;
        }
        if started {
            txn.execute_batch(sql)
                .expect("real frozen request migration");
        }
        if sql.starts_with("ALTER TABLE media_session_requests_principal_new") {
            break;
        }
    }
    txn.execute("UPDATE cluster_meta SET schema_version=82", [])
        .expect("marker");
    txn.commit().expect("migration commit");
}

fn execute_statement(
    conn: &rusqlite::Connection,
    statement: &(String, hiqlite::Params),
) -> rusqlite::Result<usize> {
    let bindings = statement.1.iter().map(|value| match value {
        Param::Integer(value) => rusqlite::types::Value::Integer(*value),
        Param::Text(value) => rusqlite::types::Value::Text(value.clone()),
        Param::Null => rusqlite::types::Value::Null,
        _ => panic!("unsupported fixture binding"),
    });
    conn.execute(&statement.0, rusqlite::params_from_iter(bindings))
}

fn request_template(
    request: &str,
    incarnation: &str,
) -> Vec<(std::borrow::Cow<'static, str>, hiqlite::Params)> {
    let layout = LocalSessionSql {
        rebuilt: false,
        template: true,
    };
    vec![(format!("INSERT INTO media_session_requests(user_id,request_id,request_fingerprint,playback_id,state,claim_expires_at_ms,incarnation_id,updated_at_ms{}) SELECT $1,$2,$3,$4,'starting',10000,$5,$6{} WHERE 1=1{} ON CONFLICT({},request_id) DO NOTHING",layout.insert_columns(),layout.insert_values(1),layout.existing_user(1),layout.column()).into(),params!(7,request,"fingerprint","playback",incarnation,1000))]
}

#[test]
fn local_request_proposal_prepared_before_live_migration_preserves_active_claims() {
    let mut conn = request_fixture();
    let retained = request_template("retained", "old-incarnation");
    for statement in
        guarded_session_statements(&retained, SessionLayout::Legacy).expect("legacy proposal")
    {
        execute_statement(&conn, &statement).expect("retained claim");
    }
    // This exact application proposal was constructed while the old schema
    // was visible, but has not been submitted when migration commits.
    let pending = request_template("pending", "new-incarnation");
    let stale = guarded_session_statements(&pending, SessionLayout::Legacy).expect("old arm");
    migrate_requests(&mut conn);
    let txn = conn.transaction().expect("stale proposal");
    let error = execute_statement(&txn, &stale[0]).expect_err("first guard must refuse");
    assert!(error.to_string().contains("plurx_source_layout_guard_v1"));
    assert_eq!(
        txn.query_row(
            "SELECT count(*) FROM media_session_requests WHERE request_id='pending'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .expect("no body execution"),
        0
    );
    txn.rollback().expect("confirmed rollback");
    let regenerated =
        guarded_session_statements(&pending, SessionLayout::Principal).expect("installed arm");
    let txn = conn.transaction().expect("regenerated proposal");
    for statement in &regenerated {
        execute_statement(&txn, statement).expect("compatible write");
    }
    txn.commit().expect("commit");
    let rows: i64 = conn.query_row("SELECT count(*) FROM media_session_requests WHERE owner_key='local:7' AND principal_kind='local' AND state='starting' AND claim_expires_at_ms=10000",[],|row|row.get(0)).expect("live claims");
    assert_eq!(rows, 2);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sharing_source_dispatch_guard",
            [],
            |row| row.get::<_, i64>(0)
        )
        .expect("guard remains inert"),
        0
    );
}

#[test]
fn shared_request_read_after_migration_uses_principal_projection_instead_of_stale_local_projection()
{
    let mut conn = request_fixture();
    migrate_requests(&mut conn);
    let grant = "00000000-0000-4000-a000-000000000001";
    let viewer = "a".repeat(64);
    let owner = format!("share:{grant}:{viewer}");
    conn.execute("INSERT INTO media_session_requests(owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,state,claim_expires_at_ms,incarnation_id,updated_at_ms) VALUES(?1,'sharing',NULL,?2,?3,'shared','fingerprint','playback','starting',10000,'shared-incarnation',1000)",rusqlite::params![owner,grant,viewer]).expect("Shared claim");
    let template = "SELECT request_fingerprint,playback_id,state,incarnation_id,owner_node_id,claim_expires_at_ms,__PLURX_LOCAL_PROJECTION__ FROM media_session_requests WHERE request_id='shared'";
    let read = |layout| {
        conn.query_row(
            &resolve_session_sql(template, layout).expect("resolved read"),
            [],
            |row| Ok(decode_request(&mut Row::Borrowed(row))),
        )
        .expect("read")
    };
    // The bridge must buffer and discard this error when its post-read
    // marker proves82, rather than exposing a false Local decoder fault.
    assert!(read(SessionLayout::Legacy).is_err());
    let current = read(SessionLayout::Principal).expect("actual principal");
    assert_eq!(
        current.principal,
        crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str(grant).expect("grant"),
            &viewer
        )
        .expect("Shared principal")
    );
}

#[test]
fn guarded_proposals_preserve_named_and_indexed_statement_output_links() {
    let templates = vec![
        ("SELECT $1 AS result".into(), params!(7)),
        (
            "SELECT $1,$2".into(),
            vec![
                Param::StmtOutputNamed(0, "result".into()),
                Param::StmtOutputIndexed(0, 0),
            ],
        ),
    ];
    let guarded =
        guarded_session_statements(&templates, SessionLayout::Principal).expect("proposal");
    assert_eq!(
        guarded[2].1,
        vec![
            Param::StmtOutputNamed(1, "result".into()),
            Param::StmtOutputIndexed(1, 0)
        ]
    );
    assert_eq!(
        templates[1].1,
        vec![
            Param::StmtOutputNamed(0, "result".into()),
            Param::StmtOutputIndexed(0, 0)
        ]
    );
    for invalid in [
        "SELECT __PLURX_LOCAL_EQUALS_0__",
        "SELECT __PLURX_LOCAL_LIVE_USER_arbitrary__",
        "SELECT __PLURX_UNKNOWN__",
    ] {
        assert!(resolve_session_sql(invalid, SessionLayout::Legacy).is_err());
        assert!(resolve_session_sql(invalid, SessionLayout::Principal).is_err());
    }
}
