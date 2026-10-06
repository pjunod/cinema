//! Closed catalogue identities and stateless live-list cursors.
//!
//! Cursor verification proves only a browse boundary. Callers must obtain current
//! export/import authority and read the page in one consistent Store boundary;
//! no generation carried by a cursor or a cached reference grants access.
use crate::{secrets::CredentialKey, sharing::SourceId};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const DEFAULT_PAGE_SIZE: usize = 60;
pub const MAX_PAGE_SIZE: usize = 200;
pub const MAX_BATCH_ITEMS: usize = 200;
pub const MAX_CURSOR_BYTES: usize = 4096;
pub const CURSOR_TTL_MS: i64 = 5 * 60 * 1000;
const MAX_SORT_KEY_BYTES: usize = 544;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogueError {
    #[error("invalid sharing catalogue request")]
    Invalid,
    #[error("sharing catalogue cursor expired")]
    Expired,
    #[error("sharing catalogue query changed; reopen the list")]
    QueryChanged,
}

fn canonical_uuid<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    let value = String::deserialize(deserializer)?;
    let id = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
    if id.to_string() != value {
        return Err(serde::de::Error::custom("noncanonical catalogue UUID"));
    }
    Ok(id)
}

/// Full source identity; equal numeric IDs on different imports never collide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedReference {
    #[serde(deserialize_with = "canonical_uuid")]
    pub import_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    pub server_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    pub catalogue_epoch: Uuid,
    pub library_id: SourceId,
    pub item_id: SourceId,
}

/// Bound before metadata lookups. Authorization is evaluated separately per ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataBatch {
    #[serde(deserialize_with = "bounded_item_ids")]
    pub item_ids: Vec<SourceId>,
}
fn bounded_item_ids<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<SourceId>, D::Error> {
    struct Items;
    impl<'de> serde::de::Visitor<'de> for Items {
        type Value = Vec<SourceId>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("one to 200 canonical item IDs")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut ids =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX_BATCH_ITEMS));
            while let Some(id) = sequence.next_element::<SourceId>()? {
                if ids.len() == MAX_BATCH_ITEMS {
                    return Err(serde::de::Error::custom("too many catalogue items"));
                }
                ids.push(id);
            }
            if ids.is_empty() {
                return Err(serde::de::Error::custom("empty catalogue batch"));
            }
            Ok(ids)
        }
    }
    deserializer.deserialize_seq(Items)
}
impl MetadataBatch {
    pub fn validate(&self) -> Result<(), CatalogueError> {
        if self.item_ids.is_empty() || self.item_ids.len() > MAX_BATCH_ITEMS {
            return Err(CatalogueError::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueCounters {
    pub scope_generation: i64,
    pub catalogue_generation: i64,
    pub library_revision: i64,
}
impl CatalogueCounters {
    fn validate(&self) -> Result<(), CatalogueError> {
        if self.scope_generation <= 0
            || self.catalogue_generation <= 0
            || self.library_revision <= 0
        {
            return Err(CatalogueError::Invalid);
        }
        Ok(())
    }
}

/// Query digest includes every filter and sort choice using a fixed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorContext {
    #[serde(deserialize_with = "canonical_uuid")]
    pub server_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    pub catalogue_epoch: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    pub grant_id: Uuid,
    pub library_id: SourceId,
    pub filter_digest: String,
}
impl CursorContext {
    fn validate(&self) -> Result<(), CatalogueError> {
        if self.filter_digest.len() != 64
            || !self
                .filter_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(CatalogueError::Invalid);
        }
        Ok(())
    }
}

/// BINARY UTF-8 order, followed by a numeric signed-64-bit item tie breaker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueBoundary {
    pub sort_key: String,
    pub item_id: SourceId,
}
impl CatalogueBoundary {
    fn validate(&self) -> Result<(), CatalogueError> {
        if self.sort_key.len() > MAX_SORT_KEY_BYTES || self.sort_key.chars().any(char::is_control) {
            return Err(CatalogueError::Invalid);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    version: u8,
    context: CursorContext,
    boundary: CatalogueBoundary,
    counters: CatalogueCounters,
    issued_at_ms: i64,
    expires_at_ms: i64,
}

/// A cluster provisioned CredentialKey signs cursors identically on every node.
/// Keys and signatures are never part of diagnostic Debug output.
pub struct CatalogueCursor<'a> {
    key: &'a CredentialKey,
}
impl<'a> CatalogueCursor<'a> {
    pub fn new(key: &'a CredentialKey) -> Self {
        Self { key }
    }

    pub fn issue(
        &self,
        context: CursorContext,
        boundary: CatalogueBoundary,
        counters: CatalogueCounters,
        now_ms: i64,
    ) -> Result<String, CatalogueError> {
        context.validate()?;
        boundary.validate()?;
        counters.validate()?;
        let expires_at_ms = now_ms
            .checked_add(CURSOR_TTL_MS)
            .filter(|expiry| now_ms >= 0 && *expiry <= MAX_SAFE_INTEGER)
            .ok_or(CatalogueError::Invalid)?;
        let payload = serde_json::to_vec(&CursorPayload {
            version: 1,
            context,
            boundary,
            counters,
            issued_at_ms: now_ms,
            expires_at_ms,
        })
        .map_err(|_| CatalogueError::Invalid)?;
        let encoded = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(self.key.sharing_catalogue_cursor_mac(&payload))
        );
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err(CatalogueError::Invalid);
        }
        Ok(encoded)
    }

    /// Current counters deliberately do not invalidate a live-list boundary.
    /// A caller must reauthorize the library, then emit its current counters.
    pub fn resume(
        &self,
        encoded: &str,
        expected: &CursorContext,
        now_ms: i64,
    ) -> Result<CatalogueBoundary, CatalogueError> {
        expected.validate()?;
        if encoded.len() > MAX_CURSOR_BYTES || !(0..=MAX_SAFE_INTEGER).contains(&now_ms) {
            return Err(CatalogueError::Invalid);
        }
        let (body, signature) = encoded.split_once('.').ok_or(CatalogueError::Invalid)?;
        let payload = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| CatalogueError::Invalid)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| CatalogueError::Invalid)?;
        if signature.len() != 32
            || !bool::from(
                signature
                    .as_slice()
                    .ct_eq(&self.key.sharing_catalogue_cursor_mac(&payload)),
            )
        {
            return Err(CatalogueError::Invalid);
        }
        let decoded: CursorPayload =
            serde_json::from_slice(&payload).map_err(|_| CatalogueError::Invalid)?;
        decoded.context.validate()?;
        decoded.boundary.validate()?;
        decoded.counters.validate()?;
        if decoded.version != 1
            || decoded.issued_at_ms < 0
            || decoded.expires_at_ms > MAX_SAFE_INTEGER
            || decoded.expires_at_ms.checked_sub(decoded.issued_at_ms) != Some(CURSOR_TTL_MS)
            || now_ms < decoded.issued_at_ms
        {
            return Err(CatalogueError::Invalid);
        }
        if now_ms >= decoded.expires_at_ms {
            return Err(CatalogueError::Expired);
        }
        if decoded.context != *expected {
            return Err(CatalogueError::QueryChanged);
        }
        Ok(decoded.boundary)
    }
}

/// Retains identities for one logical browse even after payload-page eviction.
/// Advance using the source cursor even if every item on a page is a duplicate.
#[derive(Default)]
pub struct BrowseSeen {
    references: BTreeSet<SharedReference>,
}
impl BrowseSeen {
    pub fn insert(&mut self, reference: SharedReference) -> bool {
        self.references.insert(reference)
    }
    pub fn len(&self) -> usize {
        self.references.len()
    }
    pub fn is_empty(&self) -> bool {
        self.references.is_empty()
    }
}

/// Source-only presentation fields. Filesystem and account data have no wire slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCatalogueItem {
    #[serde(
        default,
        deserialize_with = "crate::sharing_artwork::bounded_art",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub art: Vec<crate::sharing_artwork::SourceArtwork>,
    pub item_id: SourceId,
    pub library_id: SourceId,
    pub parent_id: Option<SourceId>,
    pub kind: SourceItemKind,
    pub title: String,
    pub sort_title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub genres: Vec<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceItemKind {
    Movie,
    Show,
    Season,
    Episode,
}
impl SourceCatalogueItem {
    pub fn validate(&self) -> Result<(), CatalogueError> {
        if self.art.len() > 8 {
            return Err(CatalogueError::Invalid);
        }
        let mut seen = std::collections::BTreeSet::new();
        for art in &self.art {
            let reference = art
                .resource
                .reference_unverified()
                .map_err(|_| CatalogueError::Invalid)?;
            if reference.library_id != self.library_id
                || reference.item_id != self.item_id
                || reference.kind != art.kind
                || reference.variant != art.variant
                || !seen.insert((art.kind.label(), art.variant.label()))
            {
                return Err(CatalogueError::Invalid);
            }
        }
        if self.title.len() > 512
            || self.sort_title.len() > 512
            || self.overview.as_ref().is_some_and(|v| v.len() > 8192)
            || self.genres.len() > 64
            || self.genres.iter().any(|v| v.len() > 128)
        {
            return Err(CatalogueError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct CataloguePageRequest {
    pub credential_hash: String,
    pub grant_id: Uuid,
    pub library_id: SourceId,
    pub parent_id: Option<SourceId>,
    pub query: String,
    pub boundary: Option<CatalogueBoundary>,
    pub limit: usize,
}
/// Internal records deliberately do not implement Serialize: local artwork names
/// must be converted to grant-bound resources before a peer sees presentation.
#[derive(Debug, Clone, Deserialize)]
pub struct SourceCatalogueRecord {
    pub item: SourceCatalogueItem,
    pub boundary_sort_key: String,
    pub poster_filename: Option<String>,
    pub backdrop_filename: Option<String>,
}
#[derive(Debug)]
pub struct SourceCataloguePage {
    pub records: Vec<SourceCatalogueRecord>,
    pub counters: CatalogueCounters,
    pub has_more: bool,
}

/// Closed response vocabulary shared by source presentation and the receiver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CataloguePeerPage {
    pub items: Vec<SourceCatalogueItem>,
    pub next_cursor: Option<String>,
    pub catalogue_revision: i64,
    pub scope_generation: i64,
    pub catalogue_generation: i64,
}
impl CataloguePeerPage {
    pub fn validate(&self) -> Result<(), CatalogueError> {
        if self.items.len() > MAX_PAGE_SIZE
            || self
                .next_cursor
                .as_ref()
                .is_some_and(|s| s.len() > MAX_CURSOR_BYTES)
        {
            return Err(CatalogueError::Invalid);
        }
        CatalogueCounters {
            library_revision: self.catalogue_revision,
            scope_generation: self.scope_generation,
            catalogue_generation: self.catalogue_generation,
        }
        .validate()?;
        self.items
            .iter()
            .try_for_each(SourceCatalogueItem::validate)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CataloguePeerBatchEntry {
    pub item_id: SourceId,
    pub item: Option<SourceCatalogueItem>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CataloguePeerBatch {
    pub items: Vec<CataloguePeerBatchEntry>,
}
impl CataloguePeerBatch {
    pub fn validate(&self, expected: &MetadataBatch) -> Result<(), CatalogueError> {
        expected.validate()?;
        if self.items.len() != expected.item_ids.len() {
            return Err(CatalogueError::Invalid);
        }
        for (entry, id) in self.items.iter().zip(&expected.item_ids) {
            if &entry.item_id != id || entry.item.as_ref().is_some_and(|item| &item.item_id != id) {
                return Err(CatalogueError::Invalid);
            }
            if let Some(item) = &entry.item {
                item.validate()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> CursorContext {
        CursorContext {
            server_id: Uuid::from_u128(1),
            catalogue_epoch: Uuid::from_u128(2),
            grant_id: Uuid::from_u128(3),
            library_id: SourceId::parse("12").expect("synthetic catalogue fixture"),
            filter_digest: "a".repeat(64),
        }
    }
    fn counters(revision: i64) -> CatalogueCounters {
        CatalogueCounters {
            scope_generation: 1,
            catalogue_generation: 1,
            library_revision: revision,
        }
    }
    fn boundary(id: &str) -> CatalogueBoundary {
        CatalogueBoundary {
            sort_key: "title".into(),
            item_id: SourceId::parse(id).expect("synthetic catalogue fixture"),
        }
    }

    #[test]
    fn sharing_catalogue_cursor_authenticates_boundary_and_refuses_replay_context() {
        let key = CredentialKey::from_bytes([41; 32]);
        let other_node = CredentialKey::from_bytes([41; 32]);
        let token = CatalogueCursor::new(&key)
            .issue(context(), boundary("9007199254740993"), counters(1), 1000)
            .expect("synthetic catalogue fixture");
        assert_eq!(
            CatalogueCursor::new(&other_node)
                .resume(&token, &context(), 2000)
                .expect("synthetic catalogue fixture"),
            boundary("9007199254740993")
        );
        let mut wrong = context();
        wrong.grant_id = Uuid::from_u128(4);
        assert_eq!(
            CatalogueCursor::new(&key).resume(&token, &wrong, 2000),
            Err(CatalogueError::QueryChanged)
        );
        wrong = context();
        wrong.filter_digest = "b".repeat(64);
        assert_eq!(
            CatalogueCursor::new(&key).resume(&token, &wrong, 2000),
            Err(CatalogueError::QueryChanged)
        );
        // Substitution across Sources, catalogue epochs and libraries is a
        // typed reopen, never a boundary applied to another list.
        for field in 0..3 {
            wrong = context();
            match field {
                0 => wrong.server_id = Uuid::from_u128(5),
                1 => wrong.catalogue_epoch = Uuid::from_u128(6),
                _ => wrong.library_id = SourceId::parse("9007199254740993").expect("library"),
            }
            assert_eq!(
                CatalogueCursor::new(&key).resume(&token, &wrong, 2000),
                Err(CatalogueError::QueryChanged)
            );
        }
        assert_eq!(
            CatalogueCursor::new(&CredentialKey::from_bytes([42; 32])).resume(
                &token,
                &context(),
                2000
            ),
            Err(CatalogueError::Invalid)
        );
        assert_eq!(
            CatalogueCursor::new(&key).resume(&token, &context(), 301000),
            Err(CatalogueError::Expired)
        );
        let mut tampered = token.into_bytes();
        tampered[0] = b'X';
        assert_eq!(
            CatalogueCursor::new(&key).resume(
                std::str::from_utf8(&tampered).expect("synthetic catalogue fixture"),
                &context(),
                2000
            ),
            Err(CatalogueError::Invalid)
        );
        assert_eq!(
            CatalogueCursor::new(&key).resume(&"x".repeat(4097), &context(), 2000),
            Err(CatalogueError::Invalid)
        );
    }

    #[test]
    fn sharing_catalogue_live_cursor_resumes_across_revisions_and_deduplicates_sources() {
        let key = CredentialKey::from_bytes([41; 32]);
        let codec = CatalogueCursor::new(&key);
        let mut seen = BrowseSeen::default();
        let mut previous: Option<String> = None;
        for page in 0..100 {
            if let Some(token) = previous {
                assert_eq!(
                    codec
                        .resume(&token, &context(), 1000 + page)
                        .expect("synthetic catalogue fixture"),
                    boundary(&((page - 1) * 200 + 199).to_string())
                );
            }
            for id in page * 200..(page + 1) * 200 {
                let reference = SharedReference {
                    import_id: Uuid::from_u128(10),
                    server_id: context().server_id,
                    catalogue_epoch: context().catalogue_epoch,
                    library_id: context().library_id,
                    item_id: SourceId::parse(&id.to_string()).expect("synthetic catalogue fixture"),
                };
                assert!(seen.insert(reference.clone()));
                assert!(!seen.insert(reference.clone()));
                let mut other = reference;
                other.server_id = Uuid::from_u128(20);
                assert!(seen.insert(other));
            }
            previous = Some(
                codec
                    .issue(
                        context(),
                        boundary(&(page * 200 + 199).to_string()),
                        counters(page + 1),
                        1000 + page,
                    )
                    .expect("synthetic catalogue fixture"),
            );
        }
        assert_eq!(seen.len(), 40_000);
    }

    #[test]
    fn sharing_catalogue_batch_is_closed_and_bounded() {
        assert!(
            serde_json::from_str::<MetadataBatch>(r#"{"item_ids":["1"],"path":"/media"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<MetadataBatch>(r#"{"item_ids":[9007199254740993]}"#).is_err()
        );
        let mut batch = MetadataBatch {
            item_ids: vec![SourceId::parse("1").expect("synthetic catalogue fixture"); 200],
        };
        assert!(batch.validate().is_ok());
        batch
            .item_ids
            .push(SourceId::parse("2").expect("synthetic catalogue fixture"));
        assert_eq!(batch.validate(), Err(CatalogueError::Invalid));
        assert!(serde_json::from_str::<MetadataBatch>(
            &serde_json::to_string(&batch).expect("synthetic catalogue fixture")
        )
        .is_err());
        batch.item_ids.clear();
        assert_eq!(batch.validate(), Err(CatalogueError::Invalid));
    }
}
