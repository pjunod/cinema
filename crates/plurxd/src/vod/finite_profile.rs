//! Compile the frozen daemon recipe into the finite software-AVC request.
//! Planning only: source/executable custody and all output I/O happen later.

use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteOutputLimits, FiniteResponseBuffer, FiniteSourceRequest, SoftwareAvcCommand,
    SoftwareAvcPumpPlan, SoftwareVodRequest,
};
use plurx_core::adaptive_resources::{ResourceCapacity, ResourceVector, WorkloadDemand};
use plurx_core::process::observation::Metric;
use plurx_core::transcode::{Pacing, TranscodeExecution};
use plurx_core::vod_generation::GenerationMemoryLimits;
use std::ffi::OsString;

const MIB: u64 = 1024 * 1024;
const FILE_BYTES: usize = COPY_SEGMENT_MAX_BYTES;
const READ_CHUNK_BYTES: usize = 128 * 1024;
const RETAINED_FRAGMENTS: usize = 64;
const RETAINED_FRAGMENT_MEMORY_BYTES: usize = 256 * 1024 * 1024;
const INIT_MEMORY_BYTES: usize = 4 * 1024 * 1024;
const MERGE_WORKSPACE_BYTES: usize = 128 * 1024 * 1024;
const MERGE_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRACKS: usize = 8;
const MAX_SAMPLES: usize = 262_144;
const PREFIX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const SPLIT_MEMORY_BYTES: usize = 128 * 1024 * 1024;
const PUBLISHED_BATCH_BYTES: usize = 128 * 1024 * 1024;
const DIAGNOSTIC_BYTES: usize = 16 * 1024;
const METADATA_BYTES: usize = 64 * 1024;
const READER_LEASES: u32 = 8;
const LIVE_HANDLES: u32 = 24;
const RESIDENT_MEMORY_BYTES: u64 = 128 * MIB;
const BOOTSTRAP_MEMORY_BYTES: u64 = MIB;
/// An explicit, independently enforced partition for finite client tables,
/// reader ledgers and materialization-clock tasks. Table admission derives
/// its slot count from this byte partition, not from a guessed viewer count.
pub(super) const CLIENT_STATE_BYTES: u64 = 16 * MIB;
const ALLOCATION_UNIT_BYTES: u64 = 4096;
const PER_OBJECT_RESERVE_BYTES: u64 = 16 * 1024;
const CHILD_DIRECTORY_RESERVE_BYTES: u64 = 64 * 1024;
const PARENT_ENTRY_RESERVE_BYTES: u64 = 64 * 1024;
const STATIC_MANIFEST_FILE_BYTES: u64 = 4 * MIB;

/// Expand the already compiled finite output plan for one additional
/// `complete-v1.json` in the SAME scratch directory before core admission.
/// Core separately charges its manifest serialization/validation workspace;
/// this function accounts only for the retained native file and storage.
#[cfg(target_os = "linux")]
pub(super) fn with_static_manifest_file(
    mut request: SoftwareVodRequest,
) -> Result<SoftwareVodRequest, String> {
    if request.output.one_file_logical_bytes < STATIC_MANIFEST_FILE_BYTES
        || request.output.one_io_payload_bytes < STATIC_MANIFEST_FILE_BYTES
    {
        return Err("finite static manifest exceeds the planned file or IO bound".into());
    }
    request.output.resident_file_objects = request
        .output
        .resident_file_objects
        .checked_add(1)
        .ok_or("finite static file count overflows")?;
    request.output.resident_logical_bytes = request
        .output
        .resident_logical_bytes
        .checked_add(STATIC_MANIFEST_FILE_BYTES)
        .ok_or("finite static logical bytes overflow")?;
    let extra_storage = STATIC_MANIFEST_FILE_BYTES
        .checked_add(request.output.per_object_reserve_bytes)
        .ok_or("finite static storage reserve overflows")?;
    request.output.reserved_storage_bytes = request
        .output
        .reserved_storage_bytes
        .checked_add(extra_storage)
        .ok_or("finite static storage reserve overflows")?;
    let Metric::Known(resources) = &mut request.demand.resources else {
        return Err("finite static storage requires a known resource vector".into());
    };
    resources.scratch_bytes = resources
        .scratch_bytes
        .checked_add(extra_storage)
        .ok_or("finite static scratch demand overflows")?;
    Ok(request)
}

#[cfg(target_os = "linux")]
pub(super) fn source_version(recipe: &Recipe) -> Result<String, String> {
    let encoding = recipe
        .encoding
        .as_ref()
        .ok_or_else(|| "finite software profile requires a frozen encoding".to_owned())?;
    match (
        recipe.source_object_version.as_deref(),
        Some(encoding.source_object_version.as_str()),
    ) {
        (Some(recipe_version), Some(encoding_version)) if recipe_version != encoding_version => {
            Err("recipe and encoding source versions differ".into())
        }
        (Some(version), _) | (None, Some(version)) if !version.is_empty() => Ok(version.to_owned()),
        _ => Err("finite software source requires an exact object version".into()),
    }
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::too_many_arguments,
    reason = "The finite plan binds borrowed source, key, index, segment plan, observed epoch/capacity and thread policy together before native entry; its measured projection introduces no argument allocation."
)]
pub(super) fn compile(
    shared: &Shared,
    key: &str,
    recipe: &Recipe,
    index: Option<&FragmentIndex>,
    plan: &SegmentPlan,
    limit_epoch: u64,
    capacity: ResourceCapacity,
    software_pool_threads: u64,
) -> Result<(SoftwareVodRequest, u32), String> {
    let encoding = recipe
        .encoding
        .as_ref()
        .ok_or_else(|| "finite software profile requires a frozen encoding".to_owned())?;
    if plan.timescale == 0 || plan.entries.is_empty() || key.is_empty() {
        return Err("finite software profile requires a nonempty plan and rendition key".into());
    }
    let output_path = shared.base.join(key);
    let retained_metadata_bytes = crate::vodserve::finite_metadata::projected_requested_heap_bytes(
        recipe,
        index,
        plan,
        key,
        &output_path,
    )
    .and_then(|bytes| u64::try_from(bytes).ok())
    .filter(|bytes| *bytes > 0)
    .ok_or("finite retained metadata projection overflows")?;

    let object_version = source_version(recipe)?;
    if recipe.file.size < 0 || recipe.file.mtime < 0 {
        return Err("finite software source has invalid scanner facts".into());
    }

    let cpu_ns_per_second = match capacity.cpu_ns_per_second {
        Metric::Known(value) if value > 0 => value,
        Metric::Known(_) => return Err("finite software CPU capacity is zero".into()),
        Metric::Unknown(_) => return Err("finite software CPU capacity is unknown".into()),
    };
    let software_threads = encoder_threads(
        cpu_ns_per_second,
        encoding.options.software_threads,
        software_pool_threads,
    )?;

    let mut options = encoding.options.clone();
    options.start_seconds = 0.0;
    options.start_number = 0;
    options.software_threads = Some(software_threads);
    let execution =
        TranscodeExecution::from_options(&recipe.file, &options, Pacing::unpaced(), ".")
            .map_err(|error| error.to_string())?;
    let executable = encoding
        .executable
        .finite_software_plan()
        .map_err(|error| format!("planning the selected software executable: {error}"))?;
    let loader_env = encoding
        .executable
        .finite_software_loader_env()
        .map_err(|error| format!("planning the selected executable loader environment: {error}"))?
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    let duration_seconds = plan.duration_ticks() as f64 / f64::from(plan.timescale);
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return Err("finite software plan duration is invalid".into());
    }
    let command = SoftwareAvcCommand::compile(
        &recipe.file,
        &encoding.plan,
        &execution,
        encoding.grid,
        duration_seconds,
        encoding.reorder_frames,
        encoding.source_clock,
        executable,
        encoding
            .engine
            .finite_software_snapshot(&encoding.executable)
            .map_err(|error| format!("capturing the finite engine fence: {error}"))?,
        loader_env,
        shared.runtime_cache.clone(),
    )
    .map_err(|error| format!("compiling the finite software command: {error}"))?;

    let segments = u32::try_from(plan.len())
        .map_err(|_| "finite software segment count overflows".to_owned())?;
    let resident_file_objects = segments
        .checked_add(3)
        .ok_or_else(|| "finite software resident file count overflows".to_owned())?;
    let resident_logical_bytes = u64::from(resident_file_objects)
        .checked_mul(FILE_BYTES as u64)
        .ok_or_else(|| "finite software logical output bound overflows".to_owned())?;
    let aligned_logical_bytes = align_up(resident_logical_bytes, ALLOCATION_UNIT_BYTES)?;
    let reserved_storage_bytes = aligned_logical_bytes
        .checked_add(
            u64::from(resident_file_objects)
                .checked_mul(PER_OBJECT_RESERVE_BYTES)
                .ok_or_else(|| "finite software storage reserve overflows".to_owned())?,
        )
        .and_then(|bytes| bytes.checked_add(CHILD_DIRECTORY_RESERVE_BYTES))
        .and_then(|bytes| bytes.checked_add(PARENT_ENTRY_RESERVE_BYTES))
        .ok_or_else(|| "finite software storage reserve overflows".to_owned())?;
    let response_memory_bytes = FiniteResponseBuffer::required_partition_bytes(FILE_BYTES, 2)
        .ok_or_else(|| "finite response-buffer partition overflows".to_owned())?;
    let output = FiniteOutputLimits {
        response_memory_bytes,
        retained_metadata_bytes,
        client_state_bytes: CLIENT_STATE_BYTES,
        resident_file_objects,
        resident_logical_bytes,
        reserved_storage_bytes,
        one_file_logical_bytes: FILE_BYTES as u64,
        one_io_payload_bytes: FILE_BYTES as u64,
        live_handles: LIVE_HANDLES,
        reader_leases: READER_LEASES,
        resident_memory_bytes: RESIDENT_MEMORY_BYTES,
        allocation_unit_bytes: ALLOCATION_UNIT_BYTES,
        per_object_reserve_bytes: PER_OBJECT_RESERVE_BYTES,
        child_directory_reserve_bytes: CHILD_DIRECTORY_RESERVE_BYTES,
        parent_entry_reserve_bytes: PARENT_ENTRY_RESERVE_BYTES,
        bootstrap_memory_bytes: BOOTSTRAP_MEMORY_BYTES,
    };

    let memory = GenerationMemoryLimits {
        reader: plurx_core::fmp4::FragmentReaderBounds {
            buffer_bytes: FILE_BYTES,
            unit_bytes: FILE_BYTES,
            max_tracks: MAX_TRACKS,
            max_runs: MAX_SAMPLES,
            max_samples: MAX_SAMPLES,
        },
        read_chunk_bytes: READ_CHUNK_BYTES,
        retained_fragment_memory_bytes: RETAINED_FRAGMENT_MEMORY_BYTES,
        retained_fragments: RETAINED_FRAGMENTS,
        init_memory_bytes: INIT_MEMORY_BYTES,
        segmenter: plurx_core::fmp4::FiniteSegmenterLimits {
            merge: plurx_core::fmp4::MergeMemoryLimits {
                workspace_bytes: MERGE_WORKSPACE_BYTES,
                output_bytes: MERGE_OUTPUT_BYTES,
                max_tracks: MAX_TRACKS,
                max_samples: MAX_SAMPLES,
            },
            prefix_memory_bytes: PREFIX_MEMORY_BYTES,
            split_memory_bytes: SPLIT_MEMORY_BYTES,
            published_batch_bytes: PUBLISHED_BATCH_BYTES,
            retained_fragments: RETAINED_FRAGMENTS,
        },
    };
    let pump = SoftwareAvcPumpPlan::new(
        plan.clone(),
        0,
        encoding.grid,
        super::plan::shipped_policy(plan.timescale),
        memory,
        DIAGNOSTIC_BYTES,
        METADATA_BYTES,
        encoding
            .engine
            .stable_software_digest()
            .ok_or("same-boot finite software engine identity unavailable")?
            .to_owned(),
    )
    .map_err(|error| format!("planning the finite software writer: {error}"))?;
    let known_memory = SoftwareVodRequest::known_memory_component(&command, &pump, &output, 0)
        .map_err(|error| format!("sizing the finite software memory component: {error}"))?;

    let source_count = if recipe.file.audio_streams.is_empty() {
        1
    } else {
        2
    };
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(source_count)
        .map_err(|_| "reserving finite source request entries failed".to_owned())?;
    for _ in 0..source_count {
        sources.push(FiniteSourceRequest {
            path: recipe.file.path.clone(),
            media_file_id: recipe.file.id.to_string(),
            object_version: object_version.clone(),
            scanner_size: recipe.file.size,
            scanner_mtime: recipe.file.mtime,
        });
    }
    let handles = u32::try_from(source_count)
        .ok()
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_mul(3))
        .and_then(|count| count.checked_add(LIVE_HANDLES))
        .and_then(|count| count.checked_add(3))
        // One registered engine-check worker can hold a package digest File.
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| "finite software handle demand overflows".to_owned())?;
    // This charges the selected bounded allocations. FFmpeg's additional
    // peak, parser metadata and framework/runtime overhead remain explicitly
    // unknown and are serialized by the core memory-claim policy.
    let demand = WorkloadDemand {
        resources: Metric::Known(ResourceVector {
            cpu_ns_per_second,
            peak_memory_bytes: known_memory,
            handles: u64::from(handles),
            processes: 1,
            hardware_slots: 0,
            tuner_slots: 0,
            scratch_bytes: reserved_storage_bytes,
        }),
        limit_epoch,
        trial: false,
    };

    let mut request = SoftwareVodRequest {
        rendition_id: key.to_owned(),
        generation_epoch: SoftwareVodRequest::INITIAL_GENERATION_EPOCH,
        recipe_source_binding: key.to_owned(),
        command,
        producer: pump,
        sources,
        segment_count: segments,
        encoded_process_marker: true,
        demand,
        application_owner_bytes: 0,
        output,
    };
    // Complete the SAME metadata profile before core admission. Runtime
    // knows its configured root and the exact source/executable selections;
    // construction later checks actual requested capacities against this
    // projection. Reserving a larger daemon lease after prepare is too late.
    let runtime = shared
        .resource_runtime
        .as_ref()
        .ok_or("finite resource runtime unavailable for metadata projection")?;
    let (owner_heap, output_heap) = runtime
        .software_application_metadata_projection(&request)
        .map_err(|error| format!("projecting finite application metadata: {error}"))?;
    let additional =
        super::finite_software::FiniteSoftwareProducer::projected_heap_bytes(owner_heap)
            .and_then(|bytes| bytes.checked_add(output_heap))
            .and_then(|bytes| {
                bytes.checked_add(super::finite_metadata::closing_owner_heap_bytes(key.len())?)
            })
            .and_then(|bytes| {
                bytes.checked_add(super::finite_software::projected_driver_future_bytes())
            })
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or("finite application metadata projection overflows")?;
    request.output.retained_metadata_bytes = request
        .output
        .retained_metadata_bytes
        .checked_add(additional)
        .ok_or("finite metadata partition overflows")?;
    let Metric::Known(resources) = &mut request.demand.resources else {
        return Err("finite application metadata requires a known requested component".into());
    };
    resources.peak_memory_bytes = resources
        .peak_memory_bytes
        .checked_add(additional)
        .ok_or("finite requested memory component overflows")?;
    Ok((request, software_threads))
}

#[cfg(target_os = "linux")]
fn align_up(value: u64, unit: u64) -> Result<u64, String> {
    if unit == 0 {
        return Err("finite software allocation unit is zero".into());
    }
    value
        .checked_add(unit - 1)
        .map(|rounded| rounded / unit * unit)
        .ok_or_else(|| "finite software aligned output bound overflows".into())
}

/// Encoder creation limit, separate from expected CPU service and total kernel
/// tasks. Zero pool policy means one serialized producer, not zero threads;
/// the core's unknown producer claims supply that concurrency restriction.
#[cfg(target_os = "linux")]
fn encoder_threads(
    cpu_ns_per_second: u64,
    inherited: Option<u32>,
    pool: u64,
) -> Result<u32, String> {
    if cpu_ns_per_second == 0 || inherited == Some(0) {
        return Err("finite software encoder requires positive CPU and thread limits".into());
    }
    let physical = cpu_ns_per_second
        .div_ceil(1_000_000_000)
        .min(u64::from(u32::MAX));
    let recipe = inherited.map(u64::from).unwrap_or(physical);
    let threads = physical.min(recipe);
    let threads = if pool == 0 {
        threads
    } else {
        threads.min(pool)
    };
    u32::try_from(threads).map_err(|_| "finite software encoder thread count overflows".into())
}

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::unwrap_used,
    reason = "Fixture setup and expected-refusal assertions deliberately fail the test on the opposite result; production is checked separately."
)]
mod tests {
    use super::*;

    #[test]
    fn explicit_pool_and_frozen_recipe_bound_encoder_creation_without_rewriting_cpu_service() {
        assert_eq!(encoder_threads(16_000_000_000, Some(8), 3).unwrap(), 3);
        assert_eq!(encoder_threads(16_000_000_000, Some(2), 8).unwrap(), 2);
        assert_eq!(encoder_threads(1_500_000_000, None, 8).unwrap(), 2);
    }

    #[test]
    fn serialized_zero_pool_preserves_one_producers_positive_encoder_threads() {
        assert_eq!(encoder_threads(4_000_000_000, Some(3), 0).unwrap(), 3);
        assert!(encoder_threads(0, None, 0).is_err());
        assert!(encoder_threads(4_000_000_000, Some(0), 0).is_err());
    }
}
