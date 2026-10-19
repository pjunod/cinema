//! Item ratings and per-person item lists for the single-node SQLite backend.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::annotations::{ItemList, ItemListFlags, ItemRatings, ListedItem, Rating, RatingSource};
use crate::error::StoreError;
use crate::store::AnnotationStore;

#[async_trait]
impl AnnotationStore for SqliteStore {
    async fn item_ratings(&self, item_id: i64) -> Result<Option<ItemRatings>, StoreError> {
        self.with_conn(move |conn| {
            let fetch: Option<(i64, i64)> = conn
                .query_row(
                    "SELECT fetched_at_ms, omdb_consulted FROM library_annotations_rating_fetches \
                     WHERE item_id = ?1",
                    params![item_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((fetched_at_ms, omdb)) = fetch else {
                return Ok(None);
            };
            let mut statement = conn.prepare(
                "SELECT source, value, scale, votes FROM library_annotations_ratings \
                 WHERE item_id = ?1",
            )?;
            let mut ratings = statement
                .query_map(params![item_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter_map(|(source, value, scale, votes)| {
                    Some(Rating {
                        source: RatingSource::parse(&source)?,
                        value,
                        scale: u8::try_from(scale).ok()?,
                        votes,
                    })
                })
                .collect::<Vec<_>>();
            ratings.sort_by_key(|rating| rating.source);
            Ok(Some(ItemRatings {
                fetched_at_ms,
                omdb_consulted: omdb != 0,
                ratings,
            }))
        })
        .await
    }

    async fn put_item_ratings(
        &self,
        item_id: i64,
        ratings: &ItemRatings,
    ) -> Result<(), StoreError> {
        let ratings = ratings.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM library_annotations_ratings WHERE item_id = ?1",
                params![item_id],
            )?;
            for rating in &ratings.ratings {
                tx.execute(
                    "INSERT INTO library_annotations_ratings(item_id, source, value, scale, votes) \
                     SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM items WHERE id = ?1)",
                    params![
                        item_id,
                        rating.source.as_str(),
                        rating.value,
                        i64::from(rating.scale),
                        rating.votes
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO library_annotations_rating_fetches(item_id, fetched_at_ms, omdb_consulted) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM items WHERE id = ?1) \
                 ON CONFLICT(item_id) DO UPDATE SET fetched_at_ms = excluded.fetched_at_ms, \
                 omdb_consulted = excluded.omdb_consulted",
                params![item_id, ratings.fetched_at_ms, i64::from(ratings.omdb_consulted)],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn set_item_listed(
        &self,
        user_id: i64,
        item_id: i64,
        list: ItemList,
        listed: bool,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            if listed {
                conn.execute(
                    "INSERT INTO library_annotations_lists(user_id, list, item_id, added_at_ms) \
                     SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM items WHERE id = ?3) \
                     ON CONFLICT(user_id, list, item_id) DO UPDATE SET added_at_ms = excluded.added_at_ms",
                    params![user_id, list.as_str(), item_id, at_ms],
                )?;
            } else {
                conn.execute(
                    "DELETE FROM library_annotations_lists \
                     WHERE user_id = ?1 AND list = ?2 AND item_id = ?3",
                    params![user_id, list.as_str(), item_id],
                )?;
            }
            Ok(())
        })
        .await
    }

    async fn item_list_flags(
        &self,
        user_id: i64,
        item_id: i64,
    ) -> Result<ItemListFlags, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT list, added_at_ms FROM library_annotations_lists \
                 WHERE user_id = ?1 AND item_id = ?2",
            )?;
            let mut flags = ItemListFlags::default();
            for row in statement.query_map(params![user_id, item_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (list, added) = row?;
                match ItemList::parse(&list) {
                    Some(ItemList::Watchlist) => {
                        flags.watchlist = true;
                        flags.watchlist_added_at_ms = Some(added);
                    }
                    Some(ItemList::Favorite) => flags.favorite = true,
                    None => {}
                }
            }
            Ok(flags)
        })
        .await
    }

    async fn listed_items(
        &self,
        user_id: i64,
        list: ItemList,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ListedItem>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT item_id, added_at_ms FROM library_annotations_lists \
                 WHERE user_id = ?1 AND list = ?2 \
                 ORDER BY added_at_ms DESC, item_id DESC LIMIT ?3 OFFSET ?4",
            )?;
            let rows = statement
                .query_map(params![user_id, list.as_str(), limit, offset], |row| {
                    Ok(ListedItem {
                        item_id: row.get(0)?,
                        added_at_ms: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    async fn items_on_list(
        &self,
        user_id: i64,
        list: ItemList,
        item_ids: &[i64],
    ) -> Result<Vec<i64>, StoreError> {
        let ids = serde_json::to_string(item_ids)
            .map_err(|error| StoreError::Task(format!("encode item ids: {error}")))?;
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT item_id FROM library_annotations_lists \
                 WHERE user_id = ?1 AND list = ?2 \
                   AND item_id IN (SELECT value FROM json_each(?3)) ORDER BY item_id",
            )?;
            let rows = statement
                .query_map(params![user_id, list.as_str(), ids], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }
}
