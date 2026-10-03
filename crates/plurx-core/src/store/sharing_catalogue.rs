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

#[async_trait]
pub trait SharingCatalogueStore: Send + Sync {
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
