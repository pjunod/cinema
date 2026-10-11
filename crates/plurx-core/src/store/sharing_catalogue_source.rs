//! Consistent source authority, revision and catalogue reads. No foreign user ID.
use super::sharing::Backend;
use crate::{
    error::StoreError,
    sharing::{invalid, is_hash, SourceId},
    sharing_catalogue::*,
};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

/// Candidate only: runtime migrations remain coordinated with the session rebuild.
pub const CANDIDATE_SCHEMA: &str = include_str!("sharing_catalogue_schema.sql");
pub const CANDIDATE_ITEM_IDENTITY_SCHEMA: &str = include_str!("item_identity_schema.sql");
pub const CANDIDATE_REVISION_KEY_SCHEMA: &str = include_str!("sharing_catalogue_keys_schema.sql");
const REQUIRED_OBJECTS: &str = "'sharing_catalogue_binary_order','sharing_catalogue_children_order','sharing_catalogue_library_insert','sharing_catalogue_item_insert','sharing_catalogue_item_delete','sharing_catalogue_item_move','sharing_catalogue_item_sort','item_identity_watermark','item_identity_no_reuse','item_identity_observe'";
pub(super) const RECORD: &str = "CASE WHEN length(CAST(i.title AS BLOB))<=512 AND length(CAST(i.sort_title AS BLOB))<=512 AND coalesce(length(CAST(i.overview AS BLOB)),0)<=8192 AND coalesce(length(CAST(i.poster_path AS BLOB)),0)<=256 AND coalesce(length(CAST(i.backdrop_path AS BLOB)),0)<=256 AND length(CAST(i.genres AS BLOB))<=65536 THEN CASE WHEN json_valid(i.genres) AND json_type(i.genres)='array' AND (SELECT count(*) FROM json_each(i.genres))<=64 AND NOT EXISTS(SELECT 1 FROM json_each(i.genres) g WHERE g.type!='text' OR length(CAST(g.value AS BLOB))>128) THEN json_object('item',json_object('item_id',cast(i.id AS TEXT),'library_id',cast(i.library_id AS TEXT),'parent_id',CASE WHEN i.parent_id IS NULL THEN NULL ELSE cast(i.parent_id AS TEXT) END,'kind',i.kind,'title',i.title,'sort_title',i.sort_title,'year',i.year,'overview',i.overview,'genres',json(i.genres),'season_number',i.season_number,'episode_number',i.episode_number),'boundary_sort_key',CASE WHEN i.parent_id IS NULL THEN i.sort_title ELSE printf('%010d:%010d:%s',coalesce(i.season_number,2147483647),coalesce(i.episode_number,2147483647),i.sort_title) END,'poster_filename',i.poster_path,'backdrop_filename',i.backdrop_path) ELSE NULL END ELSE NULL END";

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLibrary {
    pub library_id: SourceId,
    pub name: String,
    pub kind: String,
    pub anime: bool,
}
#[derive(Debug)]
pub struct SourceBatchEntry {
    pub item_id: SourceId,
    pub record: Option<SourceCatalogueRecord>,
}
#[async_trait]
pub trait SharingSourceCatalogueStore: Send + Sync {
    async fn source_scope_authorized(
        &self,
        hash: &str,
        grant: Uuid,
        request: &crate::sharing_catalogue_details::SourceScopeRequest,
    ) -> Result<bool, StoreError>;
    /// Current authority for an already authenticated content body. Credential
    /// rotation does not revoke a grant; effective item/library revocation does.
    async fn source_content_authorized(
        &self,
        grant: Uuid,
        server: Uuid,
        epoch: Uuid,
        libraries: &[SourceId],
        items: &[(SourceId, SourceId)],
    ) -> Result<bool, StoreError>;

    async fn source_catalogue_page(
        &self,
        request: CataloguePageRequest,
    ) -> Result<Option<SourceCataloguePage>, StoreError>;
    async fn source_catalogue_batch(
        &self,
        hash: &str,
        grant: Uuid,
        batch: MetadataBatch,
    ) -> Result<Option<Vec<SourceBatchEntry>>, StoreError>;
    async fn source_catalogue_libraries(
        &self,
        hash: &str,
        grant: Uuid,
    ) -> Result<Option<Vec<SourceLibrary>>, StoreError>;
}
pub(super) async fn ready<T: Backend>(backend: &T) -> Result<(), StoreError> {
    let rows=backend.sharing_read(&format!("SELECT json_quote(count(*)) AS payload FROM sqlite_master WHERE name IN ({REQUIRED_OBJECTS}) AND type IN ('index','trigger','table')"),vec![]).await?;
    if rows.first().map(String::as_str) != Some("10") {
        return Err(StoreError::Identity(
            "sharing catalogue order maintenance unavailable".into(),
        ));
    }
    let rows=backend.sharing_read("SELECT json_quote(count(*)) AS payload FROM item_identity_watermark WHERE singleton=1 AND importing=0",vec![]).await?;
    if rows.first().map(String::as_str) != Some("1") {
        return Err(StoreError::Identity(
            "sharing catalogue import unavailable".into(),
        ));
    }
    Ok(())
}
fn record_ok(record: &SourceCatalogueRecord) -> Result<(), StoreError> {
    record.item.validate().map_err(|_| invalid())?;
    if record.boundary_sort_key.len() > 544
        || record.boundary_sort_key.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    if record
        .poster_filename
        .as_ref()
        .is_some_and(|p| p.len() > 256)
        || record
            .backdrop_filename
            .as_ref()
            .is_some_and(|p| p.len() > 256)
    {
        return Err(invalid());
    }
    Ok(())
}
fn number(id: &SourceId) -> Result<i64, StoreError> {
    id.as_str().parse().map_err(|_| invalid())
}
fn json<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|_| invalid())
}
#[async_trait]
impl<T: Backend> SharingSourceCatalogueStore for T {
    async fn source_scope_authorized(
        &self,
        hash: &str,
        grant: Uuid,
        request: &crate::sharing_catalogue_details::SourceScopeRequest,
    ) -> Result<bool, StoreError> {
        if !is_hash(hash) {
            return Err(invalid());
        }
        request.validate()?;
        ready(self).await?;
        let request = serde_json::to_string(request).map_err(|_| invalid())?;
        let sql="SELECT json_quote(EXISTS(SELECT 1 FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1 WHERE e.id=$1 AND e.token_hash=$2 AND e.state='active' AND e.id=json_extract($3,'$.grant_id') AND e.recipient_server_id=json_extract($3,'$.recipient_server_id') AND s.server_id=json_extract($3,'$.server_id') AND s.catalogue_epoch=json_extract($3,'$.catalogue_epoch') AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0) AND NOT EXISTS(SELECT 1 FROM json_each($3,'$.libraries') r WHERE NOT EXISTS(SELECT 1 FROM sharing_export_libraries x JOIN libraries l ON l.id=x.library_id WHERE x.grant_id=e.id AND CAST(l.id AS TEXT)=r.value AND l.kind IN ('movies','shows'))) AND NOT EXISTS(SELECT 1 FROM json_each($3,'$.items') r WHERE NOT EXISTS(SELECT 1 FROM sharing_export_libraries x JOIN libraries l ON l.id=x.library_id JOIN items i ON i.library_id=l.id WHERE x.grant_id=e.id AND CAST(l.id AS TEXT)=json_extract(r.value,'$.library_id') AND CAST(i.id AS TEXT)=json_extract(r.value,'$.item_id') AND l.kind IN ('movies','shows') AND i.kind IN ('movie','show','season','episode'))) AND NOT EXISTS(SELECT 1 FROM json_each($3,'$.files') r WHERE NOT EXISTS(SELECT 1 FROM sharing_export_libraries x JOIN libraries l ON l.id=x.library_id JOIN items i ON i.library_id=l.id JOIN files f ON f.item_id=i.id WHERE x.grant_id=e.id AND CAST(l.id AS TEXT)=json_extract(r.value,'$.library_id') AND CAST(i.id AS TEXT)=json_extract(r.value,'$.item_id') AND CAST(f.id AS TEXT)=json_extract(r.value,'$.file_id') AND l.kind IN ('movies','shows') AND i.kind IN ('movie','episode'))))) AS payload";
        let rows = self
            .sharing_read(
                sql,
                vec![grant.into(), hash.to_owned().into(), request.into()],
            )
            .await?;
        Ok(rows.len() == 1 && rows[0] == "1")
    }

    async fn source_content_authorized(
        &self,
        grant: Uuid,
        server: Uuid,
        epoch: Uuid,
        libraries: &[SourceId],
        items: &[(SourceId, SourceId)],
    ) -> Result<bool, StoreError> {
        if libraries.len() > crate::sharing::MAX_LIBRARIES || items.len() > MAX_PAGE_SIZE {
            return Err(invalid());
        }
        ready(self).await?;
        let libraries = serde_json::to_string(libraries).map_err(|_| invalid())?;
        let items = serde_json::to_string(items).map_err(|_| invalid())?;
        let sql="SELECT json_quote(CASE WHEN EXISTS(SELECT 1 FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1 WHERE e.id=$1 AND e.state='active' AND s.server_id=$2 AND s.catalogue_epoch=$3 AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0) AND NOT EXISTS(SELECT 1 FROM json_each($4) requested WHERE NOT EXISTS(SELECT 1 FROM sharing_export_libraries x JOIN libraries l ON l.id=x.library_id WHERE x.grant_id=e.id AND l.id=CAST(requested.value AS INTEGER) AND l.kind IN ('movies','shows'))) AND NOT EXISTS(SELECT 1 FROM json_each($5) requested WHERE NOT EXISTS(SELECT 1 FROM items i JOIN libraries l ON l.id=i.library_id JOIN sharing_export_libraries x ON x.library_id=l.id WHERE x.grant_id=e.id AND i.library_id=CAST(json_extract(requested.value,'$[0]') AS INTEGER) AND i.id=CAST(json_extract(requested.value,'$[1]') AS INTEGER) AND i.kind IN ('movie','show','season','episode') AND l.kind IN ('movies','shows')))) THEN 1 ELSE 0 END) AS payload";
        let rows = self
            .sharing_read(
                sql,
                vec![
                    grant.into(),
                    server.into(),
                    epoch.into(),
                    libraries.into(),
                    items.into(),
                ],
            )
            .await?;
        match rows.first().map(String::as_str) {
            Some("1") => Ok(true),
            Some("0") => Ok(false),
            _ => Err(invalid()),
        }
    }

    async fn source_catalogue_page(
        &self,
        r: CataloguePageRequest,
    ) -> Result<Option<SourceCataloguePage>, StoreError> {
        if !is_hash(&r.credential_hash)
            || r.query.len() > 512
            || !(1..=MAX_PAGE_SIZE).contains(&r.limit)
        {
            return Err(invalid());
        }
        if let Some(b) = &r.boundary {
            if b.sort_key.len() > 544 || b.sort_key.chars().any(char::is_control) {
                return Err(invalid());
            }
        }
        ready(self).await?;
        let search = r
            .query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let sort_key = if r.parent_id.is_none() {
            "i.sort_title"
        } else {
            "printf('%010d:%010d:%s',coalesce(i.season_number,2147483647),coalesce(i.episode_number,2147483647),i.sort_title)"
        };
        let index = if r.parent_id.is_none() {
            "sharing_catalogue_binary_order"
        } else {
            "sharing_catalogue_children_order"
        };
        let parent = if r.parent_id.is_none() {
            "i.parent_id IS NULL"
        } else {
            "i.parent_id=cast(json_extract($6,'$') AS INTEGER)"
        };
        let seek = if r.boundary.is_none() {
            "json_type($5,'$')='null'".into()
        } else {
            format!("({sort_key} COLLATE BINARY,i.id) > (json_extract($5,'$.sort_key') COLLATE BINARY,cast(json_extract($5,'$.item_id') AS INTEGER))")
        };
        let sql=format!("WITH allowed AS (SELECT e.scope_generation,e.catalogue_generation,c.order_revision FROM sharing_exports e JOIN sharing_export_libraries x ON x.grant_id=e.id JOIN libraries l ON l.id=x.library_id JOIN sharing_catalogue_revisions c ON c.library_id=l.id WHERE e.token_hash=$1 AND e.id=$2 AND e.state='active' AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0) AND l.id=$3 AND l.kind IN ('movies','shows') AND (json_type($6,'$')='null' OR EXISTS(SELECT 1 FROM items p WHERE p.id=cast(json_extract($6,'$') AS INTEGER) AND p.library_id=l.id AND p.kind IN ('show','season')))), page AS (SELECT {RECORD} AS record FROM items i INDEXED BY {index} WHERE i.library_id=$3 AND EXISTS(SELECT 1 FROM allowed) AND i.kind IN ('movie','show','season','episode') AND {parent} AND i.title LIKE '%'||$4||'%' ESCAPE '\\' AND {seek} ORDER BY {sort_key} COLLATE BINARY,i.id LIMIT $7) SELECT json_object('scope_generation',a.scope_generation,'catalogue_generation',a.catalogue_generation,'library_revision',a.order_revision,'records',json(coalesce((SELECT json_group_array(json(record)) FROM page),'[]'))) AS payload FROM allowed a");
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    r.credential_hash.into(),
                    r.grant_id.into(),
                    number(&r.library_id)?.into(),
                    search.into(),
                    json(&r.boundary)?.into(),
                    json(&r.parent_id)?.into(),
                    (r.limit as i64 + 1).into(),
                ],
            )
            .await?;
        #[derive(Deserialize)]
        struct Page {
            scope_generation: i64,
            catalogue_generation: i64,
            library_revision: i64,
            records: Vec<SourceCatalogueRecord>,
        }
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let mut page: Page = serde_json::from_str(row).map_err(|_| invalid())?;
        if page.scope_generation <= 0
            || page.catalogue_generation <= 0
            || page.library_revision <= 0
            || page.records.len() > r.limit + 1
        {
            return Err(invalid());
        }
        for record in &page.records {
            record_ok(record)?;
            if record.item.library_id != r.library_id {
                return Err(invalid());
            }
        }
        let has_more = page.records.len() > r.limit;
        page.records.truncate(r.limit);
        Ok(Some(SourceCataloguePage {
            records: page.records,
            counters: CatalogueCounters {
                scope_generation: page.scope_generation,
                catalogue_generation: page.catalogue_generation,
                library_revision: page.library_revision,
            },
            has_more,
        }))
    }
    async fn source_catalogue_batch(
        &self,
        hash: &str,
        grant: Uuid,
        batch: MetadataBatch,
    ) -> Result<Option<Vec<SourceBatchEntry>>, StoreError> {
        if !is_hash(hash) {
            return Err(invalid());
        }
        batch.validate().map_err(|_| invalid())?;
        ready(self).await?;
        let sql=format!("WITH allowed AS (SELECT id FROM sharing_exports WHERE token_hash=$1 AND id=$2 AND state='active' AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)), requested AS (SELECT cast(key AS INTEGER) AS ordinal,value AS item_id FROM json_each($3)), results AS (SELECT json_object('item_id',r.item_id,'record',CASE WHEN i.id IS NULL THEN NULL ELSE {RECORD} END) AS result FROM requested r LEFT JOIN items i ON i.id=cast(r.item_id AS INTEGER) AND i.kind IN ('movie','show','season','episode') AND EXISTS(SELECT 1 FROM sharing_export_libraries x JOIN libraries l ON l.id=x.library_id WHERE x.grant_id=$2 AND x.library_id=i.library_id AND l.kind IN ('movies','shows')) ORDER BY r.ordinal) SELECT json_object('records',json(coalesce((SELECT json_group_array(json(result)) FROM results),'[]'))) AS payload FROM allowed");
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    hash.to_owned().into(),
                    grant.into(),
                    json(&batch.item_ids)?.into(),
                ],
            )
            .await?;
        #[derive(Deserialize)]
        struct Entry {
            item_id: SourceId,
            record: Option<SourceCatalogueRecord>,
        }
        #[derive(Deserialize)]
        struct Batch {
            records: Vec<Entry>,
        }
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let rows: Batch = serde_json::from_str(row).map_err(|_| invalid())?;
        if rows.records.len() != batch.item_ids.len() {
            return Err(invalid());
        }
        rows.records
            .into_iter()
            .zip(batch.item_ids)
            .map(|(entry, expected)| {
                if entry.item_id != expected {
                    return Err(invalid());
                }
                if let Some(record) = &entry.record {
                    record_ok(record)?;
                    if record.item.item_id != expected {
                        return Err(invalid());
                    }
                }
                Ok(SourceBatchEntry {
                    item_id: entry.item_id,
                    record: entry.record,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }
    async fn source_catalogue_libraries(
        &self,
        hash: &str,
        grant: Uuid,
    ) -> Result<Option<Vec<SourceLibrary>>, StoreError> {
        if !is_hash(hash) {
            return Err(invalid());
        }
        ready(self).await?;
        let sql="SELECT json_object('libraries',json(coalesce((SELECT json_group_array(json_object('library_id',cast(l.id AS TEXT),'name',l.name,'kind',l.kind,'anime',json(CASE WHEN l.anime=1 THEN 'true' ELSE 'false' END))) FROM sharing_export_libraries x JOIN libraries l ON l.id=x.library_id WHERE x.grant_id=e.id AND l.kind IN ('movies','shows')),'[]'))) AS payload FROM sharing_exports e WHERE e.token_hash=$1 AND e.id=$2 AND e.state='active' AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)";
        let rows = self
            .sharing_read(sql, vec![hash.to_owned().into(), grant.into()])
            .await?;
        #[derive(Deserialize)]
        struct Libraries {
            libraries: Vec<SourceLibrary>,
        }
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let result: Libraries = serde_json::from_str(row).map_err(|_| invalid())?;
        if result.libraries.len() > 64
            || result
                .libraries
                .iter()
                .any(|l| l.name.len() > 256 || !matches!(l.kind.as_str(), "movies" | "shows"))
        {
            return Err(invalid());
        }
        Ok(Some(result.libraries))
    }
}

/// Complete fixed DDL statements, including each trigger's internal semicolons.
/// Used by candidate qualification and the eventual coordinated migration.
pub fn candidate_statements() -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    for line in CANDIDATE_SCHEMA.lines() {
        if line.starts_with("CREATE ") && !current.trim().is_empty() {
            statements.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        statements.push(current);
    }
    statements
}

pub fn candidate_item_identity_statements() -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut trigger = false;
    for line in CANDIDATE_ITEM_IDENTITY_SCHEMA.lines() {
        if line.starts_with("CREATE TRIGGER") {
            trigger = true;
        }
        current.push_str(line);
        current.push('\n');
        if (trigger && line.trim() == "END;") || (!trigger && line.trim_end().ends_with(';')) {
            statements.push(std::mem::take(&mut current));
            trigger = false;
        }
    }
    assert!(current.trim().is_empty());
    statements
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{ItemKind, LibraryKind, MetadataPatch, NewItem, NewLibrary},
        sharing::*,
        store::{LibraryStore, MediaStore, SharingStore, SqliteStore},
    };
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    async fn setup(s: &SqliteStore) -> (i64, i64, Uuid) {
        let mut ids = Vec::new();
        for name in ["Shared", "Private"] {
            ids.push(
                s.create_library(&NewLibrary {
                    name: name.into(),
                    kind: LibraryKind::Movies,
                    paths: vec![PathBuf::from("/synthetic")],
                    anime: false,
                })
                .await
                .expect("source fixture library")
                .id,
            );
        }
        let invitation = Uuid::new_v4();
        let grant = Uuid::new_v4();
        s.create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![ids[0]],
            created_at_ms: 1000,
            expires_at_ms: 2000,
        })
        .await
        .expect("invitation");
        s.claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: Uuid::new_v4(),
            recipient_name: "Synthetic recipient".into(),
            credential_hash: "b".repeat(64),
            now_ms: 1001,
        })
        .await
        .expect("claim");
        s.approve_share(grant, 1, 1002).await.expect("approve");
        (ids[0], ids[1], grant)
    }

    struct ImportTransition<'a> {
        store: &'a SqliteStore,
        armed: AtomicBool,
    }
    #[async_trait]
    impl Backend for ImportTransition<'_> {
        fn sharing_is_replicated(&self) -> bool {
            self.store.sharing_is_replicated()
        }
        async fn sharing_purpose_archive_rows(&self) -> Result<Vec<String>, StoreError> {
            self.store.sharing_purpose_archive_rows().await
        }
        async fn sharing_file_locator_key_rows(&self) -> Result<Vec<String>, StoreError> {
            self.store.sharing_file_locator_key_rows().await
        }
        async fn sharing_revision_key_rows(&self) -> Result<Vec<String>, StoreError> {
            self.store.sharing_revision_key_rows().await
        }
        async fn sharing_read(
            &self,
            sql: &str,
            values: Vec<super::super::sharing::Value>,
        ) -> Result<Vec<String>, StoreError> {
            if (sql.starts_with("WITH allowed AS")
                || sql.starts_with("SELECT json_object('libraries'"))
                && self.armed.swap(false, Ordering::SeqCst)
            {
                self.store
                    .sharing_txn(vec![(
                        "UPDATE item_identity_watermark SET importing=1 WHERE singleton=1".into(),
                        vec![],
                    )])
                    .await?;
            }
            self.store.sharing_read(sql, values).await
        }
        async fn sharing_txn(
            &self,
            statements: Vec<super::super::sharing::Statement>,
        ) -> Result<Vec<usize>, StoreError> {
            self.store.sharing_txn(statements).await
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_actual_query_refuses_import_started_after_readiness() {
        let store = SqliteStore::open_in_memory().expect("source fixture");
        let (library, _, grant) = setup(&store).await;
        install(&store).await;
        let item = store
            .insert_item(&NewItem {
                library_id: library,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Scoped item".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("source item");
        for operation in 0..3 {
            store
                .sharing_txn(vec![(
                    "UPDATE item_identity_watermark SET importing=0 WHERE singleton=1".into(),
                    vec![],
                )])
                .await
                .expect("ready layout");
            let racing = ImportTransition {
                store: &store,
                armed: AtomicBool::new(true),
            };
            let refused = match operation {
                0 => racing
                    .source_catalogue_page(CataloguePageRequest {
                        credential_hash: "b".repeat(64),
                        grant_id: grant,
                        library_id: SourceId::parse(&library.to_string()).expect("library ID"),
                        parent_id: None,
                        query: String::new(),
                        boundary: None,
                        limit: 60,
                    })
                    .await
                    .expect("page")
                    .is_none(),
                1 => racing
                    .source_catalogue_batch(
                        &"b".repeat(64),
                        grant,
                        MetadataBatch {
                            item_ids: vec![SourceId::parse(&item.to_string()).expect("item ID")],
                        },
                    )
                    .await
                    .expect("batch")
                    .is_none(),
                _ => racing
                    .source_catalogue_libraries(&"b".repeat(64), grant)
                    .await
                    .expect("libraries")
                    .is_none(),
            };
            assert!(
                refused,
                "operation {operation} exposed importing catalogue after readiness"
            );
            assert!(
                !racing.armed.load(Ordering::SeqCst),
                "fixture must interpose before actual query"
            );
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_item_identity_does_not_recycle_after_deletion() {
        let s = SqliteStore::open_in_memory().expect("source fixture");
        let (library, _, _) = setup(&s).await;
        install(&s).await;
        let mut item = NewItem {
            library_id: library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Removed movie".into(),
            year: None,
            season_number: None,
            episode_number: None,
        };
        let old = s.insert_item(&item).await.expect("original item");
        s.sharing_txn(vec![(
            "DELETE FROM items WHERE id=$1".into(),
            vec![old.into()],
        )])
        .await
        .expect("removed item");
        item.title = "Unrelated new movie".into();
        let new = s.insert_item(&item).await.expect("replacement item");
        assert!(new > old, "source identity recycled: old={old}, new={new}");
        assert!(s.sharing_txn(vec![("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES($1,$2,'movie','Old explicit writer','old')".into(),vec![old.into(),library.into()])]).await.is_err(),"old explicit writer must not recreate deleted identity");
        assert!(s.sharing_txn(vec![("INSERT INTO items(library_id,kind,title,sort_title) VALUES($1,'movie','Old implicit writer','old')".into(),vec![library.into()])]).await.is_err(),"old implicit writer must be fenced");
        let before = new;
        item.library_id = i64::MAX;
        assert!(s.insert_item(&item).await.is_err());
        item.library_id = library;
        let after = s.insert_item(&item).await.expect("after failed FK insert");
        assert_eq!(after, before + 1, "failed insert must roll allocation back");
    }
    #[tokio::test]
    async fn sharing_catalogue_item_import_mode_observes_parent_first_ids_and_refuses_live_writers()
    {
        let s = SqliteStore::open_in_memory().expect("source fixture");
        let (library, _, _) = setup(&s).await;
        s.sharing_txn(
            candidate_item_identity_statements()
                .into_iter()
                .map(|sql| (sql, vec![]))
                .collect(),
        )
        .await
        .expect("identity candidate");
        s.sharing_txn(vec![(
            "UPDATE item_identity_watermark SET importing=1 WHERE singleton=1 AND high_water=0"
                .into(),
            vec![],
        )])
        .await
        .expect("authorized fresh-target import mode");
        let item = NewItem {
            library_id: library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Live writer".into(),
            year: None,
            season_number: None,
            episode_number: None,
        };
        assert!(
            s.insert_item(&item).await.is_err(),
            "normal writer cannot publish during import"
        );
        s.sharing_txn(vec![("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(10,$1,'show','Parent','parent'),(3,$1,'episode','Child','child')".into(),vec![library.into()]),("UPDATE item_identity_watermark SET importing=0 WHERE singleton=1".into(),vec![])]).await.expect("preserved explicit IDs");
        assert_eq!(
            s.insert_item(&item).await.expect("post-import allocation"),
            11
        );
    }
    fn request(
        library: i64,
        grant: Uuid,
        boundary: Option<CatalogueBoundary>,
    ) -> CataloguePageRequest {
        CataloguePageRequest {
            credential_hash: "b".repeat(64),
            grant_id: grant,
            library_id: SourceId::parse(&library.to_string()).expect("library identity"),
            parent_id: None,
            query: String::new(),
            boundary,
            limit: 60,
        }
    }
    async fn install(s: &SqliteStore) {
        s.sharing_txn(
            candidate_statements()
                .into_iter()
                .chain(candidate_item_identity_statements())
                .map(|sql| (sql, vec![]))
                .collect(),
        )
        .await
        .expect("atomic candidate order maintenance");
    }
    #[tokio::test]
    async fn sharing_catalogue_source_pages_complete_during_continuous_metadata_writes() {
        let directory = tempfile::tempdir().expect("source fixture directory");
        for s in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&directory.path().join("source.db")).expect("disk"),
        ] {
            let (library, _, grant) = setup(&s).await;
            assert!(
                s.source_catalogue_page(request(library, grant, None))
                    .await
                    .is_err(),
                "missing maintenance must not serve an apparently stable cursor"
            );
            install(&s).await;
            let first = 9_007_199_254_740_993_i64;
            for group in 0..50 {
                let mut statements = Vec::new();
                for n in group * 100..(group + 1) * 100 {
                    statements.push(("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES($1,$2,'movie',$3,$4)".into(),vec![(first+n).into(),library.into(),format!("Movie {n:05}").into(),format!("{n:05}").into()]));
                }
                s.sharing_txn(statements)
                    .await
                    .expect("large finite fixture");
            }
            let stop = AtomicBool::new(false);
            let writes = AtomicUsize::new(0);
            let writer = async {
                while !stop.load(Ordering::SeqCst) {
                    let n = writes.fetch_add(1, Ordering::SeqCst) as i64;
                    s.apply_metadata(
                        first + n % 5000,
                        &MetadataPatch {
                            overview: Some(format!("Continuous metadata write {n}")),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("actual metadata writer");
                    tokio::task::yield_now().await;
                }
            };
            let browse = async {
                let mut boundary = None;
                let mut count = 0;
                let mut revision = None;
                loop {
                    let page = s
                        .source_catalogue_page(request(library, grant, boundary))
                        .await
                        .expect("consistent source page")
                        .expect("current authority");
                    if let Some(old) = revision {
                        assert_eq!(
                            old, page.counters.library_revision,
                            "metadata writes cannot invalidate order"
                        );
                    }
                    revision = Some(page.counters.library_revision);
                    for row in &page.records {
                        assert_eq!(row.item.item_id.as_str(), (first + count).to_string());
                        count += 1;
                    }
                    if !page.has_more {
                        break;
                    }
                    let last = page.records.last().expect("nonempty advancing page");
                    boundary = Some(CatalogueBoundary {
                        sort_key: last.boundary_sort_key.clone(),
                        item_id: last.item.item_id.clone(),
                    });
                }
                stop.store(true, Ordering::SeqCst);
                assert_eq!(count, 5000);
            };
            tokio::join!(writer, browse);
            assert!(
                writes.load(Ordering::SeqCst) > 0,
                "the synthetic scan must actually write during paging"
            );
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_source_keyset_survives_boundary_deletion_and_moves_with_exact_huge_ties(
    ) {
        let s = SqliteStore::open_in_memory().expect("source fixture");
        let (library, _, grant) = setup(&s).await;
        install(&s).await;
        // Four equal sort keys on adjacent IDs above 2^53: an f64 or text
        // tie-breaker would collapse or misorder them.
        let first = 9_007_199_254_740_993_i64;
        let mut statements = Vec::new();
        for n in 0..10_i64 {
            let sort = if n < 4 {
                "same".to_owned()
            } else {
                format!("t{n}")
            };
            statements.push((
                "INSERT INTO items(id,library_id,kind,title,sort_title) VALUES($1,$2,'movie',$3,$4)"
                    .into(),
                vec![(first + n).into(), library.into(), format!("Movie {n}").into(), sort.into()],
            ));
        }
        s.sharing_txn(statements).await.expect("finite fixture");
        let ids = |page: &SourceCataloguePage| {
            page.records
                .iter()
                .map(|r| {
                    r.item
                        .item_id
                        .as_str()
                        .parse::<i64>()
                        .expect("canonical ID")
                        - first
                })
                .collect::<Vec<_>>()
        };
        let mut r = request(library, grant, None);
        r.limit = 3;
        let page = s
            .source_catalogue_page(r.clone())
            .await
            .expect("first page")
            .expect("authorized");
        assert_eq!(ids(&page), [0, 1, 2]);
        assert!(page.has_more);
        let last = page.records.last().expect("boundary");
        let boundary = CatalogueBoundary {
            sort_key: last.boundary_sort_key.clone(),
            item_id: last.item.item_id.clone(),
        };
        // Delete the boundary item itself, move an already-seen item ahead of
        // the boundary and an unseen item behind it.
        s.sharing_txn(vec![
            (
                "DELETE FROM items WHERE id=$1".into(),
                vec![(first + 2).into()],
            ),
            (
                "UPDATE items SET sort_title=$1 WHERE id=$2".into(),
                vec!["t5x".to_owned().into(), first.into()],
            ),
            (
                "UPDATE items SET sort_title=$1 WHERE id=$2".into(),
                vec!["a".to_owned().into(), (first + 9).into()],
            ),
        ])
        .await
        .expect("membership and order mutations");
        r.boundary = Some(boundary);
        r.limit = 60;
        let next = s
            .source_catalogue_page(r.clone())
            .await
            .expect("resume from the signed value boundary")
            .expect("authorized");
        assert!(next.counters.library_revision > page.counters.library_revision);
        // The deleted boundary does not stall the scan; the moved-ahead item
        // is sent again (B suppresses it by full reference); the moved-behind
        // item waits for the next open. Nothing unchanged is skipped.
        assert_eq!(ids(&next), [3, 4, 5, 0, 6, 7, 8]);
        assert!(!next.has_more);
        let mut seen = crate::sharing_catalogue::BrowseSeen::default();
        let reference = |id: &SourceId| crate::sharing_catalogue::SharedReference {
            import_id: Uuid::from_u128(1),
            server_id: Uuid::from_u128(2),
            catalogue_epoch: Uuid::from_u128(3),
            library_id: SourceId::parse(&library.to_string()).expect("library"),
            item_id: id.clone(),
        };
        let displayed = page
            .records
            .iter()
            .chain(&next.records)
            .filter(|r| seen.insert(reference(&r.item.item_id)))
            .count();
        assert_eq!(displayed, 9, "the moved-ahead repeat is displayed once");
        r.boundary = None;
        let reopened = s
            .source_catalogue_page(r.clone())
            .await
            .expect("fresh open")
            .expect("authorized");
        assert_eq!(ids(&reopened), [9, 1, 3, 4, 5, 0, 6, 7, 8]);
        // The top of the identity space: ties at i64::MAX-1 and i64::MAX keep
        // exact integer order and the boundary at i64::MAX neither wraps nor
        // repeats.
        s.sharing_txn(vec![(
            "INSERT INTO items(id,library_id,kind,title,sort_title) VALUES($1,$3,'movie','Top 1','same'),($2,$3,'movie','Top 2','same')".into(),
            vec![(i64::MAX - 1).into(), i64::MAX.into(), library.into()],
        )])
        .await
        .expect("maximum identities");
        let mut top = String::from("9223372036854775806");
        for expected in ["9223372036854775807", &(first + 4).to_string()] {
            r.boundary = Some(CatalogueBoundary {
                sort_key: "same".into(),
                item_id: SourceId::parse(&top).expect("exact maximum boundary"),
            });
            r.limit = 1;
            let page = s
                .source_catalogue_page(r.clone())
                .await
                .expect("maximum boundary page")
                .expect("authorized");
            assert_eq!(page.records.len(), 1);
            assert_eq!(page.records[0].item.item_id.as_str(), expected);
            top = expected.to_owned();
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_source_revalidates_batch_scope_and_numeric_child_order() {
        let s = SqliteStore::open_in_memory().expect("source fixture");
        let (library, private, grant) = setup(&s).await;
        install(&s).await;
        let mut item = NewItem {
            library_id: library,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Shared show".into(),
            year: None,
            season_number: None,
            episode_number: None,
        };
        let parent = s.insert_item(&item).await.expect("show");
        item.parent_id = Some(parent);
        item.kind = ItemKind::Season;
        item.title = "Season".into();
        item.season_number = Some(1);
        let season = s.insert_item(&item).await.expect("season");
        item.parent_id = Some(season);
        item.kind = ItemKind::Episode;
        let mut episodes = Vec::new();
        for number in [10, 2, 1] {
            item.episode_number = Some(number);
            item.title = format!("Episode {number}");
            episodes.push(s.insert_item(&item).await.expect("episode"));
        }
        item.library_id = private;
        item.parent_id = None;
        item.kind = ItemKind::Movie;
        item.title = "Private title".into();
        let denied = s.insert_item(&item).await.expect("private movie");
        let batch = MetadataBatch {
            item_ids: vec![
                SourceId::parse(&episodes[0].to_string()).expect("ID"),
                SourceId::parse(&denied.to_string()).expect("ID"),
            ],
        };
        let results = s
            .source_catalogue_batch(&"b".repeat(64), grant, batch)
            .await
            .expect("bounded batch")
            .expect("active grant");
        assert!(results[0].record.is_some());
        assert!(
            results[1].record.is_none(),
            "out of scope must expose no title or paths"
        );
        let mut r = request(library, grant, None);
        r.parent_id = Some(SourceId::parse(&season.to_string()).expect("parent ID"));
        let page = s
            .source_catalogue_page(r.clone())
            .await
            .expect("children")
            .expect("assigned scope");
        assert_eq!(
            page.records
                .iter()
                .map(|x| x.item.episode_number)
                .collect::<Vec<_>>(),
            [Some(1), Some(2), Some(10)]
        );
        let old = page.counters.library_revision;
        s.sharing_txn(vec![(
            "UPDATE items SET sort_title='zzz' WHERE id=$1".into(),
            vec![episodes[0].into()],
        )])
        .await
        .expect("sort move");
        assert!(
            s.source_catalogue_page(r.clone())
                .await
                .expect("live page")
                .expect("scope")
                .counters
                .library_revision
                > old
        );
        s.share_scope(grant, 2, vec![], 1010)
            .await
            .expect("remove scope");
        assert!(s
            .source_catalogue_page(r)
            .await
            .expect("revalidate removed library")
            .is_none());
        assert!(s
            .source_catalogue_batch(
                &"b".repeat(64),
                grant,
                MetadataBatch {
                    item_ids: vec![SourceId::parse(&parent.to_string()).expect("ID")]
                }
            )
            .await
            .expect("scope denied batch")
            .expect("grant still active")[0]
            .record
            .is_none());
    }
}
