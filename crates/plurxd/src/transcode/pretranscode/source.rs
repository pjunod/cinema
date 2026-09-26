use super::*;

/// How often a running producer checks whether a viewer wants its slot.
///
/// Short, because this interval *is* the latency a viewer pays to preempt it:
/// a quarter-second of polling plus a kill is well inside the five seconds a
/// live start is willing to queue, and the poll itself costs nothing.
pub(super) const PRODUCER_POLL: Duration = Duration::from_millis(250);

/// How long a producer waits before asking for a slot again after being
/// refused one. Longer than the poll: it has already been told a viewer is
/// there, and retrying eagerly would just spin.
const PRODUCER_RETRY: Duration = Duration::from_secs(5);

/// The producer timings that are data rather than constants, so a test can
/// state what it needs instead of racing the hardware.
///
/// Every field defaults to the production value above and nothing in the
/// daemon ever changes one — `TranscodeManager::new` takes the default and
/// there is no setter outside the retained live-HLS engine. They live here because the
/// resume path is only reachable by *interrupting an encoder that is still
/// running*, and whether that is possible at all depends on how fast the box
/// is: a 16-core desktop finished the whole 240-second fixture between two
/// sleeps that a 2-core CI runner needed five seconds for, so the test
/// asserting "this was preempted" was really asserting "this machine is
/// slow". Pacing the producer's input makes a part's wall-clock duration a
/// property of the *source and the rate* instead of the CPU, and shortening
/// the retry keeps the test from paying five seconds per preemption.
#[derive(Debug, Clone, Copy)]
pub(super) struct ProducerTuning {
    /// Pacing for a producer part's input. [`Pacing::unpaced`] in production —
    /// see the comment at the `hls_args` call in `produce_into` for why.
    pub(super) pacing: Pacing,
    /// Stand-in for [`PRODUCER_RETRY`].
    pub(super) retry: Duration,
}

impl Default for ProducerTuning {
    fn default() -> Self {
        ProducerTuning {
            pacing: Pacing::unpaced(),
            retry: PRODUCER_RETRY,
        }
    }
}

/// How many times one run will resume after being preempted before giving up
/// until the next producer pass.
///
/// A bound rather than a timeout because the failure it guards against is not
/// slowness but *thrash*: a busy evening where every part is killed within
/// seconds would otherwise spend the whole night starting encoders and
/// throwing them away. Progress is kept either way — the next pass resumes
/// from the same boundary.
pub(super) const PRODUCER_MAX_PARTS: usize = 64;

/// Why a producer part stopped.
#[derive(Debug)]
pub(super) enum PartEnd {
    /// ffmpeg reached the end of the file.
    Finished,
    /// A viewer wants the hardware.
    Preempted,
    /// This producer run is out of time.
    Deadline,
    Failed(String),
}

/// What a part's ending was, in the receipt's vocabulary.
///
/// §6.2 asks the receipt to distinguish "ran to the end of its input" from
/// "stopped because we asked it to". Preemption and the producer deadline are
/// both the second: a yielded part may still retain the segments it finished,
/// and calling that a failure would throw away work that is complete.
pub(super) fn part_exit_disposition(ended: &PartEnd) -> crate::decoder_health::ExitDisposition {
    match ended {
        PartEnd::Finished => crate::decoder_health::ExitDisposition::CleanEnd,
        PartEnd::Preempted | PartEnd::Deadline => {
            crate::decoder_health::ExitDisposition::IntentionalYield
        }
        PartEnd::Failed(_) => crate::decoder_health::ExitDisposition::FailedTermination,
    }
}

/// The owner-aware permits a foreground encoder keeps for its whole lifetime.
/// Constructed only after every background permit has been released.
pub(crate) struct LiveAdmission {
    pub(crate) encoder: Encoder,
    pub(super) hw_slot: Option<HwSlot>,
    pub(super) sw_permit: Option<crate::admission::SwPermit>,
}

impl LiveAdmission {
    pub(crate) fn software_threads(&self) -> Option<u32> {
        self.sw_permit
            .as_ref()
            .map(|permit| permit.threads() as u32)
    }
}

/// Which tracks a session carries. Part of its recipe, which is why it is a
/// named thing rather than two loose values.
#[derive(Debug, Clone, Default)]
pub(super) struct Tracks {
    pub(super) audio_index: Option<i64>,
    pub(super) subtitle_burn: Option<plurx_core::transcode::SubtitleBurn>,
}

/// A published cache entry, as measured on disk.
#[derive(Debug, Clone)]
pub(super) struct Published {
    pub(super) bytes: i64,
    pub(super) duration_ms: i64,
    pub(super) segments: usize,
    /// How many separate encoder runs it took — one, plus one per preemption.
    ///
    /// Worth reporting rather than inferring: it is the only number that says
    /// how contended the box was while this was made, and it is the thing a
    /// test of the resume path has to assert on, or that test passes on a
    /// fixture small enough to finish before it is ever interrupted.
    pub(super) parts: usize,
    /// What observation concluded about every part these bytes came from, or
    /// `None` when this pass did not produce them and so cannot say.
    pub(super) health: Option<crate::decoder_health::ProducerHealthReceipt>,
}

/// What one `produce` call achieved.
#[derive(Debug, Clone)]
pub struct Produced {
    pub recipe: String,
    pub bytes: i64,
    pub duration_ms: i64,
    pub segments: usize,
    pub parts: usize,
}

/// One coherent speculative-policy read. Candidate geometry, dedupe identity,
/// worker track selection and encoder choice all derive from this same value;
/// none of them rereads one setting independently.
#[derive(Debug, Clone)]
pub struct PretranscodePolicySnapshot {
    pub generation: String,
    pub(super) requested_encoder: String,
    pub(super) rate_control: RateControlSnapshot,
    pub(super) prefs: plurx_core::tracks::LangPrefs,
}

impl PretranscodePolicySnapshot {
    pub fn target_height(&self, file: &plurx_core::domain::MediaFile) -> i64 {
        TranscodeManager::pretranscode_target_height_for(file, &self.requested_encoder)
    }

    pub fn acceptable_encoder_families(&self) -> Vec<String> {
        match self.requested_encoder.trim().to_ascii_lowercase().as_str() {
            family @ ("software" | "nvenc" | "qsv" | "vaapi" | "videotoolbox") => {
                vec![family.to_owned()]
            }
            _ => ["software", "nvenc", "qsv", "vaapi", "videotoolbox"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PretranscodeProduceOutcome {
    Ready(Produced),
    Yielded,
    StoreUnavailable,
    PolicyChanged,
    SourceChanged,
    /// Produced and served, refused durable retention by its health receipt.
    /// Terminal: the same plan on the same source reaches the same decoder.
    HealthRefused,
}

/// A validated, zero-origin portable package request. Native subtitles are a
/// presentation rendition and deliberately do not alter the video recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineSpec {
    pub target_height: i64,
    pub audio_index: Option<i64>,
    pub subtitle: OfflineSubtitle,
    /// Immutable package identity captured when the request was accepted.
    pub effective_rate_control: EffectiveRateControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineSubtitle {
    None,
    Native(i64),
    Burn(i64),
}

/// Production outcomes that a durable queue can handle without inventing an
/// encoder failure for ordinary yielding or coalescing.
#[derive(Debug, Clone)]
pub enum OfflineProduceOutcome {
    Ready(Produced),
    Cached(Produced),
    Yielded,
    ClaimedElsewhere,
    StoreUnavailable,
    PolicyChanged,
    SourceChanged,
    /// The generation was produced and served, and refused durable retention
    /// because its producer health receipt does not permit reuse.
    ///
    /// A terminal answer, not a yield. The same plan on the same source will
    /// reach the same decoder and produce the same refused receipt, so a caller
    /// that retried this would re-encode the title forever.
    HealthRefused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct LocalSourceSnapshot {
    bytes: u64,
    pub(super) modified_secs: i64,
    modified_nanos: i64,
    changed_secs: i64,
    changed_nanos: i64,
    device: u64,
    inode: u64,
}

impl LocalSourceSnapshot {
    #[cfg(unix)]
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            bytes: metadata.len(),
            modified_secs: metadata.mtime(),
            modified_nanos: metadata.mtime_nsec(),
            changed_secs: metadata.ctime(),
            changed_nanos: metadata.ctime_nsec(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    #[cfg(windows)]
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt as _;

        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        Self {
            bytes: metadata.file_size(),
            modified_secs: modified.as_secs().min(i64::MAX as u64) as i64,
            modified_nanos: i64::from(modified.subsec_nanos()),
            changed_secs: modified.as_secs().min(i64::MAX as u64) as i64,
            changed_nanos: i64::from(modified.subsec_nanos()),
            device: 0,
            inode: metadata.creation_time() ^ metadata.last_write_time().rotate_left(17),
        }
    }

    #[cfg(unix)]
    fn from_file(file: &std::fs::File) -> std::io::Result<Self> {
        file.metadata()
            .map(|metadata| Self::from_metadata(&metadata))
    }

    #[cfg(windows)]
    fn from_file(file: &std::fs::File) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        let identity = plurx_core::fs_secure::std_file_identity(file)?;
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?;
        Ok(Self {
            bytes: identity.size,
            modified_secs: modified.as_secs().min(i64::MAX as u64) as i64,
            modified_nanos: i64::from(modified.subsec_nanos()),
            changed_secs: identity.changed_seconds,
            changed_nanos: identity.changed_nanoseconds,
            device: identity.device,
            inode: identity.inode ^ identity.inode_high.rotate_left(1),
        })
    }
}

const PRETRANSCODE_STAGING_IDENTITY: &str = ".pretranscode-source.json";
const MAX_PRETRANSCODE_STAGING_IDENTITY_BYTES: u64 = 4 * 1024;
pub(super) const MAX_PRETRANSCODE_PART_PLAYLIST_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_RETAINED_PLAYLIST_BYTES: u64 =
    plurx_core::transcode::manifest::MAX_MANIFEST_BYTES;
pub(super) const MAX_RETAINED_SEGMENT_DURATION_MS: i64 = 120_000;
const MAX_RETAINED_TITLE_DURATION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

pub(crate) fn validated_vod_part(text: &str) -> Option<crate::produce::Part> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next()? != "#EXTM3U" {
        return None;
    }
    let remaining = lines.collect::<Vec<_>>();
    if remaining.last().copied() != Some("#EXT-X-ENDLIST")
        || remaining[..remaining.len().saturating_sub(1)].contains(&"#EXT-X-ENDLIST")
    {
        return None;
    }
    let mut segments = Vec::new();
    let mut durations_ms = Vec::new();
    let mut pending_duration = None;
    let mut header_tags = std::collections::BTreeSet::new();
    let mut target_duration = None;
    let mut started_segments = false;
    for line in &remaining[..remaining.len().saturating_sub(1)] {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            if pending_duration.is_some() {
                return None;
            }
            started_segments = true;
            pending_duration = Some(
                rest.split(',')
                    .next()?
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|duration| duration.is_finite() && *duration >= 0.0)
                    .map(|seconds| (seconds * 1000.0).round() as i64)?,
            );
        } else if line.starts_with('#') {
            // Legacy adoption authenticates and later serves this exact
            // playlist. Only accept the URI-free header tags emitted by our
            // VOD assembler; KEY, MAP, BYTERANGE and unknown extensions can
            // otherwise smuggle references or byte interpretation outside
            // the authenticated object inventory.
            if pending_duration.is_some() || started_segments {
                return None;
            }
            let tag = if *line == "#EXT-X-VERSION:3" {
                "version"
            } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
                let seconds = value.parse::<i64>().ok().filter(|value| {
                    *value > 0 && *value <= MAX_RETAINED_SEGMENT_DURATION_MS / 1_000
                })?;
                target_duration = Some(seconds);
                "target_duration"
            } else if *line == "#EXT-X-MEDIA-SEQUENCE:0" {
                "media_sequence"
            } else if *line == "#EXT-X-PLAYLIST-TYPE:VOD" {
                "playlist_type"
            } else if *line == "#EXT-X-INDEPENDENT-SEGMENTS" {
                "independent_segments"
            } else {
                return None;
            };
            if !header_tags.insert(tag) {
                return None;
            }
        } else {
            durations_ms.push(pending_duration.take()?);
            segments.push((*line).to_owned());
        }
    }
    if pending_duration.is_some() {
        return None;
    }
    let part = crate::produce::Part {
        segments,
        durations_ms,
    };
    if part.is_empty()
        || part.segments.len() != part.durations_ms.len()
        || part.segments.len() >= plurx_core::transcode::manifest::MAX_OBJECTS
    {
        return None;
    }
    let mut total_ms = 0_i64;
    for (index, (name, duration_ms)) in part.segments.iter().zip(&part.durations_ms).enumerate() {
        if name != &format!("seg{index:05}.ts")
            || *duration_ms <= 0
            || *duration_ms > MAX_RETAINED_SEGMENT_DURATION_MS
        {
            return None;
        }
        total_ms = total_ms.checked_add(*duration_ms)?;
        if total_ms > MAX_RETAINED_TITLE_DURATION_MS {
            return None;
        }
    }
    if target_duration.is_some_and(|target| {
        let longest = part.durations_ms.iter().copied().max().unwrap_or_default();
        target < (longest + 999) / 1_000
    }) {
        return None;
    }
    Some(part)
}
pub(super) const MAX_RETAINED_TOTAL_DURATION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct PretranscodeStagingIdentity {
    pub(super) job_id: String,
    pub(super) file_id: i64,
    pub(super) source_size: i64,
    pub(super) source_mtime: i64,
    pub(super) policy_generation: String,
    pub(super) recipe_hash: String,
    pub(super) source: LocalSourceSnapshot,
}

pub(super) async fn read_pretranscode_staging_identity(
    temp: &plurx_core::fs_secure::SecureDirectory,
) -> Option<PretranscodeStagingIdentity> {
    let bytes = temp
        .read_bounded_child(
            PRETRANSCODE_STAGING_IDENTITY,
            MAX_PRETRANSCODE_STAGING_IDENTITY_BYTES,
        )
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) async fn write_pretranscode_staging_identity(
    temp: &plurx_core::fs_secure::SecureDirectory,
    identity: &PretranscodeStagingIdentity,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(identity)
        .map_err(|error| format!("serializing staging source identity: {error}"))?;
    if bytes.len() as u64 > MAX_PRETRANSCODE_STAGING_IDENTITY_BYTES {
        return Err("staging source identity exceeds its bounded format".to_owned());
    }
    temp.atomic_write_child(PRETRANSCODE_STAGING_IDENTITY, &bytes)
        .await
        .map_err(|error| format!("publishing staging source identity: {error}"))
}

pub(super) async fn bind_pretranscode_staging(
    parent: &plurx_core::fs_secure::SecureDirectory,
    name: &str,
    temp: plurx_core::fs_secure::SecureDirectory,
    job: &PretranscodeJob,
    recipe_hash: &str,
    source: LocalSourceSnapshot,
) -> Result<plurx_core::fs_secure::SecureDirectory, String> {
    let expected = PretranscodeStagingIdentity {
        job_id: job.id.clone(),
        file_id: job.file_id,
        source_size: job.source_size,
        source_mtime: job.source_mtime,
        policy_generation: job.policy_generation.clone(),
        recipe_hash: recipe_hash.to_owned(),
        source,
    };
    if read_pretranscode_staging_identity(&temp).await.as_ref() == Some(&expected) {
        return Ok(temp);
    }

    // Missing, corrupt, or mismatched identity makes every retained segment
    // untrusted. Rebuild only this job-scoped staging root, then atomically
    // bind the empty replacement before ffmpeg can create its first part.
    let expected_identity = temp
        .identity()
        .await
        .map_err(|error| format!("identifying unbound staging directory: {error}"))?;
    let quarantine = format!(".stale-{name}-{}", uuid::Uuid::new_v4().simple());
    parent
        .rename_child(name, &quarantine)
        .await
        .map_err(|error| format!("quarantining unbound staging directory: {error}"))?;
    let quarantine_identity = match parent.open_child_directory(&quarantine).await {
        Ok(directory) => directory.identity().await.ok(),
        Err(_) => None,
    };
    if !quarantine_identity.is_some_and(|identity| identity.same_inode(expected_identity)) {
        let _ = parent.rename_child_noreplace(&quarantine, name).await;
        return Err("staging directory changed while it was quarantined".to_owned());
    }
    let replacement = parent
        .create_child_directory(name)
        .await
        .map_err(|error| format!("recreating unbound staging directory: {error}"))?;
    write_pretranscode_staging_identity(&replacement, &expected).await?;
    Ok(replacement)
}

#[derive(Clone)]
pub struct BoundPretranscodeSource {
    pub(super) snapshot: LocalSourceSnapshot,
    path: std::path::PathBuf,
    pub(super) handle: Arc<std::fs::File>,
    /// FFprobe and FFmpeg inherit duplicates of the same open-file
    /// description. One owned permit spans each child lifetime so their seeks
    /// cannot race, including when a waiting task is cancelled.
    pub(super) offset_gate: Arc<tokio::sync::Semaphore>,
}

pub async fn pretranscode_source_snapshot(
    file: &plurx_core::domain::MediaFile,
    trusted_roots: &[std::path::PathBuf],
) -> Option<BoundPretranscodeSource> {
    #[cfg(any(unix, windows))]
    {
        // Resolve only the configured library root. Components beneath it are
        // untrusted media-library contents and remain subject to O_NOFOLLOW;
        // canonicalizing the complete file would turn a swapped file symlink
        // into authority to read outside the library.
        let mut matching_roots = trusted_roots
            .iter()
            .filter_map(|root| {
                file.path
                    .strip_prefix(root)
                    .ok()
                    .map(|relative| (root, relative))
            })
            .collect::<Vec<_>>();
        // Overlapping configured roots are legitimate (for example a broad
        // `/media` root and a more specific relocation mounted at
        // `/media/nas`). The narrowest authority must win; otherwise the
        // broad root sees the relocation itself as an untrusted nested
        // symlink and rejects a path whose selected root was explicitly
        // configured.
        matching_roots.sort_by_key(|(root, _)| std::cmp::Reverse(root.components().count()));
        let mut path = None;
        for (root, relative) in matching_roots {
            if relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                continue;
            }
            let Ok(canonical_root) = tokio::fs::canonicalize(root).await else {
                continue;
            };
            path = Some(canonical_root.join(relative));
            break;
        }
        let path = path?;
        let open_path = path.clone();
        let handle = tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&open_path)
        })
        .await
        .ok()?
        .ok()?;
        let metadata = handle.metadata().ok()?;
        let snapshot = LocalSourceSnapshot::from_file(&handle).ok()?;
        (metadata.is_file()
            && snapshot.bytes == file.size.max(0) as u64
            && snapshot.modified_secs == file.mtime)
            .then_some(BoundPretranscodeSource {
                snapshot,
                path,
                handle: Arc::new(handle),
                offset_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            })
    }
}

#[cfg(windows)]
pub(super) async fn bind_windows_session_source(
    file: &mut plurx_core::domain::MediaFile,
) -> Result<std::fs::File, String> {
    let path = file.path.clone();
    let handle = tokio::task::spawn_blocking(move || {
        plurx_core::fs_secure::open_read_nofollow_blocking(&path)
    })
    .await
    .map_err(|error| format!("joining Windows source open: {error}"))?
    .map_err(|error| format!("opening Windows source without reparse points: {error}"))?;
    let snapshot = LocalSourceSnapshot::from_file(&handle)
        .map_err(|error| format!("reading held Windows source identity: {error}"))?;
    if snapshot.bytes != file.size.max(0) as u64 || snapshot.modified_secs != file.mtime {
        return Err("source no longer matches the scanner's size/mtime identity".to_owned());
    }
    file.path = plurx_core::fs_secure::std_file_path(&handle)
        .map_err(|error| format!("resolving held Windows source path: {error}"))?;
    Ok(handle)
}

pub(super) async fn bound_source_snapshot(
    source: Option<&BoundPretranscodeSource>,
) -> Option<LocalSourceSnapshot> {
    let source = source?;
    let handle = Arc::clone(&source.handle);
    let path = source.path.clone();
    let expected = source.snapshot;
    tokio::task::spawn_blocking(move || {
        let handle_snapshot = handle
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .and_then(|_| LocalSourceSnapshot::from_file(&handle).ok())?;
        if handle_snapshot != expected {
            return None;
        }
        let current = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
        let current_snapshot = current
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .and_then(|_| LocalSourceSnapshot::from_file(&current).ok())?;
        (current_snapshot == expected).then_some(current_snapshot)
    })
    .await
    .ok()
    .flatten()
}

/// Renewal-safe view of a distributed queue claim.
///
/// Heartbeat replacement takes the write lock; publication and settlement
/// retain a read lock through the backend CAS. A completion can therefore use
/// neither the predecessor expiry nor a token invalidated between checking and
/// writing.
#[derive(Clone)]
pub struct PretranscodeFence {
    state: Arc<RwLock<Option<PretranscodeJob>>>,
    revoked: Arc<AtomicBool>,
}

type PretranscodeSettlementFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<bool, plurx_core::error::StoreError>> + Send + 'a>,
>;

impl PretranscodeFence {
    pub fn new(job: PretranscodeJob) -> Self {
        Self {
            state: Arc::new(RwLock::new(Some(job))),
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn snapshot(&self) -> Option<PretranscodeJob> {
        self.state.read().await.clone()
    }

    pub async fn renew(
        &self,
        store: &dyn Store,
        now_unix_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, plurx_core::error::StoreError> {
        let mut state = self.state.write().await;
        if self.revoked.load(Acquire) {
            return Ok(false);
        }
        let Some(current) = state.clone() else {
            return Ok(false);
        };
        let renewed = store
            .renew_pretranscode_job(&current, now_unix_ms, lease_expires_ms)
            .await;
        if self.revoked.load(Acquire) {
            return match renewed {
                Ok(Some(replacement)) => {
                    // Retain an acknowledged replacement only so retirement
                    // can return this now-unowned row to the queue.
                    *state = Some(replacement);
                    Ok(false)
                }
                Ok(None) => {
                    *state = None;
                    Ok(false)
                }
                Err(error) => {
                    *state = None;
                    Err(error)
                }
            };
        }
        match renewed {
            Ok(Some(replacement))
                if renewal_response_is_authoritative(&current, &replacement, unix_ms()) =>
            {
                *state = Some(replacement);
                Ok(true)
            }
            Ok(Some(replacement)) => {
                // The backend renewed before the predecessor deadline but
                // answered too late for continuous local authority. Keep the
                // exact acknowledged token for deterministic retirement while
                // synchronously blocking publication and settlement.
                *state = Some(replacement);
                self.revoke();
                Ok(false)
            }
            Ok(None) => {
                *state = None;
                Ok(false)
            }
            Err(error) => {
                // Renewal uncertainty is loss of publication authority, not
                // permission to keep the last token until its wall-clock TTL.
                *state = None;
                Err(error)
            }
        }
    }

    pub fn revoke(&self) {
        self.revoked.store(true, Release);
    }

    pub async fn invalidate(&self, expected: &PretranscodeJob) -> bool {
        let mut state = self.state.write().await;
        if state.as_ref() != Some(expected) {
            return false;
        }
        *state = None;
        true
    }

    async fn settle<'a, F>(&'a self, operation: F) -> Result<bool, plurx_core::error::StoreError>
    where
        F: FnOnce(PretranscodeJob, i64) -> PretranscodeSettlementFuture<'a>,
    {
        if self.revoked.load(Acquire) {
            return Ok(false);
        }
        let mut state = self.state.write().await;
        if self.revoked.load(Acquire) {
            return Ok(false);
        }
        let Some(job) = state.clone() else {
            return Ok(false);
        };
        let observed_at = unix_ms();
        // The backend owns its deadline. An equal outer timeout can drop a
        // still-committing Hiqlite request, while SQLite's blocking
        // transaction cannot be cancelled safely at all.
        let result = operation(job, observed_at).await;
        // Every settlement is terminal for this running token.
        *state = None;
        result
    }

    pub async fn retire(&self, store: &dyn Store) -> Result<(), plurx_core::error::StoreError> {
        self.revoke();
        let mut state = self.state.write().await;
        let Some(job) = state.clone() else {
            return Ok(());
        };
        let now_unix_ms = unix_ms();
        let result = store
            .yield_pretranscode_job(&job, now_unix_ms, now_unix_ms)
            .await;
        *state = None;
        result.map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn complete(
        &self,
        store: &dyn Store,
        recipe_hash: &str,
        relative_dir: &str,
        bytes: i64,
        expected_previous_bytes: Option<i64>,
        manifest_digest: &str,
        now_unix_ms: i64,
    ) -> Result<bool, plurx_core::error::StoreError> {
        let _ = now_unix_ms;
        self.settle(move |job, observed_at| {
            Box::pin(async move {
                store
                    .complete_pretranscode_job(
                        &job,
                        recipe_hash,
                        CACHE_RECIPE_VERSION,
                        relative_dir,
                        bytes,
                        expected_previous_bytes,
                        manifest_digest,
                        observed_at,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn yield_job(
        &self,
        store: &dyn Store,
        now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, plurx_core::error::StoreError> {
        self.settle(move |job, observed_at| {
            Box::pin(async move {
                store
                    .yield_pretranscode_job(&job, observed_at, not_before_ms.max(now_unix_ms))
                    .await
            })
        })
        .await
    }

    pub async fn fail_job(
        &self,
        store: &dyn Store,
        error_code: &str,
        now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, plurx_core::error::StoreError> {
        self.settle(move |job, observed_at| {
            Box::pin(async move {
                store
                    .fail_pretranscode_job(
                        &job,
                        error_code,
                        observed_at,
                        not_before_ms.max(now_unix_ms),
                    )
                    .await
            })
        })
        .await
    }

    pub async fn cancel_job(
        &self,
        store: &dyn Store,
        error_code: &str,
        now_unix_ms: i64,
    ) -> Result<bool, plurx_core::error::StoreError> {
        let _ = now_unix_ms;
        self.settle(move |job, observed_at| {
            Box::pin(async move {
                store
                    .cancel_pretranscode_job(&job, error_code, observed_at)
                    .await
            })
        })
        .await
    }
}

/// A backend may commit a renewal before the old deadline but deliver its
/// response after that deadline. Local publication authority is continuous
/// only when the response itself arrives while the predecessor is still live.
pub(super) fn renewal_response_is_authoritative(
    previous: &PretranscodeJob,
    replacement: &PretranscodeJob,
    response_now_ms: i64,
) -> bool {
    response_now_ms < previous.lease_expires_ms
        && replacement.lease_expires_ms > response_now_ms
        && replacement.id == previous.id
        && replacement.owner_node_id == previous.owner_node_id
        && replacement.fence == previous.fence
}
