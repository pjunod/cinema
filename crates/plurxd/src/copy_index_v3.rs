//! The reader of published v3 copy indexes.
//!
//! VOD planning asks here first. A healthy head for this node's exact source
//! key is used as it stands; a healthy head another origin published for the
//! same catalogue file is used only when this node's own full-source
//! attestation equals the variant's source digest. The variant's bytes come
//! from this node's v3 root, or from a listed holder over the peer route, and
//! are verified by size and SHA-256 before they are installed, recorded as a
//! location or decoded. Nothing here reads the source, queues work or grants
//! serving authority; without a usable head the caller keeps its existing
//! index order.
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::MediaFile;
use plurx_core::segplan::FragmentIndex;
use plurx_core::store::{
    CopyIndexV3Artifact, CopyIndexV3Head, CopyIndexV3HeadState, FragmentIndexOutputIdentity, Store,
    MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
};
use plurx_core::transcode::CopyVideoOptions;

use crate::http::peer_transport::{PeerAuthMode, PeerTransport};

pub(crate) const PEER_PATH_PREFIX: &str = "/internal/media/copy-index-v3/";
const PEER_DEADLINE: Duration = Duration::from_secs(8);

/// A decoded v3 index bound to this node's current source object.
pub(crate) struct ResolvedCopyIndex {
    pub(crate) index: FragmentIndex,
    /// This node's held-object witness the index (and its HEVC proof) is
    /// bound to.
    pub(crate) object_version: String,
    /// What a rendition planned from this index is keyed by: the semantic
    /// output contract, never a blob variant or engine.
    pub(crate) rendition_cache_key: String,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The peer path of one variant: `{contract_key}/{blob_sha256}`.
pub(crate) fn peer_path(contract_key: &str, blob_sha256: &str) -> String {
    format!("{PEER_PATH_PREFIX}{contract_key}/{blob_sha256}")
}

/// Exactly a contract key and a blob digest, nothing deeper or emptier.
pub(crate) fn peer_route_key(path: &str) -> Option<(&str, &str)> {
    let (contract_key, blob_sha256) = path.strip_prefix(PEER_PATH_PREFIX)?.split_once('/')?;
    (digest(contract_key) && digest(blob_sha256)).then_some((contract_key, blob_sha256))
}

/// A head a consumer may read: healthy, and published for the generation it
/// still desires (a quarantine moves the desire before any successor exists).
fn usable(head: &CopyIndexV3Head) -> Option<(&str, &str)> {
    (head.state == CopyIndexV3HeadState::Healthy
        && head.published_generation.as_deref() == Some(head.desired_generation.as_str()))
    .then_some(())?;
    Some((head.contract_key.as_deref()?, head.blob_sha256.as_deref()?))
}

/// Resolve the v3 index for one copy of `file` on this node, or `None` when no
/// usable head, holder or valid blob exists.
pub(crate) async fn resolve(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    runtime_cache: &Path,
    file: &MediaFile,
    video: CopyVideoOptions,
) -> Result<Option<ResolvedCopyIndex>, String> {
    // The cluster's shared catalogue identity, as the publisher keyed it.
    let namespace = store
        .instance_id()
        .await
        .map_err(|error| format!("reading the catalogue namespace: {error}"))?;
    let heads = store
        .copy_index_v3_healthy_heads(&namespace, file.id)
        .await
        .map_err(|error| format!("reading copy index heads: {error}"))?;
    if heads.is_empty() {
        return Ok(None);
    }
    // This node's current object: a stat checked against the scanner's facts.
    let object_version = crate::fragment_index_cluster::inspect_source(file).await?;
    let identity = crate::fragindex::identity_for(file, video);
    let own = crate::copy_analysis::source_for_copy(
        &namespace,
        file,
        &identity.argv_fingerprint,
        &object_version,
        node_id,
    )
    .map_err(|error| format!("deriving the copy source: {error}"))?;
    let own_key = plurx_core::store::copy_index_v3_source_key(&namespace, &own)
        .map_err(|error| format!("deriving the copy source key: {error}"))?;
    // This node's exact key first; then other origins' heads of the same
    // catalogue file, video identity and contract.
    let (exact, foreign): (Vec<_>, Vec<_>) = heads
        .iter()
        .filter(|head| {
            head.source_size == file.size
                && head.source_mtime == file.mtime
                && head.raw_video_identity == identity.argv_fingerprint
                && head.contract_version == own.contract_version
        })
        .partition(|head| head.source_key == own_key);
    let root = crate::copy_analysis::v3_root(runtime_cache);
    let mut attested: Option<Option<String>> = None;
    for (head, is_exact) in exact
        .into_iter()
        .map(|head| (head, true))
        .chain(foreign.into_iter().map(|head| (head, false)))
    {
        let Some((contract_key, blob_sha256)) = usable(head) else {
            continue;
        };
        let artifact = match store
            .copy_index_v3_artifact(contract_key, blob_sha256)
            .await
        {
            Ok(Some(artifact))
                if artifact.catalog_namespace == namespace
                    && artifact.file_id == file.id
                    && artifact.contract_version == head.contract_version =>
            {
                artifact
            }
            Ok(_) => continue,
            Err(error) => return Err(format!("reading the copy index variant: {error}")),
        };
        if !is_exact {
            // Another origin's opaque object version proves nothing here. Its
            // index describes this node's bytes only when this node's own
            // full-source receipt names the same content.
            if attested.is_none() {
                attested =
                    Some(attested_source_sha256(store, node_id, file, &object_version).await?);
            }
            if attested.as_ref().and_then(Option::as_deref) != Some(artifact.source_sha256.as_str())
            {
                continue;
            }
        }
        let Some(blob) = load_blob(store, membership, node_id, &root, &artifact).await? else {
            continue;
        };
        match decode_verified(&blob, &artifact) {
            Ok(mut index) => {
                index.source = identity.clone();
                if let Some(proof) = index.promotion.hevc_configuration.as_mut() {
                    if !proof.bind_equivalent_source(
                        &artifact.source_sha256,
                        node_id,
                        &object_version,
                    ) {
                        tracing::warn!(
                            file_id = file.id,
                            contract_key,
                            "copy index v3 HEVC proof is not bound to its attested source"
                        );
                        continue;
                    }
                }
                return Ok(Some(ResolvedCopyIndex {
                    index,
                    object_version,
                    rendition_cache_key: format!("copy-index-v3:{contract_key}"),
                }));
            }
            Err(reason) => {
                // Refused, not repaired: the caller falls back to its next
                // index path and this head stays as published.
                tracing::warn!(
                    file_id = file.id,
                    contract_key,
                    blob_sha256,
                    %reason,
                    "copy index v3 variant refused"
                );
            }
        }
    }
    Ok(None)
}

/// This node's full-file SHA-256 of its current object, from the receipts the
/// v3 publication itself accepts: a regime-1 attestation, or a full (never
/// sampled) v2 source receipt.
async fn attested_source_sha256(
    store: &dyn Store,
    node_id: &str,
    file: &MediaFile,
    object_version: &str,
) -> Result<Option<String>, String> {
    let receipt = store
        .copy_index_source_attestation(node_id, file.id, object_version)
        .await
        .map_err(|error| format!("reading the source attestation: {error}"))?;
    if let Some(receipt) = receipt
        .filter(|receipt| receipt.source_size == file.size && receipt.source_mtime == file.mtime)
    {
        return Ok(Some(receipt.source_sha256));
    }
    let full = crate::fragment_index_cluster::full_copy_object_version(object_version);
    Ok(store
        .fragment_index_source(node_id, file.id, &full)
        .await
        .map_err(|error| format!("reading the source receipt: {error}"))?
        .filter(|receipt| receipt.source_size == file.size && receipt.source_mtime == file.mtime)
        .map(|receipt| receipt.source_sha256.to_ascii_lowercase()))
}

fn blob_matches(blob: &[u8], artifact: &CopyIndexV3Artifact) -> bool {
    blob.len() <= MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES
        && i64::try_from(blob.len()).is_ok_and(|bytes| bytes == artifact.bytes)
        && plurx_core::store::cluster_fragment_index_blob_sha256(blob) == artifact.blob_sha256
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
        return Err("sealed without whole-title HDR10 metadata".into());
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

async fn read_local(
    root: &Path,
    artifact: &CopyIndexV3Artifact,
) -> Result<Option<Vec<u8>>, String> {
    let path = crate::copy_analysis::v3_blob_path(root, &artifact.blob_sha256)
        .ok_or("invalid v3 blob digest")?;
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("stat {}: {error}", path.display())),
    };
    if u64::try_from(artifact.bytes).ok() != Some(metadata.len()) {
        return Err(format!(
            "copy index v3 blob size is {}, expected {}",
            metadata.len(),
            artifact.bytes
        ));
    }
    let blob = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if !blob_matches(&blob, artifact) {
        return Err("copy index v3 blob checksum or size mismatch".into());
    }
    Ok(Some(blob))
}

/// Remove this node's copy of a variant that failed verification.
pub(crate) async fn remove_local(root: &Path, blob_sha256: &str) {
    let Some(path) = crate::copy_analysis::v3_blob_path(root, blob_sha256) else {
        return;
    };
    let Ok(_eviction) = crate::playback_cache::begin_eviction(&path) else {
        return;
    };
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(blob_sha256, %error, "removing a corrupt copy index v3 blob"),
    }
}

/// The variant's exact bytes: this node's verified blob, or one hydrated from
/// a listed holder. A hydrating node installs under the publisher's staged
/// write and disk budget and then records itself as a verified location.
async fn load_blob(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    root: &Path,
    artifact: &CopyIndexV3Artifact,
) -> Result<Option<Vec<u8>>, String> {
    let path = crate::copy_analysis::v3_blob_path(root, &artifact.blob_sha256)
        .ok_or("invalid v3 blob digest")?;
    let _pin = crate::playback_cache::pin(&path)
        .map_err(|error| format!("copy index v3 blob is busy: {error}"))?;
    match read_local(root, artifact).await {
        // The publisher's location was recorded with its publication, and a
        // hydrator's when it installed; a start writes nothing here.
        Ok(Some(blob)) => return Ok(Some(blob)),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                blob_sha256 = artifact.blob_sha256,
                %error,
                "discarding a corrupt local copy index v3 blob"
            );
            remove_local(root, &artifact.blob_sha256).await;
            // Best-effort: the corrupt blob is gone and cannot be served; a
            // peer that asks is answered 404 and retires the row itself.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetCorruptLocalIndex,
                crate::store_result::Discard::BestEffort,
                store
                    .forget_copy_index_v3_location(
                        &artifact.contract_key,
                        &artifact.blob_sha256,
                        node_id,
                    )
                    .await,
            );
        }
    }
    let Some(membership) = membership else {
        return Ok(None);
    };
    let locations = store
        .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
        .await
        .map_err(|error| format!("listing copy index holders: {error}"))?;
    if !locations.iter().any(|location| location.node_id != node_id) {
        return Ok(None);
    }
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
    let path = peer_path(&artifact.contract_key, &artifact.blob_sha256);
    for location in locations {
        let Some((base, _)) = peers.get(&location.node_id) else {
            continue;
        };
        let response = transport
            .request(
                &location.node_id,
                base,
                reqwest::Method::GET,
                &path,
                Vec::new(),
                tokio::time::Instant::now() + PEER_DEADLINE,
                MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await;
        let Ok(response) = response else { continue };
        let missing = response.status == reqwest::StatusCode::NOT_FOUND;
        if !missing && !response.status.is_success() {
            continue;
        }
        if missing || !blob_matches(&response.body, artifact) {
            // Best-effort: this holder is already excluded from this attempt
            // and nothing it sent was installed.
            crate::store_result::observe(
                if missing {
                    crate::store_result::Operation::ForgetMissingPeerIndex
                } else {
                    crate::store_result::Operation::ForgetCorruptPeerIndex
                },
                crate::store_result::Discard::BestEffort,
                store
                    .forget_copy_index_v3_location(
                        &artifact.contract_key,
                        &artifact.blob_sha256,
                        &location.node_id,
                    )
                    .await,
            );
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
        // durable. A refusal means the head moved on; the bytes still decode.
        let recorded = store
            .put_copy_index_v3_location(
                &artifact.contract_key,
                &artifact.blob_sha256,
                node_id,
                artifact.bytes,
                crate::state::clock_ms(),
            )
            .await
            .map_err(|error| format!("recording the copy index location: {error}"))?;
        tracing::info!(
            file_id = artifact.file_id,
            contract_key = artifact.contract_key,
            blob_sha256 = artifact.blob_sha256,
            holder = location.node_id,
            recorded,
            "copy index v3 hydrated from a peer"
        );
        return Ok(Some(response.body));
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
