//! Read existing receiver purpose material. No read-side factory or repair.
use super::sharing::{revision_key_envelopes, Backend};
use crate::{error::StoreError, secrets::SealedSecret, sharing::invalid};
use async_trait::async_trait;
use uuid::Uuid;

pub const CANDIDATE_FILE_LOCATOR_KEY_SCHEMA: &str =
    include_str!("sharing_file_locator_keys_schema.sql");
pub(super) const COLUMNS_SQL: &str = "SELECT json_array(name,type,\"notnull\",pk) AS payload FROM pragma_table_info('sharing_file_locator_keys') ORDER BY cid";
pub(super) const ROWS_SQL: &str = "SELECT json_object('singleton',singleton,'server_id',substr(server_id,1,37),'catalogue_epoch',substr(catalogue_epoch,1,37),'envelope',CASE WHEN length(locator_envelope)<=4096 THEN locator_envelope ELSE NULL END,'source_matches',EXISTS(SELECT 1 FROM sharing_identity s WHERE s.singleton=1 AND s.server_id=sharing_file_locator_keys.server_id AND s.catalogue_epoch=sharing_file_locator_keys.catalogue_epoch)) AS payload FROM sharing_file_locator_keys LIMIT 2";
pub(super) fn columns(rows: Vec<String>) -> Result<(), StoreError> {
    let columns: Vec<(String, String, i64, i64)> = rows
        .into_iter()
        .map(|row| serde_json::from_str(&row).map_err(|_| invalid()))
        .collect::<Result<_, _>>()?;
    if columns
        != [
            ("singleton".into(), "INTEGER".into(), 1, 1),
            ("server_id".into(), "TEXT".into(), 1, 0),
            ("catalogue_epoch".into(), "TEXT".into(), 1, 0),
            ("locator_envelope".into(), "TEXT".into(), 1, 0),
        ]
    {
        return Err(invalid());
    }
    Ok(())
}

#[async_trait]
pub trait SharingFileLocatorStore: Send + Sync {
    /// Absence is unavailable. Caller must open with the FileLocator purpose.
    async fn receiver_file_locator_key(
        &self,
        receiver: Uuid,
        epoch: Uuid,
    ) -> Result<Option<SealedSecret>, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingFileLocatorStore for T {
    async fn receiver_file_locator_key(
        &self,
        receiver: Uuid,
        epoch: Uuid,
    ) -> Result<Option<SealedSecret>, StoreError> {
        if receiver.is_nil() || epoch.is_nil() {
            return Err(invalid());
        }
        let rows = self.sharing_file_locator_key_rows().await?;
        let envelopes = revision_key_envelopes(rows.clone())?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let row: serde_json::Value = serde_json::from_str(row).map_err(|_| invalid())?;
        if row["server_id"].as_str() != Some(receiver.to_string().as_str())
            || row["catalogue_epoch"].as_str() != Some(epoch.to_string().as_str())
        {
            return Ok(None);
        }
        Ok(envelopes.into_iter().next())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        secrets::{CredentialKey, SharingSecretPurpose},
        sharing::SourceId,
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        sharing_file_locators::{FileLocatorKey, FileLocatorReference},
        store::{sharing::Statement, SharingStore, SqliteStore},
    };
    async fn execute(store: &SqliteStore, sql: &str) {
        store
            .sharing_txn(vec![(sql.into(), vec![])])
            .await
            .expect("fixture SQL");
    }
    #[tokio::test]
    async fn sharing_file_locator_key_survives_reopen_and_master_rewrap_without_read_repair() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let path = directory.path().join("receiver.sqlite");
        let store = SqliteStore::open(&path).expect("receiver");
        let identity = store.sharing_identity(1000).await.expect("identity");
        assert!(store
            .receiver_file_locator_key(identity.server_id, identity.catalogue_epoch)
            .await
            .expect("absent key")
            .is_none());
        let master = CredentialKey::from_bytes([37; 32]);
        let envelope =
            FileLocatorKey::generate_sealed(&master, &identity).expect("purpose material");
        store
            .sharing_txn(vec![
                (CANDIDATE_FILE_LOCATOR_KEY_SCHEMA.into(), vec![]),
                (
                    "INSERT INTO sharing_file_locator_keys VALUES(1,$1,$2,$3)".into(),
                    vec![
                        identity.server_id.into(),
                        identity.catalogue_epoch.into(),
                        envelope.as_stored().to_owned().into(),
                    ],
                ),
            ])
            .await
            .expect("explicit candidate provision");
        let reference = FileLocatorReference {
            item: SharedReference {
                import_id: Uuid::new_v4(),
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                library_id: SourceId::parse("9007199254740993").expect("library"),
                item_id: SourceId::parse("9223372036854775807").expect("item"),
            },
            lifecycle_generation: 3,
            file_id: SourceId::parse("7").expect("file"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
        };
        let locator = FileLocatorKey::open(&master, &identity, &envelope)
            .expect("key")
            .issue(&reference)
            .expect("locator");
        drop(store);
        let store = SqliteStore::open(&path).expect("reopen receiver");
        assert_eq!(
            store
                .sharing_sealed_census()
                .await
                .expect("restart census")
                .sealed_rows(),
            1
        );
        let restored = store
            .receiver_file_locator_key(identity.server_id, identity.catalogue_epoch)
            .await
            .expect("read existing")
            .expect("retained");
        assert!(
            FileLocatorKey::open(&master, &identity, &restored)
                .expect("restored key")
                .verify(locator.as_str(), reference.item.import_id, 3)
                .expect("retained locator")
                == reference
        );
        assert!(store
            .receiver_file_locator_key(Uuid::new_v4(), identity.catalogue_epoch)
            .await
            .expect("other receiver")
            .is_none());
        let new_master = CredentialKey::from_bytes([38; 32]);
        let clear = master
            .open_sharing(
                SharingSecretPurpose::FileLocator,
                identity.server_id,
                identity.catalogue_epoch,
                &restored,
            )
            .expect("unwrap for explicit maintenance");
        let rewrapped = new_master
            .seal_sharing(
                SharingSecretPurpose::FileLocator,
                identity.server_id,
                identity.catalogue_epoch,
                clear.expose(),
            )
            .expect("rewrap");
        let statement: Statement = (
            "UPDATE sharing_file_locator_keys SET locator_envelope=$1 WHERE singleton=1".into(),
            vec![rewrapped.as_stored().to_owned().into()],
        );
        store
            .sharing_txn(vec![statement])
            .await
            .expect("explicit rewrap");
        let retained = store
            .receiver_file_locator_key(identity.server_id, identity.catalogue_epoch)
            .await
            .expect("read rewrap")
            .expect("key");
        assert!(FileLocatorKey::open(&master, &identity, &retained).is_err());
        assert!(
            FileLocatorKey::open(&new_master, &identity, &retained)
                .expect("new master")
                .verify(locator.as_str(), reference.item.import_id, 3)
                .expect("same locator")
                == reference
        );
        execute(&store, "DELETE FROM sharing_file_locator_keys").await;
        assert!(store
            .receiver_file_locator_key(identity.server_id, identity.catalogue_epoch)
            .await
            .expect("empty candidate")
            .is_none());
        assert_eq!(
            store
                .sharing_sealed_census()
                .await
                .expect("empty census")
                .sealed_rows(),
            0
        );
    }
    #[tokio::test]
    async fn sharing_file_locator_census_refuses_partial_foreign_and_oversized_state() {
        let directory = tempfile::tempdir().expect("fixtures");
        for store in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&directory.path().join("receiver.sqlite")).expect("pooled"),
        ] {
            let identity = store.sharing_identity(1000).await.expect("identity");
            execute(
                &store,
                "CREATE VIEW sharing_file_locator_keys AS SELECT 1 AS singleton",
            )
            .await;
            assert!(store.sharing_sealed_census().await.is_err());
            execute(&store, "DROP VIEW sharing_file_locator_keys").await;
            execute(
                &store,
                "CREATE TABLE sharing_file_locator_keys(singleton INTEGER)",
            )
            .await;
            assert!(store.sharing_sealed_census().await.is_err());
            execute(&store, "DROP TABLE sharing_file_locator_keys").await;
            execute(&store, CANDIDATE_FILE_LOCATOR_KEY_SCHEMA).await;
            let master = CredentialKey::from_bytes([42; 32]);
            let envelope = FileLocatorKey::generate_sealed(&master, &identity).expect("key");
            store
                .sharing_txn(vec![(
                    "INSERT INTO sharing_file_locator_keys VALUES(1,$1,$2,$3)".into(),
                    vec![
                        identity.server_id.into(),
                        identity.catalogue_epoch.into(),
                        envelope.as_stored().to_owned().into(),
                    ],
                )])
                .await
                .expect("fixture key");
            for sql in ["UPDATE sharing_file_locator_keys SET locator_envelope='unsealed'","UPDATE sharing_file_locator_keys SET locator_envelope=hex(zeroblob(4097))","UPDATE sharing_file_locator_keys SET server_id='00000000-0000-0000-0000-000000000000'"] {
                execute(&store,sql).await;
                assert!(store.sharing_sealed_census().await.is_err());
                assert!(store.receiver_file_locator_key(identity.server_id,identity.catalogue_epoch).await.is_err());
                store.sharing_txn(vec![("UPDATE sharing_file_locator_keys SET server_id=$1,locator_envelope=$2 WHERE singleton=1".into(),vec![identity.server_id.into(),envelope.as_stored().to_owned().into()])]).await.expect("restore independently valid fixture");
                assert_eq!(store.sharing_sealed_census().await.expect("restored census").sealed_rows(),1);
            }
        }
    }
}
