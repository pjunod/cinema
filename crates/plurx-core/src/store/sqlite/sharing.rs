use super::SqliteStore;
use crate::{
    error::StoreError,
    store::sharing::{Backend, Statement, Value},
};
use async_trait::async_trait;
fn values(values: Vec<Value>) -> Vec<rusqlite::types::Value> {
    values
        .into_iter()
        .map(|v| match v {
            Value::Text(v) => rusqlite::types::Value::Text(v),
            Value::Integer(v) => rusqlite::types::Value::Integer(v),
        })
        .collect()
}
#[async_trait]
impl Backend for SqliteStore {
    async fn sharing_read(&self, sql: &str, params: Vec<Value>) -> Result<Vec<String>, StoreError> {
        let (sql, params) = crate::store::sharing::ordered(sql, params)?;
        let params = values(params);
        self.with_read(move |c| {
            let mut s = c.prepare(&sql)?;
            let result = s
                .query_map(rusqlite::params_from_iter(params), |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(result)
        })
        .await
    }
    async fn sharing_txn(&self, statements: Vec<Statement>) -> Result<Vec<usize>, StoreError> {
        self.with_conn(move |c| {
            let txn = c.unchecked_transaction()?;
            let mut counts = Vec::new();
            for (sql, params) in statements {
                let (sql, params) = crate::store::sharing::ordered(&sql, params)?;
                counts.push(txn.execute(&sql, rusqlite::params_from_iter(values(params)))?);
            }
            txn.commit()?;
            Ok(counts)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{LibraryKind, NewLibrary},
        secrets::SharingSecretPurpose,
        sharing::*,
        store::{LibraryStore, SharingStore, UserStore},
    };
    use uuid::Uuid;
    #[tokio::test]
    async fn sharing_deleted_numeric_user_id_cannot_inherit_viewer_assignment() {
        let s = SqliteStore::open_in_memory().expect("store");
        let user = s
            .create_user("viewer", "synthetic-hash", false)
            .await
            .expect("user");
        let dir = tempfile::tempdir().expect("dir");
        let key = crate::secrets::open_credential_key(&dir.path().join("key"), &Default::default())
            .expect("key");
        let source = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let id = Uuid::new_v4();
        let server = Uuid::new_v4();
        s.create_share_import(NewImport {
            id,
            source,
            source_name: "synthetic".into(),
            claim_id: Uuid::new_v4(),
            credential: key
                .seal_sharing(SharingSecretPurpose::Credential, server, id, "synthetic")
                .expect("seal"),
            claim_secret: key
                .seal_sharing(SharingSecretPurpose::Claim, server, id, "synthetic")
                .expect("seal"),
            endpoints: vec![Endpoint {
                ipv4: "100.101.102.103".parse().expect("ip"),
                ipv6: None,
                ts_fqdn: "source.example.ts.net".into(),
                port: 32443,
                spki_sha256: "a".repeat(64),
            }],
            now_ms: 1000,
        })
        .await
        .expect("import");
        s.settle_share_claim(id, Uuid::new_v4(), true, 1001)
            .await
            .expect("activate");
        let library = SourceId::parse("1").expect("id");
        s.assign_share_viewers(
            id,
            1,
            vec![Assignment {
                library_id: library.clone(),
                user_id: user.id,
            }],
            1002,
        )
        .await
        .expect("assign");
        let first = s
            .authorize_share_viewer(id, library.clone(), user.id)
            .await
            .expect("authorize")
            .expect("viewer");
        s.delete_user(user.id).await.expect("delete");
        s.with_conn(move|c| {c.execute("INSERT INTO users(id,username,password_hash) VALUES(?1,'recreated','synthetic-hash')",[user.id])?;Ok(())}).await.expect("recreate same numeric id");
        assert!(s
            .authorize_share_viewer(id, library.clone(), user.id)
            .await
            .expect("authorize")
            .is_none());
        s.assign_share_viewers(
            id,
            2,
            vec![Assignment {
                library_id: library.clone(),
                user_id: user.id,
            }],
            1003,
        )
        .await
        .expect("reassign");
        assert_ne!(
            s.authorize_share_viewer(id, library, user.id)
                .await
                .expect("authorize")
                .expect("viewer"),
            first
        );
    }
    #[tokio::test]
    async fn sharing_library_fk_refuses_delete_that_bypasses_generation_transaction() {
        let s = SqliteStore::open_in_memory().expect("store");
        let l = s
            .create_library(&NewLibrary {
                name: "synthetic".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let id = Uuid::new_v4();
        s.create_share_invitation(InvitationRecord {
            id,
            token_hash: "a".repeat(64),
            library_ids: vec![l.id],
            created_at_ms: 1000,
            expires_at_ms: 2000,
        })
        .await
        .expect("invite");
        s.claim_share(ShareClaim {
            invitation_id: id,
            invitation_hash: "a".repeat(64),
            claim_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
            recipient_server_id: Uuid::new_v4(),
            recipient_name: "synthetic".into(),
            credential_hash: "b".repeat(64),
            now_ms: 1001,
        })
        .await
        .expect("claim");
        assert!(s
            .with_conn(move |c| {
                c.execute("DELETE FROM libraries WHERE id=?1", [l.id])?;
                Ok(())
            })
            .await
            .is_err());
        assert!(s.delete_library(l.id).await.expect("transactional delete"));
    }
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test]
    async fn sharing_restore_fences_old_authority_without_deleting_private_history() {
        let s = SqliteStore::open_in_memory().expect("store");
        s.sharing_identity(1000).await.expect("identity");
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO settings(key,value) VALUES('sharing_enabled','true')",
                [],
            )?;
            c.execute("INSERT INTO users(id,username,password_hash) VALUES(1,'viewer','synthetic')",[])?;
            c.execute("INSERT INTO sharing_viewers(user_id,viewer_id) VALUES(1,'00000000-0000-4000-8000-000000000001')",[])?;
            c.execute("INSERT INTO sharing_watch(source_server_id,catalogue_epoch,remote_library_id,remote_item_id,user_id,position_ms,watched,sequence,updated_at_ms) VALUES('source','epoch','1','2',1,7000,0,1,1000)",[])?;
            crate::store::sharing::fence_restored_sharing(c)?;
            assert_eq!(c.query_row("SELECT position_ms FROM sharing_watch WHERE user_id=1",[],|r|r.get::<_,i64>(0))?,7000);
            assert_eq!(
                c.query_row(
                    "SELECT value FROM settings WHERE key='sharing_enabled'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "false"
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM sharing_identity", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .expect("fence restore");
    }
}
