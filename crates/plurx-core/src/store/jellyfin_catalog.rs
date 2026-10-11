//! Bounded catalog pages with identity and watch facts from one authoritative read.
use crate::error::StoreError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JellyfinCatalogSort {
    SortName,
    Name,
    Added,
    Year,
    PremiereDate,
    Index,
    ParentIndex,
    WatchUpdated,
}
impl JellyfinCatalogSort {
    fn column(self) -> &'static str {
        match self {
            Self::SortName => "sort_title COLLATE NOCASE",
            Self::Name => "title COLLATE NOCASE",
            Self::Added => "added_at",
            Self::Year => "year",
            Self::PremiereDate => "air_date",
            Self::Index => "COALESCE(episode_number,season_number)",
            Self::ParentIndex => "season_number",
            Self::WatchUpdated => "resume_updated",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JellyfinCatalogMode {
    Browse,
    Latest,
    Resume,
    NextUp,
    Upcoming,
    Similar,
}
/// Only digested credentials cross the storage boundary. No Debug implementation.
#[derive(Clone, Serialize)]
pub struct JellyfinCatalogQuery {
    pub mode: JellyfinCatalogMode,
    pub today: Option<String>,
    pub user_wire: String,
    pub token_hash: String,
    pub parent_wire: Option<String>,
    pub item_wire: Option<String>,
    pub recursive: bool,
    pub start: i64,
    pub limit: i64,
    pub kinds: Vec<String>,
    pub search: Option<String>,
    pub sorts: Vec<JellyfinCatalogSort>,
    pub descending: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct JellyfinCatalogMissing {
    pub kind: String,
    pub id: i64,
}
/// Internal projection; HTTP adapters must explicitly convert to protocol DTOs.
/// Rows contain no native media paths or raw probe object.
#[derive(Clone, Debug, Deserialize)]
pub struct JellyfinCatalogPage {
    pub authorized: bool,
    pub parent_valid: bool,
    pub total: i64,
    pub source_overflow: bool,
    pub missing: Vec<JellyfinCatalogMissing>,
    pub items: Vec<serde_json::Value>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct JellyfinCatalogLibrary {
    pub native_id: i64,
    pub wire_id: Option<String>,
    pub name: String,
    pub kind: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct JellyfinCatalogIdentity {
    pub native_id: i64,
    pub wire_id: Option<String>,
    pub name: String,
    pub libraries: Vec<JellyfinCatalogLibrary>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct JellyfinCatalogArtwork {
    pub wire_id: String,
    pub filename: String,
    pub generation: String,
}
#[async_trait]
pub trait JellyfinCatalogStore: Send + Sync {
    /// Public mapped Movie/TV artwork only; native incarnation, source name and
    /// enabled switch generation are checked in the same authoritative snapshot.
    async fn jellyfin_catalog_artwork(
        &self,
        wire_id: String,
        backdrop: bool,
    ) -> Result<Option<JellyfinCatalogArtwork>, StoreError>;
    /// Credential membership, user name/identity and supported views share one read.
    /// At most 501 libraries are returned; adapters refuse overflow above 500.
    async fn jellyfin_catalog_identity(
        &self,
        token_hash: String,
    ) -> Result<Option<JellyfinCatalogIdentity>, StoreError>;
    /// Return a coherent page, including the live wire mappings in that snapshot.
    /// Callers must prime `missing`, then re-read; never splice allocated IDs
    /// into the previously returned item bodies. Source fan-out is bounded at
    /// 5,000 per page; overflow fails closed rather than truncating sources.
    async fn jellyfin_catalog_page(
        &self,
        query: JellyfinCatalogQuery,
    ) -> Result<JellyfinCatalogPage, StoreError>;
}
pub(crate) fn prepare(query: &JellyfinCatalogQuery) -> Result<(String, String), StoreError> {
    let wire = |s: &str| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && s.bytes().any(|b| b != b'0')
    };
    if !wire(&query.user_wire)
        || query.parent_wire.as_deref().is_some_and(|s| !wire(s))
        || query.item_wire.as_deref().is_some_and(|s| !wire(s))
        || query.token_hash.len() != 64
        || !query
            .token_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || query.today.as_ref().is_some_and(|s| {
            s.len() != 10
                || !s.bytes().enumerate().all(|(n, b)| {
                    if n == 4 || n == 7 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                })
        })
        || (matches!(query.mode, JellyfinCatalogMode::Upcoming) && query.today.is_none())
        || (matches!(query.mode, JellyfinCatalogMode::Similar) && query.item_wire.is_none())
        || query.start < 0
        || !(0..=500).contains(&query.limit)
        || query.sorts.is_empty()
        || query.sorts.len() > 4
        || query.kinds.len() > 4
        || query
            .kinds
            .iter()
            .any(|s| !matches!(s.as_str(), "movie" | "show" | "season" | "episode"))
        || query
            .search
            .as_ref()
            .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(StoreError::Identity(
            "invalid Jellyfin catalog query".into(),
        ));
    }
    // Enum-owned SQL fragments only; all caller values remain in bound JSON.
    let direction = if query.descending { "DESC" } else { "ASC" };
    let mut order = query
        .sorts
        .iter()
        .map(|sort| format!("{} {direction}", sort.column()))
        .collect::<Vec<_>>();
    order.push(format!("id {direction}"));
    let sql = CATALOG_PAGE_SQL
        .replace("/* page order */", &order.join(", "))
        .replace(
            "/* native next up */",
            &super::sql_source::jellyfin_next_up_ids(),
        );
    let args = serde_json::to_string(query).map_err(|e| StoreError::Identity(e.to_string()))?;
    Ok((sql, args))
}

pub(crate) const CATALOG_PAGE_SQL: &str = r#"
WITH RECURSIVE
scope AS (
 SELECT u.native_id AS user_id FROM jellyfin_entity_ids u
 JOIN tokens t ON t.user_id=u.native_id AND t.token_hash=json_extract($1,'$.token_hash')
 JOIN jellyfin_login_tokens login ON login.token_hash=t.token_hash AND login.user_id=u.native_id
 WHERE u.entity_kind='user' AND u.retired=0 AND u.wire_id=json_extract($1,'$.user_wire')
), parent AS (
 SELECT wire.entity_kind,wire.native_id FROM jellyfin_entity_ids wire
 WHERE wire.retired=0 AND wire.wire_id=json_extract($1,'$.parent_wire')
 AND ((wire.entity_kind='library' AND EXISTS(SELECT 1 FROM libraries l WHERE l.id=wire.native_id AND l.kind IN ('movies','shows')))
 OR (wire.entity_kind='item' AND EXISTS(SELECT 1 FROM items i JOIN libraries l ON l.id=i.library_id WHERE i.id=wire.native_id AND l.kind IN ('movies','shows') AND i.kind IN ('movie','show','season','episode'))))
), target AS (
 SELECT i.* FROM jellyfin_entity_ids wire JOIN items i ON i.id=wire.native_id
 JOIN libraries l ON l.id=i.library_id
 WHERE wire.entity_kind='item' AND wire.retired=0 AND wire.wire_id=json_extract($1,'$.item_wire')
 AND l.kind IN ('movies','shows') AND i.kind IN ('movie','show','season','episode')
), descendants(id) AS (
 SELECT child.id FROM items child JOIN items ancestor ON child.parent_id=ancestor.id
 WHERE ancestor.id=(SELECT native_id FROM parent WHERE entity_kind='item') AND child.library_id=ancestor.library_id AND child.kind IN ('movie','show','season','episode')
 UNION SELECT child.id FROM items child JOIN descendants p ON child.parent_id=p.id JOIN items ancestor ON ancestor.id=p.id
 WHERE child.library_id=ancestor.library_id AND child.kind IN ('movie','show','season','episode')
), next_up AS ( /* native next up */ ), selected AS (
 SELECT i.*,COALESCE((SELECT updated_at FROM watch_state w WHERE w.user_id=(SELECT user_id FROM scope) AND w.item_id=i.id),0) AS resume_updated
 FROM items i JOIN libraries library ON library.id=i.library_id
 WHERE EXISTS(SELECT 1 FROM scope) AND library.kind IN ('movies','shows')
 AND i.kind IN ('movie','show','season','episode')
 AND (CASE json_extract($1,'$.mode')
   WHEN 'resume' THEN i.kind IN ('movie','episode') AND EXISTS(SELECT 1 FROM watch_state w WHERE w.user_id=(SELECT user_id FROM scope) AND w.item_id=i.id AND w.watched=0 AND w.position_ms>0)
   WHEN 'next_up' THEN i.id IN (SELECT id FROM next_up)
   WHEN 'latest' THEN i.kind IN ('movie','episode')
   WHEN 'upcoming' THEN i.kind='episode' AND i.air_date>=json_extract($1,'$.today')
   WHEN 'similar' THEN i.id<>(SELECT id FROM target) AND i.kind=(SELECT kind FROM target)
      AND EXISTS(SELECT 1 FROM json_each(i.genres) genre JOIN target t JOIN json_each(t.genres) other ON other.value=genre.value)
   ELSE 1 END)
 AND (json_extract($1,'$.parent_wire') IS NULL OR EXISTS(SELECT 1 FROM parent))
 AND (CASE WHEN EXISTS(SELECT 1 FROM parent WHERE entity_kind='library') THEN
   i.library_id=(SELECT native_id FROM parent WHERE entity_kind='library') AND (json_extract($1,'$.recursive') OR i.parent_id IS NULL)
 WHEN EXISTS(SELECT 1 FROM parent WHERE entity_kind='item') THEN
   CASE WHEN json_extract($1,'$.recursive') THEN i.id IN (SELECT id FROM descendants)
        ELSE i.parent_id=(SELECT native_id FROM parent WHERE entity_kind='item') END
 WHEN json_extract($1,'$.recursive') OR json_extract($1,'$.item_wire') IS NOT NULL THEN 1
 ELSE i.parent_id IS NULL END)
 AND (json_extract($1,'$.item_wire') IS NULL OR json_extract($1,'$.mode')='similar' OR i.id IN (SELECT id FROM target))
 AND (json_array_length(json_extract($1,'$.kinds'))=0 OR i.kind IN (SELECT value FROM json_each($1,'$.kinds')))
 AND (json_extract($1,'$.search') IS NULL OR instr(lower(i.title),lower(json_extract($1,'$.search')))>0)
), page AS (
 SELECT * FROM selected ORDER BY /* page order */
 LIMIT json_extract($1,'$.limit') OFFSET json_extract($1,'$.start')
), page_descendants(root,id) AS (
 SELECT id,id FROM page UNION SELECT p.root,c.id FROM page_descendants p JOIN items c ON c.parent_id=p.id JOIN items ancestor ON ancestor.id=p.id
 WHERE c.library_id=ancestor.library_id AND c.kind IN ('movie','show','season','episode')
), page_sources AS (
 SELECT f.* FROM files f JOIN page p ON p.id=f.item_id ORDER BY f.item_id,f.id LIMIT 5001
), required(kind,id) AS (
 SELECT 'item',id FROM page UNION SELECT 'item',a.id FROM page p JOIN items a ON a.id=p.parent_id AND a.library_id=p.library_id AND a.kind IN ('movie','show','season','episode')
 UNION SELECT 'item',g.id FROM page p JOIN items a ON p.parent_id=a.id AND a.library_id=p.library_id AND a.kind IN ('movie','show','season','episode') JOIN items g ON g.id=a.parent_id AND g.library_id=p.library_id AND g.kind IN ('movie','show','season','episode')
 UNION SELECT 'library',library_id FROM page UNION SELECT 'file',id FROM page_sources
), missing AS (
 SELECT r.kind,r.id FROM required r WHERE NOT EXISTS(SELECT 1 FROM jellyfin_entity_ids map
   WHERE map.entity_kind=r.kind AND map.native_id=r.id AND map.retired=0)
), projected AS (
 SELECT json_object('native_id',p.id,'wire_id',wire.wire_id,'library_wire',lib.wire_id,
 'kind',p.kind,'parent_wire',parent_wire.wire_id,'parent_title',a.title,'grandparent_wire',grand_wire.wire_id,'grandparent_title',g.title,
 'title',p.title,'sort_title',p.sort_title,'year',p.year,'overview',p.overview,
 'tmdb_id',p.tmdb_id,'imdb_id',p.imdb_id,'season_number',p.season_number,'episode_number',p.episode_number,
 'air_date',p.air_date,'runtime_ms',p.runtime_ms,'poster_path',p.poster_path,'backdrop_path',p.backdrop_path,
 'added_at',p.added_at,'updated_at',p.updated_at,'genres',json(p.genres),
 'position_ms',COALESCE(w.position_ms,0),'watched',COALESCE(w.watched,0),'watch_updated_at',COALESCE(w.updated_at,0),
 'child_count',(SELECT COUNT(*) FROM items child WHERE child.parent_id=p.id AND child.library_id=p.library_id AND child.kind IN ('movie','show','season','episode')),
 'recursive_count',(SELECT COUNT(*) FROM page_descendants d WHERE d.root=p.id AND d.id<>p.id),
 'playable_count',(SELECT COUNT(*) FROM page_descendants d JOIN items leaf ON leaf.id=d.id WHERE d.root=p.id AND leaf.kind IN ('movie','episode')),
 'watched_count',(SELECT COUNT(*) FROM page_descendants d JOIN items leaf ON leaf.id=d.id JOIN watch_state watch ON watch.item_id=leaf.id AND watch.user_id=(SELECT user_id FROM scope) AND watch.watched=1 WHERE d.root=p.id AND leaf.kind IN ('movie','episode')),
 'sources',json(COALESCE((SELECT json_group_array(json(source)) FROM (
   SELECT json_object('native_id',f.id,'wire_id',fm.wire_id,'size',f.size,'mtime',f.mtime,
     'duration_ms',f.duration_ms,'container',f.container,'video_codec',f.video_codec,'width',f.width,'height',f.height,
     'bit_depth',f.bit_depth,'hdr_format',f.hdr_format,'dv_profile',f.dv_profile,
     'streams',json(CASE WHEN json_valid(f.probe_json) THEN COALESCE(json_extract(f.probe_json,'$.streams'),'[]') ELSE '[]' END)) AS source
   FROM page_sources f LEFT JOIN jellyfin_entity_ids fm ON fm.entity_kind='file' AND fm.native_id=f.id AND fm.retired=0
   WHERE f.item_id=p.id ORDER BY f.id
 )), '[]'))) AS item
 FROM page p
 LEFT JOIN jellyfin_entity_ids wire ON wire.entity_kind='item' AND wire.native_id=p.id AND wire.retired=0
 LEFT JOIN jellyfin_entity_ids lib ON lib.entity_kind='library' AND lib.native_id=p.library_id AND lib.retired=0
 LEFT JOIN items a ON a.id=p.parent_id AND a.library_id=p.library_id AND a.kind IN ('movie','show','season','episode') LEFT JOIN items g ON g.id=a.parent_id AND g.library_id=p.library_id AND g.kind IN ('movie','show','season','episode')
 LEFT JOIN jellyfin_entity_ids parent_wire ON parent_wire.entity_kind='item' AND parent_wire.native_id=a.id AND parent_wire.retired=0
 LEFT JOIN jellyfin_entity_ids grand_wire ON grand_wire.entity_kind='item' AND grand_wire.native_id=g.id AND grand_wire.retired=0
 LEFT JOIN watch_state w ON w.item_id=p.id AND w.user_id=(SELECT user_id FROM scope)
)
SELECT json_object('authorized',json(CASE WHEN EXISTS(SELECT 1 FROM scope) THEN 'true' ELSE 'false' END),
 'parent_valid',json(CASE WHEN (json_extract($1,'$.parent_wire') IS NULL OR EXISTS(SELECT 1 FROM parent)) AND (json_extract($1,'$.item_wire') IS NULL OR EXISTS(SELECT 1 FROM target)) THEN 'true' ELSE 'false' END),
 'total',(SELECT COUNT(*) FROM selected),
 'source_overflow',json(CASE WHEN (SELECT COUNT(*) FROM page_sources)>5000 THEN 'true' ELSE 'false' END),
 'missing',json(COALESCE((SELECT json_group_array(json_object('kind',kind,'id',id)) FROM missing),'[]')),
 'items',json(CASE WHEN (SELECT COUNT(*) FROM page_sources)>5000 THEN '[]' ELSE COALESCE((SELECT json_group_array(json(item)) FROM projected),'[]') END)) AS result_json
"#;

pub(crate) const CATALOG_IDENTITY_SQL: &str = r#"
SELECT json_object('native_id',u.id,'wire_id',wire.wire_id,'name',u.username,
 'libraries',json(COALESCE((SELECT json_group_array(json(lib)) FROM (
   SELECT json_object('native_id',l.id,'wire_id',lm.wire_id,'name',l.name,'kind',l.kind) AS lib
   FROM libraries l LEFT JOIN jellyfin_entity_ids lm ON lm.entity_kind='library' AND lm.native_id=l.id AND lm.retired=0
   WHERE l.kind IN ('movies','shows') ORDER BY l.name COLLATE NOCASE,l.id LIMIT 501
 )), '[]'))) AS result_json
FROM users u JOIN tokens t ON t.user_id=u.id AND t.token_hash=$1
JOIN jellyfin_login_tokens login ON login.user_id=u.id AND login.token_hash=t.token_hash
LEFT JOIN jellyfin_entity_ids wire ON wire.entity_kind='user' AND wire.native_id=u.id AND wire.retired=0
"#;

pub(crate) const CATALOG_ARTWORK_SQL: &str = r#"
WITH input AS (SELECT $1 AS wire,$2 AS backdrop), candidate AS (
 SELECT wire.wire_id,CASE WHEN input.backdrop THEN i.backdrop_path ELSE i.poster_path END AS filename,
   (SELECT value FROM settings WHERE key='compat.jellyfin.generation') AS generation
 FROM input JOIN jellyfin_entity_ids wire ON wire.wire_id=input.wire AND wire.entity_kind='item' AND wire.retired=0
 JOIN items i ON i.id=wire.native_id JOIN libraries l ON l.id=i.library_id
 WHERE l.kind IN ('movies','shows') AND i.kind IN ('movie','show','season','episode')
 AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on')
)
SELECT json_object('wire_id',wire_id,'filename',filename,'generation',generation) AS result_json
FROM candidate WHERE filename IS NOT NULL AND generation IS NOT NULL
"#;
