//! Requested Rust storage for the selected five-member copy close preflight.
//! This is additive to the original profile, not an explanation of its old
//! 256KiB term. Allocator, BTree node, native and outer task peaks stay Unknown.
use super::{CopyCloseTicket, CopyVodOwner, CopyVodRequest, NodeResourceRuntime};
use crate::resource_custody::{codec_v2 as v2, BootIdentity};
use std::future::Future;
use std::mem::size_of;
use std::time::Instant;

/// A copy-specific pre-serialization limit. The length-counting writer must
/// reject EVERY envelope and aggregate settlement above this cap
/// before invoking the existing full validators. Codec validation remains
/// authoritative; this cap does not make a DTO a native capability.
pub(in crate::adaptive_resources::runtime) const MAX_VALIDATION_JSON_BYTES: usize = 64 * 1024;
const MEMBERS: usize = 5;
const NATIVE_UNKNOWN_TEXT_BYTES: usize = 256; // codec::text, three NativeValue fields
const EXECUTABLE_BINDING_BYTES: usize = "software-avc-executable-sha256:".len() + 64;
const PID_START_TEXT_BYTES: usize = 10 + 1 + 20; // u32 PID, colon, u64 start marker
const ROOT_EVIDENCE: &str =
    "original fixed copy root waited; original pipes, source parents and Child/ChildJob closed";

fn sum<const N: usize>(values: [usize; N]) -> Option<usize> {
    values.into_iter().try_fold(0usize, usize::checked_add)
}
fn times(value: usize, count: usize) -> Option<usize> {
    value.checked_mul(count)
}
fn digits(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        value.ilog10() as usize + 1
    }
}
fn signed_digits(value: i64) -> usize {
    digits(value.unsigned_abs()) + usize::from(value.is_negative())
}

// Type projection only. These factories are NEVER CALLED: no fabricated
// reference, CopyVodOwner, ticket or native owner is constructed to size them.
fn prepare_frame<F, Fut>(_factory: F) -> usize
where
    F: FnOnce(&'static mut CopyVodOwner, bool, Instant) -> Fut,
    Fut: Future,
{
    size_of::<Fut>()
}
fn finish_frame<F, Fut>(_factory: F) -> usize
where
    F: FnOnce(&'static mut CopyCloseTicket, Instant) -> Fut,
    Fut: Future,
{
    size_of::<Fut>()
}

/// Same sequential outer cleanup phases, including their actual nested async
/// frames. finish_frame includes the actual nested claim-guarded
/// finish_copy_close future, including its application/response waits and
/// deferred full preflight. Their maximum is requested Rust storage, not a peak.
fn cleanup_frame_bytes() -> usize {
    prepare_frame(CopyVodOwner::prepare_retirement)
        .max(finish_frame(CopyCloseTicket::finish_retirement))
}

pub(in crate::adaptive_resources::runtime) fn requested_bytes(
    runtime: &NodeResourceRuntime,
    request: &CopyVodRequest,
    selected_directory_capacity: usize,
) -> Option<usize> {
    let root = runtime.rendition_root.as_ref()?.to_str()?;
    let binary = request.command.executable();
    let binary_identity = binary.source_identity();
    let root_domain = runtime.owner.daemon_process.domain.len();
    let boot_text = match &runtime.owner.os_boot {
        BootIdentity::Known { provider, .. } => provider.len(),
        BootIdentity::Unknown { reason } => reason.len(),
    };
    // The constructor fixes FilesystemDomain.mount_namespace to Known. Both
    // original source observations must match that same domain. Other native
    // device/inode/mount values can carry validated <=256-byte Unknown text.
    let filesystem_domain_heap = boot_text;
    let native_heap = sum([filesystem_domain_heap, times(NATIVE_UNKNOWN_TEXT_BYTES, 3)?])?;
    let media_association = sum([
        request.source.media_file_id.len(),
        request.source.object_version.len(),
        request.recipe_source_binding.len(),
    ])?;
    // Exact s1 serialized text length, without allocating a String or statting
    // a replacement path. Three unsigned and four signed decimal fields.
    let binary_s1 = sum([
        "s1".len() + 7,
        digits(binary_identity.dev),
        digits(binary_identity.ino),
        digits(binary_identity.size),
        signed_digits(binary_identity.mtime),
        signed_digits(binary_identity.mtime_nsec),
        signed_digits(binary_identity.ctime),
        signed_digits(binary_identity.ctime_nsec),
    ])?;
    let binary_association = sum([times(EXECUTABLE_BINDING_BYTES, 2)?, binary_s1])?;
    let component_count = root.strip_prefix('/')?.split('/').count();
    let component_text = root
        .strip_prefix('/')?
        .split('/')
        .try_fold(0usize, |bytes, component| {
            bytes.checked_add(component.len())
        })?;
    // Copies of the same selected ScratchPlanV3 metadata, not its file table.
    let scratch_plan_heap = sum([
        request.rendition_id.len(),
        filesystem_domain_heap,
        times(component_count, size_of::<String>())?,
        component_text,
        root.len(),
        request.rendition_id.len(),
        selected_directory_capacity,
    ])?;
    // Both legitimate recovery branches are metadata. The chosen namespace
    // and digest bytes are known before R; no physical recovery is inferred.
    let namespace_recovery = match &runtime.owner.platform_domain.namespace {
        Some(namespace) => namespace.len().checked_add(EXECUTABLE_BINDING_BYTES)?,
        None => 0,
    };
    let recovery_heap = namespace_recovery
        .max("copy root namespace unavailable".len())
        .max("copy root continuity unavailable".len());
    let legacy_plan_heap = sum([root_domain, binary.path().as_os_str().len(), recovery_heap])?;
    let task_heap = sum([
        media_association,
        times(3, size_of::<v2::OwnerAllocation>())?,
    ])?;
    let plan_heap = sum([
        times(media_association, 2)?,
        binary_association,
        times(filesystem_domain_heap, 5)?, // source domain+recovery (four), scratch (one)
        times(4, size_of::<v2::SourceCopy>())?,
        times(3, size_of::<v2::OwnerAllocation>())?,
        times(2, size_of::<v2::Id>())?, // task dependencies
        times(3, size_of::<String>())?, // three one-element protected-path vectors
        request.source.path.as_os_str().len(),
        binary.path().as_os_str().len(),
        selected_directory_capacity,
        // scratch_plan_heap includes one domain already counted above.
        scratch_plan_heap.checked_sub(filesystem_domain_heap)?,
        legacy_plan_heap,
    ])?;
    let source_observed_heap = sum([
        media_association,
        binary_association,
        times(4, size_of::<v2::SourceCopy>())?,
        times(native_heap, 2)?,
    ])?;
    // format!("{}:{}") requests bounded Vec/String growth. Charging 3L
    // additionally covers the old/new requested allocation overlap; successful
    // wait or a reconstructed identity string still grants no close authority.
    let root_observed_heap = sum([times(root_domain, 2)?, times(PID_START_TEXT_BYTES, 3)?])?;
    // At most asset.directory/current/parent. Each successful bootstrap IO
    // registers one held copy, but reserve the codec's allowed sixteen IDs
    // per directory so failure preflight cannot rely on an empty projection.
    let scratch_observed_heap = sum([
        times(sum([native_heap, times(16, size_of::<v2::Id>())?])?, 3)?,
        request.rendition_id.len(), // possible_child component
    ])?;
    let observation_heap = sum([
        source_observed_heap,
        task_heap,
        root_observed_heap,
        scratch_observed_heap,
    ])?;
    let legacy_fact_heap = sum([
        root_domain,
        times(PID_START_TEXT_BYTES, 3)?,
        ROOT_EVIDENCE.len(),
    ])?;
    let settlement_heap = sum([
        times(4, size_of::<v2::CopyClosed>())?,
        legacy_fact_heap.max(legacy_plan_heap),
        task_heap,
        times(3, size_of::<v2::OwnerClosed>())?,
        scratch_plan_heap,
    ])?;
    // serde_json 1.0.151 ser.rs2213 starts Vec::with_capacity(128).
    // Pinned Rust1.97.1 raw_vec::grow_amortized502..531 requests
    // max(2*old_capacity, required). After the counting gate, requested new
    // capacity <=2L and old+new overlap <=3L. Nested scratch-state encoding
    // fits its enclosing envelope; validators drop each buffer before the
    // next, so serialize peak is one buffer, not six summed buffers. Allocator
    // metadata, temporary internal realloc storage and RSS remain Unknown.
    let serializer_requested = times(MAX_VALIDATION_JSON_BYTES, 3)?.max(128);
    // v2::Id derives serde(into = "String"): From<Id> calls Uuid::to_string.
    // This scalar lives DURING both the counting pass and Vec encoding, so it
    // is additional to the JSON buffer. uuid1.24 Display emits one 36-byte
    // hyphenated slice; Rust1.97.1 ToString starts an empty String, whose one
    // write requests exactly that payload. Include its inline String and
    // formatting byte array. Direct Uuid serde uses only encode_buffer on the
    // stack. Scalar visits are sequential: no ID-vector-wide String copies.
    let uuid_text = uuid::fmt::Hyphenated::LENGTH;
    let scalar_requested = sum([uuid_text, size_of::<String>(), uuid_text])?
        .max(std::mem::size_of_val(&uuid::Uuid::encode_buffer()));
    // Existing full settlement validation retains <=five member IDs plus
    // the task's expected/actual three owner IDs each. Source validation has
    // only three two-copy ID sets; scratch projection has <=sixteen borrowed
    // copy-ID keys. Count the largest live key payload, never guess private
    // BTree node/header allocation or turn this component into a peak claim.
    let validator_keys =
        times(MEMBERS + 3 + 3, size_of::<v2::Id>())?.max(times(16, size_of::<&v2::Id>())?);
    sum([
        cleanup_frame_bytes(),
        size_of::<Vec<v2::Envelope>>(),
        times(MEMBERS, size_of::<v2::Envelope>())?,
        size_of::<v2::Settlement>(),
        times(MEMBERS, size_of::<v2::MemberDisposition>())?,
        plan_heap,
        observation_heap,
        settlement_heap,
        serializer_requested,
        scalar_requested,
        validator_keys,
    ])
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::adaptive_resources::{Metric, ResourceVector};
    use std::time::Duration;

    #[tokio::test]
    async fn enlarged_close_metadata_is_charged_before_any_copy_reservation_or_native_entry() {
        let root = tempfile::tempdir().expect("actual runtime root");
        let (runtime, mut request) =
            NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
        let original = runtime
            .copy_vod_known_memory_bytes(&request)
            .expect("pure original requested profile");
        // Valid immutable metadata, independent of media/parser payload. It
        // must be counted in the retained plans AND overlapping close DTOs.
        request.recipe_source_binding.push_str(&"m".repeat(1024));
        assert!(request.recipe_source_binding.len() <= 4096);
        let enlarged = runtime
            .copy_vod_known_memory_bytes(&request)
            .expect("same pure profile with larger original association");
        assert!(
            enlarged > original + 1024,
            "a shared byte count must include simultaneous owned association copies"
        );
        let Metric::Known(mut resources) = request.demand.resources else {
            panic!("actual known fixture demand");
        };
        resources.peak_memory_bytes = original;
        request.demand.resources = Metric::Known(resources);
        let result = runtime
            .prepare_copy_vod(request, Instant::now() + Duration::from_secs(60))
            .await;
        let error = match result {
            Ok(_) => panic!("old profile may not admit larger close metadata"),
            Err(error) => error,
        };
        assert!(error.definitely_before_reservation());
        assert_eq!(
            runtime.accounting().expect("original ledger").used,
            ResourceVector::default(),
            "no R or native acquisition from insufficient pre-R profile"
        );
    }
}
