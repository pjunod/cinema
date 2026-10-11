use super::hiqlite::HiqliteAuthStore;
use super::jellyfin_catalog::prepare;
use super::{JellyfinCatalogPage, JellyfinCatalogQuery, JellyfinCatalogStore};
use crate::error::StoreError;
use async_trait::async_trait;
use hiqlite::{macros::params, Row};
struct CatalogRow(String);
impl From<&mut Row<'_>> for CatalogRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("result_json"))
    }
}
#[async_trait]
impl JellyfinCatalogStore for HiqliteAuthStore {
    async fn jellyfin_catalog_artwork(
        &self,
        wire_id: String,
        backdrop: bool,
    ) -> Result<Option<crate::store::JellyfinCatalogArtwork>, StoreError> {
        self.client()
            // authority: anonymous artwork cannot follow retired identities or a disabled/replaced switch generation.
            .query_consistent_map::<CatalogRow, _>(
                super::jellyfin_catalog::CATALOG_ARTWORK_SQL,
                params!(wire_id, backdrop),
            )
            .await?
            .into_iter()
            .next()
            .map(|r| serde_json::from_str(&r.0).map_err(|e| StoreError::Identity(e.to_string())))
            .transpose()
    }
    async fn jellyfin_catalog_identity(
        &self,
        token_hash: String,
    ) -> Result<Option<crate::store::JellyfinCatalogIdentity>, StoreError> {
        self.client()
            // authority: user incarnation and views remain bound to the presented compatibility token.
            .query_consistent_map::<CatalogRow, _>(
                super::jellyfin_catalog::CATALOG_IDENTITY_SQL,
                params!(token_hash),
            )
            .await?
            .into_iter()
            .next()
            .map(|r| serde_json::from_str(&r.0).map_err(|e| StoreError::Identity(e.to_string())))
            .transpose()
    }
    async fn jellyfin_catalog_page(
        &self,
        query: JellyfinCatalogQuery,
    ) -> Result<JellyfinCatalogPage, StoreError> {
        let (sql, args) = prepare(&query)?;
        let row = self
            .client()
            // authority: catalog bodies, credentials, retirement mappings and watch facts share one committed snapshot.
            .query_consistent_map::<CatalogRow, _>(sql, params!(args))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| StoreError::Identity("missing Jellyfin catalog snapshot".into()))?;
        serde_json::from_str(&row.0).map_err(|e| StoreError::Identity(e.to_string()))
    }
}
