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
    Ok(Some(result))
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
