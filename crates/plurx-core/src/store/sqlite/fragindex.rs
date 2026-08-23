use async_trait::async_trait;

use super::SqliteStore;
use crate::error::StoreError;
use crate::segplan::{FragmentIndex, SourceIdentity};
use crate::store::FragmentIndexStore;

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
}

fn unix_ms() -> Result<i64, StoreError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| StoreError::Task(format!("system clock precedes unix epoch: {error}")))?
        .as_millis()
        .min(i64::MAX as u128) as i64)
}
