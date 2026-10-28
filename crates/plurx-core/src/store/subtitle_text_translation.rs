//! Centrally persisted, immutable text-translation slices. Partial output is
//! recomputable and cannot become a caption without the final typed publisher.
use super::background_jobs::{decode, encode, JobPayload, JobToken, QueueSql};
use super::background_jobs_transcription::{artifact_key, SUBTITLE_OWNS};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Shared parser/publication byte limit; identifiers are preserved without truncation.
pub const MAX_TEXT_CUE_ID_BYTES: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TextTranslationCue {
    pub cue_index: u32,
    pub input_sha256: String,
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishTextSlice {
    pub token: JobToken,
    pub source: JobPayload,
    pub cues: Vec<TextTranslationCue>,
    pub now_ms: i64,
}
fn invalid() -> StoreError {
    StoreError::Task("invalid text translation slice".into())
}
fn valid_source(source: &JobPayload) -> Result<String, StoreError> {
    if !matches!(source, JobPayload::SubtitleTextTranslate { .. }) {
        return Err(invalid());
    }
    artifact_key(source)
}
fn cue_body(cue: &TextTranslationCue) -> Result<String, StoreError> {
    if cue.cue_index >= 20000
        || !super::background_jobs::digest(&cue.input_sha256)
        || cue.id.len() > MAX_TEXT_CUE_ID_BYTES
        || cue.start_ms < 0
        || cue.end_ms <= cue.start_ms
        || cue.end_ms > 86_400_000
        || cue.text.is_empty()
        || cue.text.len() > 16 * 1024
        || cue.text.contains('\0')
    {
        return Err(invalid());
    }
    let body = encode(cue)?;
    if body.len() > 32768 {
        return Err(invalid());
    }
    Ok(body)
}
const CURRENT:&str="EXISTS(SELECT 1 FROM files WHERE id=json_extract($1,'$.source.file_id') AND size=json_extract($1,'$.source.source_size') AND mtime=json_extract($1,'$.source.source_mtime') AND probe_json IS NOT NULL) AND EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.source.admin_user_id') AND is_admin=1) AND EXISTS(SELECT 1 FROM settings WHERE key='subtitles.text_translation.config.v1' AND CASE WHEN json_valid(value) THEN json_extract(value,'$.enabled')=1 ELSE 0 END)";
pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishTextSlice,
) -> Result<bool, StoreError> {
    input.token.validate()?;
    let recipe = valid_source(&input.source)?;
    if input.now_ms < 0 || input.cues.is_empty() || input.cues.len() > 8 {
        return Err(invalid());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut cues = Vec::new();
    for cue in &input.cues {
        if !seen.insert(cue.cue_index) {
            return Err(invalid());
        }
        cues.push(json!({"cue_index":cue.cue_index,"input_sha256":cue.input_sha256,"start_ms":cue.start_ms,"end_ms":cue.end_ms,"body":cue_body(cue)?}));
    }
    let body = encode(
        &json!({"token":input.token,"source":input.source,"recipe":recipe,"cues":cues,"now_ms":input.now_ms}),
    )?;
    let owns=format!("{SUBTITLE_OWNS} AND job.kind='subtitle_text_translate' AND job.payload_json=json(json_extract($1,'$.source'))");
    let guard=format!("{CURRENT} AND EXISTS(SELECT 1 FROM background_jobs job WHERE {owns}) AND NOT EXISTS(SELECT 1 FROM json_each($1,'$.cues') incoming JOIN subtitle_text_translation_cues old ON old.job_id=json_extract($1,'$.token.job_id') AND old.recipe_digest=json_extract($1,'$.recipe') AND old.cue_index=json_extract(incoming.value,'$.cue_index') WHERE old.body!=json_extract(incoming.value,'$.body'))");
    store.queue_sql(format!("INSERT INTO subtitle_text_translation_cues(job_id,recipe_digest,cue_index,input_sha256,start_ms,end_ms,body,created_at_ms) SELECT json_extract($1,'$.token.job_id'),json_extract($1,'$.recipe'),json_extract(cue.value,'$.cue_index'),json_extract(cue.value,'$.input_sha256'),json_extract(cue.value,'$.start_ms'),json_extract(cue.value,'$.end_ms'),json_extract(cue.value,'$.body'),json_extract($1,'$.now_ms') FROM json_each($1,'$.cues') cue WHERE {guard} AND (SELECT COALESCE(sum(length(CAST(body AS BLOB))),0) FROM subtitle_text_translation_cues)+(SELECT sum(length(CAST(json_extract(value,'$.body') AS BLOB))) FROM json_each($1,'$.cues'))<=67108864 AND (SELECT COALESCE(sum(length(CAST(body AS BLOB))),0) FROM subtitle_text_translation_cues WHERE job_id=json_extract($1,'$.token.job_id'))+(SELECT sum(length(CAST(json_extract(value,'$.body') AS BLOB))) FROM json_each($1,'$.cues'))<=1048576 ON CONFLICT(job_id,recipe_digest,cue_index) DO NOTHING"),body.clone(),true,false).await?;
    let rows=store.queue_sql(format!("SELECT 'true' AS result_json WHERE {guard} AND NOT EXISTS(SELECT 1 FROM json_each($1,'$.cues') incoming WHERE NOT EXISTS(SELECT 1 FROM subtitle_text_translation_cues saved WHERE saved.job_id=json_extract($1,'$.token.job_id') AND saved.recipe_digest=json_extract($1,'$.recipe') AND saved.cue_index=json_extract(incoming.value,'$.cue_index') AND saved.body=json_extract(incoming.value,'$.body')))"),body,false,true).await?;
    Ok(!rows.is_empty())
}
pub(super) async fn read<T: QueueSql>(
    store: &T,
    source: &JobPayload,
) -> Result<Vec<TextTranslationCue>, StoreError> {
    let recipe = valid_source(source)?;
    let rows=store.queue_sql("SELECT body AS result_json FROM (SELECT body,cue_index,row_number() OVER(PARTITION BY cue_index ORDER BY created_at_ms,job_id) AS winner FROM subtitle_text_translation_cues WHERE recipe_digest=json_extract($1,'$.recipe')) WHERE winner=1 ORDER BY cue_index LIMIT 20000".into(),encode(&json!({"recipe":recipe}))?,false,true).await?;
    rows.iter()
        .map(|row| {
            let cue: TextTranslationCue = decode(row)?;
            cue_body(&cue)?;
            Ok(cue)
        })
        .collect()
}
pub(super) async fn maintain<T: QueueSql>(store: &T, now_ms: i64) -> Result<(), StoreError> {
    if now_ms < 0 {
        return Err(invalid());
    }
    store.queue_sql("DELETE FROM subtitle_text_translation_cues WHERE (job_id,recipe_digest,cue_index) IN(SELECT cue.job_id,cue.recipe_digest,cue.cue_index FROM subtitle_text_translation_cues cue LEFT JOIN background_jobs job ON job.id=cue.job_id WHERE job.id IS NULL OR (job.state IN ('succeeded','failed','cancelled') AND cue.created_at_ms<json_extract($1,'$.now_ms')-86400000) ORDER BY cue.created_at_ms LIMIT 128)".into(),encode(&json!({"now_ms":now_ms}))?,true,false).await?;
    Ok(())
}

pub(super) async fn receipt<T: QueueSql>(
    store: &T,
    user_id: i64,
    request_id: &str,
    digest: &str,
) -> Result<Option<serde_json::Value>, StoreError> {
    if user_id <= 0
        || uuid::Uuid::parse_str(request_id).is_err()
        || !super::background_jobs::digest(digest)
    {
        return Err(invalid());
    }
    let body = encode(
        &json!({"user_id":user_id,"scope":format!("subtitle-text-user:{user_id}"),"request_id":request_id,"digest":digest}),
    )?;
    let rows=store.queue_sql("SELECT CASE WHEN request_digest=json_extract($1,'$.digest') THEN json_object('outcome','existing','job_id',job_id) ELSE json_object('outcome','conflict') END AS result_json FROM background_job_waiters WHERE request_scope=json_extract($1,'$.scope') AND request_id=json_extract($1,'$.request_id') AND EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.user_id') AND is_admin=1)".into(),body,false,true).await?;
    rows.first().map(|row| decode(row)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        background_jobs::{
            BackgroundJobStore, ClaimJob, ClaimOutcome, EnqueueJob, JobKind, JobRequest,
            JobSettlement, SettleJob,
        },
        SqliteStore,
    };
    fn source() -> JobPayload {
        JobPayload::SubtitleTextTranslate {
            file_id: 1,
            source_size: 100,
            source_mtime: 20,
            subtitle_index: 0,
            input_sha256: "a".repeat(64),
            source_language: "de".into(),
            target_language: "en".into(),
            engine_sha256: "e".repeat(64),
            model_id: "opus-mt-mul-en".into(),
            model_sha256: "b".repeat(64),
            adapter_contract: "candle-marian-0.11-v1".into(),
            target_node_id: "worker".into(),
            admin_user_id: 1,
        }
    }
    fn cue(index: u32) -> TextTranslationCue {
        TextTranslationCue {
            cue_index: index,
            input_sha256: "c".repeat(64),
            id: format!("cue-{index}"),
            start_ms: i64::from(index) * 1000,
            end_ms: i64::from(index) * 1000 + 1000,
            text: "Translated caption".into(),
        }
    }
    async fn fixture() -> (
        tempfile::TempDir,
        SqliteStore,
        rusqlite::Connection,
        PublishTextSlice,
    ) {
        let dir = tempfile::tempdir().expect("fixture directory");
        let path = dir.path().join("store.db");
        let store = SqliteStore::open(&path).expect("production migrations");
        let conn = rusqlite::Connection::open(path).expect("fixture connection");
        conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','hash',1);INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/media\"]');INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie');INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,1,'/media/movie',100,20,'{}');INSERT INTO settings(key,value) VALUES('subtitles.text_translation.config.v1','{\"version\":1,\"enabled\":true}');").expect("catalog fixture");
        let id = uuid::Uuid::new_v4().to_string();
        store
            .enqueue_job(EnqueueJob {
                id: id.clone(),
                payload: source(),
                dedupe_key: "text-slice-fixture".into(),
                priority: 1,
                not_before_ms: 1000,
                now_ms: 1000,
                request: JobRequest {
                    scope: "subtitle-text-user:1".into(),
                    request_id: uuid::Uuid::new_v4().to_string(),
                    request_digest: "d".repeat(64),
                    consumer_kind: "subtitle_text_translation".into(),
                    consumer_ref: "fixture".into(),
                    target_node_id: Some("worker".into()),
                    deadline_ms: Some(86400000),
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue");
        let queued = store
            .background_job(&id)
            .await
            .expect("queued lookup")
            .expect("queued job");
        let claimed = store
            .claim_artifact_job(ClaimJob {
                job_id: id,
                expected_revision: queued.revision,
                node_id: "worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleTextTranslate,
                payload_version: 1,
                now_ms: 1100,
                dispatched_at_ms: 1100,
            })
            .await
            .expect("claim");
        let ClaimOutcome::Claimed { job } = claimed else {
            panic!("typed text job not claimed: {claimed:?}")
        };
        let input = PublishTextSlice {
            token: job.token.expect("claim token"),
            source: source(),
            cues: vec![cue(0)],
            now_ms: 1200,
        };
        (dir, store, conn, input)
    }
    #[tokio::test]
    async fn text_slices_preserve_full_caption_identifier_byte_limit() {
        let (_dir, store, _conn, mut input) = fixture().await;
        input.cues[0].id = "é".repeat(MAX_TEXT_CUE_ID_BYTES / 2);
        assert_eq!(input.cues[0].id.len(), MAX_TEXT_CUE_ID_BYTES);
        assert!(store
            .publish_text_translation_slice(input.clone())
            .await
            .expect("publish full identifier"));
        let saved = store
            .read_text_translation_cues(input.source.clone())
            .await
            .expect("read durable slice");
        assert_eq!(saved, input.cues);
        let mut too_long = input.clone();
        too_long.cues[0].cue_index = 1;
        too_long.cues[0].id.push('x');
        assert!(store
            .publish_text_translation_slice(too_long)
            .await
            .is_err());
        assert_eq!(
            store
                .read_text_translation_cues(input.source.clone())
                .await
                .expect("inventory after refusal"),
            saved
        );
    }
    #[tokio::test]
    async fn text_slices_are_immutable_and_old_claims_cannot_publish_after_replacement() {
        let (_dir, store, _conn, first) = fixture().await;
        assert!(store
            .publish_text_translation_slice(first.clone())
            .await
            .expect("first publication"));
        assert!(store
            .publish_text_translation_slice(first.clone())
            .await
            .expect("exact replay"));
        let mut changed = first.clone();
        changed.cues[0].text = "Different output".into();
        assert!(!store
            .publish_text_translation_slice(changed)
            .await
            .expect("immutable conflict"));
        store
            .settle_job(SettleJob {
                token: first.token.clone(),
                settlement: JobSettlement::Yield {
                    error_code: None,
                    checkpoint: None,
                    not_before_ms: 1300,
                },
                now_ms: 1250,
            })
            .await
            .expect("yield");
        let queued = store
            .background_job(&first.token.job_id)
            .await
            .expect("lookup")
            .expect("job");
        let ClaimOutcome::Claimed { job } = store
            .claim_artifact_job(ClaimJob {
                job_id: queued.id,
                expected_revision: queued.revision,
                node_id: "worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleTextTranslate,
                payload_version: 1,
                now_ms: 1400,
                dispatched_at_ms: 1400,
            })
            .await
            .expect("replacement claim")
        else {
            panic!("replacement not claimed")
        };
        let mut stale = first.clone();
        stale.cues = vec![cue(1)];
        stale.now_ms = 1450;
        assert!(!store
            .publish_text_translation_slice(stale.clone())
            .await
            .expect("stale refusal"));
        stale.token = job.token.expect("replacement token");
        assert!(store
            .publish_text_translation_slice(stale)
            .await
            .expect("current claim publication"));
        assert_eq!(
            store
                .read_text_translation_cues(source())
                .await
                .expect("durable partials")
                .len(),
            2
        );
    }
    #[tokio::test]
    async fn text_slice_input_profile_source_and_resource_fences_are_authoritative() {
        let (_dir, store, conn, input) = fixture().await;
        for model in [false, true] {
            let mut changed = input.clone();
            if let JobPayload::SubtitleTextTranslate {
                input_sha256,
                model_sha256,
                ..
            } = &mut changed.source
            {
                if model {
                    *model_sha256 = "f".repeat(64);
                } else {
                    *input_sha256 = "f".repeat(64);
                }
            }
            assert!(!store
                .publish_text_translation_slice(changed)
                .await
                .expect("recipe mismatch refusal"));
        }
        conn.execute(
            "DELETE FROM background_job_reservations WHERE job_id=?1",
            [&input.token.job_id],
        )
        .expect("simulate lost reservation");
        assert!(!store
            .publish_text_translation_slice(input.clone())
            .await
            .expect("resource refusal"));
        assert!(store
            .read_text_translation_cues(source())
            .await
            .expect("nothing published")
            .is_empty());
    }
    #[tokio::test]
    async fn text_slice_budgets_bound_central_bytes_and_terminal_cleanup_preserves_live_work() {
        let (_dir, store, conn, input) = fixture().await;
        let filler = " ".repeat(32766) + "{}";
        // Populate central byte inventory at the per-job ceiling. The
        // production slice must refuse an additional byte rather than truncate.
        for index in 100..132 {
            conn.execute("INSERT INTO subtitle_text_translation_cues(job_id,recipe_digest,cue_index,input_sha256,start_ms,end_ms,body,created_at_ms) VALUES(?1,?2,?3,?4,0,1,?5,0)",rusqlite::params![input.token.job_id,"f".repeat(64),index,"c".repeat(64),filler]).expect("byte inventory");
        }
        assert!(!store
            .publish_text_translation_slice(input.clone())
            .await
            .expect("per job byte refusal"));
        maintain(&store, 100_000_000).await.expect("live retention");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM subtitle_text_translation_cues",
                [],
                |row| row.get(0),
            )
            .expect("live count");
        assert_eq!(count, 32);
        conn.execute(
            "UPDATE background_jobs SET state='cancelled',owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL WHERE id=?1",
            [&input.token.job_id],
        )
        .expect("terminal fixture");
        maintain(&store, 100_000_000)
            .await
            .expect("terminal retirement");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM subtitle_text_translation_cues",
                [],
                |row| row.get(0),
            )
            .expect("retired count");
        assert_eq!(count, 0);
    }
    #[tokio::test]
    async fn global_text_slice_budget_refuses_new_job_without_touching_immutable_inventory() {
        let (_dir, store, mut conn, input) = fixture().await;
        let mut inventory = Vec::new();
        for index in 0..32 {
            let mut draft = cue(index);
            draft.text = "\u{1}".repeat(5400);
            let remaining = 32768 - cue_body(&draft).expect("bounded draft").len();
            draft.text.push_str(&"x".repeat(remaining));
            let body = cue_body(&draft).expect("exact byte inventory");
            assert_eq!(body.len(), 32768);
            inventory.push((draft, body));
        }
        // Sixty-four retained terminal jobs each own exactly one MiB. All
        // bytes count, even while a different empty job owns the current claim.
        for number in 0..64 {
            let id = uuid::Uuid::new_v4().to_string();
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: source(),
                    dedupe_key: format!("text-global-budget-{number}"),
                    priority: 1,
                    not_before_ms: 1000,
                    now_ms: 1000,
                    request: JobRequest {
                        scope: "subtitle-text-user:1".into(),
                        request_id: uuid::Uuid::new_v4().to_string(),
                        request_digest: "d".repeat(64),
                        consumer_kind: "subtitle_text_translation".into(),
                        consumer_ref: "inventory".into(),
                        target_node_id: Some("worker".into()),
                        deadline_ms: Some(86400000),
                        retain_identity: false,
                    },
                })
                .await
                .expect("inventory job");
            conn.execute(
                "UPDATE background_jobs SET state='cancelled',owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL WHERE id=?1",
                [&id],
            )
            .expect("terminal inventory");
            let transaction = conn.transaction().expect("inventory transaction");
            for (draft, body) in &inventory {
                transaction.execute("INSERT INTO subtitle_text_translation_cues(job_id,recipe_digest,cue_index,input_sha256,start_ms,end_ms,body,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,1000)",rusqlite::params![id,artifact_key(&source()).expect("recipe"),draft.cue_index,draft.input_sha256,draft.start_ms,draft.end_ms,body]).expect("retained cue");
            }
            transaction.commit().expect("central inventory commit");
        }
        assert!(!store
            .publish_text_translation_slice(input.clone())
            .await
            .expect("global byte refusal"));
        let current: i64 = conn
            .query_row(
                "SELECT count(*) FROM subtitle_text_translation_cues WHERE job_id=?1",
                [&input.token.job_id],
                |row| row.get(0),
            )
            .expect("new job remains empty");
        assert_eq!(current, 0);
        let bytes: i64 = conn
            .query_row(
                "SELECT sum(length(CAST(body AS BLOB))) FROM subtitle_text_translation_cues",
                [],
                |row| row.get(0),
            )
            .expect("retained inventory");
        assert_eq!(bytes, 64 * 1024 * 1024);
    }
}
