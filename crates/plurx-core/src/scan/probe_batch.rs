//! At most one bounded page is outstanding for a re-probe coordinator.
use super::*;
use crate::domain::MediaFile;
use crate::store::background_jobs::{CancelWaiter, JobPayload, JobState};

pub(super) async fn enqueue(
    store: &PublicationStore<'_>,
    files: &[MediaFile],
    pipeline: Option<&str>,
) -> Result<BTreeMap<i64, String>, StoreError> {
    let mut jobs = BTreeMap::new();
    let Some(pipeline) = pipeline else {
        return Ok(jobs);
    };
    for file in files.iter().take(128) {
        check_scan_cancellation()?;
        if file_stat(&file.path).await.ok() == Some((file.size, file.mtime)) {
            if let Some(id) = store.enqueue_probe(file, pipeline).await? {
                jobs.insert(file.id, id);
            }
        }
    }
    Ok(jobs)
}
/// Pending work can fall back to the coordinator without delaying an otherwise
/// local pass. Already running work gets a bounded opportunity to finish.
pub(super) async fn consume(
    store: &PublicationStore<'_>,
    file: &MediaFile,
    id: &str,
) -> Result<Option<bool>, StoreError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        check_scan_cancellation()?;
        let Some(job) = store.background_job(id).await? else {
            return Ok(None);
        };
        let JobPayload::MediaProbe {
            source_generation,
            source_size,
            source_mtime,
            file_id,
            ..
        } = job.supported_payload()?
        else {
            return Ok(None);
        };
        if file_id != file.id || (source_size, source_mtime) != (file.size, file.mtime) {
            return Ok(Some(false));
        }
        match job.state {
            JobState::Succeeded => {
                if file_stat(&file.path).await.ok() != Some((source_size, source_mtime)) {
                    return Ok(Some(false));
                }
                return store.apply_probe(id).await.map(Some);
            }
            JobState::Running if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ => {
                store
                    .cancel_waiter(CancelWaiter {
                        scope: "probe".into(),
                        request_id: source_generation,
                        now_ms: crate::cluster::coordination::unix_ms()?,
                    })
                    .await?;
                return Ok(None);
            }
        }
    }
}
