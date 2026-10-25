//! Requested heap accounting for the immutable metadata retained by one VOD
//! rendition. Finite reader/demand table Vec allocations and their mutable
//! payloads use the separate client-state partition. This is not a process-RSS
//! bound: allocator bookkeeping, Tokio internals and native process memory
//! are outside this requested-capacity accounting.

use super::{reader::Recipe, Rendition};
use crate::renditiondir::RenditionDir;
use plurx_core::segplan::{FragmentIndex, SegmentPlan};
use std::path::PathBuf;

/// One terminal owner, its registry entry, and bounded key copies retained
/// by the registered cleanup and the existing maintenance snapshot. The
/// BTree envelope uses Rust 1.97.1's eleven reserved key/value slots plus
/// parent/edge storage, with at most two nodes per live entry. Unlike a
/// HashMap, removed historical entries cannot leave a large sparse table.
#[cfg(target_os = "linux")]
pub(super) fn closing_owner_heap_bytes(key_capacity: usize) -> Option<usize> {
    let entry = std::mem::size_of::<String>().checked_add(std::mem::size_of::<
        std::sync::Arc<super::shared::FiniteClosingOwner>,
    >())?;
    let nodes = entry
        .checked_mul(11)?
        .checked_add(16 * std::mem::size_of::<usize>())?
        .checked_mul(2)?;
    std::mem::size_of::<super::shared::FiniteClosingOwner>()
        .checked_add(2 * std::mem::size_of::<usize>())?
        .checked_add(nodes)?
        .checked_add(key_capacity.checked_mul(3)?)
}

pub(super) fn recipe_heap_bytes(recipe: &Recipe) -> Option<usize> {
    let mut bytes = recipe.file.requested_heap_bytes()?;
    if let Some(value) = &recipe.retained_logical {
        bytes = bytes.checked_add(value.requested_heap_bytes()?)?;
    }
    if let Some(value) = &recipe.audio_delivery {
        bytes = bytes.checked_add(value.requested_heap_bytes()?)?;
    }
    if let Some(value) = &recipe.source_object_version {
        bytes = bytes.checked_add(value.capacity())?;
    }
    if let Some(value) = &recipe.cluster_cache_key {
        bytes = bytes.checked_add(value.capacity())?;
    }
    if let Some(value) = &recipe.encoding {
        // Include the separately allocated Arc payload and its recursively
        // owned immutable recipes. Mutable producer/admission state remains
        // explicitly outside this metadata partition.
        bytes = bytes
            .checked_add(value.requested_heap_bytes()?)?
            .checked_add(2 * std::mem::size_of::<usize>())?;
    }
    Some(bytes)
}

/// Conservative requested-capacity projection before constructing a
/// `Rendition` and its `Manifest`. The actual post-construction measurement
/// must still fit the lease before the rendition is published.
pub(super) fn projected_requested_heap_bytes(
    recipe: &Recipe,
    index: Option<&FragmentIndex>,
    plan: &SegmentPlan,
    key: &str,
    output_path: &PathBuf,
) -> Option<usize> {
    let mut bytes = std::mem::size_of::<Rendition>()
        .checked_add(2 * std::mem::size_of::<usize>())? // Arc strong/weak counters
        .checked_add(key.len())?
        .checked_add(RenditionDir::projected_heap_bytes(output_path.capacity())?)?
        .checked_add(recipe_heap_bytes(recipe)?)?
        .checked_add(plan.requested_heap_bytes()?)?;
    if let Some(index) = index {
        bytes = bytes.checked_add(index.requested_heap_bytes()?)?;
    }

    // Render the same deterministic playlist during planning and observe its
    // requested capacity. The post-build fit still checks the actual retained
    // allocation; no guessed bytes-per-segment formula grants capacity.
    let playlist_capacity = plan.playlist().capacity();
    // Manifest owns a separate plan clone and a fixed SegState per entry.
    bytes = bytes
        .checked_add(plan.requested_heap_bytes()?)?
        .checked_add(
            plan.len()
                .checked_mul(std::mem::size_of::<crate::titlestore::SegState>())?,
        )?
        .checked_add(playlist_capacity)?
        .checked_add(plan.len().checked_mul(std::mem::size_of::<Option<u64>>())?)?
        // Explicit identity subpartition target. Serialized input length is
        // not a proof of decoded heap size: live capacities must fit before
        // R-to-S, while any producer-phase overshoot remains fully R charged.
        .checked_add(64 * 1024)?
        .checked_add(std::mem::size_of::<
            plurx_core::vod_generation::identity::InitIdentity,
        >())?;
    let source_version = recipe.source_object_version.as_ref().or_else(|| {
        recipe
            .encoding
            .as_ref()
            .map(|encoding| &encoding.source_object_version)
    })?;
    bytes = bytes.checked_add(
        crate::vodserve::output_measurement::projected_observation_bytes(
            source_version.capacity(),
            key.len(),
            playlist_capacity,
            64 * 1024,
            64 * 1024,
            plan.len(),
        )?,
    )?;
    Some(bytes)
}

/// Measure the live immutable rendition after construction, including the
/// manifest and publication-version capacities. A busy lock yields `None` so
/// callers cannot treat an incomplete observation as a successful fit.
fn immutable_rendition_bytes(rendition: &Rendition) -> Option<usize> {
    let mut bytes = std::mem::size_of::<Rendition>()
        .checked_add(2 * std::mem::size_of::<usize>())?
        .checked_add(rendition.key.capacity())?
        .checked_add(rendition.dir.requested_heap_bytes()?)?
        .checked_add(rendition.playlist.capacity())?
        .checked_add(recipe_heap_bytes(&rendition.recipe)?)?
        .checked_add(rendition.plan.requested_heap_bytes()?)?;
    if let Some(index) = &rendition.index {
        bytes = bytes.checked_add(index.requested_heap_bytes()?)?;
    }
    if let Some(source) = &rendition.source {
        bytes = bytes.checked_add(source.requested_heap_bytes())?;
    }
    Some(bytes)
}

fn measured_rendition_bytes(
    rendition: &Rendition,
    manifest: usize,
    identity: usize,
    observation: usize,
    versions_capacity: usize,
) -> Option<usize> {
    immutable_rendition_bytes(rendition)?
        .checked_add(manifest)?
        .checked_add(identity)?
        .checked_add(observation)?
        .checked_add(versions_capacity.checked_mul(std::mem::size_of::<Option<u64>>())?)
}

/// Construction-only measurement, before this Arc has been published.
pub(super) fn actual_rendition_bytes(rendition: &Rendition) -> Option<usize> {
    let manifest = rendition.manifest.try_lock().ok()?.requested_heap_bytes()?;
    let identity = {
        let state = rendition.identity.try_lock().ok()?;
        match &state.identity {
            Some(value) => value.requested_heap_bytes()?,
            None => 0,
        }
    };
    let observation = rendition
        .output_measurement
        .lock()
        .ok()?
        .requested_heap_bytes()?;
    let versions = rendition.publication_versions.lock().ok()?.capacity();
    measured_rendition_bytes(rendition, manifest, identity, observation, versions)
}

/// The sole finite event consumer calls this after draining events. Ordinary
/// HTTP metadata readers may hold these locks; contention is not a failed fit.
pub(super) async fn live_rendition_bytes(rendition: &Rendition) -> Option<usize> {
    let manifest = rendition.manifest.lock().await.requested_heap_bytes()?;
    let identity = {
        let state = rendition.identity.lock().await;
        match &state.identity {
            Some(value) => value.requested_heap_bytes()?,
            None => 0,
        }
    };
    let observation = rendition
        .output_measurement
        .lock()
        .ok()?
        .requested_heap_bytes()?;
    let versions = rendition.publication_versions.lock().ok()?.capacity();
    measured_rendition_bytes(rendition, manifest, identity, observation, versions)
}
