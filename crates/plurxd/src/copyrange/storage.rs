//! Owner-private payloads and independent durable cumulative observations.
//! Completed bytes precede ONE atomic manifest containing refs, rows and
//! checkpoint. Inflight observers own the opened immutable manifest itself.
//! A range's owner is typed: shared Source playback or a local user's own.
use super::ObservedCopySegment;
use crate::copy_analysis::{CopyRangeCheckpoint, CopyRowInspection, LocalPlaybackOwner};
use plurx_core::{
    fmp4::{Init, PromotionInputs},
    fs_secure::SecureDirectory,
    hevc_configuration::RangeInspection,
    segplan::{
        ranges::{RangeBoundaries, SourceTimestamp},
        IndexRow,
    },
    sharing_source_sessions::SourceDispatchAssignment,
    store::analysis_coordination::AnalysisCoordinationSource,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};
use tokio_util::sync::CancellationToken;
const MAX_ROWS: usize = 250_000;
const MAX_MANIFEST: usize = 64 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredCopySegment {
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
    pub(crate) seconds: f64,
    pub(crate) first_row: usize,
    pub(crate) rows: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredInspection {
    dts: u64,
    duration: u64,
    packets: u64,
    slices: u64,
    configuration_sha256: String,
    hdr10_mastering: Option<String>,
    hdr10_light: Option<[u16; 2]>,
    hdr10_conflict: bool,
}
impl From<&CopyRowInspection> for StoredInspection {
    fn from(value: &CopyRowInspection) -> Self {
        Self {
            dts: value.dts,
            duration: value.duration,
            packets: value.packets,
            slices: value.slices,
            configuration_sha256: value.configuration_sha256.clone(),
            hdr10_mastering: value.hdr10_mastering.clone(),
            hdr10_light: value.hdr10_light,
            hdr10_conflict: value.hdr10_conflict,
        }
    }
}
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest<'a> {
    version: u32,
    catalog_namespace: &'a str,
    source: &'a AnalysisCoordinationSource,
    // A Source owner writes exactly these three and none of the four after
    // them, which is the manifest every earlier binary wrote, byte for byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_incarnation: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_request: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_owner_key: Option<&'a str>,
    // Additive. A manifest without `owner` is a Source owner's.
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_session: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_user_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_playback_id: Option<&'a str>,
    range_id: uuid::Uuid,
    range_epoch: u64,
    checkpoint_sequence: u64,
    init_contract_sha256: &'a str,
    canonical_init_sha256: &'a str,
    av_init_sha256: &'a str,
    source_origin_dts: i64,
    source_time_base_num: u32,
    source_time_base_den: u32,
    canonical_title_start: u64,
    timescale: u32,
    media_origin_ms: i64,
    target_duration: u32,
    begins_title: bool,
    eof: bool,
    first_frame_rendered: bool,
    rows: &'a [IndexRow],
    segments: &'a [StoredCopySegment],
    row_inspections: &'a [StoredInspection],
    inspection_packets: Option<u64>,
    inspection_slices: Option<u64>,
    inspection_configuration_sha256: Option<&'a str>,
    promotion: &'a PromotionInputs,
}
#[derive(Serialize)]
struct Current<'a> {
    version: u32,
    manifest: &'a str,
    sha256: &'a str,
}
struct OldManifest {
    name: String,
    file: Weak<std::fs::File>,
}

/// Who owns one range's files. The payload directory, the observation
/// directory and the manifest's binding all come from it. Neither owner can
/// be made from the other: a Source owner is its grant-scoped assignment and
/// holds no local authority, and a local owner is a local user's own named
/// playback in one VOD session ([`LocalPlaybackOwner`], which only a live
/// viewer makes) and holds no assignment, gate or grant.
pub(crate) enum CopyRangeOwner {
    Source(Box<SourceDispatchAssignment>),
    // Constructed by tests until a local VOD session attaches a range.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "no local VOD session creates a range yet")
    )]
    Local {
        /// The local VOD session the range is the rendition of.
        session: String,
        viewer: LocalPlaybackOwner,
    },
}
impl CopyRangeOwner {
    fn binding(&self) -> Result<OwnerBinding, String> {
        match self {
            Self::Source(assignment) => Ok(OwnerBinding::Source {
                incarnation: assignment.binding().incarnation_id().to_string(),
                request: assignment.binding().request_id().to_owned(),
                owner_key: assignment.binding().principal().owner_key(),
            }),
            Self::Local { session, viewer } => {
                // The session names a directory.
                let plain = !session.is_empty()
                    && session.len() <= 128
                    && session
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
                if !plain {
                    return Err("copy_range_binding_invalid".into());
                }
                let viewer = viewer.authority();
                Ok(OwnerBinding::Local {
                    session: session.clone(),
                    user_id: viewer.user_id(),
                    playback_id: viewer.playback_id().to_owned(),
                })
            }
        }
    }
}

/// What a range's directories and manifests record of its owner.
enum OwnerBinding {
    Source {
        incarnation: String,
        request: String,
        owner_key: String,
    },
    Local {
        session: String,
        user_id: i64,
        playback_id: String,
    },
}
impl OwnerBinding {
    /// The payload namespace under the work directory and the owner's own
    /// directory in it; the range's payload is `<namespace>/<owner>/<range>`.
    fn payload(&self) -> (&'static str, &str) {
        match self {
            Self::Source { incarnation, .. } => ("source-copy-ranges", incarnation),
            Self::Local { session, .. } => ("local-copy-ranges", session),
        }
    }
    /// The manifest directory beside `copy-ranges/observations`. A local
    /// range's manifests are kept apart from the Source ones, in a directory
    /// no binary that knows only Source manifests ever opens.
    fn observations(&self) -> &'static str {
        match self {
            Self::Source { .. } => "observations",
            Self::Local { .. } => "local-observations",
        }
    }
}
/// One range's physical writer epoch. Retirement and each final rename use
/// the same lock, so a cancelled publisher cannot win a post-retirement commit.
/// This is local producer lifetime state, not an Analysis claim or IO lease.
#[derive(Clone, Default)]
pub(crate) struct CopyRangePublisher(Arc<Mutex<bool>>);
impl CopyRangePublisher {
    pub(crate) fn retire(&self) {
        *self.0.lock().expect("copy range publisher") = true;
    }
    fn commit(
        &self,
        stop: &CancellationToken,
        rename: &mut dyn FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let retired = self
            .0
            .lock()
            .map_err(|_| std::io::Error::other("copy range publisher poisoned"))?;
        if *retired || stop.is_cancelled() {
            return Err(std::io::Error::other("copy range publisher retired"));
        }
        rename()
    }
}

pub(crate) struct CopyRangeFiles {
    pub(crate) payload: SecureDirectory,
    pub(crate) payload_path: PathBuf,
    payload_parent: SecureDirectory,
    payload_name: String,
    payload_pin: Option<crate::playback_cache::CachePin>,
    payload_retired: bool,
    publisher: CopyRangePublisher,
    observation: SecureDirectory,
    observation_path: PathBuf,
    /// No reader reloads a retired range's manifests; its in-memory
    /// observation already reached the Analysis inbox, so it is removed too.
    observation_parent: SecureDirectory,
    catalog_namespace: String,
    source: AnalysisCoordinationSource,
    owner: OwnerBinding,
    range_id: uuid::Uuid,
    range_epoch: u64,
    sequence: u64,
    canonical_title_start: u64,
    source_origin: SourceTimestamp,
    contract: String,
    promotion: PromotionInputs,
    canonical_init: Option<Arc<Init>>,
    av_init: Option<Arc<Init>>,
    canonical_init_sha256: String,
    av_init_sha256: String,
    origin: Option<i64>,
    rows: Vec<IndexRow>,
    segments: Vec<StoredCopySegment>,
    row_inspections: Vec<StoredInspection>,
    inspection: Option<RangeInspection>,
    eof: bool,
    rendered: bool,
    committed_segments: usize,
    committed_eof: bool,
    current: Option<Arc<CopyRangeCheckpoint>>,
    old: Vec<OldManifest>,
}
impl CopyRangeFiles {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create(
        payload_root: &Path,
        observation_root: &Path,
        catalog_namespace: String,
        source: AnalysisCoordinationSource,
        owner: CopyRangeOwner,
        range_id: uuid::Uuid,
        range_epoch: u64,
        source_origin: SourceTimestamp,
        canonical_title_start: u64,
        contract: String,
        promotion: PromotionInputs,
    ) -> Result<Self, String> {
        if range_epoch == 0
            || source_origin.time_base_num == 0
            || source_origin.time_base_den == 0
            || contract.len() != 64
        {
            return Err("copy_range_binding_invalid".into());
        }
        let owner = owner.binding()?;
        let (payload_namespace, payload_owner) = owner.payload();
        let work = SecureDirectory::open(payload_root)
            .await
            .map_err(|_| "copy_payload_root_unavailable".to_owned())?;
        let ranges = work
            .create_child_directory(payload_namespace)
            .await
            .map_err(|_| "copy_payload_namespace_unavailable".to_owned())?;
        let range = range_id.to_string();
        let payload_parent = ranges
            .create_child_directory(payload_owner)
            .await
            .map_err(|_| "copy_payload_owner_unavailable".to_owned())?;
        let payload = payload_parent
            .create_child_directory(&range)
            .await
            .map_err(|_| "copy_payload_range_unavailable".to_owned())?;
        // This configured path is derived by AppState from its durable data
        // root. Tests must provide the same explicit shape; no scratch fallback.
        if observation_root.file_name().and_then(|name| name.to_str()) != Some("observations")
            || observation_root
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                != Some("copy-ranges")
        {
            return Err("copy_observation_root_invalid".into());
        }
        let copy_ranges = observation_root
            .parent()
            .ok_or("copy_observation_root_invalid")?;
        let data = copy_ranges
            .parent()
            .ok_or("copy_observation_root_invalid")?;
        let data = SecureDirectory::open(data)
            .await
            .map_err(|_| "copy_observation_root_unavailable".to_owned())?;
        let metadata = data
            .create_child_directory("copy-ranges")
            .await
            .map_err(|_| "copy_observation_root_unavailable".to_owned())?
            .create_child_directory(owner.observations())
            .await
            .map_err(|_| "copy_observation_root_unavailable".to_owned())?;
        let observation = metadata
            .create_child_directory(&range)
            .await
            .map_err(|_| "copy_observation_range_unavailable".to_owned())?;
        let observation_path = copy_ranges.join(owner.observations()).join(&range);
        let payload_path = payload_root
            .join(payload_namespace)
            .join(payload_owner)
            .join(&range);
        let payload_pin = crate::playback_cache::pin(&payload_path)
            .map_err(|_| "copy_payload_pin_failed".to_owned())?;
        Ok(Self {
            payload_parent,
            payload_name: range,
            payload_pin,
            payload_retired: false,
            publisher: CopyRangePublisher::default(),
            payload,
            payload_path,
            observation,
            observation_path,
            observation_parent: metadata,
            catalog_namespace,
            source,
            owner,
            range_id,
            range_epoch,
            sequence: 0,
            canonical_title_start,
            source_origin,
            contract,
            promotion,
            canonical_init: None,
            av_init: None,
            canonical_init_sha256: String::new(),
            av_init_sha256: String::new(),
            origin: None,
            rows: Vec::new(),
            segments: Vec::new(),
            row_inspections: Vec::new(),
            inspection: None,
            eof: false,
            rendered: false,
            committed_segments: 0,
            committed_eof: false,
            current: None,
            old: Vec::new(),
        })
    }
    /// The actual actor calls this only after its exact physical generation
    /// and counted response bodies/jobs are joined. Metadata is independent
    /// durable state and survives this private payload cache retirement.
    pub(crate) async fn retire_payload(&mut self) -> Result<(), String> {
        if self.payload_retired {
            return Ok(());
        }
        self.publisher.retire();
        drop(self.payload_pin.take());
        let _eviction = crate::playback_cache::begin_eviction(&self.payload_path)
            .map_err(|_| "copy_payload_eviction_busy".to_owned())?;
        self.payload_parent
            .remove_child_tree(&self.payload_name, 120100, 2)
            .await
            .map_err(|_| "copy_payload_retirement_failed".to_owned())?;
        self.observation_parent
            .remove_child_tree(&self.payload_name, 120100, 2)
            .await
            .map_err(|_| "copy_observation_retirement_failed".to_owned())?;
        self.payload_retired = true;
        Ok(())
    }
    pub(crate) fn publisher(&self) -> CopyRangePublisher {
        self.publisher.clone()
    }
    pub(crate) fn canonical_init(&self) -> Option<Arc<Init>> {
        self.canonical_init.clone()
    }
    pub(crate) fn current(&self) -> Option<Arc<CopyRangeCheckpoint>> {
        self.current.clone()
    }
    pub(crate) fn segments(&self) -> &[StoredCopySegment] {
        &self.segments[..self.committed_segments]
    }
    pub(crate) fn media_origin_ms(&self) -> Option<i64> {
        self.origin
    }
    pub(crate) fn complete(&self) -> bool {
        self.committed_eof
    }
    pub(crate) fn set_origin(&mut self, origin: i64) -> Result<(), String> {
        if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&origin)
            || self.origin.is_some_and(|old| old != origin)
        {
            return Err("copy_init_contract_changed".into());
        }
        self.origin = Some(origin);
        Ok(())
    }
    pub(crate) async fn set_init(
        &mut self,
        canonical: Arc<Init>,
        av: Arc<Init>,
        retirement: &CancellationToken,
    ) -> Result<(), String> {
        if self.canonical_init.is_some() || retirement.is_cancelled() {
            return Err("copy_init_contract_changed".into());
        }
        self.canonical_init_sha256 = hex::encode(Sha256::digest(&canonical.bytes));
        self.av_init_sha256 = hex::encode(Sha256::digest(&av.bytes));
        self.write_payload("init.mp4", &av.bytes, retirement)
            .await?;
        self.observation
            .atomic_write_child_with_commit("canonical-init.mp4", &canonical.bytes, {
                let publisher = self.publisher.clone();
                let stop = retirement.clone();
                move |rename| publisher.commit(&stop, rename)
            })
            .await
            .map_err(|_| "copy_observation_init_write_failed".to_owned())?;
        self.canonical_init = Some(canonical);
        self.av_init = Some(av);
        Ok(())
    }
    async fn write_payload(
        &self,
        name: &str,
        bytes: &[u8],
        retirement: &CancellationToken,
    ) -> Result<(), String> {
        if retirement.is_cancelled() {
            return Err("copy_range_retired".into());
        }
        let reservation = crate::playback_cache::reserve(&self.payload_path, bytes.len() as u64)
            .await
            .map_err(|_| "copy_payload_capacity".to_owned())?;
        let retirement = retirement.clone();
        let publisher = self.publisher.clone();
        self.payload
            .atomic_write_child_with_commit(name, bytes, move |rename| {
                publisher.commit(&retirement, rename)
            })
            .await
            .map_err(|_| "copy_payload_write_failed".to_owned())?;
        if let Some(reservation) = reservation {
            reservation.commit(bytes.len() as u64);
        }
        Ok(())
    }
    /// A row is not durable or servable until its completed media reference
    /// and all observations share the following atomic immutable checkpoint.
    pub(crate) async fn commit_segment(
        &mut self,
        output: ObservedCopySegment,
        retirement: &CancellationToken,
    ) -> Result<Arc<CopyRangeCheckpoint>, String> {
        let segment = output.segment;
        if segment.output_index as usize != self.segments.len()
            || !segment.seconds.is_finite()
            || segment.seconds <= 0.0
            || segment.seconds > 16.0
            || !segment.presentation.within_seconds(16)
        {
            return Err("copy_target_exceeded".into());
        }
        if self.rows.len().saturating_add(segment.rows.len()) > MAX_ROWS {
            return Err("copy_range_row_limit".into());
        }
        let mut end = match self.rows.last() {
            Some(row) => Some(
                row.dts
                    .checked_add(row.duration)
                    .ok_or("copy_range_row_overflow")?,
            ),
            None => None,
        };
        for row in &segment.rows {
            if row.duration == 0 || end.is_some_and(|end| end != row.dts) {
                return Err("copy_range_row_disagreement".into());
            }
            end = Some(
                row.dts
                    .checked_add(row.duration)
                    .ok_or("copy_range_row_overflow")?,
            );
        }
        let name = fmp4_name(segment.output_index);
        let sha256 = hex::encode(Sha256::digest(&segment.media.bytes));
        self.write_payload(&name, &segment.media.bytes, retirement)
            .await?;
        let first_row = self.rows.len();
        let row_count = segment.rows.len();
        self.rows.extend(segment.rows);
        self.row_inspections
            .extend(output.row_inspections.iter().map(StoredInspection::from));
        self.segments.push(StoredCopySegment {
            index: segment.output_index,
            name,
            bytes: segment.media.bytes.len() as u64,
            sha256,
            seconds: segment.seconds,
            first_row,
            rows: row_count,
        });
        self.checkpoint(retirement).await
    }
    pub(crate) async fn mark_rendered(
        &mut self,
        retirement: &CancellationToken,
    ) -> Result<Option<Arc<CopyRangeCheckpoint>>, String> {
        if self
            .current
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.first_frame_rendered)
            || self.rows.is_empty()
        {
            return Ok(self.current());
        }
        self.rendered = true;
        self.checkpoint(retirement).await.map(Some)
    }
    /// Call only after successful child wait, all pipes drained, parser/kernel
    /// finish and completed tail commits. EOF is range-local, not whole-file.
    pub(crate) async fn finish(
        &mut self,
        inspection: Option<RangeInspection>,
        retirement: &CancellationToken,
    ) -> Result<Arc<CopyRangeCheckpoint>, String> {
        self.inspection = inspection;
        self.eof = true;
        self.checkpoint(retirement).await
    }
    pub(crate) fn playlist(&self) -> Vec<u8> {
        let mut playlist = plurx_core::fmp4::playlist_header(
            plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS,
        );
        for segment in self.segments() {
            playlist.push_str(&plurx_core::fmp4::playlist_entry(
                segment.seconds,
                &segment.name,
            ));
        }
        if self.committed_eof {
            playlist.push_str("#EXT-X-ENDLIST\n");
        }
        playlist.into_bytes()
    }
    async fn checkpoint(
        &mut self,
        retirement: &CancellationToken,
    ) -> Result<Arc<CopyRangeCheckpoint>, String> {
        if retirement.is_cancelled() {
            return Err("copy_range_retired".into());
        }
        let init = self.canonical_init.as_ref().ok_or("copy_init_missing")?;
        let timescale = init.video().ok_or("copy_video_missing")?.timescale;
        let origin = self.origin.ok_or("copy_format_start_unknown")?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("copy_range_epoch_overflow")?;
        let boundaries = RangeBoundaries {
            begins_title: self
                .rows
                .first()
                .is_some_and(|row| row.dts == self.canonical_title_start),
            eof: self.eof,
        };
        let bytes = self.manifest(timescale, origin, boundaries)?;
        if bytes.len() > MAX_MANIFEST {
            return Err("copy_manifest_limit".into());
        }
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let name = format!("checkpoint-{}-{}.json", self.range_epoch, self.sequence);
        let stop = retirement.clone();
        let publisher = self.publisher.clone();
        self.observation
            .atomic_write_child_with_commit(&name, &bytes, move |rename| {
                publisher.commit(&stop, rename)
            })
            .await
            .map_err(|_| "copy_manifest_write_failed".to_owned())?;
        let file = Arc::new(
            self.observation
                .open_read_child(&name)
                .await
                .map_err(|_| "copy_manifest_open_failed".to_owned())?
                .into_std()
                .await,
        );
        let pointer = serde_json::to_vec(&Current {
            version: 1,
            manifest: &name,
            sha256: &sha256,
        })
        .map_err(|_| "copy_manifest_encode".to_owned())?;
        let stop = retirement.clone();
        let publisher = self.publisher.clone();
        self.observation
            .atomic_write_child_with_commit("current.json", &pointer, move |rename| {
                publisher.commit(&stop, rename)
            })
            .await
            .map_err(|_| "copy_manifest_pointer_failed".to_owned())?;
        let checkpoint = Arc::new(CopyRangeCheckpoint {
            manifest_path: self.observation_path.join(&name),
            manifest_sha256: sha256,
            timescale,
            canonical_title_start: self.canonical_title_start,
            source_origin: self.source_origin,
            rows: Arc::from(self.rows.clone()),
            boundaries,
            first_frame_rendered: self.rendered,
            inspection: self.inspection.clone(),
            row_inspections: Arc::from(
                self.row_inspections
                    .iter()
                    .map(|value| CopyRowInspection {
                        dts: value.dts,
                        duration: value.duration,
                        packets: value.packets,
                        slices: value.slices,
                        configuration_sha256: value.configuration_sha256.clone(),
                        hdr10_mastering: value.hdr10_mastering.clone(),
                        hdr10_light: value.hdr10_light,
                        hdr10_conflict: value.hdr10_conflict,
                    })
                    .collect::<Vec<_>>(),
            ),
            promotion: self.promotion.clone(),
        });
        self.committed_segments = self.segments.len();
        self.committed_eof = self.eof;
        self.current = Some(Arc::clone(&checkpoint));
        self.old.push(OldManifest {
            name,
            file: Arc::downgrade(&file),
        });
        // Current stays strongly held above. Inflight observer File leases
        // alone control superseded cleanup; there is no age timer/watchdog.
        let mut retained = Vec::new();
        for old in self.old.drain(..) {
            if old.file.strong_count() == 0 {
                let _ = self.observation.unlink_child(&old.name).await;
            } else {
                retained.push(old);
            }
        }
        self.old = retained;
        Ok(checkpoint)
    }
}
impl CopyRangeFiles {
    /// The immutable checkpoint manifest of everything held, bound to the
    /// range's owner.
    fn manifest(
        &self,
        timescale: u32,
        media_origin_ms: i64,
        boundaries: RangeBoundaries,
    ) -> Result<Vec<u8>, String> {
        let mut manifest = Manifest {
            version: 1,
            catalog_namespace: &self.catalog_namespace,
            source: &self.source,
            source_incarnation: None,
            source_request: None,
            source_owner_key: None,
            owner: None,
            local_session: None,
            local_user_id: None,
            local_playback_id: None,
            range_id: self.range_id,
            range_epoch: self.range_epoch,
            checkpoint_sequence: self.sequence,
            init_contract_sha256: &self.contract,
            canonical_init_sha256: &self.canonical_init_sha256,
            av_init_sha256: &self.av_init_sha256,
            source_origin_dts: self.source_origin.dts,
            source_time_base_num: self.source_origin.time_base_num,
            source_time_base_den: self.source_origin.time_base_den,
            canonical_title_start: self.canonical_title_start,
            timescale,
            media_origin_ms,
            target_duration: 16,
            begins_title: boundaries.begins_title,
            eof: boundaries.eof,
            first_frame_rendered: self.rendered,
            rows: &self.rows,
            segments: &self.segments,
            row_inspections: &self.row_inspections,
            inspection_packets: self.inspection.as_ref().map(|proof| proof.packets),
            inspection_slices: self.inspection.as_ref().map(|proof| proof.slices),
            inspection_configuration_sha256: self
                .inspection
                .as_ref()
                .map(|proof| proof.configuration_sha256.as_str()),
            promotion: &self.promotion,
        };
        match &self.owner {
            OwnerBinding::Source {
                incarnation,
                request,
                owner_key,
            } => {
                manifest.source_incarnation = Some(incarnation);
                manifest.source_request = Some(request);
                manifest.source_owner_key = Some(owner_key);
            }
            OwnerBinding::Local {
                session,
                user_id,
                playback_id,
            } => {
                manifest.owner = Some("local");
                manifest.local_session = Some(session);
                manifest.local_user_id = Some(*user_id);
                manifest.local_playback_id = Some(playback_id);
            }
        }
        serde_json::to_vec(&manifest).map_err(|_| "copy_manifest_encode".to_owned())
    }
}
fn fmp4_name(index: u32) -> String {
    plurx_core::fmp4::segment_name(u64::from(index))
}

#[cfg(test)]
mod publisher_tests {
    use super::*;
    #[test]
    fn retired_copy_publisher_never_renames() {
        let publisher = CopyRangePublisher::default();
        let stop = CancellationToken::new();
        let mut calls = 0;
        publisher
            .commit(&stop, &mut || {
                calls += 1;
                Ok(())
            })
            .expect("storage fixture");
        publisher.retire();
        assert!(publisher
            .commit(&stop, &mut || {
                calls += 1;
                Ok(())
            })
            .is_err());
        assert_eq!(calls, 1);
    }
    #[test]
    fn cancelled_copy_publisher_never_renames() {
        let publisher = CopyRangePublisher::default();
        let stop = CancellationToken::new();
        stop.cancel();
        let mut renamed = false;
        assert!(publisher
            .commit(&stop, &mut || {
                renamed = true;
                Ok(())
            })
            .is_err());
        assert!(!renamed);
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod owner_tests;
#[cfg(test)]
pub(crate) use owner_tests::a_source_owner_through_its_real_binding;
