use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerReference {
    pub ticket_id: String,
    pub scope_hash: String,
}
impl BrokerReference {
    pub fn encode(&self) -> Result<String, StoreError> {
        if !valid_id(&self.ticket_id) || !valid_digest(&self.scope_hash) {
            return Err(invalid());
        }
        serde_json::to_string(self).map_err(|_| invalid())
    }
    pub fn decode(s: &str) -> Result<Self, StoreError> {
        if s.len() > 256 {
            return Err(invalid());
        }
        let v: Self = serde_json::from_str(s).map_err(|_| invalid())?;
        v.encode()?;
        Ok(v)
    }
}
#[derive(Clone, Debug)]
pub struct InvitationScope {
    pub consent: InvitationConsent,
    pub phone_generation: i64,
    pub permission_granted: bool,
    pub resident_active: bool,
    pub phone_digest: String,
    pub receiver_hash: String,
    pub grant_hash: String,
}
#[derive(Clone, Debug)]
pub struct InvitationEvent {
    pub id: String,
    pub user_id: i64,
    pub enrollment_id: String,
    pub phone_id: String,
    pub receiver_id: String,
    pub foreground_id: String,
    pub grant_id: String,
    pub phone_generation: i64,
    pub consent_generation: i64,
    pub transport_generation: i64,
    pub created_at: i64,
    pub expires_at: i64,
    pub phase: String,
    pub outcome: Option<String>,
    pub revision: i64,
}
#[derive(Clone)]
pub struct StartInvitationTransport {
    pub phone_id: String,
    pub receiver_id: String,
    pub user_id: i64,
    pub phone_hash: String,
    pub grant_id: String,
    pub grant_hash: String,
    pub login_digest: String,
    pub expected_phone_generation: i64,
    pub expected_consent_generation: i64,
    pub reference: BrokerReference,
    pub provider_available: bool,
    pub now: i64,
}
impl StartInvitationTransport {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !valid_id(&self.phone_id)
            || !valid_id(&self.receiver_id)
            || !valid_id(&self.grant_id)
            || !valid_digest(&self.phone_hash)
            || !valid_digest(&self.grant_hash)
            || !valid_digest(&self.login_digest)
            || self.user_id <= 0
            || self.expected_phone_generation <= 0
            || self.expected_consent_generation <= 0
            || self.expected_consent_generation >= 9_007_199_254_740_991
            || self.now < 0
        {
            return Err(invalid());
        }
        self.reference.encode()?;
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct InvitationRevocation {
    pub id: String,
    pub user_id: i64,
    pub enrollment_id: String,
    pub generation: i64,
    pub attempts: i64,
}
pub const BINDING: &str =
    "SELECT token_digest FROM invitation_phones WHERE id=$1 AND user_id=$2 AND secret_hash=$3";
pub const REVISION: &str = "SELECT last_revision FROM invitation_phones WHERE id=$1 AND user_id=$2";
pub const REBIND:&str="UPDATE invitation_phones SET token_digest=$1,generation=generation+1,permission_granted=0,resident_active=0 WHERE id=$2 AND user_id=$3 AND secret_hash=$4 AND generation=$5 AND generation<9007199254740991 AND EXISTS(SELECT 1 FROM tokens t WHERE t.token_hash=$1 AND t.user_id=$3 AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash))";
pub const LIST_CONSENTS:&str="SELECT id,phone_id,receiver_id,user_id,grant_id,enabled,transport,generation,transport_generation,broker_enrollment,broker_ticket,transport_status,transport_phone_generation FROM invitation_consents WHERE phone_id=$1 AND user_id=$2 AND receiver_id>$3 ORDER BY receiver_id LIMIT 21";
pub const SCOPE_COLUMNS:&str="SELECT c.id,c.phone_id,c.receiver_id,c.user_id,c.grant_id,c.enabled,c.transport,c.generation,c.transport_generation,c.broker_enrollment,c.broker_ticket,c.transport_status,c.transport_phone_generation,p.generation AS phone_generation,p.permission_granted,p.resident_active,p.token_digest,r.secret_hash AS receiver_hash,g.secret_hash AS grant_hash FROM invitation_consents c JOIN invitation_phones p ON p.id=c.phone_id AND p.user_id=c.user_id JOIN remote_receivers r ON r.id=c.receiver_id AND r.user_id=c.user_id AND r.revoked_at IS NULL JOIN remote_grants g ON g.id=c.grant_id AND g.receiver_id=r.id AND g.user_id=c.user_id AND g.secret_hash=c.grant_hash AND g.revoked_at IS NULL";
pub fn scope_query(single: bool) -> String {
    format!(
        "{SCOPE_COLUMNS} {}",
        if single {
            "WHERE c.phone_id=$1 AND c.receiver_id=$2 AND c.user_id=$3"
        } else {
            "WHERE c.receiver_id=$1 AND c.user_id=$2 ORDER BY c.phone_id LIMIT 20"
        }
    )
}
pub const EVENT_COLUMNS:&str="SELECT e.id,e.user_id,e.enrollment_id,e.phone_id,e.receiver_id,e.foreground_id,e.grant_id,e.phone_generation,e.consent_generation,e.transport_generation,e.created_at,e.expires_at,e.phase,e.outcome,e.revision FROM invitation_events e";
pub fn event_query(single: bool) -> String {
    format!("{EVENT_COLUMNS} JOIN invitation_phones p ON p.id=e.phone_id AND p.generation=e.phone_generation JOIN invitation_consents c ON c.id=e.enrollment_id AND c.enabled=1 AND c.generation=e.consent_generation AND c.transport_generation=e.transport_generation WHERE e.phone_id=$1 AND e.user_id=$2 AND e.expires_at>$3 AND e.phase IN ('admitted','attempted') {}",if single{"AND e.id=$4"}else{"AND e.revision>$4 ORDER BY e.revision LIMIT 16"})
}

pub fn validate_invitation_ids(ids: &[&str], user: i64) -> Result<(), StoreError> {
    if user <= 0 || ids.iter().any(|id| !valid_id(id)) {
        Err(invalid())
    } else {
        Ok(())
    }
}

pub const QUEUE_PHONE_CLEANUP:&str="WITH input AS (SELECT $1 AS id,$2 AS user_id,$3 AS created_at) INSERT INTO invitation_broker_revocations SELECT broker_ticket,user_id,CASE WHEN json_valid(broker_ticket) THEN json_extract(broker_ticket,'$.ticket_id') ELSE NULL END,transport_generation,$3,9223372036854775807,0 FROM invitation_consents WHERE phone_id=$1 AND user_id=$2 AND broker_ticket IS NOT NULL ON CONFLICT(id) DO NOTHING";
pub const QUEUE_SCOPE_CLEANUP:&str="WITH input AS (SELECT $1 AS id,$2 AS user_id,$3 AS created_at) INSERT INTO invitation_broker_revocations SELECT broker_ticket,user_id,CASE WHEN json_valid(broker_ticket) THEN json_extract(broker_ticket,'$.ticket_id') ELSE NULL END,transport_generation,$3,9223372036854775807,0 FROM invitation_consents WHERE id=$1 AND user_id=$2 AND broker_ticket IS NOT NULL ON CONFLICT(id) DO NOTHING";
pub const REVOKE_WORK:&str="SELECT id,user_id,enrollment_id,generation,attempts FROM invitation_broker_revocations WHERE attempts<9007199254740991 ORDER BY created_at,id LIMIT 16";

pub const CLEANUP_BUDGET:&str="SELECT (SELECT count(*) FROM (SELECT id FROM invitation_broker_revocations WHERE user_id=$1 UNION SELECT broker_ticket AS id FROM invitation_consents WHERE user_id=$1 AND broker_ticket IS NOT NULL)) AS budget";
pub const GLOBAL_CLEANUP_BUDGET:&str="SELECT (SELECT count(*) FROM (SELECT id FROM invitation_broker_revocations UNION SELECT broker_ticket AS id FROM invitation_consents WHERE broker_ticket IS NOT NULL)) AS budget";
pub const CLEANUP_INVALID:&str="SELECT count(*) AS invalid FROM invitation_consents WHERE user_id=$1 AND (NOT (CASE WHEN broker_ticket IS NULL THEN broker_enrollment IS NULL WHEN json_valid(broker_ticket) THEN coalesce((json_type(broker_ticket)='object' AND (SELECT count(*) FROM json_each(broker_ticket))=2 AND json_type(broker_ticket,'$.ticket_id')='text' AND json_type(broker_ticket,'$.scope_hash')='text' AND length(CAST(broker_ticket AS BLOB))<=256 AND length(json_extract(broker_ticket,'$.ticket_id'))=36 AND substr(json_extract(broker_ticket,'$.ticket_id'),9,1)='-' AND substr(json_extract(broker_ticket,'$.ticket_id'),14,1)='-' AND substr(json_extract(broker_ticket,'$.ticket_id'),19,1)='-' AND substr(json_extract(broker_ticket,'$.ticket_id'),24,1)='-' AND length(replace(json_extract(broker_ticket,'$.ticket_id'),'-',''))=32 AND replace(json_extract(broker_ticket,'$.ticket_id'),'-','') NOT GLOB '*[^0-9a-f]*' AND length(json_extract(broker_ticket,'$.scope_hash'))=64 AND json_extract(broker_ticket,'$.scope_hash') NOT GLOB '*[^0-9a-f]*' AND (broker_enrollment IS NULL OR broker_enrollment=json_extract(broker_ticket,'$.ticket_id'))),0) ELSE 0 END))";
pub const GLOBAL_CLEANUP_INVALID:&str="SELECT count(*) AS invalid FROM invitation_consents WHERE NOT (CASE WHEN broker_ticket IS NULL THEN broker_enrollment IS NULL WHEN json_valid(broker_ticket) THEN coalesce((json_type(broker_ticket)='object' AND (SELECT count(*) FROM json_each(broker_ticket))=2 AND json_type(broker_ticket,'$.ticket_id')='text' AND json_type(broker_ticket,'$.scope_hash')='text' AND length(CAST(broker_ticket AS BLOB))<=256 AND length(json_extract(broker_ticket,'$.ticket_id'))=36 AND substr(json_extract(broker_ticket,'$.ticket_id'),9,1)='-' AND substr(json_extract(broker_ticket,'$.ticket_id'),14,1)='-' AND substr(json_extract(broker_ticket,'$.ticket_id'),19,1)='-' AND substr(json_extract(broker_ticket,'$.ticket_id'),24,1)='-' AND length(replace(json_extract(broker_ticket,'$.ticket_id'),'-',''))=32 AND replace(json_extract(broker_ticket,'$.ticket_id'),'-','') NOT GLOB '*[^0-9a-f]*' AND length(json_extract(broker_ticket,'$.scope_hash'))=64 AND json_extract(broker_ticket,'$.scope_hash') NOT GLOB '*[^0-9a-f]*' AND (broker_enrollment IS NULL OR broker_enrollment=json_extract(broker_ticket,'$.ticket_id'))),0) ELSE 0 END)";
pub const CLEAR_QUEUED_REFERENCE:&str="UPDATE invitation_consents SET broker_ticket=NULL,broker_enrollment=NULL,transport_status='unavailable',transport_phone_generation=0 WHERE id=$1 AND user_id=$2 AND broker_ticket IS NOT NULL AND EXISTS(SELECT 1 FROM invitation_broker_revocations v WHERE v.id=invitation_consents.broker_ticket)";
pub const START_INPUT:&str="WITH input AS (SELECT $1 AS phone_id,$2 AS receiver_id,$3 AS user_id,$4 AS phone_hash,$5 AS grant_id,$6 AS grant_hash,$7 AS phone_generation,$8 AS consent_generation,$9 AS reference,$10 AS status,$11 AS login_digest) ";
pub const START_AUTH:&str="JOIN invitation_phones p ON p.id=c.phone_id AND p.user_id=c.user_id AND p.secret_hash=i.phone_hash AND p.generation=i.phone_generation AND p.token_digest=i.login_digest JOIN tokens t ON t.token_hash=p.token_digest AND t.user_id=c.user_id JOIN remote_receivers r ON r.id=c.receiver_id AND r.user_id=c.user_id AND r.revoked_at IS NULL JOIN remote_grants g ON g.id=c.grant_id AND g.receiver_id=r.id AND g.user_id=c.user_id AND g.secret_hash=i.grant_hash AND g.revoked_at IS NULL WHERE c.phone_id=i.phone_id AND c.receiver_id=i.receiver_id AND c.user_id=i.user_id AND c.grant_id=i.grant_id AND c.enabled=1 AND c.transport IN ('apns','fcm') AND c.generation=i.consent_generation AND c.generation<9007199254740991 AND c.transport_generation<9007199254740991 AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash)";
pub fn start_cleanup_query() -> String {
    format!("{START_INPUT} INSERT INTO invitation_broker_revocations SELECT c.broker_ticket,c.user_id,CASE WHEN json_valid(c.broker_ticket) THEN json_extract(c.broker_ticket,'$.ticket_id') ELSE NULL END,c.transport_generation,0,9223372036854775807,0 FROM input i JOIN invitation_consents c {START_AUTH} AND c.broker_ticket IS NOT NULL ON CONFLICT(id) DO NOTHING")
}
pub fn start_query() -> String {
    format!("{START_INPUT} UPDATE invitation_consents SET generation=generation+1,transport_generation=transport_generation+1,transport_phone_generation=(SELECT phone_generation FROM input),broker_ticket=CASE WHEN (SELECT status FROM input)='pending' THEN (SELECT reference FROM input) ELSE NULL END,broker_enrollment=NULL,transport_status=(SELECT status FROM input) WHERE id IN (SELECT c.id FROM input i JOIN invitation_consents c {START_AUTH}) AND (broker_ticket IS NULL OR EXISTS(SELECT 1 FROM invitation_broker_revocations v WHERE v.id=invitation_consents.broker_ticket)) AND ((SELECT status FROM input)<>'pending' OR ((SELECT count(*) FROM invitation_broker_revocations WHERE user_id=invitation_consents.user_id)+(SELECT count(*) FROM invitation_consents WHERE user_id=invitation_consents.user_id AND broker_ticket IS NOT NULL AND id<>invitation_consents.id))<100000 AND ((SELECT count(*) FROM invitation_broker_revocations)+(SELECT count(*) FROM invitation_consents WHERE broker_ticket IS NOT NULL AND id<>invitation_consents.id))<100000)")
}
#[derive(Clone)]
pub struct ConfirmInvitationTransport {
    pub phone_id: String,
    pub receiver_id: String,
    pub user_id: i64,
    pub phone_hash: String,
    pub grant_id: String,
    pub grant_hash: String,
    pub login_digest: String,
    pub phone_generation: i64,
    pub consent_generation: i64,
    pub transport_generation: i64,
    pub reference: BrokerReference,
}
impl ConfirmInvitationTransport {
    pub fn validate(&self) -> Result<(), StoreError> {
        validate_invitation_ids(
            &[&self.phone_id, &self.receiver_id, &self.grant_id],
            self.user_id,
        )?;
        if !valid_digest(&self.phone_hash)
            || !valid_digest(&self.grant_hash)
            || !valid_digest(&self.login_digest)
            || self.phone_generation <= 0
            || self.consent_generation <= 0
            || self.transport_generation <= 0
        {
            return Err(invalid());
        }
        self.reference.encode()?;
        Ok(())
    }
}
pub const CONFIRM:&str="WITH input AS (SELECT $1 AS phone_id,$2 AS receiver_id,$3 AS user_id,$4 AS phone_hash,$5 AS grant_id,$6 AS grant_hash,$7 AS phone_generation,$8 AS consent_generation,$9 AS transport_generation,$10 AS login_digest,$11 AS reference,$12 AS enrollment_id) UPDATE invitation_consents SET broker_enrollment=(SELECT enrollment_id FROM input),transport_status='ready' WHERE id IN (SELECT c.id FROM input i JOIN invitation_consents c JOIN invitation_phones p ON p.id=c.phone_id AND p.user_id=c.user_id AND p.secret_hash=i.phone_hash AND p.generation=i.phone_generation AND p.token_digest=i.login_digest JOIN tokens t ON t.token_hash=p.token_digest AND t.user_id=c.user_id JOIN remote_receivers r ON r.id=c.receiver_id AND r.user_id=c.user_id AND r.revoked_at IS NULL JOIN remote_grants g ON g.id=c.grant_id AND g.receiver_id=r.id AND g.user_id=c.user_id AND g.secret_hash=i.grant_hash AND g.revoked_at IS NULL WHERE c.phone_id=i.phone_id AND c.receiver_id=i.receiver_id AND c.user_id=i.user_id AND c.grant_id=i.grant_id AND c.enabled=1 AND c.generation=i.consent_generation AND c.transport_generation=i.transport_generation AND c.transport_phone_generation=i.phone_generation AND c.broker_ticket=i.reference AND c.transport_status='pending' AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash))";
#[derive(Clone, Copy)]
pub enum InvitationDispatchOutcome {
    ResidentReady,
    Accepted,
    Denied,
    Unknown,
    Revoked,
    Expired,
}
impl InvitationDispatchOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResidentReady => "resident_ready",
            Self::Accepted => "accepted",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}
pub fn attempt_query() -> String {
    let projection = &ADMIT[..ADMIT
        .find("INSERT INTO invitation_events")
        .expect("constant admission projection")];
    let joins = &ADMIT[ADMIT
        .find("JOIN invitation_consents")
        .expect("constant admission joins")
        ..ADMIT
            .find("AND NOT EXISTS(SELECT 1 FROM invitation_cooldowns")
            .expect("constant cooldown boundary")];
    format!("{projection} UPDATE invitation_events SET phase='attempted' WHERE id=(SELECT id FROM input) AND phase='admitted' AND expires_at>(SELECT admitted_at FROM input) AND user_id=(SELECT user_id FROM input) AND enrollment_id=(SELECT enrollment_id FROM input) AND foreground_id=(SELECT foreground_id FROM input) AND phone_generation=(SELECT phone_generation FROM input) AND consent_generation=(SELECT consent_generation FROM input) AND transport_generation=(SELECT transport_generation FROM input) AND EXISTS(SELECT 1 FROM input i {joins} AND c.phone_id=invitation_events.phone_id AND c.receiver_id=invitation_events.receiver_id AND c.grant_id=invitation_events.grant_id)")
}
pub const FINISH:&str="WITH input AS (SELECT $1 AS id,$2 AS outcome) UPDATE invitation_events SET phase=CASE WHEN phase='admitted' THEN 'cancelled' ELSE phase END,outcome=(SELECT outcome FROM input) WHERE id=(SELECT id FROM input) AND outcome IS NULL";
