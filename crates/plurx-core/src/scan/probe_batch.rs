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
/// Accepted work stays with its durable attempt. One shared batch deadline
/// bounds waiting; it never becomes a duplicate unadmitted local subprocess.
pub(super) async fn consume(
    store: &PublicationStore<'_>,
    file: &MediaFile,
    id: &str,
    deadline: tokio::time::Instant,
) -> Result<Option<bool>, StoreError> {
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
            JobState::Queued | JobState::Running if tokio::time::Instant::now() < deadline => {
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
                // A timed-out leaf is a reported file failure, not a batch
                // abort. Later completed leaves still apply under this coordinator.
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::background_jobs::{ClaimJob, ClaimOutcome, JobKind};
    use crate::store::background_jobs_probe::{ProbeOutput, PublishProbeJob};
    use crate::store::PublicationFence;
    use crate::store::{
        BackgroundJobStore, CoordinationStore, LibraryStore, MediaStore, SqliteStore,
    };

    #[tokio::test]
    async fn overdue_first_leaf_does_not_discard_later_completed_facts() {
        let store = SqliteStore::open_in_memory().expect("store");
        let dir = tempfile::tempdir().expect("directory");
        let library = store
            .create_library(&crate::domain::NewLibrary {
                name: "probe page".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "probe".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let now = crate::cluster::coordination::unix_ms().expect("clock");
        let crate::cluster::coordination::LeaseClaim::Acquired(lease) = store
            .acquire_lease("repair:probe", "coordinator", now, now + 600_000)
            .await
            .expect("lease")
        else {
            panic!("coordinator");
        };
        let publication = PublicationStore::fenced(&store, PublicationFence::new(lease));
        let mut files = Vec::new();
        for name in ["first", "second"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"source").expect("source");
            let (size, mtime) = file_stat(&path).await.expect("stat");
            let id = store
                .upsert_file(
                    item,
                    &path.to_string_lossy(),
                    size,
                    mtime,
                    &ProbeResult::default(),
                )
                .await
                .expect("file");
            files.push(store.get_file(id).await.expect("lookup").expect("file"));
        }
        let jobs = enqueue(&publication, &files, Some(&"a".repeat(64)))
            .await
            .expect("enqueue");
        let second = store
            .background_job(&jobs[&files[1].id])
            .await
            .expect("lookup")
            .expect("job");
        let now = crate::cluster::coordination::unix_ms().expect("claim clock");
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: second.id,
                expected_revision: second.revision,
                node_id: "worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::MediaProbe,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await
            .expect("claim")
        else {
            panic!("claim");
        };
        let output = ProbeOutput {
            source: job.supported_payload().expect("payload"),
            probe: ProbeResult {
                duration_ms: Some(1234),
                ..Default::default()
            },
        };
        let published = store
            .publish_probe_job(PublishProbeJob {
                token: job.token.expect("token"),
                output,
                now_ms: now + 1,
            })
            .await
            .expect("publish");
        assert!(
            matches!(
                published,
                crate::store::background_jobs::JobPublishOutcome::Published { .. }
            ),
            "{published:?}"
        );
        let expired = tokio::time::Instant::now();
        assert_eq!(
            consume(&publication, &files[0], &jobs[&files[0].id], expired)
                .await
                .expect("overdue leaf is local failure"),
            None
        );
        assert_eq!(
            consume(&publication, &files[1], &jobs[&files[1].id], expired)
                .await
                .expect("drain completed leaf"),
            Some(true)
        );
        assert_eq!(
            store
                .get_file(files[1].id)
                .await
                .expect("file")
                .expect("file")
                .duration_ms,
            Some(1234)
        );
    }
}
