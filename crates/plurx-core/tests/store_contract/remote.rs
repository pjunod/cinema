use super::*;
use plurx_core::{
    auth,
    store::remote::{NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteProof, RemoteReceiver},
};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_replicated_credentials_caps_and_claim_budget() {
    for_each_backend(|store, _backend| async move {
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
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_adjunct_schema_repeats_at_81_82_and_refuses_denied_partial_shape() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("client");
    for version in [81_i64, 82] {
        client
            .execute(
                "UPDATE cluster_meta SET schema_version=$1 WHERE singleton=1",
                hiqlite::params!(version),
            )
            .await
            .expect("baseline");
        store
            .validation_install_remote_schema(false)
            .await
            .expect("repeat adjunct install");
        store
            .validation_install_remote_schema(false)
            .await
            .expect("repeat startup");
    }
    assert!(
        store.validation_install_remote_schema(true).await.is_err(),
        "even current adjunct needs migration admission"
    );
    client
        .execute("DELETE FROM remote_schema", hiqlite::params!())
        .await
        .expect("bad marker");
    assert!(store.validation_install_remote_schema(false).await.is_err());
    client
        .execute("INSERT INTO remote_schema VALUES(1,1)", hiqlite::params!())
        .await
        .expect("restore marker");
    client
        .execute("DROP INDEX remote_grants_receiver", hiqlite::params!())
        .await
        .expect("partial shape");
    assert!(store.validation_install_remote_schema(false).await.is_err());
}
