//! Receiver-private progress. These statements never touch household watch rows.
//! The HTTP service must separately verify current source item/library membership
//! and a live local login; this Store boundary atomically enforces B assignments.
use super::sharing::{Backend, Statement, Value};
use crate::{
    error::StoreError,
    sharing::{invalid, SourceId},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
const MAX_SAFE: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteWatch {
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub watched: bool,
    pub sequence: i64,
    pub updated_at_ms: i64,
}
impl RemoteWatch {
    fn validate(&self) -> Result<(), StoreError> {
        if !(0..=MAX_SAFE).contains(&self.position_ms)
            || self
                .duration_ms
                .is_some_and(|n| !(0..=MAX_SAFE).contains(&n))
            || !(0..=MAX_SAFE).contains(&self.sequence)
            || !(0..=MAX_SAFE).contains(&self.updated_at_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteProgressOutcome {
    Applied,
    Stale,
    Conflict,
    Unauthorized,
}

#[derive(Debug, Clone)]
pub struct RemoteWatchUpdate {
    pub import_id: Uuid,
    pub library_id: SourceId,
    pub item_id: SourceId,
    pub user_id: i64,
    pub lifecycle_generation: i64,
    pub assignment_generation: i64,
    pub progress: RemoteWatch,
}

/// Captured receiver authority; no credential or remote item data is persisted.
#[derive(Clone, Serialize)]
pub struct ReceiverCatalogueScope {
    pub import_id: Uuid,
    pub source_server_id: Uuid,
    pub catalogue_epoch: Uuid,
    pub lifecycle_generation: i64,
    pub assignment_generation: i64,
    pub endpoint_generation: i64,
    pub claim_id: Uuid,
    pub remote_grant_id: Uuid,
    pub libraries: Vec<SourceId>,
}
#[async_trait]
pub trait SharingCatalogueStore: Send + Sync {
    /// Read-only login + user + captured effective scope authority in one
    /// snapshot. Unlike authenticate_token this never renews last_seen_at.
    /// now_s is a trusted server clock capture, never request input.
    async fn receiver_catalogue_authorized(
        &self,
        token_hash: &str,
        user: i64,
        scopes: &[ReceiverCatalogueScope],
        now_s: i64,
    ) -> Result<bool, StoreError>;
    /// One consistent, generation-bound receiver authority query per operation.
    async fn assigned_catalogue_libraries(
        &self,
        import: Uuid,
        user: i64,
        lifecycle: i64,
        assignment: i64,
    ) -> Result<Vec<SourceId>, StoreError>;
    async fn save_remote_watch(
        &self,
        update: RemoteWatchUpdate,
    ) -> Result<RemoteProgressOutcome, StoreError>;
    async fn remote_watch(
        &self,
        import: Uuid,
        library: SourceId,
        item: SourceId,
        user: i64,
    ) -> Result<Option<RemoteWatch>, StoreError>;
}
const AUTH: &str = "FROM sharing_imports i JOIN sharing_assignments a ON a.import_id=i.id JOIN sharing_viewers v ON v.user_id=a.user_id JOIN users u ON u.id=v.user_id WHERE i.id=$1 AND i.state='active' AND a.remote_library_id=$2 AND a.user_id=$4 AND a.enabled=1";
#[async_trait]
impl<T: Backend> SharingCatalogueStore for T {
    async fn receiver_catalogue_authorized(
        &self,
        hash: &str,
        user: i64,
        scopes: &[ReceiverCatalogueScope],
        now_s: i64,
    ) -> Result<bool, StoreError> {
        if !crate::sharing::is_hash(hash)
            || user <= 0
            || !(0..=MAX_SAFE).contains(&now_s)
            || scopes.len() > 64
            || scopes.iter().any(|s| {
                s.lifecycle_generation <= 0
                    || s.assignment_generation <= 0
                    || s.endpoint_generation <= 0
                    || s.libraries.len() > 64
                    || s.libraries.is_empty()
            })
        {
            return Err(invalid());
        }
        let scopes = serde_json::to_string(scopes).map_err(|_| invalid())?;
        // Benign assignment additions and endpoint refreshes advance generations
        // without invalidating captured, still-effective library tuples. A
        // rewind, lifecycle replacement or lost assignment refuses the proof.
        // Settings are bounded before JSON construction. Oversized policy state
        // refuses proof instead of being silently treated as expiry disabled.
        let policy = |key: &str| {
            format!("(SELECT CASE WHEN length(CAST(value AS BLOB))<=64 THEN value ELSE NULL END FROM settings WHERE key={key})")
        };
        let enabled = policy("$4");
        let days = policy("$5");
        let since = policy("$6");
        let sql=format!("SELECT json_object('last_seen',t.last_seen_at,'enabled',{enabled},'days',{days},'since',{since},'bad_policy',EXISTS(SELECT 1 FROM settings WHERE key IN ($4,$5,$6) AND length(CAST(value AS BLOB))>64)) AS payload FROM tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND u.id=$2 AND NOT EXISTS(SELECT 1 FROM json_each($3) q WHERE NOT EXISTS(SELECT 1 FROM sharing_imports i JOIN sharing_viewers v ON v.user_id=u.id WHERE i.id=json_extract(q.value,'$.import_id') AND i.source_server_id=json_extract(q.value,'$.source_server_id') AND i.catalogue_epoch=json_extract(q.value,'$.catalogue_epoch') AND i.lifecycle_generation=json_extract(q.value,'$.lifecycle_generation') AND i.assignment_generation>=json_extract(q.value,'$.assignment_generation') AND i.endpoint_generation>=json_extract(q.value,'$.endpoint_generation') AND i.claim_id=json_extract(q.value,'$.claim_id') AND i.remote_grant_id=json_extract(q.value,'$.remote_grant_id') AND i.state='active' AND NOT EXISTS(SELECT 1 FROM json_each(q.value,'$.libraries') l WHERE NOT EXISTS(SELECT 1 FROM sharing_assignments a WHERE a.import_id=i.id AND a.user_id=u.id AND a.enabled=1 AND a.remote_library_id=l.value))))");
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    hash.to_owned().into(),
                    user.into(),
                    scopes.into(),
                    super::keys::AUTH_TOKEN_EXPIRY_ENABLED.to_owned().into(),
                    super::keys::AUTH_TOKEN_IDLE_DAYS.to_owned().into(),
                    super::keys::AUTH_TOKEN_EXPIRY_SINCE.to_owned().into(),
                ],
            )
            .await?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Row {
            last_seen: i64,
            enabled: Option<String>,
            days: Option<String>,
            since: Option<String>,
            bad_policy: i64,
        }
        if rows.len() > 1 {
            return Err(invalid());
        }
        let Some(row) = rows.first() else {
            return Ok(false);
        };
        let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
        if row.bad_policy != 0 {
            return Err(invalid());
        }
        Ok(!crate::auth::TokenIdlePolicy::from_settings(
            row.enabled.as_deref(),
            row.days.as_deref(),
            row.since.as_deref(),
        )
        .is_some_and(|p| p.is_expired(row.last_seen, now_s)))
    }

    async fn assigned_catalogue_libraries(
        &self,
        import: Uuid,
        user: i64,
        lifecycle: i64,
        assignment: i64,
    ) -> Result<Vec<SourceId>, StoreError> {
        if user <= 0 || lifecycle <= 0 || assignment <= 0 {
            return Err(invalid());
        }
        let rows=self.sharing_read("SELECT json_quote(a.remote_library_id) AS payload FROM sharing_assignments a JOIN sharing_imports i ON i.id=a.import_id JOIN sharing_viewers v ON v.user_id=a.user_id JOIN users u ON u.id=v.user_id WHERE i.id=$1 AND a.user_id=$2 AND i.lifecycle_generation=$3 AND i.assignment_generation=$4 AND i.state='active' AND a.enabled=1 ORDER BY a.remote_library_id LIMIT 65",vec![import.into(),user.into(),lifecycle.into(),assignment.into()]).await?;
        if rows.len() > crate::sharing::MAX_LIBRARIES {
            return Err(invalid());
        }
        rows.into_iter()
            .map(|row| serde_json::from_str(&row).map_err(|_| invalid()))
            .collect()
    }

    async fn save_remote_watch(
        &self,
        update: RemoteWatchUpdate,
    ) -> Result<RemoteProgressOutcome, StoreError> {
        let RemoteWatchUpdate {
            import_id: import,
            library_id: library,
            item_id: item,
            user_id: user,
            lifecycle_generation,
            assignment_generation,
            progress,
        } = update;
        progress.validate()?;
        if lifecycle_generation <= 0 || assignment_generation <= 0 {
            return Err(invalid());
        }
        if user <= 0 {
            return Err(invalid());
        }
        let data = serde_json::to_string(&progress).map_err(|_| invalid())?;
        let sql = format!("INSERT INTO sharing_watch(source_server_id,catalogue_epoch,remote_library_id,remote_item_id,user_id,position_ms,duration_ms,watched,sequence,updated_at_ms)
            SELECT i.source_server_id,i.catalogue_epoch,$2,$3,$4,json_extract($5,'$.position_ms'),json_extract($5,'$.duration_ms'),json_extract($5,'$.watched'),json_extract($5,'$.sequence'),json_extract($5,'$.updated_at_ms') {AUTH} AND i.lifecycle_generation=$6 AND i.assignment_generation=$7
            ON CONFLICT(source_server_id,catalogue_epoch,remote_item_id,user_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=excluded.duration_ms,watched=excluded.watched,sequence=excluded.sequence,updated_at_ms=excluded.updated_at_ms
            WHERE sharing_watch.remote_library_id=excluded.remote_library_id AND (sharing_watch.sequence<excluded.sequence OR (sharing_watch.sequence=excluded.sequence AND sharing_watch.position_ms=excluded.position_ms AND sharing_watch.duration_ms IS excluded.duration_ms AND sharing_watch.watched=excluded.watched AND sharing_watch.updated_at_ms=excluded.updated_at_ms))");
        let statement: Statement = (
            sql,
            vec![
                import.into(),
                library.as_str().to_owned().into(),
                item.as_str().to_owned().into(),
                user.into(),
                data.into(),
                lifecycle_generation.into(),
                assignment_generation.into(),
            ],
        );
        if self.sharing_txn(vec![statement]).await?.first() == Some(&1) {
            return Ok(RemoteProgressOutcome::Applied);
        }
        let sql=format!("SELECT json_object('sequence',w.sequence,'library_id',w.remote_library_id) AS payload FROM sharing_watch w WHERE w.source_server_id=(SELECT i.source_server_id {AUTH}) AND w.catalogue_epoch=(SELECT i.catalogue_epoch {AUTH}) AND w.remote_item_id=$3 AND w.user_id=$4 AND EXISTS(SELECT 1 {AUTH} AND i.lifecycle_generation=$5 AND i.assignment_generation=$6)");
        let mut values = params(import, &library, &item, user);
        values.extend([lifecycle_generation.into(), assignment_generation.into()]);
        let rows = self.sharing_read(&sql, values).await?;
        #[derive(Deserialize)]
        struct Current {
            sequence: i64,
            library_id: SourceId,
        }
        if let Some(row) = rows.first() {
            let current: Current = serde_json::from_str(row).map_err(|_| invalid())?;
            return Ok(
                if current.library_id == library && current.sequence > progress.sequence {
                    RemoteProgressOutcome::Stale
                } else {
                    RemoteProgressOutcome::Conflict
                },
            );
        }
        Ok(RemoteProgressOutcome::Unauthorized)
    }
    async fn remote_watch(
        &self,
        import: Uuid,
        library: SourceId,
        item: SourceId,
        user: i64,
    ) -> Result<Option<RemoteWatch>, StoreError> {
        if user <= 0 {
            return Err(invalid());
        }
        let sql=format!("SELECT json_object('position_ms',w.position_ms,'duration_ms',w.duration_ms,'watched',json(CASE WHEN w.watched=1 THEN 'true' ELSE 'false' END),'sequence',w.sequence,'updated_at_ms',w.updated_at_ms) AS payload FROM sharing_watch w WHERE w.source_server_id=(SELECT i.source_server_id {AUTH}) AND w.catalogue_epoch=(SELECT i.catalogue_epoch {AUTH}) AND w.remote_library_id=$2 AND w.remote_item_id=$3 AND w.user_id=$4");
        self.sharing_read(&sql, params(import, &library, &item, user))
            .await?
            .first()
            .map(|row| decode_watch(row))
            .transpose()
    }
}
fn params(import: Uuid, library: &SourceId, item: &SourceId, user: i64) -> Vec<Value> {
    vec![
        import.into(),
        library.as_str().to_owned().into(),
        item.as_str().to_owned().into(),
        user.into(),
    ]
}

fn decode_watch(row: &str) -> Result<RemoteWatch, StoreError> {
    let progress: RemoteWatch = serde_json::from_str(row).map_err(|_| invalid())?;
    progress.validate()?;
    Ok(progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn sharing_receiver_idle_authority_never_touches_activity_and_refuses_expiry_or_bad_policy(
    ) {
        use crate::store::{SqliteStore, UserStore};
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let directory = tempfile::tempdir().expect("authority fixtures");
        for store in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&directory.path().join("receiver.sqlite")).expect("pooled"),
        ] {
            let user = store
                .create_user("viewer", "synthetic-password-hash", false)
                .await
                .expect("user");
            let hash = "a".repeat(64);
            store
                .create_token(&hash, user.id, None)
                .await
                .expect("token");
            store.sharing_txn(vec![("UPDATE tokens SET last_seen_at=unixepoch()-259200 WHERE token_hash=$1".into(),vec![hash.clone().into()]),("INSERT INTO settings(key,value) VALUES('auth.token_expiry_enabled','false') ON CONFLICT(key) DO UPDATE SET value=excluded.value".into(),vec![])]).await.expect("idle token fixture");
            let before = store.list_tokens_for_user(user.id).await.expect("before")[0].last_seen_at;
            assert!(store
                .receiver_catalogue_authorized(&hash, user.id, &[], now_s)
                .await
                .expect("expiry off"));
            assert_eq!(
                before,
                store
                    .list_tokens_for_user(user.id)
                    .await
                    .expect("read-only activity")[0]
                    .last_seen_at
            );
            store.sharing_txn(vec![("UPDATE settings SET value='true' WHERE key='auth.token_expiry_enabled'".into(),vec![]),("INSERT INTO settings(key,value) VALUES('auth.token_idle_days','1'),('auth.token_expiry_since','1000') ON CONFLICT(key) DO UPDATE SET value=excluded.value".into(),vec![])]).await.expect("expired policy");
            assert!(!store
                .receiver_catalogue_authorized(&hash, user.id, &[], now_s)
                .await
                .expect("expiry honored"));
            assert_eq!(
                before,
                store
                    .list_tokens_for_user(user.id)
                    .await
                    .expect("expired read-only activity")[0]
                    .last_seen_at
            );
            store
                .sharing_txn(vec![(
                    "UPDATE settings SET value=$1 WHERE key='auth.token_expiry_since'".into(),
                    vec!["1".repeat(65).into()],
                )])
                .await
                .expect("oversized policy");
            assert!(store
                .receiver_catalogue_authorized(&hash, user.id, &[], now_s)
                .await
                .is_err());
            assert!(store
                .receiver_catalogue_authorized(&hash, 0, &[], now_s)
                .await
                .is_err());
        }
    }
    #[test]
    fn sharing_private_watch_rejects_unsafe_persisted_positions() {
        let progress = RemoteWatch {
            position_ms: MAX_SAFE + 1,
            duration_ms: None,
            watched: false,
            sequence: 1,
            updated_at_ms: 1000,
        };
        assert!(decode_watch(
            &serde_json::to_string(&progress).expect("synthetic progress fixture")
        )
        .is_err());
        assert!(decode_watch(r#"{"position_ms":1,"duration_ms":null,"watched":false,"sequence":1,"updated_at_ms":1000,"path":"/media"}"#).is_err());
    }
}
