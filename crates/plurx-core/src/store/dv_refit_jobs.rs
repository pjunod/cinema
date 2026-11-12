//! F+ curve fits for Profile 7 FEL titles (`docs/streaming/FEL-RPU-REFIT-BUILD.md`
//! §3.3–3.4): the `dv_refit` job's demand, its fenced publication, and the
//! per-file curve table copy-path serving reads.
//!
//! A title's fit is published in two steps through the queue's command
//! table, both fenced by the running attempt's token. The attempt first
//! stages its scenes in bounded pages, because a feature film's scenes and
//! their statistics are far larger than one replicated write may carry. Its
//! publication then adopts exactly those scenes, replaces the file's older
//! fit and settles the job in one transaction. Staged scenes of an attempt
//! that ends any other way (cancelled, yielded, lost, failed) are deleted by
//! the trigger that ends it, so nothing partial survives.
//!
//! One implementation serves both catalogues through the queue's SQL bridge,
//! and the schema is installed the same way on each: one SQLite migration
//! step, and an idempotent, shape-verified install on hiqlite.
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::background_jobs::{
    decode, digest, encode, identifier, JobPublishOutcome, JobToken, QueueSql,
};
use crate::error::StoreError;
use crate::transcode::dv_refit::RefitCurves;

pub(crate) const SCHEMA: &str = include_str!("dv_refit_jobs.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 130;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'background_dv_refit_*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM background_dv_refit_schema WHERE singleton=1 AND version=1";
/// The objects the extension's triggers reference.
const DEPENDENCIES: &str = "CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT,size INTEGER,mtime INTEGER); \
     CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,claim_id TEXT,result_ref TEXT,owner_node_id TEXT,owner_boot_id TEXT,lease_expires_ms INTEGER,revision INTEGER,updated_at_ms INTEGER); \
     CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); \
     CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,result_ref TEXT,updated_at_ms INTEGER);";

/// Bound on one staging page's request body. A hiqlite write is one Raft
/// entry, and the WAL refuses an entry above 16 MiB by restarting the node.
pub const MAX_STAGE_BYTES: usize = 4 * 1024 * 1024;
/// Bound on one scene's stored row (its two statistics sets dominate it).
pub const MAX_SCENE_BYTES: usize = 512 * 1024;
/// Bound on a title's scenes. A three-hour title at 24 fps whose scenes are
/// pooled to at least 24 frames has under 11,000.
pub const MAX_SCENES: usize = 16_384;
pub const MAX_REASON_BYTES: usize = 1024;

/// A Profile 7 FEL title whose current fit is missing or stale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefitDemand {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    /// The title scan's demand generation whose complete aggregate is
    /// published: it moves with every fresh probe, source change and scanner
    /// rebind, and a fit is current only for the generation it read.
    pub probe_revision: i64,
    pub probe_json: String,
    /// A queued job for exactly this identity (an administrator's retry, or
    /// an attempt that yielded): resume it rather than enqueue another.
    pub queued_job_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefitVerdict {
    /// At least one scene serves fitted curves.
    Fitted,
    /// Every scene serves identity curves, for the recorded reason.
    Identity,
    /// The helper refused the title (one stderr line), recorded so the title
    /// is not decoded again until its source, probe, helper or fit version
    /// changes.
    Refused,
}

/// A published title fit, without its scenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefitTitle {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub probe_revision: i64,
    pub fit_version: String,
    pub helper_digest: String,
    /// Names the served curve set (FEL-RPU-REFIT-BUILD.md §3.5).
    pub curve_digest: String,
    pub verdict: RefitVerdict,
    /// Why the title serves identity, or was refused. `None` when fitted.
    pub reason: Option<String>,
    /// 1 − served/identity pooled held-out squared error, when measurable.
    pub gain: Option<f64>,
    pub scene_count: i64,
    pub frames: i64,
}

impl RefitTitle {
    pub fn valid(&self) -> bool {
        self.file_id > 0
            && self.source_size >= 0
            && self.source_mtime >= 0
            && self.probe_revision > 0
            && valid_fit_version(&self.fit_version)
            && digest(&self.helper_digest)
            && digest(&self.curve_digest)
            && (self.verdict == RefitVerdict::Fitted) == self.reason.is_none()
            && self.reason.as_deref().is_none_or(valid_reason)
            && self.gain.is_none_or(f64::is_finite)
            && (0..=MAX_SCENES as i64).contains(&self.scene_count)
            && self.frames >= 0
            && (self.verdict != RefitVerdict::Refused || self.scene_count == 0)
    }
}

/// One scene as stored. The statistics and curves are carried as the JSON
/// text their serde layout writes, so the stored bytes are exactly what was
/// fitted and digested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedScene {
    pub first_frame: i64,
    pub frames: i64,
    pub first_pts: i64,
    /// The scene's last frame's presentation time plus its duration.
    pub end_pts: i64,
    /// `num/den` of the stream's time base, as the helper reported it.
    pub time_base: String,
    /// [`crate::transcode::dv_refit::RefitStats`] the fit saw.
    pub fit_json: String,
    /// [`crate::transcode::dv_refit::RefitStats`] held out from the fit.
    pub held_out_json: String,
    /// [`RefitCurves`] (fit domain) that passed every guard, or `None`.
    pub curves_json: Option<String>,
    /// Why the scene keeps identity curves; `None` exactly when it has curves.
    pub reason: Option<String>,
    /// [`crate::transcode::dv_refit::HeldOutVerdict`], when one was reached.
    pub verdict_json: Option<String>,
}

impl StagedScene {
    pub fn valid(&self) -> bool {
        let json = |text: &str| serde_json::from_str::<serde_json::Value>(text).is_ok();
        self.first_frame >= 0
            && self.frames > 0
            && self.end_pts > self.first_pts
            && valid_time_base(&self.time_base)
            && json(&self.fit_json)
            && json(&self.held_out_json)
            && self.curves_json.as_deref().is_none_or(json)
            && self.verdict_json.as_deref().is_none_or(json)
            && self.curves_json.is_none() == self.reason.is_some()
            && self.reason.as_deref().is_none_or(valid_reason)
            && self.bytes() <= MAX_SCENE_BYTES
    }

    /// The bytes this scene adds to a staging request, near enough to bound pages.
    pub fn bytes(&self) -> usize {
        self.fit_json.len()
            + self.held_out_json.len()
            + self.curves_json.as_ref().map_or(0, String::len)
            + self.verdict_json.as_ref().map_or(0, String::len)
            + self.reason.as_ref().map_or(0, String::len)
            + self.time_base.len()
            + 256
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRefitScenes {
    pub token: JobToken,
    pub file_id: i64,
    /// Index of `scenes[0]` in the title's presentation order.
    pub first_index: i64,
    pub scenes: Vec<StagedScene>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRefitJob {
    pub token: JobToken,
    pub title: RefitTitle,
    pub now_ms: i64,
}

/// One scene of a file's current curve table, as copy-path serving applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefitCurveScene {
    pub first_frame: i64,
    pub frames: i64,
    /// Scenes are keyed by their first presentation time (§3.5): a scene
    /// covers [first_ptsᵢ, first_ptsᵢ₊₁), the last one to its `end_pts`. A
    /// seek starts mid-title, so a running frame count cannot find a scene.
    pub first_pts: i64,
    pub end_pts: i64,
    pub time_base: String,
    /// The fitted curves, or `None` for identity. Always `None` unless the
    /// title's verdict is [`RefitVerdict::Fitted`].
    pub curves: Option<RefitCurves>,
}

/// A file's current fit: its title row and served scenes, for the source the
/// catalogue row still describes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefitCurveTable {
    pub title: RefitTitle,
    pub scenes: Vec<RefitCurveScene>,
}

fn valid_reason(reason: &str) -> bool {
    !reason.is_empty() && reason.len() <= MAX_REASON_BYTES
}

pub fn valid_fit_version(version: &str) -> bool {
    version.len() <= 64 && identifier(version)
}

/// `num/den`, both positive decimal integers that fit an `i32`, as libav
/// reports a stream time base.
pub fn valid_time_base(time_base: &str) -> bool {
    let positive = |part: &str| {
        !part.is_empty()
            && !part.starts_with('0')
            && part.bytes().all(|b| b.is_ascii_digit())
            && part.parse::<i32>().is_ok_and(|v| v > 0)
    };
    time_base
        .split_once('/')
        .is_some_and(|(num, den)| positive(num) && positive(den))
}

/// The attempt's ownership, its payload, and the file's current source, in
/// one snapshot: what both staging and publication require of the writer.
fn ownership_sql(body: &str, file: &str) -> String {
    format!(
        r#"job.state='running' AND job.kind='dv_refit' AND job.payload_version=1
 AND job.owner_node_id=json_extract({body},'$.token.node_id') AND job.owner_boot_id=json_extract({body},'$.token.boot_id')
 AND job.claim_id=json_extract({body},'$.token.claim_id') AND job.fence=json_extract({body},'$.token.fence')
 AND job.revision=json_extract({body},'$.token.revision') AND job.revision<9223372036854775807
 AND job.lease_expires_ms=json_extract({body},'$.token.lease_expires_ms') AND job.lease_expires_ms>json_extract({body},'$.now_ms')
 AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||job.owner_node_id)
 AND NOT EXISTS(SELECT 1 FROM background_job_required_resources r WHERE r.job_id=job.id
  AND NOT EXISTS(SELECT 1 FROM background_job_reservations h WHERE h.job_id=job.id AND h.fence=job.fence
   AND h.resource_key=r.resource_key AND h.expires_at_ms>=job.lease_expires_ms))
 AND EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=job.id AND w.state='pending'
  AND (w.deadline_ms IS NULL OR w.deadline_ms>json_extract({body},'$.now_ms')))
 AND json_extract(job.payload_json,'$.file_id')={file}
 AND EXISTS(SELECT 1 FROM files f WHERE f.id={file}
  AND f.size=json_extract(job.payload_json,'$.source_size') AND f.mtime=json_extract(job.payload_json,'$.source_mtime'))"#
    )
}

static STAGE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body,COALESCE(({owns}),0) AS owns
 FROM input LEFT JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.id'),'stage_dv_refit',body,
 json_object('outcome',CASE WHEN owns THEN 'staged' ELSE 'lost_ownership' END)
FROM observation RETURNING result_json
"#,
        owns = ownership_sql("body", "json_extract(body,'$.file_id')")
    )
});

static PUBLISH_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body,job.id,job.state,job.result_ref,
 'dv-refit:'||json_extract(body,'$.token.claim_id')||':'||json_extract(body,'$.title.curve_digest') AS reference,
 EXISTS(SELECT 1 FROM background_job_attempts a WHERE a.job_id=job.id
  AND a.fence=job.fence AND a.fence=json_extract(body,'$.token.fence') AND a.claim_id=json_extract(body,'$.token.claim_id')
  AND a.owner_node_id=json_extract(body,'$.token.node_id') AND a.owner_boot_id=json_extract(body,'$.token.boot_id')) AS same_attempt,
 COALESCE(({owns}
  AND json_extract(job.payload_json,'$.source_size')=json_extract(body,'$.title.source_size')
  AND json_extract(job.payload_json,'$.source_mtime')=json_extract(body,'$.title.source_mtime')
  AND json_extract(job.payload_json,'$.probe_revision')=json_extract(body,'$.title.probe_revision')
  AND json_extract(job.payload_json,'$.helper_digest')=json_extract(body,'$.title.helper_digest')
  AND json_extract(job.payload_json,'$.fit_version')=json_extract(body,'$.title.fit_version')),0) AS owns,
 (SELECT json_object('count',count(*),'last',COALESCE(max(s.scene_index),-1),'frames',COALESCE(sum(s.frames),0))
  FROM background_dv_refit_scenes s WHERE s.claim_id=json_extract(body,'$.token.claim_id')
  AND s.file_id=json_extract(body,'$.title.file_id')) AS staged
 FROM input LEFT JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
), verdict AS (
 SELECT *,CASE WHEN state='succeeded' AND same_attempt AND result_ref=reference THEN 'already_published'
 WHEN NOT owns THEN 'lost_ownership'
 WHEN json_extract(staged,'$.count')!=json_extract(body,'$.title.scene_count')
  OR json_extract(staged,'$.last')!=json_extract(body,'$.title.scene_count')-1
  OR json_extract(staged,'$.frames')!=json_extract(body,'$.title.frames') THEN 'artifact_conflict'
 ELSE 'published' END AS outcome FROM observation
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.token.claim_id'),'publish_dv_refit',body,
 CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,
 'result_ref',reference) ELSE json_object('outcome',outcome) END
FROM verdict RETURNING result_json
"#,
        owns = ownership_sql("body", "json_extract(body,'$.title.file_id')")
    )
});

/// Profile 7 FEL titles whose complete title facts are published and whose
/// fit is missing or stale, after `after_file_id`, in file order.
///
/// FEL is told from MEL by the title's FEL facts for its current source: a
/// pending FEL recipe request, a stored recipe verdict, or a legacy verdict.
/// Only the FEL route writes those, and it plans only for a source whose RPUs
/// parse as FEL (`DvElKind::Fel`); no other durable fact separates FEL from
/// MEL. A failed or cancelled attempt for the same identity stops automatic
/// demand until its identity changes or an administrator retries it.
pub(super) async fn demands<T: QueueSql>(
    store: &T,
    helper: &str,
    fit_version: &str,
    after_file_id: i64,
    limit: u32,
) -> Result<Vec<RefitDemand>, StoreError> {
    if !digest(helper)
        || !valid_fit_version(fit_version)
        || after_file_id < 0
        || limit == 0
        || limit > 128
    {
        return Err(StoreError::Task("invalid F+ demand query".into()));
    }
    let same_identity = "j.kind='dv_refit' AND json_extract(j.payload_json,'$.file_id')=f.id
  AND json_extract(j.payload_json,'$.source_size')=f.size AND json_extract(j.payload_json,'$.source_mtime')=f.mtime
  AND json_extract(j.payload_json,'$.probe_revision')=d.revision
  AND json_extract(j.payload_json,'$.helper_digest')=json_extract($1,'$.helper')
  AND json_extract(j.payload_json,'$.fit_version')=json_extract($1,'$.fit')";
    let fel = |key: &str, size: &str| {
        format!(
            "EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(f.probe_json,'$.{key}')='object'
   THEN json_extract(f.probe_json,'$.{key}') ELSE '{{}}' END) fact
   WHERE json_extract(fact.value,'$.{size}source_size')=f.size AND json_extract(fact.value,'$.{size}source_mtime')=f.mtime)"
        )
    };
    let legacy = crate::transcode::dv_fel_qualification::PROBE_KEY;
    let sql = format!(
        r#"
SELECT json_object('file_id',f.id,'source_size',f.size,'source_mtime',f.mtime,'probe_revision',d.revision,
 'probe_json',f.probe_json,
 'queued_job_id',(SELECT j.id FROM background_jobs j WHERE j.state='queued' AND {same_identity}
  ORDER BY j.created_at_ms,j.id LIMIT 1)) AS result_json
FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
WHERE f.id>json_extract($1,'$.after') AND f.video_codec='hevc' AND f.dv_profile=7
 AND f.dv_el_present=1 AND f.dv_rpu_present=1 AND json_valid(f.probe_json)
 AND json_type(f.probe_json,'$.{title}')='object' AND json_extract(f.probe_json,'$.{title}.complete')=1
 AND ({requests} OR {variants}
  OR (json_type(f.probe_json,'$.{legacy}')='object' AND json_extract(f.probe_json,'$.{legacy}.source_size')=f.size
   AND json_extract(f.probe_json,'$.{legacy}.source_mtime')=f.mtime))
 AND NOT EXISTS(SELECT 1 FROM background_dv_refit_titles t WHERE t.file_id=f.id AND t.source_size=f.size
  AND t.source_mtime=f.mtime AND t.probe_revision=d.revision AND t.fit_version=json_extract($1,'$.fit')
  AND t.helper_digest=json_extract($1,'$.helper'))
 AND NOT EXISTS(SELECT 1 FROM background_jobs j WHERE j.kind='dv_refit' AND j.state IN ('running','cancelling')
  AND json_extract(j.payload_json,'$.file_id')=f.id)
 AND (EXISTS(SELECT 1 FROM background_jobs j WHERE j.state='queued' AND {same_identity})
  OR NOT EXISTS(SELECT 1 FROM background_jobs j WHERE {same_identity}))
ORDER BY f.id LIMIT json_extract($1,'$.limit')
"#,
        title = crate::scan::dv_title::PROBE_FIELD,
        requests = fel(crate::transcode::dv_fel_qualification::REQUESTS_KEY, ""),
        variants = fel(
            crate::transcode::dv_fel_qualification::VARIANTS_KEY,
            "verdict."
        ),
    );
    let rows = store
        .queue_sql(
            sql,
            encode(&serde_json::json!({"helper":helper,"fit":fit_version,"after":after_file_id,"limit":limit}))?,
            false,
            true,
        )
        .await?;
    rows.iter().map(|row| decode(row)).collect()
}

/// Stage one page of the running attempt's scenes. `false` when the attempt
/// no longer owns the title (cancelled, lost, or its source changed).
pub(super) async fn stage<T: QueueSql>(
    store: &T,
    input: StageRefitScenes,
) -> Result<bool, StoreError> {
    input.token.validate()?;
    if input.now_ms < 0
        || input.file_id <= 0
        || input.first_index < 0
        || input.scenes.is_empty()
        || input.first_index as usize + input.scenes.len() > MAX_SCENES
        || !input.scenes.iter().all(StagedScene::valid)
        || input.scenes.iter().map(StagedScene::bytes).sum::<usize>() > MAX_STAGE_BYTES
    {
        return Err(StoreError::Task("invalid F+ staging page".into()));
    }
    let mut body = serde_json::to_value(input).map_err(|e| StoreError::Task(e.to_string()))?;
    body["id"] = uuid::Uuid::new_v4().to_string().into();
    let rows = store
        .queue_sql(STAGE_SQL.clone(), encode(&body)?, true, true)
        .await?;
    #[derive(Deserialize)]
    struct Outcome {
        outcome: String,
    }
    let outcome: Outcome = decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("no F+ staging verdict".into()))?,
    )?;
    Ok(outcome.outcome == "staged")
}

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishRefitJob,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    if input.now_ms < 0 || !input.title.valid() {
        return Err(StoreError::Task("invalid F+ publication".into()));
    }
    let body = serde_json::to_value(input).map_err(|e| StoreError::Task(e.to_string()))?;
    let rows = store
        .queue_sql(PUBLISH_SQL.clone(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("no F+ publication verdict".into()))?,
    )
}

/// The file's current curve table: its published fit, if that fit describes
/// the source the catalogue row still names. Scenes are in presentation
/// order and carry curves only when the title serves them.
pub(super) async fn curves<T: QueueSql>(
    store: &T,
    file_id: i64,
) -> Result<Option<RefitCurveTable>, StoreError> {
    if file_id <= 0 {
        return Err(StoreError::Task("invalid F+ curve lookup".into()));
    }
    let rows = store
        .queue_sql(
            r#"
SELECT json_object('title',json_object('file_id',t.file_id,'source_size',t.source_size,
  'source_mtime',t.source_mtime,'probe_revision',t.probe_revision,'fit_version',t.fit_version,
  'helper_digest',t.helper_digest,'curve_digest',t.curve_digest,'verdict',t.verdict,'reason',t.reason,
  'gain',t.gain,'scene_count',t.scene_count,'frames',t.frames),
 'scene',CASE WHEN s.claim_id IS NULL THEN NULL ELSE json_object('index',s.scene_index,
  'first_frame',s.first_frame,'frames',s.frames,'first_pts',s.first_pts,'end_pts',s.end_pts,'time_base',s.time_base,
  'curves_json',CASE WHEN t.verdict='fitted' THEN s.curves_json END) END) AS result_json
FROM background_dv_refit_titles t JOIN files f ON f.id=t.file_id AND f.size=t.source_size AND f.mtime=t.source_mtime
LEFT JOIN background_dv_refit_scenes s ON s.claim_id=t.claim_id AND s.file_id=t.file_id
WHERE t.file_id=json_extract($1,'$.file_id')
ORDER BY s.scene_index
"#
            .into(),
            encode(&serde_json::json!({"file_id": file_id}))?,
            false,
            false,
        )
        .await?;
    #[derive(Deserialize)]
    struct Scene {
        index: i64,
        first_frame: i64,
        frames: i64,
        first_pts: i64,
        end_pts: i64,
        time_base: String,
        curves_json: Option<String>,
    }
    #[derive(Deserialize)]
    struct Row {
        title: RefitTitle,
        scene: Option<Scene>,
    }
    let mut table: Option<RefitCurveTable> = None;
    for row in rows {
        let row: Row = decode(&row)?;
        let table = table.get_or_insert_with(|| RefitCurveTable {
            title: row.title.clone(),
            scenes: Vec::new(),
        });
        if let Some(scene) = row.scene {
            if scene.index != table.scenes.len() as i64 {
                return Err(StoreError::Task("F+ scene table is not contiguous".into()));
            }
            let curves = scene
                .curves_json
                .map(|text| serde_json::from_str::<RefitCurves>(&text))
                .transpose()
                .map_err(|e| StoreError::Task(format!("stored F+ curves: {e}")))?;
            table.scenes.push(RefitCurveScene {
                first_frame: scene.first_frame,
                frames: scene.frames,
                first_pts: scene.first_pts,
                end_pts: scene.end_pts,
                time_base: scene.time_base,
                curves,
            });
        }
    }
    if let Some(table) = &table {
        if table.scenes.len() as i64 != table.title.scene_count {
            return Err(StoreError::Task("F+ scene table is incomplete".into()));
        }
    }
    Ok(table)
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("F+ schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("F+ schema dependencies");
    conn.execute_batch(SCHEMA).expect("F+ schema");
    rows(&conn).expect("F+ schema shape")
});

/// `None` when nothing of the extension exists, `Some(1)` when its exact shape
/// does; anything in between is refused rather than repaired.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| {
                (
                    name.clone(),
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    if shape.is_empty() {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "F+ curve schema is partial or incompatible".into(),
        ))
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration("F+ curve schema missing".into()));
    }
    let count: i64 = conn.query_row(MARKER_SQL, [], |row| row.get(0))?;
    if count != 1 {
        return Err(StoreError::Migration(
            "F+ curve schema marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "dv_refit_jobs_tests.rs"]
mod tests;
