//! Durable invitation consent and admission. Notification identity grants no control.
use crate::{auth::TokenIdlePolicy, error::StoreError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SCHEMA_V1: &str = include_str!("invitations/schema_v1.sql");
pub const SCHEMA: &str = include_str!("invitations/schema_v2.sql");
pub const MIGRATION_V2: &str = include_str!("invitations/migration_v2.sql");
pub const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name IN ('invitation_schema','invitation_phones','invitation_phones_user','invitation_consents','invitation_consents_user','invitation_events','invitation_events_revision','invitation_events_user','invitation_events_pending','invitation_cooldowns','invitation_broker_revocations') ORDER BY name";
pub fn objects() -> Vec<(&'static str, &'static str)> {
    schema_objects(SCHEMA)
}
pub fn schema_objects(schema: &'static str) -> Vec<(&'static str, &'static str)> {
    schema
        .split(';')
        .filter(|s| s.starts_with("CREATE "))
        .map(|s| {
            (
                s.split_whitespace()
                    .nth(if s.starts_with("CREATE UNIQUE") { 3 } else { 2 })
                    .expect("constant schema object"),
                s,
            )
        })
        .collect()
}
pub fn verify_shape(rows: &[(String, String)]) -> Result<bool, StoreError> {
    verify_schema_shape(rows, SCHEMA)
}
pub fn verify_schema_shape(
    rows: &[(String, String)],
    schema: &'static str,
) -> Result<bool, StoreError> {
    if rows.is_empty() {
        return Ok(false);
    }
    let normalize = |s: &str| {
        let mut chars = s.chars().peekable();
        let mut tokens = Vec::new();
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                continue;
            }
            let mut token = c.to_string();
            if c == '\'' || c == '"' {
                while let Some(next) = chars.next() {
                    token.push(next);
                    if next == c {
                        if chars.peek() == Some(&c) {
                            token.push(chars.next().expect("peeked quote"));
                        } else {
                            break;
                        }
                    }
                }
            } else if c.is_alphanumeric() || c == '_' {
                while chars
                    .peek()
                    .is_some_and(|n| n.is_alphanumeric() || *n == '_')
                {
                    token.push(chars.next().expect("peeked identifier"));
                }
            }
            tokens.push(token);
        }
        tokens
    };
    let expected = schema_objects(schema);
    if rows.len() != expected.len()
        || expected.iter().any(|(name, sql)| {
            !rows
                .iter()
                .any(|(n, s)| n == name && normalize(s) == normalize(sql))
        })
    {
        return Err(StoreError::Migration(
            "partial or incompatible invitation schema".into(),
        ));
    }
    Ok(true)
}
fn invalid() -> StoreError {
    StoreError::Identity("invalid invitation authority".into())
}
pub fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn valid_id(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s)
}
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && !s.chars().any(char::is_control)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvitationTransport {
    Apns,
    Fcm,
    AndroidResident,
}
impl InvitationTransport {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Apns => "apns",
            Self::Fcm => "fcm",
            Self::AndroidResident => "android_resident",
        }
    }
}
#[derive(Clone, Debug)]
pub struct InvitationPhone {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub platform: String,
    pub generation: i64,
    pub created_at: i64,
    pub permission_granted: bool,
    pub resident_active: bool,
}
#[derive(Clone)]
pub struct NewInvitationPhone {
    pub phone: InvitationPhone,
    pub secret_hash: String,
    pub token_digest: String,
}
impl NewInvitationPhone {
    pub fn validate(&self) -> Result<(), StoreError> {
        let p = &self.phone;
        if !valid_id(&p.id)
            || p.user_id <= 0
            || !label(&p.name)
            || !matches!(p.platform.as_str(), "apple" | "android")
            || p.permission_granted
            || p.resident_active
            || p.generation != 1
            || !valid_digest(&self.secret_hash)
            || !valid_digest(&self.token_digest)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct InvitationConsent {
    pub id: String,
    pub phone_id: String,
    pub receiver_id: String,
    pub user_id: i64,
    pub grant_id: Option<String>,
    pub enabled: bool,
    pub transport: String,
    pub generation: i64,
    pub transport_generation: i64,
    pub broker_enrollment: Option<String>,
    pub broker_ticket: Option<String>,
    pub transport_status: String,
    pub transport_phone_generation: i64,
}
#[derive(Clone)]
pub struct SaveInvitationConsent {
    pub id: String,
    pub phone_id: String,
    pub receiver_id: String,
    pub user_id: i64,
    pub phone_hash: String,
    pub expected_generation: i64,
    pub expected_phone_generation: i64,
    pub enable: Option<(String, String, InvitationTransport)>,
}
impl SaveInvitationConsent {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !valid_id(&self.id)
            || !valid_id(&self.phone_id)
            || !valid_id(&self.receiver_id)
            || self.user_id <= 0
            || !valid_digest(&self.phone_hash)
            || self.expected_phone_generation <= 0
            || self.expected_phone_generation > 9_007_199_254_740_991
            || self.expected_generation < 0
            || self.expected_generation >= 9_007_199_254_740_991
        {
            return Err(invalid());
        }
        if let Some((id, hash, _)) = &self.enable {
            if !valid_id(id) || !valid_digest(hash) {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
/// Consistent Native login verdict without an activity refresh. Internal only.
#[derive(Clone, Debug)]
pub struct InvitationLogin {
    pub user_id: i64,
    pub digest: String,
    pub last_seen_at: i64,
    pub expiry_enabled: Option<String>,
    pub expiry_days: Option<String>,
    pub expiry_since: Option<String>,
}
impl InvitationLogin {
    pub fn live(&self, now: i64) -> bool {
        TokenIdlePolicy::from_settings(
            self.expiry_enabled.as_deref(),
            self.expiry_days.as_deref(),
            self.expiry_since.as_deref(),
        )
        .is_none_or(|p| !p.is_expired(self.last_seen_at, now))
    }
}
#[derive(Clone)]
pub struct AdmitInvitation {
    pub id: String,
    pub enrollment_id: String,
    pub foreground_id: String,
    pub receiver_hash: String,
    pub phone_generation: i64,
    pub consent_generation: i64,
    pub transport_generation: i64,
    pub phone_login: InvitationLogin,
    pub receiver_login: InvitationLogin,
    pub now: i64,
}
impl AdmitInvitation {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !valid_id(&self.id)
            || !valid_id(&self.enrollment_id)
            || !valid_id(&self.foreground_id)
            || !valid_digest(&self.receiver_hash)
            || !valid_digest(&self.phone_login.digest)
            || !valid_digest(&self.receiver_login.digest)
            || self.phone_login.user_id != self.receiver_login.user_id
            || self.phone_generation <= 0
            || self.consent_generation <= 0
            || self.transport_generation < 0
            || self.now < 0
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvitationAdmission {
    Admitted,
    Refused,
}

pub const CREATE_PHONE:&str="INSERT INTO invitation_phones(id,user_id,name,platform,secret_hash,token_digest,generation,created_at) SELECT $1,$2,$3,$4,$5,$6,1,$7 WHERE EXISTS(SELECT 1 FROM users WHERE id=$2) AND EXISTS(SELECT 1 FROM tokens t WHERE token_hash=$6 AND user_id=$2 AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash)) AND (SELECT count(*) FROM invitation_phones WHERE user_id=$2)<20 ON CONFLICT(id) DO NOTHING";
pub const PHONE_METADATA:&str="SELECT id,user_id,name,platform,generation,created_at,permission_granted,resident_active FROM invitation_phones WHERE id=$1 AND user_id=$2";
pub const PHONE_AUTHORITY:&str="SELECT id,user_id,name,platform,generation,created_at,permission_granted,resident_active FROM invitation_phones WHERE id=$1 AND user_id=$2 AND secret_hash=$3";
pub const LIST_PHONES:&str="SELECT id,user_id,name,platform,generation,created_at,permission_granted,resident_active FROM invitation_phones WHERE user_id=$1 AND id>$2 ORDER BY id LIMIT 20";
pub const AVAILABILITY: &str="UPDATE invitation_phones SET permission_granted=$1,resident_active=$2,generation=generation+1 WHERE id=$3 AND user_id=$4 AND secret_hash=$5 AND generation=$6 AND generation<9007199254740991 AND ($2=0 OR platform='android')";
pub const DELETE_PHONE: &str = "DELETE FROM invitation_phones WHERE id=$1 AND user_id=$2";
pub const LOGIN:&str="SELECT t.user_id,t.last_seen_at,(SELECT value FROM settings WHERE key='auth.token_expiry_enabled') AS expiry_enabled,(SELECT value FROM settings WHERE key='auth.token_idle_days') AS expiry_days,(SELECT value FROM settings WHERE key='auth.token_expiry_since') AS expiry_since FROM tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash)";
pub const CONSENT_ENABLE:&str="WITH input AS (SELECT $1 AS id,$2 AS phone_id,$3 AS receiver_id,$4 AS user_id,$5 AS phone_hash,$6 AS expected,$7 AS grant_id,$8 AS grant_hash,$9 AS transport,$10 AS phone_generation) INSERT INTO invitation_consents(id,phone_id,receiver_id,user_id,grant_id,grant_hash,enabled,transport,generation) SELECT i.id,i.phone_id,i.receiver_id,i.user_id,i.grant_id,i.grant_hash,1,i.transport,1 FROM input i JOIN invitation_phones p ON p.id=i.phone_id AND p.user_id=i.user_id AND p.secret_hash=i.phone_hash AND p.generation=i.phone_generation JOIN remote_receivers r ON r.id=i.receiver_id AND r.user_id=i.user_id AND r.revoked_at IS NULL JOIN remote_grants g ON g.id=i.grant_id AND g.receiver_id=r.id AND g.user_id=i.user_id AND g.secret_hash=i.grant_hash AND g.revoked_at IS NULL WHERE ((p.platform='apple' AND i.transport='apns') OR (p.platform='android' AND i.transport IN ('fcm','android_resident'))) AND ((i.expected=0 AND NOT EXISTS(SELECT 1 FROM invitation_consents c WHERE c.phone_id=p.id AND c.receiver_id=r.id) AND (SELECT count(*) FROM invitation_consents WHERE user_id=i.user_id)<160) OR EXISTS(SELECT 1 FROM invitation_consents c WHERE c.phone_id=p.id AND c.receiver_id=r.id AND c.generation=i.expected)) ON CONFLICT(phone_id,receiver_id) DO UPDATE SET grant_id=excluded.grant_id,grant_hash=excluded.grant_hash,enabled=1,transport=excluded.transport,transport_status='unavailable',transport_phone_generation=0,generation=invitation_consents.generation+1 WHERE invitation_consents.generation=(SELECT expected FROM input)";
// OFF deliberately requires no live receiver, grant, provider, feature or login binding.
pub const CONSENT_DISABLE:&str="WITH input AS (SELECT $1 AS phone_id,$2 AS receiver_id,$3 AS user_id,$4 AS phone_hash,$5 AS expected,$6 AS phone_generation) UPDATE invitation_consents SET enabled=0,generation=generation+1 WHERE phone_id=(SELECT phone_id FROM input) AND receiver_id=(SELECT receiver_id FROM input) AND user_id=(SELECT user_id FROM input) AND generation=(SELECT expected FROM input) AND EXISTS(SELECT 1 FROM invitation_phones p,input i WHERE p.id=i.phone_id AND p.user_id=i.user_id AND p.secret_hash=i.phone_hash AND p.generation=i.phone_generation)";
pub const CONSENT:&str="SELECT id,phone_id,receiver_id,user_id,grant_id,enabled,transport,generation,transport_generation,broker_enrollment,broker_ticket,transport_status,transport_phone_generation FROM invitation_consents WHERE phone_id=$1 AND receiver_id=$2 AND user_id=$3";

// Explicit input projection keeps Hiqlite's first-appearance binding order exact.
pub const ADMIT:&str=concat!(
"WITH input AS (SELECT $1 AS id,$2 AS enrollment_id,$3 AS foreground_id,$4 AS receiver_hash,$5 AS phone_generation,$6 AS consent_generation,$7 AS transport_generation,$8 AS user_id,$9 AS phone_digest,$10 AS phone_last_seen,$11 AS receiver_digest,$12 AS receiver_last_seen,$13 AS expiry_enabled,$14 AS expiry_days,$15 AS expiry_since,$16 AS admitted_at) ",
"INSERT INTO invitation_events(id,user_id,enrollment_id,phone_id,receiver_id,foreground_id,grant_id,phone_generation,consent_generation,transport_generation,created_at,expires_at,phase,revision) ",
"SELECT i.id,i.user_id,c.id,p.id,r.id,i.foreground_id,g.id,p.generation,c.generation,c.transport_generation,i.admitted_at,i.admitted_at+120,'admitted',p.last_revision+1 FROM input i ",
"JOIN invitation_consents c ON c.id=i.enrollment_id AND c.user_id=i.user_id AND c.enabled=1 AND c.generation=i.consent_generation AND c.transport_generation=i.transport_generation ",
"JOIN invitation_phones p ON p.id=c.phone_id AND p.user_id=i.user_id AND p.generation=i.phone_generation AND p.token_digest=i.phone_digest ",
"JOIN remote_receivers r ON r.id=c.receiver_id AND r.user_id=i.user_id AND r.secret_hash=i.receiver_hash AND r.revoked_at IS NULL ",
"JOIN remote_grants g ON g.id=c.grant_id AND g.receiver_id=r.id AND g.user_id=i.user_id AND g.secret_hash=c.grant_hash AND g.revoked_at IS NULL ",
"JOIN tokens pt ON pt.token_hash=i.phone_digest AND pt.user_id=i.user_id AND pt.last_seen_at>=i.phone_last_seen ",
"JOIN tokens rt ON rt.token_hash=i.receiver_digest AND rt.user_id=i.user_id AND rt.last_seen_at>=i.receiver_last_seen ",
"WHERE NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash IN (pt.token_hash,rt.token_hash)) ",
"AND (SELECT value FROM settings WHERE key='auth.token_expiry_enabled') IS i.expiry_enabled AND (SELECT value FROM settings WHERE key='auth.token_idle_days') IS i.expiry_days AND (SELECT value FROM settings WHERE key='auth.token_expiry_since') IS i.expiry_since ",
"AND (SELECT value FROM settings WHERE key='cinema.remote_control')='1' AND (SELECT value FROM settings WHERE key='cinema.remote_invitations')='1' ",
"AND p.permission_granted=1 AND ((c.transport='android_resident' AND p.resident_active=1) OR (c.transport IN ('apns','fcm') AND c.transport_status='ready' AND c.transport_phone_generation=p.generation AND c.broker_enrollment IS NOT NULL)) ",
"AND NOT EXISTS(SELECT 1 FROM invitation_cooldowns d WHERE d.receiver_id=r.id AND d.phone_id=p.id AND i.admitted_at-d.last_admitted_at<1800) ",
"AND p.last_revision<9007199254740991 AND (SELECT count(*) FROM invitation_events WHERE user_id=i.user_id)<100000 ON CONFLICT DO NOTHING"
);
pub const ADVANCE_REVISION: &str = "UPDATE invitation_phones SET last_revision=max(last_revision,(SELECT revision FROM invitation_events WHERE id=$1)) WHERE id=(SELECT phone_id FROM invitation_events WHERE id=$1)";
pub const COOLDOWN:&str="INSERT INTO invitation_cooldowns SELECT receiver_id,phone_id,created_at FROM invitation_events WHERE id=$1 ON CONFLICT(receiver_id,phone_id) DO UPDATE SET last_admitted_at=max(invitation_cooldowns.last_admitted_at,excluded.last_admitted_at)";
#[async_trait]
pub trait InvitationStore: Send + Sync {
    async fn create_invitation_phone(&self, new: NewInvitationPhone) -> Result<bool, StoreError>;
    async fn invitation_phone(
        &self,
        id: &str,
        user: i64,
    ) -> Result<Option<InvitationPhone>, StoreError>;
    async fn invitation_phone_authority(
        &self,
        id: &str,
        user: i64,
        hash: &str,
    ) -> Result<Option<InvitationPhone>, StoreError>;
    async fn invitation_phones(
        &self,
        user: i64,
        after: &str,
    ) -> Result<Vec<InvitationPhone>, StoreError>;
    async fn set_invitation_availability(
        &self,
        id: &str,
        user: i64,
        hash: &str,
        expected: i64,
        permission: bool,
        resident: bool,
    ) -> Result<bool, StoreError>;
    async fn revoke_invitation_phone(&self, id: &str, user: i64) -> Result<(), StoreError>;
    async fn save_invitation_consent(
        &self,
        request: SaveInvitationConsent,
    ) -> Result<bool, StoreError>;
    async fn invitation_consent(
        &self,
        phone: &str,
        receiver: &str,
        user: i64,
    ) -> Result<Option<InvitationConsent>, StoreError>;
    async fn invitation_login(
        &self,
        digest: &str,
        now: i64,
    ) -> Result<Option<InvitationLogin>, StoreError>;
    async fn admit_invitation(
        &self,
        request: AdmitInvitation,
    ) -> Result<InvitationAdmission, StoreError>;
}
pub fn validate_admission(r: &AdmitInvitation) -> Result<bool, StoreError> {
    r.validate()?;
    Ok(r.phone_login.live(r.now)
        && r.receiver_login.live(r.now)
        && r.phone_login.expiry_enabled == r.receiver_login.expiry_enabled
        && r.phone_login.expiry_days == r.receiver_login.expiry_days
        && r.phone_login.expiry_since == r.receiver_login.expiry_since)
}
pub fn fence_restored_invitations(c: &rusqlite::Connection) -> Result<(), StoreError> {
    let rows = c
        .prepare(SHAPE_SQL)?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Ok(());
    }
    let expected_version = if verify_shape(&rows).is_ok() {
        2
    } else {
        verify_schema_shape(&rows, SCHEMA_V1)?;
        1
    };
    let version: i64 = c.query_row(
        "SELECT version FROM invitation_schema WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if version != expected_version {
        return Err(StoreError::Migration(
            "unsupported restored invitation version".into(),
        ));
    }
    let tx = c.unchecked_transaction()?;
    // Portable restore may deliberately open with foreign_keys=OFF.
    // Clear every capability-bearing child explicitly before its parent.
    tx.execute("DELETE FROM invitation_events", [])?;
    tx.execute("DELETE FROM invitation_cooldowns", [])?;
    tx.execute("DELETE FROM invitation_consents", [])?;
    tx.execute("DELETE FROM invitation_phones", [])?;
    tx.execute("DELETE FROM invitation_broker_revocations", [])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "invitations/tests.rs"]
mod tests;
