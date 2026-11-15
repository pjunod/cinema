//! Tags, saved rows, Home layouts and grants for the replicated hiqlite
//! backend. The statements are `curation_sql`'s, shared with SQLite.
//!
//! Reads are local replica reads (`query_map`): curation is a person's own
//! taste, the write path already holds a read-your-writes floor for the
//! tables it touched, and Home must not cost a quorum round trip per row.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::{Param, Row};

use super::curation_sql::{self as q, Arg};
use super::hiqlite::{database_error, HiqliteAuthStore};
use super::CurationStore;
use crate::curation::{
    Grant, HomeLayout, IdPage, NewTag, RowSpec, RuleQuery, SavedRow, Suggestion, SuggestionPage,
    Tag, TagEdit, TagSource,
};
use crate::error::StoreError;

struct TagRow(Tag);

impl From<&mut Row<'_>> for TagRow {
    fn from(row: &mut Row<'_>) -> Self {
        let terms: String = row.get("suggest_terms");
        Self(Tag {
            id: row.get("id"),
            name: row.get("name"),
            owner_user_id: row.get("owner_user_id"),
            color: row.get("color"),
            suggest_terms: serde_json::from_str(&terms).unwrap_or_default(),
            archived_at_ms: row.get("archived_at_ms"),
            created_by: row.get("created_by"),
            created_at_ms: row.get("created_at_ms"),
            item_count: row.get("item_count"),
        })
    }
}

struct SavedRowRow(SavedRow);

impl From<&mut Row<'_>> for SavedRowRow {
    fn from(row: &mut Row<'_>) -> Self {
        let spec: String = row.get("spec");
        Self(SavedRow {
            id: row.get("id"),
            owner_user_id: row.get("owner_user_id"),
            title: row.get("title"),
            spec: serde_json::from_str::<RowSpec>(&spec).unwrap_or_default(),
            created_by: row.get("created_by"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
}

struct IdRow(i64);

impl From<&mut Row<'_>> for IdRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("id"))
    }
}

struct ItemIdRow(i64);

impl From<&mut Row<'_>> for ItemIdRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("item_id"))
    }
}

struct TagIdRow(i64);

impl From<&mut Row<'_>> for TagIdRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("tag_id"))
    }
}

struct GrantRow(String);

impl From<&mut Row<'_>> for GrantRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("grant_name"))
    }
}

struct LayoutRow(String);

impl From<&mut Row<'_>> for LayoutRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("layout"))
    }
}

struct CountRow(i64);

impl From<&mut Row<'_>> for CountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("value"))
    }
}

struct SuggestionRow(Suggestion);

impl From<&mut Row<'_>> for SuggestionRow {
    fn from(row: &mut Row<'_>) -> Self {
        let matched: String = row.get("matched");
        Self(Suggestion {
            item_id: row.get("item_id"),
            matched: serde_json::from_str(&matched).unwrap_or_default(),
        })
    }
}

fn bind(args: Vec<Arg>) -> hiqlite::Params {
    args.into_iter()
        .map(|arg| match arg {
            Arg::Int(v) => Param::from(v),
            Arg::Text(v) => Param::from(v),
        })
        .collect()
}

fn ids_json(ids: &[i64]) -> String {
    serde_json::to_string(ids).expect("ids serialize")
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| StoreError::Task(format!("encode: {error}")))
}

fn settled(results: Vec<Result<usize, hiqlite::Error>>) -> Result<Vec<usize>, StoreError> {
    results
        .into_iter()
        .map(|result| result.map_err(database_error))
        .collect()
}

impl HiqliteAuthStore {
    async fn curation_tag_by_id(
        &self,
        method: &'static str,
        id: i64,
    ) -> Result<Option<Tag>, StoreError> {
        Ok(self
            .client()
            .query_map::<TagRow, _>(method, q::TAG_BY_ID, params!(id))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn curation_row_by_id(
        &self,
        method: &'static str,
        id: i64,
    ) -> Result<Option<SavedRow>, StoreError> {
        Ok(self
            .client()
            .query_map::<SavedRowRow, _>(method, q::ROW_BY_ID, params!(id))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map(|row| row.0))
    }
}

#[async_trait]
impl CurationStore for HiqliteAuthStore {
    async fn user_grants(&self, user_id: i64) -> Result<Vec<Grant>, StoreError> {
        Ok(self
            .client()
            .query_map::<GrantRow, _>("user_grants", q::USER_GRANTS, params!(user_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .filter_map(|row| Grant::parse(&row.0))
            .collect())
    }

    async fn set_user_grant(
        &self,
        user_id: i64,
        grant: Grant,
        granted: bool,
        by: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        if granted {
            self.client()
                .execute(
                    "set_user_grant",
                    q::GRANT,
                    params!(user_id, grant.as_str(), by, at_ms),
                )
                .await
                .map_err(database_error)?;
        } else {
            self.client()
                .execute(
                    "set_user_grant",
                    q::REVOKE,
                    params!(user_id, grant.as_str()),
                )
                .await
                .map_err(database_error)?;
        }
        Ok(())
    }

    async fn visible_tags(&self, viewer: i64) -> Result<Vec<Tag>, StoreError> {
        Ok(self
            .client()
            .query_map::<TagRow, _>("visible_tags", q::VISIBLE_TAGS, params!(viewer))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn curation_tag(&self, id: i64) -> Result<Option<Tag>, StoreError> {
        self.curation_tag_by_id("curation_tag", id).await
    }

    async fn create_tag(&self, tag: &NewTag, at_ms: i64) -> Result<Option<Tag>, StoreError> {
        let returned = self
            .client()
            .execute_returning_map::<_, IdRow>(
                "create_tag",
                q::INSERT_TAG,
                params!(
                    tag.name.clone(),
                    crate::curation::tag_key(&tag.name),
                    tag.owner_user_id,
                    tag.color.clone(),
                    tag.created_by,
                    at_ms
                ),
            )
            .await
            .map_err(database_error)?;
        let Some(row) = returned.into_iter().next() else {
            return Ok(None);
        };
        let id = row.map_err(database_error)?.0;
        self.curation_tag_by_id("create_tag", id).await
    }

    async fn update_tag(&self, id: i64, edit: &TagEdit) -> Result<Option<Tag>, StoreError> {
        let terms = edit.suggest_terms.as_ref().map(encode).transpose()?;
        self.client()
            .execute(
                "update_tag",
                q::UPDATE_TAG,
                params!(
                    edit.name.clone(),
                    edit.name.as_deref().map(crate::curation::tag_key),
                    edit.color.clone(),
                    terms,
                    i64::from(edit.archived_at_ms.is_some()),
                    edit.archived_at_ms.flatten(),
                    id
                ),
            )
            .await
            .map_err(database_error)?;
        self.curation_tag_by_id("update_tag", id).await
    }

    async fn delete_tag(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute("delete_tag", q::DELETE_TAG, params!(id))
            .await
            .map_err(database_error)?
            > 0)
    }

    async fn merge_tags(&self, from: i64, into: i64) -> Result<bool, StoreError> {
        let results = self
            .client()
            .txn(
                "merge_tags",
                [
                    (q::MERGE_MEMBERS, params!(into, from)),
                    (q::MERGE_REJECTIONS, params!(into, from)),
                    (q::DELETE_TAG, params!(from)),
                ],
            )
            .await?;
        Ok(settled(results)?.last().copied().unwrap_or(0) > 0)
    }

    async fn item_tag_ids(&self, viewer: i64, item_id: i64) -> Result<Vec<i64>, StoreError> {
        Ok(self
            .client()
            .query_map::<TagIdRow, _>("item_tag_ids", q::ITEM_TAG_IDS, params!(item_id, viewer))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn set_tag_members(
        &self,
        tag_id: i64,
        add: &[i64],
        remove: &[i64],
        by: i64,
        source: TagSource,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let results = self
            .client()
            .txn(
                "set_tag_members",
                [
                    (q::REMOVE_MEMBERS, params!(tag_id, ids_json(remove))),
                    (
                        q::ADD_MEMBERS,
                        params!(tag_id, by, at_ms, source.as_str(), ids_json(add)),
                    ),
                ],
            )
            .await?;
        settled(results)?;
        Ok(())
    }

    async fn rule_items(
        &self,
        query: &RuleQuery<'_>,
        offset: i64,
        limit: i64,
    ) -> Result<IdPage, StoreError> {
        let (count_sql, count_args) = q::rule_sql(query, None);
        let (page_sql, page_args) = q::rule_sql(query, Some((offset, limit)));
        super::hiqlite::trace_statement("rule_items.count", &count_sql);
        let total = self
            .client()
            .query_map::<CountRow, _>("rule_items", count_sql, bind(count_args))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map_or(0, |row| row.0);
        super::hiqlite::trace_statement("rule_items.page", &page_sql);
        let item_ids = self
            .client()
            .query_map::<ItemIdRow, _>("rule_items", page_sql, bind(page_args))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect();
        Ok(IdPage { item_ids, total })
    }

    async fn tag_suggestions(
        &self,
        tag_id: i64,
        offset: i64,
        limit: i64,
    ) -> Result<SuggestionPage, StoreError> {
        let total = self
            .client()
            .query_map::<CountRow, _>(
                "tag_suggestions",
                q::suggestion_count_sql(),
                params!(tag_id),
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map_or(0, |row| row.0);
        let suggestions = self
            .client()
            .query_map::<SuggestionRow, _>(
                "tag_suggestions",
                q::suggestion_page_sql(),
                params!(tag_id, limit, offset),
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect();
        Ok(SuggestionPage { suggestions, total })
    }

    async fn set_suggestion_rejections(
        &self,
        tag_id: i64,
        reject: &[i64],
        restore: &[i64],
        by: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let results = self
            .client()
            .txn(
                "set_suggestion_rejections",
                [
                    (q::UNREJECT, params!(tag_id, ids_json(restore))),
                    (q::REJECT, params!(tag_id, by, at_ms, ids_json(reject))),
                ],
            )
            .await?;
        settled(results)?;
        Ok(())
    }

    async fn rejected_items(
        &self,
        tag_id: i64,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, StoreError> {
        Ok(self
            .client()
            .query_map::<ItemIdRow, _>(
                "rejected_items",
                q::REJECTED_ITEMS,
                params!(tag_id, limit, offset),
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn visible_rows(&self, viewer: i64) -> Result<Vec<SavedRow>, StoreError> {
        Ok(self
            .client()
            .query_map::<SavedRowRow, _>("visible_rows", q::VISIBLE_ROWS, params!(viewer))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn curation_row(&self, id: i64) -> Result<Option<SavedRow>, StoreError> {
        self.curation_row_by_id("curation_row", id).await
    }

    async fn rows_naming_tag(&self, tag_id: i64) -> Result<Vec<SavedRow>, StoreError> {
        Ok(self
            .client()
            .query_map::<SavedRowRow, _>("rows_naming_tag", q::ROWS_NAMING_TAG, params!(tag_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn create_row(
        &self,
        owner_user_id: Option<i64>,
        title: &str,
        spec: &RowSpec,
        by: i64,
        at_ms: i64,
    ) -> Result<SavedRow, StoreError> {
        let returned = self
            .client()
            .execute_returning_map::<_, IdRow>(
                "create_row",
                q::INSERT_ROW,
                params!(owner_user_id, title.to_owned(), encode(spec)?, by, at_ms),
            )
            .await
            .map_err(database_error)?;
        let id = returned
            .into_iter()
            .next()
            .ok_or_else(|| StoreError::Database("row insert returned nothing".into()))?
            .map_err(database_error)?
            .0;
        self.curation_row_by_id("create_row", id)
            .await?
            .ok_or_else(|| StoreError::Database("created row vanished".into()))
    }

    async fn update_row(
        &self,
        id: i64,
        title: &str,
        spec: &RowSpec,
        at_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute(
                "update_row",
                q::UPDATE_ROW,
                params!(title.to_owned(), encode(spec)?, at_ms, id),
            )
            .await
            .map_err(database_error)?
            > 0)
    }

    async fn delete_row(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute("delete_row", q::DELETE_ROW, params!(id))
            .await
            .map_err(database_error)?
            > 0)
    }

    async fn home_layout(&self, user_id: i64) -> Result<Option<HomeLayout>, StoreError> {
        Ok(self
            .client()
            .query_map::<LayoutRow, _>("home_layout", q::HOME_LAYOUT, params!(user_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .and_then(|row| serde_json::from_str(&row.0).ok()))
    }

    async fn put_home_layout(
        &self,
        user_id: i64,
        layout: Option<&HomeLayout>,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        match layout {
            Some(layout) => {
                self.client()
                    .execute(
                        "put_home_layout",
                        q::PUT_HOME_LAYOUT,
                        params!(user_id, encode(layout)?, at_ms),
                    )
                    .await
                    .map_err(database_error)?;
            }
            None => {
                self.client()
                    .execute("put_home_layout", q::CLEAR_HOME_LAYOUT, params!(user_id))
                    .await
                    .map_err(database_error)?;
            }
        }
        Ok(())
    }
}
