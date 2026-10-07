//! Authenticated candidate failure memory. This is not a network prior or a
//! recovery owner: the existing client/producer budgets still own recovery.
use crate::domain::MediaSessionRoute;
use serde::{Deserialize, Serialize};

/// Exact player, credential and source lifetime. Never a cross-device ceiling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRecoveryScope {
    pub user_id: i64,
    pub playback_id: String,
    pub recovery_epoch: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_object_version: String,
    pub credential_generation: String,
    pub client_class: String,
}

impl CandidateRecoveryScope {
    pub fn valid(&self) -> bool {
        self.user_id > 0
            && self.file_id > 0
            && self.source_size > 0
            && self.source_mtime >= 0
            && !self.source_object_version.is_empty()
            && self.source_object_version.len() <= 256
            && !self.playback_id.is_empty()
            && self.playback_id.len() <= 128
            && !self
                .playback_id
                .bytes()
                .any(|b| matches!(b, 0 | b'\r' | b'\n'))
            && uuid::Uuid::parse_str(&self.recovery_epoch).is_ok()
            && self.credential_generation.len() == 64
            && self
                .credential_generation
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && matches!(
                self.client_class.as_str(),
                "web" | "apple" | "android" | "chrome" | "safari" | "firefox" | "edge" | "other"
            )
    }
    pub(crate) fn key(&self) -> Option<String> {
        self.valid()
            .then(|| serde_json::to_string(self).expect("bounded scope serializes"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRecoveryCause {
    Link,
    Encode,
    Decode,
    Hold,
    Authority,
}
impl CandidateRecoveryCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Encode => "encode",
            Self::Decode => "decode",
            Self::Hold => "hold",
            Self::Authority => "authority",
        }
    }
}

/// Minted above Store only after authenticating the actual incumbent route and
/// reconstructing its full server-owned candidate. SQL rechecks that route.
#[derive(Clone, Debug)]
pub struct CandidateRecoveryObservation {
    pub scope: CandidateRecoveryScope,
    pub route: MediaSessionRoute,
    pub recipe_digest: [u8; 32],
    pub event_id: String,
    pub cause: CandidateRecoveryCause,
    /// Set only for the actual quality-response admission, not fault telemetry.
    pub quality_step: bool,
}
impl CandidateRecoveryObservation {
    pub(crate) fn valid(&self, now_ms: i64) -> bool {
        self.scope.valid()
            && (!self.quality_step || self.cause == CandidateRecoveryCause::Decode)
            && self.route.principal.local_user_id() == Some(self.scope.user_id)
            && self.route.playback_id == self.scope.playback_id
            && self.route.recovery_epoch == self.scope.recovery_epoch
            && self.route.state == "active"
            && self.route.publication_ready_at_ms == 0
            && self.route.lease_expires_at_ms > now_ms
            && self.route.owner_epoch > 0
            && self.route.recipe_json.len() <= 64 * 1024
            && !self.event_id.is_empty()
            && self.event_id.len() <= 128
            && !self
                .event_id
                .bytes()
                .any(|b| matches!(b, 0 | b'\r' | b'\n'))
            && uuid::Uuid::parse_str(&self.route.incarnation_id).is_ok()
            && uuid::Uuid::parse_str(&self.route.session_id).is_ok()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CandidateRecoveryMemory {
    pub rejected_recipes: Vec<[u8; 32]>,
    /// One quality-controller decoder response per exact lifetime. Further
    /// failures remain the existing compatibility owner's, not another step.
    pub decode_step_recipe: Option<[u8; 32]>,
}

/// First SQLite schema containing `candidate_recovery`; stable across later
/// migrations. `sqlite::MIGRATIONS` asserts at compile time that this entry is
/// [`SCHEMA`], so the import gate cannot drift from the migration list.
pub(crate) const SQLITE_INTRODUCED_SCHEMA: i64 = 96;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS candidate_recovery (
 scope TEXT NOT NULL, recipe TEXT NOT NULL, cause TEXT NOT NULL,
 event_id TEXT NOT NULL, user_id INTEGER NOT NULL, playback_id TEXT NOT NULL,
 recovery_epoch TEXT NOT NULL, created_ms INTEGER NOT NULL,
 quality_step INTEGER NOT NULL CHECK(quality_step IN (0,1)),
 PRIMARY KEY(scope,recipe,cause),
 CHECK(cause IN ('link','encode','decode','hold','authority'))
) STRICT;";

// Identical SQLite/Raft predicates. No caller read grants a write: exact
// pointer/owner/incarnation/recipe and current file metadata are checked here.
pub(crate) const OBSERVE_SQL: &str = "WITH input(scope,recipe,cause,event_id,user_id,playback_id,recovery_epoch,created_ms,
 incarnation_id,session_id,owner_node_id,owner_epoch,recipe_json,file_id,source_size,source_mtime,quality_step)
 AS (VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17))
 INSERT INTO candidate_recovery
 (scope,recipe,cause,event_id,user_id,playback_id,recovery_epoch,created_ms,quality_step)
 SELECT i.scope,i.recipe,i.cause,i.event_id,i.user_id,i.playback_id,i.recovery_epoch,i.created_ms,i.quality_step FROM input i
 WHERE EXISTS (SELECT 1 FROM media_sessions r JOIN media_playback_pointers p
  ON p.user_id=r.user_id AND p.playback_id=r.playback_id AND p.current_incarnation_id=r.incarnation_id
  WHERE r.incarnation_id=i.incarnation_id AND r.session_id=i.session_id AND r.owner_node_id=i.owner_node_id
   AND r.owner_epoch=i.owner_epoch AND r.recipe_json=i.recipe_json AND r.state='active' AND r.publication_ready_at_ms=0
   AND r.user_id=i.user_id AND r.playback_id=i.playback_id AND r.recovery_epoch=i.recovery_epoch AND r.lease_expires_at_ms>i.created_ms)
 AND EXISTS (SELECT 1 FROM files WHERE id=i.file_id AND size=i.source_size AND mtime=i.source_mtime)
 AND (SELECT count(*) FROM candidate_recovery WHERE scope=i.scope)<64
 AND (SELECT count(*) FROM candidate_recovery)<4096
 AND (i.quality_step=0 OR NOT EXISTS (SELECT 1 FROM candidate_recovery WHERE scope=i.scope AND cause='decode' AND quality_step=1))
 ON CONFLICT(scope,recipe,cause) DO UPDATE SET quality_step=excluded.quality_step
 WHERE candidate_recovery.quality_step=0 AND excluded.quality_step=1";
pub(crate) const READ_SQL: &str = "SELECT recipe,quality_step FROM candidate_recovery
 WHERE scope=$1 AND cause='decode' ORDER BY created_ms,rowid LIMIT 64";
pub(crate) const PRUNE_SQL: &str = "DELETE FROM candidate_recovery WHERE created_ms<$1
 AND NOT EXISTS (SELECT 1 FROM media_playback_pointers p JOIN media_sessions r
 ON r.incarnation_id=p.current_incarnation_id WHERE r.user_id=candidate_recovery.user_id
 AND r.playback_id=candidate_recovery.playback_id AND r.recovery_epoch=candidate_recovery.recovery_epoch
 AND r.state IN ('active','preparing'))";

pub(crate) fn memory(recipes: Vec<(String, bool)>) -> CandidateRecoveryMemory {
    let recipes: Vec<_> = recipes
        .into_iter()
        .filter_map(|(raw, step)| {
            let bytes = hex::decode(raw).ok()?;
            Some((<[u8; 32]>::try_from(bytes).ok()?, step))
        })
        .collect();
    CandidateRecoveryMemory {
        decode_step_recipe: recipes
            .iter()
            .find(|(_, step)| *step)
            .map(|(recipe, _)| *recipe),
        rejected_recipes: recipes.into_iter().map(|(recipe, _)| recipe).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_recovery_rejects_shared_principal_with_matching_local_scope() {
        let epoch = uuid::Uuid::new_v4().to_string();
        let mut observation = CandidateRecoveryObservation {
            scope: CandidateRecoveryScope {
                user_id: 7,
                playback_id: "player".into(),
                recovery_epoch: epoch.clone(),
                file_id: 1,
                source_size: 100,
                source_mtime: 1,
                source_object_version: "file-version".into(),
                credential_generation: "a".repeat(64),
                client_class: "web".into(),
            },
            route: MediaSessionRoute {
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: uuid::Uuid::new_v4().to_string(),
                principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                playback_id: "player".into(),
                recovery_epoch: epoch,
                request_fingerprint: "b".repeat(64),
                owner_node_id: "owner".into(),
                owner_epoch: 1,
                lease_expires_at_ms: 1000,
                state: "active".into(),
                terminal_reason: None,
                publication_ready_at_ms: 0,
                recipe_json: "{}".into(),
                response_json: "{}".into(),
                produced_playable_through_ms: 0,
                fetched_through_ms: 0,
                media_origin_ms: 0,
                media_sequence: 0,
                discontinuity_sequence: 0,
                updated_at_ms: 1,
                drain_deadline_ms: None,
            },
            recipe_digest: [0; 32],
            event_id: "decoder-failure".into(),
            cause: CandidateRecoveryCause::Decode,
            quality_step: true,
        };
        assert!(observation.valid(10));
        observation.route.principal = crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::new_v4(),
            &"c".repeat(64),
        )
        .expect("shared principal");
        assert!(!observation.valid(10));
        observation.route.principal =
            crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 8 };
        assert!(!observation.valid(10));
    }
}
