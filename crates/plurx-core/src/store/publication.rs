//! Lease-fenced publication for cluster-wide background jobs.
//!
//! Readers still use the ordinary [`Store`](super::Store) boundary. Writers
//! receive this wrapper so the lease token is checked in the same transaction
//! as every durable mutation. Keeping the token behind a read lock also means
//! a renewal cannot replace it halfway through a publication call.

use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::cluster::coordination::{unix_ms, Lease, StoreCoordinator};
use crate::domain::{BookMetadataPatch, MetadataPatch, NewItem, NewPretranscodeJob, ProbeResult};
use crate::error::StoreError;

use super::{
    DvRecoveryGuardState, IdentityRepairOutcome, IdentityRepairPlan, IdentityRepairSnapshot,
    ReconcileOutcome, RootFingerprintStatus, SeriesHintOutcome, Store,
};

const PUBLICATION_CALL_SAFETY_WINDOW: Duration = Duration::from_secs(3);
type FencedFuture<'a, T> =
    Pin<Box<dyn std::future::Future<Output = Result<T, StoreError>> + Send + 'a>>;

#[derive(Clone)]
pub struct PublicationFence {
    state: Arc<RwLock<Option<Lease>>>,
    last: Arc<std::sync::RwLock<Lease>>,
    revoked: Arc<AtomicBool>,
}

impl PublicationFence {
    pub fn new(lease: Lease) -> Self {
        Self {
            state: Arc::new(RwLock::new(Some(lease.clone()))),
            last: Arc::new(std::sync::RwLock::new(lease)),
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn snapshot(&self) -> Option<Lease> {
        self.state.read().await.clone()
    }

    /// Renew while excluding publications from observing the predecessor token.
    /// The backend CAS may advance the revision before its response reaches
    /// this process, so releasing this write lock earlier would let a valid
    /// publication race with that response and present the just-stale token.
    pub async fn renew(
        &self,
        coordinator: &StoreCoordinator,
        ttl: Duration,
    ) -> Result<bool, StoreError> {
        let mut state = self.state.write().await;
        if self.revoked.load(Ordering::Acquire) {
            // No backend request has been dispatched yet. Preserve the
            // current token so graceful release can retire it; the deadline
            // path invalidates it explicitly after draining this future.
            return Ok(false);
        }
        let Some(current) = state.clone() else {
            return Ok(false);
        };
        let renewed = coordinator.renew(&current, ttl).await;
        if self.revoked.load(Ordering::Acquire) {
            // A local deadline may win after the backend request was sent.
            // Retain an acknowledged replacement solely so release can retire
            // it; the atomic revocation still blocks every publication.
            return match renewed {
                Ok(Some(replacement)) => {
                    *self
                        .last
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = replacement.clone();
                    *state = Some(replacement);
                    Ok(false)
                }
                Ok(None) => {
                    *state = None;
                    Ok(false)
                }
                Err(error) => {
                    *state = None;
                    Err(error)
                }
            };
        }
        let response_before_expiry = unix_ms()
            .map(|now| now < current.expires_at_unix_ms)
            .unwrap_or(false);
        match renewed {
            Ok(Some(replacement)) if response_before_expiry => {
                *self
                    .last
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = replacement.clone();
                *state = Some(replacement);
                Ok(true)
            }
            Ok(Some(replacement)) => {
                // The backend may acknowledge a replacement just after the
                // predecessor deadline. Preserve that exact token solely for
                // graceful retirement, but revoke publishers before releasing
                // the state lock so it can never authorize late work.
                *self
                    .last
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = replacement.clone();
                *state = Some(replacement);
                self.revoked.store(true, Ordering::Release);
                Ok(false)
            }
            Ok(None) => {
                *state = None;
                Ok(false)
            }
            Err(error) => {
                *state = None;
                Err(error)
            }
        }
    }

    /// Self-fence after a failed renewal. Work may finish computing, but its
    /// next durable publication is rejected before it reaches the backend.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    pub async fn invalidate(&self, expected: &Lease) -> bool {
        self.revoke();
        let mut state = self.state.write().await;
        if state.as_ref() != Some(expected) {
            return false;
        }
        *state = None;
        true
    }
}

/// Read-through store view whose job-owned mutations are lease fenced.
pub struct PublicationStore<'a> {
    store: &'a dyn Store,
    fence: Option<PublicationFence>,
}

struct FencedProviderBudget {
    store: Arc<dyn Store>,
    fence: PublicationFence,
}

#[async_trait::async_trait]
impl crate::metadata::budget::ProviderBudget for FencedProviderBudget {
    async fn update(
        &self,
        provider: crate::metadata::Provider,
        action: super::background_jobs_provider::ProviderBudgetAction,
    ) -> Result<super::background_jobs_provider::ProviderBudgetOutcome, StoreError> {
        let publisher = PublicationStore::fenced(self.store.as_ref(), self.fence.clone());
        let guard = publisher.token().await?;
        let lease = guard
            .as_ref()
            .ok_or_else(|| publisher.invalidated())?
            .clone();
        self.store
            .update_provider_budget(super::background_jobs_provider::ProviderBudgetRequest {
                provider,
                lease,
                now_ms: unix_ms()?,
                action,
            })
            .await
    }
}

impl<'a> PublicationStore<'a> {
    /// A client keeps the same renewable publication fence as its maintenance
    /// pass. Each dispatched request is charged before sending; waiting does
    /// not hold the fence's read guard or prevent lease renewal.
    pub fn provider_budget(
        &self,
        store: Arc<dyn Store>,
    ) -> crate::metadata::budget::SharedProviderBudget {
        self.fence.as_ref().map(|fence| {
            Arc::new(FencedProviderBudget {
                store,
                fence: fence.clone(),
            }) as Arc<dyn crate::metadata::budget::ProviderBudget>
        })
    }

    pub async fn apply_identity_repair(
        &self,
        snapshot: &IdentityRepairSnapshot,
        plan: &IdentityRepairPlan,
    ) -> Result<IdentityRepairOutcome, StoreError> {
        let snapshot = snapshot.clone();
        let plan = plan.clone();
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .apply_identity_repair_fenced(&snapshot, &plan, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub fn unfenced(store: &'a dyn Store) -> Self {
        Self { store, fence: None }
    }

    pub fn fenced(store: &'a dyn Store, fence: PublicationFence) -> Self {
        Self {
            store,
            fence: Some(fence),
        }
    }

    /// Keep the domain heartbeat behind this read guard while the queue binds
    /// or completes its exact attempt. Catalogue writes use the same lock.
    pub async fn bind_library_job(
        &self,
        token: super::background_jobs::JobToken,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let guard = self.token().await?;
        let lease = guard.as_ref().ok_or_else(|| self.invalidated())?.clone();
        self.store
            .bind_library_job(super::background_jobs::BindLibraryJob {
                token,
                lease,
                now_ms,
            })
            .await
    }

    /// False requires reading the request result: a previous acknowledgement
    /// may have been lost, or either of the two owners may have changed.
    pub async fn complete_library_work(
        &self,
        token: super::background_jobs::JobToken,
        request_id: String,
        result: super::background_jobs_library::LibraryWorkResult,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let guard = self.token().await?;
        let lease = guard.as_ref().ok_or_else(|| self.invalidated())?.clone();
        self.store
            .complete_library_work(super::background_jobs::CompleteLibraryWork {
                token,
                lease,
                request_id,
                result,
                now_ms,
            })
            .await
    }

    pub fn raw(&self) -> &'a dyn Store {
        self.store
    }

    async fn token(&self) -> Result<tokio::sync::OwnedRwLockReadGuard<Option<Lease>>, StoreError> {
        let fence = self.fence.as_ref().ok_or_else(|| {
            StoreError::Task("fenced publication requested without a lease".to_owned())
        })?;
        if fence.revoked.load(Ordering::Acquire) {
            return Err(self.invalidated());
        }
        let token = Arc::clone(&fence.state).read_owned().await;
        if fence.revoked.load(Ordering::Acquire) {
            drop(token);
            return Err(self.invalidated());
        }
        Ok(token)
    }

    fn invalidated(&self) -> StoreError {
        let lease = self
            .fence
            .as_ref()
            .map(|fence| {
                fence
                    .last
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            })
            .expect("invalidated is called only for a fenced publisher");
        StoreError::FenceRejected {
            resource: lease.resource,
            owner_node_id: lease.owner_node_id,
            fence: lease.fence,
        }
    }

    async fn fenced_call<'call, T, F>(&'call self, operation: F) -> Result<T, StoreError>
    where
        F: FnOnce(Lease, Lease) -> FencedFuture<'call, T>,
    {
        let fence = self.fence.as_ref().ok_or_else(|| {
            StoreError::Task("fenced publication requested without a lease".to_owned())
        })?;
        if fence.revoked.load(Ordering::Acquire) {
            return Err(self.invalidated());
        }
        let mut state = fence.state.write().await;
        if fence.revoked.load(Ordering::Acquire) {
            return Err(self.invalidated());
        }
        let predecessor = state.as_ref().cloned().ok_or_else(|| self.invalidated())?;
        let now = unix_ms()?;
        if predecessor.expires_at_unix_ms <= now {
            *state = None;
            return Err(self.invalidated());
        }
        let replacement = predecessor.publication_successor()?;
        // The backend owns its operation deadline. An equal outer deadline
        // would drop the backend future with an unknown commit state (and a
        // SQLite blocking transaction cannot be cancelled at all).
        let result = operation(predecessor, replacement.clone()).await;
        match result {
            Ok(value) => {
                *fence
                    .last
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = replacement.clone();
                *state = Some(replacement);
                if fence.revoked.load(Ordering::Acquire) {
                    Err(self.invalidated())
                } else {
                    Ok(value)
                }
            }
            Err(error) => {
                *state = None;
                Err(error)
            }
        }
    }

    /// Return an immutable, content-addressed artwork filename after checking
    /// that a fenced caller still has publication authority. The database
    /// patch remains the publication point, while retries with identical
    /// bytes reuse one file instead of leaking one file per lease generation.
    pub async fn scoped_artwork_filename(
        &self,
        filename: &str,
        content: &[u8],
    ) -> Result<String, StoreError> {
        if self.fence.is_none() {
            return Ok(filename.to_owned());
        }
        let token = self.token().await?;
        let lease = token.as_ref().ok_or_else(|| self.invalidated())?;
        let now = unix_ms()?;
        if lease.expires_at_unix_ms
            <= now.saturating_add(PUBLICATION_CALL_SAFETY_WINDOW.as_millis() as i64)
        {
            return Err(StoreError::FenceRejected {
                resource: lease.resource.clone(),
                owner_node_id: lease.owner_node_id.clone(),
                fence: lease.fence,
            });
        }
        let digest = hex::encode(Sha256::digest(content));
        let path = std::path::Path::new(filename);
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| StoreError::Task("artwork filename has no safe stem".to_owned()))?;
        let extension = path.extension().and_then(|value| value.to_str());
        Ok(match extension {
            Some(extension) => format!("{stem}-c{}.{}", &digest[..16], extension),
            None => format!("{stem}-c{}", &digest[..16]),
        })
    }

    pub async fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        if self.fence.is_none() {
            return self.store.put_setting(key, value).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .put_setting_fenced(key, value, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn add_downloaded_subtitle(
        &self,
        file_id: i64,
        track: &crate::domain::DownloadedSubtitle,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self.store.add_downloaded_subtitle(file_id, track).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .add_downloaded_subtitle_fenced(file_id, track, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn put_setting_if_absent(&self, key: &str, value: &str) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self.store.put_setting_if_absent(key, value).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .put_setting_if_absent_fenced(key, value, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn put_setting_if_absent_if_artwork_repair_current(
        &self,
        key: &str,
        value: &str,
        expected_item_id: i64,
        repair_fence: &super::ArtworkRepairFence,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return Err(StoreError::Task(
                "artwork repair publication requires a singleton job lease".to_owned(),
            ));
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .put_setting_if_absent_if_artwork_repair_current_fenced(
                        key,
                        value,
                        expected_item_id,
                        repair_fence,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn mark_library_scanned(&self, id: i64, refreshed: bool) -> Result<(), StoreError> {
        if self.fence.is_none() {
            return self.store.mark_library_scanned(id, refreshed).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .mark_library_scanned_fenced(id, refreshed, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn insert_item(&self, item: &NewItem) -> Result<i64, StoreError> {
        if self.fence.is_none() {
            return self.store.insert_item(item).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .insert_item_fenced(item, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn apply_metadata(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
    ) -> Result<(), StoreError> {
        if self.fence.is_none() {
            return self.store.apply_metadata(item_id, patch).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .apply_metadata_fenced(item_id, patch, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn apply_series_tmdb_hint(
        &self,
        library_id: i64,
        show_id: i64,
        tmdb_id: i64,
    ) -> Result<SeriesHintOutcome, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .apply_series_tmdb_hint(library_id, show_id, tmdb_id)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .apply_series_tmdb_hint_fenced(
                        library_id,
                        show_id,
                        tmdb_id,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn apply_metadata_if_artwork_repair_current(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        repair_fence: &super::ArtworkRepairFence,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return Err(StoreError::Task(
                "artwork repair publication requires a singleton job lease".to_owned(),
            ));
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .apply_metadata_if_artwork_repair_current_fenced(
                        item_id,
                        patch,
                        repair_fence,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn apply_book_metadata(
        &self,
        item_id: i64,
        patch: &BookMetadataPatch,
    ) -> Result<(), StoreError> {
        if self.fence.is_none() {
            return self.store.apply_book_metadata(item_id, patch).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .apply_book_metadata_fenced(item_id, patch, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn apply_book_metadata_if_current(
        &self,
        expected: &crate::domain::Item,
        patch: &BookMetadataPatch,
        repair_fence: Option<&super::ArtworkRepairFence>,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            if repair_fence.is_some() {
                return Err(StoreError::Task(
                    "artwork repair publication requires a singleton job lease".to_owned(),
                ));
            }
            return self
                .store
                .apply_book_metadata_if_current(expected, patch, repair_fence)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .apply_book_metadata_if_current_fenced(
                        expected,
                        patch,
                        repair_fence,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn set_nfo_seeded(&self, item_id: i64) -> Result<(), StoreError> {
        if self.fence.is_none() {
            return self.store.set_nfo_seeded(item_id).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .set_nfo_seeded_fenced(item_id, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn get_dvr_recording(
        &self,
        id: &str,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError> {
        self.store.get_dvr_recording(id).await
    }

    /// Tell the DVR which item and file its finished capture became.
    ///
    /// Unfenced: this is a back-reference on a row the scan does not own,
    /// carrying no catalogue state, so there is nothing a stale publisher
    /// could overwrite. The fenced calls are the ones that write items and
    /// files.
    pub async fn link_dvr_recording_media(
        &self,
        recording_id: &str,
        item_id: i64,
        file_id: i64,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let event = crate::dvr::DvrEventInput {
            event_id: uuid::Uuid::new_v4().to_string(),
            kind: "media_linked".to_owned(),
            occurred_at_ms: now_ms,
            attempt: None,
            actor_user_id: None,
            reason_code: None,
            facts_json: serde_json::json!({"item_id": item_id, "file_id": file_id}).to_string(),
            actionable: false,
        };
        self.store
            .link_dvr_recording_media_with_event(recording_id, item_id, file_id, now_ms, &event)
            .await
    }

    /// Pure probe demand is tied to the current coordinator generation.
    pub async fn enqueue_probe(
        &self,
        file: &crate::domain::MediaFile,
        pipeline: &str,
    ) -> Result<Option<String>, StoreError> {
        use super::background_jobs::{EnqueueJob, EnqueueOutcome, JobPayload, JobRequest};
        use super::background_jobs_probe::{generation, ProbeCoordinator};
        if self.fence.is_none() {
            return Ok(None);
        }
        let guard = self.token().await?;
        let lease = guard.as_ref().ok_or_else(|| self.invalidated())?;
        let coordinator = ProbeCoordinator::from_lease(lease);
        if !coordinator.validate() {
            return Ok(None);
        }
        let generation = generation(file.id, file.size, file.mtime, &coordinator, pipeline);
        let now = unix_ms()?;
        let result = self
            .store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                dedupe_key: format!("probe:{generation}"),
                priority: 1,
                not_before_ms: now,
                now_ms: now,
                payload: JobPayload::MediaProbe {
                    file_id: file.id,
                    source_size: file.size,
                    source_mtime: file.mtime,
                    source_generation: generation.clone(),
                    probe_digest: pipeline.into(),
                    coordinator,
                },
                request: JobRequest {
                    scope: "probe".into(),
                    request_id: generation.clone(),
                    request_digest: generation,
                    consumer_kind: "probe".into(),
                    consumer_ref: file.id.to_string(),
                    target_node_id: None,
                    deadline_ms: Some(now.saturating_add(300_000)),
                    retain_identity: false,
                },
            })
            .await?;
        Ok(match result {
            EnqueueOutcome::Accepted { job_id, .. }
            | EnqueueOutcome::Existing {
                job_id,
                cancelled: false,
                ..
            } => Some(job_id),
            _ => None,
        })
    }

    pub async fn apply_probe(&self, job_id: &str) -> Result<bool, StoreError> {
        let guard = self.token().await?;
        let lease = guard.as_ref().ok_or_else(|| self.invalidated())?.clone();
        self.store
            .apply_probe_job(super::background_jobs_probe::ApplyProbeJob {
                job_id: job_id.into(),
                lease,
                now_ms: unix_ms()?,
            })
            .await
    }

    pub async fn upsert_file(
        &self,
        item_id: i64,
        path: &str,
        size: i64,
        mtime: i64,
        probe: &ProbeResult,
    ) -> Result<i64, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .upsert_file(item_id, path, size, mtime, probe)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .upsert_file_fenced(item_id, path, size, mtime, probe, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn ensure_library_root_fingerprint(
        &self,
        library_id: i64,
        fingerprint: &str,
        allow_establish: bool,
    ) -> Result<RootFingerprintStatus, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .ensure_library_root_fingerprint(library_id, fingerprint, allow_establish)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .ensure_library_root_fingerprint_fenced(
                        library_id,
                        fingerprint,
                        allow_establish,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn reconcile_library(
        &self,
        library_id: i64,
        root_fingerprint: &str,
        gone_file_ids: &[i64],
        prune_limit: u64,
    ) -> Result<ReconcileOutcome, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .reconcile_library(library_id, root_fingerprint, gone_file_ids, prune_limit)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .reconcile_library_fenced(
                        library_id,
                        root_fingerprint,
                        gone_file_ids,
                        prune_limit,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn claim_cache_entry(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        node_id: &str,
        relative_dir: &str,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .claim_cache_entry(recipe_hash, file_id, recipe_version, node_id, relative_dir)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .claim_cache_entry_fenced(
                        recipe_hash,
                        file_id,
                        recipe_version,
                        node_id,
                        relative_dir,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn touch_cache_claim(
        &self,
        recipe_hash: &str,
        node_id: &str,
    ) -> Result<(), StoreError> {
        if self.fence.is_none() {
            return self.store.touch_cache_claim(recipe_hash, node_id).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .touch_cache_claim_fenced(recipe_hash, node_id, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn complete_cache_entry(
        &self,
        recipe_hash: &str,
        node_id: &str,
        relative_dir: &str,
        bytes: i64,
        manifest_digest: Option<&str>,
    ) -> Result<(), StoreError> {
        // The digest has to reach both arms. This wrapper degrades to the
        // unfenced method when there is no fence, so a parameter added only to
        // the fenced one would be silently dropped for every unfenced caller —
        // which is most of them.
        if self.fence.is_none() {
            return self
                .store
                .complete_cache_entry(recipe_hash, node_id, bytes, manifest_digest)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .complete_cache_entry_fenced(
                        recipe_hash,
                        node_id,
                        relative_dir,
                        bytes,
                        manifest_digest,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn forget_cache_entry(
        &self,
        recipe_hash: &str,
        node_id: &str,
        storage_class: &str,
    ) -> Result<(), StoreError> {
        if self.fence.is_none() {
            self.store
                .forget_cache_entry(recipe_hash, node_id, storage_class)
                .await?;
            return Ok(());
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .forget_cache_entry_fenced(
                        recipe_hash,
                        node_id,
                        storage_class,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn mark_dv_conversion_running(
        &self,
        file_id: i64,
        bytes_before: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .mark_dv_conversion_running(file_id, bytes_before)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .mark_dv_conversion_running_fenced(file_id, bytes_before, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn mark_dv_conversion_verified(
        &self,
        file_id: i64,
        el_type: Option<&str>,
        bytes_after: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .mark_dv_conversion_verified(file_id, el_type, bytes_after)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .mark_dv_conversion_verified_fenced(
                        file_id,
                        el_type,
                        bytes_after,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn begin_dv_recovery_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        recovery_path: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .begin_dv_recovery_guard(file_id, guard_id, recovery_path, now_ms)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .begin_dv_recovery_guard_fenced(
                        file_id,
                        guard_id,
                        recovery_path,
                        now_ms,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn mark_dv_conversion_committed(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .mark_dv_conversion_committed(file_id, original_path, bytes_after, finished_at_ms)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .mark_dv_conversion_committed_fenced(
                        file_id,
                        original_path,
                        bytes_after,
                        finished_at_ms,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn mark_dv_conversion_committed_with_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .mark_dv_conversion_committed_with_guard(
                    file_id,
                    guard_id,
                    bytes_after,
                    finished_at_ms,
                )
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .mark_dv_conversion_committed_with_guard_fenced(
                        file_id,
                        guard_id,
                        bytes_after,
                        finished_at_ms,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn advance_dv_recovery_guard(
        &self,
        guard_id: &str,
        expected: DvRecoveryGuardState,
        next: DvRecoveryGuardState,
        updated_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .advance_dv_recovery_guard(guard_id, expected, next, updated_at_ms)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .advance_dv_recovery_guard_fenced(
                        guard_id,
                        expected,
                        next,
                        updated_at_ms,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn delete_dv_recovery_guard(&self, guard_id: &str) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self.store.delete_dv_recovery_guard(guard_id).await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .delete_dv_recovery_guard_fenced(guard_id, &lease, &replacement)
                    .await
            })
        })
        .await
    }

    pub async fn mark_dv_conversion_failed(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if self.fence.is_none() {
            return self
                .store
                .mark_dv_conversion_failed(file_id, error, finished_at_ms)
                .await;
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .mark_dv_conversion_failed_fenced(
                        file_id,
                        error,
                        finished_at_ms,
                        &lease,
                        &replacement,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn sync_predictions(
        &self,
        requests: Vec<super::NewAnalysisRequest>,
        desired_files: Vec<i64>,
    ) -> Result<(), StoreError> {
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .sync_predictions(super::background_jobs_predictions::SyncPredictions {
                        requests,
                        desired_files,
                        lease,
                        replacement,
                        now_ms: unix_ms()?,
                    })
                    .await
            })
        })
        .await
    }

    /// Admit one hot-copy interest under the discovery lease. Worker execution
    /// remains owned by the common queue, never by this planner lease.
    pub async fn enqueue_hot_copy(
        &self,
        request: super::background_jobs::EnqueueJob,
    ) -> Result<super::background_jobs::EnqueueOutcome, StoreError> {
        if request.request.scope != "automatic:hot-copy"
            || !matches!(
                request.payload,
                super::background_jobs::JobPayload::ArtifactHydrate { .. }
            )
        {
            return Err(StoreError::Task("invalid hot copy request".into()));
        }
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                self.store
                    .enqueue_job_fenced(request, lease, replacement)
                    .await
            })
        })
        .await
    }

    /// Admit a speculative request through the shared durable queue while
    /// preserving the singleton discovery publication fence.
    pub async fn enqueue_durable_pretranscode(
        &self,
        job: &NewPretranscodeJob,
    ) -> Result<bool, StoreError> {
        let request = super::background_jobs_pretranscode::enqueue_request(job)?;
        self.fenced_call(move |lease, replacement| {
            Box::pin(async move {
                let outcome = self
                    .store
                    .enqueue_job_fenced(request, lease, replacement)
                    .await?;
                Ok(matches!(
                    outcome,
                    super::background_jobs::EnqueueOutcome::Accepted { .. }
                ))
            })
        })
        .await
    }
}

impl Deref for PublicationStore<'_> {
    type Target = dyn Store;

    fn deref(&self) -> &Self::Target {
        self.store
    }
}
