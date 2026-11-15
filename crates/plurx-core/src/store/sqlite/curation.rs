//! Tags, saved rows, Home layouts and grants for the single-node SQLite
//! backend. The statements are `curation_sql`'s, shared with hiqlite.

use async_trait::async_trait;
use rusqlite::types::Value;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::curation::{
    Grant, HomeLayout, IdPage, NewTag, RowSpec, RuleQuery, SavedRow, Suggestion, SuggestionPage,
    Tag, TagEdit, TagSource,
};
use crate::error::StoreError;
use crate::store::curation_sql::{self as q, Arg};
use crate::store::CurationStore;

fn values(args: Vec<Arg>) -> Vec<Value> {
    args.into_iter()
        .map(|arg| match arg {
            Arg::Int(v) => Value::Integer(v),
            Arg::Text(v) => Value::Text(v),
        })
        .collect()
}

fn tag_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    let terms: String = row.get(4)?;
    Ok(Tag {
        id: row.get(0)?,
        name: row.get(1)?,
        owner_user_id: row.get(2)?,
        color: row.get(3)?,
        suggest_terms: serde_json::from_str(&terms).unwrap_or_default(),
        archived_at_ms: row.get(5)?,
        created_by: row.get(6)?,
        created_at_ms: row.get(7)?,
        item_count: row.get(8)?,
    })
}

fn row_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedRow> {
    let spec: String = row.get(3)?;
    Ok(SavedRow {
        id: row.get(0)?,
        owner_user_id: row.get(1)?,
        title: row.get(2)?,
        spec: serde_json::from_str::<RowSpec>(&spec).unwrap_or_default(),
        created_by: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn ids_json(ids: &[i64]) -> String {
    serde_json::to_string(ids).expect("ids serialize")
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| StoreError::Task(format!("encode: {error}")))
}

#[async_trait]
impl CurationStore for SqliteStore {
    async fn user_grants(&self, user_id: i64) -> Result<Vec<Grant>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(q::USER_GRANTS)?;
            let names = statement
                .query_map(params![user_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(names.iter().filter_map(|n| Grant::parse(n)).collect())
        })
        .await
    }

    async fn set_user_grant(
        &self,
        user_id: i64,
        grant: Grant,
        granted: bool,
        by: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            if granted {
                conn.execute(q::GRANT, params![user_id, grant.as_str(), by, at_ms])?;
            } else {
                conn.execute(q::REVOKE, params![user_id, grant.as_str()])?;
            }
            Ok(())
        })
        .await
    }

    async fn visible_tags(&self, viewer: i64) -> Result<Vec<Tag>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(q::VISIBLE_TAGS)?;
            let tags = statement
                .query_map(params![viewer], tag_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(tags)
        })
        .await
    }

    async fn curation_tag(&self, id: i64) -> Result<Option<Tag>, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(q::TAG_BY_ID, params![id], tag_from_row)
                .optional()?)
        })
        .await
    }

    async fn create_tag(&self, tag: &NewTag, at_ms: i64) -> Result<Option<Tag>, StoreError> {
        let tag = tag.clone();
        self.with_conn(move |conn| {
            let id: Option<i64> = conn
                .query_row(
                    q::INSERT_TAG,
                    params![
                        tag.name,
                        crate::curation::tag_key(&tag.name),
                        tag.owner_user_id,
                        tag.color,
                        tag.created_by,
                        at_ms
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            match id {
                None => Ok(None),
                Some(id) => Ok(conn
                    .query_row(q::TAG_BY_ID, params![id], tag_from_row)
                    .optional()?),
            }
        })
        .await
    }

    async fn update_tag(&self, id: i64, edit: &TagEdit) -> Result<Option<Tag>, StoreError> {
        let edit = edit.clone();
        let terms = edit.suggest_terms.as_ref().map(encode).transpose()?;
        self.with_conn(move |conn| {
            let key = edit.name.as_deref().map(crate::curation::tag_key);
            conn.execute(
                q::UPDATE_TAG,
                params![
                    edit.name,
                    key,
                    edit.color,
                    terms,
                    edit.archived_at_ms.is_some(),
                    edit.archived_at_ms.flatten(),
                    id
                ],
            )?;
            Ok(conn
                .query_row(q::TAG_BY_ID, params![id], tag_from_row)
                .optional()?)
        })
        .await
    }

    async fn delete_tag(&self, id: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| Ok(conn.execute(q::DELETE_TAG, params![id])? > 0))
            .await
    }

    async fn merge_tags(&self, from: i64, into: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(q::MERGE_MEMBERS, params![into, from])?;
            tx.execute(q::MERGE_REJECTIONS, params![into, from])?;
            let deleted = tx.execute(q::DELETE_TAG, params![from])? > 0;
            tx.commit()?;
            Ok(deleted)
        })
        .await
    }

    async fn item_tag_ids(&self, viewer: i64, item_id: i64) -> Result<Vec<i64>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(q::ITEM_TAG_IDS)?;
            let ids = statement
                .query_map(params![item_id, viewer], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ids)
        })
        .await
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
        let add = ids_json(add);
        let remove = ids_json(remove);
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(q::REMOVE_MEMBERS, params![tag_id, remove])?;
            tx.execute(
                q::ADD_MEMBERS,
                params![tag_id, by, at_ms, source.as_str(), add],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn rule_items(
        &self,
        query: &RuleQuery<'_>,
        offset: i64,
        limit: i64,
    ) -> Result<IdPage, StoreError> {
        let (count_sql, count_args) = q::rule_sql(query, None);
        let (page_sql, page_args) = q::rule_sql(query, Some((offset, limit)));
        self.with_read(move |conn| {
            super::trace_statement("rule_items.count", &count_sql);
            let total: i64 = conn.query_row(
                &count_sql,
                rusqlite::params_from_iter(values(count_args)),
                |row| row.get(0),
            )?;
            super::trace_statement("rule_items.page", &page_sql);
            let mut statement = conn.prepare(&page_sql)?;
            let item_ids = statement
                .query_map(rusqlite::params_from_iter(values(page_args)), |row| {
                    row.get(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(IdPage { item_ids, total })
        })
        .await
    }

    async fn tag_suggestions(
        &self,
        tag_id: i64,
        offset: i64,
        limit: i64,
    ) -> Result<SuggestionPage, StoreError> {
        self.with_read(move |conn| {
            let total: i64 =
                conn.query_row(&q::suggestion_count_sql(), params![tag_id], |row| {
                    row.get(0)
                })?;
            let mut statement = conn.prepare(&q::suggestion_page_sql())?;
            let suggestions = statement
                .query_map(params![tag_id, limit, offset], |row| {
                    let matched: String = row.get(1)?;
                    Ok(Suggestion {
                        item_id: row.get(0)?,
                        matched: serde_json::from_str(&matched).unwrap_or_default(),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SuggestionPage { suggestions, total })
        })
        .await
    }

    async fn set_suggestion_rejections(
        &self,
        tag_id: i64,
        reject: &[i64],
        restore: &[i64],
        by: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let reject = ids_json(reject);
        let restore = ids_json(restore);
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(q::UNREJECT, params![tag_id, restore])?;
            tx.execute(q::REJECT, params![tag_id, by, at_ms, reject])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn rejected_items(
        &self,
        tag_id: i64,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(q::REJECTED_ITEMS)?;
            let ids = statement
                .query_map(params![tag_id, limit, offset], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ids)
        })
        .await
    }

    async fn visible_rows(&self, viewer: i64) -> Result<Vec<SavedRow>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(q::VISIBLE_ROWS)?;
            let rows = statement
                .query_map(params![viewer], row_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    async fn curation_row(&self, id: i64) -> Result<Option<SavedRow>, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(q::ROW_BY_ID, params![id], row_from_row)
                .optional()?)
        })
        .await
    }

    async fn rows_naming_tag(&self, tag_id: i64) -> Result<Vec<SavedRow>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(q::ROWS_NAMING_TAG)?;
            let rows = statement
                .query_map(params![tag_id], row_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    async fn create_row(
        &self,
        owner_user_id: Option<i64>,
        title: &str,
        spec: &RowSpec,
        by: i64,
        at_ms: i64,
    ) -> Result<SavedRow, StoreError> {
        let title = title.to_owned();
        let spec = encode(spec)?;
        self.with_conn(move |conn| {
            let id: i64 = conn.query_row(
                q::INSERT_ROW,
                params![owner_user_id, title, spec, by, at_ms],
                |row| row.get(0),
            )?;
            Ok(conn.query_row(q::ROW_BY_ID, params![id], row_from_row)?)
        })
        .await
    }

    async fn update_row(
        &self,
        id: i64,
        title: &str,
        spec: &RowSpec,
        at_ms: i64,
    ) -> Result<bool, StoreError> {
        let title = title.to_owned();
        let spec = encode(spec)?;
        self.with_conn(move |conn| {
            Ok(conn.execute(q::UPDATE_ROW, params![title, spec, at_ms, id])? > 0)
        })
        .await
    }

    async fn delete_row(&self, id: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| Ok(conn.execute(q::DELETE_ROW, params![id])? > 0))
            .await
    }

    async fn home_layout(&self, user_id: i64) -> Result<Option<HomeLayout>, StoreError> {
        self.with_conn(move |conn| {
            let layout: Option<String> = conn
                .query_row(q::HOME_LAYOUT, params![user_id], |row| row.get(0))
                .optional()?;
            Ok(layout.and_then(|l| serde_json::from_str(&l).ok()))
        })
        .await
    }

    async fn put_home_layout(
        &self,
        user_id: i64,
        layout: Option<&HomeLayout>,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let layout = layout.map(encode).transpose()?;
        self.with_conn(move |conn| {
            match layout {
                Some(layout) => {
                    conn.execute(q::PUT_HOME_LAYOUT, params![user_id, layout, at_ms])?
                }
                None => conn.execute(q::CLEAR_HOME_LAYOUT, params![user_id])?,
            };
            Ok(())
        })
        .await
    }
}
