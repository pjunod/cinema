//! Explicit SHA-256 continuation. Only this versioned representation crosses
//! daemon lifetimes; the private layout of sha2's hasher is never serialized.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Continuation {
    state: [u32; 8],
    bytes: u64,
    partial: Vec<u8>,
}

impl Continuation {
    pub(super) fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            bytes: 0,
            partial: Vec::with_capacity(64),
        }
    }

    pub(super) fn valid_for_bytes(&self, bytes: u64) -> bool {
        self.bytes == bytes
            && self.partial.len() < 64
            && self.partial.len() as u64 == bytes % 64
            && bytes <= u64::MAX / 8
    }

    pub(super) fn update(&mut self, mut input: &[u8]) {
        self.bytes = self
            .bytes
            .checked_add(input.len() as u64)
            .expect("source size was bounded before hashing");
        if !self.partial.is_empty() {
            let take = (64 - self.partial.len()).min(input.len());
            self.partial.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.partial.len() == 64 {
                let block: [u8; 64] = self.partial.as_slice().try_into().expect("complete block");
                sha2::compress256(&mut self.state, &[block.into()]);
                self.partial.clear();
            }
        }
        let mut blocks = input.chunks_exact(64);
        for block in &mut blocks {
            let block: [u8; 64] = block.try_into().expect("complete block");
            sha2::compress256(&mut self.state, &[block.into()]);
        }
        self.partial.extend_from_slice(blocks.remainder());
    }

    pub(super) fn finalize(mut self) -> [u8; 32] {
        let bit_length = self.bytes.checked_mul(8).expect("bounded SHA-256 input");
        let pad = if self.partial.len() < 56 {
            56 - self.partial.len()
        } else {
            120 - self.partial.len()
        };
        let mut padding = vec![0; pad];
        padding[0] = 0x80;
        self.update(&padding);
        self.update(&bit_length.to_be_bytes());
        debug_assert!(self.partial.is_empty());
        let mut digest = [0; 32];
        for (word, output) in self.state.iter().zip(digest.chunks_exact_mut(4)) {
            output.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

/// Stable filename and payload hash are also the durable job's reference.
/// The artifact remains only resumable input, never attestation authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    version: u32,
    algorithm: String,
    regime: String,
    checkpoint: super::FullDigestCheckpoint,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: Artifact,
    payload_sha256: String,
}

pub(super) const DOMAIN: &[u8] = b"plurx/source-attestation/hevc-full-v1\0";
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024;
const ORPHAN_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
static PERSISTENCE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) enum CheckpointOwner<'a> {
    Job(&'a crate::background_jobs::JobFence),
    Analysis {
        store: &'a dyn plurx_core::store::Store,
        request: &'a plurx_core::store::AnalysisRequest,
    },
}

impl CheckpointOwner<'_> {
    async fn checkpoint(&self, checkpoint: serde_json::Value) -> Result<bool, String> {
        match self {
            Self::Job(fence) => fence.checkpoint(checkpoint).await,
            Self::Analysis { store, request } => {
                store
                    .checkpoint_analysis_source(
                        (*request).clone(),
                        checkpoint,
                        crate::state::clock_ms(),
                    )
                    .await
            }
        }
        .map_err(|error| format!("recording durable source checkpoint: {error}"))
    }

    async fn live(&self) -> Result<bool, String> {
        match self {
            Self::Job(fence) => Ok(fence.snapshot().await.is_some()),
            Self::Analysis { store, request } => Ok(store
                .analysis_request(&request.request_id)
                .await
                .map_err(|error| format!("checking source checkpoint owner: {error}"))?
                .is_some_and(|current| {
                    current.state == "running"
                        && !current.cancel_requested
                        && current.fence == request.fence
                        && current.owner_node_id == request.owner_node_id
                        && current.requested_generation == request.requested_generation
                        && current.lease_expires_ms > crate::state::clock_ms()
                })),
        }
    }

    async fn artifacts(&self, node_id: &str) -> Result<Vec<String>, String> {
        match self {
            Self::Job(fence) => fence.source_checkpoint_artifacts(node_id).await,
            Self::Analysis { store, .. } => store.source_checkpoint_artifacts(node_id).await,
        }
        .map_err(|error| format!("reading source checkpoint ownership: {error}"))
    }
}

pub(super) struct CheckpointStorage<'a> {
    directory: plurx_core::fs_secure::SecureDirectory,
    owner: CheckpointOwner<'a>,
}

fn artifact_name(key: &super::FullDigestKey) -> String {
    use sha2::{Digest, Sha256};
    let identity = serde_json::to_vec(key).expect("source checkpoint identity serialization");
    format!("{}.json", hex::encode(Sha256::digest(identity)))
}

fn is_artifact_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn is_temporary_name(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|name| name.strip_suffix(".part"))
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(artifact, nonce)| {
            is_artifact_name(artifact)
                && nonce.len() == 32
                && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn payload_sha256(payload: &Artifact) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    serde_json::to_vec(payload)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .map_err(|error| format!("encoding source checkpoint: {error}"))
}

fn decode_artifact(bytes: &[u8], key: &super::FullDigestKey) -> Option<Envelope> {
    let envelope: Envelope = serde_json::from_slice(bytes).ok()?;
    let payload = &envelope.payload;
    let expected_bytes = (DOMAIN.len() as u64)
        .checked_add(8)?
        .checked_add(payload.checkpoint.offset)?;
    (payload.version == 1
        && payload.algorithm == "sha256"
        && payload.regime == "hevc-full-v1"
        && payload.checkpoint.key == *key
        && payload.checkpoint.offset <= key.size
        && payload.checkpoint.digest.valid_for_bytes(expected_bytes)
        && payload_sha256(payload).ok()? == envelope.payload_sha256)
        .then_some(envelope)
}

impl<'a> CheckpointStorage<'a> {
    pub(super) async fn open(
        root: &std::path::Path,
        owner: CheckpointOwner<'a>,
    ) -> Result<Self, String> {
        let parent = root
            .parent()
            .ok_or("source checkpoint root has no parent")?;
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid source checkpoint root")?;
        let directory = plurx_core::fs_secure::SecureDirectory::open(parent)
            .await
            .map_err(|error| format!("opening source checkpoint parent: {error}"))?
            .create_child_directory(name)
            .await
            .map_err(|error| format!("opening source checkpoint directory: {error}"))?;
        // New directories are 0700 and atomic children are 0600. Refuse an
        // existing Unix directory with broader permissions instead of placing
        // serialized source state in a publicly readable directory.
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let held = plurx_core::fs_secure::open_directory_nofollow_blocking(root)
                .map_err(|error| format!("checking source checkpoint directory: {error}"))?;
            let metadata = held
                .metadata()
                .map_err(|error| format!("checking source checkpoint permissions: {error}"))?;
            if metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(
                    "source checkpoint directory must be owned by the daemon with mode 0700".into(),
                );
            }
        }
        Ok(Self { directory, owner })
    }

    async fn envelope(&self, key: &super::FullDigestKey) -> Option<Envelope> {
        let bytes = self
            .directory
            .read_bounded_child(&artifact_name(key), MAX_ARTIFACT_BYTES)
            .await
            .ok()?;
        decode_artifact(&bytes, key)
    }

    pub(super) async fn load(
        &self,
        key: &super::FullDigestKey,
    ) -> Result<Option<super::FullDigestCheckpoint>, String> {
        let _serial = PERSISTENCE.lock().await;
        let Some(envelope) = self.envelope(key).await else {
            return Ok(None);
        };
        self.reference(&envelope).await?;
        Ok(Some(envelope.payload.checkpoint))
    }

    async fn reference(&self, envelope: &Envelope) -> Result<(), String> {
        let checkpoint = &envelope.payload.checkpoint;
        let recorded = self
            .owner
            .checkpoint(serde_json::json!({"source_attestation":{
                "version":1, "node_id":checkpoint.key.node_id,
                "artifact":artifact_name(&checkpoint.key), "offset":checkpoint.offset,
                "payload_sha256":envelope.payload_sha256,
            }}))
            .await?;
        if !recorded {
            return Err("source checkpoint claim lost".into());
        }
        Ok(())
    }

    /// Reclaim unowned continuations after seven days, or sooner at the fixed
    /// artifact cap. Serialization covers rename-to-reference and excludes
    /// the current source. Queued, running and cleanup owners stay protected.
    async fn retain_owned(&self, node_id: &str, current: &str) -> Result<Vec<String>, String> {
        let names = self
            .directory
            .child_names(128)
            .await
            .map_err(|error| format!("listing source checkpoints: {error}"))?;
        let pressure = names.iter().filter(|name| is_artifact_name(name)).count()
            >= super::FULL_DIGEST_CHECKPOINT_LIMIT;
        let owned = self.owner.artifacts(node_id).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("source checkpoint clock: {error}"))?
            .as_secs() as i64;
        let mut retained = Vec::new();
        for name in names {
            if !is_artifact_name(&name) && !is_temporary_name(&name) {
                continue;
            }
            if !owned.contains(&name) {
                if let Ok(metadata) = self.directory.child_metadata(&name).await {
                    if metadata.is_file
                        && name != current
                        && (metadata.identity.changed_seconds
                            < now.saturating_sub(ORPHAN_RETENTION_SECS)
                            || (pressure && is_artifact_name(&name)))
                    {
                        self.directory.unlink_child(&name).await.map_err(|error| {
                            format!("removing orphan source checkpoint: {error}")
                        })?;
                        continue;
                    }
                }
            }
            if is_artifact_name(&name) {
                retained.push(name);
            }
        }
        Ok(retained)
    }

    pub(super) async fn save(&self, checkpoint: super::FullDigestCheckpoint) -> Result<(), String> {
        let _serial = PERSISTENCE.lock().await;
        if !self.owner.live().await? {
            return Err("source checkpoint claim lost".into());
        }
        // Parallel attempts for one source cannot replace further valid
        // progress with an earlier checkpoint. Each full key has one file.
        if let Some(existing) = self.envelope(&checkpoint.key).await {
            if existing.payload.checkpoint.offset >= checkpoint.offset {
                return self.reference(&existing).await;
            }
        }
        let name = artifact_name(&checkpoint.key);
        let retained = self.retain_owned(&checkpoint.key.node_id, &name).await?;
        if retained.len() >= super::FULL_DIGEST_CHECKPOINT_LIMIT && !retained.contains(&name) {
            tracing::warn!(file_id = checkpoint.key.file_id,
                "durable source checkpoint capacity held by live owners; continuing with bounded memory continuation");
            return Ok(());
        }
        let payload = Artifact {
            version: 1,
            algorithm: "sha256".into(),
            regime: "hevc-full-v1".into(),
            checkpoint,
        };
        let payload_sha256 = payload_sha256(&payload)?;
        let envelope = Envelope {
            payload,
            payload_sha256,
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|error| format!("encoding source checkpoint envelope: {error}"))?;
        if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
            return Err("source checkpoint exceeds byte cap".into());
        }
        // SecureDirectory writes owner-only, fsyncs the file, renames relative
        // to its held directory, then fsyncs that parent before we record the
        // fenced durable reference. A lost reference leaves only resume input.
        self.directory
            .atomic_write_child(&name, &bytes)
            .await
            .map_err(|error| format!("persisting source checkpoint: {error}"))?;
        self.reference(&envelope).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn serialized_sha256_continuations_match_uninterrupted_hash_at_every_block_boundary() {
        // Cover empty input, SHA padding's 55/56-byte boundary, full blocks,
        // partial blocks, and repeated serialize/restart across a large input.
        for size in [0, 1, 55, 56, 63, 64, 65, 127, 128, 129, 1024, 65539] {
            let bytes: Vec<u8> = (0..size).map(|n| (n * 37) as u8).collect();
            let expected = Sha256::digest(&bytes);
            for stride in [1, 7, 55, 56, 63, 64, 65, 257] {
                let mut state = Continuation::new();
                let mut total = 0;
                for part in bytes.chunks(stride) {
                    state.update(part);
                    total += part.len() as u64;
                    let stored = serde_json::to_vec(&state).expect("encode continuation");
                    state = serde_json::from_slice(&stored).expect("restart continuation");
                    assert!(state.valid_for_bytes(total));
                }
                assert_eq!(
                    state.finalize().as_slice(),
                    expected.as_slice(),
                    "size={size}, stride={stride}"
                );
            }
        }
    }

    #[tokio::test]
    async fn durable_continuation_restarts_with_the_same_digest_and_rejects_corrupt_or_stale_state()
    {
        let (store, job_id, active) = crate::background_jobs::tests::active().await;
        let fence = active.fence();
        let directory = tempfile::tempdir().expect("durable checkpoint fixture");
        let root = directory.path().join("source-attestation-checkpoints");
        let bytes: Vec<u8> = (0..4099).map(|n| (n * 23) as u8).collect();
        let key = super::super::FullDigestKey {
            node_id: "node-a".into(),
            file_id: 1,
            object_version: "hevc-full-v1:object-version-with-nanoseconds".into(),
            size: bytes.len() as u64,
        };
        let mut digest = Continuation::new();
        digest.update(DOMAIN);
        digest.update(&key.size.to_be_bytes());
        digest.update(&bytes[..65]);
        let checkpoint = super::super::FullDigestCheckpoint {
            key: key.clone(),
            offset: 65,
            digest,
        };
        let storage = CheckpointStorage::open(&root, CheckpointOwner::Job(&fence))
            .await
            .expect("checkpoint directory");
        storage
            .save(checkpoint)
            .await
            .expect("durable save then reference");
        drop(storage);
        let job = store
            .background_job(&job_id)
            .await
            .expect("durable job")
            .expect("job exists");
        assert_eq!(
            job.checkpoint.as_ref().expect("reference")["source_attestation"]["offset"],
            65
        );
        let storage = CheckpointStorage::open(&root, CheckpointOwner::Job(&fence))
            .await
            .expect("restarted checkpoint owner");
        let mut resumed = storage
            .load(&key)
            .await
            .expect("reload")
            .expect("durable continuation");
        resumed.digest.update(&bytes[resumed.offset as usize..]);
        let mut expected = Sha256::new();
        expected.update(DOMAIN);
        expected.update(key.size.to_be_bytes());
        expected.update(&bytes);
        assert_eq!(
            resumed.digest.finalize().as_slice(),
            expected.finalize().as_slice()
        );
        let name = artifact_name(&key);
        let saved = storage
            .directory
            .read_bounded_child(&name, MAX_ARTIFACT_BYTES)
            .await
            .expect("artifact");
        let mut changed = key.clone();
        changed.object_version.push_str("-changed");
        assert!(
            decode_artifact(&saved, &changed).is_none(),
            "source identity is an exact fence"
        );
        let mut invalid: Envelope = serde_json::from_slice(&saved).expect("envelope");
        invalid.payload.checkpoint.digest.partial.push(0);
        invalid.payload_sha256 = payload_sha256(&invalid.payload).expect("checksum");
        assert!(
            decode_artifact(&serde_json::to_vec(&invalid).expect("encoding"), &key).is_none(),
            "even checksummed invalid byte counts rebuild"
        );
        invalid.payload.version = 2;
        invalid.payload_sha256 = payload_sha256(&invalid.payload).expect("checksum");
        assert!(
            decode_artifact(&serde_json::to_vec(&invalid).expect("encoding"), &key).is_none(),
            "unknown format never resumes"
        );
        storage
            .directory
            .atomic_write_child(&name, b"corrupt interrupted artifact")
            .await
            .expect("corrupt fixture");
        assert!(storage.load(&key).await.expect("safe rebuild").is_none());
        active.finish().await;
    }

    #[test]
    fn invalid_partial_block_and_byte_count_are_rejected() {
        let mut state = Continuation::new();
        state.update(b"partial");
        assert!(!state.valid_for_bytes(8));
        state.partial.resize(64, 0);
        assert!(!state.valid_for_bytes(7));
    }
}
