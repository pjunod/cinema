use super::*;
use crate::{
    auth,
    store::{
        remote::{NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteProof, RemoteReceiver},
        SqliteStore, Store,
    },
};
use std::sync::Arc;
#[tokio::test]
async fn remote_credentials_require_exact_proofs_and_atomic_caps() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let user = store
        .create_user("remote", "hash", false)
        .await
        .expect("user");
    let other = store
        .create_user("other", "hash", false)
        .await
        .expect("other");
    let rh = auth::hash_token("receiver");
    let gh = auth::hash_token("grant");
    let id = uuid::Uuid::new_v4().to_string();
    for n in 0..20 {
        assert!(store
            .create_remote_receiver(NewRemoteReceiver {
                receiver: RemoteReceiver {
                    id: if n == 0 {
                        id.clone()
                    } else {
                        uuid::Uuid::new_v4().to_string()
                    },
                    user_id: user.id,
                    name: "TV".into(),
                    platform: "web".into(),
                    created_at: 0
                },
                secret_hash: rh.clone()
            })
            .await
            .expect("receiver"));
    }
    let new = NewRemoteReceiver {
        receiver: RemoteReceiver {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user.id,
            name: "new".into(),
            platform: "web".into(),
            created_at: 0,
        },
        secret_hash: rh.clone(),
    };
    assert!(!store
        .create_remote_receiver(new.clone())
        .await
        .expect("cap"));
    assert!(store
        .remote_authority(
            &id,
            user.id,
            RemoteProof::Receiver {
                secret_hash: String::new()
            }
        )
        .await
        .is_err());
    assert!(store
        .remote_authority(
            &id,
            user.id,
            RemoteProof::Receiver {
                secret_hash: gh.clone()
            }
        )
        .await
        .expect("wrong proof")
        .is_none());
    assert!(store
        .remote_authority(
            &id,
            other.id,
            RemoteProof::Receiver {
                secret_hash: rh.clone()
            }
        )
        .await
        .expect("wrong user")
        .is_none());
    let gid = uuid::Uuid::new_v4().to_string();
    assert!(store
        .create_remote_grant(NewRemoteGrant {
            grant: RemoteGrant {
                id: gid.clone(),
                receiver_id: id.clone(),
                name: "Phone".into(),
                created_at: 0
            },
            user_id: user.id,
            secret_hash: gh.clone()
        })
        .await
        .expect("grant"));
    for _ in 1..8 {
        assert!(store
            .create_remote_grant(NewRemoteGrant {
                grant: RemoteGrant {
                    id: uuid::Uuid::new_v4().to_string(),
                    receiver_id: id.clone(),
                    name: "phone".into(),
                    created_at: 0
                },
                user_id: user.id,
                secret_hash: gh.clone()
            })
            .await
            .expect("grant within cap"));
    }
    assert!(!store
        .create_remote_grant(NewRemoteGrant {
            grant: RemoteGrant {
                id: uuid::Uuid::new_v4().to_string(),
                receiver_id: id.clone(),
                name: "ninth".into(),
                created_at: 0
            },
            user_id: user.id,
            secret_hash: gh.clone()
        })
        .await
        .expect("eight grant cap"));

    assert!(store
        .remote_authority(
            &id,
            user.id,
            RemoteProof::Grant {
                grant_id: gid.clone(),
                secret_hash: String::new()
            }
        )
        .await
        .is_err());
    assert!(store
        .remote_authority(
            &id,
            user.id,
            RemoteProof::Grant {
                grant_id: gid.clone(),
                secret_hash: gh.clone()
            }
        )
        .await
        .expect("grant proof")
        .is_some());
    store
        .revoke_remote_grant(&gid, user.id, 1)
        .await
        .expect("revoke");
    assert!(store
        .remote_authority(
            &id,
            user.id,
            RemoteProof::Grant {
                grant_id: gid,
                secret_hash: gh
            }
        )
        .await
        .expect("revoked")
        .is_none());
    store
        .revoke_remote_receiver(&id, user.id, 1)
        .await
        .expect("cleanup");
    assert!(store.create_remote_receiver(new).await.expect("cap freed"));
    for _ in 0..10 {
        assert!(store
            .admit_remote_pair_claim(user.id, 0)
            .await
            .expect("budget"));
    }
    assert!(!store
        .admit_remote_pair_claim(user.id, 59)
        .await
        .expect("limited"));
    assert!(store
        .admit_remote_pair_claim(user.id, 60)
        .await
        .expect("new window"));
}
#[test]
fn remote_schema_and_restore_refuse_partial_shapes_and_revoke_old_proofs() {
    let c = rusqlite::Connection::open_in_memory().expect("connection");
    c.execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY);INSERT INTO users VALUES(1);")
        .expect("users");
    c.execute_batch(&migration_sql()).expect("schema");
    let shape = |c: &rusqlite::Connection| {
        c.prepare(SHAPE_SQL)
            .expect("shape")
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .expect("rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("shape rows")
    };
    assert!(verify_shape(&shape(&c)).expect("exact shape"));
    c.execute(
        "INSERT INTO remote_receivers VALUES('r',1,'TV','web','hash',0,NULL)",
        [],
    )
    .expect("receiver");
    c.execute(
        "INSERT INTO remote_grants VALUES('g','r',1,'phone','hash',0,NULL)",
        [],
    )
    .expect("grant");
    fence_restored_remote(&c).expect("restore fence");
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM remote_grants WHERE revoked_at IS NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("live grants"),
        0
    );
    c.execute("DROP INDEX remote_grants_receiver", [])
        .expect("partial shape");
    assert!(verify_shape(&shape(&c)).is_err());
    assert!(fence_restored_remote(&c).is_err());
}
