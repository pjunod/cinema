use super::SqliteStore;
use crate::error::StoreError;
use crate::store::jellyfin_identity::{allocation_sql, checked_ids, selection_sql};
use crate::store::{JellyfinEntityId, JellyfinEntityKind, JellyfinIdentityStore};
use async_trait::async_trait;
use rusqlite::{params, params_from_iter, OptionalExtension};

#[async_trait]
impl JellyfinIdentityStore for SqliteStore {
    async fn jellyfin_entity_ids(
        &self,
        kind: JellyfinEntityKind,
        native_ids: &[i64],
    ) -> Result<Vec<JellyfinEntityId>, StoreError> {
        let ids = checked_ids(native_ids)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let sql = selection_sql(kind, &ids);
            let read = || -> Result<Vec<JellyfinEntityId>, StoreError> {
                Ok(tx
                    .prepare(&sql)?
                    .query_map(params_from_iter(ids.iter()), |row| {
                        Ok(JellyfinEntityId {
                            wire_id: row.get(0)?,
                            native_id: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?)
            };
            let found = read()?;
            if found.len() == ids.len() {
                tx.commit()?;
                return Ok(found);
            }
            let insert = allocation_sql(kind);
            for id in ids
                .iter()
                .filter(|id| !found.iter().any(|v| v.native_id == **id))
            {
                tx.execute(
                    &insert,
                    params![
                        uuid::Uuid::new_v4().simple().to_string(),
                        kind.name(),
                        uuid::Uuid::new_v4().simple().to_string(),
                        id
                    ],
                )?;
            }
            let result = read()?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
    async fn jellyfin_resolve_entity(
        &self,
        kind: JellyfinEntityKind,
        wire_id: &str,
    ) -> Result<Option<i64>, StoreError> {
        let wire = wire_id.to_owned();
        self.with_conn(move |conn| Ok(conn.query_row("SELECT native_id FROM jellyfin_entity_ids WHERE entity_kind=?1 AND wire_id=?2 AND retired=0", params![kind.name(), wire], |row| row.get(0)).optional()?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::UserStore;

    #[tokio::test]
    async fn jellyfin_user_retirement_survives_reopen_and_schema_replay() {
        let root = tempfile::tempdir().expect("identity restart fixture");
        let path = root.path().join("identity.db");
        let store = SqliteStore::open(&path).expect("identity restart fixture");
        let user = store
            .create_user("original", "hash", false)
            .await
            .expect("identity restart fixture");
        let ids = store
            .jellyfin_entity_ids(JellyfinEntityKind::User, &[user.id])
            .await
            .expect("identity restart fixture");
        assert!(store
            .delete_user(user.id)
            .await
            .expect("identity restart fixture"));
        drop(store);
        let conn = rusqlite::Connection::open(&path).expect("identity restart fixture");
        // Additive replay preserves permanent tombstones and all trigger names.
        conn.execute_batch(crate::store::jellyfin_identity::JELLYFIN_IDENTITY_SCHEMA)
            .expect("identity restart fixture");
        conn.execute_batch(crate::store::jellyfin_identity::JELLYFIN_IDENTITY_SCHEMA)
            .expect("identity restart fixture");
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'jellyfin_retire_%'", [], |row| row.get::<_, i64>(0)).expect("identity restart fixture"), 4);
        assert_eq!(
            conn.query_row(
                "SELECT retired FROM jellyfin_entity_ids WHERE wire_id=?1",
                [&ids[0].wire_id],
                |row| row.get::<_, i64>(0)
            )
            .expect("identity restart fixture"),
            1
        );
        drop(conn);
        let reopened = SqliteStore::open(&path).expect("identity restart fixture");
        let replacement = reopened
            .create_user("replacement", "hash", false)
            .await
            .expect("identity restart fixture");
        assert_eq!(replacement.id, user.id);
        let new_ids = reopened
            .jellyfin_entity_ids(JellyfinEntityKind::User, &[replacement.id])
            .await
            .expect("identity restart fixture");
        assert_ne!(ids[0].wire_id, new_ids[0].wire_id);
        assert_eq!(
            reopened
                .jellyfin_resolve_entity(JellyfinEntityKind::User, &ids[0].wire_id)
                .await
                .expect("identity restart fixture"),
            None
        );
    }
}
