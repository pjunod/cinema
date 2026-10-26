//! Actual admitted Source probe ownership. Probe bytes are evidence, not
//! route authority; no caller or wire value can construct this operation.
use plurx_core::{
    domain::MediaFile,
    sharing_source_sessions::{SourceDispatchAssignment, SourceSessionWriteAuthority},
    store::Store,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::Notify,
};

trait SourceProbeHooks: std::any::Any + Send + Sync {
    fn before_spawn(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn after_spawn(&self, pid: u32) -> crate::seam_hooks::HookFuture<'_>;
    fn before_reap_retry(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn after_evidence(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn inject_wait_failure(&self) -> bool;
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}
struct NoSourceProbeHooks;
impl SourceProbeHooks for NoSourceProbeHooks {
    fn before_spawn(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn after_spawn(&self, _: u32) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn before_reap_retry(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn after_evidence(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn inject_wait_failure(&self) -> bool {
        false
    }
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
static NO_SOURCE_INDEX_HOOKS: NoSourceProbeHooks = NoSourceProbeHooks;
/// Per actual manager, with identical no-op hook fields/awaits in production.
pub(crate) struct SourceProbeHookOwner {
    slot: crate::seam_hooks::HookSlot<dyn SourceProbeHooks>,
}
impl Default for SourceProbeHookOwner {
    fn default() -> Self {
        Self {
            slot: crate::seam_hooks::HookSlot::new(&NO_SOURCE_INDEX_HOOKS),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
struct PausingSourceProbeHooks {
    before: crate::seam_hooks::PauseSlot,
    after: crate::seam_hooks::PauseSlot,
    retry: crate::seam_hooks::PauseSlot,
    evidence: crate::seam_hooks::PauseSlot,
    failed_wait: std::sync::atomic::AtomicBool,
    spawned_pid: std::sync::atomic::AtomicU32,
    closed_parents: std::sync::atomic::AtomicUsize,
    open_parent_at_settlement: std::sync::atomic::AtomicBool,
    #[cfg(target_os = "linux")]
    fixed_mode: std::sync::atomic::AtomicU8,
    #[cfg(target_os = "linux")]
    fixed_before_inputs: AtomicBool,
    #[cfg(target_os = "linux")]
    fixed_gate: Mutex<
        Option<
            Arc<plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningNativeGate>,
        >,
    >,
    #[cfg(target_os = "linux")]
    fixed_changed: Notify,
    #[cfg(target_os = "linux")]
    fixed_cpu_denial: AtomicBool,
    #[cfg(target_os = "linux")]
    fixed_refusal: Mutex<Option<String>>,
}
#[cfg(test)]
impl SourceProbeHooks for PausingSourceProbeHooks {
    fn before_spawn(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.before.hold()
    }
    fn after_spawn(&self, pid: u32) -> crate::seam_hooks::HookFuture<'_> {
        self.spawned_pid
            .store(pid, std::sync::atomic::Ordering::Release);
        self.after.hold()
    }
    fn before_reap_retry(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.retry.hold()
    }
    fn after_evidence(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.evidence.hold()
    }
    fn inject_wait_failure(&self) -> bool {
        self.failed_wait
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
impl SourceProbeHookOwner {
    pub(crate) fn record_parent_closed(&self, _closed: bool) {
        #[cfg(all(test, unix))]
        {
            let hooks = self.test_hooks();
            hooks
                .open_parent_at_settlement
                .fetch_or(!_closed, Ordering::Release);
            hooks.closed_parents.fetch_add(1, Ordering::Release);
        }
    }
    pub(crate) async fn after_evidence(&self) {
        self.slot.get().after_evidence().await;
    }
}
#[cfg(test)]
impl SourceProbeHookOwner {
    fn test_hooks(&self) -> &PausingSourceProbeHooks {
        self.slot
            .get_or_install(|| Box::new(PausingSourceProbeHooks::default()))
            .as_any()
            .downcast_ref()
            .expect("Source probe test hooks")
    }
    pub(crate) fn pause_before_spawn(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks().before.arm("source_probe_before_spawn")
    }
    pub(crate) fn pause_after_spawn(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks().after.arm("source_probe_after_spawn")
    }
    pub(crate) fn pause_after_wait_failure(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks()
            .failed_wait
            .store(true, std::sync::atomic::Ordering::Release);
        self.test_hooks().retry.arm("source_probe_reap_retry")
    }
    pub(crate) fn pause_after_evidence(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks()
            .evidence
            .arm("source_probe_after_evidence")
    }
    pub(crate) fn spawned_pid(&self) -> u32 {
        self.test_hooks()
            .spawned_pid
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Fixture handle releases the SAME original native gate on assertion failure,
/// even while the manager's diagnostic slot still retains a cheap Arc alias.
#[cfg(all(test, target_os = "linux"))]
pub(crate) struct SourceFixedNativeGate(
    Arc<plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningNativeGate>,
);
#[cfg(all(test, target_os = "linux"))]
impl SourceFixedNativeGate {
    pub(crate) async fn started(
        &self,
    ) -> plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningNativeStarted {
        self.0.started().await
    }
    pub(crate) fn release(&self) {
        self.0.release();
    }
}
#[cfg(all(test, target_os = "linux"))]
impl Drop for SourceFixedNativeGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[cfg(all(test, target_os = "linux"))]
impl SourceProbeHookOwner {
    pub(crate) fn arm_fixed_cpu_denial(&self) {
        self.test_hooks()
            .fixed_cpu_denial
            .store(true, Ordering::Release);
    }
    pub(crate) fn deny_fixed_cpu_if_armed(
        &self,
        runtime: &plurx_core::adaptive_resources::runtime::NodeResourceRuntime,
    ) -> Result<(), String> {
        if self.test_hooks().fixed_cpu_denial.load(Ordering::Acquire) {
            // Only the exclusive, fresh runtime in Source fixture19 arms
            // this negative ceiling. No standalone sampler exists there.
            runtime
                .fixture_deny_planning_cpu()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub(crate) fn record_fixed_refusal(&self, error: &str) {
        if self.test_hooks().fixed_cpu_denial.load(Ordering::Acquire) {
            *self
                .test_hooks()
                .fixed_refusal
                .lock()
                .expect("actual fixture refusal") = Some(error.to_owned());
        }
    }
    pub(crate) fn assert_fixed_cpu_refusal(&self) {
        let refusal = self
            .test_hooks()
            .fixed_refusal
            .lock()
            .expect("actual fixture refusal");
        let refusal = refusal
            .as_deref()
            .expect("actual synchronous P refusal was observed");
        assert!(
            refusal.contains("HardLimit(Cpu)"),
            "actual refusal must be CPU budget, got: {refusal}"
        );
        assert!(
            !refusal.contains("StaleEpoch"),
            "a stale epoch is not CPU refusal"
        );
    }
    pub(crate) fn arm_fixed_native(&self, panic_after_native_return: bool) {
        self.test_hooks().fixed_mode.store(
            if panic_after_native_return { 2 } else { 1 },
            Ordering::Release,
        );
    }
    pub(crate) fn pause_before_fixed_inputs(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks()
            .fixed_before_inputs
            .store(true, Ordering::Release);
        self.pause_before_spawn()
    }
    pub(crate) async fn before_fixed_inputs(&self) {
        if self
            .test_hooks()
            .fixed_before_inputs
            .load(Ordering::Acquire)
        {
            self.slot.get().before_spawn().await;
        }
    }
    pub(crate) async fn before_fixed_collect(&self) {
        if !self
            .test_hooks()
            .fixed_before_inputs
            .load(Ordering::Acquire)
        {
            self.slot.get().before_spawn().await;
        }
    }
    pub(crate) async fn install_fixed_native(&self, operation: &SourceProbeOperation) {
        let hooks = self.test_hooks();
        let mode = hooks.fixed_mode.load(Ordering::Acquire);
        if mode == 0
            || hooks
                .fixed_gate
                .lock()
                .expect("fixture gate slot")
                .is_some()
        {
            return;
        }
        let SourceProbeState::Fixed(state) = &operation.state else {
            panic!("original fixed Source state")
        };
        let gate = state
            .work
            .lock()
            .await
            .owner
            .fixture_native_gate(mode == 2)
            .await
            .expect("same original P native gate");
        *hooks.fixed_gate.lock().expect("fixture gate slot") = Some(Arc::new(gate));
        hooks.fixed_changed.notify_waiters();
    }
    pub(crate) async fn fixed_native_gate(&self) -> SourceFixedNativeGate {
        let hooks = self.test_hooks();
        loop {
            let changed = hooks.fixed_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(gate) = hooks.fixed_gate.lock().expect("fixture gate slot").as_ref() {
                return SourceFixedNativeGate(Arc::clone(gate));
            }
            changed.await;
        }
    }
}

/// Close the worker's final parent descriptor before publishing a settlement.
/// Child and pipe joins have already completed; returned evidence is data only.
pub(crate) fn close_source_before_settlement(
    source: crate::fragment_index_cluster::SourceFence,
    record: impl FnOnce(bool),
) {
    #[cfg(all(test, unix))]
    let fd = {
        use std::os::fd::AsRawFd;
        source.handle.as_raw_fd()
    };
    drop(source);
    #[cfg(all(test, unix))]
    let closed = unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1;
    #[cfg(not(all(test, unix)))]
    let closed = true;
    record(closed);
}

#[cfg(all(test, unix))]
impl SourceProbeHookOwner {
    pub(crate) fn assert_closed_parent_settlements(&self) {
        let hooks = self.test_hooks();
        assert!(
            hooks.closed_parents.load(Ordering::Acquire) > 0,
            "actual Source preparation must have closed its parent descriptor"
        );
        assert!(
            !hooks.open_parent_at_settlement.load(Ordering::Acquire),
            "Source settlement was published with its parent descriptor open"
        );
    }
}

/// The byte bound of a Source burn sidecar: the same as Local's burn cache.
pub(crate) const SOURCE_BURN_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;

/// What a burned Source recipe asks this preparation to own: the selected
/// embedded subtitle ordinal, whether it is a bitmap track, and the runtime
/// directory a text burn's frozen Fontconfig environment and the anonymous
/// sidecar live under. Built only from the Source's own prepared request and
/// scanned file facts; no wire value names a path.
pub(crate) struct SourceBurnAsk {
    pub(crate) subtitle_index: i64,
    pub(crate) bitmap: bool,
    pub(crate) runtime_dir: std::path::PathBuf,
}

/// Every artifact a burned Source recipe reads besides the source itself,
/// made by this operation's own children. The sidecar is an anonymous file:
/// no name exists to serve, leak or sweep, and its last descriptor's close
/// (the rendition's, at settlement) is the cleanup. A text burn's fonts are
/// the sidecar's attachments plus the engine's frozen, reference-counted
/// Fontconfig environment.
pub(crate) struct SourceBurnArtifacts {
    subtitle_index: i64,
    bitmap: bool,
    sidecar: std::fs::File,
    filters: crate::pipeprobe::BurnFilters,
}
impl SourceBurnArtifacts {
    pub(crate) fn subtitle_index(&self) -> i64 {
        self.subtitle_index
    }
    pub(crate) fn bitmap(&self) -> bool {
        self.bitmap
    }
    pub(crate) fn filters(&self) -> crate::pipeprobe::BurnFilters {
        self.filters
    }
    /// A second descriptor for the recipe. The evidence's own closes with it.
    pub(crate) fn sidecar(&self) -> std::io::Result<std::fs::File> {
        self.sidecar.try_clone()
    }
}

/// Read timing only from this operation's held-source document. Stored scan
/// normalization intentionally excludes reporter-dependent start timestamps.
pub(crate) fn held_source_clock(
    document: &str,
) -> Result<plurx_core::transcode::VodSourceClock, String> {
    let document: serde_json::Value = serde_json::from_str(document)
        .map_err(|_| "held source clock document is malformed".to_owned())?;
    let format = document
        .get("format")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "held source clock format is missing".to_owned())?;
    let start = match format.get("start_time") {
        None => None,
        Some(serde_json::Value::String(value)) => Some(value.as_str()),
        Some(_) => return Err("held source origin is not a reporter timestamp".to_owned()),
    };
    plurx_core::transcode::VodSourceClock::from_probe_start_time(start).map_err(str::to_owned)
}

/// Exact selected-video origin from this held probe, bound to the FORMAT
/// clock already frozen for soundtrack production. Never infer it from the
/// first picture returned by a window seek.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn held_dv_source_origin(
    stream: &serde_json::Value,
    tick: (u32, u32),
    format: plurx_core::transcode::VodSourceClock,
) -> Result<plurx_core::transcode::dv_processing::DvSourceOrigin, String> {
    use plurx_core::transcode::dv_processing::{DvSourceOrigin, DvTimestamp};
    if tick.0 == 0 || tick.1 == 0 {
        return Err("DV held source has invalid time base".into());
    }
    let ticks = stream
        .get("start_pts")
        .and_then(serde_json::Value::as_i64)
        .ok_or("DV selected-video source origin is unobserved")?;
    let origin = DvTimestamp::new(
        ticks
            .checked_mul(i64::from(tick.0))
            .ok_or("DV source origin overflow")?,
        tick.1,
    )
    .map_err(|e| e.to_string())?;
    DvSourceOrigin::observed_common(origin, format).map_err(|e| e.to_string())
}

pub(crate) struct SourceHeldProbeEvidence {
    assignment: SourceDispatchAssignment,
    object_version: String,
    document: String,
    source_clock: plurx_core::transcode::VodSourceClock,
    clock_reporter: crate::ffmpeg::EncodedExecutable,
    engine: crate::ffmpeg::EncodedEngine,
    build: String,
    burn: Option<SourceBurnArtifacts>,
}
impl SourceHeldProbeEvidence {
    pub(crate) fn clock_reporter(&self) -> &crate::ffmpeg::EncodedExecutable {
        &self.clock_reporter
    }
    pub(crate) fn source_clock(&self) -> plurx_core::transcode::VodSourceClock {
        self.source_clock
    }
    pub(crate) fn burn(&self) -> Option<&SourceBurnArtifacts> {
        self.burn.as_ref()
    }
    pub(crate) fn matches(
        &self,
        assignment: &SourceDispatchAssignment,
        object_version: &str,
    ) -> bool {
        self.assignment.same_identity(assignment) && self.object_version == object_version
    }
    pub(crate) fn engine(&self) -> crate::ffmpeg::EncodedEngine {
        self.engine.clone()
    }
    pub(crate) fn build(&self) -> &str {
        &self.build
    }
    pub(crate) fn document(&self) -> &str {
        &self.document
    }
}
pub(crate) struct SourceProbeSettlement {
    assignment: SourceDispatchAssignment,
}
impl SourceProbeSettlement {
    pub(crate) fn matches(&self, assignment: &SourceDispatchAssignment) -> bool {
        self.assignment.same_identity(assignment)
    }
}
struct ProbeState {
    permit: Mutex<Option<crate::vodencode::EncodePermit>>,
    result: Mutex<Option<Result<SourceHeldProbeEvidence, String>>>,
    settled: AtomicBool,
    changed: Notify,
}
// The selected software Source route borrows this SAME retained owner on
// replay. No daemon task, descriptor or reconstructed cleanup authority is
// introduced; core retains its original opener, native tasks and transitions.
#[cfg(target_os = "linux")]
struct FixedSourceWork {
    owner: plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeOwner,
    report:
        Option<plurx_core::adaptive_resources::runtime::planning_probe::FixedProbeMetadataLease>,
    native_closed: bool,
}
#[cfg(target_os = "linux")]
struct FixedSourceState {
    work: tokio::sync::Mutex<FixedSourceWork>,
    snapshot: Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    recipe_binding: String,
    // All original application payloads above drop before this last fence.
    // It signals lifetime only; the original core supervisor settles M.
    _application: plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease,
}
/// A failed constructor may already have reserved P/M without returning its
/// selector. Missing owner is uncertainty, never never-acquired evidence.
#[cfg(target_os = "linux")]
struct UncertainSourcePlanning {
    snapshot: Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    reason: String,
    // If an owner was returned before application retention failed, preserve
    // that SAME selector. Drop requests cleanup only after rich fields above.
    _original:
        Option<plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeOwner>,
}
#[cfg(target_os = "linux")]
pub(crate) struct SourceFixedStartFailure {
    reason: String,
    held: Option<SourceProbeOperation>,
}
#[cfg(target_os = "linux")]
impl std::fmt::Debug for SourceFixedStartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceFixedStartFailure")
            .field("reason", &self.reason)
            .field("uncertainty_held", &self.held.is_some())
            .finish()
    }
}
#[cfg(target_os = "linux")]
impl SourceFixedStartFailure {
    pub(crate) fn into_parts(self) -> (String, Option<SourceProbeOperation>) {
        (self.reason, self.held)
    }
    fn before_call(reason: &str) -> Self {
        Self {
            reason: reason.to_owned(),
            held: None,
        }
    }
    fn held(
        reason: String,
        assignment: SourceDispatchAssignment,
        snapshot: Arc<plurx_core::store::PlaybackPlanningSnapshot>,
        original: Option<
            plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeOwner,
        >,
    ) -> Self {
        let held = SourceProbeOperation {
            assignment,
            cancel: tokio_util::sync::CancellationToken::new(),
            state: SourceProbeState::PlanningUncertain(UncertainSourcePlanning {
                snapshot,
                reason: reason.clone(),
                _original: original,
            }),
        };
        Self {
            reason,
            held: Some(held),
        }
    }
}
enum SourceProbeState {
    Legacy(Arc<ProbeState>),
    #[cfg(target_os = "linux")]
    Fixed(Arc<FixedSourceState>),
    #[cfg(target_os = "linux")]
    PlanningUncertain(UncertainSourcePlanning),
}
pub(crate) struct SourceProbeOperation {
    assignment: SourceDispatchAssignment,
    cancel: tokio_util::sync::CancellationToken,
    // Last: assignment/token allocations disappear before M's application
    // fence in the fixed state can signal final release.
    state: SourceProbeState,
}
impl SourceProbeOperation {
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(crate) async fn settle(&self) -> SourceProbeSettlement {
        #[cfg(not(target_os = "linux"))]
        let SourceProbeState::Legacy(state) = &self.state;
        #[cfg(target_os = "linux")]
        let state = match &self.state {
            SourceProbeState::Legacy(state) => state,
            #[cfg(target_os = "linux")]
            SourceProbeState::PlanningUncertain(held) => {
                tracing::warn!(target: "plurxd::transcode", reason = %held.reason, "Source planning constructor uncertainty retains the original Source assignment");
                // No returned selector/Err/status can authorize Source SQL
                // release. No native or NeverAcquired fact is reconstructed.
                return std::future::pending::<SourceProbeSettlement>().await;
            }
            #[cfg(target_os = "linux")]
            SourceProbeState::Fixed(_) => {
                // Cancellation is not a close receipt. Keep the exact P
                // selector and M while its original closer remains pending.
                let mut warned = false;
                loop {
                    match self
                        .finish_fixed_native(Instant::now() + Duration::from_secs(30))
                        .await
                    {
                        Ok(()) => {
                            return SourceProbeSettlement {
                                assignment: self.assignment.clone(),
                            }
                        }
                        Err(error) => {
                            if !warned {
                                tracing::warn!(target: "plurxd::transcode", %error, "Source original planning cleanup remains held");
                                warned = true;
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }
        };
        loop {
            let changed = state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if state.settled.load(Ordering::Acquire) {
                return SourceProbeSettlement {
                    assignment: self.assignment.clone(),
                };
            }
            changed.await;
        }
    }
    pub(crate) async fn outcome(&self) -> Result<SourceHeldProbeEvidence, String> {
        #[cfg(not(target_os = "linux"))]
        let SourceProbeState::Legacy(state) = &self.state;
        #[cfg(target_os = "linux")]
        let state = match &self.state {
            SourceProbeState::Legacy(state) => state,
            #[cfg(target_os = "linux")]
            SourceProbeState::Fixed(_) | SourceProbeState::PlanningUncertain(_) => {
                return Err("fixed Source planning requires its admitted metadata lease".into())
            }
        };
        let _receipt = self.settle().await;
        state
            .result
            .lock()
            .map_err(|_| "Source probe result poisoned".to_owned())?
            .take()
            .ok_or_else(|| "Source probe result already consumed".to_owned())?
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn uses_fixed_planning(&self) -> bool {
        matches!(
            self.state,
            SourceProbeState::Fixed(_) | SourceProbeState::PlanningUncertain(_)
        )
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn fixed_matches(
        &self,
        assignment: &SourceDispatchAssignment,
        fingerprint: &str,
    ) -> bool {
        matches!(&self.state, SourceProbeState::Fixed(state) if self.assignment.same_identity(assignment) && state.recipe_binding == fingerprint)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn fixed_snapshot(
        &self,
    ) -> Result<&Arc<plurx_core::store::PlaybackPlanningSnapshot>, String> {
        match &self.state {
            SourceProbeState::Fixed(state) => Ok(&state.snapshot),
            SourceProbeState::PlanningUncertain(held) => Ok(&held.snapshot),
            SourceProbeState::Legacy(_) => {
                Err("Source original fixed planning owner is absent".into())
            }
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) async fn prepare_fixed_inputs(&self, deadline: Instant) -> Result<(), String> {
        let SourceProbeState::Fixed(state) = &self.state else {
            return Err("Source original fixed planning owner is absent".into());
        };
        if self.cancel.is_cancelled() {
            return Err("Source original planning was cancelled".into());
        }
        let mut work = tokio::time::timeout_at(deadline.into(), state.work.lock())
            .await
            .map_err(|_| "Source planning owner deadline")?;
        if work.report.is_some() {
            return Ok(());
        }
        if work.native_closed {
            return Err("Source original planning is already closed".into());
        }
        self.assignment
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| "Source original planning observation expired")?;
        work.owner
            .prepare_inputs(deadline)
            .await
            .map_err(|error| format!("preparing Source original planning inputs: {error}"))
    }
    #[cfg(target_os = "linux")]
    pub(crate) async fn fixed_report(
        &self,
        deadline: Instant,
    ) -> Result<
        plurx_core::adaptive_resources::runtime::planning_probe::FixedProbeMetadataLease,
        String,
    > {
        let SourceProbeState::Fixed(state) = &self.state else {
            return Err("Source original fixed planning owner is absent".into());
        };
        if self.cancel.is_cancelled() {
            return Err("Source original planning was cancelled".into());
        }
        let mut work = tokio::time::timeout_at(deadline.into(), state.work.lock())
            .await
            .map_err(|_| "Source planning owner deadline")?;
        if let Some(report) = &work.report {
            return Ok(report.clone());
        }
        self.assignment
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| "Source original planning observation expired")?;
        let report = work
            .owner
            .collect_and_refine(deadline)
            .await
            .map_err(|error| format!("collecting Source original planning report: {error}"))?;
        // No await between receiving the original report and retaining it.
        // Cancelled/repeated callers share M; they cannot enter another P.
        work.report = Some(report.clone());
        Ok(report)
    }
    #[cfg(target_os = "linux")]
    pub(crate) async fn finish_fixed_native(&self, deadline: Instant) -> Result<(), String> {
        let SourceProbeState::Fixed(state) = &self.state else {
            return Err("Source original fixed planning owner is absent".into());
        };
        let mut work = tokio::time::timeout_at(deadline.into(), state.work.lock())
            .await
            .map_err(|_| "Source original planning close deadline")?;
        if !work.native_closed {
            work.owner.finish_native(deadline).await.map_err(|error| {
                format!("closing Source original planning native owner: {error}")
            })?;
            work.native_closed = true;
        }
        Ok(())
    }
}

/// Synchronous installation before any P/native await. The application lease
/// is the original M partition, not a second admission or a native receipt.
#[expect(
    clippy::result_large_err,
    reason = "Retain the original Source assignment and move-only P selector inline on construction failure; boxing would allocate when application admission failed. The admitted coordinator frame includes this inline storage, and uncertainty keeps the original charge held."
)]
#[cfg(target_os = "linux")]
pub(crate) fn start_source_fixed_probe(
    runtime: &Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>,
    mut request: plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeRequest,
    snapshot: Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    assignment: SourceDispatchAssignment,
    deadline: Instant,
) -> Result<SourceProbeOperation, SourceFixedStartFailure> {
    let recipe_binding = request.recipe_source_binding.clone();
    // Count the capacities of the actual moved assignment and the retained
    // replay binding, including a refreshed membership snapshot's real size.
    request.application_metadata_bytes = request
        .application_metadata_bytes
        .checked_add(assignment.requested_heap_bytes().ok_or_else(|| {
            SourceFixedStartFailure::before_call("Source assignment capacity overflow")
        })?)
        .and_then(|bytes| bytes.checked_add(recipe_binding.capacity()))
        .ok_or_else(|| {
            SourceFixedStartFailure::before_call("Source original replay capacity overflow")
        })?;
    let requested_bytes = request.application_metadata_bytes;
    let owner = match runtime.prepare_fixed_planning_probe(request, deadline) {
        Ok(owner) => owner,
        Err(error) => {
            let definite = error.definitely_before_reservation();
            let reason = format!("admitting Source original planning: {error}");
            return Err(if definite {
                SourceFixedStartFailure { reason, held: None }
            } else {
                SourceFixedStartFailure::held(reason, assignment, snapshot, None)
            });
        }
    };
    let application = match owner.take_application_lease(requested_bytes) {
        Ok(application) => application,
        Err(error) => {
            return Err(SourceFixedStartFailure::held(
                format!("retaining Source original planning application: {error}"),
                assignment,
                snapshot,
                Some(owner),
            ))
        }
    };
    Ok(SourceProbeOperation {
        assignment,
        state: SourceProbeState::Fixed(Arc::new(FixedSourceState {
            work: tokio::sync::Mutex::new(FixedSourceWork {
                owner,
                report: None,
                native_closed: false,
            }),
            snapshot,
            recipe_binding,
            _application: application,
        })),
        cancel: tokio_util::sync::CancellationToken::new(),
    })
}

/// Requested layouts introduced by the original Source carrier, separate
/// from the snapshot and the checked P-to-R recipe allowance. The synchronous
/// move-only failure return has its own compiler-measured storage component;
/// it need not survive an await to occupy the caller's stack. Tokio/native,
/// private-stack and allocator peaks remain Unknown, not an RSS upper bound.
#[cfg(target_os = "linux")]
pub(crate) fn fixed_source_requested_bytes() -> usize {
    std::mem::size_of::<FixedSourceState>()
        + 2 * std::mem::size_of::<usize>()
        + std::mem::size_of::<SourceProbeOperation>()
        + std::mem::size_of::<SourceFixedStartFailure>()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_source_probe(
    file: MediaFile,
    source: crate::fragment_index_cluster::SourceFence,
    proof: Box<SourceSessionWriteAuthority>,
    permit: crate::vodencode::EncodePermit,
    store: Arc<dyn Store>,
    deadline: Instant,
    hooks: Arc<SourceProbeHookOwner>,
    burn: Option<SourceBurnAsk>,
) -> SourceProbeOperation {
    let assignment = proof.assignment().clone();
    let state = Arc::new(ProbeState {
        permit: Mutex::new(Some(permit)),
        result: Mutex::new(None),
        settled: AtomicBool::new(false),
        changed: Notify::new(),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let owned = Arc::clone(&state);
    let owned_cancel = cancel.clone();
    tokio::spawn(async move {
        let result = run_probe(
            &file,
            &source,
            &proof,
            store.as_ref(),
            deadline,
            &owned_cancel,
            &hooks,
            burn.as_ref(),
        )
        .await;
        *owned.result.lock().expect("Source probe result") = Some(result);
        close_source_before_settlement(source, |closed| hooks.record_parent_closed(closed));
        // Only confirmed child/job and pipe settlement lets this permit drop.
        drop(owned.permit.lock().expect("Source probe permit").take());
        owned.settled.store(true, Ordering::Release);
        owned.changed.notify_waiters();
    });
    SourceProbeOperation {
        assignment,
        state: SourceProbeState::Legacy(state),
        cancel,
    }
}

/// Copy a child's stdout into an owned file, refusing past `cap`. The file is
/// handed back so the caller keeps the only descriptors.
async fn bounded_into(
    input: impl AsyncRead + Unpin,
    cap: usize,
    mut sink: tokio::fs::File,
) -> Result<tokio::fs::File, String> {
    use tokio::io::AsyncWriteExt;
    let copied = tokio::io::copy(&mut input.take(cap as u64 + 1), &mut sink)
        .await
        .map_err(|e| e.to_string())?;
    if copied > cap as u64 {
        return Err("Source child output exceeded its byte bound".into());
    }
    sink.flush().await.map_err(|e| e.to_string())?;
    Ok(sink)
}

async fn bounded(input: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    input
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > cap {
        return Err("Source probe exceeded its byte bound".into());
    }
    Ok(bytes)
}
pub(crate) struct SourceCommandExecutor<'a> {
    file: &'a MediaFile,
    source: &'a crate::fragment_index_cluster::SourceFence,
    proof: &'a SourceSessionWriteAuthority,
    store: &'a dyn Store,
    deadline: Instant,
    cancel: &'a tokio_util::sync::CancellationToken,
    hooks: &'a SourceProbeHookOwner,
}
impl SourceCommandExecutor<'_> {
    pub(crate) async fn output(
        &self,
        command: tokio::process::Command,
        cap: usize,
    ) -> Result<crate::ffmpeg::BoundedOutput, String> {
        run_child(
            self.file,
            self.source,
            self.proof,
            self.store,
            self.deadline,
            self.cancel,
            command,
            cap,
            self.hooks,
        )
        .await
    }

    /// The same owned child, with stdout written into `sink` instead of
    /// memory. The sink comes back only after the child is reaped and both
    /// pipe tasks have joined.
    pub(crate) async fn output_into(
        &self,
        command: tokio::process::Command,
        cap: usize,
        sink: tokio::fs::File,
    ) -> Result<tokio::fs::File, String> {
        let (_, sink) = run_child_with(
            self.file,
            self.source,
            self.proof,
            self.store,
            self.deadline,
            self.cancel,
            command,
            cap,
            self.hooks,
            Some(sink),
        )
        .await?;
        sink.ok_or_else(|| "Source child sink missing".to_owned())
    }
}

/// Make every artifact a burned recipe reads, as this operation's own
/// children: the build's burn filters (proved, never assumed — an unreadable
/// listing refuses here, where Local's preflight lets a burn proceed) and the
/// subtitle-only sidecar, extracted from the held source into an anonymous
/// file under the Source runtime directory.
async fn prepare_burn(
    execution: &SourceCommandExecutor<'_>,
    ask: &SourceBurnAsk,
) -> Result<SourceBurnArtifacts, String> {
    use tokio::io::AsyncSeekExt;
    let mut listing = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    listing.args(["-hide_banner", "-filters"]);
    let output = execution.output(listing, 1024 * 1024).await?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let filters = crate::pipeprobe::burn_filters_from_listing(&text);
    if !filters.probed {
        return Err("Source burn filters could not be read".into());
    }
    if let Some(reason) = filters.refusal(ask.bitmap) {
        return Err(reason);
    }
    let directory = ask.runtime_dir.clone();
    let sidecar = tokio::task::spawn_blocking(move || tempfile::tempfile_in(directory))
        .await
        .map_err(|error| format!("Source burn sidecar task failed: {error}"))?
        .map_err(|error| format!("Source burn sidecar could not be created: {error}"))?;
    let command = crate::ffmpeg::source_burn_sidecar_command(
        &execution.source.handle,
        ask.subtitle_index,
        SOURCE_BURN_SIDECAR_BYTES,
    )?;
    let sink = tokio::fs::File::from_std(sidecar.try_clone().map_err(|e| e.to_string())?);
    let mut written = execution
        .output_into(command, SOURCE_BURN_SIDECAR_BYTES as usize, sink)
        .await?;
    let length = written
        .metadata()
        .await
        .map_err(|error| format!("Source burn sidecar: {error}"))?
        .len();
    if length == 0 {
        return Err("Source burn sidecar is empty".into());
    }
    // Both descriptors share one offset. Rewind it before the recipe digests
    // and hands the sidecar to a producer.
    written
        .rewind()
        .await
        .map_err(|error| format!("Source burn sidecar: {error}"))?;
    drop(written);
    Ok(SourceBurnArtifacts {
        subtitle_index: ask.subtitle_index,
        bitmap: ask.bitmap,
        sidecar,
        filters,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_probe(
    file: &MediaFile,
    source: &crate::fragment_index_cluster::SourceFence,
    proof: &SourceSessionWriteAuthority,
    store: &dyn Store,
    deadline: Instant,
    cancel: &tokio_util::sync::CancellationToken,
    hooks: &SourceProbeHookOwner,
    burn: Option<&SourceBurnAsk>,
) -> Result<SourceHeldProbeEvidence, String> {
    // The reporter query is physical work too. It runs under this same actual
    // permit/owner; no cold global reporter-cache call may spawn unowned work.
    let clock_reporter = tokio::time::timeout_at(
        deadline.into(),
        crate::ffmpeg::EncodedExecutable::capture_program(&crate::ffmpeg::ffprobe_bin()),
    )
    .await
    .map_err(|_| "Source clock reporter capture deadline".to_owned())??;
    let mut reporter = tokio::process::Command::new(&clock_reporter.path);
    reporter.arg("-version");
    let reporter = run_child(
        file,
        source,
        proof,
        store,
        deadline.min(Instant::now() + Duration::from_secs(5)),
        cancel,
        reporter,
        64 * 1024,
        hooks,
    )
    .await?;
    let reporter =
        String::from_utf8(reporter.stdout).map_err(|_| "Source reporter not UTF-8".to_owned())?;
    let reporter = reporter
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "Source FFprobe reporter identity missing".to_owned())?;
    let command = crate::ffmpeg::source_held_probe_command(&source.handle, &clock_reporter.path)?;
    let raw = run_child(
        file,
        source,
        proof,
        store,
        deadline,
        cancel,
        command,
        1024 * 1024,
        hooks,
    )
    .await?;
    let mut document: serde_json::Value = serde_json::from_slice(&raw.stdout)
        .map_err(|_| "Source probe JSON malformed".to_owned())?;
    plurx_core::scan::probe::stamp_reporter(&mut document, reporter);
    let document = serde_json::to_string(&document)
        .map_err(|_| "Source probe serialization failed".to_owned())?;
    let execution = SourceCommandExecutor {
        file,
        source,
        proof,
        store,
        deadline,
        cancel,
        hooks,
    };
    let artifacts = match burn {
        Some(ask) => Some(prepare_burn(&execution, ask).await?),
        None => None,
    };
    // A text burn freezes its Fontconfig environment as this operation's own
    // work; a bitmap burn and an unburned recipe render no fonts.
    let (engine, build) = crate::ffmpeg::EncodedEngine::capture_source(
        &execution,
        burn.filter(|ask| !ask.bitmap)
            .map(|ask| ask.runtime_dir.as_path()),
    )
    .await?;
    if !clock_reporter.is_current().await {
        return Err("Source clock reporter changed during its owned query".to_owned());
    }
    let source_clock = held_source_clock(&document)?;
    Ok(SourceHeldProbeEvidence {
        clock_reporter,
        source_clock,
        engine,
        build,
        assignment: proof.assignment().clone(),
        object_version: source.object_version().to_owned(),
        document,
        burn: artifacts,
    })
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_child(
    file: &MediaFile,
    source: &crate::fragment_index_cluster::SourceFence,
    proof: &SourceSessionWriteAuthority,
    store: &dyn Store,
    deadline: Instant,
    cancel: &tokio_util::sync::CancellationToken,
    command: tokio::process::Command,
    cap: usize,
    hooks: &SourceProbeHookOwner,
) -> Result<crate::ffmpeg::BoundedOutput, String> {
    run_child_with(
        file, source, proof, store, deadline, cancel, command, cap, hooks, None,
    )
    .await
    .map(|(output, _)| output)
}

#[allow(clippy::too_many_arguments)]
async fn run_child_with(
    file: &MediaFile,
    source: &crate::fragment_index_cluster::SourceFence,
    proof: &SourceSessionWriteAuthority,
    store: &dyn Store,
    deadline: Instant,
    cancel: &tokio_util::sync::CancellationToken,
    mut command: tokio::process::Command,
    cap: usize,
    hooks: &SourceProbeHookOwner,
    sink: Option<tokio::fs::File>,
) -> Result<(crate::ffmpeg::BoundedOutput, Option<tokio::fs::File>), String> {
    hooks.slot.get().before_spawn().await;
    let current = crate::fragment_index_cluster::open_source_playback_fence(
        file,
        Some(source.object_version()),
    )
    .await?;
    if Instant::now() >= deadline
        || cancel.is_cancelled()
        || !source.unchanged()
        || !current.unchanged()
    {
        return Err("Source probe cancelled or file changed before spawn".into());
    }
    proof
        .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
        .map_err(|_| "Source probe observation expired".to_owned())?;
    if !store
        .authorize_source_media_preparation(proof)
        .await
        .map_err(|_| "Source probe authority write failed".to_owned())?
    {
        return Err("Source probe current authority unavailable".into());
    }
    proof
        .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
        .map_err(|_| "Source probe observation expired at spawn".to_owned())?;
    if Instant::now() >= deadline
        || cancel.is_cancelled()
        || !source.unchanged()
        || !current.unchanged()
    {
        return Err("Source probe expired at spawn".into());
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _job) =
        crate::process_control::spawn_job_owned(&mut command, super::VOD_START_HELD_PROBE)
            .map_err(|e| e.to_string())?;
    hooks.slot.get().after_spawn(child.id().unwrap_or(0)).await;
    let mut output = child.stdout.take().map(|pipe| {
        tokio::spawn(async move {
            match sink {
                None => bounded(pipe, cap).await.map(|bytes| (bytes, None)),
                Some(sink) => bounded_into(pipe, cap, sink)
                    .await
                    .map(|sink| (Vec::new(), Some(sink))),
            }
        })
    });
    let mut diagnostics = child
        .stderr
        .take()
        .map(|pipe| tokio::spawn(bounded(pipe, 8 * 1024)));
    let mut output_joined = false;
    let mut diagnostics_joined = false;
    let collected = {
        let collect = async {
            let out = output
                .as_mut()
                .ok_or_else(|| "Source probe stdout missing".to_owned())?;
            let err = diagnostics
                .as_mut()
                .ok_or_else(|| "Source probe stderr missing".to_owned())?;
            let ((stdout, sink), stderr) = tokio::try_join!(
                async {
                    let result = out.await;
                    output_joined = true;
                    result.map_err(|e| e.to_string())?
                },
                async {
                    let result = err.await;
                    diagnostics_joined = true;
                    result.map_err(|e| e.to_string())?
                },
            )?;
            Ok::<_, String>((crate::ffmpeg::BoundedOutput { stdout, stderr }, sink))
        };
        tokio::pin!(collect);
        tokio::select! {
            result=&mut collect=>result,
            ()=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>Err("Source probe deadline".into()),
            ()=cancel.cancelled()=>Err("Source probe cancelled".into()),
        }
    };
    if collected.is_err() {
        let _ = child.start_kill();
    }
    let status = if hooks.slot.get().inject_wait_failure() {
        Ok(Err(std::io::Error::other(
            "injected Source probe wait failure",
        )))
    } else {
        tokio::time::timeout(Duration::from_secs(5), child.wait()).await
    };
    let (status, wait_failed) = match status {
        Ok(Ok(status)) => (status, false),
        _ => {
            let _ = child.start_kill();
            hooks.slot.get().before_reap_retry().await;
            // A wait error is quarantine, never proof of physical settlement.
            let status = loop {
                match child.wait().await {
                    Ok(status) => break status,
                    Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            };
            (status, true)
        }
    };
    // These flags are set only by actual JoinHandle completion above. Any
    // reader whose await was interrupted remains owned until abort-and-join.
    if !output_joined {
        if let Some(task) = output.as_mut() {
            task.abort();
            let _ = task.await;
        }
    }
    if !diagnostics_joined {
        if let Some(task) = diagnostics.as_mut() {
            task.abort();
            let _ = task.await;
        }
    }
    if wait_failed {
        return Err("Source probe initial wait did not confirm reap".into());
    }
    if !status.success() {
        return Err("Source probe child failed".into());
    }
    let collected_output = collected?;
    if !source.unchanged() || !current.unchanged() {
        return Err("Source file changed during probe".into());
    }
    Ok(collected_output)
}

#[cfg(test)]
mod source_clock_tests {
    use super::held_source_clock;

    #[test]
    fn source_operation_clock_uses_held_format_not_stream_or_catalog_starts() {
        let clock = held_source_clock(r#"{"format":{"start_time":"-0.021333"},"streams":[{"start_time":"0.0"},{"start_time":"0.250000"}]}"#)
            .expect("held container origin");
        assert_eq!(
            clock,
            plurx_core::transcode::VodSourceClock::ReportedOrigin(-21_333)
        );
        assert_eq!(
            held_source_clock(r#"{"format":{},"streams":[{"start_time":"66271.0"}]}"#)
                .expect("producer AV_NOPTS default"),
            plurx_core::transcode::VodSourceClock::ProducerDefaultZero
        );
        for malformed in [
            r#"{"format":{"start_time":0}}"#,
            r#"{"format":{"start_time":"NaN"}}"#,
            r#"{"streams":[]}"#,
        ] {
            assert!(held_source_clock(malformed).is_err());
        }
    }
}

#[cfg(test)]
mod dv_source_origin_tests {
    use super::{held_dv_source_origin, held_source_clock};
    use plurx_core::transcode::dv_processing::DvTimestamp;

    #[test]
    fn dv_origin_requires_matching_observed_held_format_and_selected_video() {
        for (start, micros) in [(0, 0), (125, 125_000)] {
            let document = serde_json::json!({"format":{"start_time":format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)},
                "streams":[{"start_pts": start}, {"start_pts":start + 10}]});
            let format = held_source_clock(&document.to_string()).expect("held FORMAT clock");
            let origin = held_dv_source_origin(&document["streams"][0], (1, 1000), format)
                .expect("common origin");
            assert_eq!(
                origin.timestamp(),
                DvTimestamp::new(start, 1000).expect("valid held source timestamp")
            );
            assert!(
                held_dv_source_origin(&document["streams"][1], (1, 1000), format).is_err(),
                "a later audio/other-stream start cannot replace the common clock"
            );
        }
        let default = held_source_clock(r#"{"format":{}}"#).expect("default held FORMAT clock");
        assert!(
            held_dv_source_origin(&serde_json::json!({"start_pts":0}), (1, 1000), default).is_err()
        );
        let observed = held_source_clock(r#"{"format":{"start_time":"0.000000"}}"#)
            .expect("observed held FORMAT clock");
        for stream in [
            serde_json::json!({}),
            serde_json::json!({"start_pts":"N/A"}),
            serde_json::json!({"start_pts":-1}),
            serde_json::json!({"start_pts":i64::MAX}),
        ] {
            assert!(held_dv_source_origin(&stream, (2, 1000), observed).is_err());
        }
        assert!(
            held_dv_source_origin(&serde_json::json!({"start_pts":0}), (0, 1000), observed)
                .is_err()
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "source_preparation/fixed_tests.rs"]
mod fixed_tests;
#[cfg(all(test, target_os = "linux"))]
pub(in crate::transcode) use fixed_tests::source_fixed_planning_fixture;
