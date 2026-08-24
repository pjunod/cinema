use async_trait::async_trait;

use super::SqliteStore;
use crate::error::StoreError;
use crate::segplan::{FragmentIndex, SegmentPlan, SourceIdentity};
use crate::store::{FragmentIndexStore, RenditionPlanStore};

#[async_trait]
impl FragmentIndexStore for SqliteStore {
    async fn put_fragment_index(
        &self,
        file_id: i64,
        index: &FragmentIndex,
    ) -> Result<(), StoreError> {
        let index = index.clone();
        let now_ms = unix_ms()?;
        self.with_conn(move |conn| crate::store::fragindex::put(conn, file_id, &index, now_ms))
            .await
    }

    async fn fragment_index(
        &self,
        file_id: i64,
        identity: &SourceIdentity,
    ) -> Result<Option<FragmentIndex>, StoreError> {
        let identity = identity.clone();
        self.with_read(move |conn| crate::store::fragindex::get(conn, file_id, &identity))
            .await
    }

    async fn forget_fragment_index(&self, file_id: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| crate::store::fragindex::forget(conn, file_id))
            .await
    }

    async fn vod_row_file_ids(&self, limit: i64) -> Result<Vec<i64>, StoreError> {
        self.with_read(move |conn| crate::store::fragindex::vod_row_file_ids(conn, limit))
            .await
    }

    async fn surviving_file_ids(&self, file_ids: &[i64]) -> Result<Vec<i64>, StoreError> {
        let wanted = file_ids.to_vec();
        self.with_read(move |conn| {
            let mut alive = Vec::new();
            let mut statement = conn.prepare("SELECT 1 FROM files WHERE id = ?1")?;
            for id in wanted {
                if statement.exists(rusqlite::params![id])? {
                    alive.push(id);
                }
            }
            Ok(alive)
        })
        .await
    }
}

#[async_trait]
impl RenditionPlanStore for SqliteStore {
    async fn put_rendition_plan(
        &self,
        rendition_key: &str,
        file_id: i64,
        plan: &SegmentPlan,
        source: &SourceIdentity,
    ) -> Result<bool, StoreError> {
        let rendition_key = rendition_key.to_owned();
        let plan = plan.clone();
        let source = source.clone();
        let now_ms = unix_ms()?;
        self.with_conn(move |conn| {
            crate::store::renditionplan::put_if_absent(
                conn,
                &rendition_key,
                file_id,
                &plan,
                &source,
                now_ms,
            )
        })
        .await
    }

    async fn rendition_plan(
        &self,
        rendition_key: &str,
        source: &SourceIdentity,
    ) -> Result<Option<SegmentPlan>, StoreError> {
        let rendition_key = rendition_key.to_owned();
        let source = source.clone();
        self.with_read(move |conn| crate::store::renditionplan::get(conn, &rendition_key, &source))
            .await
    }

    async fn forget_rendition_plans(&self, file_id: i64) -> Result<usize, StoreError> {
        self.with_conn(move |conn| crate::store::renditionplan::forget_file(conn, file_id))
            .await
    }
}

fn unix_ms() -> Result<i64, StoreError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| StoreError::Task(format!("system clock precedes unix epoch: {error}")))?
        .as_millis()
        .min(i64::MAX as u128) as i64)
}
