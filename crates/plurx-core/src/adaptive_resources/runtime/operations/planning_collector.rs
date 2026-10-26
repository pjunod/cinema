//! Original admitted P collector. The operations book retains and joins the
//! task running this function; a caller-facing future never owns the task or
//! any raw native handle. Source and sealed ELF aliases die before return.
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use crate::process::fixed_planning_engine::{
    collect_fixed_engine, FixedEngineNativeCompletion, FixedEngineNativeFailure,
};
use crate::process::fixed_probe::{
    AdmittedProbeCompletion, DecodeFactCache, DecodeFactError, DecodeFactSource,
    DecodeProbeIdentity, ProbeStreamSelection,
};
use crate::process::ChildWork;
#[cfg(target_os = "linux")]
use crate::resource_custody::codec_v2 as v2;
use crate::transcode::DecodeCatalogMetadata;

use super::source_files::RetainedSourceLease;

/// Static native shape derived from the two fixed original collectors. These
/// are component counts and explicit byte limits, not claims about peak RSS.
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources::runtime) struct FixedPlanningNativeProfile {
    pub original_root_slots: u64,
    pub retained_parent_file_slots: u64,
    pub sealed_image_file_slots: u64,
    pub inherited_source_file_slots: u64,
    pub engine_file_slots: u64,
    pub engine_inherited_child_file_slots: u64,
    pub pipe_reader_slots: u64,
    pub max_probe_image_bytes: u64,
    pub max_probe_version_stdout_bytes: u64,
    pub max_probe_document_stdout_bytes: u64,
    pub max_probe_document_stderr_bytes: u64,
    pub engine_hash_buffer_bytes: u64,
    pub max_engine_command_output_bytes: u64,
    pub retained_custody_bytes: u64,
}

#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources::runtime) fn fixed_planning_native_profile(
) -> FixedPlanningNativeProfile {
    let probe = crate::process::fixed_probe::fixed_probe_native_profile();
    let engine = crate::process::fixed_planning_engine::fixed_engine_native_profile();
    FixedPlanningNativeProfile {
        original_root_slots: probe.original_roots + engine.original_roots,
        retained_parent_file_slots: probe.retained_parent_files,
        sealed_image_file_slots: probe.sealed_image_files,
        inherited_source_file_slots: probe.inherited_child_files,
        engine_file_slots: engine.file_slots,
        engine_inherited_child_file_slots: engine.inherited_child_file_slots,
        pipe_reader_slots: probe.pipe_readers + engine.pipe_readers,
        max_probe_image_bytes: probe.max_image_bytes,
        max_probe_version_stdout_bytes: probe.max_version_stdout_bytes,
        max_probe_document_stdout_bytes: probe.max_document_stdout_bytes,
        max_probe_document_stderr_bytes: probe.max_document_stderr_bytes,
        engine_hash_buffer_bytes: engine.hash_buffer_bytes,
        max_engine_command_output_bytes: engine.max_command_output_bytes,
        retained_custody_bytes: probe.retained_custody_bytes + engine.retained_custody_bytes,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::adaptive_resources::runtime) async fn run_fixed_probe(
    source_lease: RetainedSourceLease,
    observed_source_object_version: String,
    configured_probe_locator: String,
    cache: Arc<DecodeFactCache>,
    catalog: DecodeCatalogMetadata,
    selected_stream: ProbeStreamSelection,
    work: ChildWork,
    deadline: Instant,
    version_close_operation_id: Uuid,
    document_close_operation_id: Uuid,
) -> Result<AdmittedProbeCompletion, crate::process::fixed_probe::AdmittedProbeNativeFailure> {
    if !cfg!(target_os = "linux")
        || Instant::now() >= deadline
        || version_close_operation_id.is_nil()
        || document_close_operation_id.is_nil()
        || version_close_operation_id == document_close_operation_id
    {
        return Err(
            crate::process::fixed_probe::AdmittedProbeNativeFailure::before_entry(
                DecodeFactError::Deadline,
            ),
        );
    }
    // This image owner is created inside the already registered P task.
    // Discovery's version process and any timed-out hash workers are joined
    // by its private gates before this function can finish.
    let image = DecodeProbeIdentity::discover_admitted(
        &configured_probe_locator,
        version_close_operation_id,
    )
    .await?;
    let source = DecodeFactSource::new(
        source_lease.shared_file(),
        Arc::new(tokio::sync::Semaphore::new(1)),
        work,
    );
    let remaining = deadline.saturating_duration_since(Instant::now());
    // Even an expired window goes through the admitted image owner, which
    // retains the original image/root cleanup instead of dropping custody.
    let completion = image
        .collect_source(
            &cache,
            source,
            &catalog,
            selected_stream,
            remaining,
            &observed_source_object_version,
            document_close_operation_id,
        )
        .await?;
    // Drop the task's alias to the SAME File before the book closes the
    // SourceCell parent. No descriptor is returned to the daemon bridge.
    drop(source_lease);
    Ok(completion)
}

/// The second original P task. Its own JoinHandle remains in P's operations
/// book from before entry until every native root and File owner is closed.
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources::runtime) async fn run_fixed_engine(
    task: v2::FixedPlanningEngineTaskV1,
    boot_id: Option<Uuid>,
    deadline: Instant,
) -> Result<FixedEngineNativeCompletion, FixedEngineNativeFailure> {
    collect_fixed_engine(task, boot_id, deadline).await
}
