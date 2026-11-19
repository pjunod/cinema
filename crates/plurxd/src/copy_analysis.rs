//! Metadata contributions from an already committed copy range. No input here
//! grants physical source I/O, a complete-source proof, or media publication.
//! Rows the playback ranges left unobserved are produced by an Analysis-owned
//! generation of the same copy engine, inside its own exact physical phase.

#[cfg(all(test, target_os = "linux"))]
mod gap_fill_tests;
mod physical;
#[cfg(test)]
mod quarantine_tests;
mod work;

pub(crate) use work::{
    install_blob, register_playback_cache, v3_blob_path, v3_root, work_copy_observations,
    CopyAnalysisContext,
};

use plurx_core::{
    fmp4::{Init, PromotionInputs},
    hevc_configuration::RangeInspection,
    segplan::{
        ranges::{RangeBoundaries, SourceTimestamp},
        IndexRow,
    },
    sharing_source_sessions::SourceDispatchAssignment,
    store::analysis_coordination::{
        AnalysisCoordinationSource, AnalysisPhysicalPhase, AnalysisSourceInterestAuthority,
    },
};
use std::{path::PathBuf, sync::Arc};

/// Per-row inspection is correlated by the physical producer with its exact
/// clock/sample interval. Packet numbers alone are local to a seeked child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CopyRowInspection {
    pub(crate) dts: u64,
    pub(crate) duration: u64,
    pub(crate) packets: u64,
    pub(crate) slices: u64,
    pub(crate) configuration_sha256: String,
    /// This row's own HDR10 SEI facts, from every sample of its canonical
    /// fragment (hex mastering-display payload, content-light maxima).
    pub(crate) hdr10_mastering: Option<String>,
    pub(crate) hdr10_light: Option<[u16; 2]>,
    /// The fragment's HDR10 SEI could not be read as one consistent set of
    /// facts. Playback is unaffected; the legacy index pass owns such a title.
    pub(crate) hdr10_conflict: bool,
}
impl CopyRowInspection {
    fn hdr10(&self) -> Result<plurx_core::fmp4::Hdr10IndexMetadata, CopyObservationError> {
        if self.hdr10_conflict {
            return Err(CopyObservationError::Hdr10);
        }
        let mastering = self
            .hdr10_mastering
            .as_deref()
            .map(hex::decode)
            .transpose()
            .map_err(|_| CopyObservationError::Inspection)?;
        plurx_core::fmp4::Hdr10IndexMetadata::from_parts(mastering, self.hdr10_light)
            .map_err(|_| CopyObservationError::Inspection)
    }
}

/// Rows, references and checkpoint are committed together by the range owner.
/// An observer message follows that commit and never substitutes for it.
pub(crate) struct CopyRangeCheckpoint {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_sha256: String,
    pub(crate) timescale: u32,
    pub(crate) canonical_title_start: u64,
    pub(crate) source_origin: SourceTimestamp,
    pub(crate) rows: Arc<[IndexRow]>,
    pub(crate) boundaries: RangeBoundaries,
    pub(crate) first_frame_rendered: bool,
    pub(crate) inspection: Option<RangeInspection>,
    pub(crate) row_inspections: Arc<[CopyRowInspection]>,
    pub(crate) promotion: PromotionInputs,
}

/// The one copy recipe of a range: its canonical video options and the audio
/// half of the AV companion the kernel verified those rows against. Playback
/// and an Analysis gap fill both build their child's arguments from it.
#[derive(Clone)]
pub(crate) struct CopyRecipe {
    pub(crate) video: plurx_core::transcode::CopyVideoOptions,
    pub(crate) audio_index: Option<i64>,
    pub(crate) transcode_audio: bool,
    pub(crate) audio_delivery: Option<plurx_core::playback::audio::AudioDelivery>,
}
impl CopyRecipe {
    /// Triple-output arguments: AV on stdout, canonical MP4 on descriptor 6
    /// and the packet clock on descriptor 7.
    pub(crate) fn args(
        &self,
        file: &plurx_core::domain::MediaFile,
        start_seconds: f64,
    ) -> Result<Vec<String>, &'static str> {
        plurx_core::transcode::copy_pipe_args_with_index_and_packet_clock(
            file,
            start_seconds,
            self.audio_index,
            self.transcode_audio,
            plurx_core::transcode::Pacing::unpaced(),
            self.video,
            self.audio_delivery.as_ref(),
            "pipe:6",
            "pipe:7",
        )
    }
    /// The planner reserves conservative audio headroom; actual AV bytes and
    /// repaired presentation span are still checked by the kernel.
    pub(crate) fn tracks(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> plurx_core::segplan::TrackDurations {
        let audio_bits_per_second = self
            .audio_delivery
            .as_ref()
            .and_then(|audio| audio.bitrate_kbps())
            .map(|rate| rate.saturating_mul(1000))
            .unwrap_or(if self.transcode_audio { 256000 } else { 640000 });
        plurx_core::segplan::TrackDurations {
            video_ms: file.duration_ms.unwrap_or(0),
            audio_ms: file.duration_ms.unwrap_or(0),
            audio_bits_per_second,
        }
    }
}

/// Rows one Analysis-owned generation observed. They were never served, so
/// they carry no manifest, range identity or rendered milestone; the exact
/// physical phase that read them is their authority.
pub(crate) struct AnalysisCopyRows {
    pub(crate) init: Arc<Init>,
    pub(crate) source_origin: SourceTimestamp,
    pub(crate) canonical_title_start: u64,
    pub(crate) rows: Vec<IndexRow>,
    pub(crate) boundaries: RangeBoundaries,
    pub(crate) inspection: Option<RangeInspection>,
    pub(crate) row_inspections: Vec<CopyRowInspection>,
    pub(crate) promotion: PromotionInputs,
}

/// One batch of observed rows, whichever owner produced it.
struct ObservedRows<'a> {
    init: &'a Init,
    timescale: u32,
    source_origin: SourceTimestamp,
    canonical_title_start: u64,
    rows: &'a [IndexRow],
    boundaries: RangeBoundaries,
    inspection: Option<&'a RangeInspection>,
    row_inspections: &'a [CopyRowInspection],
    promotion: &'a PromotionInputs,
}

/// The authority an observation or a demand arrived under. A Source
/// observation carries its grant-scoped assignment; a local one carries only
/// the fact that a local user's own playback asked. Neither can be made from
/// the other: no Source observation mints a Local authority, and a Local
/// authority holds no grant, assignment or Source interest.
pub(crate) enum CopyObservationAuthority {
    Source {
        assignment: Box<SourceDispatchAssignment>,
        authority: AnalysisSourceInterestAuthority,
    },
    Local(LocalCopyAuthority),
}

/// A local user's own playback of this node's file. [`LocalCopyAuthority::of`]
/// makes one from a viewer and refuses a Sharing principal, so shared
/// playback can never present itself as local demand. The two other
/// constructors only carry one across a restart: `of_durable_request` gives
/// back what a request this worker wrote records, and `unattributed` stands
/// for an asker that no longer exists. It is attribution and admission to
/// the worker's inbox, nothing more: it grants no source read, which every
/// Analysis phase still takes from the store, so an authority read back from
/// a row confers nothing a row could not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalCopyAuthority {
    user_id: i64,
    playback_id: String,
}
impl LocalCopyAuthority {
    pub(crate) fn of(viewer: &crate::state::PlaybackViewerDemand) -> Result<Self, &'static str> {
        viewer.require_local_authority()?;
        match &viewer.principal {
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id } => Ok(Self {
                user_id: *user_id,
                playback_id: viewer.playback_id.clone(),
            }),
            plurx_core::playback_principal::PlaybackPrincipal::Sharing { .. } => {
                Err("sharing playback demand requires typed source authority")
            }
        }
    }
    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }
    pub(crate) fn playback_id(&self) -> &str {
        &self.playback_id
    }
    /// Local demand with nobody to name: the user a durable request records
    /// has since been removed. The build is the title's, not the user's.
    pub(crate) fn unattributed() -> Self {
        Self {
            user_id: 0,
            playback_id: String::new(),
        }
    }
    /// Whether this names a user.
    pub(crate) fn is_attributed(&self) -> bool {
        self.user_id > 0
    }
    /// The authority a durable request recorded. A request under
    /// [`LOCAL_PLAYBACK_REQUEST_PREFIX`] is written only by this worker for
    /// a demand `of` admitted, so a restarted worker that finds one takes
    /// the build up again under the same attribution. The playback is empty
    /// when the id had no room for it; an id that names no user is
    /// unattributed.
    pub(crate) fn of_durable_request(request_id: &str) -> Option<Self> {
        let rest = request_id.strip_prefix(LOCAL_PLAYBACK_REQUEST_PREFIX)?;
        let Some(rest) = rest.strip_prefix('u') else {
            return Some(Self::unattributed());
        };
        let (user, rest) = rest.split_once('.')?;
        let user_id = user.parse::<i64>().ok().filter(|user_id| *user_id > 0)?;
        let playback_id = rest
            .rsplit_once('.')
            .map(|(playback, _)| playback.to_owned())
            .unwrap_or_default();
        Some(Self {
            user_id,
            playback_id,
        })
    }
    /// The id of the Analysis request this demand creates, which is how the
    /// build is attributed wherever requests are listed: it names the local
    /// user and, when it fits the 64-byte bound and is plain enough to sit
    /// in a URL path, the playback that asked. `analysis_requests.trigger`
    /// is constrained by the schema to three values, so the attribution
    /// lives in the one free-form field every listing already shows.
    pub(crate) fn request_id(&self) -> String {
        let unique = uuid::Uuid::new_v4().simple().to_string();
        // Still recognisably a local demand's request, naming nobody.
        let anonymous = format!("{LOCAL_PLAYBACK_REQUEST_PREFIX}{unique}");
        if !self.is_attributed() {
            return anonymous;
        }
        let plain = !self.playback_id.is_empty()
            && self
                .playback_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        let full = format!(
            "{LOCAL_PLAYBACK_REQUEST_PREFIX}u{}.{}.{}",
            self.user_id,
            self.playback_id,
            &unique[..4]
        );
        if plain && full.len() <= 64 {
            return full;
        }
        let short = format!("{LOCAL_PLAYBACK_REQUEST_PREFIX}u{}.{unique}", self.user_id);
        if short.len() <= 64 {
            short
        } else {
            anonymous
        }
    }
}

/// What the id of an Analysis request raised by local playback starts with.
pub(crate) const LOCAL_PLAYBACK_REQUEST_PREFIX: &str = "local-play.";

/// A demand for the copy index of one exact source that carries no rows: the
/// playback that raised it is served by another path and observed nothing.
/// The Analysis worker builds the index with its own generations of the copy
/// engine once the source domain is idle, exactly as it fills a gap.
pub(crate) struct CopyIndexDemand {
    pub(crate) file: Arc<plurx_core::domain::MediaFile>,
    pub(crate) catalog_namespace: String,
    pub(crate) source: AnalysisCoordinationSource,
    /// The copy recipe of the playback that asked; the fill repeats it.
    pub(crate) recipe: CopyRecipe,
    pub(crate) authority: CopyObservationAuthority,
    /// The coordination request a previous process made for this demand,
    /// when a restarted worker took it up. None for a demand a start raised.
    pub(crate) durable_request: Option<String>,
}
impl CopyIndexDemand {
    /// A local viewer's demand for the index of the file it is playing on
    /// this node. Refused for anything but a local user's own playback.
    pub(crate) fn local(
        viewer: &crate::state::PlaybackViewerDemand,
        file: Arc<plurx_core::domain::MediaFile>,
        catalog_namespace: String,
        source: AnalysisCoordinationSource,
        recipe: CopyRecipe,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            file,
            catalog_namespace,
            source,
            recipe,
            authority: CopyObservationAuthority::Local(LocalCopyAuthority::of(viewer)?),
            durable_request: None,
        })
    }
}

pub(crate) struct CompletedCopyRange {
    pub(crate) file: Arc<plurx_core::domain::MediaFile>,
    pub(crate) catalog_namespace: String,
    pub(crate) source: AnalysisCoordinationSource,
    pub(crate) authority: CopyObservationAuthority,
    pub(crate) range_id: uuid::Uuid,
    pub(crate) range_epoch: u64,
    pub(crate) init: Arc<Init>,
    pub(crate) checkpoint: Arc<CopyRangeCheckpoint>,
    pub(crate) recipe: CopyRecipe,
}

/// What a reader found wrong with the variant a healthy head points at. Each
/// is a fact about that exact head epoch and blob; the Analysis worker
/// re-reads the head and quarantines it under its own claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyIndexDefectKind {
    /// The verified blob was sealed without the whole-title HDR10 fold.
    UnindexedHdr10,
    /// No node holds the bytes: this node has none and every listed holder
    /// answered that it has none either.
    NoHolder,
    /// A generation's promoted muxer init is not the init the seal published.
    SealedInit,
}
impl CopyIndexDefectKind {
    /// The quarantine ledger code of this defect.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::UnindexedHdr10 => "copy_index_hdr10_unindexed",
            Self::NoHolder => plurx_core::store::COPY_INDEX_V3_NO_HOLDER_CODE,
            Self::SealedInit => plurx_core::store::COPY_INDEX_V3_SEALED_INIT_CODE,
        }
    }
}

/// A reader's typed demand to rebuild one head. It carries no authority: the
/// store quarantines only the exact healthy head, epoch and blob it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CopyIndexDefect {
    pub(crate) source_key: String,
    pub(crate) head_epoch: i64,
    pub(crate) blob_sha256: String,
    pub(crate) kind: CopyIndexDefectKind,
}

/// Everything the Analysis worker's inbox carries.
pub(crate) enum CopyAnalysisMessage {
    Range(Box<CompletedCopyRange>),
    Demand(Box<CopyIndexDemand>),
    Defect(CopyIndexDefect),
}

pub(crate) type CopyAnalysisInbox = tokio::sync::mpsc::Sender<CopyAnalysisMessage>;

/// The existing Analysis worker consumes these notifications. The channel is
/// deliberately bounded; durable manifests retain observations across retries.
pub(crate) struct CopyAnalysisRegistry {
    sender: CopyAnalysisInbox,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<CopyAnalysisMessage>>,
    /// Coalesced per-source observations. A pass holds this lock only to fold
    /// the inbox and hand ready sources to their own units.
    pending: Arc<tokio::sync::Mutex<work::PendingSources>>,
    /// When this process's registry was made. A queued coordination request
    /// older than this was left by a previous process.
    created_ms: i64,
    /// Set once this process has retired what its predecessor stranded.
    stranded_retired: tokio::sync::OnceCell<()>,
    /// Source keys a demand was already sent for and whose work has not
    /// settled. A start checks it before the inbox, so a title started
    /// again and again sends one message, not one per start.
    asked: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}
impl Default for CopyAnalysisRegistry {
    fn default() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
            pending: Default::default(),
            created_ms: crate::state::clock_ms(),
            stranded_retired: tokio::sync::OnceCell::new(),
            asked: Default::default(),
        }
    }
}
impl CopyAnalysisRegistry {
    /// Deliver one committed range and wake the existing Analysis dispatcher.
    /// A full inbox refuses; the producer rebuilds the latest durable
    /// checkpoint's message on its own next lease or production event.
    pub(crate) fn notify(&self, message: CompletedCopyRange) -> bool {
        self.send(CopyAnalysisMessage::Range(Box::new(message)))
    }
    /// Hand the worker a row-less demand for one source's index, unless one
    /// was already sent and its work has not settled. A full inbox refuses,
    /// and the next start of the same title asks again.
    pub(crate) fn notify_demand(&self, demand: CopyIndexDemand) -> bool {
        let Ok(key) =
            plurx_core::store::copy_index_v3_source_key(&demand.catalog_namespace, &demand.source)
        else {
            return false;
        };
        let mut asked = self
            .asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if asked.contains(&key) {
            return false;
        }
        let sent = self.send(CopyAnalysisMessage::Demand(Box::new(demand)));
        if sent {
            asked.insert(key);
        }
        sent
    }
    /// The already-asked keys, for the worker to clear as sources settle.
    pub(crate) fn asked(&self) -> Arc<std::sync::Mutex<std::collections::HashSet<String>>> {
        Arc::clone(&self.asked)
    }
    /// Hand the worker one defect a reader found. A full inbox refuses, and
    /// the next start that meets the same head reports it again.
    pub(crate) fn notify_defect(&self, defect: CopyIndexDefect) -> bool {
        self.send(CopyAnalysisMessage::Defect(defect))
    }
    fn send(&self, message: CopyAnalysisMessage) -> bool {
        let sent = self.sender.try_send(message).is_ok();
        if sent {
            crate::state::wake_fragment_analysis();
        }
        sent
    }
    /// Run `retire` once per process, before its first pass works anything.
    /// A failed attempt is made again by the next pass.
    pub(crate) async fn retire_stranded_once<F, Fut>(&self, retire: F)
    where
        F: FnOnce(i64) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let created_ms = self.created_ms;
        if let Err(error) = self
            .stranded_retired
            .get_or_try_init(|| retire(created_ms))
            .await
        {
            tracing::warn!(%error, "stranded copy coordination requests were not retired");
        }
    }
    /// The per-source state every pass and unit shares.
    pub(crate) fn pending(&self) -> Arc<tokio::sync::Mutex<work::PendingSources>> {
        Arc::clone(&self.pending)
    }
    /// Everything the inbox holds now (at most its two slots).
    pub(crate) fn take_pending(&self) -> Vec<CopyAnalysisMessage> {
        let Ok(mut receiver) = self.receiver.try_lock() else {
            return Vec::new();
        };
        let mut messages = Vec::with_capacity(2);
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }
        messages
    }
}

/// Normal physical-object generation shared by Local and Source observations
/// on the designated origin. It contains no Sharing grant, revision HMAC or
/// global settings generation. Logical origin election remains the claim's job.
pub(crate) fn source_for_copy(
    catalog_namespace: &str,
    file: &plurx_core::domain::MediaFile,
    video_identity: &str,
    object_version: &str,
    origin_node_id: &str,
) -> Result<AnalysisCoordinationSource, plurx_core::error::StoreError> {
    use sha2::{Digest, Sha256};
    let mut source = AnalysisCoordinationSource {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        origin_node_id: origin_node_id.to_owned(),
        object_version: object_version.to_owned(),
        video_identity: video_identity.to_owned(),
        contract_version: plurx_core::store::FRAGMENT_INDEX_CONTRACT,
        requested_generation: "normal".to_owned(),
    };
    let key = plurx_core::store::copy_index_v3_source_key(catalog_namespace, &source)?;
    let mut digest = Sha256::new();
    digest.update(b"plurx/copy-analysis/normal-generation/v1\0");
    digest.update(key.as_bytes());
    source.requested_generation = hex::encode(digest.finalize());
    Ok(source)
}

#[derive(Debug)]
pub(crate) enum CopyObservationError {
    Binding,
    Init,
    Inspection,
    Coverage,
    Incomplete,
    Attestation,
    Overflow,
    /// HDR10 facts that cannot fold into one whole-title answer.
    Hdr10,
}

/// Metadata-only accumulation. Overlap equality and complete begin/EOF/gap-free
/// coverage are enforced independently of media, reader and Source permissions.
pub(crate) struct CopyObservationSet {
    /// The source this set seals for. Its generation is the observations'
    /// own until the set adopts a quarantined head's repair generation.
    source: AnalysisCoordinationSource,
    /// The source every playback observation of this set is built with.
    observed_source: AnalysisCoordinationSource,
    catalog_namespace: String,
    init: Arc<Init>,
    init_contract_sha256: String,
    source_origin: SourceTimestamp,
    canonical_title_start: u64,
    promotion: PromotionInputs,
    rows: plurx_core::segplan::ranges::SparseIndexRows,
    inspections: std::collections::BTreeMap<u64, CopyRowInspection>,
    /// The order-free fold of every held row's HDR10 facts, so a conflict
    /// discards the set when it arrives rather than after every gap read.
    hdr10: plurx_core::fmp4::Hdr10IndexMetadata,
    hevc: bool,
}
impl CopyObservationSet {
    pub(crate) fn source(&self) -> &AnalysisCoordinationSource {
        &self.source
    }
    pub(crate) fn catalog_namespace(&self) -> &str {
        &self.catalog_namespace
    }
    pub(crate) fn init_contract_sha256(&self) -> &str {
        &self.init_contract_sha256
    }
    pub(crate) fn is_complete(&self) -> bool {
        self.rows.is_complete()
    }
    /// The earliest span no generation has observed yet.
    pub(crate) fn first_gap(&self) -> Option<plurx_core::segplan::ranges::RangeGap> {
        self.rows.first_gap()
    }
    pub(crate) fn timescale(&self) -> u32 {
        self.rows.timescale()
    }
    pub(crate) fn canonical_title_start(&self) -> u64 {
        self.canonical_title_start
    }
    pub(crate) fn observed_rows(&self) -> usize {
        self.rows.rows().len()
    }
    /// Seal for `generation` instead: the head of this source was quarantined
    /// and desires exactly this repair generation. The rows are facts about
    /// the source object, which the generation does not name, so they stay;
    /// later playback observations still arrive under their own generation
    /// and still fold in.
    pub(crate) fn adopt_generation(&mut self, generation: &str) {
        self.source.requested_generation = generation.to_owned();
    }
    /// Why these observations contradict `published`, a decoded index of the
    /// same source, or `None` when every observed row is in it unchanged.
    /// Only the observed rows are judged: a set compared with a healthy head
    /// holds the ranges just committed, never the whole title.
    pub(crate) fn contradiction(
        &self,
        published: &plurx_core::segplan::FragmentIndex,
    ) -> Option<String> {
        if published.timescale != self.rows.timescale() {
            return Some(format!(
                "timescale {} was published, {} observed",
                published.timescale,
                self.rows.timescale()
            ));
        }
        // An unordered table cannot be searched; it is not this check's to judge.
        if !published
            .rows
            .windows(2)
            .all(|pair| pair[0].dts < pair[1].dts)
        {
            return None;
        }
        for row in self.rows.rows() {
            match published
                .rows
                .binary_search_by_key(&row.dts, |held| held.dts)
            {
                Ok(at) if &published.rows[at] == row => {}
                Ok(_) => return Some(format!("the row at {} differs", row.dts)),
                Err(_) => return Some(format!("no row was published at {}", row.dts)),
            }
        }
        None
    }
    pub(crate) fn new(message: &CompletedCopyRange) -> Result<Self, CopyObservationError> {
        let mut value = Self::empty(
            &message.catalog_namespace,
            &message.source,
            &message.init,
            message.checkpoint.timescale,
            message.checkpoint.source_origin,
            message.checkpoint.canonical_title_start,
            &message.checkpoint.promotion,
        )?;
        value.observe(message)?;
        Ok(value)
    }

    /// The set of one exact source before any rows: its binding is the source
    /// key, generation and the init contract its observations must share.
    fn empty(
        catalog_namespace: &str,
        source: &AnalysisCoordinationSource,
        init: &Arc<Init>,
        timescale: u32,
        source_origin: SourceTimestamp,
        canonical_title_start: u64,
        promotion: &PromotionInputs,
    ) -> Result<Self, CopyObservationError> {
        use plurx_core::segplan::ranges::{RangeBinding, SparseIndexRows};
        let init_contract_sha256 = plurx_core::fmp4::copy_video_init_contract_sha256(init)
            .map_err(|_| CopyObservationError::Init)?;
        let key = plurx_core::store::copy_index_v3_source_key(catalog_namespace, source)
            .map_err(|_| CopyObservationError::Binding)?;
        let binding = RangeBinding {
            source_generation: source.requested_generation.clone(),
            contract: format!("{}:{key}:{init_contract_sha256}", source.contract_version),
        };
        let rows = SparseIndexRows::new(
            binding,
            timescale,
            canonical_title_start,
            plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
        )
        .map_err(|_| CopyObservationError::Coverage)?;
        let hevc = init
            .video()
            .is_some_and(|track| track.codec == Some(plurx_core::fmp4::VideoCodec::Hevc));
        Ok(Self {
            source: source.clone(),
            observed_source: source.clone(),
            catalog_namespace: catalog_namespace.to_owned(),
            init: Arc::clone(init),
            init_contract_sha256,
            source_origin,
            canonical_title_start,
            promotion: promotion.clone(),
            rows,
            inspections: Default::default(),
            hdr10: Default::default(),
            hevc,
        })
    }

    pub(crate) fn observe(
        &mut self,
        message: &CompletedCopyRange,
    ) -> Result<(), CopyObservationError> {
        let checkpoint = &message.checkpoint;
        if message.file.id != self.source.file_id
            || message.file.size != self.source.source_size
            || message.file.mtime != self.source.source_mtime
        {
            return Err(CopyObservationError::Binding);
        }
        match &message.authority {
            CopyObservationAuthority::Source { assignment, .. } => {
                if assignment.owner_node_id() != self.source.origin_node_id {
                    return Err(CopyObservationError::Binding);
                }
            }
            // A local range has no assignment to bind; its source, checked
            // below, names this node as the origin of the object it read.
            CopyObservationAuthority::Local(_) => {}
        }
        if message.source != self.observed_source
            || message.catalog_namespace != self.catalog_namespace
            || !checkpoint.first_frame_rendered
            || message.range_epoch == 0
            || !valid_copy_digest(&checkpoint.manifest_sha256)
            || checkpoint.manifest_path.as_os_str().is_empty()
        {
            return Err(CopyObservationError::Binding);
        }
        self.merge(ObservedRows {
            init: &message.init,
            timescale: checkpoint.timescale,
            source_origin: checkpoint.source_origin,
            canonical_title_start: checkpoint.canonical_title_start,
            rows: &checkpoint.rows,
            boundaries: checkpoint.boundaries,
            inspection: checkpoint.inspection.as_ref(),
            row_inspections: &checkpoint.row_inspections,
            promotion: &checkpoint.promotion,
        })
    }

    /// The set of a source nothing has observed yet, from the first rows an
    /// Analysis-owned generation read of it: they establish the init
    /// contract, clock origin and promotion every later observation must
    /// share. `source` is the source as playback observes it, under its own
    /// generation, so later playback ranges still fold in; when the phase
    /// ran for a quarantined head's repair generation the set adopts that,
    /// exactly as a set of playback rows does.
    pub(crate) fn from_analysis(
        catalog_namespace: &str,
        source: &AnalysisCoordinationSource,
        phase: &AnalysisPhysicalPhase,
        observed: &AnalysisCopyRows,
    ) -> Result<Self, CopyObservationError> {
        let timescale = observed
            .init
            .video()
            .ok_or(CopyObservationError::Init)?
            .timescale;
        let mut value = Self::empty(
            catalog_namespace,
            source,
            &observed.init,
            timescale,
            observed.source_origin,
            observed.canonical_title_start,
            &observed.promotion,
        )?;
        if phase.source.requested_generation != source.requested_generation {
            value.adopt_generation(&phase.source.requested_generation);
        }
        value.observe_analysis(phase, observed)?;
        Ok(value)
    }

    /// Rows from an Analysis-owned generation. The phase must be this exact
    /// source's, held by its origin node. They pass the same init, inspection
    /// and overlap-equality checks as a playback range's rows.
    pub(crate) fn observe_analysis(
        &mut self,
        phase: &AnalysisPhysicalPhase,
        observed: &AnalysisCopyRows,
    ) -> Result<(), CopyObservationError> {
        if phase.source != self.source || phase.claim.node_id != self.source.origin_node_id {
            return Err(CopyObservationError::Binding);
        }
        let timescale = observed
            .init
            .video()
            .ok_or(CopyObservationError::Init)?
            .timescale;
        self.merge(ObservedRows {
            init: &observed.init,
            timescale,
            source_origin: observed.source_origin,
            canonical_title_start: observed.canonical_title_start,
            rows: &observed.rows,
            boundaries: observed.boundaries,
            inspection: observed.inspection.as_ref(),
            row_inspections: &observed.row_inspections,
            promotion: &observed.promotion,
        })
    }

    fn merge(&mut self, observed: ObservedRows<'_>) -> Result<(), CopyObservationError> {
        if observed.source_origin != self.source_origin
            || observed.canonical_title_start != self.canonical_title_start
            || observed.timescale != self.rows.timescale()
            || observed.promotion != &self.promotion
            || observed.promotion.hevc_configuration.is_some()
        {
            return Err(CopyObservationError::Binding);
        }
        let init_contract = plurx_core::fmp4::copy_video_init_contract_sha256(observed.init)
            .map_err(|_| CopyObservationError::Init)?;
        if init_contract != self.init_contract_sha256 {
            return Err(CopyObservationError::Init);
        }
        let mut hdr10 = self.hdr10.clone();
        if self.hevc {
            if observed.row_inspections.len() != observed.rows.len() {
                return Err(CopyObservationError::Inspection);
            }
            let graph = self
                .inspections
                .values()
                .next()
                .map(|inspection| inspection.configuration_sha256.as_str());
            for (row, inspection) in observed.rows.iter().zip(observed.row_inspections.iter()) {
                if inspection.dts != row.dts
                    || inspection.duration != row.duration
                    || inspection.packets == 0
                    || inspection.slices < inspection.packets
                    || !valid_copy_digest(&inspection.configuration_sha256)
                    || graph.is_some_and(|digest| digest != inspection.configuration_sha256)
                    || self
                        .inspections
                        .get(&row.dts)
                        .is_some_and(|old| old != inspection)
                    || observed.row_inspections.first().is_some_and(|first| {
                        first.configuration_sha256 != inspection.configuration_sha256
                    })
                {
                    return Err(CopyObservationError::Inspection);
                }
                // A held row already agreed byte for byte and is in the fold.
                if !self.inspections.contains_key(&row.dts) {
                    hdr10
                        .merge(&inspection.hdr10()?)
                        .map_err(|_| CopyObservationError::Hdr10)?;
                }
            }
            if let Some(range) = observed.inspection {
                let (packets, slices) = observed
                    .row_inspections
                    .iter()
                    .try_fold((0u64, 0u64), |(p, s), row| {
                        Some((p.checked_add(row.packets)?, s.checked_add(row.slices)?))
                    })
                    .ok_or(CopyObservationError::Overflow)?;
                if range.packets != packets
                    || range.slices != slices
                    || observed
                        .row_inspections
                        .first()
                        .is_none_or(|row| row.configuration_sha256 != range.configuration_sha256)
                {
                    return Err(CopyObservationError::Inspection);
                }
            } else if observed.boundaries.eof {
                return Err(CopyObservationError::Inspection);
            }
        } else if !observed.row_inspections.is_empty() || observed.inspection.is_some() {
            return Err(CopyObservationError::Inspection);
        }
        let binding = self.rows.binding().clone();
        self.rows
            .insert(&binding, observed.rows, observed.boundaries)
            .map_err(|_| CopyObservationError::Coverage)?;
        for inspection in observed.row_inspections.iter() {
            self.inspections
                .entry(inspection.dts)
                .or_insert_with(|| inspection.clone());
        }
        self.hdr10 = hdr10;
        Ok(())
    }

    /// The typed receipt must have been loaded/recorded through the fenced
    /// full-source phase API and revalidated against this exact held witness.
    /// A seeked suffix, sampled digest or aggregate trace cannot seal an index.
    pub(crate) fn seal(
        &self,
        attestation: &physical::VerifiedFullSource,
    ) -> Result<SealedCopyIndex, CopyObservationError> {
        if !self.rows.is_complete() {
            return Err(CopyObservationError::Incomplete);
        }
        if !attestation.matches(&self.source) {
            return Err(CopyObservationError::Attestation);
        }
        use sha2::{Digest, Sha256};
        let identity = plurx_core::segplan::SourceIdentity::new(
            u64::try_from(self.source.source_size)
                .map_err(|_| CopyObservationError::Attestation)?,
            self.source.source_mtime,
            self.source.video_identity.clone(),
        );
        let mut index = plurx_core::segplan::FragmentIndex::new(
            self.rows.timescale(),
            self.rows.rows().to_vec(),
            hex::encode(Sha256::digest(&self.init.bytes)),
            identity,
        );
        index.promotion = self.promotion.clone();
        if self.hevc {
            let mut slices = 0u64;
            for row in self.rows.rows() {
                let inspection = self
                    .inspections
                    .get(&row.dts)
                    .ok_or(CopyObservationError::Inspection)?;
                if inspection.duration != row.duration {
                    return Err(CopyObservationError::Inspection);
                }
                slices = slices
                    .checked_add(inspection.slices)
                    .ok_or(CopyObservationError::Overflow)?;
            }
            index.promotion.hevc_configuration = Some(plurx_core::hevc_configuration::Proof {
                revision: plurx_core::hevc_configuration::REVISION,
                source_object_version: self.source.object_version.clone(),
                source_node_id: self.source.origin_node_id.clone(),
                source_sha256: Some(attestation.sha256().to_owned()),
                stream_index: 0,
                slices,
                refusal: None,
            });
        }
        // The whole-title HDR10 maximum the legacy index pass computes, from
        // every row's own facts. Overlapping observations agreed row by row,
        // and the fold is order-free, so this equals one sequential scan.
        index.promotion.hdr10_sei = self.hdr10.promotion_nals();
        index.promotion.indexed_hdr10_metadata = true;
        index.parameter_sets_constant = true;
        // The contract this index promises is the init its own promotion
        // serves, not the head-frozen init the observing copies served.
        let init_contract_sha256 =
            crate::copy_normalization::served_init_contract_sha256(&self.init, &index.promotion)
                .map_err(|_| CopyObservationError::Hdr10)?;
        Ok(SealedCopyIndex {
            index,
            init_contract_sha256,
        })
    }
}

/// A sealed index and the init contract its stored promotion serves.
pub(crate) struct SealedCopyIndex {
    pub(crate) index: plurx_core::segplan::FragmentIndex,
    pub(crate) init_contract_sha256: String,
}
fn valid_copy_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
