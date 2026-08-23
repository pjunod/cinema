//! Verified optional shared-cache mount and fenced reclamation.
//!
//! A configured path is never treated as shared by itself. Every voter proves
//! that it can read a peer-created canary and that the peer can read its own
//! response through the same mount. Failure disables the fast path
//! immediately; ordinary node-local cache routing remains available.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plurx_core::cluster::coordination::LeaseClaim;
use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::CacheStorageMember;
use plurx_core::fs_secure::SecureDirectory;
use plurx_core::store::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::peer_transport::{PeerAuthMode, PeerTransport};

pub(crate) const CANARY_PATH: &str = "/api/v1/internal/media/shared-cache-canary";
pub(crate) const MAX_CANARY_REQUEST_BYTES: usize = 1_024;
pub(crate) const MAX_CANARY_RESPONSE_BYTES: usize = 512;
const CANARY_DIRECTORY: &str = ".plurx-canary";
const CANARY_BYTES: u64 = 128;
const PROBE_DEADLINE: Duration = Duration::from_secs(3);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const CANARY_ORPHAN_AGE: Duration = Duration::from_secs(30);
const CANARY_SWEEP_LIMIT: usize = 64;
const GC_INTERVAL: Duration = Duration::from_secs(60);
const GC_LEASE_MS: i64 = 30_000;
const GC_BATCH: i64 = 32;
const PIN_PRUNE_BATCH: i64 = 1_024;
const GC_MIN_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const STALE_PUBLICATION_MS: i64 = 60 * 60 * 1_000;
const MOUNT_IO_DEADLINE: Duration = Duration::from_secs(10);
const MOUNT_IO_CONCURRENCY: usize = 8;
const MOUNT_PUBLICATION_CONCURRENCY: usize = 2;

#[derive(Clone, Debug)]
struct SharedConfig {
    configured_root: PathBuf,
    storage_id: String,
}

#[derive(Clone, Debug)]
struct VerifiedRoot {
    path: PathBuf,
    identity: plurx_core::fs_secure::FileIdentity,
}

#[derive(Clone)]
pub(crate) struct SharedCacheCoordinator {
    config: Option<SharedConfig>,
    verified_root: Arc<tokio::sync::RwLock<Option<VerifiedRoot>>>,
    verified: Arc<AtomicBool>,
    loss_generation: Arc<AtomicU64>,
    admitted_generation: Arc<AtomicU64>,
    suspect_report_pending: Arc<AtomicBool>,
    verification_transition: Arc<tokio::sync::Mutex<()>>,
    publication_commit: Arc<tokio::sync::RwLock<()>>,
    mount_io: Arc<tokio::sync::Semaphore>,
    mount_publications: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    publication_install_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    publication_commit_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    node_id: String,
    membership: MembershipManager,
    transport: PeerTransport,
    store: Arc<dyn Store>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanaryRequest {
    pub storage_id: String,
    pub canary_name: String,
    pub canary_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanaryResponse {
    pub storage_id: String,
    pub response_name: String,
    pub response_digest: String,
}

impl SharedCacheCoordinator {
    pub(crate) fn new(
        configured_root: PathBuf,
        shared_id: String,
        cluster_id: &str,
        node_id: String,
        membership: MembershipManager,
        store: Arc<dyn Store>,
    ) -> Arc<Self> {
        let config =
            (!configured_root.as_os_str().is_empty() && !shared_id.is_empty()).then(|| {
                SharedConfig {
                    configured_root,
                    storage_id: storage_id(cluster_id, &shared_id),
                }
            });
        Arc::new(Self {
            config,
            verified_root: Arc::new(tokio::sync::RwLock::new(None)),
            verified: Arc::new(AtomicBool::new(false)),
            loss_generation: Arc::new(AtomicU64::new(0)),
            admitted_generation: Arc::new(AtomicU64::new(u64::MAX)),
            suspect_report_pending: Arc::new(AtomicBool::new(false)),
            verification_transition: Arc::new(tokio::sync::Mutex::new(())),
            publication_commit: Arc::new(tokio::sync::RwLock::new(())),
            mount_io: Arc::new(tokio::sync::Semaphore::new(MOUNT_IO_CONCURRENCY)),
            mount_publications: Arc::new(tokio::sync::Semaphore::new(
                MOUNT_PUBLICATION_CONCURRENCY,
            )),
            #[cfg(test)]
            publication_install_pause: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            publication_commit_pause: Arc::new(std::sync::Mutex::new(None)),
            node_id,
            transport: PeerTransport::new(membership.clone()),
            membership,
            store,
        })
    }

    pub(crate) fn storage_id(&self) -> Option<&str> {
        self.config
            .as_ref()
            .map(|config| config.storage_id.as_str())
    }

    pub(crate) fn is_verified(&self) -> bool {
        self.verified.load(Ordering::Acquire)
            && self.admitted_generation.load(Ordering::Acquire)
                == self.loss_generation.load(Ordering::Acquire)
    }

    fn generation_is_admitted(&self, generation: u64) -> bool {
        self.verified.load(Ordering::Acquire)
            && self.admitted_generation.load(Ordering::Acquire) == generation
            && self.loss_generation.load(Ordering::Acquire) == generation
    }

    /// Run mount-backed work behind a fixed permit pool and a hard caller
    /// deadline. A timed-out task is deliberately detached while retaining
    /// its permit until the kernel operation actually returns; a wedged NAS
    /// can therefore consume at most `MOUNT_IO_CONCURRENCY` tasks.
    pub(crate) async fn run_mount_io<T, F>(
        &self,
        reason: &'static str,
        future: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        let permit = match Arc::clone(&self.mount_io).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.report_io_failure("mount_io_capacity_exhausted").await;
                return Err("shared cache mount I/O capacity is exhausted".to_owned());
            }
        };
        let mut task = tokio::spawn(async move {
            let _permit = permit;
            future.await
        });
        match tokio::time::timeout(MOUNT_IO_DEADLINE, &mut task).await {
            Ok(result) => result.map_err(|error| format!("shared mount task failed: {error}"))?,
            Err(_) => {
                self.report_io_failure(reason).await;
                Err("shared cache mount I/O exceeded its hard deadline".to_owned())
            }
        }
    }

    pub(crate) async fn root(&self) -> Option<PathBuf> {
        if !self.is_verified() {
            return None;
        }
        self.verified_root
            .read()
            .await
            .as_ref()
            .map(|root| root.path.clone())
    }

    /// Reopen the admitted root and prove that the pathname still names the
    /// same filesystem object. Publication and GC mutate portable state, so
    /// merely retaining the configured spelling after an unmount is unsafe.
    async fn open_verified_root(&self) -> Result<Option<(PathBuf, SecureDirectory, u64)>, String> {
        if !self.is_verified() {
            return Ok(None);
        }
        let admitted = self
            .verified_root
            .read()
            .await
            .clone()
            .ok_or_else(|| "verified shared cache has no admitted root".to_owned())?;
        let root = match SecureDirectory::open(&admitted.path).await {
            Ok(root) => root,
            Err(error) => {
                self.report_io_failure("shared_root_open_failed").await;
                return Err(format!("opening admitted shared cache root: {error}"));
            }
        };
        let observed = match root.identity().await {
            Ok(identity) => identity,
            Err(error) => {
                self.report_io_failure("shared_root_identity_failed").await;
                return Err(format!("identifying admitted shared cache root: {error}"));
            }
        };
        if !observed.same_inode(admitted.identity) {
            self.report_io_failure("shared_root_identity_changed").await;
            return Err("shared cache root identity changed after admission".to_owned());
        }
        let admitted_generation = self.admitted_generation.load(Ordering::Acquire);
        if !self.generation_is_admitted(admitted_generation) {
            return Ok(None);
        }
        Ok(Some((admitted.path, root, admitted_generation)))
    }

    #[cfg(test)]
    pub(crate) async fn admit_local_for_test(&self) -> Result<(), String> {
        let attempted_generation = self.loss_generation.load(Ordering::Acquire);
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let (path, identity, canaries) = prepare_canary_directory(&config.configured_root).await?;
        let name = canary_name("single");
        let bytes = random_canary_bytes();
        canaries
            .atomic_write_child(&name, &bytes)
            .await
            .map_err(|error| error.to_string())?;
        let observed = canaries
            .read_bounded_child(&name, CANARY_BYTES)
            .await
            .map_err(|error| error.to_string())?;
        let _ = canaries.unlink_child(&name).await;
        if observed != bytes {
            return Err("test shared-cache canary changed while read".to_owned());
        }
        self.publish_verified_root(path, identity, attempted_generation)
            .await
    }

    async fn publish_verified_root(
        &self,
        path: PathBuf,
        identity: plurx_core::fs_secure::FileIdentity,
        attempted_generation: u64,
    ) -> Result<(), String> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let _transition = self.verification_transition.lock().await;
        if self.loss_generation.load(Ordering::Acquire) != attempted_generation {
            return Err("shared cache proof was superseded by a newer I/O failure".to_owned());
        }
        if self.suspect_report_pending.load(Ordering::Acquire) {
            return Err("shared cache loss is still being recorded".to_owned());
        }
        self.store
            .put_cache_storage_member(&CacheStorageMember {
                storage_id: config.storage_id.clone(),
                node_id: self.node_id.clone(),
                storage_class: "shared".to_owned(),
                verified_at_ms: unix_ms(),
                verification_state: "verified".to_owned(),
            })
            .await
            .map_err(|error| error.to_string())?;
        if self.loss_generation.load(Ordering::Acquire) != attempted_generation
            || self.suspect_report_pending.load(Ordering::Acquire)
        {
            return Err("shared cache proof was superseded while publishing".to_owned());
        }
        *self.verified_root.write().await = Some(VerifiedRoot { path, identity });
        self.admitted_generation
            .store(attempted_generation, Ordering::Release);
        self.verified.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn report_io_failure(&self, reason: &'static str) {
        // Revoke local serving authority before any lock or Store wait. The
        // admitted generation is part of every `is_verified` verdict, so an
        // older proof cannot reopen the race even if it completes after this
        // increment. Durable membership telemetry is serialized in a bounded
        // background task; a wedged Store can never extend a media caller's
        // mount-I/O deadline or make callers pile up behind the transition.
        self.loss_generation.fetch_add(1, Ordering::AcqRel);
        let was_verified = self.verified.swap(false, Ordering::AcqRel);
        let Some(config) = self.config.as_ref() else {
            return;
        };
        if was_verified {
            tracing::warn!(
                storage_id = %config.storage_id,
                reason,
                "shared cache proof lost; falling back to node-local holders"
            );
        }
        if self
            .suspect_report_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let transition = Arc::clone(&self.verification_transition);
        let publication_commit = Arc::clone(&self.publication_commit);
        let pending = Arc::clone(&self.suspect_report_pending);
        let store = Arc::clone(&self.store);
        let storage_id = config.storage_id.clone();
        let node_id = self.node_id.clone();
        tokio::spawn(async move {
            // A node-scoped mount loss revokes local access immediately, but
            // its durable suspect record must be ordered after any global
            // generation whose completion already began. Healthy peers may
            // continue serving those immutable bytes.
            let _publication_commit = publication_commit.write().await;
            let _transition = transition.lock().await;
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                store.mark_cache_storage_suspect(&storage_id, &node_id, unix_ms()),
            )
            .await;
            pending.store(false, Ordering::Release);
        });
    }

    pub(crate) async fn run(self: Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        if self.config.is_none() {
            return;
        }
        let mut probes = tokio::time::interval(PROBE_INTERVAL);
        probes.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut gc = tokio::time::interval(GC_INTERVAL);
        gc.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Do not let GC's immediate first tick race the initial mount proof.
        gc.tick().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = probes.tick() => {
                    if let Err(error) = self.verify_once().await {
                        self.report_io_failure("canary_failed").await;
                        tracing::debug!(%error, "shared cache canary proof failed");
                    }
                }
                _ = gc.tick() => {
                    if let Err(error) = self.gc_once().await {
                        tracing::debug!(%error, "shared cache GC pass unavailable");
                    }
                }
            }
        }
    }

    async fn verify_once(&self) -> Result<(), String> {
        let coordinator = self.clone();
        self.run_mount_io("canary_mount_timeout", async move {
            coordinator.verify_once_inner().await
        })
        .await
    }

    async fn verify_once_inner(&self) -> Result<(), String> {
        let attempted_generation = self.loss_generation.load(Ordering::Acquire);
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let (path, identity, canaries) = prepare_canary_directory(&config.configured_root).await?;
        let voter_count = self
            .membership
            .activity_voter_count()
            .await
            .map_err(|error| error.to_string())?;
        let peers = self
            .membership
            .activity_peers()
            .await
            .map_err(|error| error.to_string())?;
        if peers.len().saturating_add(1) != voter_count {
            return Err("the complete voter directory is not available".to_owned());
        }
        if voter_count == 1 {
            let name = canary_name("single");
            let bytes = random_canary_bytes();
            canaries
                .atomic_write_child(&name, &bytes)
                .await
                .map_err(|error| error.to_string())?;
            let observed = canaries
                .read_bounded_child(&name, CANARY_BYTES)
                .await
                .map_err(|error| error.to_string())?;
            let _ = canaries.unlink_child(&name).await;
            if observed != bytes {
                return Err("single-voter canary changed while read".to_owned());
            }
        } else {
            let mut proofs = Vec::with_capacity(peers.len());
            for peer in &peers {
                if !peer.reachable {
                    return Err(format!(
                        "shared-cache peer {} is not reachable",
                        peer.node_id
                    ));
                }
                let base = peer.http_base.as_deref().ok_or_else(|| {
                    format!("shared-cache peer {} has no HTTP base", peer.node_id)
                })?;
                proofs.push(self.verify_peer(&config.storage_id, &canaries, &peer.node_id, base));
            }
            // Every voter has the same bounded proof deadline. Running the
            // independent canaries together keeps admission latency bounded
            // by the slowest voter rather than multiplying it by cluster
            // size.
            for proof in futures_util::future::join_all(proofs).await {
                proof?;
            }
        }
        self.publish_verified_root(path, identity, attempted_generation)
            .await
    }

    async fn verify_peer(
        &self,
        storage_id: &str,
        canaries: &SecureDirectory,
        peer_node_id: &str,
        peer_base: &str,
    ) -> Result<(), String> {
        let canary_name = canary_name("probe");
        let canary_bytes = random_canary_bytes();
        canaries
            .atomic_write_child(&canary_name, &canary_bytes)
            .await
            .map_err(|error| error.to_string())?;
        let request = CanaryRequest {
            storage_id: storage_id.to_owned(),
            canary_name: canary_name.clone(),
            canary_digest: digest(&canary_bytes),
        };
        let body = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        let deadline = tokio::time::Instant::now() + PROBE_DEADLINE;
        let response = self
            .transport
            .request(
                peer_node_id,
                peer_base,
                reqwest::Method::POST,
                CANARY_PATH,
                body,
                deadline,
                MAX_CANARY_RESPONSE_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await;
        // The origin owns this probe file. Remove it on timeout/auth/network
        // failure too, otherwise a degraded peer can accumulate one orphan
        // every probe interval indefinitely.
        let _ = canaries.unlink_child(&canary_name).await;
        let response =
            response.map_err(|error| format!("peer canary request failed: {error:?}"))?;
        if !response.status.is_success() {
            return Err(format!("peer canary returned {}", response.status));
        }
        let response = serde_json::from_slice::<CanaryResponse>(&response.body)
            .map_err(|error| error.to_string())?;
        if response.storage_id != storage_id || !safe_canary_name(&response.response_name, "reply")
        {
            return Err("peer returned an invalid canary identity".to_owned());
        }
        let response_bytes = canaries
            .read_bounded_child(&response.response_name, CANARY_BYTES)
            .await
            .map_err(|error| error.to_string())?;
        let _ = canaries.unlink_child(&response.response_name).await;
        if digest(&response_bytes) != response.response_digest {
            return Err("peer response canary was not visible on this mount".to_owned());
        }
        Ok(())
    }

    pub(crate) async fn answer_canary(
        &self,
        request: &CanaryRequest,
    ) -> Result<CanaryResponse, String> {
        let coordinator = self.clone();
        let request = request.clone();
        self.run_mount_io("canary_reply_mount_timeout", async move {
            coordinator.answer_canary_inner(&request).await
        })
        .await
    }

    async fn answer_canary_inner(&self, request: &CanaryRequest) -> Result<CanaryResponse, String> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        if request.storage_id != config.storage_id
            || !safe_canary_name(&request.canary_name, "probe")
            || request.canary_digest.len() != 64
            || !request
                .canary_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("shared cache canary identity does not match".to_owned());
        }
        let (_, _, canaries) = prepare_canary_directory(&config.configured_root).await?;
        let bytes = canaries
            .read_bounded_child(&request.canary_name, CANARY_BYTES)
            .await
            .map_err(|error| error.to_string())?;
        if digest(&bytes) != request.canary_digest {
            return Err("request canary is not visible on this mount".to_owned());
        }
        let response_name = canary_name("reply");
        let response_bytes = random_canary_bytes();
        canaries
            .atomic_write_child(&response_name, &response_bytes)
            .await
            .map_err(|error| error.to_string())?;
        // The requester normally removes this response after proving it. A
        // lost HTTP response must not leave one file per probe forever.
        let cleanup_directory = canaries.clone();
        let cleanup_name = response_name.clone();
        let cleanup_coordinator = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANARY_ORPHAN_AGE).await;
            let _ = cleanup_coordinator
                .run_mount_io("canary_cleanup_timeout", async move {
                    cleanup_directory
                        .unlink_child(&cleanup_name)
                        .await
                        .map_err(|error| error.to_string())
                })
                .await;
        });
        Ok(CanaryResponse {
            storage_id: config.storage_id.clone(),
            response_name,
            response_digest: digest(&response_bytes),
        })
    }

    #[allow(clippy::too_many_arguments)] // one exact cache generation identity and its two paths
    async fn abandon_publication(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
        fanout: &SecureDirectory,
        staging_name: &str,
        final_name: &str,
        max_entries: usize,
    ) -> Result<bool, String> {
        if !self
            .store
            .abandon_shared_cache_entry(recipe_hash, storage_id, generation_id, relative_dir)
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(false);
        }

        for name in [staging_name, final_name] {
            remove_shared_tree_if_present(fanout, name, max_entries).await?;
        }
        self.store
            .finalize_abandoned_shared_cache_entry(
                recipe_hash,
                storage_id,
                generation_id,
                relative_dir,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    /// Publish an already fenced local generation into the verified shared
    /// root. The replicated pointer is claimed before any byte becomes
    /// visible and completed only after every copied object re-verifies
    /// against the immutable manifest.
    pub(crate) async fn publish_generation(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        source_dir: &Path,
        manifest: &plurx_core::transcode::manifest::GenerationManifest,
    ) -> Result<bool, String> {
        // Whole-title publication is intentionally not a request-sized mount
        // operation: a correct multi-gigabyte copy may take far longer than
        // ten seconds. Keep it in its own small pool so two slow publications
        // cannot consume the permits reserved for playlist, segment, offer,
        // canary, and GC latency boundaries.
        let permit = match Arc::clone(&self.mount_publications).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(false),
        };
        let coordinator = self.clone();
        let recipe_hash = recipe_hash.to_owned();
        let source_dir = source_dir.to_owned();
        let manifest = manifest.clone();
        // The task, not its caller, owns the permit. Dropping a request future
        // therefore detaches at most one still-counted publication instead of
        // freeing capacity while descriptor-backed blocking copies continue.
        tokio::spawn(async move {
            let _permit = permit;
            coordinator
                .publish_generation_inner(
                    &recipe_hash,
                    file_id,
                    recipe_version,
                    &source_dir,
                    &manifest,
                )
                .await
        })
        .await
        .map_err(|error| format!("shared publication task failed: {error}"))?
    }

    async fn publish_generation_inner(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        source_dir: &Path,
        manifest: &plurx_core::transcode::manifest::GenerationManifest,
    ) -> Result<bool, String> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let Some((root_path, root, admitted_generation)) = self.open_verified_root().await? else {
            return Ok(false);
        };
        let short_hash = recipe_hash
            .get(..16)
            .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| "shared cache recipe has no safe generation prefix".to_owned())?;
        let prefix = &short_hash[..2];
        let fanout = match root.open_child_directory(prefix).await {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match root.create_child_directory(prefix).await {
                    Ok(directory) => directory,
                    Err(error) => {
                        self.report_io_failure("shared_publish_fanout_failed").await;
                        return Err(format!("creating shared cache fanout: {error}"));
                    }
                }
            }
            Err(error) => {
                self.report_io_failure("shared_publish_fanout_failed").await;
                return Err(format!("opening shared cache fanout: {error}"));
            }
        };
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let staging_name = format!(".stage-{nonce}");
        let final_name = format!("{short_hash}-{nonce}");
        let relative_dir = format!("{prefix}/{final_name}");
        let publication_entries = manifest.objects.len().saturating_add(1);
        let claimed = match self
            .store
            .claim_shared_cache_entry(
                recipe_hash,
                file_id,
                recipe_version,
                &config.storage_id,
                &manifest.generation_id,
                &relative_dir,
                unix_ms(),
            )
            .await
        {
            Ok(claimed) => claimed,
            Err(error) => {
                let _ = self
                    .abandon_publication(
                        recipe_hash,
                        &config.storage_id,
                        &manifest.generation_id,
                        &relative_dir,
                        &fanout,
                        &staging_name,
                        &final_name,
                        publication_entries,
                    )
                    .await;
                return Err(error.to_string());
            }
        };
        if !claimed {
            return Ok(false);
        }

        // Claim before creating bytes. A crash can now leave only a bounded,
        // discoverable incomplete row; no untracked `.stage-*` directory is
        // created before durable ownership exists.
        let staging = match fanout.create_child_directory(&staging_name).await {
            Ok(staging) => staging,
            Err(error) => {
                let _ = self
                    .abandon_publication(
                        recipe_hash,
                        &config.storage_id,
                        &manifest.generation_id,
                        &relative_dir,
                        &fanout,
                        &staging_name,
                        &final_name,
                        publication_entries,
                    )
                    .await;
                self.report_io_failure("shared_publish_staging_failed")
                    .await;
                return Err(format!("creating shared generation staging: {error}"));
            }
        };

        let source = match SecureDirectory::open(source_dir).await {
            Ok(source) => source,
            Err(error) => {
                let _ = self
                    .abandon_publication(
                        recipe_hash,
                        &config.storage_id,
                        &manifest.generation_id,
                        &relative_dir,
                        &fanout,
                        &staging_name,
                        &final_name,
                        publication_entries,
                    )
                    .await;
                return Err(format!(
                    "opening local generation for shared publication: {error}"
                ));
            }
        };
        let staging_path = root_path.join(prefix).join(&staging_name);
        let publication = async {
            let mut total_bytes = 0_u64;
            for object in &manifest.objects {
                let placed = staging
                    .place_regular_child_from(&source, &object.name, &object.name, object.bytes)
                    .await
                    .map_err(|error| {
                        (
                            format!("copying shared generation object {}: {error}", object.name),
                            true,
                        )
                    })?;
                if placed != object.bytes {
                    return Err((
                        format!("shared generation object {} changed size", object.name),
                        true,
                    ));
                }
                total_bytes = total_bytes.saturating_add(placed);
            }
            let manifest_bytes = staging
                .place_regular_child_from(
                    &source,
                    plurx_core::transcode::manifest::MANIFEST_FILE,
                    plurx_core::transcode::manifest::MANIFEST_FILE,
                    plurx_core::transcode::manifest::MAX_MANIFEST_BYTES,
                )
                .await
                .map_err(|error| (format!("copying shared generation manifest: {error}"), true))?;
            total_bytes = total_bytes.saturating_add(manifest_bytes);
            let copied_manifest = plurx_core::transcode::manifest::load(&staging_path)
                .await
                .map_err(|error| (format!("authenticating shared generation: {error}"), true))?;
            if copied_manifest != *manifest {
                return Err((
                    "shared generation manifest changed while copied".to_owned(),
                    true,
                ));
            }
            for object in &manifest.objects {
                if !manifest
                    .verify_object(&staging_path, &object.name)
                    .await
                    .map_err(|error| {
                        (
                            format!(
                                "verifying shared generation object {}: {error}",
                                object.name
                            ),
                            true,
                        )
                    })?
                {
                    return Err((
                        format!("shared generation object {} failed its digest", object.name),
                        true,
                    ));
                }
            }
            #[cfg(test)]
            {
                let pause = self
                    .publication_install_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(pause) = pause {
                    pause.wait().await;
                    pause.wait().await;
                }
            }
            if !self.generation_is_admitted(admitted_generation) {
                return Err((
                    "shared cache authority changed before generation installation".to_owned(),
                    false,
                ));
            }
            if !fanout
                .rename_child_noreplace(&staging_name, &final_name)
                .await
                .map_err(|error| (format!("installing shared generation: {error}"), true))?
            {
                return Err((
                    "shared generation destination already exists".to_owned(),
                    true,
                ));
            }
            if !self.generation_is_admitted(admitted_generation) {
                return Err((
                    "shared cache authority changed during generation installation".to_owned(),
                    false,
                ));
            }
            Ok::<u64, (String, bool)>(total_bytes)
        }
        .await;
        let total_bytes = match publication {
            Ok(total_bytes) => total_bytes,
            Err((error, io_failure)) => {
                let _ = self
                    .abandon_publication(
                        recipe_hash,
                        &config.storage_id,
                        &manifest.generation_id,
                        &relative_dir,
                        &fanout,
                        &staging_name,
                        &final_name,
                        publication_entries,
                    )
                    .await;
                if io_failure {
                    self.report_io_failure("shared_publish_failed").await;
                }
                return Err(error);
            }
        };

        let _publication_commit = self.publication_commit.read().await;
        if !self.generation_is_admitted(admitted_generation) {
            let _ = self
                .abandon_publication(
                    recipe_hash,
                    &config.storage_id,
                    &manifest.generation_id,
                    &relative_dir,
                    &fanout,
                    &staging_name,
                    &final_name,
                    publication_entries,
                )
                .await;
            return Ok(false);
        }

        #[cfg(test)]
        {
            let pause = self
                .publication_commit_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }

        let completion = self
            .store
            .complete_shared_cache_entry(
                recipe_hash,
                &config.storage_id,
                &manifest.generation_id,
                total_bytes.min(i64::MAX as u64) as i64,
                &manifest.manifest_digest,
                unix_ms(),
            )
            .await;
        let completion_error = match completion {
            Ok(true) => return Ok(true),
            Ok(false) => "shared cache claim changed before completion".to_owned(),
            Err(error) => error.to_string(),
        };

        // Completion can be commit-unknown. Fencing the exact incomplete row
        // into state 2 wins exclusive cleanup authority. If no incomplete row
        // remains, preserve bytes until a consistent read proves whether this
        // exact generation became authoritative.
        match self
            .abandon_publication(
                recipe_hash,
                &config.storage_id,
                &manifest.generation_id,
                &relative_dir,
                &fanout,
                &staging_name,
                &final_name,
                publication_entries,
            )
            .await
        {
            Ok(true) => Err(completion_error),
            Ok(false) => match self
                .store
                .shared_cache_hit(recipe_hash, &config.storage_id)
                .await
            {
                Ok(Some(current))
                    if current.generation_id == manifest.generation_id
                        && current.relative_dir == relative_dir
                        && current.manifest_digest.as_deref()
                            == Some(manifest.manifest_digest.as_str()) =>
                {
                    Ok(true)
                }
                Ok(_) => Err(completion_error),
                Err(reconcile_error) => Err(format!(
                    "{completion_error}; shared completion reconciliation remains unresolved: {reconcile_error}"
                )),
            },
            Err(abandon_error) => Err(format!(
                "{completion_error}; exact shared claim settlement remains unresolved: {abandon_error}"
            )),
        }
    }

    async fn reconcile_stale_publication(
        &self,
        root: &SecureDirectory,
        generation: &plurx_core::domain::SharedCacheGeneration,
    ) -> Result<(), String> {
        let components = shared_generation_components(&generation.relative_dir)
            .ok_or_else(|| "stale shared publication has an invalid relative path".to_owned())?;
        let final_name = components.last().expect("bounded non-empty components");
        let staging_name = staging_name_for_generation(final_name)
            .ok_or_else(|| "stale shared publication has an invalid generation name".to_owned())?;
        if !self
            .store
            .abandon_shared_cache_entry(
                &generation.recipe_hash,
                &generation.storage_id,
                &generation.generation_id,
                &generation.relative_dir,
            )
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }

        let mut parent = root.clone();
        for component in &components[..components.len().saturating_sub(1)] {
            parent = match parent.open_child_directory(component).await {
                Ok(directory) => directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.store
                        .finalize_abandoned_shared_cache_entry(
                            &generation.recipe_hash,
                            &generation.storage_id,
                            &generation.generation_id,
                            &generation.relative_dir,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                Err(error) => return Err(error.to_string()),
            };
        }
        let max_entries = plurx_core::transcode::manifest::MAX_OBJECTS.saturating_add(1);
        remove_shared_tree_if_present(&parent, &staging_name, max_entries).await?;
        remove_shared_tree_if_present(&parent, final_name, max_entries).await?;
        self.store
            .finalize_abandoned_shared_cache_entry(
                &generation.recipe_hash,
                &generation.storage_id,
                &generation.generation_id,
                &generation.relative_dir,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn reclaim_retired_generation(
        &self,
        root: &SecureDirectory,
        generation: &plurx_core::domain::SharedCacheGeneration,
        lease: &plurx_core::cluster::coordination::Lease,
    ) -> Result<Option<plurx_core::cluster::coordination::Lease>, String> {
        let components = shared_generation_components(&generation.relative_dir)
            .ok_or_else(|| "shared GC generation has an invalid relative path".to_owned())?;
        let generation_name = components.last().expect("bounded non-empty components");
        let quarantine = format!(".delete-{generation_name}");
        let mut parent = root.clone();
        for component in &components[..components.len().saturating_sub(1)] {
            parent = match parent.open_child_directory(component).await {
                Ok(directory) => directory,
                Err(error)
                    if generation.cleanup_pending
                        && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    let successor = self
                        .store
                        .finalize_retired_shared_cache_generation(generation, unix_ms(), lease)
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(successor);
                }
                Err(error) => return Err(format!("opening shared GC parent: {error}")),
            };
        }

        let (generation_capability, already_quarantined) = match parent
            .open_child_directory(generation_name)
            .await
        {
            Ok(directory) => (directory, false),
            Err(error)
                if generation.cleanup_pending && error.kind() == std::io::ErrorKind::NotFound =>
            {
                match parent.open_child_directory(&quarantine).await {
                    Ok(directory) => (directory, true),
                    Err(quarantine_error)
                        if quarantine_error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        let successor = self
                            .store
                            .finalize_retired_shared_cache_generation(generation, unix_ms(), lease)
                            .await
                            .map_err(|error| error.to_string())?;
                        return Ok(successor);
                    }
                    Err(quarantine_error) => {
                        return Err(format!("opening shared GC quarantine: {quarantine_error}"));
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "opening complete shared generation before retirement: {error}"
                ));
            }
        };
        let generation_identity = generation_capability
            .identity()
            .await
            .map_err(|error| format!("identifying shared GC generation: {error}"))?;
        let Some(successor) = self
            .store
            .retire_shared_cache_generation(generation, unix_ms(), lease)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };

        if !already_quarantined {
            if successor.expires_at_unix_ms <= unix_ms() || !self.is_verified() {
                return Ok(None);
            }
            match parent
                .rename_child_noreplace(generation_name, &quarantine)
                .await
            {
                Ok(true) => {}
                Ok(false) => return Err("shared GC quarantine already exists".to_owned()),
                Err(error) => return Err(format!("quarantining shared generation: {error}")),
            }
            let moved_directory = parent
                .open_child_directory(&quarantine)
                .await
                .map_err(|error| format!("opening quarantined shared generation: {error}"))?;
            let moved_identity = moved_directory
                .identity()
                .await
                .map_err(|error| format!("identifying quarantined shared generation: {error}"))?;
            if !moved_identity.same_inode(generation_identity) {
                return Err("shared GC quarantine identity changed".to_owned());
            }
        }
        if successor.expires_at_unix_ms <= unix_ms() || !self.is_verified() {
            return Ok(None);
        }
        parent
            .remove_child_tree(
                &quarantine,
                plurx_core::transcode::manifest::MAX_OBJECTS.saturating_add(1),
                1,
            )
            .await
            .map_err(|error| format!("deleting quarantined shared generation: {error}"))?;
        let finalized = self
            .store
            .finalize_retired_shared_cache_generation(generation, unix_ms(), &successor)
            .await
            .map_err(|error| error.to_string())?;
        Ok(finalized)
    }

    async fn gc_once(&self) -> Result<(), String> {
        let coordinator = self.clone();
        self.run_mount_io("shared_gc_timeout", async move {
            coordinator.gc_once_inner().await
        })
        .await
    }

    async fn gc_once_inner(&self) -> Result<(), String> {
        let Some(config) = self.config.as_ref() else {
            return Ok(());
        };
        let Some((_root_path, root, _admitted_generation)) = self.open_verified_root().await?
        else {
            return Ok(());
        };
        let now_ms = unix_ms();
        let resource = format!("shared-cache-gc:{}", config.storage_id);
        let mut lease = match self
            .store
            .acquire_lease(
                &resource,
                &self.node_id,
                now_ms,
                now_ms.saturating_add(GC_LEASE_MS),
            )
            .await
            .map_err(|error| error.to_string())?
        {
            LeaseClaim::Acquired(lease) => lease,
            LeaseClaim::Held { .. } => return Ok(()),
        };
        self.store
            .prune_expired_cache_consumer_pins(&config.storage_id, now_ms, PIN_PRUNE_BATCH)
            .await
            .map_err(|error| error.to_string())?;
        let stale_claims = self
            .store
            .stale_shared_cache_claims(
                &config.storage_id,
                now_ms.saturating_sub(STALE_PUBLICATION_MS),
                GC_BATCH,
            )
            .await
            .map_err(|error| error.to_string())?;
        for generation in stale_claims {
            if let Err(error) = self.reconcile_stale_publication(&root, &generation).await {
                self.report_io_failure("stale_publication_cleanup_failed")
                    .await;
                tracing::warn!(
                    storage_id = %generation.storage_id,
                    recipe = %generation.recipe_hash,
                    %error,
                    "shared cache stale publication cleanup remains retryable"
                );
                break;
            }
        }
        if !self.is_verified() {
            let _ = self.store.release_lease(&lease, unix_ms()).await;
            return Ok(());
        }
        let candidates = self
            .store
            .shared_cache_gc_candidates(&config.storage_id, now_ms, GC_BATCH)
            .await
            .map_err(|error| error.to_string())?;
        let cutoff = now_ms.saturating_sub(GC_MIN_AGE_MS);
        for generation in candidates
            .into_iter()
            .filter(|generation| generation.cleanup_pending || generation.last_used_at <= cutoff)
        {
            match self
                .reclaim_retired_generation(&root, &generation, &lease)
                .await
            {
                Ok(Some(successor)) => lease = successor,
                Ok(None) => break,
                Err(error) => {
                    self.report_io_failure("gc_reclamation_failed").await;
                    tracing::warn!(
                        storage_id = %generation.storage_id,
                        recipe = %generation.recipe_hash,
                        %error,
                        "shared cache GC reclamation remains retryable"
                    );
                    break;
                }
            }
        }
        let _ = self.store.release_lease(&lease, unix_ms()).await;
        Ok(())
    }
}

async fn remove_shared_tree_if_present(
    parent: &SecureDirectory,
    name: &str,
    max_entries: usize,
) -> Result<(), String> {
    match parent.open_child_directory(name).await {
        Ok(_) => parent
            .remove_child_tree(name, max_entries, 1)
            .await
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn shared_generation_components(relative_dir: &str) -> Option<Vec<&str>> {
    let components = Path::new(relative_dir)
        .components()
        .map(|component| match component {
            std::path::Component::Normal(component) => component.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (!components.is_empty() && components.len() <= 8).then_some(components)
}

fn staging_name_for_generation(final_name: &str) -> Option<String> {
    let (prefix, nonce) = final_name.split_once('-')?;
    (prefix.len() == 16
        && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
        && nonce.len() == 32
        && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| format!(".stage-{nonce}"))
}

fn storage_id(cluster_id: &str, shared_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cluster_id.as_bytes());
    hasher.update([0]);
    hasher.update(shared_id.as_bytes());
    format!("shared:{}", hex::encode(hasher.finalize()))
}

async fn prepare_canary_directory(
    root: &Path,
) -> Result<
    (
        PathBuf,
        plurx_core::fs_secure::FileIdentity,
        SecureDirectory,
    ),
    String,
> {
    let canonical = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| format!("canonicalizing shared cache root: {error}"))?;
    let metadata = tokio::fs::symlink_metadata(&canonical)
        .await
        .map_err(|error| format!("inspecting shared cache root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("shared cache root is not a regular directory".to_owned());
    }
    let root = SecureDirectory::open(&canonical)
        .await
        .map_err(|error| error.to_string())?;
    let identity = root.identity().await.map_err(|error| error.to_string())?;
    let canaries = match root.open_child_directory(CANARY_DIRECTORY).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root
            .create_child_directory(CANARY_DIRECTORY)
            .await
            .map_err(|error| error.to_string())?,
        Err(error) => return Err(error.to_string()),
    };
    sweep_canary_orphans(&canonical.join(CANARY_DIRECTORY), &canaries).await?;
    Ok((canonical, identity, canaries))
}

async fn sweep_canary_orphans(
    directory_path: &Path,
    directory: &SecureDirectory,
) -> Result<(), String> {
    let mut entries = tokio::fs::read_dir(directory_path)
        .await
        .map_err(|error| error.to_string())?;
    for _ in 0..CANARY_SWEEP_LIMIT {
        let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        else {
            break;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !["probe", "reply", "single"]
            .into_iter()
            .any(|prefix| safe_canary_name(&name, prefix))
        {
            continue;
        }
        let modified = match tokio::fs::symlink_metadata(entry.path()).await {
            Ok(metadata) => metadata.modified().ok(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        if modified
            .is_some_and(|modified| modified.elapsed().unwrap_or_default() >= CANARY_ORPHAN_AGE)
        {
            let _ = directory.unlink_child(&name).await;
        }
    }
    Ok(())
}

fn canary_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn safe_canary_name(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('-'))
        .is_some_and(|value| {
            value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn random_canary_bytes() -> Vec<u8> {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
    .into_bytes()
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use plurx_core::store::{SqliteStore, Store};

    fn coordinator(
        root: &Path,
        shared_id: &str,
        node_id: &str,
        store: Arc<dyn Store>,
    ) -> Arc<SharedCacheCoordinator> {
        SharedCacheCoordinator::new(
            root.to_path_buf(),
            shared_id.to_owned(),
            "test-cluster",
            node_id.to_owned(),
            MembershipManager::unavailable(),
            store,
        )
    }

    async fn wait_for_suspect_member(store: &Arc<dyn Store>, storage_id: &str, node_id: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if store
                    .cache_storage_member(storage_id, node_id)
                    .await
                    .expect("suspect member")
                    .is_some_and(|member| member.verification_state == "suspect")
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded suspect membership publication");
    }

    async fn source_generation(
        directory: &Path,
        generation_id: &str,
    ) -> plurx_core::transcode::manifest::GenerationManifest {
        tokio::fs::create_dir_all(directory)
            .await
            .expect("source generation directory");
        tokio::fs::write(
            directory.join("index.m3u8"),
            b"#EXTM3U\n#EXTINF:2,\nseg00000.ts\n",
        )
        .await
        .expect("source playlist");
        tokio::fs::write(directory.join("seg00000.ts"), b"generation bytes")
            .await
            .expect("source segment");
        plurx_core::transcode::manifest::publish(
            directory,
            generation_id,
            &["index.m3u8".to_owned(), "seg00000.ts".to_owned()],
        )
        .await
        .expect("source manifest")
    }

    async fn seed_media_files(store: &Arc<dyn Store>, count: usize) -> Vec<i64> {
        let library = store
            .create_library(&NewLibrary {
                name: "Shared Cache Publication Fixtures".to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("publication fixture library");
        let mut files = Vec::with_capacity(count);
        for index in 0..count {
            let item_id = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: format!("Shared Cache Fixture {index}"),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("publication fixture item");
            files.push(
                store
                    .upsert_file(
                        item_id,
                        &format!("/shared-cache-fixture-{index}.mkv"),
                        1,
                        1,
                        &ProbeResult::default(),
                    )
                    .await
                    .expect("publication fixture file"),
            );
        }
        files
    }

    #[test]
    fn storage_identity_is_cluster_scoped_and_canary_names_are_single_components() {
        assert_eq!(
            storage_id("cluster-a", "media"),
            storage_id("cluster-a", "media")
        );
        assert_ne!(
            storage_id("cluster-a", "media"),
            storage_id("cluster-b", "media")
        );
        assert!(safe_canary_name(
            "probe-00000000000040008000000000000000",
            "probe"
        ));
        for unsafe_name in ["", "../probe", "probe-x", "reply-0000/secret"] {
            assert!(!safe_canary_name(unsafe_name, "probe"));
        }
    }

    #[tokio::test]
    async fn two_way_canary_requires_the_same_writable_root_and_identity() {
        let shared = tempfile::tempdir().expect("shared root");
        let different = tempfile::tempdir().expect("different root");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let origin = coordinator(shared.path(), "media-a", "origin", Arc::clone(&store));
        let peer = coordinator(shared.path(), "media-a", "peer", Arc::clone(&store));
        let wrong_root = coordinator(
            different.path(),
            "media-a",
            "wrong-root",
            Arc::clone(&store),
        );
        let wrong_identity = coordinator(
            shared.path(),
            "media-b",
            "wrong-identity",
            Arc::clone(&store),
        );

        let (_, _, canaries) = prepare_canary_directory(shared.path())
            .await
            .expect("origin canary directory");
        let canary_name = canary_name("probe");
        let canary_bytes = random_canary_bytes();
        canaries
            .atomic_write_child(&canary_name, &canary_bytes)
            .await
            .expect("origin canary");
        let request = CanaryRequest {
            storage_id: origin.storage_id().expect("origin storage id").to_owned(),
            canary_name,
            canary_digest: digest(&canary_bytes),
        };

        assert!(wrong_root.answer_canary(&request).await.is_err());
        assert!(wrong_identity.answer_canary(&request).await.is_err());
        let response = peer
            .answer_canary(&request)
            .await
            .expect("peer sees origin canary");
        let response_bytes = canaries
            .read_bounded_child(&response.response_name, CANARY_BYTES)
            .await
            .expect("origin sees peer response");
        assert_eq!(digest(&response_bytes), response.response_digest);
        assert_eq!(response.storage_id, request.storage_id);
    }

    #[tokio::test]
    async fn runtime_failure_revokes_shared_classification_until_readmission() {
        let shared = tempfile::tempdir().expect("shared root");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let shared_cache = coordinator(shared.path(), "media-a", "reader", Arc::clone(&store));
        shared_cache
            .admit_local_for_test()
            .await
            .expect("local canary proof");
        assert!(shared_cache.is_verified());
        let canonical = std::fs::canonicalize(shared.path()).expect("canonical shared root");
        assert_eq!(
            shared_cache.root().await.as_deref(),
            Some(canonical.as_path())
        );

        shared_cache.report_io_failure("test_mount_loss").await;
        assert!(!shared_cache.is_verified());
        assert!(shared_cache.root().await.is_none());
        wait_for_suspect_member(
            &store,
            shared_cache.storage_id().expect("storage id"),
            "reader",
        )
        .await;

        shared_cache
            .admit_local_for_test()
            .await
            .expect("readmission canary");
        assert!(shared_cache.is_verified());

        let missing = shared.path().join("missing");
        let missing = coordinator(&missing, "media-a", "missing", store);
        assert!(missing.admit_local_for_test().await.is_err());
        assert!(!missing.is_verified());
    }

    #[tokio::test]
    async fn gc_refuses_a_replaced_root_before_any_retirement_work() {
        let parent = tempfile::tempdir().expect("shared parent");
        let shared_root = parent.path().join("shared");
        tokio::fs::create_dir(&shared_root)
            .await
            .expect("shared root");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let shared_cache = coordinator(&shared_root, "media-a", "gc-node", Arc::clone(&store));
        shared_cache
            .admit_local_for_test()
            .await
            .expect("admit original root");

        tokio::fs::rename(&shared_root, parent.path().join("detached"))
            .await
            .expect("detach admitted root");
        tokio::fs::create_dir(&shared_root)
            .await
            .expect("replacement mount point");

        assert!(shared_cache.gc_once().await.is_err());
        assert!(!shared_cache.is_verified());
        wait_for_suspect_member(
            &store,
            shared_cache.storage_id().expect("storage id"),
            "gc-node",
        )
        .await;
    }

    #[tokio::test]
    async fn stale_canary_proof_cannot_reenable_a_failed_mount() {
        let shared = tempfile::tempdir().expect("shared root");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let shared_cache = coordinator(shared.path(), "media-a", "reader", Arc::clone(&store));
        let attempted_generation = shared_cache.loss_generation.load(Ordering::Acquire);
        let (path, identity, _) = prepare_canary_directory(shared.path())
            .await
            .expect("canary proof");

        shared_cache.report_io_failure("newer_mount_loss").await;
        assert!(shared_cache
            .publish_verified_root(path, identity, attempted_generation)
            .await
            .is_err());
        assert!(!shared_cache.is_verified());
    }

    #[tokio::test]
    async fn cancelled_publication_callers_do_not_release_copy_capacity() {
        let shared = tempfile::tempdir().expect("shared root");
        let sources = tempfile::tempdir().expect("source roots");
        let source_a = sources.path().join("a");
        let source_b = sources.path().join("b");
        let manifest_a = source_generation(&source_a, "generation-a").await;
        let manifest_b = source_generation(&source_b, "generation-b").await;
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let files = seed_media_files(&store, 2).await;
        let shared_cache = coordinator(shared.path(), "media-a", "publisher", store);
        shared_cache
            .admit_local_for_test()
            .await
            .expect("verified shared mount");

        let pause = Arc::new(tokio::sync::Barrier::new(3));
        *shared_cache
            .publication_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let publisher_a = Arc::clone(&shared_cache);
        let file_a = files[0];
        let publication_a = tokio::spawn(async move {
            publisher_a
                .publish_generation(&"a".repeat(64), file_a, 1, &source_a, &manifest_a)
                .await
        });
        let publisher_b = Arc::clone(&shared_cache);
        let file_b = files[1];
        let publication_b = tokio::spawn(async move {
            publisher_b
                .publish_generation(&"b".repeat(64), file_b, 1, &source_b, &manifest_b)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.wait())
            .await
            .expect("both publications reached installation");
        publication_a.abort();
        publication_b.abort();
        tokio::task::yield_now().await;
        assert_eq!(shared_cache.mount_publications.available_permits(), 0);

        *shared_cache
            .publication_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        pause.wait().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while shared_cache.mount_publications.available_permits()
                != MOUNT_PUBLICATION_CONCURRENCY
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached publications retained then released their permits");
    }

    #[tokio::test]
    async fn mount_loss_during_publication_cannot_install_or_complete_generation() {
        let shared = tempfile::tempdir().expect("shared root");
        let sources = tempfile::tempdir().expect("source roots");
        let source = sources.path().join("generation");
        let manifest = source_generation(&source, "generation-revoked").await;
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let file_id = seed_media_files(&store, 1).await[0];
        let shared_cache = coordinator(
            shared.path(),
            "media-a",
            "revoked-publisher",
            Arc::clone(&store),
        );
        shared_cache
            .admit_local_for_test()
            .await
            .expect("verified shared mount");
        let storage_id = shared_cache.storage_id().expect("storage id").to_owned();
        let recipe_hash = "c".repeat(64);
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *shared_cache
            .publication_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let publisher = Arc::clone(&shared_cache);
        let published_recipe = recipe_hash.clone();
        let publication = tokio::spawn(async move {
            publisher
                .publish_generation(&published_recipe, file_id, 1, &source, &manifest)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.wait())
            .await
            .expect("publication reached installation fence");
        shared_cache
            .report_io_failure("test_publication_mount_loss")
            .await;
        pause.wait().await;
        let outcome = tokio::time::timeout(Duration::from_secs(3), publication)
            .await
            .expect("revoked publication finished")
            .expect("publication task joined");
        assert!(!matches!(outcome, Ok(true)));
        assert!(store
            .shared_cache_hit(&recipe_hash, &storage_id)
            .await
            .expect("shared cache lookup")
            .is_none());
        assert!(store
            .stale_shared_cache_claims(&storage_id, i64::MAX, 8)
            .await
            .expect("stale publication inventory")
            .is_empty());
    }

    #[tokio::test]
    async fn mount_loss_after_commit_begins_preserves_global_generation() {
        let shared = tempfile::tempdir().expect("shared root");
        let sources = tempfile::tempdir().expect("source roots");
        let source = sources.path().join("generation");
        let manifest = source_generation(&source, "generation-committing").await;
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("shared cache store"));
        let file_id = seed_media_files(&store, 1).await[0];
        let shared_cache = coordinator(
            shared.path(),
            "media-a",
            "committing-publisher",
            Arc::clone(&store),
        );
        shared_cache
            .admit_local_for_test()
            .await
            .expect("verified shared mount");
        let storage_id = shared_cache.storage_id().expect("storage id").to_owned();
        let recipe_hash = "d".repeat(64);
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *shared_cache
            .publication_commit_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let publisher = Arc::clone(&shared_cache);
        let published_recipe = recipe_hash.clone();
        let publication = tokio::spawn(async move {
            publisher
                .publish_generation(&published_recipe, file_id, 1, &source, &manifest)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.wait())
            .await
            .expect("publication entered linearized completion");

        shared_cache
            .report_io_failure("test_post_commit_mount_loss")
            .await;
        assert!(!shared_cache.is_verified());
        pause.wait().await;
        assert!(tokio::time::timeout(Duration::from_secs(3), publication)
            .await
            .expect("committing publication finished")
            .expect("publication task joined")
            .expect("publication result"));
        wait_for_suspect_member(&store, &storage_id, "committing-publisher").await;
        assert!(store
            .shared_cache_hit(&recipe_hash, &storage_id)
            .await
            .expect("shared cache lookup")
            .is_some());
    }

    #[test]
    fn shared_generation_name_derives_only_its_owned_staging_path() {
        assert_eq!(
            staging_name_for_generation("0123456789abcdef-0123456789abcdef0123456789abcdef")
                .as_deref(),
            Some(".stage-0123456789abcdef0123456789abcdef")
        );
        assert!(staging_name_for_generation("../other").is_none());
        assert!(staging_name_for_generation("0123456789abcdef-short").is_none());
    }
}
