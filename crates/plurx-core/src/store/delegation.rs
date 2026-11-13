//! Optional scoped application login, using the same atomic SQL on SQLite
//! and Hiqlite. Source login activity is read, never refreshed by a grant.
use super::sharing::Backend;
use crate::error::StoreError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
pub(crate) const SCHEMA: &str = include_str!("delegation.sql");
pub(crate) const SHAPE_SQL:&str="SELECT name,sql FROM sqlite_master WHERE name GLOB 'delegation*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM delegation_schema WHERE singleton=1 AND version=1";
const DEPENDENCIES: &str = "CREATE TABLE users(id INTEGER PRIMARY KEY);";
fn invalid() -> StoreError {
    StoreError::Migration("invalid delegated login operation".into())
}
fn now() -> Result<i64, StoreError> {
    Ok(super::sharing::wall_clock_ms()? / 1000)
}
fn decode<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, StoreError> {
    serde_json::from_str(s).map_err(|_| invalid())
}
fn policy_sql() -> String {
    format!("json_object('enabled',(SELECT value FROM settings WHERE key='{}'),'days',(SELECT value FROM settings WHERE key='{}'),'since',(SELECT value FROM settings WHERE key='{}'))",super::keys::AUTH_TOKEN_EXPIRY_ENABLED,super::keys::AUTH_TOKEN_IDLE_DAYS,super::keys::AUTH_TOKEN_EXPIRY_SINCE)
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct DelegationClient {
    pub id: String,
    pub name: String,
    pub redirect: String,
    pub generation: i64,
}
#[derive(Clone, Deserialize, Debug)]
pub struct DelegationIdentity {
    pub subject: String,
    pub display_name: String,
    pub expires_at: i64,
}
#[derive(Clone)]
pub struct DelegationCode {
    pub code_hash: String,
    pub client_id: String,
    pub user_id: i64,
    pub subject: String,
    pub source_hash: String,
    pub challenge: String,
}
#[derive(Clone)]
pub struct DelegationRedemption {
    pub code_hash: String,
    pub client_id: String,
    pub secret_hash: String,
    pub redirect: String,
    pub challenge: String,
    pub grant_hash: String,
    pub claim: String,
}
#[derive(Serialize, Deserialize)]
struct Policy {
    enabled: Option<String>,
    days: Option<String>,
    since: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct SourceProof {
    hash: String,
    user_id: i64,
    subject: String,
    last_seen: i64,
    policy: Policy,
    observed: i64,
    expires: i64,
}
async fn source_proof_scoped<T: Backend>(
    s: &T,
    hash: &str,
    user: i64,
    subject: &str,
    grant: Option<(&str, &str, &str)>,
) -> Result<Option<String>, StoreError> {
    let clock = now()?;
    let mut sql=format!("SELECT json_object('hash',t.token_hash,'user_id',u.id,'subject',j.wire_id,'last_seen',t.last_seen_at,'policy',{},'observed',$4,'expires',$4+2592000) AS payload FROM tokens t JOIN users u ON u.id=t.user_id JOIN jellyfin_entity_ids j ON j.entity_kind='user' AND j.native_id=u.id AND j.retired=0 WHERE t.token_hash=$1 AND u.id=$2 AND j.wire_id=$3 AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash)",policy_sql());
    let mut values = vec![
        hash.to_owned().into(),
        user.into(),
        subject.to_owned().into(),
        clock.into(),
    ];
    if let Some((client, secret, grant_hash)) = grant {
        sql.push_str(" AND EXISTS(SELECT 1 FROM delegation_grants g JOIN delegation_clients c ON c.id=g.client_id AND c.generation=g.client_generation WHERE g.grant_hash=$5 AND c.id=$6 AND c.secret_hash=$7 AND c.active=1 AND g.expires_at>$4 AND g.source_hash=t.token_hash AND g.user_id=u.id AND g.subject=j.wire_id)");
        values.extend([
            grant_hash.to_owned().into(),
            client.to_owned().into(),
            secret.to_owned().into(),
        ]);
    }
    let rows = s.sharing_read(&sql, values).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let mut proof: SourceProof = decode(row)?;
    if let Some(policy) = crate::auth::TokenIdlePolicy::from_settings(
        proof.policy.enabled.as_deref(),
        proof.policy.days.as_deref(),
        proof.policy.since.as_deref(),
    ) {
        proof.expires = proof.expires.min(policy.expires_at(proof.last_seen));
    }
    if proof.expires <= clock {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&proof).map_err(|_| invalid())?))
}
async fn source_proof<T: Backend>(
    s: &T,
    hash: &str,
    user: i64,
    subject: &str,
) -> Result<Option<String>, StoreError> {
    source_proof_scoped(s, hash, user, subject, None).await
}
fn source_fence(proof: &str, clock: &str) -> String {
    format!("EXISTS(SELECT 1 FROM tokens t JOIN users u ON u.id=t.user_id JOIN jellyfin_entity_ids j ON j.entity_kind='user' AND j.native_id=u.id AND j.retired=0 WHERE t.token_hash=json_extract({proof},'$.hash') AND u.id=json_extract({proof},'$.user_id') AND j.wire_id=json_extract({proof},'$.subject') AND t.last_seen_at>=json_extract({proof},'$.last_seen') AND {}=json_extract({proof},'$.policy') AND NOT EXISTS(SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash=t.token_hash)) AND {clock}>=json_extract({proof},'$.observed') AND {clock}-json_extract({proof},'$.observed')<=5 AND {clock}<json_extract({proof},'$.expires')",policy_sql())
}
const CLIENT_JSON: &str =
    "json_object('id',id,'name',name,'redirect',redirect,'generation',generation)";
#[async_trait]
pub trait DelegationStore: Send + Sync {
    async fn delegation_register(
        &self,
        client: DelegationClient,
        secret_hash: String,
    ) -> Result<(), StoreError>;
    async fn delegation_client_authorized(
        &self,
        id: &str,
        secret_hash: &str,
    ) -> Result<bool, StoreError>;
    async fn delegation_clients(&self) -> Result<Vec<DelegationClient>, StoreError>;
    async fn delegation_client(&self, id: &str) -> Result<Option<DelegationClient>, StoreError>;
    async fn delegation_disconnect(&self, id: &str) -> Result<(), StoreError>;
    async fn delegation_mint(&self, code: DelegationCode) -> Result<bool, StoreError>;
    async fn delegation_redeem(
        &self,
        r: DelegationRedemption,
    ) -> Result<Option<DelegationIdentity>, StoreError>;
    async fn delegation_introspect(
        &self,
        client: &str,
        secret_hash: &str,
        grant_hash: &str,
    ) -> Result<Option<DelegationIdentity>, StoreError>;
    async fn delegation_revoke(
        &self,
        client: &str,
        secret_hash: &str,
        grant_hash: &str,
    ) -> Result<(), StoreError>;
}
#[async_trait]
impl<T: Backend> DelegationStore for T {
    async fn delegation_register(
        &self,
        c: DelegationClient,
        secret: String,
    ) -> Result<(), StoreError> {
        if c.id.len() > 80 || c.name.len() > 100 || c.redirect.len() > 2048 || secret.len() != 64 {
            return Err(invalid());
        }
        self.sharing_txn(vec![(
            "INSERT INTO delegation_clients(id,name,secret_hash,redirect) VALUES($1,$2,$3,$4)"
                .into(),
            vec![c.id.into(), c.name.into(), secret.into(), c.redirect.into()],
        )])
        .await?;
        Ok(())
    }
    async fn delegation_client_authorized(
        &self,
        id: &str,
        secret_hash: &str,
    ) -> Result<bool, StoreError> {
        Ok(!self.sharing_read("SELECT '{}' AS payload FROM delegation_clients WHERE id=$1 AND secret_hash=$2 AND active=1",vec![id.to_owned().into(),secret_hash.to_owned().into()]).await?.is_empty())
    }
    async fn delegation_clients(&self) -> Result<Vec<DelegationClient>, StoreError> {
        self.sharing_read(&format!("SELECT {CLIENT_JSON} AS payload FROM delegation_clients WHERE active=1 ORDER BY id LIMIT 100"),vec![]).await?.into_iter().map(|r|decode(&r)).collect()
    }
    async fn delegation_client(&self, id: &str) -> Result<Option<DelegationClient>, StoreError> {
        self.sharing_read(
            &format!(
                "SELECT {CLIENT_JSON} AS payload FROM delegation_clients WHERE id=$1 AND active=1"
            ),
            vec![id.to_owned().into()],
        )
        .await?
        .first()
        .map(|r| decode(r))
        .transpose()
    }
    async fn delegation_disconnect(&self, id: &str) -> Result<(), StoreError> {
        self.sharing_txn(vec![(
            "UPDATE delegation_clients SET active=0,generation=generation+1 WHERE id=$1".into(),
            vec![id.to_owned().into()],
        )])
        .await?;
        Ok(())
    }
    async fn delegation_mint(&self, c: DelegationCode) -> Result<bool, StoreError> {
        let Some(proof) = source_proof(self, &c.source_hash, c.user_id, &c.subject).await? else {
            return Ok(false);
        };
        let clock = now()?;
        let fence = source_fence("$9", "$10");
        let sql=format!("INSERT INTO delegation_codes(code_hash,client_id,client_generation,user_id,subject,source_hash,challenge,redirect,expires_at) SELECT $1,id,generation,$2,$3,$4,$5,redirect,$6 FROM delegation_clients WHERE id=$7 AND active=1 AND $8='household-requests' AND {fence}");
        let result = self
            .sharing_txn(vec![("DELETE FROM delegation_grants WHERE grant_hash IN(SELECT grant_hash FROM delegation_grants WHERE expires_at<=$1 LIMIT 64)".into(),vec![clock.into()]),("DELETE FROM delegation_codes WHERE code_hash IN(SELECT code_hash FROM delegation_codes WHERE expires_at<=$1 LIMIT 64)".into(),vec![clock.into()]),(
                sql,
                vec![
                    c.code_hash.into(),
                    c.user_id.into(),
                    c.subject.into(),
                    c.source_hash.into(),
                    c.challenge.into(),
                    (clock + 120).into(),
                    c.client_id.into(),
                    "household-requests".to_owned().into(),
                    proof.into(),
                    clock.into(),
                ],
            )])
            .await?;
        Ok(result.last() == Some(&1))
    }
    async fn delegation_redeem(
        &self,
        r: DelegationRedemption,
    ) -> Result<Option<DelegationIdentity>, StoreError> {
        let rows=self.sharing_read("SELECT json_object('source',d.source_hash,'user',d.user_id,'subject',d.subject) AS payload FROM delegation_codes d JOIN delegation_clients c ON c.id=d.client_id AND c.generation=d.client_generation WHERE d.code_hash=$1 AND c.id=$2 AND c.secret_hash=$3 AND c.active=1 AND d.redirect=$4 AND d.challenge=$5 AND d.claim='' AND d.expires_at>$6",vec![r.code_hash.clone().into(),r.client_id.clone().into(),r.secret_hash.clone().into(),r.redirect.clone().into(),r.challenge.clone().into(),now()?.into()]).await?;
        #[derive(Deserialize)]
        struct Source {
            source: String,
            user: i64,
            subject: String,
        }
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let source: Source = decode(row)?;
        let Some(proof) = source_proof(self, &source.source, source.user, &source.subject).await?
        else {
            return Ok(None);
        };
        let clock = now()?;
        let fence = source_fence("$8", "$7");
        let claim=format!("UPDATE delegation_codes SET claim=$1 WHERE code_hash=$2 AND claim='' AND redirect=$3 AND challenge=$4 AND client_id=$5 AND EXISTS(SELECT 1 FROM delegation_clients c WHERE c.id=delegation_codes.client_id AND c.secret_hash=$6 AND c.active=1 AND c.generation=delegation_codes.client_generation) AND expires_at>$7 AND {fence}");
        self.sharing_txn(vec![(claim,vec![r.claim.clone().into(),r.code_hash.clone().into(),r.redirect.into(),r.challenge.into(),r.client_id.clone().into(),r.secret_hash.clone().into(),clock.into(),proof.into()]),("INSERT INTO delegation_grants(grant_hash,code_hash,client_id,client_generation,user_id,subject,source_hash,expires_at) SELECT $1,code_hash,client_id,client_generation,user_id,subject,source_hash,$2 FROM delegation_codes WHERE code_hash=$3 AND claim=$4".into(),vec![r.grant_hash.clone().into(),(clock+2592000).into(),r.code_hash.into(),r.claim.into()])]).await?;
        self.delegation_introspect(&r.client_id, &r.secret_hash, &r.grant_hash)
            .await
    }
    async fn delegation_introspect(
        &self,
        client: &str,
        secret: &str,
        grant: &str,
    ) -> Result<Option<DelegationIdentity>, StoreError> {
        let rows=self.sharing_read("SELECT json_object('subject',g.subject,'display_name',u.username,'expires_at',g.expires_at,'source',g.source_hash,'user_id',g.user_id) AS payload FROM delegation_grants g JOIN delegation_clients c ON c.id=g.client_id AND c.generation=g.client_generation JOIN users u ON u.id=g.user_id WHERE g.grant_hash=$1 AND c.id=$2 AND c.secret_hash=$3 AND c.active=1 AND g.expires_at>$4",vec![grant.to_owned().into(),client.to_owned().into(),secret.to_owned().into(),now()?.into()]).await?;
        #[derive(Deserialize)]
        struct Grant {
            subject: String,
            display_name: String,
            expires_at: i64,
            source: String,
            user_id: i64,
        }
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let g: Grant = decode(row)?;
        if source_proof_scoped(
            self,
            &g.source,
            g.user_id,
            &g.subject,
            Some((client, secret, grant)),
        )
        .await?
        .is_none()
        {
            return Ok(None);
        }
        Ok(Some(DelegationIdentity {
            subject: g.subject,
            display_name: g.display_name,
            expires_at: g.expires_at,
        }))
    }
    async fn delegation_revoke(
        &self,
        client: &str,
        secret: &str,
        grant: &str,
    ) -> Result<(), StoreError> {
        self.sharing_txn(vec![("DELETE FROM delegation_grants WHERE grant_hash=$1 AND client_id=$2 AND EXISTS(SELECT 1 FROM delegation_clients c WHERE c.id=$2 AND c.secret_hash=$3)".into(),vec![grant.to_owned().into(),client.to_owned().into(),secret.to_owned().into()])]).await?;
        Ok(())
    }
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
    let conn = rusqlite::Connection::open_in_memory().expect("delegation schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("delegation schema dependencies");
    conn.execute_batch(SCHEMA).expect("delegation schema");
    rows(&conn).expect("delegation schema shape")
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
            "playback delegation schema is partial or incompatible".into(),
        ))
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "playback delegation schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(MARKER_SQL, [], |row| row.get(0))?;
    if count != 1 {
        return Err(StoreError::Migration(
            "playback delegation marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{JellyfinEntityKind, JellyfinIdentityStore, SqliteStore, UserStore};
    #[tokio::test]
    async fn delegation_idle_expiry_and_partial_schema_fail_closed() {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("cinema.db");
        let store = SqliteStore::open(&path).expect("store");
        let user = store
            .create_user("person", "password", false)
            .await
            .expect("user");
        store
            .create_token("source", user.id, None)
            .await
            .expect("source");
        let subject = store
            .jellyfin_entity_ids(JellyfinEntityKind::User, &[user.id])
            .await
            .expect("subject")[0]
            .wire_id
            .clone();
        let conn = rusqlite::Connection::open(path).expect("connection");
        let clock = now().expect("clock");
        conn.execute(
            "UPDATE tokens SET last_seen_at=?1 WHERE token_hash='source'",
            [clock - 172800],
        )
        .expect("old activity");
        for (key, value) in [
            (
                super::super::keys::AUTH_TOKEN_EXPIRY_ENABLED,
                "true".to_owned(),
            ),
            (super::super::keys::AUTH_TOKEN_IDLE_DAYS, "1".to_owned()),
            (
                super::super::keys::AUTH_TOKEN_EXPIRY_SINCE,
                (clock - 259200).to_string(),
            ),
        ] {
            conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",rusqlite::params![key,value]).expect("policy");
        }
        assert!(source_proof(&store, "source", user.id, &subject)
            .await
            .expect("idle proof")
            .is_none());
        let last: i64 = conn
            .query_row(
                "SELECT last_seen_at FROM tokens WHERE token_hash='source'",
                [],
                |r| r.get(0),
            )
            .expect("activity");
        assert_eq!(last, clock - 172800);
        verify_sqlite(&conn).expect("installed");
        conn.execute_batch("DROP INDEX delegation_codes_expiry")
            .expect("partial fixture");
        assert!(verify_sqlite(&conn).is_err());
    }
}
