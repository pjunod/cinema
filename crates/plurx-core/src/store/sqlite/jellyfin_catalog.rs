use super::SqliteStore;
use crate::error::StoreError;
use crate::store::jellyfin_catalog::prepare;
use crate::store::{JellyfinCatalogPage, JellyfinCatalogQuery, JellyfinCatalogStore};
use async_trait::async_trait;
use rusqlite::OptionalExtension;
#[async_trait]
impl JellyfinCatalogStore for SqliteStore {
    async fn jellyfin_catalog_artwork(
        &self,
        wire_id: String,
        backdrop: bool,
    ) -> Result<Option<crate::store::JellyfinCatalogArtwork>, StoreError> {
        self.with_conn(move |conn| {
            let text: Option<String> = conn
                .query_row(
                    crate::store::jellyfin_catalog::CATALOG_ARTWORK_SQL,
                    rusqlite::params![wire_id, backdrop],
                    |row| row.get(0),
                )
                .optional()?;
            text.map(|s| serde_json::from_str(&s).map_err(|e| StoreError::Identity(e.to_string())))
                .transpose()
        })
        .await
    }
    async fn jellyfin_catalog_identity(
        &self,
        token_hash: String,
    ) -> Result<Option<crate::store::JellyfinCatalogIdentity>, StoreError> {
        self.with_conn(move |conn| {
            let text: Option<String> = conn
                .query_row(
                    crate::store::jellyfin_catalog::CATALOG_IDENTITY_SQL,
                    [token_hash],
                    |row| row.get(0),
                )
                .optional()?;
            text.map(|s| serde_json::from_str(&s).map_err(|e| StoreError::Identity(e.to_string())))
                .transpose()
        })
        .await
    }
    async fn jellyfin_catalog_page(
        &self,
        query: JellyfinCatalogQuery,
    ) -> Result<JellyfinCatalogPage, StoreError> {
        let (sql, args) = prepare(&query)?;
        self.with_conn(move |conn| {
            let text: String = conn.query_row(&sql, [args], |row| row.get(0))?;
            serde_json::from_str(&text).map_err(|e| StoreError::Identity(e.to_string()))
        })
        .await
    }
}
