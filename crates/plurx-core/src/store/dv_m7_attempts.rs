//! Replicated protocol anchor and single-use candidate reservations.
//!
//! Local encoder artifacts can be recovered under a fresh claim. Only this
//! source-keyed authority can grant a new candidate launch.

use super::{
    background_jobs::{encode, JobToken, QueueSql},
    dv_m7_receipts::fenced_maintenance_sql,
};
use crate::{
    error::StoreError,
    scan::dv_title::{DvTitleStats, TitleConstant, TitleElKind, TitleEligibility},
    transcode::dv_m7_receipt::M7SourceProbe,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;

type Digest = [u8; 32];
const MAX_ATTEMPTS: u8 = 4;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE dv_m7_candidate_attempts_v1 (
  source_sha256 TEXT NOT NULL PRIMARY KEY
    CHECK (length(source_sha256)=64 AND source_sha256 NOT GLOB '*[^0-9a-f]*'),
  protocol_sha256 TEXT NOT NULL
    CHECK (length(protocol_sha256)=64 AND protocol_sha256 NOT GLOB '*[^0-9a-f]*'),
  recipe_sha256 TEXT NOT NULL
    CHECK (length(recipe_sha256)=64 AND recipe_sha256 NOT GLOB '*[^0-9a-f]*'),
  maximum_attempts INTEGER NOT NULL CHECK (maximum_attempts BETWEEN 0 AND 4
    AND (maximum_attempts=0)=(recipe_sha256='0000000000000000000000000000000000000000000000000000000000000000')),
  slots_json TEXT NOT NULL DEFAULT '[]' CHECK (length(slots_json)<=8192
    AND json_valid(slots_json) AND json_type(slots_json)='array'
    AND json_array_length(slots_json)<=maximum_attempts)
) STRICT
"#;

/// Immutable evidence for the one automatic retry of a legacy rate-admission stop.
pub(crate) const SUPERSESSION_SCHEMA: &str = r#"
CREATE TABLE dv_m7_rate_supersessions_v1 (
  revision INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL,
  old_identity TEXT NOT NULL,
  new_identity TEXT NOT NULL,
  fit_table TEXT NOT NULL,
  gate TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  source_sha256 TEXT UNIQUE,
  old_ledger TEXT CHECK (old_ledger IS NULL OR json_valid(old_ledger)),
  new_protocol TEXT,
  new_recipe TEXT,
  new_cap INTEGER,
  CHECK ((source_sha256 IS NULL)=(old_ledger IS NULL)
    AND (source_sha256 IS NULL)=(new_protocol IS NULL)
    AND (source_sha256 IS NULL)=(new_recipe IS NULL)
    AND (source_sha256 IS NULL)=(new_cap IS NULL))
) STRICT
"#;

pub(crate) const SUPERSESSION_SHAPE_SQL: &str =
    "SELECT name,sql FROM sqlite_master WHERE type='table' AND name='dv_m7_rate_supersessions_v1'";

pub(crate) fn has_supersession_schema(rows: &[(String, String)]) -> Result<bool, StoreError> {
    match rows {
        [] => Ok(false),
        [(name, sql)]
            if name == "dv_m7_rate_supersessions_v1"
                && sql
                    .split_whitespace()
                    .eq(SUPERSESSION_SCHEMA.split_whitespace()) =>
        {
            Ok(true)
        }
        _ => Err(StoreError::Migration(
            "invalid M7 rate supersession schema".into(),
        )),
    }
}

pub(crate) const SHAPE_SQL: &str =
    "SELECT name,sql FROM sqlite_master WHERE type='table' AND name='dv_m7_candidate_attempts_v1'";

pub(crate) fn has_schema(rows: &[(String, String)]) -> Result<bool, StoreError> {
    match rows {
        [] => Ok(false),
        [(name, sql)]
            if name == "dv_m7_candidate_attempts_v1"
                && sql.split_whitespace().eq(SCHEMA.split_whitespace()) =>
        {
            Ok(true)
        }
        _ => Err(StoreError::Migration(
            "invalid M7 candidate attempt schema".into(),
        )),
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let mut statement = conn.prepare(SHAPE_SQL)?;
    let observed = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if !has_schema(&observed.collect::<Result<Vec<_>, _>>()?)? {
        return Err(StoreError::Migration(
            "M7 candidate attempt schema missing".into(),
        ));
    }
    let mut query = conn.prepare(SUPERSESSION_SHAPE_SQL)?;
    let observed = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if !has_supersession_schema(&observed.collect::<Result<Vec<_>, _>>()?)? {
        return Err(StoreError::Migration(
            "M7 rate supersession schema missing".into(),
        ));
    }
    Ok(())
}

/// The title attempt supplies these current catalogue facts even when
/// another file with the same complete source bytes created the anchor.
#[derive(Clone, Debug, Serialize)]
pub struct M7CandidateScope {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub probe_revision: i64,
    pub scanner_digest: String,
    pub source_sha256: Digest,
}

#[derive(Clone)]
pub struct M7CandidateAuthority {
    pub token: JobToken,
    pub source_probe: M7SourceProbe,
    pub scope: M7CandidateScope,
    pub now_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M7CandidatePlan {
    pub protocol_sha256: Digest,
    pub recipe_sha256: Digest,
    pub maximum_attempts: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct M7CandidateIntent {
    pub source_sha256: Digest,
    pub protocol_sha256: Digest,
    pub recipe_sha256: Digest,
    pub binding_sha256: Digest,
    pub slot: u8,
    pub target_kbps: u32,
    pub maximum_attempts: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M7CandidateLedger {
    pub source_sha256: Digest,
    pub protocol_sha256: Digest,
    pub recipe_sha256: Digest,
    pub maximum_attempts: u8,
    pub slots: Vec<M7CandidateIntent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M7CandidateAnchor {
    Ready,
    DifferentProtocol,
    Fenced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M7CandidateReservation {
    Reserved,
    AlreadyConsumed,
    Fenced,
    Conflict,
}

impl M7CandidateAuthority {
    fn body(&self) -> Result<Value, StoreError> {
        self.token.validate()?;
        let scope = &self.scope;
        if self.now_ms <= 0
            || scope.file_id <= 0
            || scope.source_size <= 0
            || scope.source_mtime < 0
            || scope.probe_revision <= 0
            || !valid_text_digest(&scope.scanner_digest)
            || scope.source_sha256 == [0; 32]
        {
            return Err(invalid());
        }
        Ok(json!({
            "token": self.token,
            "output": scope,
            "source_probe": self.source_probe.value(),
            "source_sha256": hex::encode(scope.source_sha256),
            "now_ms": self.now_ms,
        }))
    }
}

impl M7CandidatePlan {
    /// A previously sealed experiment has no authority to reserve new slots.
    pub fn sealed_historical(protocol_sha256: Digest) -> Self {
        Self {
            protocol_sha256,
            recipe_sha256: [0; 32],
            maximum_attempts: 0,
        }
    }

    fn validate(self) -> Result<(), StoreError> {
        if self.protocol_sha256 == [0; 32]
            || (self.recipe_sha256 == [0; 32]) != (self.maximum_attempts == 0)
            || self.maximum_attempts > MAX_ATTEMPTS
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl M7CandidateIntent {
    fn plan(&self) -> M7CandidatePlan {
        M7CandidatePlan {
            protocol_sha256: self.protocol_sha256,
            recipe_sha256: self.recipe_sha256,
            maximum_attempts: self.maximum_attempts,
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        self.plan().validate()?;
        if self.source_sha256 == [0; 32]
            || self.binding_sha256 == [0; 32]
            || self.slot >= self.maximum_attempts
            || self.target_kbps == 0
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl M7CandidateLedger {
    fn plan(&self) -> M7CandidatePlan {
        M7CandidatePlan {
            protocol_sha256: self.protocol_sha256,
            recipe_sha256: self.recipe_sha256,
            maximum_attempts: self.maximum_attempts,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Serialize)]
struct StoredLedger {
    source_sha256: String,
    protocol_sha256: String,
    recipe_sha256: String,
    maximum_attempts: u8,
    slots: Vec<M7CandidateIntent>,
}

/// A first complete title scan is input to the independently scheduled F+
/// fitter. Expose that result without settling the still-open title job.
pub(super) async fn record_title_facts<T: QueueSql>(
    store: &T,
    authority: &M7CandidateAuthority,
    stats: &DvTitleStats,
) -> Result<bool, StoreError> {
    if !stats.complete
        || stats.el_kind != TitleConstant::Consistent(TitleElKind::Fel)
        || stats.eligibility() == TitleEligibility::InvalidAggregate
    {
        return Err(invalid());
    }
    let mut body = authority.body()?;
    body["title_stats"] = json!(stats);
    let rows = store
        .queue_sql(TITLE_FACTS_SQL.clone(), encode(&body)?, true, true)
        .await?;
    Ok(!rows.is_empty())
}

pub(super) async fn anchor<T: QueueSql>(
    store: &T,
    authority: &M7CandidateAuthority,
    plan: M7CandidatePlan,
) -> Result<M7CandidateAnchor, StoreError> {
    plan.validate()?;
    let mut body = authority.body()?;
    bind_plan(&mut body, plan);
    let inserted = store
        .queue_sql(ANCHOR_SQL.clone(), encode(&body)?, true, true)
        .await?;
    if !inserted.is_empty() {
        return Ok(M7CandidateAnchor::Ready);
    }
    Ok(match lookup(store, authority).await? {
        Some(ledger) if ledger.plan() == plan => M7CandidateAnchor::Ready,
        Some(_) => M7CandidateAnchor::DifferentProtocol,
        None => M7CandidateAnchor::Fenced,
    })
}

pub(super) async fn lookup<T: QueueSql>(
    store: &T,
    authority: &M7CandidateAuthority,
) -> Result<Option<M7CandidateLedger>, StoreError> {
    let rows = store
        .queue_sql(LOOKUP_SQL.clone(), encode(&authority.body()?)?, false, true)
        .await?;
    let [value] = rows.as_slice() else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err(invalid())
        };
    };
    decode_ledger(value, authority).map(Some)
}

fn decode_ledger(
    value: &str,
    authority: &M7CandidateAuthority,
) -> Result<M7CandidateLedger, StoreError> {
    let stored: StoredLedger = serde_json::from_str(value).map_err(|_| invalid())?;
    let result = M7CandidateLedger {
        source_sha256: read_digest(&stored.source_sha256)?,
        protocol_sha256: read_digest(&stored.protocol_sha256)?,
        recipe_sha256: read_digest(&stored.recipe_sha256)?,
        maximum_attempts: stored.maximum_attempts,
        slots: stored.slots,
    };
    result.plan().validate()?;
    if result.source_sha256 != authority.scope.source_sha256
        || result.slots.len() > usize::from(result.maximum_attempts)
    {
        return Err(invalid());
    }
    for (slot, intent) in result.slots.iter().enumerate() {
        intent.validate()?;
        if usize::from(intent.slot) != slot
            || intent.source_sha256 != result.source_sha256
            || intent.plan() != result.plan()
        {
            return Err(invalid());
        }
    }
    Ok(result)
}

/// The archived stop and exact candidate reservations used for its replay proof.
pub struct M7LegacyRateStop {
    pub gate: String,
    pub ledger: M7CandidateLedger,
}

/// The historical NP1 floor, shared by the sweep and the read-only planner replay.
pub const M7_LEGACY_MIN_TARGET_KBPS: u32 = 83_334;

pub(super) const LEGACY_GATE_PREFIX: &str =
    "NP1 did not land a rate-only candidate: OutsideTargetBounds(";

/// SQL counterpart to `M7LegacyRateStop::refused_target`. The caller supplies
/// the qualified gate column from its own query.
pub(super) fn legacy_rate_gate_sql(gate: &str) -> String {
    let first_digit = LEGACY_GATE_PREFIX.len() + 1;
    let target =
        format!("CAST(substr({gate},{first_digit},length({gate})-{first_digit}) AS INTEGER)");
    format!(
        "({target}>0 AND {target}<{M7_LEGACY_MIN_TARGET_KBPS}
 AND {gate}='{LEGACY_GATE_PREFIX}' || printf('%lld',{target}) || ')')"
    )
}

impl M7LegacyRateStop {
    pub fn refused_target(&self) -> Option<i64> {
        let digits = self
            .gate
            .strip_prefix(LEGACY_GATE_PREFIX)?
            .strip_suffix(')')?;
        digits.parse::<i64>().ok().filter(|value| {
            (1..i64::from(M7_LEGACY_MIN_TARGET_KBPS)).contains(value) && value.to_string() == digits
        })
    }
}

pub(super) async fn legacy_rate_stop<T: QueueSql>(
    store: &T,
    authority: &M7CandidateAuthority,
    fit_table: Digest,
) -> Result<Option<M7LegacyRateStop>, StoreError> {
    let mut body = authority.body()?;
    body["fit_table"] = json!(hex::encode(fit_table));
    let rows = store
        .queue_sql(LEGACY_SQL.clone(), encode(&body)?, false, true)
        .await?;
    #[derive(Deserialize)]
    struct Legacy {
        gate: String,
        ledger: Value,
    }
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let Legacy { gate, ledger } = serde_json::from_str(row).map_err(|_| invalid())?;
    let legacy = M7LegacyRateStop {
        gate,
        ledger: decode_ledger(&ledger.to_string(), authority)?,
    };
    if legacy.refused_target().is_none() {
        return Err(invalid());
    }
    Ok(Some(legacy))
}

pub(super) async fn supersede<T: QueueSql>(
    store: &T,
    authority: &M7CandidateAuthority,
    legacy: &M7LegacyRateStop,
    fit_table: Digest,
    plan: M7CandidatePlan,
) -> Result<M7CandidateAnchor, StoreError> {
    plan.validate()?;
    if legacy.refused_target().is_none()
        || plan.maximum_attempts == 0
        || legacy.ledger.slots.is_empty()
        || plan.maximum_attempts != legacy.ledger.maximum_attempts
        || plan.protocol_sha256 == legacy.ledger.protocol_sha256
        || plan.recipe_sha256 == legacy.ledger.recipe_sha256
        || legacy.ledger.source_sha256 != authority.scope.source_sha256
    {
        return Err(invalid());
    }
    let mut body = authority.body()?;
    bind_plan(&mut body, plan);
    body["fit_table"] = json!(hex::encode(fit_table));
    body["fit_version"] = json!(crate::transcode::dv_refit::FIT_VERSION);
    body["gate"] = json!(legacy.gate);
    body["old"] = serde_json::to_value(StoredLedger {
        source_sha256: hex::encode(legacy.ledger.source_sha256),
        protocol_sha256: hex::encode(legacy.ledger.protocol_sha256),
        recipe_sha256: hex::encode(legacy.ledger.recipe_sha256),
        maximum_attempts: legacy.ledger.maximum_attempts,
        slots: legacy.ledger.slots.clone(),
    })
    .map_err(|_| invalid())?;
    let body = encode(&body)?;
    // The exact legacy reservations move with this source before any new
    // reservation can succeed. Old workers still compare the active plan.
    store
        .queue_transaction(vec![
            (ARCHIVE_SQL.clone(), body.clone()),
            (SUPERSEDE_SQL.clone(), body),
        ])
        .await?;
    Ok(match lookup(store, authority).await? {
        Some(ledger) if ledger.plan() == plan => M7CandidateAnchor::Ready,
        Some(_) => M7CandidateAnchor::DifferentProtocol,
        None => M7CandidateAnchor::Fenced,
    })
}

pub(super) async fn reserve<T: QueueSql>(
    store: &T,
    authority: &M7CandidateAuthority,
    intent: M7CandidateIntent,
) -> Result<M7CandidateReservation, StoreError> {
    intent.validate()?;
    if intent.source_sha256 != authority.scope.source_sha256 {
        return Err(invalid());
    }
    let mut body = authority.body()?;
    bind_plan(&mut body, intent.plan());
    body["slot"] = json!(intent.slot);
    body["intent"] = json!(intent);
    let inserted = store
        .queue_sql(RESERVE_SQL.clone(), encode(&body)?, true, true)
        .await?;
    if !inserted.is_empty() {
        return Ok(M7CandidateReservation::Reserved);
    }
    Ok(match lookup(store, authority).await? {
        Some(ledger)
            if ledger.plan() == intent.plan() && usize::from(intent.slot) < ledger.slots.len() =>
        {
            M7CandidateReservation::AlreadyConsumed
        }
        Some(_) => M7CandidateReservation::Conflict,
        None => M7CandidateReservation::Fenced,
    })
}

fn bind_plan(body: &mut Value, plan: M7CandidatePlan) {
    body["protocol_sha256"] = json!(hex::encode(plan.protocol_sha256));
    body["recipe_sha256"] = json!(hex::encode(plan.recipe_sha256));
    body["maximum_attempts"] = json!(plan.maximum_attempts);
}

fn valid_text_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_digest(digest: &str) -> Result<Digest, StoreError> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(digest, &mut bytes).map_err(|_| invalid())?;
    Ok(bytes)
}

fn invalid() -> StoreError {
    StoreError::Task("invalid M7 candidate attempt authority or ledger".into())
}

fn statement(command: &str) -> String {
    fenced_maintenance_sql(&format!(
        r#"
__M7_AUTHORITY__, permitted AS (
 SELECT body FROM input JOIN files file ON file.id=json_extract(body,'$.output.file_id')
 WHERE EXISTS(SELECT 1 FROM owned) AND __CURRENT_FILE__
)
{command}
"#
    ))
}

static TITLE_FACTS_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(
        r#"
UPDATE files SET probe_json=json_set(probe_json,'$.plurx_dv_title_v1',json_extract($1,'$.title_stats'))
WHERE id=json_extract($1,'$.output.file_id') AND EXISTS(SELECT 1 FROM permitted)
RETURNING 'true' AS result_json
"#,
    )
});

static ANCHOR_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(
        r#"
INSERT INTO dv_m7_candidate_attempts_v1
  (source_sha256,protocol_sha256,recipe_sha256,maximum_attempts)
SELECT json_extract(body,'$.source_sha256'),json_extract(body,'$.protocol_sha256'),
 json_extract(body,'$.recipe_sha256'),json_extract(body,'$.maximum_attempts')
FROM permitted WHERE 1
ON CONFLICT(source_sha256) DO NOTHING
RETURNING 'true' AS result_json
"#,
    )
});

static LOOKUP_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(
        r#"
SELECT json_object('source_sha256',ledger.source_sha256,'protocol_sha256',ledger.protocol_sha256,
 'recipe_sha256',ledger.recipe_sha256,'maximum_attempts',ledger.maximum_attempts,
 'slots',json(ledger.slots_json)) AS result_json
FROM permitted JOIN dv_m7_candidate_attempts_v1 ledger
 ON ledger.source_sha256=json_extract(body,'$.source_sha256')
"#,
    )
});

static RESERVE_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(
        r#"
UPDATE dv_m7_candidate_attempts_v1 AS ledger
SET slots_json=json_insert(slots_json,'$[#]',json_extract($1,'$.intent'))
WHERE EXISTS(SELECT 1 FROM permitted)
 AND ledger.source_sha256=json_extract($1,'$.source_sha256')
 AND ledger.protocol_sha256=json_extract($1,'$.protocol_sha256')
 AND ledger.recipe_sha256=json_extract($1,'$.recipe_sha256')
 AND ledger.maximum_attempts=json_extract($1,'$.maximum_attempts')
 AND json_array_length(ledger.slots_json)=json_extract($1,'$.slot')
 AND json_array_length(ledger.slots_json)<ledger.maximum_attempts
RETURNING 'true' AS result_json
"#,
    )
});

const LEDGER_JSON: &str =
    "json_object('source_sha256',ledger.source_sha256,'protocol_sha256',ledger.protocol_sha256,
 'recipe_sha256',ledger.recipe_sha256,'maximum_attempts',ledger.maximum_attempts,
 'slots',json(ledger.slots_json))";

static LEGACY_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(&format!(
        r#"
SELECT json_object('gate',history.gate,'ledger',json(COALESCE(history.old_ledger,{LEDGER_JSON}))) AS result_json
FROM permitted
JOIN dv_m7_candidate_attempts_v1 ledger ON ledger.source_sha256=json_extract(body,'$.source_sha256')
JOIN dv_m7_rate_supersessions_v1 history ON history.fit_table=json_extract(body,'$.fit_table')
WHERE history.source_sha256=ledger.source_sha256
 OR (history.source_sha256 IS NULL
 AND history.file_id=json_extract(body,'$.output.file_id')
 AND history.revision=json_extract(body,'$.output.probe_revision'))
ORDER BY history.source_sha256 IS NULL LIMIT 1
"#,
    ))
});

static ARCHIVE_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(&format!(
        r#"
UPDATE dv_m7_rate_supersessions_v1 AS history
SET source_sha256=json_extract($1,'$.source_sha256'),
 old_ledger=(SELECT {LEDGER_JSON} FROM dv_m7_candidate_attempts_v1 ledger
  WHERE ledger.source_sha256=json_extract($1,'$.source_sha256')),
 new_protocol=json_extract($1,'$.protocol_sha256'),
 new_recipe=json_extract($1,'$.recipe_sha256'),new_cap=json_extract($1,'$.maximum_attempts')
WHERE EXISTS(SELECT 1 FROM permitted)
 AND history.file_id=json_extract($1,'$.output.file_id')
 AND history.revision=json_extract($1,'$.output.probe_revision')
 AND history.source_sha256 IS NULL
 AND history.fit_table=json_extract($1,'$.fit_table')
 AND history.gate=json_extract($1,'$.gate')
 AND EXISTS(SELECT 1 FROM files f WHERE f.id=history.file_id
  AND history.fit_table={current_fit})
 AND EXISTS(SELECT 1 FROM dv_m7_title_outcomes_v1 outcome WHERE outcome.revision=history.revision
  AND outcome.identity=history.new_identity AND outcome.fit_table=history.fit_table
  AND outcome.outcome='pending')
 AND NOT EXISTS(SELECT 1 FROM dv_m7_rate_supersessions_v1 previous
  WHERE previous.source_sha256=json_extract($1,'$.source_sha256'))
 AND EXISTS(SELECT 1 FROM dv_m7_candidate_attempts_v1 ledger
  WHERE ledger.source_sha256=json_extract($1,'$.source_sha256')
  AND ledger.protocol_sha256=json_extract($1,'$.old.protocol_sha256')
  AND ledger.recipe_sha256=json_extract($1,'$.old.recipe_sha256')
  AND ledger.maximum_attempts=json_extract($1,'$.old.maximum_attempts')
  AND json(ledger.slots_json)=json_extract($1,'$.old.slots'))
"#,
        current_fit = super::dv_m7_outcomes::CURRENT_FIT_TABLE,
    ))
});

static SUPERSEDE_SQL: LazyLock<String> = LazyLock::new(|| {
    statement(
        r#"
UPDATE dv_m7_candidate_attempts_v1 AS ledger
SET protocol_sha256=json_extract($1,'$.protocol_sha256'),
 recipe_sha256=json_extract($1,'$.recipe_sha256'),
 maximum_attempts=json_extract($1,'$.maximum_attempts'),slots_json='[]'
WHERE EXISTS(SELECT 1 FROM permitted)
 AND ledger.source_sha256=json_extract($1,'$.source_sha256')
 AND ledger.protocol_sha256=json_extract($1,'$.old.protocol_sha256')
 AND ledger.recipe_sha256=json_extract($1,'$.old.recipe_sha256')
 AND ledger.maximum_attempts=json_extract($1,'$.old.maximum_attempts')
 AND EXISTS(SELECT 1 FROM dv_m7_rate_supersessions_v1 history
  WHERE history.source_sha256=ledger.source_sha256
  AND history.new_protocol=json_extract($1,'$.protocol_sha256')
  AND history.new_recipe=json_extract($1,'$.recipe_sha256')
  AND history.new_cap=json_extract($1,'$.maximum_attempts'))
"#,
    )
});
