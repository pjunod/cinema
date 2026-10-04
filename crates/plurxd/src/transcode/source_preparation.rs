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

pub(crate) struct SourceHeldProbeEvidence {
    assignment: SourceDispatchAssignment,
    object_version: String,
    document: String,
    engine: crate::ffmpeg::EncodedEngine,
    build: String,
}
impl SourceHeldProbeEvidence {
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
pub(crate) struct SourceProbeOperation {
    assignment: SourceDispatchAssignment,
    state: Arc<ProbeState>,
    cancel: tokio_util::sync::CancellationToken,
}
impl SourceProbeOperation {
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(crate) async fn settle(&self) -> SourceProbeSettlement {
        loop {
            let changed = self.state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.state.settled.load(Ordering::Acquire) {
                return SourceProbeSettlement {
                    assignment: self.assignment.clone(),
                };
            }
            changed.await;
        }
    }
    pub(crate) async fn outcome(&self) -> Result<SourceHeldProbeEvidence, String> {
        let _receipt = self.settle().await;
        self.state
            .result
            .lock()
            .map_err(|_| "Source probe result poisoned".to_owned())?
            .take()
            .ok_or_else(|| "Source probe result already consumed".to_owned())?
    }
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
        )
        .await;
        *owned.result.lock().expect("Source probe result") = Some(result);
        // Only confirmed child/job and pipe settlement lets this permit drop.
        drop(owned.permit.lock().expect("Source probe permit").take());
        owned.settled.store(true, Ordering::Release);
        owned.changed.notify_waiters();
    });
    SourceProbeOperation {
        assignment,
        state,
        cancel,
    }
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
) -> Result<SourceHeldProbeEvidence, String> {
    // The reporter query is physical work too. It runs under this same actual
    // permit/owner; no cold global reporter-cache call may spawn unowned work.
    let mut reporter = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
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
    let command = crate::ffmpeg::source_held_probe_command(&source.handle)?;
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
    let (engine, build) = crate::ffmpeg::EncodedEngine::capture_source(&execution).await?;
    Ok(SourceHeldProbeEvidence {
        engine,
        build,
        assignment: proof.assignment().clone(),
        object_version: source.object_version().to_owned(),
        document,
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
    mut command: tokio::process::Command,
    cap: usize,
    hooks: &SourceProbeHookOwner,
) -> Result<crate::ffmpeg::BoundedOutput, String> {
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
    let mut output = child
        .stdout
        .take()
        .map(|pipe| tokio::spawn(bounded(pipe, cap)));
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
            let (stdout, stderr) = tokio::try_join!(
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
            Ok::<_, String>(crate::ffmpeg::BoundedOutput { stdout, stderr })
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
    for (task, joined) in [
        (&mut output, output_joined),
        (&mut diagnostics, diagnostics_joined),
    ] {
        if !joined {
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
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
