use super::SqliteStore;
use crate::{error::StoreError, store::classification::*};
use async_trait::async_trait;
use rusqlite::params;
#[async_trait]
impl ClassificationStore for SqliteStore {
    async fn classification_page(&self, after: i64, limit: i64) -> Result<Vec<Entry>, StoreError> {
        self.with_read(move |conn| {
            let mut s = conn.prepare(&inventory_sql())?;
            let rows = s
                .query_map(params![after, limit.clamp(1, 256)], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows.iter().map(|r| decode(r)).collect()
        })
        .await
    }
    async fn write_classification(&self, id: i64, r: &Record) -> Result<bool, StoreError> {
        let r = r.clone();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                &write_sql(),
                params![
                    id,
                    r.source_json,
                    serde_json::to_string(&r.classification)
                        .map_err(|e| StoreError::Database(e.to_string()))?,
                    serde_json::to_string(&r.overrides)
                        .map_err(|e| StoreError::Database(e.to_string()))?,
                    terms(&r),
                    r.revision
                ],
            )? == 1)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metadata::classification, store::MediaStore};
    async fn setup() -> SqliteStore {
        let s = SqliteStore::open_in_memory().expect("store");
        s.with_conn(|c|{c.execute("INSERT INTO libraries(id,name,kind,paths) VALUES(1,'Movies','movies','[]')",[])?;c.execute("INSERT INTO items(id,library_id,kind,title,sort_title,overview,genres,tags) VALUES(1,1,'movie','Cosmos','Cosmos','An astronaut explores distant planets.','[\"Documentary\"]','[]')",[])?;Ok(())}).await.expect("seed");
        s
    }
    async fn record(s: &SqliteStore) -> Record {
        let e = s.classification_page(0, 10).await.expect("page").remove(0);
        Record {
            classification: classification::classify(
                &e.input().expect("input").metadata(),
                vec!["space exploration".into()],
            ),
            source_json: e.source_json,
            overrides: Default::default(),
            revision: e.record.map(|r| r.revision).unwrap_or(0),
        }
    }
    #[tokio::test]
    async fn classification_fences_edits_and_rebuilds_only_current_text() {
        let s = setup().await;
        let mut r = record(&s).await;
        assert!(s.write_classification(1, &r).await.expect("write"));
        assert_eq!(
            s.search_items("cosmos space", 10)
                .await
                .expect("search across title and keyword")
                .len(),
            1
        );
        assert_eq!(
            s.search_items("cosmos -space", 10)
                .await
                .expect("exclude enriched text")
                .len(),
            0
        );
        assert!(!s
            .write_classification(1, &r)
            .await
            .expect("stale revision refused"));
        r.revision = 1;
        r.overrides.include = vec!["topic:cooking".into()];
        r.overrides.exclude = vec!["topic:space".into()];
        assert!(s.write_classification(1, &r).await.expect("correction"));
        s.with_conn(|c| {
            c.execute(
                "UPDATE items SET overview='A chef prepares dinner.' WHERE id=1",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("metadata edit");
        assert!(s
            .search_items("space", 10)
            .await
            .expect("stale labels hidden")
            .is_empty());
        assert_eq!(
            s.search_items("chef", 10)
                .await
                .expect("original FTS still works")
                .len(),
            1
        );
        s.rebuild_search_index().await.expect("rebuild");
        assert!(s
            .search_items("space", 10)
            .await
            .expect("rebuild does not resurrect stale")
            .is_empty());
        r.revision = 2;
        assert!(!s
            .write_classification(1, &r)
            .await
            .expect("old metadata refused"));
        let e = s.classification_page(0, 10).await.expect("page").remove(0);
        let existing = e.record.expect("old record");
        assert_eq!(existing.overrides.include, vec!["topic:cooking"]);
        r.source_json = e.source_json;
        r.classification = classification::classify(
            &serde_json::from_str::<Input>(&r.source_json)
                .expect("source")
                .metadata(),
            vec![],
        );
        assert!(s
            .write_classification(1, &r)
            .await
            .expect("refresh preserves correction"));
        assert_eq!(
            s.search_items("topic:cooking", 10)
                .await
                .expect("manual label searchable")
                .len(),
            1
        );
        s.with_conn(|c| {
            c.execute("DELETE FROM items WHERE id=1", [])?;
            Ok(())
        })
        .await
        .expect("delete");
        assert!(s
            .classification_page(0, 10)
            .await
            .expect("cascade")
            .is_empty());
        assert!(s
            .search_items("cooking", 10)
            .await
            .expect("no orphan FTS")
            .is_empty());
    }
    #[tokio::test]
    async fn phrases_aliases_and_literal_fts_operators() {
        let s = setup().await;
        s.with_conn(|c|{c.execute("UPDATE items SET title='One Night',overview='A stand-up comedy special.' WHERE id=1",[])?;Ok(())}).await.expect("update");
        assert_eq!(s.search_items("standup", 10).await.expect("alias").len(), 1);
        assert_eq!(
            s.search_items("\"stand-up comedy\" -backstage", 10)
                .await
                .expect("phrase")
                .len(),
            1
        );
        assert!(s
            .search_items("\"comedy stand-up\"", 10)
            .await
            .expect("phrase order")
            .is_empty());
        for q in ["*", "-", "\"", "NEAR(a b)", "title:NOT", "a OR b"] {
            s.search_items(q, 10)
                .await
                .expect("literal input cannot break MATCH");
        }
    }
}
