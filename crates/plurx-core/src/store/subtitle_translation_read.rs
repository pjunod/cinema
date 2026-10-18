//! Bounded source-fenced observations. Reads never create worker demand.
use super::background_jobs::{decode, encode, QueueSql};
use crate::error::StoreError;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum TranslationQuery {
    RequestReceipt {
        request_id: String,
        request_digest: String,
    },
    Operation {
        operation_id: String,
    },
    Assets {
        file_id: i64,
        after: Option<String>,
    },
    Asset {
        asset_id: String,
    },
    SourceBinding {
        asset_id: String,
        node_id: String,
    },
    Segments {
        asset_id: String,
        lease_id: String,
        after_core: i64,
        through_core: i64,
    },
    Coverage {
        asset_id: String,
        lease_id: String,
        after_start_ms: i64,
        window_start_ms: i64,
        window_end_ms: i64,
    },
    Segment {
        asset_id: String,
        lease_id: String,
        segment_id: String,
    },
    Lease {
        asset_id: String,
        lease_id: String,
    },
    FailedRanges {
        operation_id: String,
        lease_id: String,
        after_core: i64,
        expected_operation_epoch: i64,
        expected_view_revision: i64,
    },
    Capabilities {
        node_id: String,
    },
    LiveDemands {
        operation_id: String,
        operation_epoch: i64,
    },
    CoreState {
        operation_id: String,
        operation_epoch: i64,
        core_index: i64,
    },
    WorkerOperations {
        node_id: String,
        after: Option<String>,
    },
}
#[derive(Debug, Clone)]
pub struct TranslationRead {
    /// Zero is permitted only for the service's operation/source-binding worker
    /// observations. HTTP routes always pass the authenticated positive user ID.
    pub user_id: i64,
    pub now_ms: i64,
    pub query: TranslationQuery,
}
#[derive(Debug, Clone)]
pub struct AcquireTranslationLease {
    pub user_id: i64,
    pub asset_id: String,
    pub now_ms: i64,
}
const LIVE_ASSET: &str = r#"a.id=json_extract($1,'$.asset_id') AND a.retired_at_ms IS NULL
 AND EXISTS(SELECT 1 FROM files f WHERE f.id=a.file_id AND f.size=json_extract(a.source_json,'$.source_size') AND f.mtime=json_extract(a.source_json,'$.source_mtime') AND f.probe_json IS NOT NULL)"#;
const USER: &str = "EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.user_id'))";
const LEASE: &str = r#"l.id=json_extract($1,'$.lease_id') AND l.user_id=json_extract($1,'$.user_id')
 AND l.asset_id=json_extract($1,'$.asset_id') AND l.expires_at_ms>json_extract($1,'$.now_ms')"#;
const ASSET_JSON: &str = r#"json_object('asset_id',a.id,'file_id',CAST(a.file_id AS TEXT),'source_size',CAST(json_extract(a.source_json,'$.source_size') AS TEXT),'source_mtime',CAST(json_extract(a.source_json,'$.source_mtime') AS TEXT),'content_revision',a.source_sha256,'profile_id',a.profile_id,'audio_index',a.audio_index,'spoken_language',a.spoken_language,'output_language','en','timeline_contract',json_extract(a.profile_json,'$.timeline_contract'),'manifest_revision',CAST(a.manifest_revision AS TEXT),'duration_ms',a.duration_ms,'core_count',a.core_count,'operation_id',a.operation_id)"#;
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}
fn digest(value: &str) -> bool {
    super::subtitle_translation::valid_digest(value)
}
fn bad() -> StoreError {
    StoreError::Task("invalid translation read".into())
}
pub(super) async fn read<T: QueueSql>(
    store: &T,
    input: TranslationRead,
) -> Result<Vec<Value>, StoreError> {
    if input.user_id < 0 || input.now_ms < 0 {
        return Err(bad());
    }
    let mut body = json!({"user_id":input.user_id,"now_ms":input.now_ms});
    let sql = match input.query {
        TranslationQuery::RequestReceipt {
            request_id,
            request_digest,
        } => {
            if input.user_id <= 0 || !uuid(&request_id) || !digest(&request_digest) {
                return Err(bad());
            }
            body["request_id"] = request_id.into();
            body["request_digest"] = request_digest.into();
            "SELECT CASE WHEN r.body_digest=json_extract($1,'$.request_digest') THEN r.outcome_json ELSE json_object('outcome','request_conflict') END AS result_json FROM subtitle_translation_requests r WHERE r.user_id=json_extract($1,'$.user_id') AND r.request_id=json_extract($1,'$.request_id') AND EXISTS(SELECT 1 FROM users WHERE id=r.user_id AND is_admin=1)".into()
        }

        TranslationQuery::Operation { operation_id } => {
            if !uuid(&operation_id) {
                return Err(bad());
            }
            body["operation_id"] = operation_id.into();
            format!(
                r#"SELECT json_object('version',1,'operation_id',op.id,'operation_epoch',CAST(op.operation_epoch AS TEXT),'view_revision',CAST(op.view_revision AS TEXT),
 'state',CASE WHEN op.stopped=0 AND op.state NOT IN ('failed','complete') AND NOT EXISTS(SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now_ms') AND u.is_admin=1) THEN 'suspended' ELSE op.state END,
 'reason',op.reason,'retryable',json(CASE WHEN op.retryable=1 THEN 'true' ELSE 'false' END),'asset_id',op.asset_id,'manifest_revision',(SELECT CAST(manifest_revision AS TEXT) FROM subtitle_translation_assets WHERE id=op.asset_id),
 'file_id',CAST(op.file_id AS TEXT),'source_size',CAST(json_extract(op.source_json,'$.source_size') AS TEXT),'source_mtime',CAST(json_extract(op.source_json,'$.source_mtime') AS TEXT),'content_revision',(SELECT source_sha256 FROM subtitle_translation_assets WHERE id=op.asset_id),'audio_index',op.audio_index,'spoken_language',op.spoken_language,'output_language','en','task','translate',
 'model_id',json_extract(op.profile_json,'$.model_id'),'model_sha256',json_extract(op.profile_json,'$.model_sha256'),'profile',json(op.profile_json),'timeline_contract',json_extract(op.profile_json,'$.timeline_contract'),
 'preparation_job_id',op.preparation_job_id,'target_node_id',op.target_node_id,
 'active_job',json((SELECT json_object('job_id',j.id,'kind',j.kind,'state',j.state,'attempt',CAST(j.fence AS TEXT),'fence',CAST(j.fence AS TEXT),'phase',json_extract(j.checkpoint_json,'$.phase'),'core_index',json_extract(j.payload_json,'$.recipe.core_index'),'owner_node_id',j.owner_node_id,'cancellable',json('true')) FROM background_jobs j WHERE j.state IN ('queued','running','cancelling') AND COALESCE(json_extract(j.payload_json,'$.operation_id'),json_extract(j.payload_json,'$.recipe.operation_id'))=op.id AND COALESCE(json_extract(j.payload_json,'$.operation_epoch'),json_extract(j.payload_json,'$.recipe.operation_epoch'))=op.operation_epoch ORDER BY CASE j.state WHEN 'running' THEN 0 WHEN 'cancelling' THEN 1 ELSE 2 END,j.created_at_ms LIMIT 1)),
 'server_now_ms',json_extract($1,'$.now_ms')) AS result_json FROM subtitle_translation_operations op WHERE op.id=json_extract($1,'$.operation_id') AND (json_extract($1,'$.user_id')=0 OR {USER})"#
            )
        }
        TranslationQuery::Assets { file_id, after } => {
            if input.user_id <= 0 || file_id <= 0 || after.as_deref().is_some_and(|v| !digest(v)) {
                return Err(bad());
            }
            body["file_id"] = file_id.into();
            body["after"] = json!(after);
            format!("SELECT {ASSET_JSON} AS result_json FROM subtitle_translation_assets a WHERE a.file_id=json_extract($1,'$.file_id') AND a.retired_at_ms IS NULL AND (json_extract($1,'$.after') IS NULL OR a.id>json_extract($1,'$.after')) AND EXISTS(SELECT 1 FROM files f WHERE f.id=a.file_id AND f.size=json_extract(a.source_json,'$.source_size') AND f.mtime=json_extract(a.source_json,'$.source_mtime')) AND {USER} ORDER BY a.id LIMIT 17")
        }
        TranslationQuery::Asset { asset_id } => {
            if !digest(&asset_id) {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            format!("SELECT {ASSET_JSON} AS result_json FROM subtitle_translation_assets a WHERE {LIVE_ASSET} AND (json_extract($1,'$.user_id')=0 OR {USER})")
        }
        TranslationQuery::SourceBinding { asset_id, node_id } => {
            if input.user_id != 0 || !digest(&asset_id) || node_id.is_empty() || node_id.len() > 128
            {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            body["node_id"] = node_id.into();
            format!(
                r#"SELECT json_object('operation_id',op.id,'operation_epoch',op.operation_epoch,'source',json(a.source_json),'source_sha256',a.source_sha256,'local_identity',b.local_identity,'boot_id',b.boot_id,'verified_at_ms',b.verified_at_ms) AS result_json FROM subtitle_translation_assets a JOIN subtitle_translation_operations op ON op.id=a.operation_id JOIN subtitle_translation_source_bindings b ON b.operation_id=op.id AND b.source_sha256=a.source_sha256 WHERE {LIVE_ASSET} AND b.node_id=json_extract($1,'$.node_id')"#
            )
        }
        TranslationQuery::Segments {
            asset_id,
            lease_id,
            after_core,
            through_core,
        } => {
            if input.user_id <= 0
                || !digest(&asset_id)
                || !uuid(&lease_id)
                || !(-1..2048).contains(&after_core)
                || through_core < after_core
                || through_core > 2047
                || through_core - after_core > 8
            {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            body["lease_id"] = lease_id.into();
            body["after"] = after_core.into();
            body["through"] = through_core.into();
            format!("SELECT json_object('segment_id',s.segment_id,'digest',s.digest,'core_index',s.core_index,'start_ms',s.start_ms,'end_ms',s.end_ms) AS result_json FROM subtitle_translation_segments s JOIN subtitle_translation_assets a ON a.id=s.asset_id JOIN subtitle_translation_read_leases l ON l.asset_id=a.id WHERE {LIVE_ASSET} AND {LEASE} AND {USER} AND s.accepted_revision<=l.manifest_revision AND s.core_index>json_extract($1,'$.after') AND s.core_index<=json_extract($1,'$.through') ORDER BY s.core_index LIMIT 65")
        }
        TranslationQuery::Coverage {
            asset_id,
            lease_id,
            after_start_ms,
            window_start_ms,
            window_end_ms,
        } => {
            if input.user_id <= 0
                || !digest(&asset_id)
                || !uuid(&lease_id)
                || !(-1..=14400000).contains(&after_start_ms)
                || window_start_ms < 0
                || window_end_ms <= window_start_ms
                || window_end_ms > 14400000
                || window_end_ms - window_start_ms > 180000
            {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            body["lease_id"] = lease_id.into();
            body["after"] = after_start_ms.into();
            body["window_start"] = window_start_ms.into();
            body["window_end"] = window_end_ms.into();
            format!(
                r#"WITH available AS(SELECT s.start_ms,s.end_ms,lag(s.end_ms) OVER(ORDER BY s.start_ms) previous_end FROM subtitle_translation_segments s JOIN subtitle_translation_assets a ON a.id=s.asset_id JOIN subtitle_translation_read_leases l ON l.asset_id=a.id WHERE {LIVE_ASSET} AND {LEASE} AND {USER} AND s.accepted_revision<=l.manifest_revision), marked AS(SELECT *,sum(CASE WHEN previous_end=start_ms THEN 0 ELSE 1 END) OVER(ORDER BY start_ms) island FROM available), intervals AS(SELECT min(start_ms) start_ms,max(end_ms) end_ms FROM marked GROUP BY island) SELECT json_object('start_ms',start_ms,'end_ms',end_ms) AS result_json FROM intervals WHERE start_ms>json_extract($1,'$.after') AND end_ms>json_extract($1,'$.window_start') AND start_ms<json_extract($1,'$.window_end') ORDER BY start_ms LIMIT 65"#
            )
        }
        TranslationQuery::Lease { asset_id, lease_id } => {
            if input.user_id <= 0 || !digest(&asset_id) || !uuid(&lease_id) {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            body["lease_id"] = lease_id.into();
            format!("SELECT json_object('lease_id',l.id,'manifest_revision',CAST(l.manifest_revision AS TEXT),'expires_at_ms',l.expires_at_ms) AS result_json FROM subtitle_translation_read_leases l JOIN subtitle_translation_assets a ON a.id=l.asset_id WHERE {LIVE_ASSET} AND {LEASE} AND {USER}")
        }
        TranslationQuery::Segment {
            asset_id,
            lease_id,
            segment_id,
        } => {
            if input.user_id <= 0 || !digest(&asset_id) || !uuid(&lease_id) || !digest(&segment_id)
            {
                return Err(bad());
            }
            body["asset_id"] = asset_id.into();
            body["lease_id"] = lease_id.into();
            body["segment_id"] = segment_id.into();
            format!("SELECT json_object('body',s.body,'digest',s.digest,'lease_expires_at_ms',l.expires_at_ms) AS result_json FROM subtitle_translation_segments s JOIN subtitle_translation_assets a ON a.id=s.asset_id JOIN subtitle_translation_read_leases l ON l.asset_id=a.id WHERE {LIVE_ASSET} AND {LEASE} AND {USER} AND s.accepted_revision<=l.manifest_revision AND s.segment_id=json_extract($1,'$.segment_id')")
        }
        TranslationQuery::FailedRanges {
            operation_id,
            lease_id,
            after_core,
            expected_operation_epoch,
            expected_view_revision,
        } => {
            if input.user_id <= 0
                || !uuid(&operation_id)
                || !uuid(&lease_id)
                || !(-1..2048).contains(&after_core)
                || expected_operation_epoch <= 0
                || expected_view_revision <= 0
            {
                return Err(bad());
            }
            body["operation_id"] = operation_id.into();
            body["lease_id"] = lease_id.into();
            body["after"] = after_core.into();
            body["epoch"] = expected_operation_epoch.into();
            body["view"] = expected_view_revision.into();
            format!(
                r#"SELECT json_object('core_index',r.core_index,'revision',CAST(r.range_revision AS TEXT),'state',r.state,'reason',r.reason,'retryable',json(CASE WHEN r.retryable=1 THEN 'true' ELSE 'false' END),'attempts',r.attempts) AS result_json FROM subtitle_translation_operations op JOIN subtitle_translation_ranges r ON r.operation_id=op.id AND r.operation_epoch=op.operation_epoch JOIN subtitle_translation_read_leases l ON l.asset_id=op.asset_id WHERE op.id=json_extract($1,'$.operation_id') AND op.operation_epoch=json_extract($1,'$.epoch') AND op.view_revision=json_extract($1,'$.view') AND l.id=json_extract($1,'$.lease_id') AND l.user_id=json_extract($1,'$.user_id') AND l.expires_at_ms>json_extract($1,'$.now_ms') AND {USER} AND r.state='failed' AND r.core_index>json_extract($1,'$.after') ORDER BY r.core_index LIMIT 17"#
            )
        }
        TranslationQuery::Capabilities { node_id } => {
            if input.user_id != 0 || node_id.is_empty() || node_id.len() > 128 {
                return Err(bad());
            }
            body["node_id"] = node_id.into();
            "SELECT receipt_json AS result_json FROM subtitle_translation_capabilities WHERE node_id=json_extract($1,'$.node_id') ORDER BY model_id,profile_id LIMIT 16".into()
        }
        TranslationQuery::LiveDemands {
            operation_id,
            operation_epoch,
        } => {
            if input.user_id != 0 || !uuid(&operation_id) || operation_epoch <= 0 {
                return Err(bad());
            }
            body["operation_id"] = operation_id.into();
            body["epoch"] = operation_epoch.into();
            "SELECT json_object('demand_id',d.id,'generation',d.generation,'user_id',d.user_id,'position_ms',d.position_ms,'playback_rate',d.playback_rate,'paused',json(CASE WHEN d.paused=1 THEN 'true' ELSE 'false' END),'expires_at_ms',d.expires_at_ms) AS result_json FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id JOIN subtitle_translation_operations op ON op.id=d.operation_id AND op.operation_epoch=d.operation_epoch WHERE op.stopped=0 AND d.operation_id=json_extract($1,'$.operation_id') AND d.operation_epoch=json_extract($1,'$.epoch') AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now_ms') AND u.is_admin=1 ORDER BY d.user_id,d.id LIMIT 16".into()
        }
        TranslationQuery::CoreState {
            operation_id,
            operation_epoch,
            core_index,
        } => {
            if input.user_id != 0
                || !uuid(&operation_id)
                || operation_epoch <= 0
                || !(0..2048).contains(&core_index)
            {
                return Err(bad());
            }
            body["operation_id"] = operation_id.into();
            body["epoch"] = operation_epoch.into();
            body["core"] = core_index.into();
            "SELECT json_object('accepted',json(CASE WHEN EXISTS(SELECT 1 FROM subtitle_translation_segments s WHERE s.asset_id=op.asset_id AND s.core_index=json_extract($1,'$.core')) THEN 'true' ELSE 'false' END),'state',r.state,'job_id',r.job_id,'attempts',r.attempts,'range_revision',r.range_revision) AS result_json FROM subtitle_translation_operations op LEFT JOIN subtitle_translation_ranges r ON r.operation_id=op.id AND r.operation_epoch=op.operation_epoch AND r.core_index=json_extract($1,'$.core') WHERE op.id=json_extract($1,'$.operation_id') AND op.operation_epoch=json_extract($1,'$.epoch')".into()
        }
        TranslationQuery::WorkerOperations { node_id, after } => {
            if input.user_id != 0
                || node_id.is_empty()
                || node_id.len() > 128
                || after.as_deref().is_some_and(|v| !uuid(v))
            {
                return Err(bad());
            }
            body["node_id"] = node_id.into();
            body["after"] = json!(after);
            "SELECT json_object('operation_id',id,'operation_epoch',operation_epoch,'preparation_job_id',preparation_job_id,'asset_id',asset_id,'state',state,'source',json(source_json),'profile',json(profile_json),'audio_index',audio_index,'spoken_language',spoken_language,'duration_ms',duration_ms) AS result_json FROM subtitle_translation_operations o WHERE o.target_node_id=json_extract($1,'$.node_id') AND o.stopped=0 AND o.state NOT IN ('failed','complete') AND EXISTS(SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id WHERE d.operation_id=o.id AND d.operation_epoch=o.operation_epoch AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now_ms') AND u.is_admin=1) AND (json_extract($1,'$.after') IS NULL OR o.id>json_extract($1,'$.after')) ORDER BY o.id LIMIT 33".into()
        }
    };
    let rows = store.queue_sql(sql, encode(&body)?, false, true).await?;
    rows.iter().map(|row| decode(row)).collect()
}
pub(super) async fn lease<T: QueueSql>(
    store: &T,
    input: AcquireTranslationLease,
) -> Result<Option<Value>, StoreError> {
    if input.user_id <= 0
        || input.now_ms < 0
        || input.now_ms.checked_add(30000).is_none()
        || !digest(&input.asset_id)
    {
        return Err(bad());
    }
    let body = encode(
        &json!({"user_id":input.user_id,"now_ms":input.now_ms,"asset_id":input.asset_id,"lease_id":uuid::Uuid::new_v4().to_string()}),
    )?;
    store.queue_transaction(vec![
 ("DELETE FROM subtitle_translation_read_leases WHERE id IN(SELECT id FROM subtitle_translation_read_leases WHERE expires_at_ms<=json_extract($1,'$.now_ms') ORDER BY expires_at_ms LIMIT 128)".into(),body.clone()),
 ("DELETE FROM subtitle_translation_snapshots WHERE (asset_id,manifest_revision) IN(SELECT s.asset_id,s.manifest_revision FROM subtitle_translation_snapshots s WHERE s.created_at_ms<json_extract($1,'$.now_ms')-120000 AND NOT EXISTS(SELECT 1 FROM subtitle_translation_read_leases l WHERE l.asset_id=s.asset_id AND l.manifest_revision=s.manifest_revision AND l.expires_at_ms>json_extract($1,'$.now_ms')) ORDER BY s.created_at_ms LIMIT 128)".into(),body.clone()),
 (format!("INSERT INTO subtitle_translation_snapshots(asset_id,manifest_revision,created_at_ms) SELECT a.id,a.manifest_revision,json_extract($1,'$.now_ms') FROM subtitle_translation_assets a WHERE {LIVE_ASSET} AND {USER} AND (SELECT count(*) FROM subtitle_translation_snapshots WHERE asset_id=a.id)<16 ON CONFLICT DO NOTHING"),body.clone()),
 ]).await?;
    // Reuse never extends expiry. Every new lease snapshots one exact manifest;
    // clients renew explicitly before expiry even when their ETag is unchanged.
    let sql = format!(
        r#"INSERT INTO subtitle_translation_read_leases(id,user_id,asset_id,manifest_revision,expires_at_ms,created_at_ms)
 SELECT COALESCE((SELECT id FROM subtitle_translation_read_leases WHERE asset_id=a.id AND user_id=json_extract($1,'$.user_id') AND manifest_revision=a.manifest_revision AND expires_at_ms>json_extract($1,'$.now_ms')+5000 LIMIT 1),json_extract($1,'$.lease_id')),
 json_extract($1,'$.user_id'),a.id,a.manifest_revision,json_extract($1,'$.now_ms')+30000,json_extract($1,'$.now_ms') FROM subtitle_translation_assets a
 WHERE {LIVE_ASSET} AND {USER} AND EXISTS(SELECT 1 FROM subtitle_translation_snapshots WHERE asset_id=a.id AND manifest_revision=a.manifest_revision)
 AND (EXISTS(SELECT 1 FROM subtitle_translation_read_leases WHERE asset_id=a.id AND user_id=json_extract($1,'$.user_id') AND manifest_revision=a.manifest_revision AND expires_at_ms>json_extract($1,'$.now_ms')+5000) OR
 ((SELECT count(*) FROM subtitle_translation_read_leases)<256 AND (SELECT count(*) FROM subtitle_translation_read_leases WHERE user_id=json_extract($1,'$.user_id'))<16 AND (SELECT count(*) FROM subtitle_translation_read_leases WHERE user_id=json_extract($1,'$.user_id') AND created_at_ms>json_extract($1,'$.now_ms')-120000)<16))
 ON CONFLICT(id) DO UPDATE SET id=excluded.id
 RETURNING json_object('lease_id',id,'manifest_revision',CAST(manifest_revision AS TEXT),'expires_at_ms',expires_at_ms,'server_now_ms',json_extract($1,'$.now_ms')) AS result_json"#
    );
    store.queue_sql(format!("UPDATE subtitle_translation_assets AS a SET last_read_at_ms=json_extract($1,'$.now_ms') WHERE {LIVE_ASSET} AND {USER}"),body.clone(),true,false).await?;
    let rows = store.queue_sql(sql, body, true, true).await?;
    rows.first().map(|row| decode(row)).transpose()
}

/// Retire at most sixteen idle assets per pass. Stop/request tombstones remain.
/// Live demands and unexpired read leases protect bytes throughout their use.
pub(super) async fn maintain<T: QueueSql>(store: &T, now_ms: i64) -> Result<(), StoreError> {
    if now_ms < 0 {
        return Err(bad());
    }
    let body = encode(&json!({"now_ms":now_ms}))?;
    store.queue_transaction(vec![
        ("UPDATE subtitle_translation_assets SET retired_at_ms=json_extract($1,'$.now_ms'),manifest_revision=manifest_revision+1 WHERE manifest_revision<9223372036854775807 AND id IN(SELECT a.id FROM subtitle_translation_assets a WHERE a.retired_at_ms IS NULL AND (a.last_read_at_ms<json_extract($1,'$.now_ms')-86400000 OR (a.accepted_bytes=0 AND a.created_at_ms<json_extract($1,'$.now_ms')-3600000)) AND NOT EXISTS(SELECT 1 FROM subtitle_translation_read_leases l WHERE l.asset_id=a.id AND l.expires_at_ms>json_extract($1,'$.now_ms')) AND NOT EXISTS(SELECT 1 FROM subtitle_translation_demands d JOIN subtitle_translation_operations op ON op.id=d.operation_id WHERE op.id=a.operation_id AND op.operation_epoch=d.operation_epoch AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now_ms')) ORDER BY a.last_read_at_ms LIMIT 16)".into(),body.clone()),
        ("DELETE FROM subtitle_translation_segments WHERE json_extract($1,'$.now_ms')>=0 AND asset_id IN(SELECT id FROM subtitle_translation_assets WHERE retired_at_ms IS NOT NULL)".into(),body.clone()),
        ("UPDATE subtitle_translation_assets SET accepted_bytes=0 WHERE retired_at_ms IS NOT NULL AND json_extract($1,'$.now_ms')>=0".into(),body.clone()),
        ("UPDATE subtitle_translation_operations SET asset_id=NULL,state=CASE WHEN stopped=1 THEN 'stopped' ELSE 'suspended' END,view_revision=view_revision+1 WHERE asset_id IN(SELECT id FROM subtitle_translation_assets WHERE retired_at_ms IS NOT NULL) AND view_revision<9223372036854775807 AND json_extract($1,'$.now_ms')>=0".into(),body.clone()),
        ("DELETE FROM subtitle_translation_requests WHERE rowid IN(SELECT rowid FROM subtitle_translation_requests WHERE expires_at_ms<json_extract($1,'$.now_ms') ORDER BY expires_at_ms LIMIT 128)".into(),body)
    ]).await?;
    Ok(())
}
