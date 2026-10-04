//! Exact published B session authority plus global Source/item/user ordering.
use super::{
    sharing::{ordered, Backend, Value},
    sharing_receiver_sessions::{
        source_assert, source_current, source_values, source_write_refused, ATTACHED, PUBLISHED,
    },
};
use crate::{
    error::StoreError,
    sharing::invalid,
    sharing_receiver_progress::{ReceiverProgress, ReceiverProgressOutcome},
    sharing_receiver_sessions::ReceiverSessionWriteAuthority,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
const MAX_SAFE: i64 = 9_007_199_254_740_991;
#[async_trait]
pub trait SharingReceiverProgressStore: Send + Sync {
    /// B metadata authority only; the caller owns actual Source actor evidence.
    async fn save_receiver_progress(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        progress: &ReceiverProgress,
    ) -> Result<ReceiverProgressOutcome, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingReceiverProgressStore for T {
    async fn save_receiver_progress(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        progress: &ReceiverProgress,
    ) -> Result<ReceiverProgressOutcome, StoreError> {
        if !(0..=MAX_SAFE).contains(&progress.sequence)
            || !(0..=MAX_SAFE).contains(&progress.position_ms)
            || progress
                .duration_ms
                .is_some_and(|n| !(0..=MAX_SAFE).contains(&n))
        {
            return Err(invalid());
        }
        let Some(mut values) = source_values(authority, &progress.attachment)? else {
            return Ok(ReceiverProgressOutcome::Refused);
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .filter(|n| (1..=MAX_SAFE).contains(n))
            .ok_or_else(invalid)?;
        let data=serde_json::json!({"sequence":progress.sequence,"position_ms":progress.position_ms,"duration_ms":progress.duration_ms,"watched":progress.watched,"updated_at_ms":now}).to_string();
        values.push(Value::Text(data));
        let current = source_current(ATTACHED, PUBLISHED);
        let key="w.source_server_id=json_extract($3,'$.source_server_id') AND w.catalogue_epoch=json_extract($3,'$.catalogue_epoch') AND w.remote_item_id=json_extract($11,'$.reference.item_id') AND w.user_id=$2";
        let library = "w.remote_library_id=json_extract($11,'$.reference.library_id')";
        let same="w.sequence=json_extract($20,'$.sequence') AND w.position_ms=json_extract($20,'$.position_ms') AND w.duration_ms IS json_extract($20,'$.duration_ms') AND w.watched=json_extract($20,'$.watched')";
        // The leader captures a bounded preimage, then the transaction proves
        // that exact row still exists (or remains absent) before mutation. This
        // is deterministic replicated SQL: no connection-local changes() or DB
        // clock. Concurrent advancement requires a fresh read, never overwrite.
        let current=format!("({current}) AND (SELECT count(*) FROM (SELECT 1 FROM sharing_watch w WHERE {key} LIMIT 2))<=1");
        let projection="CASE WHEN typeof(w.remote_library_id)='text' AND length(CAST(w.remote_library_id AS BLOB))<=19 THEN json_object('library_id',w.remote_library_id,'sequence',w.sequence,'position_ms',w.position_ms,'duration_ms',w.duration_ms,'watched',w.watched,'updated_at_ms',w.updated_at_ms) ELSE '{}' END";
        let snapshot =
            format!("coalesce((SELECT {projection} FROM sharing_watch w WHERE {key}),'null')");
        let read = format!(
            "SELECT {snapshot} AS payload WHERE ({current}) AND json_extract($20,'$.sequence')>=0"
        );
        let statement = ordered(&read, values.clone())?;
        let rows = self.sharing_read(&statement.0, statement.1).await?;
        let [preimage] = rows.as_slice() else {
            return if rows.is_empty() {
                Ok(ReceiverProgressOutcome::Refused)
            } else {
                Err(invalid())
            };
        };
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Row {
            library_id: crate::sharing::SourceId,
            sequence: i64,
            position_ms: i64,
            duration_ms: Option<i64>,
            watched: i64,
            updated_at_ms: i64,
        }
        let row: Option<Row> = serde_json::from_str(preimage).map_err(|_| invalid())?;
        if row.as_ref().is_some_and(|row| {
            ![0, 1].contains(&row.watched)
                || !(0..=MAX_SAFE).contains(&row.sequence)
                || !(0..=MAX_SAFE).contains(&row.position_ms)
                || !(0..=MAX_SAFE).contains(&row.updated_at_ms)
                || row
                    .duration_ms
                    .is_some_and(|n| !(0..=MAX_SAFE).contains(&n))
        }) {
            return Err(invalid());
        }
        let outcome = match row.as_ref() {
            None => ReceiverProgressOutcome::Applied,
            Some(row) if row.library_id != progress.attachment.binding.reference.library_id => {
                ReceiverProgressOutcome::Conflict
            }
            Some(row) if row.sequence < progress.sequence => ReceiverProgressOutcome::Applied,
            Some(row) if row.sequence > progress.sequence => ReceiverProgressOutcome::Stale,
            Some(row)
                if row.position_ms == progress.position_ms
                    && row.duration_ms == progress.duration_ms
                    && (row.watched == 1) == progress.watched =>
            {
                ReceiverProgressOutcome::Replay
            }
            Some(_) => ReceiverProgressOutcome::Conflict,
        };
        // The consistent read may await Raft/network. Re-capture the actual
        // clock and refuse expired/stale owner or login proof before the txn.
        let Some(mut fresh) = source_values(authority, &progress.attachment)? else {
            return Ok(ReceiverProgressOutcome::Refused);
        };
        fresh.push(values[19].clone());
        values = fresh;
        values.push(Value::Text(preimage.clone()));
        let captured =
            format!("({current}) AND ({snapshot})=$21 AND json_extract($20,'$.sequence')>=0");
        let pre = ordered(&source_assert(captured.clone(), vec![]).0, values.clone())?;
        let statements = if outcome == ReceiverProgressOutcome::Applied {
            let sql=format!("INSERT INTO sharing_watch(source_server_id,catalogue_epoch,remote_library_id,remote_item_id,user_id,position_ms,duration_ms,watched,sequence,updated_at_ms) SELECT json_extract($3,'$.source_server_id'),json_extract($3,'$.catalogue_epoch'),json_extract($11,'$.reference.library_id'),json_extract($11,'$.reference.item_id'),$2,json_extract($20,'$.position_ms'),json_extract($20,'$.duration_ms'),json_extract($20,'$.watched'),json_extract($20,'$.sequence'),json_extract($20,'$.updated_at_ms') WHERE ({captured}) ON CONFLICT(source_server_id,catalogue_epoch,remote_item_id,user_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=excluded.duration_ms,watched=excluded.watched,sequence=excluded.sequence,updated_at_ms=excluded.updated_at_ms WHERE sharing_watch.remote_library_id=excluded.remote_library_id AND sharing_watch.sequence<excluded.sequence");
            let effect=format!("EXISTS(SELECT 1 FROM sharing_watch w WHERE {key} AND ({library}) AND ({same}) AND w.updated_at_ms=json_extract($20,'$.updated_at_ms'))");
            let post = ordered(
                &source_assert(format!("({current}) AND ({effect})"), vec![]).0,
                values[..20].to_vec(),
            )?;
            vec![pre, ordered(&sql, values)?, post]
        } else {
            // No history INSERT/UPDATE occurs for replay, stale or conflict.
            vec![pre, ordered(&source_assert(captured, vec![]).0, values)?]
        };
        match self.sharing_txn(statements).await {
            Ok(_) => Ok(outcome),
            Err(e) if source_write_refused(&e) => Ok(ReceiverProgressOutcome::Refused),
            Err(e) => Err(e),
        }
    }
}

/// Test-only interleaving: return the captured watch read after another actual
/// published B owner has committed progress through the production writer.
#[cfg(test)]
pub(crate) struct ProgressSnapshotRace<'a> {
    pub store: &'a super::SqliteStore,
    pub authority: &'a ReceiverSessionWriteAuthority,
    pub progress: &'a ReceiverProgress,
    pub advanced: std::sync::atomic::AtomicBool,
}
#[cfg(test)]
#[async_trait]
impl Backend for ProgressSnapshotRace<'_> {
    async fn sharing_read(&self, sql: &str, values: Vec<Value>) -> Result<Vec<String>, StoreError> {
        let rows = self.store.sharing_read(sql, values).await?;
        if sql.starts_with("SELECT coalesce(")
            && !self
                .advanced
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            && self
                .store
                .save_receiver_progress(self.authority, self.progress)
                .await?
                != ReceiverProgressOutcome::Applied
        {
            return Err(invalid());
        }
        Ok(rows)
    }
    async fn sharing_txn(
        &self,
        statements: Vec<super::sharing::Statement>,
    ) -> Result<Vec<usize>, StoreError> {
        self.store.sharing_txn(statements).await
    }
    async fn sharing_revision_key_rows(&self) -> Result<Vec<String>, StoreError> {
        self.store.sharing_revision_key_rows().await
    }
    async fn sharing_file_locator_key_rows(&self) -> Result<Vec<String>, StoreError> {
        self.store.sharing_file_locator_key_rows().await
    }
    async fn sharing_purpose_archive_rows(&self) -> Result<Vec<String>, StoreError> {
        self.store.sharing_purpose_archive_rows().await
    }
}
