use super::*;
use crate::{
    auth,
    store::{InvitationStore, SqliteStore, UserStore},
};
#[test]
fn invitations_restore_clears_capabilities_with_foreign_keys_off() {
    let c = rusqlite::Connection::open_in_memory().expect("raw restore connection");
    c.execute_batch("PRAGMA foreign_keys=OFF;CREATE TABLE users(id INTEGER PRIMARY KEY);INSERT INTO users VALUES(1);").expect("raw restore FK OFF");
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .expect("FK setting"),
        0
    );
    c.execute_batch(&crate::store::remote::migration_sql())
        .expect("remote parent schema");
    c.execute_batch(SCHEMA).expect("invitations");
    c.execute_batch("INSERT INTO remote_receivers VALUES('receiver',1,'TV','web','hash',0,NULL);INSERT INTO invitation_phones(id,user_id,name,platform,secret_hash,token_digest,generation,created_at) VALUES('phone',1,'Phone','android','phone-hash','login-digest',1,0);INSERT INTO invitation_consents(id,phone_id,receiver_id,user_id,grant_id,grant_hash,enabled,transport,generation,broker_enrollment,broker_ticket,transport_status) VALUES('consent','phone','receiver',1,'grant','grant-hash',1,'fcm',1,'broker-capability','ticket-capability','ready');INSERT INTO invitation_events VALUES('event',1,'consent','phone','receiver','foreground','grant',1,1,0,0,120,'admitted',NULL,1);INSERT INTO invitation_cooldowns VALUES('receiver','phone',0);INSERT INTO invitation_broker_revocations VALUES('work',1,'broker-capability',1,0,120,0);").expect("old image capabilities");
    fence_restored_invitations(&c).expect("explicit restore fence");
    for table in [
        "invitation_phones",
        "invitation_consents",
        "invitation_events",
        "invitation_cooldowns",
        "invitation_broker_revocations",
    ] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .expect("remaining rows"),
            0
        );
    }
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check()", [], |r| r
            .get::<_, i64>(0))
            .expect("integrity"),
        0
    );
    c.execute("DROP INDEX invitation_events_pending", [])
        .expect("partial image");
    assert!(fence_restored_invitations(&c).is_err());
}
#[tokio::test]
async fn invitations_no_touch_login_expires_at_existing_policy_boundary() {
    let directory = tempfile::tempdir().expect("database directory");
    let path = directory.path().join("plurx.db");
    let store = SqliteStore::open(&path).expect("store");
    let user = store
        .create_user("expiry", "hash", false)
        .await
        .expect("user");
    let digest = auth::hash_token("synthetic login");
    store
        .create_token(&digest, user.id, None)
        .await
        .expect("login");
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let c = rusqlite::Connection::open(path).expect("controlled expiry fixture");
    c.execute(
        "UPDATE tokens SET last_seen_at=$1 WHERE token_hash=$2",
        rusqlite::params![timestamp - 86400, digest],
    )
    .expect("old activity");
    for (key, value) in [
        ("auth.token_expiry_enabled", "1".to_owned()),
        ("auth.token_idle_days", "1".to_owned()),
        ("auth.token_expiry_since", (timestamp - 172800).to_string()),
    ] {
        c.execute("INSERT INTO settings(key,value,updated_at) VALUES($1,$2,0) ON CONFLICT(key) DO UPDATE SET value=excluded.value",rusqlite::params![key,value]).expect("expiry policy");
    }
    assert!(store
        .invitation_login(&digest, timestamp - 1)
        .await
        .expect("before expiry")
        .is_some());
    assert!(store
        .invitation_login(&digest, timestamp)
        .await
        .expect("exact expiry")
        .is_none());
    assert_eq!(
        store
            .list_tokens_for_user(user.id)
            .await
            .expect("inventory")[0]
            .last_seen_at,
        timestamp - 86400,
        "background checks never revive or refresh login"
    );
}

#[test]
fn invitations_v1_upgrade_matches_exact_v2_shape_and_preserves_cursor() {
    let c = rusqlite::Connection::open_in_memory().expect("v1 image");
    c.execute_batch("PRAGMA foreign_keys=OFF;CREATE TABLE users(id INTEGER PRIMARY KEY);")
        .expect("fixture");
    c.execute_batch(SCHEMA_V1).expect("v1 schema");
    c.execute_batch("INSERT INTO invitation_phones(id,user_id,name,platform,secret_hash,token_digest,generation,created_at) VALUES('phone',1,'Phone','android','proof','login',1,0);INSERT INTO invitation_events VALUES('event',1,'consent','phone','receiver','foreground','grant',1,1,0,0,120,'admitted',NULL);").expect("old event");
    c.execute_batch(MIGRATION_V2).expect("checked upgrade");
    let rows = c
        .prepare(SHAPE_SQL)
        .expect("shape")
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("shape rows");
    assert!(verify_shape(&rows).expect("exact v2 shape"));
    assert_eq!(
        c.query_row("SELECT last_revision FROM invitation_phones", [], |r| r
            .get::<_, i64>(0))
            .expect("migrated high water"),
        1
    );
    assert_eq!(
        c.query_row("SELECT revision FROM invitation_events", [], |r| r
            .get::<_, i64>(0))
            .expect("migrated event"),
        1
    );
    let mut bad = rows.clone();
    bad.iter_mut()
        .find(|(n, _)| n == "invitation_phones")
        .expect("phone DDL")
        .1 = bad
        .iter()
        .find(|(n, _)| n == "invitation_phones")
        .expect("DDL")
        .1
        .replace("'android'", "'and roid'");
    assert!(
        verify_shape(&bad).is_err(),
        "quoted whitespace cannot normalize away an incompatible check"
    );
}
