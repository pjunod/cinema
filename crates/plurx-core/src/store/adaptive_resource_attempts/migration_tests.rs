//! Synthetic SQL audit-history fixtures, never physical-close proof or API.
use super::*;

fn evaluate(conn: &rusqlite::Connection, predicate: &str) -> bool {
    conn.query_row(&format!("SELECT ({predicate})"), [], |row| row.get(0))
        .expect("exact schema predicate")
}

#[tokio::test]
async fn expired_unknown_attempt_refuses_source_rebuild_and_preserves_history() {
    let (store, claim) = fixture().await;
    store
        .claim_adaptive_attempt(&claim)
        .await
        .expect("committed obligation");
    store
        .with_conn(|conn| {
            assert!(
                !evaluate(conn, &schema::migration_drained_guard()),
                "expired starting route never proves physical closure"
            );
            let transaction = conn.unchecked_transaction()?;
            assert!(transaction
                .execute(&schema::assertion(&schema::migration_drained_guard()), [])
                .is_err());
            transaction.rollback()?;
            assert_eq!(
                conn.query_row("SELECT count(*) FROM media_resource_attempts", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                conn.query_row("SELECT count(*) FROM media_resource_operations", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                1
            );
            Ok(())
        })
        .await
        .expect("migration refusal");
}

#[tokio::test]
async fn drained_source_rebind_preserves_synthetic_terminal_audit_bytes_and_operations() {
    let (store, claim) = fixture().await;
    store
        .claim_adaptive_attempt(&claim)
        .await
        .expect("actual coordination");
    store.with_conn(|conn| {
        // No worker or resource ever exists in this fixture. Directly seed a
        // terminal audit fact after detaching guards in THIS disposable DB;
        // this is not a physical receipt and introduces no production writer.
        let tx=conn.unchecked_transaction()?;
        for (name,_) in schema::trigger_objects(schema::Layout::Legacy) { tx.execute(&format!("DROP TRIGGER {name}"),[])?; }
        tx.execute("UPDATE media_resource_attempts SET phase='closed'",[])?;
        tx.execute("UPDATE media_session_requests SET state='failed'",[])?;
        tx.execute("UPDATE media_sessions SET state='ended',terminal_reason='replaced',lease_expires_at_ms=1",[])?;
        tx.execute("UPDATE job_leases SET expires_at_ms=1",[])?;
        for (_,sql) in schema::trigger_objects(schema::Layout::Legacy) {tx.execute(&sql,[])?;}
        tx.commit()?;
        assert!(evaluate(conn,&schema::migration_drained_guard()));
        let before:(String,String,String)=conn.query_row("SELECT binding_json,key_json,payload_digest FROM media_resource_requests r JOIN media_resource_attempts a USING(owner_key,request_id,request_incarnation_id) JOIN media_resource_operations o ON o.attempt_id=a.attempt_id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let tx=conn.unchecked_transaction()?;
        tx.execute(&schema::assertion(&schema::migration_drained_guard()),[])?;
        for sql in crate::store::sharing_source_schema::adaptive_layout_statements() {tx.execute(&sql,[])?;}
        tx.commit()?;
        assert!(schema::sqlite_ready(conn,schema::Layout::Principal)?);
        let after:(String,String,String)=conn.query_row("SELECT binding_json,key_json,payload_digest FROM media_resource_requests r JOIN media_resource_attempts a USING(owner_key,request_id,request_incarnation_id) JOIN media_resource_operations o ON o.attempt_id=a.attempt_id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(before,after,"immutable request/attempt/op history survives actual rebuild");
        conn.execute("CREATE TRIGGER unrecognized_principal_writer AFTER UPDATE ON media_sessions BEGIN SELECT 1; END",[])?;
        assert!(!schema::sqlite_ready(conn,schema::Layout::Principal)?);
        Ok(())
    }).await.expect("real source rebuild transaction");
}

#[test]
fn partial_future_and_extra_schema_objects_remain_unavailable_without_repair() {
    for mode in ["partial", "future", "extra"] {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        SqliteStore::apply_migrations_for_test(&conn, crate::store::SQLITE_SCHEMA_VERSION)
            .expect("legacy");
        schema::install_sqlite_checked(&conn).expect("install");
        match mode {
            "partial" => {
                conn.execute("DROP TRIGGER media_resource_route_update", [])
                    .expect("partial fixture");
            }
            "future" => {
                conn.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE media_resource_schema SET version=2; PRAGMA ignore_check_constraints=OFF;").expect("synthetic future marker");
            }
            _ => {
                conn.execute("CREATE TRIGGER unknown_writer AFTER UPDATE ON media_sessions BEGIN SELECT 1; END",[]).expect("extra writer");
            }
        }
        let before=conn.query_row("SELECT group_concat(name||coalesce(sql,''),'|') FROM (SELECT name,sql FROM sqlite_master ORDER BY name)",[],|row|row.get::<_,String>(0)).expect("schema before");
        assert!(!schema::sqlite_ready(&conn, schema::Layout::Legacy).expect("unavailable"));
        schema::install_sqlite_checked(&conn).expect("preserve partial/future, never rebuild");
        let after=conn.query_row("SELECT group_concat(name||coalesce(sql,''),'|') FROM (SELECT name,sql FROM sqlite_master ORDER BY name)",[],|row|row.get::<_,String>(0)).expect("schema after");
        assert_eq!(before, after, "{mode}: no destructive repair");
    }
}
