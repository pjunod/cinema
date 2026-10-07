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
    fn sharing_is_replicated(&self) -> bool {
        false
    }
    async fn sharing_purpose_archive_rows(&self) -> Result<Vec<String>, StoreError> {
        self.with_read(|connection| {
            let snapshot=connection.unchecked_transaction()?;
            let present:i64=snapshot.query_row("SELECT CASE WHEN count(*)=0 THEN 0 WHEN count(*)=1 AND max(type)='table' THEN 1 ELSE 2 END FROM sqlite_master WHERE name='sharing_purpose_key_archive'",[],|row| row.get(0))?;
            if present==0 {return Ok(Vec::new());}
            if present!=1 {return Err(crate::sharing::invalid());}
            let read=|sql:&str| -> Result<Vec<String>,StoreError> {
                Ok(snapshot.prepare(sql)?.query_map([],|row| row.get(0))?.collect::<Result<_,_>>()?)
            };
            crate::store::sharing_purpose_keys::archive_columns(read(crate::store::sharing_purpose_keys::ARCHIVE_COLUMNS_SQL)?)?;
            let rows=read(crate::store::sharing_purpose_keys::ARCHIVE_ROWS_SQL)?;
            snapshot.commit()?;
            Ok(rows)
        }).await
    }
    async fn sharing_revision_key_rows(&self) -> Result<Vec<String>, StoreError> {
        self.with_read(|connection| {
            let snapshot=connection.unchecked_transaction()?;
            let present:i64=snapshot.query_row("SELECT CASE WHEN count(*)=0 THEN 0 WHEN count(*)=1 AND max(type)='table' THEN 1 ELSE 2 END FROM sqlite_master WHERE name='sharing_catalogue_keys'",[],|row| row.get(0))?;
            if present==0 {return Ok(Vec::new());}
            if present!=1 {return Err(crate::sharing::invalid());}
            let read=|sql:&str| -> Result<Vec<String>,StoreError> {
                Ok(snapshot.prepare(sql)?.query_map([],|row| row.get(0))?.collect::<Result<_,_>>()?)
            };
            crate::store::sharing::revision_key_columns(read(crate::store::sharing::REVISION_KEY_COLUMNS_SQL)?)?;
            let rows=read(crate::store::sharing::REVISION_KEY_ROWS_SQL)?;
            snapshot.commit()?;
            Ok(rows)
        }).await
    }
    async fn sharing_file_locator_key_rows(&self) -> Result<Vec<String>, StoreError> {
        self.with_read(|connection| {
            let snapshot=connection.unchecked_transaction()?;
            let present:i64=snapshot.query_row("SELECT CASE WHEN count(*)=0 THEN 0 WHEN count(*)=1 AND max(type)='table' THEN 1 ELSE 2 END FROM sqlite_master WHERE name='sharing_file_locator_keys'",[],|row| row.get(0))?;
            if present==0 {return Ok(Vec::new());}
            if present!=1 {return Err(crate::sharing::invalid());}
            let read=|sql:&str| -> Result<Vec<String>,StoreError> {
                Ok(snapshot.prepare(sql)?.query_map([],|row| row.get(0))?.collect::<Result<_,_>>()?)
            };
            crate::store::sharing_file_locators::columns(read(crate::store::sharing_file_locators::COLUMNS_SQL)?)?;
            let rows=read(crate::store::sharing_file_locators::ROWS_SQL)?;
            snapshot.commit()?;
            Ok(rows)
        }).await
    }
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
    async fn sharing_catalogue_revision_census_refuses_partial_foreign_and_oversized_rows() {
        let directory = tempfile::tempdir().expect("key census fixtures");
        for s in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&directory.path().join("keys.sqlite")).expect("pooled"),
        ] {
            assert_eq!(
                s.sharing_sealed_census()
                    .await
                    .expect("legacy absent table")
                    .sealed_rows(),
                0
            );
            let identity = s.sharing_identity(1000).await.expect("identity");
            let key = crate::secrets::CredentialKey::from_bytes([31; 32]);
            let envelope = key
                .seal_sharing(
                    SharingSecretPurpose::CatalogueRevision,
                    identity.server_id,
                    identity.catalogue_epoch,
                    "synthetic-purpose-key",
                )
                .expect("purpose key");
            s.sharing_txn(vec![
                (
                    crate::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA.into(),
                    vec![],
                ),
                (
                    "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".into(),
                    vec![
                        identity.server_id.into(),
                        identity.catalogue_epoch.into(),
                        envelope.as_stored().to_owned().into(),
                    ],
                ),
            ])
            .await
            .expect("candidate key fixture only");
            assert_eq!(
                s.sharing_sealed_census()
                    .await
                    .expect("key included")
                    .sealed_rows(),
                1
            );
            for invalid in ["unwrapped".to_owned(), "x".repeat(4097)] {
                s.sharing_txn(vec![(
                    "UPDATE sharing_catalogue_keys SET revision_envelope=$1".into(),
                    vec![invalid.into()],
                )])
                .await
                .expect("corruption fixture");
                assert!(s.sharing_sealed_census().await.is_err());
            }
            s.sharing_txn(vec![(
                "UPDATE sharing_catalogue_keys SET revision_envelope=$1,server_id=$2".into(),
                vec![
                    envelope.as_stored().to_owned().into(),
                    Uuid::new_v4().into(),
                ],
            )])
            .await
            .expect("foreign source fixture");
            assert!(s.sharing_sealed_census().await.is_err());
            s.sharing_txn(vec![("DROP TABLE sharing_catalogue_keys".into(),vec![]),("CREATE TABLE sharing_catalogue_keys(singleton INTEGER,server_id TEXT,catalogue_epoch TEXT)".into(),vec![])]).await.expect("partial shape fixture");
            assert!(
                s.sharing_sealed_census().await.is_err(),
                "empty partial table is not legacy absence"
            );
            s.sharing_txn(vec![("DROP TABLE sharing_catalogue_keys".into(),vec![]),("CREATE TABLE sharing_catalogue_keys(singleton INTEGER NOT NULL PRIMARY KEY,server_id TEXT NOT NULL,catalogue_epoch TEXT NOT NULL,revision_envelope TEXT NOT NULL)".into(),vec![]),("INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3),(2,$1,$2,$3)".into(),vec![identity.server_id.into(),identity.catalogue_epoch.into(),envelope.as_stored().to_owned().into()])]).await.expect("excess rows fixture");
            assert!(s.sharing_sealed_census().await.is_err());
        }
    }
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
