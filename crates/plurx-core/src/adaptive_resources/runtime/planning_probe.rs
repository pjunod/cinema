//! Selected Linux software VOD pre-acquisition planning ownership.
//! Native source, parser, producer and dependency work is entered only by the
//! original durable P owner; M retains its actual metadata aliases after P
//! process credit is returned.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Read-only attestation of one exact executable object. It is data carried
/// by an admitted report, never a physical receipt or launch capability.
#[derive(Debug)]
pub struct FixedPlanningExecutable {
    path: PathBuf,
    object_version: String,
    sha256: String,
    source_identity: crate::source_identity::SourceIdentity,
}
impl FixedPlanningExecutable {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn object_version(&self) -> &str {
        &self.object_version
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn source_identity(&self) -> crate::source_identity::SourceIdentity {
        self.source_identity
    }
}

/// The exact cold-cache engine observation made under P. Construction remains
/// private to core's fixed collector and closure path. Consumers may derive
/// legacy recipe identities from it but cannot use it as native-close proof.
#[derive(Debug)]
pub struct FixedPlanningEngineAttestation {
    producer: FixedPlanningExecutable,
    reporter: FixedPlanningExecutable,
    dependencies: Arc<[(PathBuf, String)]>,
    media_digest: String,
    build_line: String,
    boot_id: Option<Uuid>,
}
impl FixedPlanningEngineAttestation {
    pub fn producer(&self) -> &FixedPlanningExecutable {
        &self.producer
    }
    pub fn reporter(&self) -> &FixedPlanningExecutable {
        &self.reporter
    }
    pub fn dependencies(&self) -> &[(PathBuf, String)] {
        &self.dependencies
    }
    pub fn media_digest(&self) -> &str {
        &self.media_digest
    }
    /// Already formatted as the incumbent `ffmpeg_build` recipe field.
    pub fn build_line(&self) -> &str {
        &self.build_line
    }
    pub fn boot_id(&self) -> Option<Uuid> {
        self.boot_id
    }
}

/// The original M role retains this separately from all application aliases.
/// Only the last alias's payload destructor signals release. The signal is a
/// heap lifetime fence, not a journal close receipt or physical proof.
pub(super) struct MetadataReleaseState {
    // Reuse the original finite application fence: a report's final guard
    // and the one-shot coordinator/handoff lease must both leave first.
    pub(super) application: Arc<super::finite_lifetime::FiniteAppState>,
    binding: super::ReservationBinding,
    close_taken: std::sync::atomic::AtomicBool,
    pub(super) cleanup_requested: Arc<std::sync::atomic::AtomicBool>,
    pub(super) known_closed: Arc<std::sync::atomic::AtomicBool>,
}
impl MetadataReleaseState {
    pub(super) fn new(binding: super::ReservationBinding, application_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            binding,
            close_taken: std::sync::atomic::AtomicBool::new(false),
            cleanup_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            known_closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            application: super::finite_lifetime::FiniteAppState::with_application_limit(
                application_limit,
            ),
        })
    }
    pub(super) fn take_close(
        &self,
    ) -> Result<FixedMetadataClosure, super::operations::OperationError> {
        if !self.application.released()
            || self
                .close_taken
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(super::operations::OperationError::State);
        }
        Ok(FixedMetadataClosure {
            binding: self.binding,
        })
    }
}

struct MetadataContents {
    document: String,
    facts: crate::transcode::DecodeFacts,
    engine: FixedPlanningEngineAttestation,
    source: FixedObservedSource,
}
struct MetadataPayload {
    contents: Option<MetadataContents>,
    release: Arc<MetadataReleaseState>,
    // Last: actual content deallocation precedes the final alias signal.
    _guard: Arc<super::finite_lifetime::FiniteAppGuard>,
}
impl Drop for MetadataPayload {
    fn drop(&mut self) {
        drop(self.contents.take());
    }
}

/// An admitted, freshly refined source document and exact engine observation.
/// Every clone retains the SAME M role payload; no File, Child, task, or
/// physical acquisition authority is available to daemon callers.
#[derive(Clone)]
pub struct FixedProbeMetadataLease {
    payload: Arc<MetadataPayload>,
}
impl FixedProbeMetadataLease {
    /// One original planning coordinator and its P-to-R handoff. The requested
    /// capacity must fit the application partition charged with M before entry.
    /// This lease is deliberately not cloneable, and Drop signals only the
    /// original fence; it never releases ledger or journal accounting.
    pub fn take_application_lease(
        &self,
        requested_bytes: usize,
    ) -> Result<super::finite_lifetime::FiniteApplicationLease, FixedPlanningProbeError> {
        self.payload
            .release
            .application
            .take_application_lease(requested_bytes)
            .ok_or_else(|| super::operations::OperationError::State.into())
    }
    pub fn application_limit_bytes(&self) -> Option<u64> {
        self.payload.release.application.application_limit_bytes()
    }
    fn contents(&self) -> &MetadataContents {
        self.payload.contents.as_ref().expect("live metadata alias")
    }
    pub fn document(&self) -> &str {
        &self.contents().document
    }
    pub fn facts(&self) -> &crate::transcode::DecodeFacts {
        &self.contents().facts
    }
    pub fn engine(&self) -> &FixedPlanningEngineAttestation {
        &self.contents().engine
    }
    pub fn source_object_version(&self) -> &str {
        &self.contents().source.object_version
    }
    pub fn source_file_id(&self) -> &str {
        &self.contents().source.media_file_id
    }
    pub fn scanner_size(&self) -> i64 {
        self.contents().source.scanner_size
    }
    pub fn scanner_mtime(&self) -> i64 {
        self.contents().source.scanner_mtime
    }
    pub fn source_path(&self) -> &Path {
        &self.contents().source.path
    }
    pub fn finite_source_request(&self) -> super::finite_vod::FiniteSourceRequest {
        super::finite_vod::FiniteSourceRequest {
            path: self.contents().source.path.clone(),
            media_file_id: self.contents().source.media_file_id.clone(),
            object_version: self.contents().source.object_version.clone(),
            scanner_size: self.contents().source.scanner_size,
            scanner_mtime: self.contents().source.scanner_mtime,
        }
    }
    pub fn matches_source(&self, source: &super::finite_vod::FiniteSourceRequest) -> bool {
        self.contents().source.path == source.path
            && self.contents().source.media_file_id == source.media_file_id
            && self.contents().source.object_version == source.object_version
            && self.contents().source.scanner_size == source.scanner_size
            && self.contents().source.scanner_mtime == source.scanner_mtime
    }
}

/// Initial name used at the planning bridge. Clones remain the same M alias.
pub type FixedPlanningProbeReport = FixedProbeMetadataLease;

/// All selected inputs known before the first per-start source/executable
/// lookup. The caller's source authorization remains in the daemon; this
/// request carries scanner and recipe association, never an opened File.
pub struct FixedPlanningSourceRequest {
    pub path: PathBuf,
    pub media_file_id: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
}

struct FixedObservedSource {
    path: PathBuf,
    media_file_id: String,
    object_version: String,
    scanner_size: i64,
    scanner_mtime: i64,
}

pub struct FixedPlanningProbeRequest {
    pub source: FixedPlanningSourceRequest,
    pub recipe_source_binding: String,
    /// Plan-only locator. The admitted collector discovers and seals the
    /// original executable; this string cannot authorize a native launch.
    pub probe_program_locator: String,
    pub cache: Arc<crate::process::fixed_probe::DecodeFactCache>,
    pub catalog: crate::transcode::DecodeCatalogMetadata,
    pub selected_stream: crate::process::fixed_probe::ProbeStreamSelection,
    pub work: crate::process::ChildWork,
    pub ffmpeg_program: String,
    /// The original daemon planning/control frames and P-to-R handoff payload.
    /// Core adds its own source, report, plan and native component projection.
    pub application_metadata_bytes: usize,
}

impl FixedPlanningProbeRequest {
    pub(in crate::adaptive_resources::runtime) fn requested_heap_bytes(&self) -> Option<usize> {
        self.source
            .path
            .capacity()
            .checked_add(self.source.media_file_id.capacity())?
            .checked_add(self.recipe_source_binding.capacity())?
            .checked_add(self.probe_program_locator.capacity())?
            .checked_add(self.ffmpeg_program.capacity())?
            .checked_add(self.catalog.requested_heap_bytes()?)
    }
}

#[derive(Debug)]
pub struct FixedPlanningProbeError(pub(crate) super::operations::OperationError);
impl FixedPlanningProbeError {
    /// Only this exact upstream tag proves refusal before P/M reservation.
    /// A generic constructor error may already retain original obligations;
    /// absence of a returned owner is never native or never-acquired proof.
    pub fn definitely_before_reservation(&self) -> bool {
        matches!(
            self.0,
            super::operations::OperationError::BeforeReservation(_)
        )
    }
}
impl std::fmt::Display for FixedPlanningProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for FixedPlanningProbeError {}
impl From<super::operations::OperationError> for FixedPlanningProbeError {
    fn from(error: super::operations::OperationError) -> Self {
        Self(error)
    }
}

/// Only the original M alias fence can construct this one-shot capability,
/// after its actual content and coordinator/handoff owners have disappeared.
/// This is heap closure only; the M graph may contain no native acquisitions.
pub(in crate::adaptive_resources) struct FixedMetadataClosure {
    binding: super::ReservationBinding,
}
impl FixedMetadataClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> super::ReservationBinding {
        self.binding
    }
}

pub(super) struct PlanningMetadataRole {
    pub(super) plan: crate::resource_custody::codec_v2::FixedProbeMetadataV1,
    pub(super) release: Arc<MetadataReleaseState>,
    /// Structural bound of the bookkeeping surviving P's refund, charged in
    /// M's admission and excluded from the report capacity at install. P's
    /// settlement asserts actual capacities against it; nothing measures it.
    book_structural_bytes: u64,
    payload: std::sync::Weak<MetadataPayload>,
    issued: bool,
    closing: bool,
}

impl PlanningMetadataRole {
    pub(super) fn new(
        plan: crate::resource_custody::codec_v2::FixedProbeMetadataV1,
        binding: super::ReservationBinding,
        application_limit: usize,
        book_structural_bytes: u64,
    ) -> Self {
        Self {
            plan,
            release: MetadataReleaseState::new(binding, application_limit),
            book_structural_bytes,
            payload: std::sync::Weak::new(),
            issued: false,
            closing: false,
        }
    }
    pub(super) fn seal_after_last_payload(&mut self) -> bool {
        // No new payload may be installed after this fence. An existing
        // payload can still be cloned, so its actual final Drop is required.
        // Callers require P to have refunded first.
        if self.payload.upgrade().is_some() {
            return false;
        }
        self.closing = true;
        self.release.application.seal()
    }
}

pub(in crate::adaptive_resources::runtime) mod plan;
use super::operations::{OperationError, OwnedPreparation};
use super::NodeResourceRuntime;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

impl FixedPlanningExecutable {
    fn from_observed(
        path: PathBuf,
        object_version: String,
        sha256: String,
    ) -> Result<Self, OperationError> {
        let mut fields = object_version.split(':');
        if fields.next() != Some("s1") {
            return Err(OperationError::State);
        }
        let mut number = || fields.next().ok_or(OperationError::State);
        let dev = number()?.parse().map_err(|_| OperationError::State)?;
        let ino = number()?.parse().map_err(|_| OperationError::State)?;
        let size = number()?.parse().map_err(|_| OperationError::State)?;
        let mtime = number()?.parse().map_err(|_| OperationError::State)?;
        let mtime_nsec = number()?.parse().map_err(|_| OperationError::State)?;
        let ctime = number()?.parse().map_err(|_| OperationError::State)?;
        let ctime_nsec = number()?.parse().map_err(|_| OperationError::State)?;
        if fields.next().is_some()
            || !path.is_absolute()
            || sha256.len() != 64
            || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(OperationError::State);
        }
        let source_identity = crate::source_identity::SourceIdentity::from_fields(
            dev, ino, size, mtime, mtime_nsec, ctime, ctime_nsec,
        );
        if source_identity.render_s1() != object_version {
            return Err(OperationError::State);
        }
        Ok(Self {
            path,
            object_version,
            sha256,
            source_identity,
        })
    }
}

impl MetadataContents {
    fn requested_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<MetadataPayload>()
            .checked_add(std::mem::size_of::<MetadataContents>())?
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(self.document.capacity())?
            .checked_add(self.facts.requested_heap_bytes()?)?
            .checked_add(self.source.path.capacity())?
            .checked_add(self.source.media_file_id.capacity())?
            .checked_add(self.source.object_version.capacity())?
            .checked_add(self.engine.media_digest.capacity())?
            .checked_add(self.engine.build_line.capacity())?
            .checked_add(
                self.engine
                    .dependencies
                    .len()
                    .checked_mul(std::mem::size_of::<(PathBuf, String)>())?,
            )?
            .checked_add(2 * std::mem::size_of::<usize>())?;
        for executable in [&self.engine.producer, &self.engine.reporter] {
            bytes = bytes
                .checked_add(executable.path.capacity())?
                .checked_add(executable.object_version.capacity())?
                .checked_add(executable.sha256.capacity())?;
        }
        for (path, version) in self.engine.dependencies.iter() {
            bytes = bytes
                .checked_add(path.capacity())?
                .checked_add(version.capacity())?;
        }
        Some(bytes)
    }
}

impl PlanningMetadataRole {
    pub(in crate::adaptive_resources::runtime) fn install(
        &mut self,
        probe: crate::adaptive_resources::physical_graph::fixed_probe::FixedProbeReportData,
        engine: crate::adaptive_resources::physical_graph::fixed_engine::FixedEngineReportData,
        source: FixedPlanningSourceRequest,
    ) -> Result<FixedProbeMetadataLease, OperationError> {
        if self.issued || self.closing || self.payload.upgrade().is_some() {
            return Err(OperationError::State);
        }
        let (document, facts) = probe.output.map_err(OperationError::Planning)?;
        let engine = FixedPlanningEngineAttestation {
            producer: FixedPlanningExecutable::from_observed(
                engine.producer_path,
                engine.producer_object_version,
                engine.producer_sha256,
            )?,
            reporter: FixedPlanningExecutable::from_observed(
                probe.executable,
                probe.executable_object_version,
                probe.executable_sha256,
            )?,
            dependencies: engine.dependencies.into(),
            media_digest: engine.media_digest,
            build_line: engine.build_line,
            boot_id: engine.boot_id,
        };
        let contents = MetadataContents {
            document,
            facts,
            engine,
            source: FixedObservedSource {
                path: source.path,
                media_file_id: source.media_file_id,
                object_version: probe.source_object_version,
                scanner_size: source.scanner_size,
                scanner_mtime: source.scanner_mtime,
            },
        };
        let capacity = self
            .plan
            .retained_memory_bytes
            .checked_sub(
                self.release
                    .application
                    .application_limit_bytes()
                    .ok_or(OperationError::State)?,
            )
            .and_then(|bytes| bytes.checked_sub(self.book_structural_bytes))
            .ok_or(OperationError::State)?;
        if u64::try_from(contents.requested_bytes().ok_or(OperationError::State)?)
            .map_err(|_| OperationError::State)?
            > capacity
        {
            return Err(OperationError::State);
        }
        let guard = self
            .release
            .application
            .new_guard()
            .ok_or(OperationError::State)?;
        let payload = Arc::new(MetadataPayload {
            contents: Some(contents),
            release: Arc::clone(&self.release),
            _guard: guard,
        });
        self.payload = Arc::downgrade(&payload);
        self.issued = true;
        Ok(FixedProbeMetadataLease { payload })
    }
}

/// Original selector, registered by the application before the first await.
/// Drop requests cleanup from the existing supervisor and never refunds.
pub struct FixedPlanningProbeOwner {
    runtime: Arc<NodeResourceRuntime>,
    preparation: Option<OwnedPreparation>,
    release: Arc<MetadataReleaseState>,
    cleanup_requested: Arc<AtomicBool>,
    known_closed: Arc<AtomicBool>,
}

/// Compact selector for the SAME retained operations. Its status becomes true
/// only after both journal roles and the actual rich bundle have left.
pub struct FixedPlanningProbeCleanup {
    runtime: Arc<NodeResourceRuntime>,
    preparation: Option<OwnedPreparation>,
    known_closed: Arc<AtomicBool>,
}
impl FixedPlanningProbeCleanup {
    pub fn known_closed(&self) -> bool {
        self.known_closed.load(Ordering::Acquire)
    }
    /// Seal a failed attempt's native/report payload before its logical Store
    /// Close. The compact Close record and original controller future remain
    /// charged by M's application lease until that logical operation is known
    /// and those allocations actually disappear. This cannot refund M.
    pub async fn seal_for_logical_close(
        &mut self,
        deadline: Instant,
    ) -> Result<(), FixedPlanningProbeError> {
        if self.known_closed() {
            return Ok(());
        }
        let owner = self.preparation.as_ref().ok_or(OperationError::State)?;
        self.runtime.finish_planning_native(owner, deadline).await?;
        self.runtime.seal_planning_payload(owner).await?;
        Ok(())
    }

    pub async fn finish_retirement(
        &mut self,
        deadline: Instant,
    ) -> Result<(), FixedPlanningProbeError> {
        let Some(owner) = self.preparation.as_ref() else {
            return Ok(());
        };
        self.runtime.finish_planning_native(owner, deadline).await?;
        self.runtime
            .queue_planning_metadata_close(owner, deadline)
            .await?;
        self.runtime
            .finish_planning_metadata_close(&mut self.preparation, 1, deadline)
            .await?;
        self.known_closed.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for FixedPlanningProbeOwner {
    fn drop(&mut self) {
        self.cleanup_requested.store(true, Ordering::Release);
    }
}
#[cfg(any(test, feature = "fixtures"))]
pub use crate::process::fixed_planning_engine::fixtures::{
    FixedPlanningNativeGate, FixedPlanningNativeStarted,
};

impl FixedPlanningProbeOwner {
    /// Fixture-only observation of this SAME original engine command. This
    /// returns no native owner, liveness guarantee, receipt or release right.
    #[cfg(any(test, feature = "fixtures"))]
    pub async fn fixture_native_gate(
        &self,
        panic_after_native_return: bool,
    ) -> Result<FixedPlanningNativeGate, FixedPlanningProbeError> {
        self.runtime
            .fixture_planning_native_gate(self.selection()?, panic_after_native_return)
            .await
            .map_err(Into::into)
    }
    fn selection(&self) -> Result<&OwnedPreparation, FixedPlanningProbeError> {
        self.preparation
            .as_ref()
            .ok_or_else(|| OperationError::State.into())
    }
    pub fn take_application_lease(
        &self,
        bytes: usize,
    ) -> Result<super::finite_lifetime::FiniteApplicationLease, FixedPlanningProbeError> {
        self.release
            .application
            .take_application_lease(bytes)
            .ok_or_else(|| OperationError::State.into())
    }
    pub async fn prepare_inputs(
        &mut self,
        deadline: Instant,
    ) -> Result<(), FixedPlanningProbeError> {
        self.runtime
            .prepare_planning_inputs(self.selection()?, deadline)
            .await
            .map_err(Into::into)
    }
    pub async fn collect_and_refine(
        &mut self,
        deadline: Instant,
    ) -> Result<FixedProbeMetadataLease, FixedPlanningProbeError> {
        self.runtime
            .collect_planning_tasks(self.selection()?, deadline)
            .await
            .map_err(Into::into)
    }
    pub async fn finish_native(
        &mut self,
        deadline: Instant,
    ) -> Result<(), FixedPlanningProbeError> {
        self.runtime
            .finish_planning_native(self.selection()?, deadline)
            .await
            .map_err(Into::into)
    }
    pub fn into_cleanup(mut self) -> FixedPlanningProbeCleanup {
        self.cleanup_requested.store(true, Ordering::Release);
        FixedPlanningProbeCleanup {
            runtime: Arc::clone(&self.runtime),
            preparation: self.preparation.take(),
            known_closed: Arc::clone(&self.known_closed),
        }
    }
}
impl NodeResourceRuntime {
    /// NEGATIVE fixture ceiling only, after the exclusive fixture's sole real
    /// observation has returned and immediately before synchronous planning
    /// admission. No standalone sampler/concurrent refresher may exist in
    /// this window. This is neither measured capacity nor an operator policy;
    /// it cannot authorize work, native absence, closure, refund or reset.
    #[cfg(all(target_os = "linux", any(test, feature = "fixtures")))]
    pub fn fixture_deny_planning_cpu(&self) -> Result<(), FixedPlanningProbeError> {
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        let next_epoch = state.epoch.checked_add(1).ok_or(OperationError::State)?;
        state.cap.cpu_ns_per_second = crate::process::observation::Metric::Known(0);
        state.epoch = next_epoch;
        // Preserve other ceilings, all held charges and the actual observation
        // timestamp. The next plan reads this fresh epoch under the same ledger.
        Ok(())
    }
}

impl NodeResourceRuntime {
    pub fn prepare_fixed_planning_probe(
        self: &Arc<Self>,
        request: FixedPlanningProbeRequest,
        deadline: Instant,
    ) -> Result<FixedPlanningProbeOwner, FixedPlanningProbeError> {
        let owner = plan::prepare(self, request, deadline)?;
        #[cfg(test)]
        self.planning_constructor_checkpoint(
            owner
                .preparation
                .as_ref()
                .expect("complete private planning owner"),
        );
        // All original payload/fences were installed privately. No fallible
        // registration or book lookup separates publication from this owner.
        Ok(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heap_fixture() -> (
        Arc<MetadataReleaseState>,
        FixedProbeMetadataLease,
        super::super::finite_lifetime::FiniteApplicationLease,
    ) {
        // Heap-lifetime fixture only: no journal/native receipt is created.
        let release = MetadataReleaseState::new(
            super::super::ReservationBinding {
                ledger: 1,
                reservation: 2,
                allocation_epoch: 3,
            },
            64,
        );
        let application = release
            .application
            .take_application_lease(32)
            .expect("one coordinator");
        let guard = release
            .application
            .new_guard()
            .expect("original report guard");
        let report = FixedProbeMetadataLease {
            payload: Arc::new(MetadataPayload {
                contents: None,
                release: Arc::clone(&release),
                _guard: guard,
            }),
        };
        assert!(release.application.seal());
        (release, report, application)
    }

    #[test]
    fn metadata_close_waits_for_last_report_alias_and_coordinator() {
        let (release, report, application) = heap_fixture();
        let alias = report.clone();
        drop(report);
        assert!(release.take_close().is_err());
        drop(alias);
        assert!(
            release.take_close().is_err(),
            "coordinator still owns its charged frame"
        );
        drop(application);
        assert!(release.take_close().is_ok());
        assert!(
            release.take_close().is_err(),
            "original close capability is one-shot"
        );
    }

    #[test]
    fn coordinator_completion_does_not_release_live_report_metadata() {
        let (release, report, application) = heap_fixture();
        drop(application);
        assert!(
            release.take_close().is_err(),
            "report alias still owns its payload"
        );
        drop(report);
        assert!(release.take_close().is_ok());
    }
}
