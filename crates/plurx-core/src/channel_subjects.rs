//! Durable subject work and validated metadata decisions. SQL is shared by both stores.
use crate::library_channels::{normalize_subject_text, ChannelCandidate, LibraryChannelRecipe};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const BATCH_SIZE: usize = 8;
pub const INPUT_CHARS: usize = 12_000;
pub const CLAIM_MS: i64 = 120_000;
pub const PREVIEW_TTL_MS: i64 = 86_400_000;
pub const FINISHED_TTL_MS: i64 = 7 * PREVIEW_TTL_MS;
pub const CACHE_TTL_MS: i64 = 30 * PREVIEW_TTL_MS;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS library_channel_subject_jobs (
 id TEXT PRIMARY KEY,
 owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 channel_id TEXT REFERENCES library_channels(id) ON DELETE CASCADE,
 channel_revision INTEGER,
 identity TEXT NOT NULL,
 state TEXT NOT NULL,
 payload TEXT NOT NULL,
 claim_id TEXT,
 claim_expires_ms INTEGER NOT NULL DEFAULT 0,
 available_ms INTEGER NOT NULL,
 updated_ms INTEGER NOT NULL,
 expires_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS channel_subject_job_queue ON library_channel_subject_jobs(state, available_ms);
CREATE INDEX IF NOT EXISTS channel_subject_job_identity ON library_channel_subject_jobs(owner_user_id, identity);
CREATE TABLE IF NOT EXISTS library_channel_subject_decisions (
 owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 subject_digest TEXT NOT NULL,
 item_id INTEGER NOT NULL,
 metadata_digest TEXT NOT NULL,
 classifier_profile TEXT NOT NULL,
 payload TEXT NOT NULL,
 last_used_ms INTEGER NOT NULL,
 PRIMARY KEY(owner_user_id, subject_digest, item_id, metadata_digest, classifier_profile)
) STRICT;
CREATE INDEX IF NOT EXISTS channel_subject_decision_expiry ON library_channel_subject_decisions(last_used_ms);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Match,
    NoMatch,
    Uncertain,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SubjectDecision {
    pub verdict: Verdict,
    pub reason: String,
    pub evidence_field: String,
    pub evidence_quote: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDecision {
    #[serde(skip)]
    pub last_used_ms: i64,
    pub item_id: i64,
    pub metadata_digest: String,
    #[serde(flatten)]
    pub decision: SubjectDecision,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub total: usize,
    pub processed: usize,
    pub matched: usize,
    pub rejected: usize,
    pub uncertain: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectJob {
    pub id: String,
    pub owner_user_id: i64,
    pub channel_id: Option<String>,
    pub channel_revision: Option<i64>,
    pub identity: String,
    pub recipe: LibraryChannelRecipe,
    pub seed: [u8; 32],
    pub subject_digest: String,
    pub classifier_profile: Option<String>,
    pub catalogue_digest: Option<String>,
    pub state: String,
    pub counts: Counts,
    pub error: Option<String>,
    pub result_revision: u64,
    pub created_ms: i64,
    pub published_ms: Option<i64>,
    #[serde(default)]
    pub activate_next_programme: bool,
    #[serde(default)]
    pub selection_count: usize,
}
impl SubjectJob {
    pub fn new(
        owner: i64,
        recipe: LibraryChannelRecipe,
        seed: [u8; 32],
        channel: Option<(String, i64)>,
        now: i64,
    ) -> Self {
        let identity = digest(&(owner, &recipe, seed, &channel));
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            owner_user_id: owner,
            channel_id: channel.as_ref().map(|v| v.0.clone()),
            channel_revision: channel.map(|v| v.1),
            identity,
            subject_digest: digest(&recipe.subject),
            recipe,
            seed,
            classifier_profile: None,
            catalogue_digest: None,
            state: "queued".into(),
            counts: Counts::default(),
            error: None,
            result_revision: 0,
            created_ms: now,
            published_ms: None,
            activate_next_programme: false,
            selection_count: 0,
        }
    }
}

/// Only evidence fields, never file paths or account information, reach the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub fields: BTreeMap<String, String>,
    pub missing_overview: bool,
    pub truncated: bool,
}
impl Metadata {
    pub fn from_candidate(c: &ChannelCandidate) -> Self {
        let mut fields = BTreeMap::new();
        let mut truncated = false;
        for (name, text, limit) in [
            ("title", c.title.as_str(), 500),
            ("overview", c.overview.as_str(), 1200),
            (
                "show_title",
                c.show_title.as_deref().unwrap_or_default(),
                500,
            ),
            ("show_overview", c.show_overview.as_str(), 1200),
        ] {
            let normalized =
                normalize_subject_text(&text.split_whitespace().collect::<Vec<_>>().join(" "));
            truncated |= normalized.chars().count() > limit;
            fields.insert(name.into(), normalized.chars().take(limit).collect());
        }
        for (name, values) in [
            ("genres", &c.genres),
            ("tags", &c.tags),
            ("show_genres", &c.show_genres),
            ("show_tags", &c.show_tags),
        ] {
            let text = normalize_subject_text(&values.join(", "));
            truncated |= text.chars().count() > 500;
            fields.insert(name.into(), text.chars().take(500).collect());
        }
        fields.insert(
            "kind".into(),
            match c.kind {
                crate::library_channels::ChannelItemKind::Movie => "movie",
                _ => "episode",
            }
            .into(),
        );
        fields.insert(
            "year".into(),
            c.year.map(|v| v.to_string()).unwrap_or_default(),
        );
        if c.season_number.is_some() || c.episode_number.is_some() {
            fields.insert(
                "episode_identity".into(),
                format!(
                    "season {}, episode {}",
                    c.season_number.map(|v| v.to_string()).unwrap_or_default(),
                    c.episode_number.map(|v| v.to_string()).unwrap_or_default()
                ),
            );
        }
        Self {
            fields,
            missing_overview: c.overview.trim().is_empty(),
            truncated,
        }
    }
    pub fn from_candidate_local(c: &ChannelCandidate) -> Self {
        let mut result = Self::from_candidate(c);
        for (key, value) in [
            ("title", c.title.clone()),
            ("overview", c.overview.clone()),
            ("tags", c.tags.join(", ")),
            ("genres", c.genres.join(", ")),
            ("show_title", c.show_title.clone().unwrap_or_default()),
            ("show_overview", c.show_overview.clone()),
            ("show_tags", c.show_tags.join(", ")),
            ("show_genres", c.show_genres.join(", ")),
        ] {
            result.fields.insert(key.into(), value);
        }
        result.truncated = false;
        result
    }
    pub fn digest(c: &ChannelCandidate) -> String {
        // Hash full source metadata, including beyond the bounded prompt.
        digest(&(
            "nfc-input-v1",
            &c.title,
            &c.overview,
            &c.show_overview,
            &c.genres,
            &c.tags,
            &c.show_genres,
            &c.show_tags,
            c.year,
            c.kind,
            c.show_id,
            &c.show_title,
            c.season_number,
            c.episode_number,
        ))
    }
    pub fn validates(&self, d: &SubjectDecision) -> bool {
        !d.reason.trim().is_empty()
            && d.reason.chars().count() <= 240
            && d.evidence_quote.chars().count() <= 160
            && self.fields.contains_key(&d.evidence_field)
            && (d.evidence_quote.is_empty()
                || self.fields[&d.evidence_field].contains(&d.evidence_quote))
            && (d.verdict != Verdict::Match || !d.evidence_quote.trim().is_empty())
    }
}
pub fn digest(value: &impl Serialize) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).expect("domain values serialize"),
    ))
}

/// Text literals use UTF-8 hex to keep SQL data separate from syntax in both adapters.
fn text(s: &str) -> String {
    format!("CAST(X'{}' AS TEXT)", hex::encode(s))
}
fn optional(s: Option<&str>) -> String {
    s.map(text).unwrap_or_else(|| "NULL".into())
}
fn payload(v: &impl Serialize) -> String {
    text(&serde_json::to_string(v).expect("domain values serialize"))
}
fn revision_fence() -> &'static str {
    "(channel_id IS NULL OR EXISTS(SELECT 1 FROM library_channels c WHERE c.id=channel_id AND c.owner_user_id=library_channel_subject_jobs.owner_user_id AND c.definition_revision=channel_revision))"
}
#[derive(Clone)]
pub enum JobQuery {
    Profile {
        owner: i64,
        subject: String,
        now: i64,
    },
    Id {
        owner: i64,
        admin: bool,
        id: String,
        now: i64,
    },
    Identity {
        owner: i64,
        identity: String,
        now: i64,
    },
    Pending {
        now: i64,
    },
    Channel {
        owner: i64,
        id: String,
        revision: i64,
        now: i64,
    },
}
impl JobQuery {
    pub fn sql(&self) -> String {
        let (condition,order)=match self {
            Self::Profile{owner,subject,now}=>(format!("owner_user_id={owner} AND json_extract(payload, '$.subject_digest')={} AND json_extract(payload, '$.classifier_profile') IS NOT NULL AND expires_ms>{now}",text(subject)),"ORDER BY updated_ms DESC"),
            Self::Id{owner,admin,id,now}=>(format!("id={} AND (owner_user_id={owner} OR {}) AND expires_ms>{now}",text(id),i32::from(*admin)),""),
            Self::Identity{owner,identity,now}=>(format!("owner_user_id={owner} AND identity={} AND expires_ms>{now} AND state NOT IN ('cancelled','superseded')",text(identity)),"ORDER BY updated_ms DESC"),
            Self::Channel{owner,id,revision,now}=>(format!("owner_user_id={owner} AND channel_id={} AND channel_revision={revision} AND expires_ms>{now} AND state NOT IN ('cancelled','superseded')",text(id)),"ORDER BY updated_ms DESC"),
            Self::Pending{now}=>(format!("state IN ('queued','running','waiting_for_provider') AND available_ms<={now} AND expires_ms>{now} AND claim_expires_ms<={now} AND {}",revision_fence()),"ORDER BY channel_id IS NOT NULL, updated_ms"),
        };
        format!("SELECT payload, state FROM library_channel_subject_jobs WHERE {condition} {order} LIMIT 1")
    }
}
#[derive(Clone)]
pub enum JobWrite {
    Enqueue(SubjectJob),
    Touch {
        owner: i64,
        subject: String,
        profile: String,
        keys: Vec<(i64, String)>,
        now: i64,
    },
    Supersede {
        id: String,
        now: i64,
    },
    Claim {
        id: String,
        claim: String,
        now: i64,
    },
    Renew {
        id: String,
        claim: String,
        now: i64,
    },
    Commit {
        job: SubjectJob,
        claim: String,
        decisions: Vec<CachedDecision>,
        now: i64,
        delay_ms: i64,
    },
    Cancel {
        id: String,
        owner: i64,
        admin: bool,
        now: i64,
    },
    Prune {
        now: i64,
    },
}
impl JobWrite {
    pub fn sql(&self) -> Vec<String> {
        match self {
            Self::Touch{owner,subject,profile,keys,now}=>{
                let query=decision_query(*owner,subject,profile,keys);
                let condition=query.split_once(" WHERE ").expect("domain query").1.split(" ORDER BY").next().expect("condition");
                vec![format!("UPDATE library_channel_subject_decisions SET last_used_ms={now} WHERE {condition} AND last_used_ms<{}",now-PREVIEW_TTL_MS)]
            }
            Self::Supersede{id,now}=>vec![format!("UPDATE library_channel_subject_jobs SET state='superseded',claim_id=NULL,claim_expires_ms=0,updated_ms={now} WHERE id={} AND claim_expires_ms<={now}",text(id))],
            Self::Enqueue(j)=>vec![format!("INSERT INTO library_channel_subject_jobs(id,owner_user_id,channel_id,channel_revision,identity,state,payload,available_ms,updated_ms,expires_ms) SELECT {},{},{},{},{},'queued',{},{},{},{} WHERE NOT EXISTS(SELECT 1 FROM library_channel_subject_jobs WHERE owner_user_id={} AND identity={} AND expires_ms>{} AND state NOT IN ('cancelled','superseded')) AND ({} OR (SELECT COUNT(*) FROM library_channel_subject_jobs WHERE owner_user_id={} AND expires_ms>{})<200)",
                text(&j.id),j.owner_user_id,optional(j.channel_id.as_deref()),j.channel_revision.map(|v|v.to_string()).unwrap_or_else(||"NULL".into()),text(&j.identity),payload(j),j.created_ms,j.created_ms,j.created_ms+if j.channel_id.is_some(){FINISHED_TTL_MS}else{PREVIEW_TTL_MS},j.owner_user_id,text(&j.identity),j.created_ms,i32::from(j.channel_id.is_some()),j.owner_user_id,j.created_ms)],
            Self::Claim{id,claim,now}=>vec![format!("UPDATE library_channel_subject_jobs SET claim_id={},claim_expires_ms={},state='running' WHERE id={} AND state IN ('queued','running','waiting_for_provider') AND expires_ms>{now} AND available_ms<={now} AND claim_expires_ms<={now} AND NOT EXISTS(SELECT 1 FROM library_channel_subject_jobs WHERE claim_expires_ms>{now}) AND {}",text(claim),now+CLAIM_MS,text(id),revision_fence())],
            Self::Renew{id,claim,now}=>vec![format!("UPDATE library_channel_subject_jobs SET claim_expires_ms={} WHERE id={} AND claim_id={} AND claim_expires_ms>{now} AND state='running' AND {}",now+CLAIM_MS,text(id),text(claim),revision_fence())],
            Self::Commit{job:j,claim,decisions,now,delay_ms}=>{
                let fence=format!("id={} AND claim_id={} AND claim_expires_ms>{now} AND state='running' AND {}",text(&j.id),text(claim),revision_fence());
                let mut sql=Vec::new();
                for d in decisions.iter().take(200) {
                    sql.push(format!("INSERT OR IGNORE INTO library_channel_subject_decisions(owner_user_id,subject_digest,item_id,metadata_digest,classifier_profile,payload,last_used_ms) SELECT {},{},{},{},{},{},{now} WHERE EXISTS(SELECT 1 FROM library_channel_subject_jobs WHERE {fence})",j.owner_user_id,text(&j.subject_digest),d.item_id,text(&d.metadata_digest),text(j.classifier_profile.as_deref().unwrap_or_default()),payload(d)));
                }

                if j.state == "complete" && j.selection_count == 0 {
                    if let (Some(channel),Some(revision)) = (&j.channel_id,j.channel_revision) {
                        sql.push(format!("UPDATE library_channels SET build_state='failed',build_error_code='channel_empty',build_error_message='No eligible subject matches; any existing schedule is retained.',build_candidate_count={},build_entry_count=0,build_last_attempt_ms={now} WHERE id={} AND definition_revision={revision} AND EXISTS(SELECT 1 FROM library_channel_subject_jobs WHERE {fence})",j.counts.total,text(channel)));
                    }
                }
                sql.push(format!("UPDATE library_channel_subject_jobs SET payload={},state={},claim_id=NULL,claim_expires_ms=0,updated_ms={now},available_ms={},expires_ms={} WHERE {fence}",payload(j),text(&j.state),now+delay_ms,if j.channel_id.is_some(){now+FINISHED_TTL_MS}else{j.created_ms+PREVIEW_TTL_MS}));sql
            },
            Self::Cancel{id,owner,admin,now}=>vec![format!("UPDATE library_channel_subject_jobs SET state='cancelled',claim_id=NULL,claim_expires_ms=0,updated_ms={now},expires_ms=MIN(expires_ms,{now}+60000) WHERE id={} AND channel_id IS NULL AND (owner_user_id={owner} OR {})",text(id),i32::from(*admin))],
            Self::Prune{now}=>vec![format!("DELETE FROM library_channel_subject_jobs WHERE id IN (SELECT id FROM library_channel_subject_jobs WHERE expires_ms<={now} AND claim_expires_ms<={now} LIMIT 200)"),format!("DELETE FROM library_channel_subject_decisions WHERE (owner_user_id,subject_digest,item_id,metadata_digest,classifier_profile) IN (SELECT d.owner_user_id,d.subject_digest,d.item_id,d.metadata_digest,d.classifier_profile FROM library_channel_subject_decisions d WHERE last_used_ms<{} AND NOT EXISTS(SELECT 1 FROM library_channel_subject_jobs j WHERE j.owner_user_id=d.owner_user_id AND j.state IN ('queued','running','waiting_for_provider') AND j.expires_ms>{now}) LIMIT 200)",now-CACHE_TTL_MS)],
        }
    }
}
pub fn decision_query(owner: i64, subject: &str, profile: &str, keys: &[(i64, String)]) -> String {
    let matches = keys
        .iter()
        .take(200)
        .map(|(id, metadata)| format!("(item_id={id} AND metadata_digest={})", text(metadata)))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("SELECT payload,last_used_ms FROM library_channel_subject_decisions WHERE owner_user_id={owner} AND subject_digest={} AND classifier_profile={} AND ({}) ORDER BY item_id LIMIT 200",text(subject),text(profile),if matches.is_empty(){"0"}else{&matches})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_channels::{evaluate_recipe, evaluate_subject_recipe, subject_candidates};
    fn candidate() -> ChannelCandidate {
        serde_json::from_value(serde_json::json!({"item_id":6336326135859793011_i64,"file_id":2,"file_size":10,"file_mtime":0,"duration_ms":60000,"library_id":1,"kind":"movie","title":"Stage special","overview":"A filmed stand-up performance before a live audience.","genres":["Comedy"],"tags":[],"year":2020,"show_id":null,"show_title":null,"season_number":null,"episode_number":null,"special":false,"explicitly_included":false})).expect("candidate")
    }
    #[test]
    fn subject_scope_and_manual_precedence_share_eligibility() {
        let c = candidate();
        let mut r = LibraryChannelRecipe {
            subject: Some("Stand-up performances".into()),
            match_all_in_scope: true,
            ..Default::default()
        };
        assert_eq!(subject_candidates(&r, [c.clone()]).expect("scope").len(), 1);
        assert!(evaluate_recipe(&r, [c.clone()])
            .expect("without inference")
            .is_empty());
        let d = SubjectDecision {
            verdict: Verdict::Match,
            reason: "Performance evidence".into(),
            evidence_field: "overview".into(),
            evidence_quote: "filmed stand-up performance".into(),
        };
        assert!(Metadata::from_candidate(&c).validates(&d));
        let decisions = BTreeMap::from([(c.item_id, d)]);
        assert_eq!(
            evaluate_subject_recipe(&r, [c.clone()], &decisions)
                .expect("match")
                .len(),
            1
        );
        r.include_item_ids.push(c.item_id);
        r.genres_any = vec!["Documentary".into()];
        assert_eq!(evaluate_recipe(&r, [c.clone()]).expect("manual").len(), 1);
        r.exclude_item_ids.push(c.item_id);
        assert!(subject_candidates(&r, [c.clone()])
            .expect("excluded")
            .is_empty());
        r.exclude_item_ids.clear();
        r.library_ids = vec![99];
        assert!(evaluate_recipe(&r, [c]).expect("scope wins").is_empty());
    }
    #[test]
    fn subject_unicode_clear_and_full_metadata_invalidation() {
        let recipe = LibraryChannelRecipe {
            subject: Some("  Cafe\u{301}  ".into()),
            ..Default::default()
        }
        .normalize()
        .expect("normalize");
        assert_eq!(recipe.subject.as_deref(), Some("Café"));
        assert!(LibraryChannelRecipe {
            subject: Some(" ".into()),
            ..Default::default()
        }
        .normalize()
        .expect("clear")
        .subject
        .is_none());
        assert!(LibraryChannelRecipe {
            subject: Some("x".repeat(501)),
            ..Default::default()
        }
        .normalize()
        .is_err());
        let mut c = candidate();
        let old = Metadata::digest(&c);
        c.show_overview = "changed inherited metadata".into();
        assert_ne!(old, Metadata::digest(&c));
        c.overview = "x".repeat(1300);
        let old = Metadata::digest(&c);
        c.overview.push('y');
        assert_ne!(old, Metadata::digest(&c));
        assert!(Metadata::from_candidate(&c).truncated);
    }
    #[test]
    fn subject_model_evidence_cannot_invent_quotes() {
        let metadata = Metadata::from_candidate(&candidate());
        assert!(
            !metadata.fields.contains_key("episode_identity"),
            "absent episode numbers cannot be admission evidence"
        );
        let mut d = SubjectDecision {
            verdict: Verdict::Match,
            reason: "A performance".into(),
            evidence_field: "overview".into(),
            evidence_quote: "invented quote".into(),
        };
        assert!(!metadata.validates(&d));
        d.evidence_quote.clear();
        assert!(!metadata.validates(&d));
        d.verdict = Verdict::Uncertain;
        assert!(metadata.validates(&d));
    }
}
