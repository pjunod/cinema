//! Replicated receiver/grant credentials. Ephemeral control never writes here.
use crate::error::StoreError;
use async_trait::async_trait;
use serde::Serialize;

pub const SCHEMA: &[(&str, &str)] = &[
("remote_schema", "CREATE TABLE remote_schema (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1)) STRICT"),
("remote_receivers", "CREATE TABLE remote_receivers (id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, name TEXT NOT NULL, platform TEXT NOT NULL, secret_hash TEXT NOT NULL, created_at INTEGER NOT NULL, revoked_at INTEGER) STRICT"),
("remote_grants", "CREATE TABLE remote_grants (id TEXT PRIMARY KEY, receiver_id TEXT NOT NULL REFERENCES remote_receivers(id) ON DELETE CASCADE, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, name TEXT NOT NULL, secret_hash TEXT NOT NULL, created_at INTEGER NOT NULL, revoked_at INTEGER) STRICT"),
("remote_grants_receiver", "CREATE INDEX remote_grants_receiver ON remote_grants(receiver_id, revoked_at)"),
("remote_claim_budget", "CREATE TABLE remote_claim_budget (user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE, window_started INTEGER NOT NULL, attempts INTEGER NOT NULL) STRICT"),
];
pub fn migration_sql() -> String {
    let mut sql = SCHEMA
        .iter()
        .map(|(_, sql)| format!("{sql};\n"))
        .collect::<String>();
    sql.push_str("INSERT INTO remote_schema VALUES(1,1);\n");
    sql
}
pub const SHAPE_SQL: &str = "SELECT name, sql FROM sqlite_master WHERE name IN ('remote_schema','remote_receivers','remote_grants','remote_grants_receiver','remote_claim_budget') ORDER BY name";
pub fn verify_shape(rows: &[(String, String)]) -> Result<bool, StoreError> {
    if rows.is_empty() {
        return Ok(false);
    }
    let normalized = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if rows.len() != SCHEMA.len()
        || SCHEMA.iter().any(|(name, sql)| {
            !rows
                .iter()
                .any(|(n, s)| n == name && normalized(s) == normalized(sql))
        })
    {
        return Err(StoreError::Migration(
            "incompatible or partial Cinema remote schema".into(),
        ));
    }
    Ok(true)
}
#[derive(Clone, Debug, Serialize)]
pub struct RemoteReceiver {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub platform: String,
    pub created_at: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct RemoteGrant {
    pub id: String,
    pub receiver_id: String,
    pub name: String,
    pub created_at: i64,
}
#[derive(Clone)]
pub struct NewRemoteReceiver {
    pub receiver: RemoteReceiver,
    pub secret_hash: String,
}
#[derive(Clone)]
pub struct NewRemoteGrant {
    pub grant: RemoteGrant,
    pub user_id: i64,
    pub secret_hash: String,
}

pub const INSERT_RECEIVER: &str = "INSERT INTO remote_receivers(id,user_id,name,platform,secret_hash,created_at) SELECT $1,$2,$3,$4,$5,$6 WHERE EXISTS(SELECT 1 FROM users WHERE id=$2) AND (SELECT count(*) FROM remote_receivers WHERE user_id=$2 AND revoked_at IS NULL)<20";
pub const INSERT_GRANT: &str = "INSERT INTO remote_grants(id,receiver_id,user_id,name,secret_hash,created_at) SELECT $1,$2,$3,$4,$5,$6 WHERE EXISTS(SELECT 1 FROM remote_receivers WHERE id=$2 AND user_id=$3 AND revoked_at IS NULL) AND (SELECT count(*) FROM remote_grants WHERE receiver_id=$2 AND revoked_at IS NULL)<8";
pub const AUTHORITY: &str = "SELECT r.id,r.user_id,r.name,r.platform,r.created_at FROM remote_receivers r JOIN users u ON u.id=r.user_id WHERE r.id=$1 AND r.user_id=$2 AND r.revoked_at IS NULL AND (($3='receiver' AND r.secret_hash=$4) OR ($3='grant' AND EXISTS(SELECT 1 FROM remote_grants g WHERE g.id=$4 AND g.receiver_id=r.id AND g.user_id=r.user_id AND g.revoked_at IS NULL AND g.secret_hash=$5)))";
pub const METADATA: &str = "SELECT r.id,r.user_id,r.name,r.platform,r.created_at FROM remote_receivers r JOIN users u ON u.id=r.user_id WHERE r.id=$1 AND r.user_id=$2 AND r.revoked_at IS NULL";
#[derive(Clone)]
pub enum RemoteProof {
    Receiver {
        secret_hash: String,
    },
    Grant {
        grant_id: String,
        secret_hash: String,
    },
}
impl RemoteProof {
    pub fn args(&self) -> Result<(&'static str, &str, &str), StoreError> {
        let valid_hash =
            |hash: &str| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        match self {
            Self::Receiver { secret_hash } if valid_hash(secret_hash) => {
                Ok(("receiver", secret_hash, ""))
            }
            Self::Grant {
                grant_id,
                secret_hash,
            } if uuid::Uuid::parse_str(grant_id).is_ok() && valid_hash(secret_hash) => {
                Ok(("grant", grant_id, secret_hash))
            }
            _ => Err(StoreError::Credential("invalid Cinema remote proof".into())),
        }
    }
}
pub const LIST_RECEIVERS: &str = "SELECT id,user_id,name,platform,created_at FROM remote_receivers WHERE user_id=$1 AND revoked_at IS NULL ORDER BY created_at,id LIMIT 20";
pub const LIST_GRANTS: &str = "SELECT g.id,g.receiver_id,g.name,g.created_at FROM remote_grants g JOIN remote_receivers r ON r.id=g.receiver_id JOIN users u ON u.id=g.user_id WHERE g.user_id=$1 AND g.revoked_at IS NULL AND r.revoked_at IS NULL AND r.user_id=g.user_id ORDER BY g.created_at,g.id LIMIT 160";
pub const REVOKE_RECEIVER: &str =
    "UPDATE remote_receivers SET revoked_at=$1 WHERE id=$2 AND user_id=$3 AND revoked_at IS NULL";
pub const REVOKE_GRANT: &str =
    "UPDATE remote_grants SET revoked_at=$1 WHERE id=$2 AND user_id=$3 AND revoked_at IS NULL";
pub const CLAIM_BUDGET: &str = "INSERT INTO remote_claim_budget VALUES($1,$2,1) ON CONFLICT(user_id) DO UPDATE SET window_started=CASE WHEN $2-window_started>=60 THEN $2 ELSE window_started END, attempts=CASE WHEN $2-window_started>=60 THEN 1 ELSE attempts+1 END WHERE $2-window_started>=60 OR attempts<10";
#[async_trait]
pub trait RemoteStore: Send + Sync {
    async fn create_remote_receiver(&self, new: NewRemoteReceiver) -> Result<bool, StoreError>;
    async fn create_remote_grant(&self, new: NewRemoteGrant) -> Result<bool, StoreError>;
    /// Exactly one typed proof; malformed/missing proof is never metadata.
    async fn remote_authority(
        &self,
        receiver_id: &str,
        user_id: i64,
        proof: RemoteProof,
    ) -> Result<Option<RemoteReceiver>, StoreError>;
    /// Same-user metadata only. This never grants installation/control authority.
    async fn remote_receiver_metadata(
        &self,
        receiver_id: &str,
        user_id: i64,
    ) -> Result<Option<RemoteReceiver>, StoreError>;
    async fn remote_receivers(&self, user_id: i64) -> Result<Vec<RemoteReceiver>, StoreError>;
    async fn remote_grants(&self, user_id: i64) -> Result<Vec<RemoteGrant>, StoreError>;
    async fn revoke_remote_receiver(
        &self,
        id: &str,
        user_id: i64,
        now: i64,
    ) -> Result<(), StoreError>;
    async fn revoke_remote_grant(&self, id: &str, user_id: i64, now: i64)
        -> Result<(), StoreError>;
    async fn admit_remote_pair_claim(&self, user_id: i64, now: i64) -> Result<bool, StoreError>;
}

/// Offline portable restore must never resurrect previously revoked device
/// proofs. Older images have no remote objects and need no credential repair.
pub fn fence_restored_remote(connection: &rusqlite::Connection) -> Result<(), StoreError> {
    let rows = connection
        .prepare(SHAPE_SQL)?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if !verify_shape(&rows)? {
        return Ok(());
    }
    let version: i64 = connection.query_row(
        "SELECT version FROM remote_schema WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if version != 1 {
        return Err(StoreError::Migration(
            "unsupported restored remote schema".into(),
        ));
    }
    let tx = connection.unchecked_transaction()?;
    tx.execute(
        "UPDATE remote_receivers SET revoked_at=unixepoch() WHERE revoked_at IS NULL",
        [],
    )?;
    tx.execute(
        "UPDATE remote_grants SET revoked_at=unixepoch() WHERE revoked_at IS NULL",
        [],
    )?;
    tx.execute("DELETE FROM remote_claim_budget", [])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
