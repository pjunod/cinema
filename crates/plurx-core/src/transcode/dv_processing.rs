//! Bounded DV processing contracts. No production backend is qualified here.
//!
//! Intent, source sampling and per-emitted-frame observations are separate.
//! These types do not change any existing playback plan, recipe or producer.

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
            DvDurationProvenance::StoredContainer | DvDurationProvenance::DeclaredFixtureInterval
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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
pub enum DvMetadataSubset {
    SyntheticP7IdentityLinearNlqV1,
    SyntheticP7AffineLumaLinearNlqV1,
    SyntheticP81IdentityV1,
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
    identity: DvBackendIdentity,
    schema: u32,
    subset: DvMetadataSubset,
    decoder: DecodeBackend,
    #[serde(serialize_with = "serialize_encoder")]
    encoder: Encoder,
    raster: (u32, u32),
    bl_shape: DvPlaneShape,
    el_shape: DvPlaneShape,
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
    /// One deterministic semantic identity for the future enclosing plan.
    /// Never fed into a shipping recipe in M1.
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

/// Deliberately empty. A scope label/digest cannot add an entry.
#[derive(Debug, Default)]
pub struct DvProductionRegistry;
impl DvProductionRegistry {
    pub fn permits(&self, _plan: &DvProcessingPlan) -> bool {
        false
    }
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
    if input.existing_route == DvExistingRoute::NativeDvCopy {
        return unchanged(R::NativeCompatible);
    }
    if !input.enhanced_encode_allowed {
        return unchanged(R::ExistingDeliveryConstraint);
    }
    match input.destination {
        DvDestination::Hdr10 if !prefs.hdr_processing => return unchanged(R::PreferenceDisabled),
        DvDestination::Profile81 if !prefs.conversion_permitted => {
            return unchanged(R::ConversionForbidden)
        }
        DvDestination::Profile81 if !prefs.fel_reencode => return unchanged(R::PreferenceDisabled),
        _ => {}
    }
    let source = &input.source;
    if source.catalog.profile != Some(7)
        || !matches!(source.catalog.bl_compat_id, Some(1 | 6))
        || !source
            .catalog
            .level
            .is_some_and(|level| (1..=63).contains(&level))
        || source.catalog.rpu_present == Some(false)
        || source.catalog.el_present == Some(false)
    {
        return unchanged(R::UnsupportedProfileOrBase);
    }
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
        || cap.el_shape != DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7ElYuv420P10Residual)?
        || source.bl_shape != Some(cap.bl_shape)
        || source.el_shape != Some(cap.el_shape)
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
    fn supported(&self, subset: DvMetadataSubset) -> bool {
        let curve = match subset {
            DvMetadataSubset::SyntheticP7IdentityLinearNlqV1
            | DvMetadataSubset::SyntheticP81IdentityV1 => ([0, 1], [0, 0]),
            DvMetadataSubset::SyntheticP7AffineLumaLinearNlqV1 => ([0, 0], [524_288, 6_291_456]),
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
    bl_payload: DvDigest,
    bl_shape: DvPlaneShape,
    el_shape: Option<DvPlaneShape>,
    el_payload: Option<DvDigest>,
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
            frame.el_payload.is_some()
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

    /// Empty production registry means no receipt can award HDR10-E in M1.
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
                bl_payload: frame.bl_payload,
                bl_shape: frame.bl_shape,
                el_shape: frame.el_shape,
                el_payload: frame.el_payload.map(|(_, payload)| payload),
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
                                && frame.el_shape == Some(self.plan.capability.el_shape) =>
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
            && (frame.el_shape != Some(self.plan.capability.el_shape)
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
