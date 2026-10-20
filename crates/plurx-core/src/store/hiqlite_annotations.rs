//! Item ratings and per-person item lists for the replicated hiqlite backend.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::AnnotationStore;
use crate::annotations::{ItemList, ItemListFlags, ItemRatings, ListedItem, Rating, RatingSource};
use crate::error::StoreError;

struct FetchRow {
    fetched_at_ms: i64,
    omdb_consulted: i64,
}

impl From<&mut Row<'_>> for FetchRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            fetched_at_ms: row.get("fetched_at_ms"),
            omdb_consulted: row.get("omdb_consulted"),
        }
    }
}

struct RatingRow {
    source: String,
    value: f64,
    scale: i64,
    votes: Option<i64>,
}

impl From<&mut Row<'_>> for RatingRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            source: row.get("source"),
            value: row.get("value"),
            scale: row.get("scale"),
            votes: row.get("votes"),
        }
    }
}

struct ListRow {
    item_id: i64,
    added_at_ms: i64,
}

impl From<&mut Row<'_>> for ListRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            item_id: row.get("item_id"),
            added_at_ms: row.get("added_at_ms"),
        }
    }
}

struct ListNameRow {
    list: String,
    added_at_ms: i64,
}

impl From<&mut Row<'_>> for ListNameRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            list: row.get("list"),
            added_at_ms: row.get("added_at_ms"),
        }
    }
}

#[async_trait]
impl AnnotationStore for HiqliteAuthStore {
    async fn item_ratings(&self, item_id: i64) -> Result<Option<ItemRatings>, StoreError> {
        let sql = "SELECT fetched_at_ms, omdb_consulted FROM library_annotations_rating_fetches \
                   WHERE item_id = $1";
        validate_sql(sql)?;
        let Some(fetch) = self
            .client()
            // authority: a completed refresh must be visible before deciding whether to fetch again.
            .query_consistent_map::<FetchRow, _>(sql, params!(item_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let sql = "SELECT source, value, scale, votes FROM library_annotations_ratings \
                   WHERE item_id = $1";
        validate_sql(sql)?;
        let mut ratings = self
            .client()
            // authority: a caller must observe the committed ratings replacement it just wrote.
            .query_consistent_map::<RatingRow, _>(sql, params!(item_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .filter_map(|row| {
                Some(Rating {
                    source: RatingSource::parse(&row.source)?,
                    value: row.value,
                    scale: u8::try_from(row.scale).ok()?,
                    votes: row.votes,
                })
            })
            .collect::<Vec<_>>();
        ratings.sort_by_key(|rating| rating.source);
        Ok(Some(ItemRatings {
            fetched_at_ms: fetch.fetched_at_ms,
            omdb_consulted: fetch.omdb_consulted != 0,
            ratings,
        }))
    }

    async fn put_item_ratings(
        &self,
        item_id: i64,
        ratings: &ItemRatings,
    ) -> Result<(), StoreError> {
        let delete = "DELETE FROM library_annotations_ratings WHERE item_id = $1";
        let insert =
            "INSERT INTO library_annotations_ratings(item_id, source, value, scale, votes) \
                      SELECT $1, $2, $3, $4, $5 WHERE EXISTS (SELECT 1 FROM items WHERE id = $1)";
        let fetch = "INSERT INTO library_annotations_rating_fetches(item_id, fetched_at_ms, omdb_consulted) \
                     SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM items WHERE id = $1) \
                     ON CONFLICT(item_id) DO UPDATE SET fetched_at_ms = excluded.fetched_at_ms, \
                     omdb_consulted = excluded.omdb_consulted";
        for sql in [delete, insert, fetch] {
            validate_sql(sql)?;
        }
        let mut statements: Vec<(String, hiqlite::Params)> =
            vec![(delete.to_owned(), params!(item_id))];
        for rating in &ratings.ratings {
            statements.push((
                insert.to_owned(),
                params!(
                    item_id,
                    rating.source.as_str(),
                    rating.value,
                    i64::from(rating.scale),
                    rating.votes
                ),
            ));
        }
        statements.push((
            fetch.to_owned(),
            params!(
                item_id,
                ratings.fetched_at_ms,
                i64::from(ratings.omdb_consulted)
            ),
        ));
        for result in self
            .client()
            .txn(statements)
            .await
            .map_err(database_error)?
        {
            result.map_err(database_error)?;
        }
        Ok(())
    }

    async fn set_item_listed(
        &self,
        user_id: i64,
        item_id: i64,
        list: ItemList,
        listed: bool,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        if listed {
            let sql = "INSERT INTO library_annotations_lists(user_id, list, item_id, added_at_ms) \
                       SELECT $1, $2, $3, $4 WHERE EXISTS (SELECT 1 FROM items WHERE id = $3) \
                       ON CONFLICT(user_id, list, item_id) DO UPDATE SET added_at_ms = excluded.added_at_ms";
            validate_sql(sql)?;
            self.client()
                .execute(sql, params!(user_id, list.as_str(), item_id, at_ms))
                .await
                .map_err(database_error)?;
        } else {
            let sql = "DELETE FROM library_annotations_lists \
                       WHERE user_id = $1 AND list = $2 AND item_id = $3";
            validate_sql(sql)?;
            self.client()
                .execute(sql, params!(user_id, list.as_str(), item_id))
                .await
                .map_err(database_error)?;
        }
        Ok(())
    }

    async fn item_list_flags(
        &self,
        user_id: i64,
        item_id: i64,
    ) -> Result<ItemListFlags, StoreError> {
        let sql = "SELECT list, added_at_ms FROM library_annotations_lists WHERE user_id = $1 AND item_id = $2";
        validate_sql(sql)?;
        let mut flags = ItemListFlags::default();
        for row in self
            .client()
            // authority: a person's own list membership, read where they wrote it.
            .query_consistent_map::<ListNameRow, _>(sql, params!(user_id, item_id))
            .await
            .map_err(database_error)?
        {
            match ItemList::parse(&row.list) {
                Some(ItemList::Watchlist) => {
                    flags.watchlist = true;
                    flags.watchlist_added_at_ms = Some(row.added_at_ms);
                }
                Some(ItemList::Favorite) => flags.favorite = true,
                None => {}
            }
        }
        Ok(flags)
    }

    async fn listed_items(
        &self,
        user_id: i64,
        list: ItemList,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ListedItem>, StoreError> {
        let sql = "SELECT item_id, added_at_ms FROM library_annotations_lists \
                   WHERE user_id = $1 AND list = $2 \
                   ORDER BY added_at_ms DESC, item_id DESC LIMIT $3 OFFSET $4";
        validate_sql(sql)?;
        Ok(self
            .client()
            // authority: a person's own list, read where they wrote it.
            .query_consistent_map::<ListRow, _>(sql, params!(user_id, list.as_str(), limit, offset))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| ListedItem {
                item_id: row.item_id,
                added_at_ms: row.added_at_ms,
            })
            .collect())
    }

    async fn items_on_list(
        &self,
        user_id: i64,
        list: ItemList,
        item_ids: &[i64],
    ) -> Result<Vec<i64>, StoreError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = serde_json::to_string(item_ids)
            .map_err(|error| StoreError::Task(format!("encode item ids: {error}")))?;
        let sql = "SELECT item_id, added_at_ms FROM library_annotations_lists \
                   WHERE user_id = $1 AND list = $2 \
                     AND item_id IN (SELECT value FROM json_each($3)) ORDER BY item_id";
        validate_sql(sql)?;
        Ok(self
            .client()
            // authority: a person's own list membership, read where they wrote it.
            .query_consistent_map::<ListRow, _>(sql, params!(user_id, list.as_str(), ids))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.item_id)
            .collect())
    }
}
