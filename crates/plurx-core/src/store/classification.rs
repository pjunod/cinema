//! Shared classification persistence contract; generated labels never overwrite manual tags.
use crate::{
    channel_subjects::Metadata,
    error::StoreError,
    metadata::classification::{Classification, Overrides},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS media_classifications (
 item_id INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 source_json TEXT NOT NULL,
 payload TEXT NOT NULL,
 overrides TEXT NOT NULL DEFAULT '{"include":[],"exclude":[]}',
 terms TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE VIRTUAL TABLE IF NOT EXISTS classification_fts USING fts5(terms);
CREATE TRIGGER IF NOT EXISTS classification_ai AFTER INSERT ON media_classifications BEGIN
 INSERT INTO classification_fts(rowid,terms) VALUES(new.item_id,new.terms || ' ' || json_extract(new.source_json,'$.title') || ' ' || json_extract(new.source_json,'$.overview') || ' ' || json_extract(new.source_json,'$.genres') || ' ' || json_extract(new.source_json,'$.tags'));
END;
CREATE TRIGGER IF NOT EXISTS classification_ad AFTER DELETE ON media_classifications BEGIN
 DELETE FROM classification_fts WHERE rowid=old.item_id;
END;
CREATE TRIGGER IF NOT EXISTS classification_au AFTER UPDATE OF terms ON media_classifications BEGIN
 DELETE FROM classification_fts WHERE rowid=old.item_id;
 INSERT INTO classification_fts(rowid,terms) VALUES(new.item_id,new.terms || ' ' || json_extract(new.source_json,'$.title') || ' ' || json_extract(new.source_json,'$.overview') || ' ' || json_extract(new.source_json,'$.genres') || ' ' || json_extract(new.source_json,'$.tags'));
END;
CREATE TRIGGER IF NOT EXISTS classification_source_changed AFTER UPDATE OF title,overview,genres,tags,year,tmdb_id,kind ON items
WHEN old.kind IS NOT new.kind OR old.title IS NOT new.title OR old.overview IS NOT new.overview OR old.genres IS NOT new.genres OR old.tags IS NOT new.tags OR old.year IS NOT new.year OR old.tmdb_id IS NOT new.tmdb_id BEGIN
 DELETE FROM classification_fts WHERE rowid=new.id;
END;
"#;
/// Same serialization in inventory and write fence; no timestamp collision can
/// accept a classification generated from an older title/overview/provider ID.
pub const SOURCE:&str="json_object('id',i.id,'kind',i.kind,'title',i.title,'overview',COALESCE(i.overview,''),'year',i.year,'tmdb_id',i.tmdb_id,'genres',json(i.genres),'tags',json(i.tags))";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub overview: String,
    pub year: Option<i32>,
    pub tmdb_id: Option<i64>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
}
impl Input {
    pub fn metadata(&self) -> Metadata {
        Metadata {
            fields: [
                ("title".into(), self.title.clone()),
                ("overview".into(), self.overview.clone()),
                ("genres".into(), self.genres.join(", ")),
                ("tags".into(), self.tags.join(", ")),
                ("kind".into(), self.kind.clone()),
                (
                    "year".into(),
                    self.year.map(|v| v.to_string()).unwrap_or_default(),
                ),
            ]
            .into(),
            missing_overview: self.overview.is_empty(),
            truncated: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub source_json: String,
    pub classification: Classification,
    pub overrides: Overrides,
    pub revision: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub indexed: bool,
    pub source_json: String,
    pub record: Option<Record>,
}
impl Entry {
    pub fn input(&self) -> Result<Input, StoreError> {
        serde_json::from_str(&self.source_json).map_err(|e| StoreError::Database(e.to_string()))
    }
}
pub fn inventory_sql() -> String {
    format!("SELECT json_object('indexed',json(CASE WHEN EXISTS(SELECT 1 FROM classification_fts WHERE rowid=i.id) THEN 'true' ELSE 'false' END),'source_json',({SOURCE} || ''),'record',CASE WHEN c.item_id IS NULL THEN NULL ELSE json_object('source_json',c.source_json,'classification',json(c.payload),'overrides',json(c.overrides),'revision',c.revision) END) AS payload FROM items i LEFT JOIN media_classifications c ON c.item_id=i.id WHERE i.id>$1 AND i.kind IN ('movie','show','episode','video','book','audiobook') ORDER BY i.id LIMIT $2")
}
pub fn write_sql() -> String {
    format!("INSERT INTO media_classifications(item_id,source_json,payload,overrides,terms,revision) SELECT $1,$2,$3,$4,$5,$6+1 WHERE EXISTS(SELECT 1 FROM items i WHERE i.id=$1 AND {SOURCE}=$2) AND COALESCE((SELECT revision FROM media_classifications WHERE item_id=$1),0)=$6 ON CONFLICT(item_id) DO UPDATE SET source_json=excluded.source_json,payload=excluded.payload,overrides=excluded.overrides,terms=excluded.terms,revision=excluded.revision")
}
#[async_trait]
pub trait ClassificationStore: Send + Sync {
    async fn classification_page(&self, after: i64, limit: i64) -> Result<Vec<Entry>, StoreError>;
    async fn write_classification(&self, item_id: i64, record: &Record)
        -> Result<bool, StoreError>;
}
pub fn decode(payload: &str) -> Result<Entry, StoreError> {
    serde_json::from_str(payload).map_err(|e| StoreError::Database(e.to_string()))
}
pub fn terms(record: &Record) -> String {
    let mut terms = record.classification.terms(&record.overrides);
    terms.insert(0, "classification:ready".into());
    serde_json::to_string(&terms).expect("strings serialize")
}

pub fn rebuild_sql() -> String {
    format!("INSERT INTO classification_fts(rowid,terms) SELECT c.item_id,c.terms || ' ' || i.title || ' ' || COALESCE(i.overview,'') || ' ' || i.genres || ' ' || i.tags FROM media_classifications c JOIN items i ON i.id=c.item_id WHERE c.source_json={SOURCE}")
}
