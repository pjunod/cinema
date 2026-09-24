use super::*;

/// The policy and source shared by the cache-claim and encoder stages of one
/// portable production attempt. Keeping these together makes it much harder
/// for a resume path to accidentally change one input between the two stages.
#[derive(Clone)]
pub(super) struct PortableProduction<'a> {
    pub(super) file: &'a plurx_core::domain::MediaFile,
    pub(super) opts: &'a TranscodeOptions,
    pub(super) plan: &'a ResolvedTranscode,
    pub(super) deadline: Instant,
    pub(super) yield_to_offline: bool,
    pub(super) cancelled: Option<&'a tokio_util::sync::CancellationToken>,
    pub(super) offline_package_id: Option<&'a str>,
    pub(super) offline_claim_generation: Option<i64>,
    pub(super) publication_fence: Option<PublicationFence>,
    pub(super) pretranscode_fence: Option<PretranscodeFence>,
    pub(super) expected_policy_generation: Option<String>,
    pub(super) expected_source_snapshot: Option<LocalSourceSnapshot>,
    pub(super) bound_source: Option<Arc<BoundPretranscodeSource>>,
}

/// Everything an earlier pass already encoded, in order.
///
/// Contiguity is what makes this a simple walk: a part that produced nothing is
/// deleted rather than left as a gap, so the first missing number is the end.
/// Reading the parts back off disk — rather than recording a resume point in
/// the database — keeps the bookmark and the bytes the same fact, so they
/// cannot disagree after a crash between writing one and the other.
pub(super) const MAX_RETAINED_PART_DIRECTORIES: usize = 10_000;
/// Worst-case resumable staging descendants: every generation segment, one
/// directory and playlist per retained part, the source identity, plus a
/// small fixed allowance for checkpoint/control files.
pub(crate) const MAX_PRETRANSCODE_CLEANUP_ENTRIES: usize =
    plurx_core::transcode::manifest::MAX_OBJECTS + (2 * MAX_RETAINED_PART_DIRECTORIES) + 100;

async fn discard_dependent_parts(
    temp: &plurx_core::fs_secure::SecureDirectory,
    first: usize,
    force: bool,
) -> Result<(), String> {
    let names = temp
        .child_names(MAX_RETAINED_PART_DIRECTORIES.saturating_add(4))
        .await
        .map_err(|error| format!("walking retained transcode parts: {error}"))?;
    let dependent = names
        .into_iter()
        .filter(|name| pretranscode_part_index(name).is_some_and(|index| index >= first))
        .collect::<Vec<_>>();
    if !force && dependent.is_empty() {
        return Ok(());
    }
    for name in [ASSEMBLED_TEMP_DIR, ASSEMBLED_DIR] {
        remove_staged_child(temp, name).await?;
    }
    for name in dependent {
        remove_staged_child(temp, &name).await?;
    }
    Ok(())
}

pub(super) async fn remove_staged_child(
    temp: &plurx_core::fs_secure::SecureDirectory,
    name: &str,
) -> Result<(), String> {
    match temp
        .remove_child_tree(
            name,
            plurx_core::transcode::manifest::MAX_OBJECTS.saturating_add(100),
            2,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("removing retained directory: {error}")),
    }
}

pub(crate) async fn quarantine_remove_cache_tree(
    path: &std::path::Path,
    max_depth: usize,
) -> Result<(), String> {
    let parent = path.parent().ok_or("cache tree has no parent")?;
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or("cache tree has no safe name")?;
    let identity = match plurx_core::fs_secure::directory_identity_nofollow(path).await {
        Ok(identity) => identity,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("identifying cache tree: {error}")),
    };
    let quarantine = format!(".delete-{name}-{}", uuid::Uuid::new_v4().simple());
    plurx_core::fs_secure::rename_child(parent, name, &quarantine)
        .await
        .map_err(|error| format!("quarantining cache tree: {error}"))?;
    let quarantined = parent.join(&quarantine);
    let moved_identity = plurx_core::fs_secure::directory_identity_nofollow(&quarantined)
        .await
        .ok();
    if !moved_identity.is_some_and(|moved| moved.same_inode(identity)) {
        let _ = plurx_core::fs_secure::rename_child_noreplace(parent, &quarantine, name).await;
        return Err("cache tree changed while it was quarantined".to_owned());
    }
    match plurx_core::fs_secure::remove_bounded_directory_tree_child(
        parent,
        &quarantine,
        MAX_PRETRANSCODE_CLEANUP_ENTRIES,
        max_depth,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = plurx_core::fs_secure::rename_child_noreplace(parent, &quarantine, name).await;
            Err(format!("removing quarantined cache tree: {error}"))
        }
    }
}

/// Everything an earlier pass left behind for this generation, with what it
/// observed while leaving it.
pub(super) struct ResumedParts {
    pub(super) parts: Vec<crate::produce::Part>,
    /// One receipt per element of `parts`, in the same order. A part with no
    /// readable record of its own is `unobserved`, so a resumed generation is
    /// certified only from records that still bind to the bytes on disk.
    pub(super) receipts: Vec<crate::decoder_health::ProducerHealthReceipt>,
}

pub(super) async fn resume_parts(
    temp: &plurx_core::fs_secure::SecureDirectory,
    plan_digest: &str,
) -> Result<ResumedParts, String> {
    let mut parts = Vec::new();
    let mut receipts: Vec<crate::decoder_health::ProducerHealthReceipt> = Vec::new();
    let mut playlist_bytes = 0_u64;
    let mut segments = 0_usize;
    let mut duration_ms = 0_i64;
    loop {
        if parts.len() >= MAX_RETAINED_PART_DIRECTORIES {
            return Err("retained transcode exceeds its part bound".to_owned());
        }
        let name = crate::produce::part_dir(parts.len());
        let Ok(dir) = temp.open_child_directory(&name).await else {
            discard_dependent_parts(temp, parts.len(), false).await?;
            return Ok(ResumedParts { parts, receipts });
        };
        let remaining_playlist_bytes = MAX_RETAINED_PLAYLIST_BYTES.saturating_sub(playlist_bytes);
        let Some(validated) = read_validated_part(&dir, remaining_playlist_bytes).await else {
            // A directory with no listed segments contributes nothing and
            // would shift every later part's numbering if it were counted.
            discard_dependent_parts(temp, parts.len(), true).await?;
            return Ok(ResumedParts { parts, receipts });
        };
        let ValidatedPart {
            part,
            playlist_bytes: part_playlist_bytes,
            shape,
        } = validated;
        playlist_bytes = playlist_bytes.saturating_add(part_playlist_bytes);
        segments = segments.saturating_add(part.segments.len());
        let Some(total_duration) = duration_ms.checked_add(part.duration_ms()) else {
            discard_dependent_parts(temp, parts.len(), true).await?;
            return Ok(ResumedParts { parts, receipts });
        };
        duration_ms = total_duration;
        if segments >= plurx_core::transcode::manifest::MAX_OBJECTS
            || duration_ms > MAX_RETAINED_TOTAL_DURATION_MS
        {
            discard_dependent_parts(temp, parts.len(), true).await?;
            return Ok(ResumedParts { parts, receipts });
        }
        // Read after the part validates, and against the shape validation just
        // measured: a record is only evidence about the bytes that are
        // actually there.
        receipts.push(
            resumed_part_health(&dir, plan_digest, &shape)
                .await
                .unwrap_or_else(|| {
                    crate::decoder_health::ProducerHealthReceipt::unobserved(
                        plan_digest.to_owned(),
                        // The disposition of the read. The receipt is
                        // unqualified regardless of it.
                        crate::decoder_health::ExitDisposition::CleanEnd,
                    )
                }),
        );
        parts.push(part);
    }
}

/// Read what one part actually produced, from the playlist ffmpeg wrote.
///
/// The playlist rather than a directory listing, because a directory contains
/// the segment that was being written when the process was killed and the
/// playlist does not — an unlisted `.ts` file is a truncated one, and treating
/// it as content puts a corrupt two seconds into the middle of a film.
pub(super) async fn read_part(part_dir: &plurx_core::fs_secure::SecureDirectory) -> ValidatedPart {
    read_validated_part(part_dir, MAX_PRETRANSCODE_PART_PLAYLIST_BYTES)
        .await
        .unwrap_or_else(|| ValidatedPart {
            part: crate::produce::Part {
                segments: Vec::new(),
                durations_ms: Vec::new(),
            },
            playlist_bytes: 0,
            shape: Vec::new(),
        })
}

/// The name of the record a part carries about its own health.
///
/// Dotted so it cannot collide with a segment name, and never placed into the
/// assembled generation: this is staging-local evidence about how the part was
/// made, not one of the objects the manifest inventories.
pub(crate) const PART_HEALTH_FILE: &str = ".part-health.json";
/// The staging-root ledger of observed attempts that left no part behind.
///
/// A part record can only describe a part that exists, and the attempt this
/// effort is named after leaves none — FFmpeg drops every frame, exits zero,
/// writes no segment, and the retry reuses the same directory. This is where
/// that receipt is kept so it survives a preemption.
pub(crate) const GENERATION_HEALTH_FILE: &str = ".generation-health.json";
/// A record holding one receipt with two short digests and a bounded contract
/// id. Generous by a wide margin, and bounded so a corrupt staging directory
/// cannot turn a resume into an unbounded read.
pub(super) const MAX_PART_HEALTH_BYTES: u64 = 4 * 1024;

/// One validated part, with what is needed to tie a receipt to it.
pub(super) struct ValidatedPart {
    pub(super) part: crate::produce::Part,
    playlist_bytes: u64,
    /// `(name, bytes, duration_ms)` for every listed segment, in playlist
    /// order — the shape a retained receipt is bound to.
    pub(super) shape: Vec<(String, u64, i64)>,
}

pub(super) async fn read_validated_part(
    part_dir: &plurx_core::fs_secure::SecureDirectory,
    remaining_playlist_bytes: u64,
) -> Option<ValidatedPart> {
    let bytes = part_dir
        .read_bounded_child(
            "index.m3u8",
            MAX_PRETRANSCODE_PART_PLAYLIST_BYTES.min(remaining_playlist_bytes),
        )
        .await
        .ok()?;
    let encoded_len = bytes.len() as u64;
    let Ok(text) = String::from_utf8(bytes) else {
        return None;
    };
    let part = crate::produce::Part::from_retained_playlist(&text)?;
    if part.is_empty()
        || part.segments.len() != part.durations_ms.len()
        || part.segments.len() >= plurx_core::transcode::manifest::MAX_OBJECTS
    {
        return None;
    }
    let mut names = std::collections::HashSet::with_capacity(part.segments.len());
    let mut shape = Vec::with_capacity(part.segments.len());
    for (index, (name, duration_ms)) in part.segments.iter().zip(&part.durations_ms).enumerate() {
        if name != &format!("seg{index:05}.ts")
            || !names.insert(name.as_str())
            || *duration_ms <= 0
            || *duration_ms > MAX_RETAINED_SEGMENT_DURATION_MS
        {
            return None;
        }
        let metadata = part_dir.child_metadata(name).await.ok()?;
        if !metadata.is_file
            || metadata.identity.size == 0
            || metadata.identity.size > plurx_core::transcode::manifest::MAX_OBJECT_BYTES
        {
            return None;
        }
        shape.push((name.clone(), metadata.identity.size, *duration_ms));
    }
    Some(ValidatedPart {
        part,
        playlist_bytes: encoded_len,
        shape,
    })
}

/// Write what this attempt observed beside the part it produced.
///
/// Best effort by design. The bytes are already on disk and already listed in
/// a playlist; a staging directory that will not take a 4 KiB record is not a
/// reason to throw away an encoded part. The cost of failing is that a later
/// pass reads no record and calls the part unobserved, which is exactly what
/// it did before this record existed.
pub(super) async fn retain_part_health(
    part_dir: &plurx_core::fs_secure::SecureDirectory,
    shape: &[(String, u64, i64)],
    receipt: &crate::decoder_health::ProducerHealthReceipt,
) {
    let part_shape = plurx_core::transcode::health::part_shape_digest(&receipt.plan_digest, shape);
    let sealed =
        plurx_core::transcode::health::RetainedPartReceipt::seal(part_shape, receipt.clone())
            .and_then(|record| {
                serde_json::to_vec(&record)
                    .map_err(|error| format!("serializing a retained part receipt: {error}"))
            });
    let encoded = match sealed {
        Ok(encoded) if encoded.len() as u64 <= MAX_PART_HEALTH_BYTES => encoded,
        Ok(encoded) => {
            tracing::warn!(
                bytes = encoded.len(),
                "a part health record exceeded its bound and was not retained"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "a part health record could not be sealed");
            return;
        }
    };
    if let Err(error) = part_dir
        .atomic_write_child(PART_HEALTH_FILE, &encoded)
        .await
    {
        tracing::warn!(%error, "a part health record could not be written");
    }
}

/// Carry forward what earlier passes observed in attempts that produced
/// nothing.
///
/// `None` when the ledger is absent, which is the honest reading of "no pass
/// has claimed an unproductive attempt". A ledger that is *present* and will
/// not open is a different statement — something was recorded and cannot be
/// read — so that yields `unobserved`, which refuses reuse. The asymmetry with
/// a missing part record is deliberate: a part's bytes exist whether or not a
/// record describes them, and an absent ledger describes no bytes at all.
pub(super) async fn carried_generation_health(
    temp: &plurx_core::fs_secure::SecureDirectory,
    plan_digest: &str,
) -> Option<crate::decoder_health::ProducerHealthReceipt> {
    let bytes = temp
        .read_bounded_child(GENERATION_HEALTH_FILE, MAX_PART_HEALTH_BYTES)
        .await
        .ok()?;
    let unreadable = || {
        Some(crate::decoder_health::ProducerHealthReceipt::unobserved(
            plan_digest.to_owned(),
            crate::decoder_health::ExitDisposition::CleanEnd,
        ))
    };
    let Ok(record) =
        serde_json::from_slice::<plurx_core::transcode::health::RetainedGenerationHealth>(&bytes)
    else {
        return unreadable();
    };
    match record.opened(plan_digest) {
        Some(receipt) => Some(receipt.clone()),
        None => unreadable(),
    }
}

/// Write the ledger back after joining this pass's own unproductive attempts.
///
/// Best effort, like the part records: the alternative to a ledger that will
/// not write is throwing away an encode. What it costs is that the next pass
/// reads no ledger, which is what happened before it existed.
pub(super) async fn retain_generation_health(
    temp: &plurx_core::fs_secure::SecureDirectory,
    receipt: &crate::decoder_health::ProducerHealthReceipt,
) {
    let sealed = plurx_core::transcode::health::RetainedGenerationHealth::seal(receipt.clone())
        .and_then(|record| {
            serde_json::to_vec(&record)
                .map_err(|error| format!("serializing a retained generation record: {error}"))
        });
    let encoded = match sealed {
        Ok(encoded) if encoded.len() as u64 <= MAX_PART_HEALTH_BYTES => encoded,
        Ok(encoded) => {
            tracing::warn!(
                bytes = encoded.len(),
                "a generation health ledger exceeded its bound and was not retained"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "a generation health ledger could not be sealed");
            return;
        }
    };
    if let Err(error) = temp
        .atomic_write_child(GENERATION_HEALTH_FILE, &encoded)
        .await
    {
        tracing::warn!(%error, "a generation health ledger could not be written");
    }
}

/// Read back what an earlier pass observed about this part.
///
/// `unobserved` for anything that is not a record binding a receipt to exactly
/// these bytes: no record, an unreadable one, a version this build does not
/// know, a torn one, or one sealed over a different shape. Every one of those
/// is the honest answer, and all of them refuse reuse.
/// The shape is digested under *this pass's* plan, not the one the record
/// names, so a record can only be opened by the plan it was written for. A
/// receipt from another plan is evidence about other bytes even when the
/// segment sizes happen to line up.
async fn resumed_part_health(
    part_dir: &plurx_core::fs_secure::SecureDirectory,
    plan_digest: &str,
    shape: &[(String, u64, i64)],
) -> Option<crate::decoder_health::ProducerHealthReceipt> {
    let bytes = part_dir
        .read_bounded_child(PART_HEALTH_FILE, MAX_PART_HEALTH_BYTES)
        .await
        .ok()?;
    let record: plurx_core::transcode::health::RetainedPartReceipt =
        serde_json::from_slice(&bytes).ok()?;
    let shape_digest = plurx_core::transcode::health::part_shape_digest(plan_digest, shape);
    record.opened(&shape_digest).cloned()
}

fn is_pretranscode_part_segment(name: &str) -> bool {
    let Some(digits) = name
        .strip_prefix("seg")
        .and_then(|rest| rest.strip_suffix(".ts"))
    else {
        return false;
    };
    digits.len() == 5 && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn pretranscode_part_index(name: &str) -> Option<usize> {
    let index = name.strip_prefix("part-")?;
    (index.len() >= 3 && index.len() <= 9 && index.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| index.parse().ok())
        .flatten()
}

fn is_pretranscode_part_path(path: &str) -> bool {
    use std::path::Component;

    let mut components = std::path::Path::new(path).components();
    let (Some(Component::Normal(part)), Some(Component::Normal(segment)), None) =
        (components.next(), components.next(), components.next())
    else {
        return false;
    };
    let Some(part) = part.to_str() else {
        return false;
    };
    pretranscode_part_index(part).is_some()
        && segment.to_str().is_some_and(is_pretranscode_part_segment)
}

pub(crate) const ASSEMBLED_DIR: &str = "assembled";
pub(crate) const ASSEMBLED_TEMP_DIR: &str = ".assembled.tmp";

/// Adopt an assembly an earlier pass already placed, if it is *this* set of
/// parts' assembly.
///
/// The tie is the playlist. `publish_from` writes exactly the bytes
/// `crate::produce::assemble` produces, so a byte-equal playlist means these
/// parts, in this order, with these durations — and the assembled segments are
/// hard links to their bytes. When it matches, the parts' own receipts describe
/// the assembled bytes and the caller's settled health is the generation's.
///
/// When it does not, the assembly was built from something else and this pass
/// can certify nothing about it, so the receipt is dropped. Getting this wrong
/// in the safe-looking direction is expensive: a long film is assembled and
/// then re-hashed for its manifest, and that hash can yield. Refusing to carry
/// the receipt across that yield would make the next pass adopt a receipt-less
/// assembly and, under the qualified identity, refuse the film permanently —
/// the very outcome the per-part records exist to prevent.
pub(super) async fn assembled_publication(
    directory: &plurx_core::fs_secure::SecureDirectory,
    parts: &[crate::produce::Part],
    health: Option<crate::decoder_health::ProducerHealthReceipt>,
) -> Option<Published> {
    let expected = crate::produce::assemble(parts);
    let bytes = directory
        .read_bounded_child(
            "index.m3u8",
            plurx_core::transcode::manifest::MAX_MANIFEST_BYTES,
        )
        .await
        .ok()?;
    let playlist = String::from_utf8(bytes).ok()?;
    let parsed = validated_vod_part(&playlist)?;
    let mut measured = playlist.len().min(i64::MAX as usize) as i64;
    for (index, name) in parsed.segments.iter().enumerate() {
        if name != &format!("seg{index:05}.ts") {
            return None;
        }
        let metadata = directory.child_metadata(name).await.ok()?;
        if !metadata.is_file
            || metadata.identity.size == 0
            || metadata.identity.size > plurx_core::transcode::manifest::MAX_OBJECT_BYTES
        {
            return None;
        }
        measured = measured.saturating_add(metadata.identity.size.min(i64::MAX as u64) as i64);
    }
    if playlist != expected.playlist {
        // Not this set of parts' assembly. The bytes may still be a perfectly
        // good generation, so it is still adopted — but nothing this pass
        // observed describes them.
        tracing::info!(
            "adopting an assembled generation that these parts did not produce; \
             it carries no producer health receipt"
        );
    }
    Some(Published {
        bytes: measured,
        duration_ms: parsed.duration_ms(),
        segments: parsed.segments.len(),
        parts: parts.len(),
        health: health.filter(|_| playlist == expected.playlist),
    })
}

/// Build one flat, atomic generation while retaining all resumable part bytes.
///
/// Hard links are metadata-only on the same cache filesystem. A crash at any
/// placement boundary leaves only `.assembled.tmp`, which the retry rebuilds;
/// the numbered parts remain the authoritative encode checkpoint until the
/// final generation is durably settled.
/// Release a claim this pass took and will not settle.
///
/// A queue-fenced production does not own an unfenced claim row, so it has
/// nothing to release; every other path does.
pub(super) async fn forget_unfenced_claim_with(
    store: &dyn Store,
    hash: &str,
    node_id: &str,
    publication_fence: Option<&PublicationFence>,
) {
    match publication_fence {
        Some(fence) => {
            // Best-effort cache repair: the invalid fenced entry is already
            // excluded and later reconciliation retries its removal.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetFencedUnfencedClaim,
                crate::store_result::Discard::BestEffort,
                PublicationStore::fenced(store, fence.clone())
                    .forget_cache_entry(hash, node_id, "local")
                    .await,
            );
        }
        None => {
            // Best-effort cache repair: the invalid entry is already excluded
            // from this lookup and later cache reconciliation retries removal.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetUnfencedCacheEntry,
                crate::store_result::Discard::BestEffort,
                store.forget_cache_entry(hash, node_id, "local").await,
            );
        }
    }
}

/// What prevents complete, unambiguous health-qualified coverage.
///
/// Ordered by what an operator should do about it, and stated rather than
/// implied: a control whose only feedback is "still off" tells the person who
/// turned it on nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualificationRefusal {
    /// Nobody asked. The ordinary state of every deployed node.
    NotRequested,
    /// This node never measured its own FFmpeg, so no contract can be matched
    /// to it and nothing it prints is evidence.
    BuildUnmeasured,
    /// The boot probe named no decoder implementation. A plan that names no
    /// decoder can never be matched to a contract, which is qualified against
    /// a named one.
    NoDecoderMeasured,
    /// The build is measured and decoders are named, but no retained
    /// diagnostic contract covers this binary under the qualified log flags.
    /// This is the fleet's ordinary state today and the one that takes real
    /// work to leave: it needs a capture from this build.
    NoContractCoversThisBuild,
    /// At least one selectable path is measured and covered, but at least one
    /// other selectable path is unmeasured or not uniquely covered. The
    /// enabled policy applies only to the covered paths; this warning prevents
    /// a partial rollout being mistaken for fleet-wide enforcement.
    IncompleteCoverage,
    /// More than one retained contract covers this build for the same codec,
    /// decoder and log mode. `contract_for` refuses ambiguity, so this reads
    /// as "no grammar" to everything downstream — but the fix is the opposite
    /// of the one for having none, and an operator told to capture a contract
    /// they already have twice would make it worse.
    AmbiguousContract,
    /// The request could not be read from the store at all, so this node does
    /// not know what was asked of it. It keeps the identity it has, which is
    /// the one every deployed node already has.
    SettingUnreadable,
}

impl QualificationRefusal {
    pub fn name(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::BuildUnmeasured => "build_unmeasured",
            Self::NoDecoderMeasured => "no_decoder_measured",
            Self::NoContractCoversThisBuild => "no_contract_covers_this_build",
            Self::IncompleteCoverage => "incomplete_coverage",
            Self::AmbiguousContract => "ambiguous_contract",
            Self::SettingUnreadable => "setting_unreadable",
        }
    }

    /// One sentence an operator can act on, for the settings surface.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::NotRequested => "Not requested on this node.",
            Self::BuildUnmeasured => {
                "This node has not measured its own FFmpeg build, so no diagnostic \
                 contract can be matched to it."
            }
            Self::NoDecoderMeasured => {
                "The startup probe named no decoder implementation, and a diagnostic \
                 contract is qualified against a named decoder."
            }
            Self::NoContractCoversThisBuild => {
                "No retained diagnostic contract covers this node's FFmpeg build under \
                 the qualified log flags. Capture one from this build before expecting \
                 verified artifacts."
            }
            Self::IncompleteCoverage => {
                "Only some selectable decode paths are measured and uniquely covered. The \
                 enabled policy applies only to qualified paths; complete the missing \
                 measurements or contracts before treating this node as fully covered."
            }
            Self::AmbiguousContract => {
                "More than one retained diagnostic contract covers this build for the \
                 same decoder. Remove the duplicate; capturing another makes it worse."
            }
            Self::SettingUnreadable => {
                "This node could not read the setting, so it kept the identity it had. \
                 It will read it again on its next start."
            }
        }
    }
}

/// What this node measured, which paths can honour the request, and its
/// operator-selected policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactQualificationReadiness {
    /// Whether an operator asked for the qualified identity on this node.
    pub requested: bool,
    /// The FFmpeg version this node measured for itself, if it measured one.
    pub measured_build: Option<String>,
    /// Every `(codec, backend, decoder implementation)` the boot probe measured.
    pub measured_decoders: Vec<(String, plurx_core::transcode::DecodeBackend, String)>,
    /// The subset of those a retained diagnostic contract covers on this
    /// build, under the qualified log flags.
    pub covered_decoders: Vec<(String, plurx_core::transcode::DecodeBackend, String)>,
    /// Conservative whole-node projection retained for existing API clients.
    /// This is qualified only when every selectable path is measured and
    /// uniquely covered;
    /// individual plans still apply `requested` to exact covered paths.
    pub effective: plurx_core::transcode::ArtifactQualification,
    /// Readiness advisory for incomplete or ambiguous coverage, if any.
    pub refusal: Option<QualificationRefusal>,
}

impl ArtifactQualificationReadiness {
    /// Whether at least one measured path could honour a request, whether or
    /// not one was made.
    ///
    /// Conjoined with the build rather than derived from coverage alone, so
    /// this cannot answer "yes" on a node that never identified the binary the
    /// coverage is about. Coverage is computed from the measured build, so the
    /// two agree today; stating it here keeps the invariant local instead of
    /// borrowing it from another module.
    pub fn eligible(&self) -> bool {
        self.measured_build.is_some() && !self.covered_decoders.is_empty()
    }

    /// What a node with no answer at all reports: the identity every deployed
    /// node has, and a stated reason rather than a silent default.
    pub(super) fn unreadable() -> Self {
        Self {
            requested: false,
            measured_build: None,
            measured_decoders: Vec::new(),
            covered_decoders: Vec::new(),
            effective: plurx_core::transcode::ArtifactQualification::Unqualified,
            refusal: Some(QualificationRefusal::SettingUnreadable),
        }
    }
}

/// The operator request and its advisory readiness report.
///
/// The request is never refused because a prerequisite is missing. Planning
/// applies it per decode path: a uniquely covered `(codec, backend, decoder)`
/// uses the qualified identity, while every other path keeps its existing
/// unqualified identity. That makes the prerequisites advisory without
/// rotating uncovered work into a namespace whose receipts it cannot produce.
///
/// A free function, and deliberately not a method: it takes only immutable
/// boot facts and the paths planning may select, so the whole rule can be
/// tested without a manager, a store, a cache or an FFmpeg.
pub fn artifact_qualification_readiness(
    requested: bool,
    policy: &crate::decoder_health::DiagnosticPolicy,
    measured: &plurx_core::transcode::decoder_inventory::MeasuredDecoders,
    selectable_paths: &[(String, plurx_core::transcode::DecodeBackend)],
) -> ArtifactQualificationReadiness {
    use plurx_core::transcode::ArtifactQualification;

    let measured_build = policy
        .measured_build()
        .map(|build| build.ffmpeg_version.clone());
    let mut measured_decoders = measured
        .measured_paths()
        .map(|(codec, backend, decoder)| (codec.to_owned(), backend, decoder.to_owned()))
        .collect::<Vec<_>>();
    measured_decoders.sort();
    // Covered means covered *as this node will run it*: the same codec, the
    // same decoder implementation the probe measured, and the qualified log
    // flags. A contract qualified under different flags describes a different
    // log and cannot certify this one.
    let covering = |codec: &str, backend: plurx_core::transcode::DecodeBackend, decoder: &str| {
        policy.covering_contracts(
            codec,
            decoder,
            backend.name(),
            crate::decoder_health::QUALIFIED_STDERR_MODE,
        )
    };
    let covered_decoders = measured_decoders
        .iter()
        .filter(|(codec, backend, decoder)| covering(codec, *backend, decoder) == 1)
        .cloned()
        .collect::<Vec<_>>();
    // Ambiguity is refused upstream by returning no contract at all, so
    // without this it would present as "capture one" to an operator who has
    // captured two.
    let ambiguous = measured_decoders
        .iter()
        .any(|(codec, backend, decoder)| covering(codec, *backend, decoder) > 1);
    // Successful measurements are not a completeness denominator. The probe
    // deliberately omits a codec whose decoder exists but whose probe encoder
    // does not, and planning can still select that codec. Legacy whole-node
    // fields may say qualified only when every path the manager can select is
    // both measured and uniquely covered.
    let all_selectable_paths_covered = !selectable_paths.is_empty()
        && selectable_paths.iter().all(|(codec, backend)| {
            covered_decoders
                .iter()
                .any(|(covered_codec, covered_backend, _)| {
                    covered_codec == codec && covered_backend == backend
                })
        });

    let refusal = if !requested {
        Some(QualificationRefusal::NotRequested)
    } else if measured_build.is_none() {
        Some(QualificationRefusal::BuildUnmeasured)
    } else if measured_decoders.is_empty() {
        Some(QualificationRefusal::NoDecoderMeasured)
    } else if ambiguous {
        Some(QualificationRefusal::AmbiguousContract)
    } else if covered_decoders.is_empty() {
        Some(QualificationRefusal::NoContractCoversThisBuild)
    } else if !all_selectable_paths_covered || covered_decoders.len() != measured_decoders.len() {
        Some(QualificationRefusal::IncompleteCoverage)
    } else {
        None
    };

    ArtifactQualificationReadiness {
        requested,
        measured_build,
        measured_decoders,
        covered_decoders,
        effective: if requested && refusal.is_none() {
            ArtifactQualification::HealthQualified
        } else {
            ArtifactQualification::Unqualified
        },
        refusal,
    }
}

/// The final directory's own name, which is what identifies one production.
///
/// `relative` is `<shard>/<identity>`, and the shard prefix is a directory
/// shared by many recipes, so only the last component identifies anything.
///
/// The shape is checked rather than asserted in prose. This value becomes a
/// manifest `generation_id`, and `publish_controlled_directory` rejects one
/// that is not `safe_generation_id`-shaped — a rejection its callers treat as
/// a retryable production error, so a name that can never be accepted would
/// retry the title on a backoff forever and report it as an encoder fault.
pub(super) fn identity_for(relative: &str) -> Result<&str, String> {
    let (shard, identity) = relative
        .rsplit_once('/')
        .ok_or_else(|| format!("cache publication {relative} has no shard prefix"))?;
    if shard.is_empty() || !plurx_core::transcode::manifest::is_safe_generation_id(identity) {
        return Err(format!(
            "cache publication {relative} has no safe generation identity"
        ));
    }
    Ok(identity)
}

/// Whether a generation may be kept, or reused, under this plan's identity.
///
/// One rule for both directions, and it reads the *manifest* rather than any
/// in-memory value, because the manifest is what a future request will have to
/// re-read. A receipt that exists only in this process cannot certify anything
/// tomorrow, so under the qualified identity a generation with no manifest is
/// refused exactly as one whose receipt refuses itself is.
///
/// The unqualified identity keeps its present behaviour exactly: it never
/// asked for a receipt and it still does not.
///
/// There is deliberately no third case for "old artifact, be lenient".
/// Relabelling an artifact that predates the contract as health-qualified is
/// what separate namespaces exist to make impossible, and a leniency here
/// would put it straight back.
pub(super) fn generation_permits_reuse(
    plan: &ResolvedTranscode,
    manifest: Option<&plurx_core::transcode::manifest::GenerationManifest>,
) -> bool {
    if !plan.enforces_receipt() {
        return true;
    }
    manifest.is_some_and(|manifest| {
        manifest
            .producer_health
            .as_ref()
            .is_some_and(plurx_core::transcode::health::ProducerHealthReceipt::permits_reuse)
    })
}

/// Everything this pass knows about the health of one generation.
///
/// Not "one receipt per part". Every producer attempt made toward the
/// generation is recorded, whether or not it left bytes behind, because the
/// case this effort exists for is an attempt that decodes nothing and exits
/// zero: FFmpeg drops every frame, writes no segment, and `read_part` returns
/// an empty part. Keying the record on "did it produce" would discard exactly
/// the receipt worth keeping, and the producer's progress observer carries no
/// control handle, so nothing else in the process would ever see it.
///
/// A part carried over from an earlier pass is recorded too, with whatever
/// that pass sealed beside it — or `unobserved` when no record still binds to
/// those bytes. Certifying a resumed film from the tail this pass happened to
/// encode is exactly the false certificate this effort exists to prevent;
/// refusing to read back a record that *is* there would make every film long
/// enough to need two passes permanently uncertifiable, which is not the
/// conservative answer, only the useless one.
pub(super) struct GenerationObservation {
    plan_digest: String,
    attempts: Vec<crate::decoder_health::ProducerHealthReceipt>,
    /// The join of this pass's attempts that produced nothing, carried
    /// separately because no part exists for them to be sealed beside.
    unproductive: Option<crate::decoder_health::ProducerHealthReceipt>,
}

impl GenerationObservation {
    /// Start from what an earlier pass left on disk: one receipt per resumed
    /// part, plus whatever the staging ledger carried about attempts that
    /// produced nothing.
    pub(super) fn inheriting(
        inherited: Vec<crate::decoder_health::ProducerHealthReceipt>,
        carried: Option<crate::decoder_health::ProducerHealthReceipt>,
        plan_digest: &str,
    ) -> Self {
        let mut attempts = inherited;
        attempts.extend(carried);
        Self {
            plan_digest: plan_digest.to_owned(),
            attempts,
            unproductive: None,
        }
    }

    /// Record one producer attempt. Bounded by `PRODUCER_MAX_PARTS`, which is
    /// what bounds the spawn loop this is called from.
    ///
    /// `produced` decides only whether the attempt's own bytes carry the
    /// receipt or the staging ledger does — never whether it is recorded.
    pub(super) fn record(
        &mut self,
        receipt: crate::decoder_health::ProducerHealthReceipt,
        produced: bool,
    ) {
        if !produced {
            self.unproductive = Some(match self.unproductive.take() {
                Some(carried) => crate::decoder_health::ProducerHealthReceipt::join(
                    &self.plan_digest,
                    &[carried, receipt.clone()],
                )
                .unwrap_or_else(|| receipt.clone()),
                None => receipt.clone(),
            });
        }
        self.attempts.push(receipt);
    }

    /// The join of every attempt this pass made that left no part behind, or
    /// `None` when every attempt produced. This is what the staging ledger
    /// has to carry so a preemption cannot forget it.
    pub(super) fn unproductive(&self) -> Option<&crate::decoder_health::ProducerHealthReceipt> {
        self.unproductive.as_ref()
    }

    /// The one receipt the generation may present, or `None` when this pass
    /// attempted nothing and inherited nothing.
    pub(super) fn settle(&self) -> Option<crate::decoder_health::ProducerHealthReceipt> {
        crate::decoder_health::ProducerHealthReceipt::join(&self.plan_digest, &self.attempts)
    }
}

pub(super) async fn publish_from(
    temp: &plurx_core::fs_secure::SecureDirectory,
    parts: &[crate::produce::Part],
    health: Option<crate::decoder_health::ProducerHealthReceipt>,
) -> Result<Option<Published>, String> {
    if let Ok(generation) = temp.open_child_directory(ASSEMBLED_DIR).await {
        if let Some(published) = assembled_publication(&generation, parts, health.clone()).await {
            // An assembly an earlier pass already placed. Whether this pass's
            // receipt describes it is `assembled_publication`'s question, and
            // it answers by comparing the on-disk playlist against the one
            // these parts assemble to — a byte-equal playlist means these
            // parts, in this order, and the assembled segments are hard links
            // to their bytes.
            return Ok(Some(published));
        }
    }
    remove_staged_child(temp, ASSEMBLED_TEMP_DIR).await?;
    remove_staged_child(temp, ASSEMBLED_DIR).await?;
    let staging = temp
        .create_child_directory(ASSEMBLED_TEMP_DIR)
        .await
        .map_err(|error| format!("creating assembled generation: {error}"))?;
    let assembled = crate::produce::assemble(parts);
    if assembled.placements.is_empty() {
        return Ok(None);
    }
    let mut bytes = 0i64;
    for p in &assembled.placements {
        if !is_pretranscode_part_path(&p.from) || !is_pretranscode_part_segment(&p.to) {
            return Err("assembled placement contains an unsafe segment path".to_owned());
        }
        let (part_name, segment_name) = p
            .from
            .split_once('/')
            .ok_or("assembled placement lacks a part boundary")?;
        let source = temp
            .open_child_directory(part_name)
            .await
            .map_err(|error| format!("opening {part_name}: {error}"))?;
        let placed = staging
            .place_regular_child_from(
                &source,
                segment_name,
                &p.to,
                plurx_core::transcode::manifest::MAX_OBJECT_BYTES,
            )
            .await
            .map_err(|error| format!("placing {}: {error}", p.to))?;
        bytes = bytes.saturating_add(placed.min(i64::MAX as u64) as i64);
    }
    staging
        .atomic_write_child("index.m3u8", assembled.playlist.as_bytes())
        .await
        .map_err(|e| format!("writing the playlist: {e}"))?;
    bytes += assembled.playlist.len() as i64;
    temp.rename_child(ASSEMBLED_TEMP_DIR, ASSEMBLED_DIR)
        .await
        .map_err(|error| format!("publishing assembled generation: {error}"))?;
    Ok(Some(Published {
        bytes,
        duration_ms: assembled.duration_ms,
        segments: assembled.placements.len(),
        parts: parts.len(),
        health,
    }))
}

/// Where this node keeps finished transcodes, and what identifies its output.
#[derive(Debug, Clone)]
pub(super) struct CacheConfig {
    /// Root the location rows are relative to. Deliberately *not* under the
    /// session scratch dir, which is wiped at every boot: a cache that empties
    /// on restart is a warm-up cost with none of the benefit.
    pub(super) dir: PathBuf,
    pub(super) ffmpeg_build: String,
    pub(super) node_id: String,
}

pub(super) async fn ensure_cache_directory(
    root: &std::path::Path,
    directory: &std::path::Path,
) -> Result<(), String> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| "cache directory escapes its configured root".to_owned())?;
    let root_metadata = tokio::fs::symlink_metadata(root)
        .await
        .map_err(|error| format!("inspecting cache root: {error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.file_type().is_dir() {
        return Err("cache root is not a regular directory".to_owned());
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("cache directory has an unsafe component".to_owned());
        };
        current.push(name);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => return Err(format!("{} is not a regular directory", current.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = current
                    .parent()
                    .ok_or_else(|| "cache directory has no parent".to_owned())?;
                let name = current
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .ok_or_else(|| "cache directory has no safe name".to_owned())?;
                plurx_core::fs_secure::create_directory_child(parent, name)
                    .await
                    .map_err(|error| format!("creating {}: {error}", current.display()))?;
            }
            Err(error) => return Err(format!("inspecting {}: {error}", current.display())),
        }
    }
    Ok(())
}

/// Free bytes the queue may safely promise on the cache filesystem. Keep a
/// fixed emergency margin for SQLite/Raft logs, manifests, and foreground
/// session scratch that can arrive immediately after the claim decision.
#[cfg(unix)]
pub(super) fn available_cache_scratch_bytes(path: &std::path::Path) -> Option<i64> {
    const EMERGENCY_MARGIN: u128 = 512 * 1024 * 1024;
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // Safety: `path` is NUL-terminated for the duration of the call and
    // `stats` is initialized by libc only when statvfs returns success.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // Safety: the successful call above initialized the complete structure.
    let stats = unsafe { stats.assume_init() };
    let fragment = if stats.f_frsize == 0 {
        stats.f_bsize
    } else {
        stats.f_frsize
    } as u128;
    let bytes = (stats.f_bavail as u128)
        .saturating_mul(fragment)
        .saturating_sub(EMERGENCY_MARGIN)
        .min(i64::MAX as u128);
    Some(bytes as i64)
}

#[cfg(windows)]
fn available_cache_scratch_bytes(path: &std::path::Path) -> Option<i64> {
    const EMERGENCY_MARGIN: u64 = 512 * 1024 * 1024;
    use std::os::windows::ffi::OsStrExt as _;

    let mut path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if path.is_empty() || path.contains(&0) {
        return None;
    }
    path.push(0);
    let mut available = 0u64;
    // SAFETY: the path is NUL-terminated and `available` is writable for the
    // duration of the read-only filesystem query.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &raw mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then(|| {
        available
            .saturating_sub(EMERGENCY_MARGIN)
            .min(i64::MAX as u64) as i64
    })
}
