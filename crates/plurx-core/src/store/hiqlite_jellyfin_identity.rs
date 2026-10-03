use super::hiqlite::{database_error, HiqliteAuthStore};
use super::jellyfin_identity::{allocation_sql, checked_ids, selection_sql};
use super::{JellyfinEntityId, JellyfinEntityKind, JellyfinIdentityStore};
use crate::error::StoreError;
use async_trait::async_trait;
use hiqlite::{macros::params, Row};
struct IdentityRow(JellyfinEntityId);
impl From<&mut Row<'_>> for IdentityRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(JellyfinEntityId {
            wire_id: row.get("wire_id"),
            native_id: row.get("native_id"),
        })
    }
}
struct NativeRow(i64);
impl From<&mut Row<'_>> for NativeRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("native_id"))
    }
}

#[async_trait]
impl JellyfinIdentityStore for HiqliteAuthStore {
    async fn jellyfin_entity_ids(
        &self,
        kind: JellyfinEntityKind,
        native_ids: &[i64],
    ) -> Result<Vec<JellyfinEntityId>, StoreError> {
        let ids = checked_ids(native_ids)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let sql = selection_sql(kind, &ids);
        let args = ids
            .iter()
            .map(|id| (*id).into())
            .collect::<hiqlite::Params>();
        let found = self
            .client()
            // authority: allocation must see existing live mappings and retirements.
            .query_consistent_map::<IdentityRow, _>(sql.clone(), args.clone())
            .await?;
        let missing = ids
            .iter()
            .filter(|id| !found.iter().any(|v| v.0.native_id == **id));
        let statements = missing
            .map(|id| {
                (
                    allocation_sql(kind),
                    params!(
                        uuid::Uuid::new_v4().simple().to_string(),
                        kind.name(),
                        uuid::Uuid::new_v4().simple().to_string(),
                        *id
                    ),
                )
            })
            .collect::<Vec<_>>();
        if statements.is_empty() {
            return Ok(found.into_iter().map(|r| r.0).collect());
        }
        for result in self
            .client()
            .txn(statements)
            .await
            .map_err(database_error)?
        {
            result.map_err(database_error)?;
        }
        Ok(self
            .client()
            // authority: read the committed winner after concurrent conditional allocation.
            .query_consistent_map::<IdentityRow, _>(sql, args)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect())
    }
    async fn jellyfin_resolve_entity(
        &self,
        kind: JellyfinEntityKind,
        wire_id: &str,
    ) -> Result<Option<i64>, StoreError> {
        // authority: a retired wire ID must never resolve through a reused native integer.
        Ok(self.client().query_consistent_map::<NativeRow,_>("SELECT native_id FROM jellyfin_entity_ids WHERE entity_kind=$1 AND wire_id=$2 AND retired=0", params!(kind.name(), wire_id)).await?.into_iter().next().map(|r| r.0))
    }
}
