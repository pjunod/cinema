//! F+ curve fits for Profile 7 FEL titles (`docs/streaming/FEL-RPU-REFIT-BUILD.md`
//! §3.3–3.5): the `dv_refit` job's demand, its fenced publication, the
//! terminal outcome of each fit identity, and the curve tables copy-path
//! serving reads.
//!
//! What is replicated is only what serving and the operator need: the title
//! row, each scene's span, the served coefficients as the quantised integers
//! `apply_to_rpu` writes, why a scene keeps identity, and its held-out error
//! numbers. A title is published in one replicated write bounded by
//! [`MAX_PUBLISH_BYTES`]; the statistics the fit was solved from stay in the
//! node-local spool of the node that decoded the title.
//!
//! Each curve table is named by its curve digest. Publication makes the new
//! table current and the file's previous one superseded; a superseded table
//! stays addressable by digest for [`RETAIN_SUPERSEDED_MS`], so a session can
//! stay pinned to the table it started with on any node.
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
use crate::transcode::dv_refit::{QuantisedCurves, ServableCurves};

pub(crate) const SCHEMA: &str = include_str!("dv_refit_jobs.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 131;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'background_dv_refit_*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM background_dv_refit_schema WHERE singleton=1 AND version=1";
/// The objects the extension's triggers reference.
const DEPENDENCIES: &str = "CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT,size INTEGER,mtime INTEGER); \
     CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,dedupe_key TEXT,payload_json TEXT,last_error_code TEXT,claim_id TEXT,result_ref TEXT,owner_node_id TEXT,owner_boot_id TEXT,lease_expires_ms INTEGER,revision INTEGER,updated_at_ms INTEGER); \
     CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); \
     CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,result_ref TEXT,updated_at_ms INTEGER);";

/// Bound on one publication's request body. A hiqlite write is one Raft
/// entry, and the WAL refuses an entry above 16 MiB by restarting the node.
/// A scene costs about 1.2 KB with served coefficients, so this holds some
/// 10,000 fitted scenes; a title beyond it fails rather than splitting.
pub const MAX_PUBLISH_BYTES: usize = 12 * 1024 * 1024;
/// Bound on a title's scenes. A three-hour title at 24 fps whose scenes are
/// pooled to at least 24 frames has under 11,000.
pub const MAX_SCENES: usize = 16_384;
pub const MAX_REASON_BYTES: usize = 1024;
/// How long a superseded curve table stays addressable by its digest.
pub const RETAIN_SUPERSEDED_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// `f64` carried as the decimal text Rust writes (`{:e}`, the shortest text
/// that parses back to the same value) and parses: through a JSON request,
/// a TEXT column and back, bit for bit, whatever the JSON parser does with
/// numbers.
pub mod decimal {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn text(v: f64) -> String {
        format!("{v:e}")
    }

    pub fn parse(text: &str) -> Option<f64> {
        (text.len() <= 64)
            .then(|| text.parse::<f64>().ok())
            .flatten()
            .filter(|v| v.is_finite())
    }

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&text(*v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let text = String::deserialize(d)?;
        parse(&text).ok_or_else(|| D::Error::custom("not a finite decimal"))
    }

    pub mod option {
        use serde::{de::Error, Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
            match v {
                Some(v) => s.serialize_some(&super::text(*v)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
            Option::<String>::deserialize(d)?
                .map(|text| super::parse(&text).ok_or_else(|| D::Error::custom("not a finite decimal")))
                .transpose()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefitVerdict {
    /// At least one scene serves fitted curves.
    Fitted,
    /// Every scene serves identity curves, for the recorded reason.
    Identity,
    /// The helper refused the title for its content (exit 3, one stderr
    /// line), recorded so the title is not decoded again until its identity
    /// changes.
    Refused,
}

/// A curve table's title row, without its scenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefitTitle {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    /// The selected video stream the helper decoded.
    pub video_index: i64,
    /// The title-scan generation it was read under. Not part of the fit's
    /// identity: a newer generation that selects the same stream of the same
    /// source only moves this ([`rebind`]).
    pub probe_revision: i64,
    /// The refit mode and the decoder ABI it ran on (`ToolSet::refit_helper_identity`).
    pub helper_identity: String,
    pub fit_version: String,
    /// Names the served curve set (FEL-RPU-REFIT-BUILD.md §3.5): sha256 over
    /// the scenes' spans and quantised coefficients.
    pub curve_digest: String,
    pub verdict: RefitVerdict,
    /// Why the title serves identity, or was refused. `None` when fitted.
    pub reason: Option<String>,
    /// 1 − served/identity pooled held-out squared error, when measurable.
    #[serde(with = "decimal::option")]
    pub gain: Option<f64>,
    pub scene_count: i64,
    pub frames: i64,
}

impl RefitTitle {
    pub fn valid(&self) -> bool {
        self.file_id > 0
            && self.source_size >= 0
            && self.source_mtime >= 0
            && (0..=255).contains(&self.video_index)
            && self.probe_revision > 0
            && digest(&self.helper_identity)
            && valid_fit_version(&self.fit_version)
            && digest(&self.curve_digest)
            && (self.verdict == RefitVerdict::Fitted) == self.reason.is_none()
            && self.reason.as_deref().is_none_or(valid_reason)
            && self.gain.is_none_or(f64::is_finite)
            && (0..=MAX_SCENES as i64).contains(&self.scene_count)
            && self.frames >= 0
            && (self.verdict != RefitVerdict::Refused || self.scene_count == 0)
    }
}

/// One scene of a curve table as stored and replicated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefitSceneRow {
    pub first_frame: i64,
    pub frames: i64,
    pub first_pts: i64,
    /// The scene's last frame's presentation time plus its duration.
    pub end_pts: i64,
    /// `num/den` of the stream's time base, as the helper reported it.
    pub time_base: String,
    /// The served coefficients (numerators over 2^23), or `None` for
    /// identity. Only a fitted title serves any.
    pub coefficients: Option<QuantisedCurves>,
    /// Why the scene's own curves were not usable; `None` when they passed.
    pub reason: Option<String>,
    /// Held-out squared error of the identity curves, in 12-bit codes².
    #[serde(with = "decimal")]
    pub identity_error: f64,
    /// Held-out squared error of the scene's own curves, when they passed.
    #[serde(with = "decimal::option")]
    pub fitted_error: Option<f64>,
    /// 1 − fitted/identity on the held-out samples, when the curves passed.
    #[serde(with = "decimal::option")]
    pub gain: Option<f64>,
}

impl RefitSceneRow {
    pub fn valid(&self) -> bool {
        self.first_frame >= 0
            && self.frames > 0
            && self.end_pts > self.first_pts
            && valid_time_base(&self.time_base)
            && self.reason.as_deref().is_none_or(valid_reason)
            && !(self.coefficients.is_some() && self.reason.is_some())
            && self.reason.is_some() == self.fitted_error.is_none()
            && self.fitted_error.is_some() == self.gain.is_some()
            && self.identity_error.is_finite()
            && self.coefficients.as_ref().is_none_or(|q| {
                ServableCurves::from_quantised(q).is_ok()
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRefitJob {
    pub token: JobToken,
    pub title: RefitTitle,
    pub scenes: Vec<RefitSceneRow>,
    pub now_ms: i64,
}

impl PublishRefitJob {
    /// Everything about the publication that does not depend on the store:
    /// the rows agree with each other and with their digest, every served
    /// table loads back, and the whole request fits one replicated write.
    /// An error here is deterministic: the same rows fail again.
    pub fn check(&self) -> Result<(), String> {
        let title = &self.title;
        if self.now_ms < 0 || !title.valid() {
            return Err("invalid F+ title row".into());
        }
        if self.scenes.len() as i64 != title.scene_count
            || self.scenes.iter().map(|s| s.frames).sum::<i64>() != title.frames
        {
            return Err("the F+ scenes disagree with their title".into());
        }
        if let Some(scene) = self.scenes.iter().find(|s| !s.valid()) {
            return Err(format!(
                "invalid F+ scene row at frame {}",
                scene.first_frame
            ));
        }
        if title.verdict != RefitVerdict::Fitted
            && self.scenes.iter().any(|s| s.coefficients.is_some())
        {
            return Err("an identity title serves no curves".into());
        }
        for (i, pair) in self.scenes.windows(2).enumerate() {
            let (a, b) = (&pair[0], &pair[1]);
            if b.first_frame != a.first_frame + a.frames
                || b.first_pts <= a.first_pts
                || b.time_base != a.time_base
            {
                return Err(format!("F+ scenes {i} and {} are not contiguous", i + 1));
            }
        }
        if self.scenes.first().is_some_and(|s| s.first_frame != 0) {
            return Err("F+ scenes do not start at the title's first frame".into());
        }
        let recomputed = crate::transcode::dv_refit_title::curve_digest(self.scenes.iter().map(
            |s| crate::transcode::dv_refit_title::DigestScene {
                first_frame: s.first_frame as u64,
                frames: s.frames as u64,
                first_pts: s.first_pts,
                end_pts: s.end_pts,
                time_base: &s.time_base,
                coefficients: s.coefficients.as_ref(),
            },
        ));
        if recomputed != title.curve_digest {
            return Err("the F+ curve digest does not name these rows".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?.len();
        if bytes > MAX_PUBLISH_BYTES {
            return Err(format!(
                "the F+ table is {bytes} bytes, over the {MAX_PUBLISH_BYTES}-byte publication bound"
            ));
        }
        Ok(())
    }
}

/// A Profile 7 FEL title whose complete title facts are published and whose
/// current fit, or terminal outcome, is missing or was read under an older
/// title-scan generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefitDemand {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    /// The title scan's demand generation: it moves with every fresh probe,
    /// source change and scanner rebind.
    pub probe_revision: i64,
    pub probe_json: String,
    /// The video stream of the file's current table for this source, helper
    /// and fit version, when there is one (its generation is older).
    pub stored_video_index: Option<i64>,
    /// Identities of this source, helper and fit version that ended
    /// terminally (failed, or cancelled), with their video streams.
    #[serde(default)]
    pub terminal: Vec<RefitTerminal>,
    /// The oldest queued `dv_refit` job of the file, of any identity.
    pub queued: Option<RefitQueued>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefitTerminal {
    pub identity: String,
    pub video_index: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefitQueued {
    pub job_id: String,
    /// Its dedupe key: the identity it fits.
    pub identity: String,
}

/// Moves a stored table's (and an identity's outcome's) title-scan
/// generation, when a newer generation selects the same video stream of the
/// same source: decision G, the probe revision matters only through them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebindRefit {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub video_index: i64,
    pub helper_identity: String,
    pub fit_version: String,
    /// The fit identity's dedupe key, for its outcome.
    pub identity: String,
    pub probe_revision: i64,
}

/// What the Developer card counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefitProgress {
    /// Profile 7 titles the FEL route has facts for, for their current source.
    pub fel_titles: i64,
    /// Of those, the ones with a current table, by verdict.
    pub fitted: i64,
    pub identity: i64,
    pub refused: i64,
}

/// One scene of a curve table, as copy-path serving applies it.
#[derive(Debug, Clone, PartialEq)]
pub struct RefitCurveScene {
    pub first_frame: i64,
    pub frames: i64,
    /// Scenes are keyed by their first presentation time (§3.5): a scene
    /// covers [first_ptsᵢ, first_ptsᵢ₊₁), the last one to its `end_pts`. A
    /// seek starts mid-title, so a running frame count cannot find a scene.
    pub first_pts: i64,
    pub end_pts: i64,
    pub time_base: String,
    /// The served curves, checked again as they load
    /// ([`ServableCurves::from_quantised`]), or `None` for identity. Always
    /// `None` unless the title's verdict is [`RefitVerdict::Fitted`].
    pub curves: Option<ServableCurves>,
}

/// A curve table: its title row and scenes, for the source the catalogue
/// row still describes.
#[derive(Debug, Clone, PartialEq)]
pub struct RefitCurveTable {
    pub title: RefitTitle,
    /// When the file's next table replaced this one; `None` while current.
    pub superseded_at_ms: Option<i64>,
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
/// one snapshot: what publication requires of the writer.
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
  AND json_extract(job.payload_json,'$.video_index')=json_extract(body,'$.title.video_index')
  AND json_extract(job.payload_json,'$.probe_revision')=json_extract(body,'$.title.probe_revision')
  AND json_extract(job.payload_json,'$.helper_identity')=json_extract(body,'$.title.helper_identity')
  AND json_extract(job.payload_json,'$.fit_version')=json_extract(body,'$.title.fit_version')),0) AS owns
 FROM input LEFT JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
), verdict AS (
 SELECT *,CASE WHEN state='succeeded' AND same_attempt AND result_ref=reference THEN 'already_published'
 WHEN NOT owns THEN 'lost_ownership'
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

/// The FEL route's facts for a title's current source: a pending FEL recipe
/// request, a stored recipe verdict, or a legacy verdict. Only the FEL route
/// writes those, and it plans only for a source whose RPUs parse as FEL
/// (`DvElKind::Fel`); no other durable fact separates FEL from MEL.
fn fel_sql() -> String {
    let fel = |key: &str, size: &str| {
        format!(
            "EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(f.probe_json,'$.{key}')='object'
   THEN json_extract(f.probe_json,'$.{key}') ELSE '{{}}' END) fact
   WHERE json_extract(fact.value,'$.{size}source_size')=f.size AND json_extract(fact.value,'$.{size}source_mtime')=f.mtime)"
        )
    };
    let legacy = crate::transcode::dv_fel_qualification::PROBE_KEY;
    format!(
        "f.video_codec='hevc' AND f.dv_profile=7 AND f.dv_el_present=1 AND f.dv_rpu_present=1
 AND json_valid(f.probe_json)
 AND ({requests} OR {variants}
  OR (json_type(f.probe_json,'$.{legacy}')='object' AND json_extract(f.probe_json,'$.{legacy}.source_size')=f.size
   AND json_extract(f.probe_json,'$.{legacy}.source_mtime')=f.mtime))",
        requests = fel(crate::transcode::dv_fel_qualification::REQUESTS_KEY, ""),
        variants = fel(
            crate::transcode::dv_fel_qualification::VARIANTS_KEY,
            "verdict."
        ),
    )
}

/// Profile 7 FEL titles after `after_file_id`, in file order, whose complete
/// title facts are published and that need the job or a rebind: no current
/// table and no terminal outcome for this source, helper and fit version
/// read under the current title-scan generation, and no attempt running.
/// The complete title aggregate is a precondition, not part of the identity.
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
    let same = |t: &str| {
        format!(
            "{t}.file_id=f.id AND {t}.source_size=f.size AND {t}.source_mtime=f.mtime
  AND {t}.helper_identity=json_extract($1,'$.helper') AND {t}.fit_version=json_extract($1,'$.fit')"
        )
    };
    let sql = format!(
        r#"
SELECT json_object('file_id',f.id,'source_size',f.size,'source_mtime',f.mtime,'probe_revision',d.revision,
 'probe_json',f.probe_json,
 'stored_video_index',(SELECT t.video_index FROM background_dv_refit_titles t
  WHERE t.superseded_at_ms IS NULL AND {title}),
 'terminal',json((SELECT json_group_array(json_object('identity',o.identity,'video_index',o.video_index))
  FROM background_dv_refit_outcomes o WHERE {outcome})),
 'queued',json((SELECT json_object('job_id',j.id,'identity',j.dedupe_key) FROM background_jobs j
  WHERE j.kind='dv_refit' AND j.state='queued' AND json_extract(j.payload_json,'$.file_id')=f.id
  ORDER BY j.created_at_ms,j.id LIMIT 1))) AS result_json
FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
WHERE f.id>json_extract($1,'$.after') AND {fel}
 AND json_type(f.probe_json,'$.{complete}')='object' AND json_extract(f.probe_json,'$.{complete}.complete')=1
 AND NOT EXISTS(SELECT 1 FROM background_dv_refit_titles t WHERE t.superseded_at_ms IS NULL AND {title}
  AND t.probe_revision=d.revision)
 AND NOT EXISTS(SELECT 1 FROM background_dv_refit_outcomes o WHERE {outcome} AND o.probe_revision=d.revision)
 AND NOT EXISTS(SELECT 1 FROM background_jobs j WHERE j.kind='dv_refit' AND j.state IN ('running','cancelling')
  AND json_extract(j.payload_json,'$.file_id')=f.id)
ORDER BY f.id LIMIT json_extract($1,'$.limit')
"#,
        title = same("t"),
        outcome = same("o"),
        fel = fel_sql(),
        complete = crate::scan::dv_title::PROBE_FIELD,
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

/// Publish the title's whole curve table under the running attempt's token,
/// in one replicated write, and settle the job.
pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishRefitJob,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    input
        .check()
        .map_err(|error| StoreError::Task(format!("invalid F+ publication: {error}")))?;
    let body = serde_json::to_value(input).map_err(|e| StoreError::Task(e.to_string()))?;
    let rows = store
        .queue_sql(PUBLISH_SQL.clone(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("no F+ publication verdict".into()))?,
    )
}

async fn command<T: QueueSql>(
    store: &T,
    operation: &str,
    body: serde_json::Value,
) -> Result<(), StoreError> {
    let mut body = body;
    body["id"] = uuid::Uuid::new_v4().to_string().into();
    body["operation"] = operation.into();
    store
        .queue_sql(
            "INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract($1,'$.id'),json_extract($1,'$.operation'),json($1),'{}' RETURNING 'true' AS result_json"
                .into(),
            encode(&body)?,
            true,
            true,
        )
        .await?;
    Ok(())
}

pub(super) async fn rebind<T: QueueSql>(store: &T, input: RebindRefit) -> Result<(), StoreError> {
    if input.file_id <= 0
        || !(0..=255).contains(&input.video_index)
        || input.probe_revision <= 0
        || !digest(&input.helper_identity)
        || !valid_fit_version(&input.fit_version)
        || input.identity.is_empty()
        || input.identity.len() > 128
    {
        return Err(StoreError::Task("invalid F+ rebind".into()));
    }
    command(
        store,
        "rebind_dv_refit",
        serde_json::to_value(input).map_err(|e| StoreError::Task(e.to_string()))?,
    )
    .await
}

/// Drops curve tables superseded more than [`RETAIN_SUPERSEDED_MS`] before
/// `now_ms`, and outcomes of sources the catalogue no longer has.
pub(super) async fn prune<T: QueueSql>(store: &T, now_ms: i64) -> Result<(), StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid F+ prune".into()));
    }
    command(
        store,
        "prune_dv_refit",
        serde_json::json!({"now_ms": now_ms}),
    )
    .await
}

pub(super) async fn progress<T: QueueSql>(store: &T) -> Result<RefitProgress, StoreError> {
    let sql = format!(
        r#"
SELECT json_object('fel_titles',count(*),
 'fitted',COALESCE(sum(t.verdict='fitted'),0),'identity',COALESCE(sum(t.verdict='identity'),0),
 'refused',COALESCE(sum(t.verdict='refused'),0)) AS result_json
FROM files f LEFT JOIN background_dv_refit_titles t ON t.file_id=f.id AND t.superseded_at_ms IS NULL
 AND t.source_size=f.size AND t.source_mtime=f.mtime
WHERE {fel} AND json_type($1)='object'
"#,
        fel = fel_sql()
    );
    let rows = store.queue_sql(sql, "{}".into(), false, false).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(Option::unwrap_or_default)
}

/// The file's current curve table, for the source its row still describes.
pub(super) async fn curves<T: QueueSql>(
    store: &T,
    file_id: i64,
) -> Result<Option<RefitCurveTable>, StoreError> {
    if file_id <= 0 {
        return Err(StoreError::Task("invalid F+ curve lookup".into()));
    }
    table(store, file_id, None).await
}

/// One of the file's curve tables by digest, current or superseded within
/// [`RETAIN_SUPERSEDED_MS`], for the source its row still describes.
pub(super) async fn curves_by_digest<T: QueueSql>(
    store: &T,
    file_id: i64,
    curve_digest: &str,
) -> Result<Option<RefitCurveTable>, StoreError> {
    if file_id <= 0 || !digest(curve_digest) {
        return Err(StoreError::Task("invalid F+ curve lookup".into()));
    }
    table(store, file_id, Some(curve_digest)).await
}

/// Scenes are in presentation order and carry curves only when the title
/// serves them. Read with the authority's consistency: a follower's local
/// copy may still hold a table publication has already superseded.
async fn table<T: QueueSql>(
    store: &T,
    file_id: i64,
    curve_digest: Option<&str>,
) -> Result<Option<RefitCurveTable>, StoreError> {
    let rows = store
        .queue_sql(
            r#"
SELECT json_object('title',json_object('file_id',t.file_id,'source_size',t.source_size,
  'source_mtime',t.source_mtime,'video_index',t.video_index,'probe_revision',t.probe_revision,
  'helper_identity',t.helper_identity,'fit_version',t.fit_version,'curve_digest',t.curve_digest,
  'verdict',t.verdict,'reason',t.reason,'gain',t.gain,'scene_count',t.scene_count,'frames',t.frames),
 'superseded_at_ms',t.superseded_at_ms,
 'scene',CASE WHEN s.scene_index IS NULL THEN NULL ELSE json_object('index',s.scene_index,
  'first_frame',s.first_frame,'frames',s.frames,'first_pts',s.first_pts,'end_pts',s.end_pts,'time_base',s.time_base,
  'coefficients',CASE WHEN t.verdict='fitted' AND s.coefficients IS NOT NULL THEN json(s.coefficients) END) END) AS result_json
FROM background_dv_refit_titles t JOIN files f ON f.id=t.file_id AND f.size=t.source_size AND f.mtime=t.source_mtime
LEFT JOIN background_dv_refit_scenes s ON s.file_id=t.file_id AND s.curve_digest=t.curve_digest
WHERE t.file_id=json_extract($1,'$.file_id')
 AND CASE WHEN json_extract($1,'$.digest') IS NULL THEN t.superseded_at_ms IS NULL
  ELSE t.curve_digest=json_extract($1,'$.digest') END
ORDER BY s.scene_index
"#
            .into(),
            encode(&serde_json::json!({"file_id": file_id, "digest": curve_digest}))?,
            false,
            true,
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
        coefficients: Option<QuantisedCurves>,
    }
    #[derive(Deserialize)]
    struct Row {
        title: RefitTitle,
        superseded_at_ms: Option<i64>,
        scene: Option<Scene>,
    }
    let mut table: Option<RefitCurveTable> = None;
    for row in rows {
        let row: Row = decode(&row)?;
        let table = table.get_or_insert_with(|| RefitCurveTable {
            title: row.title.clone(),
            superseded_at_ms: row.superseded_at_ms,
            scenes: Vec::new(),
        });
        if let Some(scene) = row.scene {
            if scene.index != table.scenes.len() as i64 {
                return Err(StoreError::Task("F+ scene table is not contiguous".into()));
            }
            let curves = scene
                .coefficients
                .as_ref()
                .map(ServableCurves::from_quantised)
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
