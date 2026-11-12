//! The reader of published v3 copy indexes.
//!
//! VOD planning asks here first. This node's own head is read by its exact
//! source key and used as it stands. Only on a miss, and only when this node
//! holds its own full-source receipt, a readable head another origin
//! published for the same catalogue file is used if that receipt names the
//! variant's source digest. The variant's bytes come from this node's v3
//! root, or from a listed holder over the peer route, and are verified by
//! size and SHA-256 before they are installed, recorded as a location or
//! decoded. Nothing here reads the source, queues work or grants serving
//! authority; without a usable head the caller keeps its existing index
//! order. A healthy head this reader finds defective (sealed without the
//! HDR10 fold, held by no node) is reported to the Analysis worker, which
//! alone can quarantine it; the reader still just moves to its next index.
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::MediaFile;
use plurx_core::segplan::{FragmentIndex, SourceIdentity};
use plurx_core::store::{
    CopyIndexV3Artifact, CopyIndexV3HeadState, CopyIndexV3Location, CopyIndexV3Variant,
    FragmentIndexOutputIdentity, Store, MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
};
use plurx_core::transcode::CopyVideoOptions;

use crate::copy_analysis::{CopyAnalysisRegistry, CopyIndexDefect, CopyIndexDefectKind};
use crate::http::peer_transport::{PeerAuthMode, PeerTransport};

pub(crate) const PEER_PATH_PREFIX: &str = "/internal/media/copy-index-v3/";
const PEER_DEADLINE: Duration = Duration::from_secs(8);

/// The body of the one 404 that means "this route knows the variant and this
/// node does not hold its bytes". Only that answer retires a holder's row: a
/// node without the route, or a variant it will not serve, answers a plain
/// 404 and proves nothing about what it holds.
pub(crate) const ABSENT_BODY: &[u8] = b"copy-index-v3-variant-absent";

/// A decoded v3 index bound to this node's current source object.
pub(crate) struct ResolvedCopyIndex {
    pub(crate) index: FragmentIndex,
    /// This node's held-object witness the index (and its HEVC proof) is
    /// bound to.
    pub(crate) object_version: String,
    /// What a rendition planned from this index is keyed by: the semantic
    /// output contract, never a blob variant or engine.
    pub(crate) rendition_cache_key: String,
    /// The init contract the seal published. A generation serves this index
    /// only when its own muxer init, promoted, has exactly this contract.
    pub(crate) init_contract_sha256: String,
}

/// Contracts whose sealed init this process's engine does not produce. The
/// engine is fixed for the life of the process, so a refusal found by one
/// generation holds for every later plan: planning then uses its next index
/// instead of selecting the same contract again.
static UNSERVED_CONTRACTS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
const MAX_UNSERVED_CONTRACTS: usize = 4096;

/// Record that a generation of `rendition_cache_key` produced an init other
/// than the sealed one.
pub(crate) fn refuse_on_this_engine(rendition_cache_key: &str) {
    let mut unserved = UNSERVED_CONTRACTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let unserved = unserved.get_or_insert_with(HashSet::new);
    if unserved.len() >= MAX_UNSERVED_CONTRACTS {
        // Forgetting only costs one more refused generation per contract.
        unserved.clear();
    }
    unserved.insert(rendition_cache_key.to_owned());
}

/// The head variant each rendition key was last planned from, so a generation
/// that refuses the sealed init can name the exact head it was planned from.
static PLANNED_VARIANTS: Mutex<Option<HashMap<String, CopyIndexDefect>>> = Mutex::new(None);

fn remember_planned(rendition_cache_key: &str, variant: &CopyIndexV3Variant, blob_sha256: &str) {
    let mut planned = PLANNED_VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let planned = planned.get_or_insert_with(HashMap::new);
    if planned.len() >= MAX_UNSERVED_CONTRACTS {
        // Forgetting only costs a sealed-init refusal its report.
        planned.clear();
    }
    planned.insert(
        rendition_cache_key.to_owned(),
        CopyIndexDefect {
            source_key: variant.head.source_key.clone(),
            head_epoch: variant.head.head_epoch,
            blob_sha256: blob_sha256.to_owned(),
            kind: CopyIndexDefectKind::SealedInit,
        },
    );
}

/// The sealed-init defect of the head `rendition_cache_key` was last planned
/// from. Whether it is a defect of the variant or only of this node's engine
/// is the Analysis worker's to decide.
pub(crate) fn sealed_init_defect(rendition_cache_key: &str) -> Option<CopyIndexDefect> {
    PLANNED_VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|planned| planned.get(rendition_cache_key).cloned())
}

fn refused_on_this_engine(rendition_cache_key: &str) -> bool {
    UNSERVED_CONTRACTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|unserved| unserved.contains(rendition_cache_key))
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The peer path of one head's variant: `{source_key}/{blob_sha256}`.
pub(crate) fn peer_path(source_key: &str, blob_sha256: &str) -> String {
    format!("{PEER_PATH_PREFIX}{source_key}/{blob_sha256}")
}

/// Exactly a source key and a blob digest, nothing deeper or emptier.
pub(crate) fn peer_route_key(path: &str) -> Option<(&str, &str)> {
    let (source_key, blob_sha256) = path.strip_prefix(PEER_PATH_PREFIX)?.split_once('/')?;
    (digest(source_key) && digest(blob_sha256)).then_some((source_key, blob_sha256))
}

/// What one resolution needs of this node, fixed before any head is read.
struct Consumer<'a> {
    store: &'a dyn Store,
    membership: Option<&'a MembershipManager>,
    node_id: &'a str,
    namespace: &'a str,
    root: std::path::PathBuf,
    file: &'a MediaFile,
    identity: SourceIdentity,
    object_version: String,
    /// Where a defective healthy head is reported. None reports nothing.
    defects: Option<&'a CopyAnalysisRegistry>,
}

/// Resolve the v3 index for one copy of `file` on this node, or `None` when no
/// usable head, holder or valid blob exists. `namespace` is the cluster's
/// catalogue identity, as the publisher keyed it. `defects` is the Analysis
/// worker's inbox for a healthy head found defective.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    namespace: &str,
    runtime_cache: &Path,
    file: &MediaFile,
    video: CopyVideoOptions,
    defects: Option<&CopyAnalysisRegistry>,
) -> Result<Option<ResolvedCopyIndex>, String> {
    // This node's current object: a stat checked against the scanner's facts.
    let object_version = crate::fragment_index_cluster::inspect_source(file).await?;
    let identity = crate::fragindex::identity_for(file, video);
    let own = crate::copy_analysis::source_for_copy(
        namespace,
        file,
        &identity.argv_fingerprint,
        &object_version,
        node_id,
    )
    .map_err(|error| format!("deriving the copy source: {error}"))?;
    let own_key = plurx_core::store::copy_index_v3_source_key(namespace, &own)
        .map_err(|error| format!("deriving the copy source key: {error}"))?;
    let consumer = Consumer {
        store,
        membership,
        node_id,
        namespace,
        root: crate::copy_analysis::v3_root(runtime_cache),
        file,
        identity,
        object_version,
        defects,
    };
    // This node's own head, by primary key.
    let own_variant = store
        .copy_index_v3_variant(&own_key, node_id)
        .await
        .map_err(|error| format!("reading the copy index head: {error}"))?;
    if let Some(variant) = &own_variant {
        if let Some(resolved) = consumer.plan_from(variant).await? {
            return Ok(Some(resolved));
        }
    }
    // Another origin's opaque object version proves nothing here. Its index
    // describes this node's bytes only when this node's own full-source
    // receipt names the same content, so without one there is nothing to
    // list.
    let Some(attested) = consumer.attested_source_sha256().await? else {
        return Ok(None);
    };
    // A blob this node's own head was quarantined for is never adopted back
    // through another origin's pointer.
    let quarantined = own_variant
        .as_ref()
        .filter(|variant| variant.head.state == CopyIndexV3HeadState::Quarantined)
        .and_then(|variant| variant.head.blob_sha256.as_deref());
    let heads = store
        .copy_index_v3_healthy_heads(namespace, &own)
        .await
        .map_err(|error| format!("reading copy index heads: {error}"))?;
    for head in heads {
        if head.source_key == own_key
            || (quarantined.is_some() && head.blob_sha256.as_deref() == quarantined)
        {
            continue;
        }
        let Some(variant) = store
            .copy_index_v3_variant(&head.source_key, node_id)
            .await
            .map_err(|error| format!("reading the copy index variant: {error}"))?
        else {
            continue;
        };
        if variant
            .readable()
            .is_none_or(|artifact| artifact.source_sha256 != attested)
            || (quarantined.is_some() && variant.head.blob_sha256.as_deref() == quarantined)
        {
            continue;
        }
        if let Some(resolved) = consumer.plan_from(&variant).await? {
            return Ok(Some(resolved));
        }
    }
    Ok(None)
}

impl Consumer<'_> {
    /// Report a defect of the healthy head `variant` to the Analysis worker.
    fn report(&self, variant: &CopyIndexV3Variant, blob_sha256: &str, kind: CopyIndexDefectKind) {
        let Some(defects) = self.defects else { return };
        let sent = defects.notify_defect(CopyIndexDefect {
            source_key: variant.head.source_key.clone(),
            head_epoch: variant.head.head_epoch,
            blob_sha256: blob_sha256.to_owned(),
            kind,
        });
        tracing::debug!(
            file_id = self.file.id,
            blob_sha256,
            code = kind.code(),
            sent,
            "copy index v3 defect reported"
        );
    }

    /// Plan from one head's variant, or `None` when it is unreadable, its
    /// bytes are unavailable, or it is refused. A refusal repairs nothing
    /// here: the caller moves to its next index, and a defect of the head
    /// itself is reported for the Analysis worker to quarantine.
    async fn plan_from(
        &self,
        variant: &CopyIndexV3Variant,
    ) -> Result<Option<ResolvedCopyIndex>, String> {
        let Some(artifact) = variant.readable().filter(|artifact| {
            artifact.catalog_namespace == self.namespace
                && artifact.file_id == self.file.id
                && artifact.contract_version == variant.head.contract_version
        }) else {
            return Ok(None);
        };
        let rendition_cache_key = format!("copy-index-v3:{}", artifact.contract_key);
        if refused_on_this_engine(&rendition_cache_key) {
            return Ok(None);
        }
        let blob = match load_blob(
            self.store,
            self.membership,
            self.node_id,
            &self.root,
            variant,
            artifact,
        )
        .await?
        {
            Loaded::Blob(blob) => blob,
            Loaded::Unavailable => return Ok(None),
            Loaded::NoHolder => {
                self.report(
                    variant,
                    &artifact.blob_sha256,
                    CopyIndexDefectKind::NoHolder,
                );
                return Ok(None);
            }
        };
        let mut index = match decode_verified(&blob, artifact) {
            Ok(index) => index,
            Err(reason) => {
                if reason == UNINDEXED_HDR10 {
                    self.report(
                        variant,
                        &artifact.blob_sha256,
                        CopyIndexDefectKind::UnindexedHdr10,
                    );
                }
                tracing::warn!(
                    file_id = self.file.id,
                    contract_key = artifact.contract_key,
                    blob_sha256 = artifact.blob_sha256,
                    %reason,
                    "copy index v3 variant refused"
                );
                return Ok(None);
            }
        };
        index.source = self.identity.clone();
        if let Some(proof) = index.promotion.hevc_configuration.as_mut() {
            if !proof.bind_equivalent_source(
                &artifact.source_sha256,
                self.node_id,
                &self.object_version,
            ) {
                tracing::warn!(
                    file_id = self.file.id,
                    contract_key = artifact.contract_key,
                    "copy index v3 HEVC proof is not bound to its attested source"
                );
                return Ok(None);
            }
        }
        remember_planned(&rendition_cache_key, variant, &artifact.blob_sha256);
        Ok(Some(ResolvedCopyIndex {
            index,
            object_version: self.object_version.clone(),
            rendition_cache_key,
            init_contract_sha256: artifact.init_contract_sha256.clone(),
        }))
    }

    /// This node's full-file SHA-256 of its current object, from the receipts
    /// the v3 publication itself accepts: a full (never sampled) v2 source
    /// receipt, or a regime-1 attestation.
    async fn attested_source_sha256(&self) -> Result<Option<String>, String> {
        let (store, file) = (self.store, self.file);
        let current = |size: i64, mtime: i64| size == file.size && mtime == file.mtime;
        let full = crate::fragment_index_cluster::full_copy_object_version(&self.object_version);
        if let Some(receipt) = store
            .fragment_index_source(self.node_id, file.id, &full)
            .await
            .map_err(|error| format!("reading the source receipt: {error}"))?
            .filter(|receipt| current(receipt.source_size, receipt.source_mtime))
        {
            return Ok(Some(receipt.source_sha256.to_ascii_lowercase()));
        }
        Ok(store
            .copy_index_source_attestation(self.node_id, file.id, &self.object_version)
            .await
            .map_err(|error| format!("reading the source attestation: {error}"))?
            .filter(|receipt| current(receipt.source_size, receipt.source_mtime))
            .map(|receipt| receipt.source_sha256))
    }
}

fn blob_matches(blob: &[u8], artifact: &CopyIndexV3Artifact) -> bool {
    blob.len() <= MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES
        && i64::try_from(blob.len()).is_ok_and(|bytes| bytes == artifact.bytes)
        && plurx_core::store::cluster_fragment_index_blob_sha256(blob) == artifact.blob_sha256
}

/// Why a verified blob sealed before the whole-title HDR10 fold is refused.
const UNINDEXED_HDR10: &str = "sealed without whole-title HDR10 metadata";

/// Whether `blob` is the row's exact bytes and is refused only for lacking
/// the whole-title HDR10 fold.
pub(crate) fn lacks_indexed_hdr10(blob: &[u8], artifact: &CopyIndexV3Artifact) -> bool {
    decode_verified(blob, artifact).is_err_and(|reason| reason == UNINDEXED_HDR10)
}

/// Decode the variant against its artifact row. The blob must be the exact
/// catalogued bytes, carry the row's source and output identity, promise the
/// whole-title HDR10 metadata a legacy index promises, and its planning table
/// must be the one bound to the row's init contract.
pub(crate) fn decode_verified(
    blob: &[u8],
    artifact: &CopyIndexV3Artifact,
) -> Result<FragmentIndex, String> {
    if !blob_matches(blob, artifact) {
        return Err("blob checksum or size mismatch".into());
    }
    let output = FragmentIndexOutputIdentity {
        contract_version: artifact.contract_version,
        keyframe_table_sha256: artifact.output_contract_sha256.clone(),
    };
    let index = plurx_core::store::decode_cluster_fragment_index_blob_v3(
        blob,
        &artifact.source_sha256,
        &output,
    )
    .map_err(|error| error.to_string())?
    .index;
    if !index.promotion.indexed_hdr10_metadata {
        // Sealed before the whole-title HDR10 fold: its promotion would serve
        // a different init than the legacy index of the same title.
        return Err(UNINDEXED_HDR10.into());
    }
    // The served init is `promote_from(muxer init, index.promotion)`, and the
    // seal published the contract of exactly that promotion. The planning
    // digest covers both, so the row's init contract is this blob's.
    let bound = plurx_core::store::cluster_fragment_index_output_identity(
        &index,
        &artifact.source_sha256,
        &artifact.init_contract_sha256,
    )
    .map_err(|error| error.to_string())?;
    if bound != output {
        return Err("init contract is not the one this index was sealed with".into());
    }
    Ok(index)
}

/// What this node's v3 root holds for one variant.
enum LocalBlob {
    Verified(Vec<u8>),
    Missing,
    /// The file could not be stated or read. Nothing is known about its
    /// bytes, so nothing is deleted and no holder is retired.
    Unreadable(String),
    /// The file was read in full and is not the catalogued bytes.
    Corrupt(String),
}

async fn read_local(path: &Path, artifact: &CopyIndexV3Artifact) -> LocalBlob {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LocalBlob::Missing,
        Err(error) => return LocalBlob::Unreadable(format!("stat {}: {error}", path.display())),
    };
    if u64::try_from(artifact.bytes).ok() != Some(metadata.len()) {
        return LocalBlob::Corrupt(format!(
            "size is {}, expected {}",
            metadata.len(),
            artifact.bytes
        ));
    }
    match tokio::fs::read(path).await {
        Ok(blob) if blob_matches(&blob, artifact) => LocalBlob::Verified(blob),
        Ok(_) => LocalBlob::Corrupt("checksum or size mismatch".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LocalBlob::Missing,
        Err(error) => LocalBlob::Unreadable(format!("read {}: {error}", path.display())),
    }
}

/// Remove this node's copy of a variant that failed verification. The caller
/// holds no pin on it: eviction refuses a pinned path.
pub(crate) async fn remove_local(root: &Path, blob_sha256: &str) {
    let Some(path) = crate::copy_analysis::v3_blob_path(root, blob_sha256) else {
        return;
    };
    let _eviction = match crate::playback_cache::begin_eviction(&path) {
        Ok(eviction) => eviction,
        Err(error) => {
            tracing::warn!(blob_sha256, %error, "a corrupt copy index v3 blob is in use");
            return;
        }
    };
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(blob_sha256, %error, "removing a corrupt copy index v3 blob"),
    }
}

/// Record this node as a verified holder. Best-effort: the bytes are already
/// verified and durable here, and a later hydration or local hit records the
/// row again.
async fn record_location(
    store: &dyn Store,
    node_id: &str,
    source_key: &str,
    artifact: &CopyIndexV3Artifact,
) {
    let now = crate::state::clock_ms();
    let location = CopyIndexV3Location {
        contract_key: artifact.contract_key.clone(),
        blob_sha256: artifact.blob_sha256.clone(),
        node_id: node_id.to_owned(),
        bytes: artifact.bytes,
        verified_at_ms: now,
        last_seen_at_ms: now,
    };
    match store.put_copy_index_v3_location(source_key, location).await {
        // False: the head moved on between the read and this write.
        Ok(recorded) => tracing::debug!(
            blob_sha256 = artifact.blob_sha256,
            recorded,
            "copy index v3 location recorded"
        ),
        Err(error) => tracing::warn!(
            blob_sha256 = artifact.blob_sha256,
            %error,
            "copy index v3 location was not recorded"
        ),
    }
}

async fn forget_own_location(
    store: &dyn Store,
    node_id: &str,
    artifact: &CopyIndexV3Artifact,
    operation: crate::store_result::Operation,
) {
    // Best-effort: the bytes are not here either way; a peer that asks is
    // answered by the route's own positive 404.
    crate::store_result::observe(
        operation,
        crate::store_result::Discard::BestEffort,
        store
            .forget_copy_index_v3_location(&artifact.contract_key, &artifact.blob_sha256, node_id)
            .await,
    );
}

/// What became of one variant's bytes.
enum Loaded {
    Blob(Vec<u8>),
    /// Not obtained now; a holder may still have them.
    Unavailable,
    /// This node has no copy and no other node is a holder: none is listed,
    /// or every listed one answered that it holds nothing. An unreachable
    /// or unanswering holder is never counted as absent.
    NoHolder,
}

/// The variant's exact bytes: this node's verified blob, or one hydrated from
/// a listed holder. A hydrating node installs under the publisher's staged
/// write and disk budget and then records itself as a verified location.
async fn load_blob(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    root: &Path,
    variant: &CopyIndexV3Variant,
    artifact: &CopyIndexV3Artifact,
) -> Result<Loaded, String> {
    let path = crate::copy_analysis::v3_blob_path(root, &artifact.blob_sha256)
        .ok_or("invalid v3 blob digest")?;
    let source_key = variant.head.source_key.as_str();
    // The pin covers only the read: removing a corrupt blob needs it gone.
    let local = {
        let _pin = crate::playback_cache::pin(&path)
            .map_err(|error| format!("copy index v3 blob is busy: {error}"))?;
        read_local(&path, artifact).await
    };
    match local {
        LocalBlob::Verified(blob) => {
            // The head read already said whether this node is a listed
            // holder, so the common start writes nothing and a row another
            // node retired is restored without a further read.
            if !variant.held {
                record_location(store, node_id, source_key, artifact).await;
            }
            return Ok(Loaded::Blob(blob));
        }
        LocalBlob::Missing => {
            if variant.held {
                forget_own_location(
                    store,
                    node_id,
                    artifact,
                    crate::store_result::Operation::ForgetMissingInternalIndex,
                )
                .await;
            }
        }
        LocalBlob::Unreadable(error) => {
            tracing::warn!(
                blob_sha256 = artifact.blob_sha256,
                %error,
                "local copy index v3 blob is unreadable; leaving it in place"
            );
            return Ok(Loaded::Unavailable);
        }
        LocalBlob::Corrupt(reason) => {
            tracing::warn!(
                blob_sha256 = artifact.blob_sha256,
                %reason,
                "discarding a corrupt local copy index v3 blob"
            );
            remove_local(root, &artifact.blob_sha256).await;
            if variant.held {
                forget_own_location(
                    store,
                    node_id,
                    artifact,
                    crate::store_result::Operation::ForgetCorruptLocalIndex,
                )
                .await;
            }
        }
    }
    // This node's own row, if any, is gone or was never there: its bytes are
    // not here. Whether anyone else holds them is what remains.
    let locations = store
        .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
        .await
        .map_err(|error| format!("listing copy index holders: {error}"))?
        .into_iter()
        .filter(|location| location.node_id != node_id)
        .collect::<Vec<_>>();
    if locations.is_empty() {
        return Ok(Loaded::NoHolder);
    }
    let Some(membership) = membership else {
        return Ok(Loaded::Unavailable);
    };
    // Holders that did not themselves say they hold nothing.
    let mut possible = locations.len();
    let peers = membership
        .media_peers()
        .await
        .map_err(|error| format!("listing copy index peers: {error}"))?
        .into_iter()
        .filter_map(|peer| {
            peer.http_base
                .map(|base| (peer.node_id, (base, peer.reachable)))
        })
        .filter(|(peer_node, (_, reachable))| peer_node != node_id && *reachable)
        .collect::<HashMap<_, _>>();
    let transport = PeerTransport::new(membership.clone());
    let request_path = peer_path(source_key, &artifact.blob_sha256);
    for location in locations {
        let Some((base, _)) = peers.get(&location.node_id) else {
            continue;
        };
        let response = transport
            .request(
                &location.node_id,
                base,
                reqwest::Method::GET,
                &request_path,
                Vec::new(),
                tokio::time::Instant::now() + PEER_DEADLINE,
                MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await;
        let Ok(response) = response else { continue };
        if response.status == reqwest::StatusCode::NOT_FOUND && response.body == ABSENT_BODY {
            // Best-effort: the holder itself said it lacks these bytes. Any
            // other answer (a node without this route, a refusal, a damaged
            // transfer) leaves its row alone.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetMissingPeerIndex,
                crate::store_result::Discard::BestEffort,
                store
                    .forget_copy_index_v3_location(
                        &artifact.contract_key,
                        &artifact.blob_sha256,
                        &location.node_id,
                    )
                    .await,
            );
            possible -= 1;
            continue;
        }
        if !response.status.is_success() || !blob_matches(&response.body, artifact) {
            continue;
        }
        crate::copy_analysis::install_blob(
            root,
            &artifact.contract_key,
            &artifact.blob_sha256,
            &response.body,
        )
        .await?;
        // Only now is this node a holder: the exact bytes are verified and
        // durable. The installed blob is used whether or not the row lands.
        record_location(store, node_id, source_key, artifact).await;
        tracing::info!(
            file_id = artifact.file_id,
            contract_key = artifact.contract_key,
            blob_sha256 = artifact.blob_sha256,
            holder = location.node_id,
            "copy index v3 hydrated from a peer"
        );
        return Ok(Loaded::Blob(response.body));
    }
    Ok(if possible == 0 {
        Loaded::NoHolder
    } else {
        Loaded::Unavailable
    })
}

#[cfg(test)]
mod tests;
