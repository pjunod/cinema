//! Verified optional shared-cache mount and fenced reclamation.
//!
//! A configured path is never treated as shared by itself. Every voter proves
//! that it can read a peer-created canary and that the peer can read its own
//! response through the same mount. Failure disables the fast path
//! immediately; ordinary node-local cache routing remains available.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
const GC_INTERVAL: Duration = Duration::from_secs(60);
const GC_LEASE_MS: i64 = 30_000;
const GC_BATCH: i64 = 32;
const GC_MIN_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
struct SharedConfig {
    configured_root: PathBuf,
    storage_id: String,
}

#[derive(Clone)]
pub(crate) struct SharedCacheCoordinator {
    config: Option<SharedConfig>,
    canonical_root: Arc<tokio::sync::RwLock<Option<PathBuf>>>,
    verified: Arc<AtomicBool>,
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
            canonical_root: Arc::new(tokio::sync::RwLock::new(None)),
            verified: Arc::new(AtomicBool::new(false)),
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
    }

    pub(crate) async fn root(&self) -> Option<PathBuf> {
        if !self.is_verified() {
            return None;
        }
        self.canonical_root.read().await.clone()
    }

    #[cfg(test)]
    pub(crate) async fn admit_local_for_test(&self) -> Result<(), String> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let (root, canaries) = prepare_canary_directory(&config.configured_root).await?;
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
        *self.canonical_root.write().await = Some(root);
        self.verified.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn report_io_failure(&self, reason: &'static str) {
        if !self.verified.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(config) = self.config.as_ref() else {
            return;
        };
        tracing::warn!(
            storage_id = %config.storage_id,
            reason,
            "shared cache proof lost; falling back to node-local holders"
        );
        let _ = self
            .store
            .mark_cache_storage_suspect(&config.storage_id, &self.node_id, unix_ms())
            .await;
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
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let (root, canaries) = prepare_canary_directory(&config.configured_root).await?;
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
            for peer in peers {
                if !peer.reachable {
                    return Err(format!(
                        "shared-cache peer {} is not reachable",
                        peer.node_id
                    ));
                }
                let base = peer.http_base.ok_or_else(|| {
                    format!("shared-cache peer {} has no HTTP base", peer.node_id)
                })?;
                self.verify_peer(&config.storage_id, &canaries, &peer.node_id, &base)
                    .await?;
            }
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
        *self.canonical_root.write().await = Some(root);
        self.verified.store(true, Ordering::Release);
        Ok(())
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
            .await
            .map_err(|error| format!("peer canary request failed: {error:?}"))?;
        let _ = canaries.unlink_child(&canary_name).await;
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
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        if request.storage_id != config.storage_id
            || !safe_canary_name(&request.canary_name, "probe")
            || request.canary_digest.len() != 64
        {
            return Err("shared cache canary identity does not match".to_owned());
        }
        let (_, canaries) = prepare_canary_directory(&config.configured_root).await?;
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
        Ok(CanaryResponse {
            storage_id: config.storage_id.clone(),
            response_name,
            response_digest: digest(&response_bytes),
        })
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
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "shared cache is not configured".to_owned())?;
        let Some(root_path) = self.root().await else {
            return Ok(false);
        };
        let short_hash = recipe_hash
            .get(..16)
            .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| "shared cache recipe has no safe generation prefix".to_owned())?;
        let prefix = &short_hash[..2];
        let root = match SecureDirectory::open(&root_path).await {
            Ok(root) => root,
            Err(error) => {
                self.report_io_failure("shared_publish_root_failed").await;
                return Err(format!("opening shared cache root: {error}"));
            }
        };
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
        let staging = match fanout.create_child_directory(&staging_name).await {
            Ok(staging) => staging,
            Err(error) => {
                self.report_io_failure("shared_publish_staging_failed")
                    .await;
                return Err(format!("creating shared generation staging: {error}"));
            }
        };
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
                let _ = fanout
                    .remove_child_tree(&staging_name, manifest.objects.len().saturating_add(1), 1)
                    .await;
                return Err(error.to_string());
            }
        };
        if !claimed {
            let _ = fanout
                .remove_child_tree(&staging_name, manifest.objects.len().saturating_add(1), 1)
                .await;
            return Ok(false);
        }

        let source = match SecureDirectory::open(source_dir).await {
            Ok(source) => source,
            Err(error) => {
                let _ = fanout
                    .remove_child_tree(&staging_name, manifest.objects.len().saturating_add(1), 1)
                    .await;
                let _ = self
                    .store
                    .forget_cache_entry(recipe_hash, &config.storage_id, "shared")
                    .await;
                return Err(format!(
                    "opening local generation for shared publication: {error}"
                ));
            }
        };
        let staging_path = root_path.join(prefix).join(&staging_name);
        let mut installed = false;
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
            installed = true;
            let complete = self
                .store
                .complete_shared_cache_entry(
                    recipe_hash,
                    &config.storage_id,
                    &manifest.generation_id,
                    total_bytes.min(i64::MAX as u64) as i64,
                    &manifest.manifest_digest,
                    unix_ms(),
                )
                .await
                .map_err(|error| (error.to_string(), false))?;
            if !complete {
                return Err((
                    "shared cache claim changed before completion".to_owned(),
                    false,
                ));
            }
            Ok::<(), (String, bool)>(())
        }
        .await;
        match publication {
            Ok(()) => Ok(true),
            Err((error, io_failure)) => {
                let child = if installed {
                    final_name.as_str()
                } else {
                    staging_name.as_str()
                };
                let _ = fanout
                    .remove_child_tree(child, manifest.objects.len().saturating_add(1), 1)
                    .await;
                let _ = self
                    .store
                    .forget_cache_entry(recipe_hash, &config.storage_id, "shared")
                    .await;
                if io_failure {
                    self.report_io_failure("shared_publish_failed").await;
                }
                Err(error)
            }
        }
    }

    async fn gc_once(&self) -> Result<(), String> {
        let Some(config) = self.config.as_ref() else {
            return Ok(());
        };
        let Some(root) = self.root().await else {
            return Ok(());
        };
        let now_ms = unix_ms();
        let resource = format!("shared-cache-gc:{}", config.storage_id);
        let lease = match self
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
        let candidates = self
            .store
            .shared_cache_gc_candidates(&config.storage_id, now_ms, GC_BATCH)
            .await
            .map_err(|error| error.to_string())?;
        let cutoff = now_ms.saturating_sub(GC_MIN_AGE_MS);
        for generation in candidates
            .into_iter()
            .filter(|generation| generation.last_used_at <= cutoff)
        {
            if self
                .store
                .retire_shared_cache_generation(&generation, unix_ms(), &lease)
                .await
                .map_err(|error| error.to_string())?
            {
                let Some(directory) =
                    crate::cachekeep::validated_entry_dir(&root, &generation.relative_dir).await
                else {
                    self.report_io_failure("gc_path_invalid").await;
                    break;
                };
                if let Err(error) =
                    crate::transcode::quarantine_remove_cache_tree(&directory, 1).await
                {
                    tracing::warn!(
                        storage_id = %generation.storage_id,
                        recipe = %generation.recipe_hash,
                        %error,
                        "shared cache pointer retired but filesystem deletion failed"
                    );
                    self.report_io_failure("gc_delete_failed").await;
                    break;
                }
            }
        }
        let _ = self.store.release_lease(&lease, unix_ms()).await;
        Ok(())
    }
}

fn storage_id(cluster_id: &str, shared_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cluster_id.as_bytes());
    hasher.update([0]);
    hasher.update(shared_id.as_bytes());
    format!("shared:{}", hex::encode(hasher.finalize()))
}

async fn prepare_canary_directory(root: &Path) -> Result<(PathBuf, SecureDirectory), String> {
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
    let canaries = match root.open_child_directory(CANARY_DIRECTORY).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root
            .create_child_directory(CANARY_DIRECTORY)
            .await
            .map_err(|error| error.to_string())?,
        Err(error) => return Err(error.to_string()),
    };
    Ok((canonical, canaries))
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

        let (_, canaries) = prepare_canary_directory(shared.path())
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
        assert_eq!(
            store
                .cache_storage_member(shared_cache.storage_id().expect("storage id"), "reader",)
                .await
                .expect("suspect member")
                .map(|member| member.verification_state),
            Some("suspect".to_owned())
        );

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
}
