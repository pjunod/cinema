#[test]
fn relay_acquisition_registration_is_atomic_and_rollback_claims_expire() {
    let mut db = rusqlite::Connection::open_in_memory().expect("membership DB");
    db.execute_batch("CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER,last_seen_at INTEGER,removed_at INTEGER); CREATE TABLE cluster_node_capabilities(node_id TEXT,capability TEXT,last_seen_at INTEGER,PRIMARY KEY(node_id,capability)); INSERT INTO cluster_nodes VALUES('owner',7,100,NULL);").expect("membership rows");
    let proof = |db: &rusqlite::Connection, members: &str, owner: &str, cutoff: i64, now: i64| {
        db.query_row(
            super::MEDIA_RELAY_ACQUISITION_PROOF_SQL,
            rusqlite::params![members, owner, cutoff, now],
            |row| row.get::<_, i64>(0),
        )
        .expect("production proof")
    };
    assert_eq!(
        proof(&db, "[7]", "owner", 1, 100),
        0,
        "generic membership has no daemon registration"
    );
    {
        let tx = db.transaction().expect("heartbeat transaction");
        tx.execute("UPDATE cluster_nodes SET last_seen_at=101", [])
            .expect("heartbeat");
        tx.execute(
            super::MEDIA_RELAY_ACQUISITION_HEARTBEAT_SQL,
            rusqlite::params!["owner", super::MEDIA_RELAY_ACQUISITION_CAPABILITY, 101],
        )
        .expect("registered daemon capability");
        assert_eq!(proof(&tx, "[7]", "owner", 1, 101), 1);
        tx.rollback()
            .expect("failed heartbeat rolls back capability too");
    }
    assert_eq!(proof(&db, "[7]", "owner", 1, 101), 0);
    {
        let tx = db.transaction().expect("heartbeat transaction");
        tx.execute("UPDATE cluster_nodes SET last_seen_at=102", [])
            .expect("heartbeat");
        tx.execute(
            super::MEDIA_RELAY_ACQUISITION_HEARTBEAT_SQL,
            rusqlite::params!["owner", super::MEDIA_RELAY_ACQUISITION_CAPABILITY, 102],
        )
        .expect("capability");
        tx.commit().expect("atomic registered heartbeat");
    }
    assert_eq!(proof(&db, "[7]", "owner", 1, 102), 1);
    assert_eq!(
        proof(&db, "[]", "owner", 1, 102),
        0,
        "uncommitted identity cannot advertise"
    );
    assert_eq!(proof(&db, "[7]", "other-owner", 1, 102), 0);
    assert_eq!(
        proof(&db, "[7]", "owner", 103, 200),
        0,
        "stale proof cannot advertise"
    );
    db.execute("UPDATE cluster_nodes SET last_seen_at=103", [])
        .expect("legacy rollback heartbeat");
    assert_eq!(
        proof(&db, "[7]", "owner", 1, 103),
        0,
        "old binary cannot refresh retained new-protocol claim"
    );
    db.execute(
        super::MEDIA_RELAY_ACQUISITION_HEARTBEAT_SQL,
        rusqlite::params!["owner", super::MEDIA_RELAY_ACQUISITION_CAPABILITY, 103],
    )
    .expect("updated daemon heartbeat");
    assert_eq!(proof(&db, "[7]", "owner", 1, 103), 1);
    db.execute("UPDATE cluster_nodes SET removed_at=104", [])
        .expect("removed owner");
    assert_eq!(proof(&db, "[7]", "owner", 1, 104), 0);
}
