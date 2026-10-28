//! Closed administrator mutations. The SQL verdict and effects use one writer snapshot.
use super::background_jobs::{
    decode, encode, enqueue_body, EnqueueJob, JobPayload, JobRequest, QueueSql, ENQUEUE_SQL,
};
use super::subtitle_translation::{digest, SourceRevision, TranslationProfile};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationMutation {
    pub user_id: i64,
    pub request_id: String,
    pub request_digest: String,
    pub intent_expires_at_ms: i64,
    pub now_ms: i64,
    pub action: TranslationAction,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranslationAction {
    Create {
        source: SourceRevision,
        profile: TranslationProfile,
        audio_index: i64,
        spoken_language: String,
        duration_ms: i64,
        target_node_id: String,
        playback_id: String,
        position_ms: i64,
        playback_rate: f64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuity_id: Option<String>,
    },
    Demand {
        operation_id: String,
        expected_operation_epoch: i64,
        demand_id: String,
        expected_generation: i64,
        playback_id: String,
        position_ms: i64,
        playback_rate: f64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuity_id: Option<String>,
    },
    Release {
        operation_id: String,
        expected_operation_epoch: i64,
        demand_id: String,
        expected_generation: i64,
        playback_id: String,
    },
    Stop {
        operation_id: String,
        expected_operation_epoch: i64,
    },
    RetryOperation {
        operation_id: String,
        expected_operation_epoch: i64,
        expected_view_revision: i64,
        expected_state: String,
        playback_id: String,
        position_ms: i64,
        playback_rate: f64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuity_id: Option<String>,
    },
    /// Internal reconciliation of an already-authorized live demand; never changes its root epoch.
    Retarget {
        operation_id: String,
        expected_operation_epoch: i64,
        expected_view_revision: i64,
        expected_preparation_job_id: String,
        expected_target_node_id: String,
        target_node_id: String,
    },
    RetryRange {
        operation_id: String,
        expected_operation_epoch: i64,
        core_index: i64,
        expected_range_revision: i64,
        playback_id: String,
        position_ms: i64,
        playback_rate: f64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuity_id: Option<String>,
    },
}
impl TranslationMutation {
    pub fn validate(&self) -> Result<(), StoreError> {
        let bad = || StoreError::Task("invalid translation mutation".into());
        if !super::subtitle_translation::valid_digest(&self.request_digest)
            || self.user_id <= 0
            || uuid::Uuid::parse_str(&self.request_id).is_err()
            || self.now_ms < 0
            || self.now_ms.checked_add(604800000).is_none()
            || self.intent_expires_at_ms < 0
        {
            return Err(bad());
        }
        let continuity = match &self.action {
            TranslationAction::Create { continuity_id, .. }
            | TranslationAction::Demand { continuity_id, .. }
            | TranslationAction::RetryOperation { continuity_id, .. }
            | TranslationAction::RetryRange { continuity_id, .. } => continuity_id.as_deref(),
            _ => None,
        };
        if continuity.is_some_and(|id| id.len() != 36 || uuid::Uuid::parse_str(id).is_err()) {
            return Err(bad());
        }
        let window = |position: &i64, rate: &f64, playback: &str| {
            *position >= 0
                && *position <= 14400000
                && rate.is_finite()
                && *rate > 0.0
                && *rate <= 4.0
                && uuid::Uuid::parse_str(playback).is_ok()
        };
        match &self.action {
            TranslationAction::Create {
                source,
                profile,
                audio_index,
                spoken_language,
                duration_ms,
                target_node_id,
                playback_id,
                position_ms,
                playback_rate,
                ..
            } => {
                source.validate()?;
                profile.validate()?;
                if !(0..=255).contains(audio_index)
                    || !super::background_jobs_transcription::whisper_language(spoken_language)
                    || (profile.task.is_translation() && spoken_language == "en")
                    || !(1..=14400000).contains(duration_ms)
                    || target_node_id.is_empty()
                    || target_node_id.len() > 128
                    || !window(position_ms, playback_rate, playback_id)
                {
                    return Err(bad());
                }
            }
            TranslationAction::Demand {
                operation_id,
                expected_operation_epoch,
                demand_id,
                expected_generation,
                playback_id,
                position_ms,
                playback_rate,
                ..
            } => {
                if uuid::Uuid::parse_str(operation_id).is_err()
                    || *expected_operation_epoch <= 0
                    || uuid::Uuid::parse_str(demand_id).is_err()
                    || *expected_generation <= 0
                    || !window(position_ms, playback_rate, playback_id)
                {
                    return Err(bad());
                }
            }
            TranslationAction::Release {
                operation_id,
                expected_operation_epoch,
                demand_id,
                expected_generation,
                playback_id,
            } => {
                if uuid::Uuid::parse_str(operation_id).is_err()
                    || *expected_operation_epoch <= 0
                    || uuid::Uuid::parse_str(demand_id).is_err()
                    || *expected_generation <= 0
                    || uuid::Uuid::parse_str(playback_id).is_err()
                {
                    return Err(bad());
                }
            }
            TranslationAction::Stop {
                operation_id,
                expected_operation_epoch,
            } => {
                if uuid::Uuid::parse_str(operation_id).is_err() || *expected_operation_epoch <= 0 {
                    return Err(bad());
                }
            }
            TranslationAction::RetryOperation {
                operation_id,
                expected_operation_epoch,
                expected_view_revision,
                expected_state,
                playback_id,
                position_ms,
                playback_rate,
                ..
            } => {
                if uuid::Uuid::parse_str(operation_id).is_err()
                    || *expected_operation_epoch <= 0
                    || *expected_view_revision <= 0
                    || !matches!(expected_state.as_str(), "stopped" | "failed")
                    || !window(position_ms, playback_rate, playback_id)
                {
                    return Err(bad());
                }
            }
            TranslationAction::Retarget {
                operation_id,
                expected_operation_epoch,
                expected_view_revision,
                expected_preparation_job_id,
                expected_target_node_id,
                target_node_id,
            } => {
                if uuid::Uuid::parse_str(operation_id).is_err()
                    || *expected_operation_epoch <= 0
                    || *expected_view_revision <= 0
                    || uuid::Uuid::parse_str(expected_preparation_job_id).is_err()
                    || [expected_target_node_id, target_node_id]
                        .iter()
                        .any(|node| node.is_empty() || node.len() > 128)
                {
                    return Err(bad());
                }
            }
            TranslationAction::RetryRange {
                operation_id,
                expected_operation_epoch,
                core_index,
                expected_range_revision,
                playback_id,
                position_ms,
                playback_rate,
                ..
            } => {
                if uuid::Uuid::parse_str(operation_id).is_err()
                    || *expected_operation_epoch <= 0
                    || *expected_range_revision <= 0
                    || !(0..2048).contains(core_index)
                    || !window(position_ms, playback_rate, playback_id)
                {
                    return Err(bad());
                }
            }
        }
        Ok(())
    }
}
const MUTATE: &str = r#"WITH input AS MATERIALIZED(SELECT json($1) body), prior AS MATERIALIZED(
 SELECT body,(SELECT outcome_json FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id') AND request_id=json_extract(body,'$.request_id')) previous,
 (SELECT body_digest FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id') AND request_id=json_extract(body,'$.request_id')) previous_digest FROM input),
 selected AS MATERIALIZED(SELECT *,COALESCE(json_extract(body,'$.action.operation_id'),(SELECT id FROM subtitle_translation_operations WHERE recipe_key=json_extract(body,'$.recipe_key')),json_extract(body,'$.allocated_operation_id')) op_id FROM prior),
 classified AS MATERIALIZED(SELECT *,CASE
 WHEN NOT EXISTS(SELECT 1 FROM users WHERE id=json_extract(body,'$.user_id') AND is_admin=1) THEN 'forbidden'
 WHEN previous IS NOT NULL AND previous_digest!=json_extract(body,'$.body_digest') THEN 'request_conflict'
 WHEN previous IS NOT NULL THEN 'replay'
 WHEN json_extract(body,'$.intent_expires_at_ms')<=json_extract(body,'$.now_ms') OR json_extract(body,'$.intent_expires_at_ms')>json_extract(body,'$.now_ms')+300000 THEN 'intent_expired'
 WHEN (SELECT count(*) FROM subtitle_translation_requests)>=CASE WHEN json_extract(body,'$.action.action') IN ('stop','release') THEN 16384 ELSE 16352 END OR (SELECT count(*) FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id'))>=CASE WHEN json_extract(body,'$.action.action') IN ('stop','release') THEN 1024 ELSE 1016 END
 OR (SELECT COALESCE(sum(length(CAST(outcome_json AS BLOB))),0) FROM subtitle_translation_requests)+2048>=CASE WHEN json_extract(body,'$.action.action') IN ('stop','release') THEN 8388608 ELSE 8323072 END
 OR (SELECT COALESCE(sum(length(CAST(outcome_json AS BLOB))),0) FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id'))+2048>=CASE WHEN json_extract(body,'$.action.action') IN ('stop','release') THEN 524288 ELSE 507904 END THEN 'receipt_quota'
 WHEN json_extract(body,'$.action.action') NOT IN ('stop','release') AND (SELECT count(*) FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id') AND created_at_ms>json_extract(body,'$.now_ms')-60000)>=60 THEN 'rate_limited'
 WHEN json_extract(body,'$.action.action') IN ('create','retry_operation','retry_range','retarget') AND NOT EXISTS(SELECT 1 FROM settings WHERE key='subtitles.translation.config.v1' AND CASE WHEN json_valid(value) THEN json_extract(value,'$.version')=1 AND json_type(value,'$.enabled')='true' ELSE 0 END) THEN 'disabled'
 WHEN json_extract(body,'$.action.action')='create' AND NOT EXISTS(SELECT 1 FROM files WHERE id=json_extract(body,'$.action.source.file_id') AND size=json_extract(body,'$.action.source.source_size') AND mtime=json_extract(body,'$.action.source.source_mtime') AND probe_json IS NOT NULL) THEN 'source_changed'
 WHEN json_extract(body,'$.action.action') IN ('create','retry_operation','retry_range') AND NOT EXISTS(SELECT 1 FROM subtitle_translation_demands WHERE operation_id=op_id AND operation_epoch=(SELECT operation_epoch FROM subtitle_translation_operations WHERE id=op_id) AND user_id=json_extract(body,'$.user_id') AND playback_id=json_extract(body,'$.action.playback_id') AND released=0 AND expires_at_ms>json_extract(body,'$.now_ms')) AND (
 (SELECT count(*) FROM subtitle_translation_demands WHERE released=0 AND expires_at_ms>json_extract(body,'$.now_ms'))>=16 OR
 (SELECT count(*) FROM subtitle_translation_demands WHERE released=0 AND expires_at_ms>json_extract(body,'$.now_ms') AND user_id=json_extract(body,'$.user_id'))>=4 OR
 (SELECT count(*) FROM subtitle_translation_demands WHERE released=0 AND expires_at_ms>json_extract(body,'$.now_ms') AND operation_id=op_id)>=4) THEN 'demand_quota'
 WHEN json_extract(body,'$.action.action')='create' AND EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id AND target_node_id!=json_extract(body,'$.action.target_node_id')) THEN 'worker_not_current'
 WHEN json_extract(body,'$.action.action')='create' AND NOT EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id) AND (SELECT count(*) FROM subtitle_translation_operations)>=16384 THEN 'operation_quota'
 WHEN json_extract(body,'$.action.action')!='create' AND NOT EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id AND operation_epoch=json_extract(body,'$.action.expected_operation_epoch')) THEN 'state_conflict'
 WHEN EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id AND (operation_epoch>=9223372036854775806 OR view_revision>=9223372036854775806)) THEN 'counter_exhausted'
 WHEN json_extract(body,'$.action.action')='stop' AND EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id AND stopped=1) THEN 'already_stopped'
 WHEN json_extract(body,'$.action.action') IN ('create','demand') AND EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id AND (stopped=1 OR state='failed')) THEN 'stopped'
 WHEN json_extract(body,'$.action.action') IN ('demand','release') AND NOT EXISTS(SELECT 1 FROM subtitle_translation_demands WHERE id=json_extract(body,'$.action.demand_id') AND operation_id=op_id AND operation_epoch=json_extract(body,'$.action.expected_operation_epoch') AND generation=json_extract(body,'$.action.expected_generation') AND generation<9223372036854775807 AND user_id=json_extract(body,'$.user_id') AND playback_id=json_extract(body,'$.action.playback_id') AND released=0) THEN 'demand_conflict'
 WHEN json_extract(body,'$.action.action')='retarget' AND NOT EXISTS(SELECT 1 FROM subtitle_translation_operations op JOIN files f ON f.id=op.file_id WHERE op.id=op_id AND op.operation_epoch=json_extract(body,'$.action.expected_operation_epoch') AND op.view_revision=json_extract(body,'$.action.expected_view_revision') AND op.preparation_job_id=json_extract(body,'$.action.expected_preparation_job_id') AND op.target_node_id=json_extract(body,'$.action.expected_target_node_id') AND op.stopped=0 AND op.state NOT IN ('failed','stopped','complete') AND f.size=json_extract(op.source_json,'$.source_size') AND f.mtime=json_extract(op.source_json,'$.source_mtime') AND f.probe_json IS NOT NULL AND EXISTS(SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.user_id=json_extract(body,'$.user_id') AND d.released=0 AND d.expires_at_ms>json_extract(body,'$.now_ms') AND u.is_admin=1) AND NOT __READABLE_OPERATION__) THEN 'state_conflict'
 WHEN json_extract(body,'$.action.action')='retry_operation' AND NOT EXISTS(SELECT 1 FROM subtitle_translation_operations WHERE id=op_id AND view_revision=json_extract(body,'$.action.expected_view_revision') AND state=json_extract(body,'$.action.expected_state') AND (stopped=1 OR (state='failed' AND retryable=1))) THEN 'state_conflict'
 WHEN json_extract(body,'$.action.action')='retry_range' AND NOT EXISTS(SELECT 1 FROM subtitle_translation_operations op JOIN subtitle_translation_ranges r ON r.operation_id=op.id AND r.operation_epoch=op.operation_epoch WHERE op.id=op_id AND op.state='partial' AND op.stopped=0 AND r.core_index=json_extract(body,'$.action.core_index') AND r.range_revision=json_extract(body,'$.action.expected_range_revision') AND r.state='failed' AND r.retryable=1 AND r.attempts<3 AND r.range_revision<9223372036854775807) THEN 'range_conflict'
 ELSE 'accepted' END outcome FROM selected)
 INSERT INTO subtitle_translation_commands(id,operation,request_json,result_json)
 SELECT json_extract(body,'$.command_id'),'mutate',body,CASE WHEN outcome='replay' THEN previous ELSE json_object('outcome',outcome,'operation_id',op_id,'intent_expires_at_ms',json_extract(body,'$.intent_expires_at_ms'),'server_now_ms',json_extract(body,'$.now_ms')) END FROM classified RETURNING result_json"#;

#[derive(Deserialize)]
struct PreparationFacts {
    source: SourceRevision,
    profile: TranslationProfile,
    audio_index: i64,
    spoken_language: String,
    duration_ms: i64,
    target_node_id: String,
}
pub(super) async fn mutate<T: QueueSql>(
    store: &T,
    input: TranslationMutation,
) -> Result<Value, StoreError> {
    input.validate()?;
    let body_digest = input.request_digest.clone();
    let mut body = serde_json::to_value(&input).map_err(|e| StoreError::Task(e.to_string()))?;
    body["body_digest"] = body_digest.into();
    body["command_id"] = uuid::Uuid::new_v4().to_string().into();
    body["allocated_operation_id"] = uuid::Uuid::new_v4().to_string().into();
    body["allocated_demand_id"] = uuid::Uuid::new_v4().to_string().into();
    body["allocated_job_id"] = uuid::Uuid::new_v4().to_string().into();
    let mut statements = vec![];
    // At most128 rows, wholly expired beyond the maximum replayable intent. Root
    // Stop tombstones are independent and never removed by receipt retirement.
    statements.push(("DELETE FROM subtitle_translation_requests WHERE rowid IN (SELECT rowid FROM subtitle_translation_requests WHERE expires_at_ms<json_extract($1,'$.now_ms') ORDER BY expires_at_ms LIMIT 128)".into(),encode(&body)?));
    statements.push(("DELETE FROM subtitle_translation_commands WHERE id IN (SELECT id FROM subtitle_translation_commands WHERE json_extract(request_json,'$.now_ms')<json_extract($1,'$.now_ms')-300000 LIMIT 128)".into(),encode(&body)?));
    statements.push((
        MUTATE.replace(
            "__READABLE_OPERATION__",
            super::subtitle_translation_read::READABLE_OPERATION,
        ),
        encode(&body)?,
    ));
    let preparation = match &input.action {
        TranslationAction::Create {
            source,
            profile,
            audio_index,
            spoken_language,
            duration_ms,
            target_node_id,
            ..
        } => Some(PreparationFacts {
            source: source.clone(),
            profile: profile.clone(),
            audio_index: *audio_index,
            spoken_language: spoken_language.clone(),
            duration_ms: *duration_ms,
            target_node_id: target_node_id.clone(),
        }),
        TranslationAction::RetryOperation { operation_id, .. }
        | TranslationAction::Retarget { operation_id, .. } => {
            let rows=store.queue_sql("SELECT json_object('source',json(source_json),'profile',json(profile_json),'audio_index',audio_index,'spoken_language',spoken_language,'duration_ms',duration_ms,'target_node_id',target_node_id) AS result_json FROM subtitle_translation_operations WHERE id=json_extract($1,'$.operation_id') AND EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.user_id') AND is_admin=1)".into(),encode(&json!({"operation_id":operation_id,"user_id":input.user_id}))?,false,true).await?;
            let mut facts = rows
                .first()
                .map(|r| decode::<PreparationFacts>(r))
                .transpose()?;
            if let (Some(facts), TranslationAction::Retarget { target_node_id, .. }) =
                (&mut facts, &input.action)
            {
                facts.target_node_id = target_node_id.clone();
            }
            facts
        }
        _ => None,
    };
    if let Some(PreparationFacts {
        source,
        profile,
        audio_index,
        spoken_language,
        duration_ms,
        target_node_id,
    }) = preparation
    {
        let operation_id = body["allocated_operation_id"]
            .as_str()
            .expect("owned operation UUID")
            .to_owned();
        let recipe_key = digest(
            &json!({"source":source,"profile":profile,"audio_index":audio_index,"spoken_language":spoken_language,"duration_ms":duration_ms}),
        )?;
        body["recipe_key"] = recipe_key.into();
        // Replace the just-queued mutation with its complete immutable recipe input.
        statements[2].1 = encode(&body)?;
        let request = EnqueueJob {
            id: body["allocated_job_id"]
                .as_str()
                .expect("owned job UUID")
                .to_owned(),
            payload: JobPayload::SubtitleTranslationPrepare {
                file_id: source.file_id,
                source_size: source.source_size,
                source_mtime: source.source_mtime,
                operation_id,
                operation_epoch: 1,
                profile: profile.clone(),
                audio_index,
                spoken_language: spoken_language.clone(),
                duration_ms,
                target_node_id: target_node_id.clone(),
            },
            dedupe_key: format!("translation-prep-{}", input.request_id),
            priority: 1,
            not_before_ms: input.now_ms,
            now_ms: input.now_ms,
            request: JobRequest {
                scope: "subtitle-translation".into(),
                request_id: input.request_id.clone(),
                request_digest: body["body_digest"]
                    .as_str()
                    .expect("owned digest")
                    .to_owned(),
                consumer_kind: "subtitle_translation".into(),
                consumer_ref: "translation".into(),
                target_node_id: Some(target_node_id.clone()),
                deadline_ms: None,
                retain_identity: false,
            },
        };
        request.validate()?;
        let mut enqueue = enqueue_body(store, &request).await?;
        enqueue["translation_user_id"] = input.user_id.into();
        enqueue["translation_request_id"] = input.request_id.clone().into();
        let provided = r#"WITH provided AS (SELECT json_set($1,'$.id',op.preparation_job_id,'$.payload.operation_id',op.id,'$.payload.operation_epoch',op.operation_epoch,'$.dedupe_key','translation-prep-'||op.preparation_job_id,'$.request.consumer_ref',op.id) AS body
 FROM subtitle_translation_requests receipt JOIN subtitle_translation_operations op ON op.id=json_extract(receipt.outcome_json,'$.operation_id')
 WHERE receipt.user_id=json_extract($1,'$.translation_user_id') AND receipt.request_id=json_extract($1,'$.translation_request_id')
 AND json_extract(receipt.outcome_json,'$.outcome')='accepted' AND op.state='checking_source' AND op.stopped=0), input AS MATERIALIZED ("#;
        let sql = ENQUEUE_SQL.replacen(
            "WITH provided AS (SELECT json($1) AS body), input AS MATERIALIZED (",
            provided,
            1,
        );
        statements.push((sql, encode(&enqueue)?));
        statements.push((r#"UPDATE subtitle_translation_schema SET version=0 WHERE EXISTS(SELECT 1 FROM subtitle_translation_requests r JOIN subtitle_translation_operations op ON op.id=json_extract(r.outcome_json,'$.operation_id') WHERE r.user_id=json_extract($1,'$.user_id') AND r.request_id=json_extract($1,'$.request_id') AND json_extract(r.outcome_json,'$.outcome')='accepted' AND op.state='checking_source' AND op.stopped=0 AND op.operation_epoch=CAST(json_extract(r.outcome_json,'$.operation_epoch') AS INTEGER) AND NOT EXISTS(SELECT 1 FROM background_jobs j WHERE j.id=op.preparation_job_id AND j.state IN ('queued','running')))"#.into(),encode(&body)?));
    }
    if let TranslationAction::RetryRange {
        operation_id,
        expected_operation_epoch,
        core_index,
        ..
    } = &input.action
    {
        let rows=store.queue_sql("SELECT json_object('producer_preparation_job_id',op.preparation_job_id,'asset_id',a.id,'operation_id',op.id,'operation_epoch',op.operation_epoch,'source',json(op.source_json),'source_sha256',a.source_sha256,'profile',json(op.profile_json),'audio_index',op.audio_index,'spoken_language',op.spoken_language,'duration_ms',op.duration_ms,'core_index',json_extract($1,'$.core_index')) AS result_json FROM subtitle_translation_operations op JOIN subtitle_translation_assets a ON a.id=op.asset_id WHERE op.id=json_extract($1,'$.operation_id') AND op.operation_epoch=json_extract($1,'$.epoch')".into(),encode(&json!({"operation_id":operation_id,"epoch":expected_operation_epoch,"core_index":core_index}))?,false,true).await?;
        if let Some(row) = rows.first() {
            let mut recipe_row: Value = decode(row)?;
            let producer_preparation_job_id = recipe_row
                .as_object_mut()
                .and_then(|row| row.remove("producer_preparation_job_id"))
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| StoreError::Task("translation producer missing".into()))?;
            let recipe: super::subtitle_translation::RangeRecipe =
                serde_json::from_value(recipe_row)
                    .map_err(|error| StoreError::Task(error.to_string()))?;
            recipe.validate()?;
            let target_rows=store.queue_sql("SELECT json_quote(target_node_id) AS result_json FROM subtitle_translation_operations WHERE id=json_extract($1,'$.operation_id')".into(),encode(&json!({"operation_id":operation_id}))?,false,true).await?;
            let node: String = decode(
                target_rows
                    .first()
                    .ok_or_else(|| StoreError::Task("translation root disappeared".into()))?,
            )?;
            let request = EnqueueJob {
                id: body["allocated_job_id"]
                    .as_str()
                    .expect("owned job UUID")
                    .into(),
                payload: JobPayload::SubtitleTranslationRange {
                    file_id: recipe.source.file_id,
                    recipe,
                    producer_preparation_job_id,
                    target_node_id: node.clone(),
                },
                dedupe_key: format!("translation-range-{}", input.request_id),
                priority: 1,
                not_before_ms: input.now_ms,
                now_ms: input.now_ms,
                request: JobRequest {
                    scope: "subtitle-translation".into(),
                    request_id: input.request_id.clone(),
                    request_digest: body["body_digest"].as_str().expect("owned digest").into(),
                    consumer_kind: "subtitle_translation".into(),
                    consumer_ref: operation_id.clone(),
                    target_node_id: Some(node),
                    deadline_ms: None,
                    retain_identity: false,
                },
            };
            request.validate()?;
            let mut enqueue = enqueue_body(store, &request).await?;
            enqueue["attempt_limit"] = 3.into();
            enqueue["translation_user_id"] = input.user_id.into();
            enqueue["translation_request_id"] = input.request_id.clone().into();
            let provided = r#"WITH provided AS(SELECT json_set($1,'$.id',json_extract(receipt.outcome_json,'$.job_id')) AS body FROM subtitle_translation_requests receipt WHERE receipt.user_id=json_extract($1,'$.translation_user_id') AND receipt.request_id=json_extract($1,'$.translation_request_id') AND json_extract(receipt.outcome_json,'$.outcome')='accepted' AND json_extract(receipt.outcome_json,'$.operation_epoch')=CAST(json_extract($1,'$.payload.recipe.operation_epoch') AS TEXT)), input AS MATERIALIZED ("#;
            statements.push((
                ENQUEUE_SQL.replacen(
                    "WITH provided AS (SELECT json($1) AS body), input AS MATERIALIZED (",
                    provided,
                    1,
                ),
                encode(&enqueue)?,
            ));
            statements.push(("UPDATE subtitle_translation_schema SET version=0 WHERE EXISTS(SELECT 1 FROM subtitle_translation_requests r WHERE r.user_id=json_extract($1,'$.user_id') AND r.request_id=json_extract($1,'$.request_id') AND json_extract(r.outcome_json,'$.outcome')='accepted' AND NOT EXISTS(SELECT 1 FROM background_jobs j WHERE j.id=json_extract(r.outcome_json,'$.job_id')))".into(),encode(&body)?));
        }
    }
    store.queue_transaction(statements).await?;
    let rows=store.queue_sql("DELETE FROM subtitle_translation_commands WHERE id=json_extract($1,'$.command_id') RETURNING result_json".into(),encode(&body)?,true,true).await?;
    rows.first()
        .map(|r| decode(r))
        .transpose()?
        .ok_or_else(|| StoreError::Task("translation request not retained".into()))
}
