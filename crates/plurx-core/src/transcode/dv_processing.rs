//! Bounded DV processing contracts and completed-window output validation.
//!
//! Intent, source sampling and per-emitted-frame observations are separate.
//! Saved intent and claimed helper scope never grant a processing route.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{DecodeBackend, DecodeCacheIdentity, DecodeSourceIdentity, Encoder, PlanSourceBinding};
use crate::domain::DolbyVisionFacts;

fn encoder_token(encoder: Encoder) -> &'static str {
    match encoder {
        Encoder::Software => "software",
        Encoder::Nvenc => "nvenc",
        Encoder::Qsv => "qsv",
        Encoder::Vaapi => "vaapi",
        Encoder::VideoToolbox => "videotoolbox",
    }
}
fn serialize_encoder<S: serde::Serializer>(
    encoder: &Encoder,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(encoder_token(*encoder))
}

pub const DV_CONTRACT_VERSION: u32 = 1;
pub const DV_MAX_CONTROL_FRAMES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DvContractError {
    #[error("invalid DV contract: {0}")]
    Invalid(&'static str),
    #[error("production DV qualification is unavailable")]
    ProductionQualificationUnavailable,
    #[error("duplicate or ambiguous DV frame")]
    DuplicateFrame,
    #[error("incomplete DV emitted-frame coverage")]
    IncompleteCoverage,
    #[error("bounded DV control capacity exhausted")]
    Capacity,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DvDigest(String);

impl DvDigest {
    pub fn new(value: String) -> Result<Self, DvContractError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DvContractError::Invalid("SHA-256"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bind the existing active playback UUID to M1's processing-generation digest.
/// The future receipt owner and this presentation projection use the same helper.
pub fn dv_playback_generation_digest(generation: &str) -> Result<DvDigest, DvContractError> {
    let generation = uuid::Uuid::parse_str(generation)
        .map_err(|_| DvContractError::Invalid("playback generation UUID"))?;
    let mut digest = Sha256::new();
    digest.update(b"plurx.dv.playback-generation.v1\0");
    digest.update(generation.as_bytes());
    Ok(DvDigest(hex::encode(digest.finalize())))
}

/// Optional presentation evidence, never a request or a route admission token.
/// Only a production-qualified receipt can construct it. Durable responses must
/// reacquire current authority rather than deserialize this sidecar.
#[derive(Debug, Clone, Serialize)]
pub struct DvEffectiveProcessingReport {
    generation: String,
    hdr10_enhanced: bool,
    fel_contributed: bool,
    applied_operations: Vec<DvOperation>,
}
impl DvEffectiveProcessingReport {
    /// Bounded advisory transfer from the current control owner. The daemon
    /// calls this on an owner response, never on a request or durable replay.
    pub fn from_owner_wire(
        bytes: &[u8],
        generation: &str,
        delivered: Option<&str>,
    ) -> Option<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            generation: String,
            hdr10_enhanced: bool,
            fel_contributed: bool,
            applied_operations: Vec<DvOperation>,
        }
        if bytes.len() > 4096
            || delivered != Some("hdr10")
            || dv_playback_generation_digest(generation).is_err()
        {
            return None;
        }
        let wire: Wire = serde_json::from_slice(bytes).ok()?;
        let operations: BTreeSet<_> = wire.applied_operations.iter().copied().collect();
        if wire.generation != generation
            || !wire.hdr10_enhanced
            || wire.applied_operations.len() > 16
            || operations.len() != wire.applied_operations.len()
            || !operations.contains(&DvOperation::RpuColorConversion)
            || !operations.contains(&DvOperation::TargetMapping)
            || wire.fel_contributed && !operations.contains(&DvOperation::LinearNlqResidual)
        {
            return None;
        }
        Some(Self {
            generation: wire.generation,
            hdr10_enhanced: true,
            fel_contributed: wire.fel_contributed,
            applied_operations: wire.applied_operations,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvDestination {
    Hdr10,
    Profile81,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvPreferences {
    pub hdr_processing: bool,
    pub fel_reencode: bool,
    pub conversion_permitted: bool,
    pub planning_input_generation: i64,
}

impl Default for DvPreferences {
    fn default() -> Self {
        Self {
            hdr_processing: false,
            fel_reencode: false,
            conversion_permitted: true,
            planning_input_generation: 0,
        }
    }
}

impl DvPreferences {
    pub fn validate(self) -> Result<Self, DvContractError> {
        if self.planning_input_generation < 0 {
            return Err(DvContractError::Invalid("planning revision"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvElKind {
    Unknown,
    None,
    Mel,
    Fel,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvRpuState {
    Unknown,
    Absent,
    PresentUnvalidated,
    ValidatedFresh,
    ReuseUnsupported,
    Malformed,
}

/// Reduced signed PTS. Equality is exact; ordering compares rational values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvTimestamp {
    numerator: i64,
    denominator: u32,
}
impl DvTimestamp {
    pub fn new(numerator: i64, denominator: u32) -> Result<Self, DvContractError> {
        if denominator == 0 {
            return Err(DvContractError::Invalid("timestamp denominator"));
        }
        let mut a = numerator.unsigned_abs();
        let mut b = u64::from(denominator);
        while b != 0 {
            let next = a % b;
            a = b;
            b = next;
        }
        let divisor = a.max(1);
        Ok(Self {
            numerator: (i128::from(numerator) / i128::from(divisor)) as i64,
            denominator: (u64::from(denominator) / divisor) as u32,
        })
    }
}
impl Ord for DvTimestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (i128::from(self.numerator) * i128::from(other.denominator))
            .cmp(&(i128::from(other.numerator) * i128::from(self.denominator)))
    }
}
impl PartialOrd for DvTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvDurationProvenance {
    StoredContainer,
    CodedPacketObserved,
    OutputFrameGrid,
    DeclaredFixtureInterval,
    DecoderInferred,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvDuration {
    value: DvTimestamp,
    provenance: DvDurationProvenance,
}
impl DvDuration {
    pub fn new(
        numerator: i64,
        denominator: u32,
        provenance: DvDurationProvenance,
    ) -> Result<Self, DvContractError> {
        if numerator <= 0 {
            return Err(DvContractError::Invalid("positive duration"));
        }
        Ok(Self {
            value: DvTimestamp::new(numerator, denominator)?,
            provenance,
        })
    }
    pub fn end_from(self, start: DvTimestamp) -> Result<DvTimestamp, DvContractError> {
        let mut numerator = i128::from(start.numerator) * i128::from(self.value.denominator)
            + i128::from(self.value.numerator) * i128::from(start.denominator);
        let mut denominator = u64::from(start.denominator) * u64::from(self.value.denominator);
        let mut a = numerator.unsigned_abs();
        let mut b = u128::from(denominator);
        while b != 0 {
            let next = a % b;
            a = b;
            b = next;
        }
        let divisor = a.max(1);
        numerator /= divisor as i128;
        denominator /= divisor as u64;
        DvTimestamp::new(
            i64::try_from(numerator).map_err(|_| DvContractError::Invalid("interval overflow"))?,
            u32::try_from(denominator)
                .map_err(|_| DvContractError::Invalid("interval time base"))?,
        )
    }
    fn initial_timing_supported(self) -> bool {
        matches!(
            self.provenance,
            DvDurationProvenance::StoredContainer
                | DvDurationProvenance::CodedPacketObserved
                | DvDurationProvenance::DeclaredFixtureInterval
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DvFrameKey {
    pub absolute_video_index: u32,
    pub continuity_epoch: u64,
    pub pts: DvTimestamp,
    /// Diagnostic only: never resolves equal-PTS ambiguity in the initial subset.
    pub display_ordinal: u64,
}
/// One source picture and its explicitly assigned output presentation. Source
/// keys remain unchanged for BL/EL/RPU association; only encoder timing uses
/// the output clock. This is a mapping fact, never route authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvFrameGridMapping {
    pub source: DvFrameKey,
    pub output_pts: DvTimestamp,
    pub output_duration: DvDuration,
}

/// Bind a complete coded-source trace to consecutive output frames. Container
/// timestamp quantization may move each picture by at most one stored tick;
/// a tick must be smaller than half an output frame so equal/ambiguous pictures
/// cannot be rescued by rounding. Missing pictures never acquire synthetic keys.
pub fn dv_map_source_to_output_grid(
    source: &[DvFrameKey],
    source_tick: (u32, u32),
    grid: super::VodFrameGrid,
    first_output_frame: u64,
    expected_frames: usize,
) -> Result<Vec<DvFrameGridMapping>, DvContractError> {
    if source.is_empty()
        || source.len() != expected_frames
        || expected_frames > DV_MAX_CONTROL_FRAMES
        || source_tick.0 == 0
        || source_tick.1 == 0
        || super::VodFrameGrid::new(grid.numerator, grid.denominator) != Some(grid)
        || 2 * u128::from(source_tick.0) * u128::from(grid.numerator)
            >= u128::from(source_tick.1) * u128::from(grid.denominator)
    {
        return Err(DvContractError::Invalid("source/output clock mapping"));
    }
    let mut mapping = Vec::with_capacity(source.len());
    let binding = (source[0].absolute_video_index, source[0].continuity_epoch);
    for (index, key) in source.iter().enumerate() {
        if (key.absolute_video_index, key.continuity_epoch) != binding
            || index > 0 && source[index - 1].pts >= key.pts
        {
            return Err(DvContractError::DuplicateFrame);
        }
        let output_frame = first_output_frame
            .checked_add(index as u64)
            .ok_or(DvContractError::Capacity)?;
        let output_ticks = output_frame
            .checked_mul(u64::from(grid.denominator))
            .and_then(|ticks| i64::try_from(ticks).ok())
            .ok_or(DvContractError::Capacity)?;
        let output_pts = DvTimestamp::new(output_ticks, grid.numerator)?;
        let error = (i128::from(key.pts.numerator) * i128::from(output_pts.denominator)
            - i128::from(output_pts.numerator) * i128::from(key.pts.denominator))
        .unsigned_abs();
        if error * u128::from(source_tick.1)
            > u128::from(source_tick.0)
                * u128::from(key.pts.denominator)
                * u128::from(output_pts.denominator)
        {
            return Err(DvContractError::Invalid(
                "source cadence differs from output grid",
            ));
        }
        mapping.push(DvFrameGridMapping {
            source: key.clone(),
            output_pts,
            output_duration: DvDuration::new(
                i64::from(grid.denominator),
                grid.numerator,
                DvDurationProvenance::OutputFrameGrid,
            )?,
        });
    }
    Ok(mapping)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvSourceCoverage {
    epoch: u64,
    sampled_frames: BTreeSet<DvFrameKey>,
}
impl DvSourceCoverage {
    pub fn sampled(epoch: u64, frames: Vec<DvFrameKey>) -> Result<Self, DvContractError> {
        if frames.is_empty() || frames.len() > DV_MAX_CONTROL_FRAMES {
            return Err(DvContractError::Invalid("sample coverage"));
        }
        let mut pts = BTreeSet::new();
        for key in &frames {
            if key.continuity_epoch != epoch {
                return Err(DvContractError::Invalid("sample epoch"));
            }
            if !pts.insert((key.absolute_video_index, key.pts)) {
                return Err(DvContractError::DuplicateFrame);
            }
        }
        Ok(Self {
            epoch,
            sampled_frames: frames.into_iter().collect(),
        })
    }
    pub fn covers(&self, frame: &DvFrameKey) -> bool {
        self.sampled_frames.contains(frame)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvPlaneRepresentation {
    P7BaseYuv420P10Limited,
    P7ElYuv420P10Residual,
    DoviBaseP10Limited,
    DoviBaseP10Full,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvPlaneShape {
    width: u32,
    height: u32,
    representation: DvPlaneRepresentation,
}
impl DvPlaneShape {
    pub fn new(
        width: u32,
        height: u32,
        representation: DvPlaneRepresentation,
    ) -> Result<Self, DvContractError> {
        if width == 0
            || height == 0
            || width > 16_384
            || height > 16_384
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            return Err(DvContractError::Invalid("native 420 geometry"));
        }
        Ok(Self {
            width,
            height,
            representation,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DvSourceFacts {
    catalog: DolbyVisionFacts,
    absolute_video_index: u32,
    observed_source: DecodeSourceIdentity,
    cache_identity: DecodeCacheIdentity,
    binding: PlanSourceBinding,
    parser_identity: DvDigest,
    rpu_state: DvRpuState,
    el_kind: DvElKind,
    coverage: Option<DvSourceCoverage>,
    bl_shape: Option<DvPlaneShape>,
    el_shape: Option<DvPlaneShape>,
}
impl DvSourceFacts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: DolbyVisionFacts,
        absolute_video_index: u32,
        observed_source: DecodeSourceIdentity,
        cache_identity: DecodeCacheIdentity,
        binding: PlanSourceBinding,
        parser_identity: DvDigest,
        rpu_state: DvRpuState,
        el_kind: DvElKind,
        coverage: Option<DvSourceCoverage>,
        bl_shape: Option<DvPlaneShape>,
        el_shape: Option<DvPlaneShape>,
    ) -> Result<Self, DvContractError> {
        if (rpu_state == DvRpuState::ValidatedFresh) != coverage.is_some() {
            return Err(DvContractError::Invalid("fresh RPU coverage"));
        }
        if coverage.as_ref().is_some_and(|c| {
            c.sampled_frames
                .iter()
                .any(|f| f.absolute_video_index != absolute_video_index)
        }) {
            return Err(DvContractError::Invalid("sample stream"));
        }
        Ok(Self {
            catalog,
            absolute_video_index,
            observed_source,
            cache_identity,
            binding,
            parser_identity,
            rpu_state,
            el_kind,
            coverage,
            bl_shape,
            el_shape,
        })
    }
    pub fn sampled_coverage(&self) -> Option<&DvSourceCoverage> {
        self.coverage.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DvOperation {
    RepresentationNormalization,
    BaseNormalization,
    PolynomialReshape,
    MmrReshape,
    LinearNlqResidual,
    RpuColorConversion,
    TargetMapping,
    CreativeTrimApplication,
    MetadataAdaptation,
    Bt2020NclConversion,
    Main10Encoding,
    P81ContainerSignaling,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvIntermediateDomain {
    DecodedBl,
    NormalizedBaseVdrComponents,
    ReshapedBaseVdrComponents,
    ReconstructedVdrComponents,
    BaseBt2020PqRgb,
    ReconstructedBt2020PqRgb,
    TargetMappedBt2020PqRgb,
    LimitedBt2020NclYuv420P10,
    EncodedMain10,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum DvStrategy {
    BaseRpuToHdr10,
    FelRpuToHdr10,
    ReconstructedProfile81,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvProcessingMode {
    Fel,
    BaseRpu,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvMetadataSubset {
    SyntheticP7IdentityLinearNlqV1,
    SyntheticP7AffineLumaLinearNlqV1,
    SyntheticP81IdentityV1,
    RuntimeP7MasterDomainLinearDzV1,
    RuntimeBaseRpuMasterDomainV1,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvEvidenceScope {
    SyntheticControl,
    ProductionRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvGraphEdge {
    pub operation: DvOperation,
    pub input: DvIntermediateDomain,
    pub output: DvIntermediateDomain,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvGraph {
    strategy: DvStrategy,
    pixels: Vec<DvGraphEdge>,
    adapt_metadata_to_reconstructed_base: bool,
}
impl DvGraph {
    fn expected(strategy: DvStrategy) -> Vec<DvGraphEdge> {
        use DvIntermediateDomain as D;
        use DvOperation as O;
        let edge = |operation, input, output| DvGraphEdge {
            operation,
            input,
            output,
        };
        let mut edges = vec![
            edge(
                O::RepresentationNormalization,
                D::DecodedBl,
                D::NormalizedBaseVdrComponents,
            ),
            edge(
                O::PolynomialReshape,
                D::NormalizedBaseVdrComponents,
                D::ReshapedBaseVdrComponents,
            ),
        ];
        let rgb = if strategy == DvStrategy::BaseRpuToHdr10 {
            edges.push(edge(
                O::RpuColorConversion,
                D::ReshapedBaseVdrComponents,
                D::BaseBt2020PqRgb,
            ));
            D::BaseBt2020PqRgb
        } else {
            edges.push(edge(
                O::LinearNlqResidual,
                D::ReshapedBaseVdrComponents,
                D::ReconstructedVdrComponents,
            ));
            edges.push(edge(
                O::RpuColorConversion,
                D::ReconstructedVdrComponents,
                D::ReconstructedBt2020PqRgb,
            ));
            D::ReconstructedBt2020PqRgb
        };
        edges.push(edge(
            O::Bt2020NclConversion,
            rgb,
            D::LimitedBt2020NclYuv420P10,
        ));
        edges.push(edge(
            O::Main10Encoding,
            D::LimitedBt2020NclYuv420P10,
            D::EncodedMain10,
        ));
        edges
    }
    pub fn new(
        strategy: DvStrategy,
        pixels: Vec<DvGraphEdge>,
        adapt_metadata_to_reconstructed_base: bool,
    ) -> Result<Self, DvContractError> {
        if pixels != Self::expected(strategy)
            || adapt_metadata_to_reconstructed_base
                != (strategy == DvStrategy::ReconstructedProfile81)
        {
            return Err(DvContractError::Invalid("ordered synthetic graph"));
        }
        Ok(Self {
            strategy,
            pixels,
            adapt_metadata_to_reconstructed_base,
        })
    }
    pub fn required_operations(&self) -> Vec<DvOperation> {
        let mut result: Vec<_> = self.pixels.iter().map(|e| e.operation).collect();
        if self.adapt_metadata_to_reconstructed_base {
            result.push(DvOperation::MetadataAdaptation);
            if self
                .pixels
                .iter()
                .any(|edge| edge.operation == DvOperation::TargetMapping)
            {
                result.push(DvOperation::P81ContainerSignaling);
            }
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvBackendIdentity {
    executable: DvDigest,
    libraries: DvDigest,
    parser: DvDigest,
    graph_patch: DvDigest,
    abi_revision: u32,
    environment: DvDigest,
}
impl DvBackendIdentity {
    pub fn parser_identity(&self) -> &DvDigest {
        &self.parser
    }
    /// Bind the renderer's actual Vulkan device/driver observation to the
    /// captured helper/library identity. The daemon compares this identity
    /// again for each completed window before publication.
    pub fn with_runtime_device(&self, device: &DvDigest) -> Self {
        let mut bound = self.clone();
        let body = ("plurx.dv.runtime-device.v1", &self.environment, device);
        bound.environment = DvDigest(hex::encode(Sha256::digest(
            serde_json::to_vec(&body).expect("validated backend identity serializes"),
        )));
        bound
    }
    pub fn new(
        executable: DvDigest,
        libraries: DvDigest,
        parser: DvDigest,
        graph_patch: DvDigest,
        abi_revision: u32,
        environment: DvDigest,
    ) -> Result<Self, DvContractError> {
        if abi_revision == 0 {
            return Err(DvContractError::Invalid("ABI revision"));
        }
        Ok(Self {
            executable,
            libraries,
            parser,
            graph_patch,
            abi_revision,
            environment,
        })
    }
}

/// Observation validity is not a byte-changing output parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvProofValidity {
    observed_at: i64,
    expires_at: Option<i64>,
    snapshot_revision: u64,
    invalidated: bool,
}
impl DvProofValidity {
    pub fn new(
        observed_at: i64,
        expires_at: Option<i64>,
        snapshot_revision: u64,
        invalidated: bool,
    ) -> Result<Self, DvContractError> {
        if observed_at < 0 || expires_at.is_some_and(|end| end <= observed_at) {
            return Err(DvContractError::Invalid("proof lifetime"));
        }
        Ok(Self {
            observed_at,
            expires_at,
            snapshot_revision,
            invalidated,
        })
    }
    pub fn valid_at(&self, now: i64) -> bool {
        !self.invalidated && now >= self.observed_at && self.expires_at.is_none_or(|end| now < end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvTargetPolicy {
    revision: u32,
    peak_millinit: u32,
    black_millinit: u32,
    parameters: DvDigest,
}
impl DvTargetPolicy {
    /// This validates numbers, not a display measurement or production target.
    pub fn new(
        revision: u32,
        peak_millinit: u32,
        black_millinit: u32,
        parameters: DvDigest,
    ) -> Result<Self, DvContractError> {
        if revision == 0 || peak_millinit > 10_000_000 || black_millinit >= peak_millinit {
            return Err(DvContractError::Invalid("target luminance"));
        }
        Ok(Self {
            revision,
            peak_millinit,
            black_millinit,
            parameters,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DvCapabilityEvidence {
    scope: DvEvidenceScope,
    completed_window_checked: bool,
    identity: DvBackendIdentity,
    schema: u32,
    subset: DvMetadataSubset,
    decoder: DecodeBackend,
    #[serde(serialize_with = "serialize_encoder")]
    encoder: Encoder,
    raster: (u32, u32),
    bl_shape: DvPlaneShape,
    el_shape: Option<DvPlaneShape>,
    max_frames: usize,
    target: DvTargetPolicy,
    evidence_digest: DvDigest,
    validity: DvProofValidity,
}
impl DvCapabilityEvidence {
    /// No constructor can grant production authority in M1.
    pub fn validate_claimed_scope(scope: DvEvidenceScope) -> Result<(), DvContractError> {
        if scope == DvEvidenceScope::ProductionRoute {
            return Err(DvContractError::ProductionQualificationUnavailable);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvExistingRoute {
    NativeDvCopy,
    CompatibleBaseHdr10,
    P7BaseConversion,
    RequiredP5Render,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvFallbackReason {
    PreferenceDisabled,
    NativeCompatible,
    ConversionForbidden,
    ExistingDeliveryConstraint,
    UnsupportedProfileOrBase,
    UnknownElKind,
    RpuUnavailable,
    UnsupportedMetadata,
    MissingOrMisalignedEl,
    StaleBackendProof,
    UnqualifiedSourceBinding,
    UnsupportedCodecOrGeometry,
    UnqualifiedTargetOrResources,
    InvalidOutputContract,
    BackendFailure,
    Cancelled,
    UnsupportedReceiptSchema,
    ProductionUnqualified,
}
#[derive(Debug, Clone, Serialize)]
pub struct DvProcessingPlan {
    source: DvSourceFacts,
    preferences: DvPreferences,
    destination: DvDestination,
    graph: DvGraph,
    capability: DvCapabilityEvidence,
}
impl DvProcessingPlan {
    pub fn destination(&self) -> DvDestination {
        self.destination
    }
    pub fn profile81_level(&self) -> Option<u8> {
        (self.destination == DvDestination::Profile81 && self.capability.completed_window_checked)
            .then(|| {
                self.source
                    .catalog
                    .level
                    .and_then(|level| u8::try_from(level).ok())
                    .filter(|level| (1..=9).contains(level))
                    .unwrap_or(1)
            })
    }
    /// Deterministic identity of the selected source/backend/pixel policy.
    pub fn semantic_digest(&self) -> DvDigest {
        // Only validated, integer/enumerated values: no debug or float spelling.
        let body = (
            &self.source.cache_identity,
            self.source.absolute_video_index,
            self.destination,
            &self.graph,
            self.capability.schema,
            &self.capability.identity,
            self.capability.subset,
            self.capability.decoder,
            encoder_token(self.capability.encoder),
            self.capability.raster,
            self.capability.bl_shape,
            self.capability.el_shape,
            self.capability.max_frames,
            &self.capability.target,
        );
        DvDigest(hex::encode(Sha256::digest(
            serde_json::to_vec(&body).expect("validated DV semantic fields serialize"),
        )))
    }
}
#[derive(Debug, Clone, Serialize)]
pub enum DvSelection {
    KeepExisting(DvFallbackReason),
    Selected(Box<DvProcessingPlan>),
}

/// The selected-worker identity is captured by the caller, never controller tools.
pub struct DvResolutionInput {
    pub existing_route: DvExistingRoute,
    pub enhanced_encode_allowed: bool,
    pub destination: DvDestination,
    pub preferences: DvPreferences,
    pub source: DvSourceFacts,
    pub selected_backend: DvBackendIdentity,
    pub capability: Option<DvCapabilityEvidence>,
    pub now: i64,
}

/// Only the checked completed-window constructor can grant a route. A scope
/// label or a helper-controlled digest cannot add an entry.
#[derive(Debug, Default)]
pub struct DvProductionRegistry;
impl DvProductionRegistry {
    pub fn permits(&self, plan: &DvProcessingPlan) -> bool {
        plan.capability.scope == DvEvidenceScope::ProductionRoute
            && plan.capability.completed_window_checked
    }
}

/// Independently inspect the completed private mux output. The source RPU
/// order is the explicit source-to-output mapping order, never coded order.
/// This does not accept an encoder log as evidence of sample configuration.
#[allow(clippy::too_many_arguments)]
pub fn dv_validate_encoded_window(
    bytes: &[u8],
    raster: (u32, u32),
    timescale: u32,
    start: u64,
    frame_ticks: u32,
    source_rpus: &[Vec<u8>],
    destination: DvDestination,
) -> Result<Vec<DvDigest>, DvContractError> {
    use crate::fmp4::{self, Unit};
    use dolby_vision::rpu::{dovi_rpu::DoviRpu, ConversionMode};
    let invalid = || DvContractError::Invalid("completed encoded DV window");
    if bytes.is_empty()
        || bytes.len() > 64 * 1024 * 1024
        || source_rpus.is_empty()
        || source_rpus.len() > 64
        || frame_ticks == 0
    {
        return Err(invalid());
    }
    let mut reader = fmp4::FragmentReader::new();
    reader.push(bytes);
    let Some(Unit::Init(init)) = reader.next_unit().map_err(|_| invalid())? else {
        return Err(invalid());
    };
    let facts = fmp4::hevc_hdr10_sample_entry_facts(&init).map_err(|_| invalid())?;
    if (u32::from(facts.width), u32::from(facts.height)) != raster {
        return Err(invalid());
    }
    let config = fmp4::dolby_vision_record(&init).map_err(|_| invalid())?;
    match destination {
        DvDestination::Hdr10 if config.is_some() => return Err(invalid()),
        DvDestination::Profile81
            if !config.is_some_and(|record| {
                record.profile == 8
                    && record.bl_signal_compatibility_id == 1
                    && record.rpu_present
                    && record.bl_present
                    && !record.el_present
            }) =>
        {
            return Err(invalid())
        }
        _ => {}
    }
    let video = init.video().ok_or_else(invalid)?;
    let mut ordinal = 0_usize;
    let mut position = start;
    let mut payloads = Vec::new();
    while let Some(unit) = reader.next_unit().map_err(|_| invalid())? {
        let Unit::Fragment(fragment) = unit else {
            if matches!(unit, Unit::Trailer) {
                continue;
            }
            return Err(invalid());
        };
        let track = fragment.track(video.id).ok_or_else(invalid)?;
        let duration = u64::from(frame_ticks)
            .checked_mul(track.sample_count() as u64)
            .ok_or_else(invalid)?;
        fmp4::validate_encoded_grid(&fragment, &init, timescale, position, duration, frame_ticks)
            .map_err(|_| invalid())?;
        for run in &track.runs {
            let mut offset = run.data_offset;
            for sample in &run.samples {
                if sample.cto != 0 || ordinal >= source_rpus.len() {
                    return Err(invalid());
                }
                let end = offset
                    .checked_add(sample.size as usize)
                    .ok_or_else(invalid)?;
                if offset < fragment.mdat_payload.start || end > fragment.mdat_payload.end {
                    return Err(invalid());
                }
                let payload = fragment.bytes.get(offset..end).ok_or_else(invalid)?;
                let mut nal_offset = 0_usize;
                let mut rpu = None;
                let mut slices = 0_u32;
                while nal_offset < payload.len() {
                    let length_end = nal_offset.checked_add(4).ok_or_else(invalid)?;
                    let length = u32::from_be_bytes(
                        payload
                            .get(nal_offset..length_end)
                            .ok_or_else(invalid)?
                            .try_into()
                            .map_err(|_| invalid())?,
                    ) as usize;
                    let nal_end = length_end.checked_add(length).ok_or_else(invalid)?;
                    let nal = payload.get(length_end..nal_end).ok_or_else(invalid)?;
                    if nal.len() < 2 || nal[0] & 0x80 != 0 || nal[1] & 7 == 0 {
                        return Err(invalid());
                    }
                    let kind = (nal[0] >> 1) & 63;
                    let layer = ((nal[0] & 1) << 5) | (nal[1] >> 3);
                    if layer != 0 || kind == 63 {
                        return Err(invalid());
                    }
                    if kind <= 31 {
                        if nal.len() < 3 {
                            return Err(invalid());
                        }
                        if nal[2] & 0x80 != 0 {
                            slices += 1;
                        }
                    }
                    if kind == 62 && rpu.replace(nal).is_some() {
                        return Err(invalid());
                    }
                    nal_offset = nal_end;
                }
                if slices != 1 {
                    return Err(invalid());
                }
                match destination {
                    DvDestination::Hdr10 if rpu.is_some() => return Err(invalid()),
                    DvDestination::Profile81 => {
                        let mut expected = DoviRpu::parse_unspec62_nalu(&source_rpus[ordinal])
                            .map_err(|_| invalid())?;
                        expected
                            .convert_with_mode(ConversionMode::To81)
                            .map_err(|_| invalid())?;
                        expected.remove_mapping();
                        let expected =
                            expected.write_hevc_unspec62_nalu().map_err(|_| invalid())?;
                        if rpu != Some(expected.as_slice()) {
                            return Err(invalid());
                        }
                    }
                    _ => {}
                }
                payloads.push(DvDigest(hex::encode(Sha256::digest(payload))));
                ordinal += 1;
                offset = end;
            }
        }
        position = position.checked_add(duration).ok_or_else(invalid)?;
    }
    if ordinal != source_rpus.len() || reader.buffered() != 0 {
        return Err(invalid());
    }
    Ok(payloads)
}

/// Author an already encoded, independently checked HDR10 window. Base and
/// audio samples are reused verbatim; only one associated RPU is appended to
/// each video sample and the sample entry receives the matching DV record.
/// The level must come from the held source with unchanged cadence and no
/// increase in raster. Calling this does not qualify the source graph.
#[allow(clippy::too_many_arguments)]
pub fn dv_author_profile81_window(
    bytes: &[u8],
    raster: (u32, u32),
    timescale: u32,
    start: u64,
    frame_ticks: u32,
    source_rpus: &[Vec<u8>],
    source_level: u8,
) -> Result<Vec<u8>, DvContractError> {
    use crate::fmp4::{self, Unit};
    use dolby_vision::rpu::{dovi_rpu::DoviRpu, ConversionMode};
    let invalid = || DvContractError::Invalid("packet-aware P8.1 authoring");
    let base = dv_validate_encoded_window(
        bytes,
        raster,
        timescale,
        start,
        frame_ticks,
        source_rpus,
        DvDestination::Hdr10,
    )?;
    if !(1..=9).contains(&source_level) {
        return Err(invalid());
    }
    let mut adapted = Vec::with_capacity(source_rpus.len());
    for raw in source_rpus {
        // Authoring retains valid opaque display metadata. The completed
        // pixel-graph factory separately enforces its narrower render subset.
        DvParsedMetadata::from_raw_rpu(raw)?;
        let mut rpu = DoviRpu::parse_unspec62_nalu(raw).map_err(|_| invalid())?;
        rpu.convert_with_mode(ConversionMode::To81)
            .map_err(|_| invalid())?;
        rpu.remove_mapping();
        let nal = rpu.write_hevc_unspec62_nalu().map_err(|_| invalid())?;
        if nal.len() > 4098 {
            return Err(invalid());
        }
        adapted.push(nal);
    }
    let mut reader = fmp4::FragmentReader::new();
    reader.push(bytes);
    let Some(Unit::Init(mut init)) = reader.next_unit().map_err(|_| invalid())? else {
        return Err(invalid());
    };
    let video = init.video().ok_or_else(invalid)?.id;
    let record = fmp4::DolbyVisionRecord::new(8, source_level, false, true, true, 1)
        .map_err(|_| invalid())?;
    fmp4::set_dolby_vision_record(&mut init, &record).map_err(|_| invalid())?;
    fmp4::declare_hevc_parameter_sets_in_band(&mut init).map_err(|_| invalid())?;
    let mut output = init.bytes.clone();
    let mut index = 0_usize;
    while let Some(unit) = reader.next_unit().map_err(|_| invalid())? {
        let Unit::Fragment(mut fragment) = unit else {
            continue;
        };
        fmp4::rewrite_video_samples(&mut fragment, &init.tracks, video, |sample| {
            let expected = base
                .get(index)
                .ok_or_else(|| fmp4::Fmp4Error::Unsupported("extra encoded picture".into()))?;
            if hex::encode(Sha256::digest(sample)) != expected.0 {
                return Err(fmp4::Fmp4Error::Unsupported(
                    "encoded base changed before authoring".into(),
                ));
            }
            let nal = adapted
                .get(index)
                .ok_or_else(|| fmp4::Fmp4Error::Unsupported("missing associated RPU".into()))?;
            let mut packet = Vec::with_capacity(sample.len() + 4 + nal.len());
            packet.extend_from_slice(sample);
            packet.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            packet.extend_from_slice(nal);
            index += 1;
            Ok(packet)
        })
        .map_err(|_| invalid())?;
        output.extend_from_slice(&fragment.bytes);
    }
    if index != source_rpus.len() || !reader.saw_trailer() || reader.buffered() != 0 {
        return Err(invalid());
    }
    // RPU insertion changes moof offsets. Do not retain a stale random-access
    // index: emit empty entries for the actual tracks and a correct mfro.
    output.extend_from_slice(&completed_window_trailer(&init));
    if output.len() > 64 * 1024 * 1024 {
        return Err(invalid());
    }
    dv_validate_encoded_window(
        &output,
        raster,
        timescale,
        start,
        frame_ticks,
        source_rpus,
        DvDestination::Profile81,
    )?;
    Ok(output)
}

fn completed_window_trailer(init: &crate::fmp4::Init) -> Vec<u8> {
    let size = 24 + 24 * init.tracks.len() as u32;
    let mut trailer = Vec::with_capacity(size as usize);
    trailer.extend_from_slice(&size.to_be_bytes());
    trailer.extend_from_slice(b"mfra");
    for track in &init.tracks {
        trailer.extend_from_slice(&24_u32.to_be_bytes());
        trailer.extend_from_slice(b"tfra");
        trailer.extend_from_slice(&0x01000000_u32.to_be_bytes());
        trailer.extend_from_slice(&track.id.to_be_bytes());
        trailer.extend_from_slice(&0_u32.to_be_bytes()); // one-byte traf/trun/sample numbers
        trailer.extend_from_slice(&0_u32.to_be_bytes()); // no stale offset entries
    }
    trailer.extend_from_slice(&16_u32.to_be_bytes());
    trailer.extend_from_slice(b"mfro");
    trailer.extend_from_slice(&0_u32.to_be_bytes());
    trailer.extend_from_slice(&size.to_be_bytes());
    trailer
}

/// Inputs are collected by the daemon from its held source and completed
/// private processes. This type is intentionally neither deserializable nor
/// a helper receipt. Every byte-changing output fact is rechecked below.
pub struct DvCompletedWindowInput<'a> {
    pub source: DvSourceFacts,
    pub preferences: DvPreferences,
    pub backend: DvBackendIdentity,
    pub destination: DvDestination,
    pub encoder: Encoder,
    pub mode: DvProcessingMode,
    pub source_keys: &'a [DvFrameKey],
    pub source_rpus: &'a [Vec<u8>],
    pub source_tick: (u32, u32),
    pub grid: super::VodFrameGrid,
    pub first_global_frame: u64,
    pub encoded: &'a [u8],
    pub raster: (u32, u32),
    pub source_trace: DvDigest,
    pub observed_at: i64,
}

pub fn dv_plan_completed_window(
    input: DvCompletedWindowInput<'_>,
) -> Result<DvProcessingPlan, DvContractError> {
    let invalid = || DvContractError::Invalid("daemon completed DV window");
    let preferences = input.preferences.validate()?;
    if !matches!(input.encoder, Encoder::Software | Encoder::Nvenc) {
        return Err(invalid());
    }
    let full_fel = input.mode == DvProcessingMode::Fel;
    if !full_fel && input.destination != DvDestination::Hdr10 {
        return Err(invalid());
    }
    if dv_processing_preflight(
        DvExistingRoute::Other,
        true,
        input.destination,
        preferences,
        &input.source.catalog,
    )?
    .is_some()
    {
        return Err(invalid());
    }
    if input.destination == DvDestination::Profile81 && !preferences.conversion_permitted
        || !(match input.destination {
            DvDestination::Hdr10 => preferences.hdr_processing,
            DvDestination::Profile81 => preferences.fel_reencode,
        })
        || full_fel && input.source.catalog.profile != Some(7)
        || !full_fel && !matches!(input.source.catalog.profile, Some(5 | 7 | 8))
        || input.source.binding != PlanSourceBinding::DescriptorBound
        || input.source.rpu_state != DvRpuState::ValidatedFresh
        || full_fel && input.source.el_kind != DvElKind::Fel
        || input.source.parser_identity != input.backend.parser
        || input.source_keys.len() != input.source_rpus.len()
        || input
            .source
            .coverage
            .as_ref()
            .is_none_or(|coverage| input.source_keys.iter().any(|key| !coverage.covers(key)))
    {
        return Err(invalid());
    }
    let bl_shape = input.source.bl_shape.ok_or_else(invalid)?;
    let el_shape = input.source.el_shape;
    let expected_bl = if full_fel {
        DvPlaneRepresentation::P7BaseYuv420P10Limited
    } else if input.source.catalog.profile == Some(5) {
        DvPlaneRepresentation::DoviBaseP10Full
    } else {
        DvPlaneRepresentation::DoviBaseP10Limited
    };
    if bl_shape.representation != expected_bl
        || bl_shape.width > 3840
        || bl_shape.height > 2160
        || input.raster.0 == 0
        || input.raster.1 == 0
        || input.raster.0 > bl_shape.width
        || input.raster.1 > bl_shape.height
        || full_fel
            && el_shape.is_none_or(|el| {
                el.representation != DvPlaneRepresentation::P7ElYuv420P10Residual
                    || !((bl_shape.width == el.width && bl_shape.height == el.height)
                        || (el.width.checked_mul(2) == Some(bl_shape.width)
                            && el.height.checked_mul(2) == Some(bl_shape.height)))
            })
        || !full_fel && el_shape.is_some()
    {
        return Err(invalid());
    }
    dv_map_source_to_output_grid(
        input.source_keys,
        input.source_tick,
        input.grid,
        input.first_global_frame,
        input.source_rpus.len(),
    )?;
    for raw in input.source_rpus {
        if full_fel {
            if !DvParsedMetadata::from_raw_rpu(raw)?.runtime_supported() {
                return Err(invalid());
            }
        } else {
            let metadata = DvBaseMetadata::from_raw_rpu(raw, (bl_shape.width, bl_shape.height))?;
            if Some(i64::from(metadata.profile)) != input.source.catalog.profile
                || metadata.source_el_kind != input.source.el_kind
            {
                return Err(invalid());
            }
        }
    }
    dv_validate_encoded_window(
        input.encoded,
        input.raster,
        input.grid.numerator,
        super::vod::vod_reconstructed_video_origin(input.grid, input.first_global_frame)
            .map_err(|_| invalid())?,
        input.grid.denominator,
        input.source_rpus,
        input.destination,
    )?;
    let strategy = match input.destination {
        DvDestination::Hdr10 if !full_fel => DvStrategy::BaseRpuToHdr10,
        DvDestination::Hdr10 => DvStrategy::FelRpuToHdr10,
        DvDestination::Profile81 => DvStrategy::ReconstructedProfile81,
    };
    let mut pixels = DvGraph::expected(strategy);
    if !full_fel {
        // The graph identifies the supported conditional renderer policy,
        // independent of which curve methods occur in this window. Actual
        // operations and segment counts belong to each fresh frame receipt.
        pixels.retain(|edge| edge.operation != DvOperation::PolynomialReshape);
        for (index, operation) in [DvOperation::PolynomialReshape, DvOperation::MmrReshape]
            .into_iter()
            .enumerate()
        {
            pixels.insert(
                index + 1,
                DvGraphEdge {
                    operation,
                    input: if index == 0 {
                        DvIntermediateDomain::NormalizedBaseVdrComponents
                    } else {
                        DvIntermediateDomain::ReshapedBaseVdrComponents
                    },
                    output: DvIntermediateDomain::ReshapedBaseVdrComponents,
                },
            );
        }
    }
    let index = pixels
        .iter()
        .position(|edge| edge.operation == DvOperation::Bt2020NclConversion)
        .ok_or_else(invalid)?;
    let rgb = pixels[index].input;
    pixels[index].input = DvIntermediateDomain::TargetMappedBt2020PqRgb;
    pixels.insert(
        index,
        DvGraphEdge {
            operation: DvOperation::TargetMapping,
            input: rgb,
            output: DvIntermediateDomain::TargetMappedBt2020PqRgb,
        },
    );
    let graph = DvGraph {
        strategy,
        pixels,
        adapt_metadata_to_reconstructed_base: input.destination == DvDestination::Profile81,
    };
    let target = DvTargetPolicy::new(
        1,
        10_000_000,
        5,
        DvDigest(hex::encode(Sha256::digest(b"bt2020-pq-master-clip-v1"))),
    )?;
    let capability = DvCapabilityEvidence {
        scope: DvEvidenceScope::ProductionRoute,
        completed_window_checked: true,
        identity: input.backend,
        schema: DV_CONTRACT_VERSION,
        subset: if full_fel {
            DvMetadataSubset::RuntimeP7MasterDomainLinearDzV1
        } else {
            DvMetadataSubset::RuntimeBaseRpuMasterDomainV1
        },
        decoder: DecodeBackend::Software,
        encoder: input.encoder,
        raster: input.raster,
        bl_shape,
        el_shape,
        max_frames: 64,
        target,
        evidence_digest: input.source_trace,
        validity: DvProofValidity::new(
            input.observed_at,
            None,
            preferences.planning_input_generation as u64,
            false,
        )?,
    };
    Ok(DvProcessingPlan {
        source: input.source,
        preferences,
        destination: input.destination,
        graph,
        capability,
    })
}

/// Decide whether an existing delivery needs source/backend qualification.
/// `None` requests that qualification; it never authorizes processing. The
/// shipping worker can evaluate saved choices without fabricating parsed RPU,
/// EL, decoder or backend facts merely to construct a resolution input.
pub fn dv_processing_preflight(
    existing_route: DvExistingRoute,
    enhanced_encode_allowed: bool,
    destination: DvDestination,
    preferences: DvPreferences,
    catalog: &DolbyVisionFacts,
) -> Result<Option<DvFallbackReason>, DvContractError> {
    use DvFallbackReason as R;
    let prefs = preferences.validate()?;
    if existing_route == DvExistingRoute::NativeDvCopy {
        return Ok(Some(R::NativeCompatible));
    }
    if !enhanced_encode_allowed {
        return Ok(Some(R::ExistingDeliveryConstraint));
    }
    match destination {
        DvDestination::Hdr10 if !prefs.hdr_processing => {
            return Ok(Some(R::PreferenceDisabled));
        }
        DvDestination::Profile81 if !prefs.conversion_permitted => {
            return Ok(Some(R::ConversionForbidden));
        }
        DvDestination::Profile81 if !prefs.fel_reencode => {
            return Ok(Some(R::PreferenceDisabled));
        }
        _ => {}
    }
    let supported = matches!(
        (destination, catalog.profile, catalog.bl_compat_id),
        (DvDestination::Hdr10, Some(5), Some(0))
            | (DvDestination::Hdr10, Some(8), Some(1))
            | (_, Some(7), Some(1 | 6))
    );
    if !supported
        || !catalog.level.is_some_and(|level| (1..=63).contains(&level))
        || catalog.rpu_present == Some(false)
        || destination == DvDestination::Profile81 && catalog.el_present == Some(false)
    {
        return Ok(Some(R::UnsupportedProfileOrBase));
    }
    Ok(None)
}

pub fn resolve_dv_processing(input: &DvResolutionInput) -> Result<DvSelection, DvContractError> {
    resolve_inner(input, false)
}
fn resolve_inner(
    input: &DvResolutionInput,
    synthetic_test: bool,
) -> Result<DvSelection, DvContractError> {
    use DvFallbackReason as R;
    let unchanged = |reason| Ok(DvSelection::KeepExisting(reason));
    let prefs = input.preferences.validate()?;
    if let Some(reason) = dv_processing_preflight(
        input.existing_route,
        input.enhanced_encode_allowed,
        input.destination,
        prefs,
        &input.source.catalog,
    )? {
        return unchanged(reason);
    }
    let source = &input.source;
    if source.el_kind == DvElKind::Unknown {
        return unchanged(R::UnknownElKind);
    }
    if source.el_kind != DvElKind::Fel {
        return unchanged(R::UnsupportedMetadata);
    }
    if source.rpu_state != DvRpuState::ValidatedFresh {
        return unchanged(R::RpuUnavailable);
    }
    if source.binding != PlanSourceBinding::DescriptorBound {
        return unchanged(R::UnqualifiedSourceBinding);
    }
    let Some(cap) = &input.capability else {
        return unchanged(R::ProductionUnqualified);
    };
    if cap.schema != DV_CONTRACT_VERSION {
        return unchanged(R::UnsupportedReceiptSchema);
    }
    if cap.identity != input.selected_backend
        || cap.identity.parser != source.parser_identity
        || !cap.validity.valid_at(input.now)
    {
        return unchanged(R::StaleBackendProof);
    }
    if cap.scope != DvEvidenceScope::SyntheticControl || !synthetic_test {
        return unchanged(R::ProductionUnqualified);
    }
    if cap.decoder != DecodeBackend::Software
        || cap.encoder != Encoder::Software
        || cap.raster != (64, 64)
        || cap.bl_shape != DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7BaseYuv420P10Limited)?
        || cap.el_shape
            != Some(DvPlaneShape::new(
                64,
                64,
                DvPlaneRepresentation::P7ElYuv420P10Residual,
            )?)
        || source.bl_shape != Some(cap.bl_shape)
        || source.el_shape != cap.el_shape
        || cap.max_frames == 0
        || cap.max_frames > DV_MAX_CONTROL_FRAMES
    {
        return unchanged(R::UnsupportedCodecOrGeometry);
    }
    if cap.target.peak_millinit != 10_000_000 || cap.target.black_millinit != 5 {
        return unchanged(R::UnqualifiedTargetOrResources);
    }
    if cap.subset == DvMetadataSubset::SyntheticP81IdentityV1 {
        return unchanged(R::UnsupportedMetadata);
    }
    let strategy = match input.destination {
        DvDestination::Profile81 if source.el_kind != DvElKind::Fel => {
            return unchanged(R::UnknownElKind)
        }
        DvDestination::Profile81 => DvStrategy::ReconstructedProfile81,
        DvDestination::Hdr10 if source.el_kind == DvElKind::Fel => DvStrategy::FelRpuToHdr10,
        DvDestination::Hdr10 => DvStrategy::BaseRpuToHdr10,
    };
    let graph = DvGraph::new(
        strategy,
        DvGraph::expected(strategy),
        strategy == DvStrategy::ReconstructedProfile81,
    )?;
    Ok(DvSelection::Selected(Box::new(DvProcessingPlan {
        source: source.clone(),
        preferences: prefs,
        destination: input.destination,
        graph,
        capability: cap.clone(),
    })))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DvCurveRepresentation {
    PolynomialAffine,
    Mmr,
    Unsupported,
}

/// Independent projection for the public base+RPU renderer. EL residuals and
/// display trims are deliberately absent from its applied operation list.
#[derive(Debug, Clone)]
pub struct DvBaseMetadata {
    pub profile: u8,
    pub source_el_kind: DvElKind,
    pub applied_operations: Vec<DvOperation>,
    pub polynomial_segments: u32,
    pub mmr_segments: u32,
    pub metadata_levels: BTreeSet<u8>,
}
impl DvBaseMetadata {
    pub fn from_raw_rpu(raw: &[u8], raster: (u32, u32)) -> Result<Self, DvContractError> {
        use dolby_vision::rpu::{
            dovi_rpu::DoviRpu, extension_metadata::blocks::ExtMetadataBlock,
            rpu_data_mapping::DoviMappingMethod, rpu_data_nlq::DoviELType,
        };
        let invalid = || DvContractError::Invalid("unsupported fresh base RPU");
        if !(3..=4098).contains(&raw.len()) || raw[..2] != [0x7c, 0x01] {
            return Err(invalid());
        }
        let rpu = DoviRpu::parse_unspec62_nalu(raw).map_err(|_| invalid())?;
        let profile = rpu.dovi_profile;
        let h = &rpu.header;
        let mapping = rpu.rpu_data_mapping.as_ref().ok_or_else(invalid)?;
        let dm = rpu.vdr_dm_data.as_ref().ok_or_else(invalid)?;
        if !matches!(profile, 5 | 7 | 8)
            || h.use_prev_vdr_rpu_flag
            || !h.vdr_dm_metadata_present_flag
            || h.rpu_type != 2
            || h.rpu_format != 18
            || h.vdr_rpu_profile != if profile == 5 { 0 } else { 1 }
            || h.vdr_rpu_level != 0
            || !h.vdr_seq_info_present_flag
            || h.coefficient_data_type != 0
            || h.coefficient_log2_denom != 23
            || h.bl_bit_depth_minus8 != 2
            || h.vdr_bit_depth_minus8 != 4
            || h.vdr_rpu_normalized_idc != 1
            || h.chroma_resampling_explicit_filter_flag
            || h.spatial_resampling_filter_flag
            || h.bl_video_full_range_flag != (profile == 5)
            || profile != 7 && !h.disable_residual_flag
            || mapping.num_x_partitions_minus1 != 0
            || mapping.num_y_partitions_minus1 != 0
            || mapping.mapping_color_space != 0
            || mapping.mapping_chroma_format_idc != 0
            || dm.signal_bit_depth != 12
            || dm.signal_color_space != if profile == 5 { 2 } else { 0 }
            || dm.signal_chroma_format != 0
            || dm.signal_eotf != 65535
            || dm.signal_full_range_flag != 1
            || dm.signal_eotf_param0 != 0
            || dm.signal_eotf_param1 != 0
            || dm.signal_eotf_param2 != 0
        {
            return Err(invalid());
        }
        let mut polynomial_segments = 0_u32;
        let mut mmr_segments = 0_u32;
        for curve in &mapping.curves {
            // libdovi stores the first absolute pivot followed by positive
            // deltas; FFmpeg's public metadata exposes cumulative pivots.
            if !(2..=9).contains(&curve.pivots.len()) || curve.pivots.first() != Some(&0) {
                return Err(invalid());
            }
            let mut last = 0_u64;
            for delta in curve.pivots.iter().skip(1) {
                if *delta == 0 {
                    return Err(invalid());
                }
                last = last.checked_add(u64::from(*delta)).ok_or_else(invalid)?;
                if last > 1023 {
                    return Err(invalid());
                }
            }
            if last != 1023 {
                return Err(invalid());
            }
            let segments = curve.pivots.len() - 1;
            match curve.mapping_idc {
                DoviMappingMethod::Polynomial => {
                    let value = curve.polynomial.as_ref().ok_or_else(invalid)?;
                    if value.poly_order_minus1.len() != segments
                        || value.poly_order_minus1.iter().any(|order| *order > 1)
                        || value.linear_interp_flag.iter().any(|flag| *flag)
                        || curve.mmr.is_some()
                    {
                        return Err(invalid());
                    }
                    polynomial_segments = polynomial_segments
                        .checked_add(segments as u32)
                        .ok_or_else(invalid)?;
                }
                DoviMappingMethod::MMR => {
                    let value = curve.mmr.as_ref().ok_or_else(invalid)?;
                    if value.mmr_order_minus1.len() != segments
                        || value.mmr_order_minus1.iter().any(|order| *order > 2)
                        || curve.polynomial.is_some()
                    {
                        return Err(invalid());
                    }
                    mmr_segments = mmr_segments
                        .checked_add(segments as u32)
                        .ok_or_else(invalid)?;
                }
                _ => return Err(invalid()),
            }
        }
        let mut levels = BTreeSet::new();
        for level in 1..=255 {
            let blocks: Vec<_> = dm.level_blocks_iter(level).collect();
            if blocks.is_empty() {
                continue;
            }
            if !matches!(level, 1 | 2 | 3 | 4 | 5 | 6 | 8 | 9 | 11 | 254)
                || matches!(level, 2 | 8) && blocks.len() > 16
            {
                return Err(invalid());
            }
            for block in blocks {
                let valid = match block {
                    ExtMetadataBlock::Level5(value) => {
                        u32::from(value.active_area_left_offset)
                            + u32::from(value.active_area_right_offset)
                            < raster.0
                            && u32::from(value.active_area_top_offset)
                                + u32::from(value.active_area_bottom_offset)
                                < raster.1
                    }
                    ExtMetadataBlock::Level8(value) => value.length == 10,
                    ExtMetadataBlock::Level9(value) => {
                        value.length == 1 && value.source_primary_index == 0
                    }
                    ExtMetadataBlock::Level11(value) => {
                        value.content_type == 1
                            && value.whitepoint == 0
                            && value.reference_mode_flag
                            && value.reserved_byte2 == 0
                            && value.reserved_byte3 == 0
                    }
                    ExtMetadataBlock::Level254(value) => {
                        value.dm_mode == 0 && value.dm_version_index == 2
                    }
                    _ => true,
                };
                if !valid {
                    return Err(invalid());
                }
            }
            levels.insert(level);
        }
        let mut operations = vec![DvOperation::RepresentationNormalization];
        if polynomial_segments > 0 {
            operations.push(DvOperation::PolynomialReshape);
        }
        if mmr_segments > 0 {
            operations.push(DvOperation::MmrReshape);
        }
        operations.extend([
            DvOperation::RpuColorConversion,
            DvOperation::TargetMapping,
            DvOperation::Bt2020NclConversion,
            DvOperation::Main10Encoding,
        ]);
        Ok(Self {
            profile,
            source_el_kind: match rpu.el_type {
                Some(DoviELType::FEL) => DvElKind::Fel,
                Some(DoviELType::MEL) => DvElKind::Mel,
                None => DvElKind::None,
            },
            applied_operations: operations,
            polynomial_segments,
            mmr_segments,
            metadata_levels: levels,
        })
    }
}

/// Exact parsed predicate for the initial synthetic source (not a parser).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvParsedMetadata {
    pub profile: u8,
    pub depths: [u8; 3],
    pub luma_integer: [i64; 2],
    pub luma_fraction: [u64; 2],
    pub coefficient_denominator: u64,
    pub pivots: [u16; 2],
    pub curve_representation: DvCurveRepresentation,
    pub polynomial_order_minus1: u8,
    pub chroma_integer: [[i64; 2]; 2],
    pub chroma_fraction: [[u64; 2]; 2],
    pub coefficient_data_type: u8,
    pub bl_full_range: bool,
    pub source_el_kind: DvElKind,
    pub nonlinear_matrix: [i16; 9],
    pub linear_matrix: [i16; 9],
    pub nonlinear_offsets: [u32; 3],
    pub nlq_offset: [u16; 3],
    pub nlq_slope: [u32; 3],
    pub nlq_slope_integer: [u32; 3],
    pub nlq_threshold_integer: [u32; 3],
    pub nlq_threshold: [u32; 3],
    pub nlq_max_integer: [u8; 3],
    pub nlq_max_fraction: [u32; 3],
    pub residual_disabled: bool,
    pub reused: bool,
    pub creative_trim_levels: BTreeSet<u8>,
    pub other_metadata_levels: BTreeSet<u8>,
}
impl DvParsedMetadata {
    /// Project checked scalar facts from the actual fresh source RPU. Parser
    /// failure, reuse, missing curves, extra polynomial segments or unknown NLQ
    /// cannot be replaced by helper JSON claims. Route admission still applies
    /// the selected subset predicate to this projection.
    pub fn from_raw_rpu(raw: &[u8]) -> Result<Self, DvContractError> {
        use dolby_vision::rpu::{
            dovi_rpu::DoviRpu,
            rpu_data_mapping::{DoviMappingMethod, DoviNlqMethod},
            rpu_data_nlq::DoviELType,
        };
        if raw.len() < 3 || raw.len() > 4098 || raw[..2] != [0x7c, 0x01] {
            return Err(DvContractError::Invalid("bounded source RPU NAL"));
        }
        let rpu = DoviRpu::parse_unspec62_nalu(raw)
            .map_err(|_| DvContractError::Invalid("raw RPU parser"))?;
        let header = &rpu.header;
        if rpu.dovi_profile != 7
            || rpu.el_type != Some(DoviELType::FEL)
            || header.use_prev_vdr_rpu_flag
            || header.disable_residual_flag
            || !header.vdr_dm_metadata_present_flag
            || header.rpu_type != 2
            || header.rpu_format != 18
            || header.vdr_rpu_profile != 1
            || header.vdr_rpu_level != 0
            || !header.vdr_seq_info_present_flag
            || header.chroma_resampling_explicit_filter_flag
            || header.vdr_rpu_normalized_idc != 1
            || header.reserved_zero_3bits != 0
            || header.spatial_resampling_filter_flag
            || !header.el_spatial_resampling_filter_flag
            || header.coefficient_log2_denom != 23
        {
            return Err(DvContractError::Invalid("unsupported source RPU header"));
        }
        let mapping = rpu
            .rpu_data_mapping
            .as_ref()
            .ok_or(DvContractError::Invalid("missing source mapping"))?;
        let dm = rpu
            .vdr_dm_data
            .as_ref()
            .ok_or(DvContractError::Invalid("missing source color conversion"))?;
        if mapping.num_x_partitions_minus1 != 0
            || mapping.num_y_partitions_minus1 != 0
            || mapping.nlq_method_idc != Some(DoviNlqMethod::LinearDeadzone)
            || mapping.nlq_num_pivots_minus2 != Some(0)
            || dm.signal_bit_depth != 12
            || dm.signal_color_space != 0
            || dm.signal_chroma_format != 0
            || dm.signal_eotf != 65535
            || dm.signal_full_range_flag != 1
            || dm.signal_eotf_param0 != 0
            || dm.signal_eotf_param1 != 0
            || dm.signal_eotf_param2 != 0
        {
            return Err(DvContractError::Invalid(
                "unsupported source RPU representation",
            ));
        }
        let mut integer = [[0; 2]; 3];
        let mut fraction = [[0; 2]; 3];
        for (component, curve) in mapping.curves.iter().enumerate() {
            let polynomial = curve
                .polynomial
                .as_ref()
                .ok_or(DvContractError::Invalid("missing polynomial"))?;
            if curve.mapping_idc != DoviMappingMethod::Polynomial
                || curve.pivots != [0, 1023]
                || curve.num_pivots_minus2 != 0
                || curve.mmr.is_some()
                || polynomial.poly_order_minus1 != [0]
                || polynomial.linear_interp_flag.iter().any(|flag| *flag)
                || polynomial.poly_coef_int.len() != 1
                || polynomial.poly_coef.len() != 1
                || polynomial.poly_coef_int[0].len() != 2
                || polynomial.poly_coef[0].len() != 2
            {
                return Err(DvContractError::Invalid("unsupported polynomial shape"));
            }
            integer[component].copy_from_slice(&polynomial.poly_coef_int[0]);
            fraction[component].copy_from_slice(&polynomial.poly_coef[0]);
        }
        let nlq = mapping
            .nlq
            .as_ref()
            .ok_or(DvContractError::Invalid("missing NLQ"))?;
        fn u32s(values: [u64; 3]) -> Result<[u32; 3], DvContractError> {
            let mut result = [0; 3];
            for (index, value) in values.into_iter().enumerate() {
                result[index] = u32::try_from(value)
                    .map_err(|_| DvContractError::Invalid("NLQ scalar overflow"))?;
            }
            Ok(result)
        }
        let mut maximum_integer = [0; 3];
        for (index, value) in nlq.vdr_in_max_int.into_iter().enumerate() {
            maximum_integer[index] = u8::try_from(value)
                .map_err(|_| DvContractError::Invalid("NLQ maximum overflow"))?;
        }
        let mut creative = BTreeSet::new();
        let mut other = BTreeSet::new();
        for level in 1..=255 {
            if dm.level_blocks_iter(level).next().is_some() {
                if matches!(level, 2 | 8) {
                    creative.insert(level);
                } else {
                    other.insert(level);
                }
            }
        }
        let mut depths = [0; 3];
        for (index, depth) in [
            header.bl_bit_depth_minus8,
            header.el_bit_depth_minus8,
            header.vdr_bit_depth_minus8,
        ]
        .into_iter()
        .enumerate()
        {
            depths[index] = depth
                .checked_add(8)
                .and_then(|depth| u8::try_from(depth).ok())
                .ok_or(DvContractError::Invalid("RPU depth overflow"))?;
        }
        Ok(Self {
            profile: rpu.dovi_profile,
            depths,
            luma_integer: integer[0],
            luma_fraction: fraction[0],
            coefficient_denominator: 8_388_608,
            pivots: [0, 1023],
            curve_representation: DvCurveRepresentation::PolynomialAffine,
            polynomial_order_minus1: 0,
            chroma_integer: [integer[1], integer[2]],
            chroma_fraction: [fraction[1], fraction[2]],
            coefficient_data_type: header.coefficient_data_type,
            bl_full_range: header.bl_video_full_range_flag,
            source_el_kind: DvElKind::Fel,
            nonlinear_matrix: [
                dm.ycc_to_rgb_coef0,
                dm.ycc_to_rgb_coef1,
                dm.ycc_to_rgb_coef2,
                dm.ycc_to_rgb_coef3,
                dm.ycc_to_rgb_coef4,
                dm.ycc_to_rgb_coef5,
                dm.ycc_to_rgb_coef6,
                dm.ycc_to_rgb_coef7,
                dm.ycc_to_rgb_coef8,
            ],
            linear_matrix: [
                dm.rgb_to_lms_coef0,
                dm.rgb_to_lms_coef1,
                dm.rgb_to_lms_coef2,
                dm.rgb_to_lms_coef3,
                dm.rgb_to_lms_coef4,
                dm.rgb_to_lms_coef5,
                dm.rgb_to_lms_coef6,
                dm.rgb_to_lms_coef7,
                dm.rgb_to_lms_coef8,
            ],
            nonlinear_offsets: [
                dm.ycc_to_rgb_offset0,
                dm.ycc_to_rgb_offset1,
                dm.ycc_to_rgb_offset2,
            ],
            nlq_offset: nlq.nlq_offset,
            nlq_slope: u32s(nlq.linear_deadzone_slope)?,
            nlq_slope_integer: u32s(nlq.linear_deadzone_slope_int)?,
            nlq_threshold_integer: u32s(nlq.linear_deadzone_threshold_int)?,
            nlq_threshold: u32s(nlq.linear_deadzone_threshold)?,
            nlq_max_integer: maximum_integer,
            nlq_max_fraction: u32s(nlq.vdr_in_max)?,
            residual_disabled: header.disable_residual_flag,
            reused: header.use_prev_vdr_rpu_flag,
            creative_trim_levels: creative,
            other_metadata_levels: other,
        })
    }

    pub fn runtime_supported(&self) -> bool {
        let mut scalar = self.clone();
        scalar.creative_trim_levels.clear();
        scalar.other_metadata_levels.clear();
        scalar.nlq_offset = [512; 3];
        scalar.nlq_slope = [8192; 3];
        scalar.nlq_threshold = [4096; 3];
        scalar.nlq_max_integer = [1; 3];
        scalar.nlq_max_fraction = [0; 3];
        if !scalar.supported(DvMetadataSubset::SyntheticP7IdentityLinearNlqV1)
            || !self
                .creative_trim_levels
                .iter()
                .all(|level| matches!(level, 2 | 8))
            || !self
                .other_metadata_levels
                .iter()
                .all(|level| matches!(level, 1 | 3 | 4 | 5 | 6 | 9 | 11 | 254))
        {
            return false;
        }
        for component in 0..3 {
            let offset = u64::from(self.nlq_offset[component]);
            let slope = u64::from(self.nlq_slope[component]);
            let threshold = u64::from(self.nlq_threshold[component]);
            let maximum = u64::from(self.nlq_max_integer[component]) * 8_388_608
                + u64::from(self.nlq_max_fraction[component]);
            if offset > 1023
                || slope >= 8_388_608
                || threshold >= 8_388_608
                || self.nlq_max_integer[component] > 1
                || self.nlq_max_fraction[component] >= 8_388_608
            {
                return false;
            }
            let worst = (2 * offset.max(1023 - offset) - 1) * slope + 2 * threshold;
            if worst > 2 * 8_388_608 || worst != 0 && worst + 256 > 2 * maximum {
                return false;
            }
        }
        true
    }

    fn supported(&self, subset: DvMetadataSubset) -> bool {
        if subset == DvMetadataSubset::RuntimeBaseRpuMasterDomainV1 {
            return false;
        }
        if subset == DvMetadataSubset::RuntimeP7MasterDomainLinearDzV1 {
            return self.runtime_supported();
        }
        let curve = match subset {
            DvMetadataSubset::SyntheticP7IdentityLinearNlqV1
            | DvMetadataSubset::SyntheticP81IdentityV1 => ([0, 1], [0, 0]),
            DvMetadataSubset::SyntheticP7AffineLumaLinearNlqV1 => ([0, 0], [524_288, 6_291_456]),
            DvMetadataSubset::RuntimeP7MasterDomainLinearDzV1
            | DvMetadataSubset::RuntimeBaseRpuMasterDomainV1 => {
                unreachable!("handled runtime subset")
            }
        };
        let p81 = subset == DvMetadataSubset::SyntheticP81IdentityV1;
        self.profile == if p81 { 8 } else { 7 }
            && self.depths == [10, 10, 12]
            && self.luma_integer == curve.0
            && self.luma_fraction == curve.1
            && self.coefficient_denominator == 8_388_608
            && self.pivots == [0, 1023]
            && self.curve_representation == DvCurveRepresentation::PolynomialAffine
            && self.polynomial_order_minus1 == 0
            && self.chroma_integer == [[0, 1]; 2]
            && self.chroma_fraction == [[0, 0]; 2]
            && self.coefficient_data_type == 0
            && !self.bl_full_range
            && self.source_el_kind == if p81 { DvElKind::None } else { DvElKind::Fel }
            && self.nonlinear_matrix == [9574, 0, 13802, 9574, -1540, -5348, 9574, 17610, 0]
            && self.linear_matrix == [7222, 8771, 390, 2654, 12430, 1300, 0, 422, 15962]
            && self.nonlinear_offsets == [16_777_216, 134_217_728, 134_217_728]
            && self.residual_disabled == p81
            && !self.reused
            && self.creative_trim_levels.is_empty()
            && self
                .other_metadata_levels
                .iter()
                .all(|level| matches!(level, 1 | 6))
            && (p81
                || (self.nlq_offset == [512; 3]
                    && self.nlq_slope_integer == [0; 3]
                    && self.nlq_threshold_integer == [0; 3]
                    && self.nlq_slope == [8192; 3]
                    && self.nlq_threshold == [4096; 3]
                    && self.nlq_max_integer == [1; 3]
                    && self.nlq_max_fraction == [0; 3]))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvMetadataAdaptationEvidence {
    pub source_rpu: DvDigest,
    pub reconstructed_base: DvDigest,
    pub destination_rpu: DvDigest,
    pub parsed: DvParsedMetadata,
}

pub struct DvFrameEvidence {
    pub key: DvFrameKey,
    pub duration: DvDuration,
    pub observed_source: DecodeSourceIdentity,
    pub backend: DvBackendIdentity,
    pub bl_payload: DvDigest,
    pub bl_shape: DvPlaneShape,
    pub el_shape: Option<DvPlaneShape>,
    pub el_payload: Option<(DvFrameKey, DvDigest)>,
    pub fresh_raw_rpu: Option<DvDigest>,
    pub raw_rpu_frame: Option<DvFrameKey>,
    pub parsed: DvParsedMetadata,
    pub reconstructed_base: Option<DvDigest>,
    pub adapted: Option<DvMetadataAdaptationEvidence>,
    pub applied_operations: Vec<DvOperation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DvFrameAcceptance {
    key: DvFrameKey,
    duration: DvDuration,
    bl_payload: Option<DvDigest>,
    bl_shape: DvPlaneShape,
    el_shape: Option<DvPlaneShape>,
    el_payload: Option<DvDigest>,
    el_bound: bool,
    raw_rpu: DvDigest,
    applied_operations: Vec<DvOperation>,
    metadata_subset: DvMetadataSubset,
    metadata_levels_present: BTreeSet<u8>,
    unprocessed_metadata_levels: BTreeSet<u8>,
    adapted: Option<DvMetadataAdaptationEvidence>,
}
#[derive(Debug, Clone, Serialize)]
pub struct DvStageCounts {
    pub decoded: u64,
    pub seen: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub emitted: u64,
    pub rpu_fresh_accepted: u64,
    pub rpu_reuse_accepted: u64,
    pub rpu_rejected: u64,
    pub rpu_missing: u64,
    pub el_accepted: u64,
    pub el_not_required: u64,
    pub el_rejected: u64,
    pub el_missing: u64,
    pub residual_dropped_frames: u64,
    pub operations_applied: BTreeMap<DvOperation, u64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct DvProcessingReceipt {
    schema: u32,
    plan_digest: DvDigest,
    generation: DvDigest,
    attempt: DvDigest,
    object_shape: DvDigest,
    backend: DvBackendIdentity,
    observed_source: DecodeSourceIdentity,
    epoch: u64,
    expected_interval: DvIntervalExpectation,
    served_frames: Vec<DvFrameAcceptance>,
    counts: DvStageCounts,
    observation_complete: bool,
    terminal_failure: Option<DvFallbackReason>,
    scope: DvEvidenceScope,
}
impl DvProcessingReceipt {
    /// Bind completed private-window evidence to the publication that actually
    /// consumed it. This is called only after the existing sink commits bytes.
    pub fn bind_publication(
        &mut self,
        playback_generation: &str,
        object_shape: DvDigest,
    ) -> Result<(), DvContractError> {
        self.generation = dv_playback_generation_digest(playback_generation)?;
        self.object_shape = object_shape;
        Ok(())
    }
    pub fn counts(&self) -> &DvStageCounts {
        &self.counts
    }
    pub fn served_frames(&self) -> &[DvFrameAcceptance] {
        &self.served_frames
    }
    /// Project actual served-frame evidence onto the existing playback identity.
    /// M1's empty registry keeps every shipping response unchanged and omitted.
    pub fn hdr10_effective_report(
        &self,
        registry: &DvProductionRegistry,
        plan: &DvProcessingPlan,
        active_playback_generation: &str,
        object_shape: &DvDigest,
    ) -> Result<Option<DvEffectiveProcessingReport>, DvContractError> {
        let generation = dv_playback_generation_digest(active_playback_generation)?;
        if !self.reports_hdr10_enhanced(registry, plan, &generation, object_shape) {
            return Ok(None);
        }
        let mut common: BTreeSet<_> = self.served_frames[0]
            .applied_operations
            .iter()
            .copied()
            .collect();
        for frame in &self.served_frames[1..] {
            common.retain(|operation| frame.applied_operations.contains(operation));
        }
        let fel_contributed = self.served_frames.iter().all(|frame| {
            frame.el_bound
                && frame
                    .applied_operations
                    .contains(&DvOperation::LinearNlqResidual)
        });
        Ok(Some(DvEffectiveProcessingReport {
            generation: active_playback_generation.to_owned(),
            hdr10_enhanced: true,
            fel_contributed,
            applied_operations: common.into_iter().collect(),
        }))
    }

    /// Only completed daemon-checked runtime windows can award HDR10-E.
    pub fn reports_hdr10_enhanced(
        &self,
        registry: &DvProductionRegistry,
        plan: &DvProcessingPlan,
        active_generation: &DvDigest,
        object_shape: &DvDigest,
    ) -> bool {
        registry.permits(plan)
            && self.scope == DvEvidenceScope::ProductionRoute
            && self.schema == DV_CONTRACT_VERSION
            && self.plan_digest == plan.semantic_digest()
            && &self.generation == active_generation
            && &self.object_shape == object_shape
            && self.observation_complete
            && self.terminal_failure.is_none()
            && !self.served_frames.is_empty()
            && plan.destination == DvDestination::Hdr10
    }
}

/// Record the bounded window already accepted by the daemon completion factory.
/// Pixel checksums are optional diagnostics; actual paired-layer custody and
/// fresh RPU/sample association establish which operations ran.
pub fn dv_receipt_completed_window(
    plan: &DvProcessingPlan,
    keys: &[DvFrameKey],
    durations: &[DvDuration],
    rpus: &[Vec<u8>],
) -> Result<DvProcessingReceipt, DvContractError> {
    let invalid = || DvContractError::Invalid("completed window receipt");
    if !DvProductionRegistry.permits(plan)
        || keys.is_empty()
        || keys.len() > 64
        || keys.len() != durations.len()
        || keys.len() != rpus.len()
        || plan
            .source
            .coverage
            .as_ref()
            .is_none_or(|coverage| coverage.sampled_frames != keys.iter().cloned().collect())
    {
        return Err(invalid());
    }
    let requires_el = plan.graph.strategy != DvStrategy::BaseRpuToHdr10;
    let mut served_frames = Vec::with_capacity(keys.len());
    let mut expected = BTreeMap::new();
    let mut operations_applied = BTreeMap::new();
    for ((key, duration), raw) in keys.iter().zip(durations).zip(rpus) {
        if !duration.initial_timing_supported() || expected.insert(key.clone(), *duration).is_some()
        {
            return Err(invalid());
        }
        let (operations, retained_levels) = if requires_el {
            let parsed = DvParsedMetadata::from_raw_rpu(raw)?;
            if !parsed.runtime_supported() || parsed.source_el_kind != plan.source.el_kind {
                return Err(invalid());
            }
            (
                plan.graph.required_operations(),
                parsed
                    .other_metadata_levels
                    .union(&parsed.creative_trim_levels)
                    .copied()
                    .collect(),
            )
        } else {
            let shape = plan.capability.bl_shape;
            let parsed = DvBaseMetadata::from_raw_rpu(raw, (shape.width, shape.height))?;
            if Some(i64::from(parsed.profile)) != plan.source.catalog.profile
                || parsed.source_el_kind != plan.source.el_kind
                || plan.capability.el_shape.is_some()
            {
                return Err(invalid());
            }
            (parsed.applied_operations, parsed.metadata_levels)
        };
        for operation in &operations {
            let value = operations_applied.entry(*operation).or_insert(0);
            bump(value)?;
        }
        served_frames.push(DvFrameAcceptance {
            key: key.clone(),
            duration: *duration,
            bl_payload: None,
            bl_shape: plan.capability.bl_shape,
            el_shape: plan.capability.el_shape,
            el_payload: None,
            el_bound: requires_el,
            raw_rpu: DvDigest(hex::encode(Sha256::digest(raw))),
            applied_operations: operations,
            metadata_subset: plan.capability.subset,
            metadata_levels_present: retained_levels.clone(),
            unprocessed_metadata_levels: retained_levels,
            adapted: None,
        });
    }
    let count = keys.len() as u64;
    let binding = plan.capability.evidence_digest.clone();
    Ok(DvProcessingReceipt {
        schema: DV_CONTRACT_VERSION,
        plan_digest: plan.semantic_digest(),
        generation: binding.clone(),
        attempt: binding.clone(),
        object_shape: binding.clone(),
        backend: plan.capability.identity.clone(),
        observed_source: plan.source.observed_source.clone(),
        epoch: keys[0].continuity_epoch,
        expected_interval: DvIntervalExpectation {
            source: plan.source.observed_source.clone(),
            stream: plan.source.absolute_video_index,
            epoch: keys[0].continuity_epoch,
            evidence: binding,
            start: keys[0].pts,
            end: durations[count as usize - 1].end_from(keys[count as usize - 1].pts)?,
            frames: expected,
        },
        served_frames,
        counts: DvStageCounts {
            decoded: count,
            seen: count,
            accepted: count,
            rejected: 0,
            emitted: count,
            rpu_fresh_accepted: count,
            rpu_reuse_accepted: 0,
            rpu_rejected: 0,
            rpu_missing: 0,
            el_accepted: if requires_el { count } else { 0 },
            el_not_required: if requires_el { 0 } else { count },
            el_rejected: 0,
            el_missing: 0,
            residual_dropped_frames: 0,
            operations_applied,
        },
        observation_complete: true,
        terminal_failure: None,
        scope: DvEvidenceScope::ProductionRoute,
    })
}

fn bump(value: &mut u64) -> Result<(), DvContractError> {
    *value = value.checked_add(1).ok_or(DvContractError::Capacity)?;
    Ok(())
}

/// Complete expected presentation comes from a separately bound source trace,
/// never from the observed emitted subset. M1 has only a fixture constructor.
#[derive(Debug, Clone, Serialize)]
pub struct DvIntervalExpectation {
    source: DecodeSourceIdentity,
    stream: u32,
    epoch: u64,
    evidence: DvDigest,
    start: DvTimestamp,
    end: DvTimestamp,
    frames: BTreeMap<DvFrameKey, DvDuration>,
}
impl DvIntervalExpectation {
    #[cfg(test)]
    fn fixture(
        source: DecodeSourceIdentity,
        stream: u32,
        epoch: u64,
        evidence: DvDigest,
        source_trace: Vec<(DvFrameKey, DvDuration)>,
        start: DvTimestamp,
        end: DvTimestamp,
    ) -> Result<Self, DvContractError> {
        if source_trace.is_empty() || source_trace.len() > DV_MAX_CONTROL_FRAMES || start >= end {
            return Err(DvContractError::Invalid("source interval"));
        }
        let mut trace = source_trace;
        trace.sort_by_key(|a| a.0.pts);
        let mut previous_end = None;
        let mut frames = BTreeMap::new();
        for (key, duration) in trace {
            if key.absolute_video_index != stream
                || key.continuity_epoch != epoch
                || !duration.initial_timing_supported()
            {
                return Err(DvContractError::Invalid("source interval binding"));
            }
            let frame_end = duration.end_from(key.pts)?;
            if previous_end.is_some_and(|previous| previous > key.pts) {
                return Err(DvContractError::Invalid("overlapping source pictures"));
            }
            previous_end = Some(frame_end);
            if key.pts >= start && key.pts < end {
                if frame_end > end {
                    return Err(DvContractError::Invalid("unqualified presentation cut"));
                }
                if frames.keys().any(|known: &DvFrameKey| known.pts == key.pts) {
                    return Err(DvContractError::DuplicateFrame);
                }
                frames.insert(key, duration);
            }
        }
        if frames.first_key_value().map(|(key, _)| key.pts) != Some(start)
            || frames
                .last_key_value()
                .map(|(key, duration)| duration.end_from(key.pts))
                .transpose()?
                != Some(end)
        {
            return Err(DvContractError::Invalid("unqualified presentation bounds"));
        }
        Ok(Self {
            source,
            stream,
            epoch,
            evidence,
            start,
            end,
            frames,
        })
    }
}

#[derive(Debug, Default)]
pub struct DvIntervalSettlementLedger {
    settled: Vec<DvIntervalExpectation>,
}
impl DvIntervalSettlementLedger {
    fn record(&mut self, next: &DvIntervalExpectation) -> Result<(), DvContractError> {
        if self.settled.len() >= DV_MAX_CONTROL_FRAMES {
            return Err(DvContractError::Capacity);
        }
        if self.settled.iter().any(|old| {
            old.source == next.source
                && old.stream == next.stream
                && old.epoch == next.epoch
                && old.start < next.end
                && next.start < old.end
        }) {
            return Err(DvContractError::Invalid("overlapping settled intervals"));
        }
        self.settled.push(next.clone());
        Ok(())
    }
}

enum DvTerminalFrame {
    Accepted(Box<DvFrameAcceptance>),
    Rejected {
        rpu: DvRpuDecision,
        el: DvElDecision,
    },
}
#[derive(Clone, Copy)]
enum DvRpuDecision {
    FreshAccepted,
    Rejected,
    Missing,
}
#[derive(Clone, Copy)]
enum DvElDecision {
    Accepted,
    Rejected,
    Missing,
    NotRequired,
}
/// Bounded interval observation: decoded/preroll work is not the emitted set.
pub struct DvIntervalObserver {
    plan: DvProcessingPlan,
    epoch: u64,
    expectation: DvIntervalExpectation,
    decoded: BTreeMap<DvFrameKey, DvDuration>,
    terminals: BTreeMap<DvFrameKey, DvTerminalFrame>,
    emitted: BTreeSet<DvFrameKey>,
    terminal_failure: Option<DvFallbackReason>,
}
impl DvIntervalObserver {
    pub fn new(
        plan: DvProcessingPlan,
        expectation: DvIntervalExpectation,
    ) -> Result<Self, DvContractError> {
        if expectation.source != plan.source.observed_source
            || expectation.stream != plan.source.absolute_video_index
            || plan
                .source
                .coverage
                .as_ref()
                .is_none_or(|coverage| coverage.epoch != expectation.epoch)
        {
            return Err(DvContractError::Invalid("expected source/stream"));
        }
        let epoch = expectation.epoch;
        Ok(Self {
            plan,
            epoch,
            expectation,
            decoded: BTreeMap::new(),
            terminals: BTreeMap::new(),
            emitted: BTreeSet::new(),
            terminal_failure: None,
        })
    }
    pub fn record_decoded(
        &mut self,
        key: DvFrameKey,
        duration: DvDuration,
    ) -> Result<(), DvContractError> {
        if self.decoded.len() >= self.plan.capability.max_frames {
            return Err(DvContractError::Capacity);
        }
        if key.continuity_epoch != self.epoch
            || key.absolute_video_index != self.plan.source.absolute_video_index
        {
            return Err(DvContractError::Invalid("decoded epoch/stream"));
        }
        if !duration.initial_timing_supported() {
            return Err(DvContractError::Invalid("duration provenance"));
        }
        if self.decoded.keys().any(|known| known.pts == key.pts) {
            return Err(DvContractError::DuplicateFrame);
        }
        if key.pts >= self.expectation.start
            && key.pts < self.expectation.end
            && self.expectation.frames.get(&key) != Some(&duration)
        {
            return Err(DvContractError::Invalid(
                "source-grounded interval coverage",
            ));
        }
        self.decoded.insert(key, duration);
        Ok(())
    }
    pub fn accept(&mut self, frame: DvFrameEvidence) -> Result<(), DvContractError> {
        if self.terminals.contains_key(&frame.key) {
            return Err(DvContractError::DuplicateFrame);
        }
        if self.decoded.get(&frame.key) != Some(&frame.duration) {
            return Err(DvContractError::Invalid("decoded frame coverage"));
        }
        let result = self.validate_frame(&frame);
        let terminal = match result {
            Ok(()) => DvTerminalFrame::Accepted(Box::new(DvFrameAcceptance {
                key: frame.key.clone(),
                duration: frame.duration,
                bl_payload: Some(frame.bl_payload),
                bl_shape: frame.bl_shape,
                el_shape: frame.el_shape,
                el_payload: frame.el_payload.map(|(_, payload)| payload),
                el_bound: frame.el_shape.is_some(),
                raw_rpu: frame.fresh_raw_rpu.expect("validated raw RPU"),
                applied_operations: frame.applied_operations,
                metadata_subset: self.plan.capability.subset,
                metadata_levels_present: frame.parsed.other_metadata_levels.clone(),
                unprocessed_metadata_levels: frame.parsed.other_metadata_levels.clone(),
                adapted: frame.adapted,
            })),
            Err(_) => {
                let source_bound = frame.observed_source == self.plan.source.observed_source
                    && frame.backend == self.plan.capability.identity;
                let rpu = if frame.fresh_raw_rpu.is_none() {
                    DvRpuDecision::Missing
                } else if source_bound
                    && frame.raw_rpu_frame.as_ref() == Some(&frame.key)
                    && frame.parsed.supported(self.plan.capability.subset)
                {
                    DvRpuDecision::FreshAccepted
                } else {
                    DvRpuDecision::Rejected
                };
                let el = if self.plan.graph.strategy == DvStrategy::BaseRpuToHdr10 {
                    DvElDecision::NotRequired
                } else {
                    match &frame.el_payload {
                        None => DvElDecision::Missing,
                        Some((key, _))
                            if source_bound
                                && key == &frame.key
                                && frame.el_shape == self.plan.capability.el_shape =>
                        {
                            DvElDecision::Accepted
                        }
                        Some(_) => DvElDecision::Rejected,
                    }
                };
                DvTerminalFrame::Rejected { rpu, el }
            }
        };
        self.terminals.insert(frame.key, terminal);
        result
    }
    fn validate_frame(&self, frame: &DvFrameEvidence) -> Result<(), DvContractError> {
        if frame.bl_shape != self.plan.capability.bl_shape {
            return Err(DvContractError::Invalid(
                "observed base representation/geometry",
            ));
        }
        if frame.observed_source != self.plan.source.observed_source
            || frame.backend != self.plan.capability.identity
        {
            return Err(DvContractError::Invalid("consumed source/backend"));
        }
        if frame.raw_rpu_frame.as_ref() != Some(&frame.key)
            || frame.fresh_raw_rpu.is_none()
            || frame.parsed.source_el_kind != self.plan.source.el_kind
            || !frame.parsed.supported(self.plan.capability.subset)
        {
            return Err(DvContractError::Invalid("fresh supported RPU"));
        }
        let requires_el = self.plan.graph.strategy != DvStrategy::BaseRpuToHdr10;
        if requires_el
            && (frame.el_shape != self.plan.capability.el_shape
                || !frame
                    .el_payload
                    .as_ref()
                    .is_some_and(|(key, _)| key == &frame.key))
        {
            return Err(DvContractError::Invalid("same-frame enhancement"));
        }
        if !requires_el && (frame.el_payload.is_some() || frame.el_shape.is_some()) {
            return Err(DvContractError::Invalid(
                "unexpected enhancement application",
            ));
        }
        if frame.applied_operations != self.plan.graph.required_operations() {
            return Err(DvContractError::Invalid(
                "once-applied same-frame operation chain",
            ));
        }
        if self.plan.graph.adapt_metadata_to_reconstructed_base {
            if !frame.adapted.as_ref().is_some_and(|adapted| {
                adapted
                    .parsed
                    .supported(DvMetadataSubset::SyntheticP81IdentityV1)
                    && Some(&adapted.source_rpu) == frame.fresh_raw_rpu.as_ref()
                    && Some(&adapted.reconstructed_base) == frame.reconstructed_base.as_ref()
            }) {
                return Err(DvContractError::Invalid(
                    "reconstructed-base metadata adaptation",
                ));
            }
        } else if frame.adapted.is_some() {
            return Err(DvContractError::Invalid("unexpected metadata adaptation"));
        }
        Ok(())
    }
    pub fn emit(&mut self, key: &DvFrameKey) -> Result<(), DvContractError> {
        if !self.expectation.frames.contains_key(key) {
            return Err(DvContractError::Invalid("preroll/lookahead is not served"));
        }
        if !matches!(self.terminals.get(key), Some(DvTerminalFrame::Accepted(_))) {
            return Err(DvContractError::IncompleteCoverage);
        }
        if !self.emitted.insert(key.clone()) {
            return Err(DvContractError::DuplicateFrame);
        }
        Ok(())
    }
    pub fn fail(&mut self, reason: DvFallbackReason) {
        self.terminal_failure.get_or_insert(reason);
    }
    #[allow(clippy::too_many_arguments)]
    pub fn settle(
        &self,
        served: &[DvFrameKey],
        video_objects: usize,
        generation: DvDigest,
        attempt: DvDigest,
        object_shape: DvDigest,
        observation_complete: bool,
        ledger: &mut DvIntervalSettlementLedger,
    ) -> Result<DvProcessingReceipt, DvContractError> {
        let served_set: BTreeSet<_> = served.iter().cloned().collect();
        if served_set.len() != served.len()
            || served_set != self.emitted
            || served_set != self.expectation.frames.keys().cloned().collect()
            || (video_objects > 0 && served.is_empty())
            || !observation_complete
            || self.terminal_failure.is_some()
            || self.terminals.len() != self.decoded.len()
        {
            return Err(DvContractError::IncompleteCoverage);
        }
        ledger.record(&self.expectation)?;
        let mut counts = DvStageCounts {
            decoded: self.decoded.len() as u64,
            seen: self.terminals.len() as u64,
            accepted: 0,
            rejected: 0,
            emitted: self.emitted.len() as u64,
            rpu_fresh_accepted: 0,
            rpu_reuse_accepted: 0,
            rpu_rejected: 0,
            rpu_missing: 0,
            el_accepted: 0,
            el_not_required: 0,
            el_rejected: 0,
            el_missing: 0,
            residual_dropped_frames: 0,
            operations_applied: BTreeMap::new(),
        };
        let mut accepted = Vec::new();
        for (key, terminal) in &self.terminals {
            match terminal {
                DvTerminalFrame::Rejected { rpu, el } => {
                    bump(&mut counts.rejected)?;
                    match rpu {
                        DvRpuDecision::FreshAccepted => counts.rpu_fresh_accepted += 1,
                        DvRpuDecision::Rejected => counts.rpu_rejected += 1,
                        DvRpuDecision::Missing => counts.rpu_missing += 1,
                    }
                    match el {
                        DvElDecision::Accepted => counts.el_accepted += 1,
                        DvElDecision::Rejected => counts.el_rejected += 1,
                        DvElDecision::Missing => counts.el_missing += 1,
                        DvElDecision::NotRequired => counts.el_not_required += 1,
                    }
                }
                DvTerminalFrame::Accepted(frame) => {
                    bump(&mut counts.accepted)?;
                    bump(&mut counts.rpu_fresh_accepted)?;
                    if frame.el_payload.is_some() {
                        bump(&mut counts.el_accepted)?;
                    } else {
                        bump(&mut counts.el_not_required)?;
                    }
                    if self.emitted.contains(key) {
                        if self.plan.source.el_kind == DvElKind::Fel && frame.el_payload.is_none() {
                            bump(&mut counts.residual_dropped_frames)?;
                        }
                        for operation in &frame.applied_operations {
                            let value = counts.operations_applied.entry(*operation).or_insert(0);
                            *value = value.checked_add(1).ok_or(DvContractError::Capacity)?;
                        }
                        accepted.push(frame.as_ref().clone());
                    }
                }
            }
        }
        Ok(DvProcessingReceipt {
            schema: DV_CONTRACT_VERSION,
            plan_digest: self.plan.semantic_digest(),
            generation,
            attempt,
            object_shape,
            backend: self.plan.capability.identity.clone(),
            observed_source: self.plan.source.observed_source.clone(),
            epoch: self.epoch,
            expected_interval: self.expectation.clone(),
            served_frames: accepted,
            counts,
            observation_complete,
            terminal_failure: self.terminal_failure,
            scope: self.plan.capability.scope,
        })
    }
}

/// No implicit reset on generation replacement. A new request makes a new value.
#[derive(Debug)]
pub struct DvRecoveryEpisode {
    attempted: BTreeSet<DvStrategy>,
    failures: u32,
    max_failures: u32,
    pending: Option<DvStrategy>,
    exhausted: bool,
}
impl Default for DvRecoveryEpisode {
    fn default() -> Self {
        Self::new(3).expect("fixed three-candidate episode budget")
    }
}
impl DvRecoveryEpisode {
    pub fn new(max_failures: u32) -> Result<Self, DvContractError> {
        if max_failures == 0 || max_failures > 3 {
            return Err(DvContractError::Invalid("recovery budget"));
        }
        Ok(Self {
            attempted: BTreeSet::new(),
            failures: 0,
            max_failures,
            pending: None,
            exhausted: false,
        })
    }
    pub fn attempt(&mut self, strategy: DvStrategy) -> Result<(), DvContractError> {
        if self.exhausted {
            return Err(DvContractError::Capacity);
        }
        if self.pending.is_some() {
            return Err(DvContractError::Invalid("candidate still active"));
        }
        if !self.attempted.insert(strategy) {
            return Err(DvContractError::Invalid(
                "candidate already attempted in recovery episode",
            ));
        }
        self.pending = Some(strategy);
        Ok(())
    }
    pub fn record_failure(&mut self) -> Result<(), DvContractError> {
        if self.exhausted {
            return Err(DvContractError::Capacity);
        }
        self.pending
            .take()
            .ok_or(DvContractError::Invalid("failure without active candidate"))?;
        self.failures = self
            .failures
            .checked_add(1)
            .ok_or(DvContractError::Capacity)?;
        if self.failures >= self.max_failures {
            self.exhausted = true;
            return Err(DvContractError::Capacity);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
