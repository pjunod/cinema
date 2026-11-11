//! Metadata contributions from an already committed copy range. No input here
//! grants physical source I/O, a complete-source proof, or media publication.

mod physical;
mod work;

pub(crate) use work::{work_copy_observations, CopyAnalysisContext};

use plurx_core::{
    fmp4::{Init, PromotionInputs},
    hevc_configuration::RangeInspection,
    segplan::{
        ranges::{RangeBoundaries, SourceTimestamp},
        IndexRow,
    },
    sharing_source_sessions::SourceDispatchAssignment,
    store::analysis_coordination::{AnalysisCoordinationSource, AnalysisSourceInterestAuthority},
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
}

/// Rows, references and checkpoint are committed together by the range owner.
/// An observer message follows that commit and never substitutes for it.
pub(crate) struct CopyRangeCheckpoint {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_file: Arc<std::fs::File>,
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

/// Local demand and Shared Source grants remain different authority paths.
/// A Source observation can never mint a Local principal to join Analysis.
pub(crate) enum CopyObservationAuthority {
    Local(crate::state::PlaybackViewerDemand),
    Source {
        assignment: SourceDispatchAssignment,
        authority: AnalysisSourceInterestAuthority,
    },
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
}

pub(crate) type CopyAnalysisInbox = tokio::sync::mpsc::Sender<CompletedCopyRange>;

/// The existing Analysis worker consumes these notifications. The channel is
/// deliberately bounded; durable manifests retain observations across retries.
pub(crate) struct CopyAnalysisRegistry {
    sender: CopyAnalysisInbox,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<CompletedCopyRange>>,
    /// Coalesced per-source observations. Holding this lock is the pass.
    pending: tokio::sync::Mutex<work::PendingSources>,
}
impl Default for CopyAnalysisRegistry {
    fn default() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
            pending: Default::default(),
        }
    }
}
impl CopyAnalysisRegistry {
    pub(crate) fn inbox(&self) -> CopyAnalysisInbox {
        self.sender.clone()
    }
    /// Deliver one committed range and wake the existing Analysis dispatcher.
    /// A full inbox returns the message; the producer retries on its own
    /// next lease or production event.
    pub(crate) fn notify(&self, message: CompletedCopyRange) -> Result<(), CompletedCopyRange> {
        match self.sender.try_send(message) {
            Ok(()) => {
                crate::state::wake_fragment_analysis();
                Ok(())
            }
            Err(error) => Err(error.into_inner()),
        }
    }
    /// At most one pass at a time; a concurrent wake finds it held.
    pub(crate) fn begin_pass(&self) -> Option<tokio::sync::MutexGuard<'_, work::PendingSources>> {
        self.pending.try_lock().ok()
    }
    pub(crate) fn take_pending(&self) -> Vec<CompletedCopyRange> {
        let Ok(mut receiver) = self.receiver.try_lock() else {
            return Vec::new();
        };
        let mut messages = Vec::with_capacity(2);
        for _ in 0..2 {
            match receiver.try_recv() {
                Ok(message) => messages.push(message),
                Err(_) => break,
            }
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
}

/// Metadata-only accumulation. Overlap equality and complete begin/EOF/gap-free
/// coverage are enforced independently of media, reader and Source permissions.
pub(crate) struct CopyObservationSet {
    source: AnalysisCoordinationSource,
    catalog_namespace: String,
    init: Arc<Init>,
    init_contract_sha256: String,
    source_origin: SourceTimestamp,
    canonical_title_start: u64,
    promotion: PromotionInputs,
    rows: plurx_core::segplan::ranges::SparseIndexRows,
    inspections: std::collections::BTreeMap<u64, CopyRowInspection>,
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
    pub(crate) fn new(message: &CompletedCopyRange) -> Result<Self, CopyObservationError> {
        use plurx_core::segplan::ranges::{RangeBinding, SparseIndexRows};
        let init_contract_sha256 = plurx_core::fmp4::copy_video_init_contract_sha256(&message.init)
            .map_err(|_| CopyObservationError::Init)?;
        let key = plurx_core::store::copy_index_v3_source_key(
            &message.catalog_namespace,
            &message.source,
        )
        .map_err(|_| CopyObservationError::Binding)?;
        let binding = RangeBinding {
            source_generation: message.source.requested_generation.clone(),
            contract: format!(
                "{}:{key}:{init_contract_sha256}",
                message.source.contract_version
            ),
        };
        let rows = SparseIndexRows::new(
            binding,
            message.checkpoint.timescale,
            message.checkpoint.canonical_title_start,
            plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
        )
        .map_err(|_| CopyObservationError::Coverage)?;
        let hevc = message
            .init
            .video()
            .is_some_and(|track| track.codec == Some(plurx_core::fmp4::VideoCodec::Hevc));
        let mut value = Self {
            source: message.source.clone(),
            catalog_namespace: message.catalog_namespace.clone(),
            init: Arc::clone(&message.init),
            init_contract_sha256,
            source_origin: message.checkpoint.source_origin,
            canonical_title_start: message.checkpoint.canonical_title_start,
            promotion: message.checkpoint.promotion.clone(),
            rows,
            inspections: Default::default(),
            hevc,
        };
        value.observe(message)?;
        Ok(value)
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
            CopyObservationAuthority::Local(demand) => demand
                .require_local_authority()
                .map_err(|_| CopyObservationError::Binding)?,
            CopyObservationAuthority::Source { assignment, .. } => {
                if assignment.owner_node_id() != self.source.origin_node_id {
                    return Err(CopyObservationError::Binding);
                }
            }
        }
        if message.source != self.source
            || message.catalog_namespace != self.catalog_namespace
            || checkpoint.source_origin != self.source_origin
            || checkpoint.canonical_title_start != self.canonical_title_start
            || checkpoint.timescale != self.rows.timescale()
            || checkpoint.promotion != self.promotion
            || checkpoint.promotion.hevc_configuration.is_some()
            || !checkpoint.first_frame_rendered
            || message.range_epoch == 0
            || !valid_copy_digest(&checkpoint.manifest_sha256)
            || checkpoint.manifest_path.as_os_str().is_empty()
        {
            return Err(CopyObservationError::Binding);
        }
        let init_contract = plurx_core::fmp4::copy_video_init_contract_sha256(&message.init)
            .map_err(|_| CopyObservationError::Init)?;
        if init_contract != self.init_contract_sha256 {
            return Err(CopyObservationError::Init);
        }
        if self.hevc {
            if checkpoint.row_inspections.len() != checkpoint.rows.len() {
                return Err(CopyObservationError::Inspection);
            }
            let graph = self
                .inspections
                .values()
                .next()
                .map(|inspection| inspection.configuration_sha256.as_str());
            for (row, inspection) in checkpoint
                .rows
                .iter()
                .zip(checkpoint.row_inspections.iter())
            {
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
                    || checkpoint.row_inspections.first().is_some_and(|first| {
                        first.configuration_sha256 != inspection.configuration_sha256
                    })
                {
                    return Err(CopyObservationError::Inspection);
                }
            }
            if let Some(range) = &checkpoint.inspection {
                let (packets, slices) = checkpoint
                    .row_inspections
                    .iter()
                    .try_fold((0u64, 0u64), |(p, s), row| {
                        Some((p.checked_add(row.packets)?, s.checked_add(row.slices)?))
                    })
                    .ok_or(CopyObservationError::Overflow)?;
                if range.packets != packets
                    || range.slices != slices
                    || checkpoint
                        .row_inspections
                        .first()
                        .is_none_or(|row| row.configuration_sha256 != range.configuration_sha256)
                {
                    return Err(CopyObservationError::Inspection);
                }
            } else if checkpoint.boundaries.eof {
                return Err(CopyObservationError::Inspection);
            }
        } else if !checkpoint.row_inspections.is_empty() || checkpoint.inspection.is_some() {
            return Err(CopyObservationError::Inspection);
        }
        let binding = self.rows.binding().clone();
        self.rows
            .insert(&binding, &checkpoint.rows, checkpoint.boundaries)
            .map_err(|_| CopyObservationError::Coverage)?;
        for inspection in checkpoint.row_inspections.iter() {
            self.inspections
                .entry(inspection.dts)
                .or_insert_with(|| inspection.clone());
        }
        Ok(())
    }

    /// The typed receipt must have been loaded/recorded through the fenced
    /// full-source phase API and revalidated against this exact held witness.
    /// A seeked suffix, sampled digest or aggregate trace cannot seal an index.
    pub(crate) fn seal(
        &self,
        attestation: &physical::VerifiedFullSource,
    ) -> Result<plurx_core::segplan::FragmentIndex, CopyObservationError> {
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
        index.parameter_sets_constant = true;
        Ok(index)
    }
}
fn valid_copy_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
