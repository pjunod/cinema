//! Read-only committed captions under a typed Source-grant principal. Caller
//! authenticates the grant and exact file witness before and after each read.
//! These transactions can retain bytes; none can touch demands or work jobs.
use super::background_jobs::{decode, encode, QueueSql};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrantCaptionPrincipal {
    pub grant_id: String,
    /// Digest of the exact SourcePlaybackTarget plus grant, including revision.
    pub binding_sha256: String,
    /// Source-local catalogue identity, used only inside Source SQL.
    pub file_id: i64,
}
#[derive(Clone, Debug)]
pub enum GrantCaptionQuery {
    Assets {
        after: Option<String>,
    },
    Asset {
        asset_id: String,
    },
    AcquireLease {
        asset_id: String,
    },
    Lease {
        asset_id: String,
        lease_id: String,
    },
    Window {
        asset_id: String,
        lease_id: String,
        position_ms: i64,
    },
    Segment {
        asset_id: String,
        lease_id: String,
        segment_id: String,
    },
}
#[derive(Clone, Debug)]
pub struct GrantCaptionRead {
    pub principal: GrantCaptionPrincipal,
    pub now_ms: i64,
    pub query: GrantCaptionQuery,
}
const SCOPE: &str = r#"a.file_id=json_extract($1,'$.principal.file_id') AND a.retired_at_ms IS NULL
 AND EXISTS(SELECT 1 FROM files f WHERE f.id=a.file_id AND f.size=json_extract(a.source_json,'$.source_size') AND f.mtime=json_extract(a.source_json,'$.source_mtime') AND f.probe_json IS NOT NULL)"#;
const LEASE: &str = r#"l.asset_id=a.id AND l.id=json_extract($1,'$.lease_id') AND l.grant_id=json_extract($1,'$.principal.grant_id')
 AND l.principal=json_extract($1,'$.principal.binding_sha256') AND l.file_id=a.file_id AND l.expires_at_ms>json_extract($1,'$.now_ms')"#;
const ASSET: &str = r#"json_object('asset_id',a.id,'file_id',CAST(a.file_id AS TEXT),'content_revision',a.source_sha256,
 'profile_id',a.profile_id,'model_id',json_extract(a.profile_json,'$.model_id'),'model_sha256',json_extract(a.profile_json,'$.model_sha256'),
 'task',COALESCE(json_extract(a.profile_json,'$.task'),'translate'),'audio_index',a.audio_index,'spoken_language',a.spoken_language,
 'output_language',CASE WHEN json_extract(a.profile_json,'$.task')='transcribe' THEN a.spoken_language ELSE 'en' END,
 'duration_ms',a.duration_ms,'manifest_revision',CAST(a.manifest_revision AS TEXT),'state',op.state,'stopped',op.stopped,'reason',op.reason)"#;
fn bad() -> StoreError {
    StoreError::Task("invalid Shared caption read".into())
}
fn digest(value: &str) -> bool {
    super::subtitle_translation::valid_digest(value)
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

pub(super) async fn read<T: QueueSql>(
    store: &T,
    input: GrantCaptionRead,
) -> Result<Vec<Value>, StoreError> {
    if !uuid(&input.principal.grant_id)
        || !digest(&input.principal.binding_sha256)
        || input.principal.file_id < 0
        || input.now_ms < 0
        || input.now_ms.checked_add(30_000).is_none()
    {
        return Err(bad());
    }
    let mut body = json!({"principal":input.principal,"now_ms":input.now_ms});
    let sql = match input.query {
        GrantCaptionQuery::Assets { after } => {
            if after.as_deref().is_some_and(|value| !digest(value)) {
                return Err(bad());
            }
            body["after"] = json!(after);
            format!("SELECT {ASSET} AS result_json FROM subtitle_translation_assets a JOIN subtitle_translation_operations op ON op.id=a.operation_id WHERE {SCOPE} AND a.accepted_bytes>0 AND (json_extract($1,'$.after') IS NULL OR a.id>json_extract($1,'$.after')) ORDER BY a.id LIMIT 17")
        }
        GrantCaptionQuery::Asset { asset_id } => {
            if !digest(&asset_id) {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            format!("SELECT {ASSET} AS result_json FROM subtitle_translation_assets a JOIN subtitle_translation_operations op ON op.id=a.operation_id WHERE {SCOPE} AND a.id=json_extract($1,'$.asset_id')")
        }
        GrantCaptionQuery::AcquireLease { asset_id } => {
            if !digest(&asset_id) {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            return lease(store, body).await;
        }
        GrantCaptionQuery::Lease { asset_id, lease_id } => {
            bind(&mut body, asset_id, lease_id)?;
            format!("SELECT json_object('lease_id',l.id,'revision',CAST(l.manifest_revision AS TEXT),'expires_at_ms',l.expires_at_ms) AS result_json FROM shared_caption_read_leases l JOIN subtitle_translation_assets a ON a.id=l.asset_id WHERE {SCOPE} AND {LEASE}")
        }
        GrantCaptionQuery::Window {
            asset_id,
            lease_id,
            position_ms,
        } => {
            bind(&mut body, asset_id, lease_id)?;
            if !(0..=14_400_000).contains(&position_ms) {
                return Err(bad());
            }
            body["low"] = (position_ms - 30_000).max(0).into();
            body["high"] = position_ms.saturating_add(90_000).into();
            format!("SELECT json_object('segment_id',s.segment_id,'digest',s.digest,'start_ms',s.start_ms,'end_ms',s.end_ms) AS result_json FROM subtitle_translation_segments s JOIN subtitle_translation_assets a ON a.id=s.asset_id JOIN shared_caption_read_leases l ON l.asset_id=a.id WHERE {SCOPE} AND {LEASE} AND s.accepted_revision<=l.manifest_revision AND s.end_ms>json_extract($1,'$.low') AND s.start_ms<json_extract($1,'$.high') ORDER BY s.core_index LIMIT 9")
        }
        GrantCaptionQuery::Segment {
            asset_id,
            lease_id,
            segment_id,
        } => {
            bind(&mut body, asset_id, lease_id)?;
            if !digest(&segment_id) {
                return Err(bad());
            }
            body["segment_id"] = segment_id.into();
            format!("SELECT json_object('body',s.body,'digest',s.digest) AS result_json FROM subtitle_translation_segments s JOIN subtitle_translation_assets a ON a.id=s.asset_id JOIN shared_caption_read_leases l ON l.asset_id=a.id WHERE {SCOPE} AND {LEASE} AND s.accepted_revision<=l.manifest_revision AND s.segment_id=json_extract($1,'$.segment_id')")
        }
    };
    store
        .queue_sql(sql, encode(&body)?, false, true)
        .await?
        .iter()
        .map(|row| decode(row))
        .collect()
}
fn bind(body: &mut Value, asset: String, lease: String) -> Result<(), StoreError> {
    if !digest(&asset) || !uuid(&lease) {
        return Err(bad());
    }
    body["asset_id"] = asset.into();
    body["lease_id"] = lease.into();
    Ok(())
}
async fn lease<T: QueueSql>(store: &T, mut body: Value) -> Result<Vec<Value>, StoreError> {
    body["lease_id"] = uuid::Uuid::new_v4().to_string().into();
    let encoded = encode(&body)?;
    store.queue_transaction(vec![
        ("DELETE FROM shared_caption_read_leases WHERE id IN(SELECT id FROM shared_caption_read_leases WHERE expires_at_ms<=json_extract($1,'$.now_ms') ORDER BY expires_at_ms LIMIT 128)".into(),encoded.clone()),
        ("DELETE FROM subtitle_translation_snapshots WHERE (asset_id,manifest_revision) IN(SELECT s.asset_id,s.manifest_revision FROM subtitle_translation_snapshots s WHERE s.created_at_ms<json_extract($1,'$.now_ms')-120000 AND NOT EXISTS(SELECT 1 FROM subtitle_translation_read_leases l WHERE l.asset_id=s.asset_id AND l.manifest_revision=s.manifest_revision AND l.expires_at_ms>json_extract($1,'$.now_ms')) AND NOT EXISTS(SELECT 1 FROM shared_caption_read_leases g WHERE g.asset_id=s.asset_id AND g.manifest_revision=s.manifest_revision AND g.expires_at_ms>json_extract($1,'$.now_ms')) ORDER BY s.created_at_ms LIMIT 128)".into(),encoded.clone()),
        (format!("INSERT INTO subtitle_translation_snapshots(asset_id,manifest_revision,created_at_ms) SELECT a.id,a.manifest_revision,json_extract($1,'$.now_ms') FROM subtitle_translation_assets a WHERE {SCOPE} AND a.id=json_extract($1,'$.asset_id') AND (SELECT count(*) FROM subtitle_translation_snapshots WHERE asset_id=a.id)<16 ON CONFLICT DO NOTHING"),encoded.clone()),
    ]).await?;
    let sql = format!(
        r#"INSERT INTO shared_caption_read_leases(id,grant_id,principal,file_id,asset_id,manifest_revision,expires_at_ms,created_at_ms)
 SELECT COALESCE((SELECT id FROM shared_caption_read_leases WHERE asset_id=a.id AND grant_id=json_extract($1,'$.principal.grant_id') AND file_id=a.file_id AND principal=json_extract($1,'$.principal.binding_sha256') AND manifest_revision=a.manifest_revision AND expires_at_ms>json_extract($1,'$.now_ms')+5000 LIMIT 1),json_extract($1,'$.lease_id')),
 json_extract($1,'$.principal.grant_id'),json_extract($1,'$.principal.binding_sha256'),a.file_id,a.id,a.manifest_revision,json_extract($1,'$.now_ms')+30000,json_extract($1,'$.now_ms')
 FROM subtitle_translation_assets a WHERE {SCOPE} AND a.id=json_extract($1,'$.asset_id') AND EXISTS(SELECT 1 FROM subtitle_translation_snapshots WHERE asset_id=a.id AND manifest_revision=a.manifest_revision)
 AND (EXISTS(SELECT 1 FROM shared_caption_read_leases WHERE asset_id=a.id AND grant_id=json_extract($1,'$.principal.grant_id') AND file_id=a.file_id AND principal=json_extract($1,'$.principal.binding_sha256') AND manifest_revision=a.manifest_revision AND expires_at_ms>json_extract($1,'$.now_ms')+5000) OR ((SELECT count(*) FROM shared_caption_read_leases WHERE expires_at_ms>json_extract($1,'$.now_ms'))<256 AND (SELECT count(*) FROM shared_caption_read_leases WHERE principal=json_extract($1,'$.principal.binding_sha256') AND expires_at_ms>json_extract($1,'$.now_ms'))<16))
 ON CONFLICT(id) DO UPDATE SET id=excluded.id RETURNING json_object('lease_id',id,'revision',CAST(manifest_revision AS TEXT),'expires_at_ms',expires_at_ms) AS result_json"#
    );
    let result = store.queue_sql(sql, encoded.clone(), true, true).await?;
    store.queue_sql(format!("UPDATE subtitle_translation_assets AS a SET last_read_at_ms=json_extract($1,'$.now_ms') WHERE {SCOPE} AND a.id=json_extract($1,'$.asset_id')"),encoded,true,false).await?;
    result.iter().map(|row| decode(row)).collect()
}
