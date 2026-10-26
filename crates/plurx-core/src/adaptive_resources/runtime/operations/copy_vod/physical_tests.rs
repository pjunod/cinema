//! Authentic original-copy native cases. Authored/compiled only: execution is
//! held for complete-candidate review. Fixture diagnostics are never receipts.
use super::*;
use crate::adaptive_resources::physical_graph::copy_vod::{
    CopyFixtureFault, CopyStartDisposition, CopyTaskHoldReason, CopyTaskTerminal,
};
use crate::adaptive_resources::runtime::copy_vod::{CopyPipeCommand, CopyVodOwner};
use std::future::Future;
use std::path::Path;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout_at(deadline().into(), future)
        .await
        .expect(
            "FAILURE watchdog: original copy native operation did not finish; no closure inferred",
        )
}
async fn original(root: &Path) -> (Arc<NodeResourceRuntime>, CopyVodOwner, OwnedPreparation) {
    let (runtime, request) = NodeResourceRuntime::fixture_actual_copy_request(root, false).await;
    let owner = bounded(runtime.prepare_copy_vod(request, deadline()))
        .await
        .expect("actual admitted original copy");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    (runtime, owner, preparation)
}
async fn cancel_at(
    runtime: &NodeResourceRuntime,
    owner: &mut CopyVodOwner,
    stage: CopyStartFixtureStage,
) {
    let gate = runtime.fixture_pause_copy_start(stage);
    let mut start = Box::pin(owner.start(deadline()));
    tokio::select! {
        result = &mut start => panic!("original start returned before {stage:?}: {result:?}"),
        () = gate.reached() => {},
        () = tokio::time::sleep(Duration::from_secs(20)) => panic!("FAILURE stage watchdog; no no-entry inference"),
    }
    drop(start); // cancel ONLY borrowed waiter; original R, task and child remain
    drop(gate);
}
async fn seal_registered(
    preparation: &OwnedPreparation,
) -> (SealedPhysicalGraph, GraphCopyTask, GraphRoot) {
    let mut bundle = preparation.state.lock().await;
    let role = &mut bundle.roles[0];
    let copy = role.copy_vod.as_ref().expect("original copy carrier");
    let task = copy.task.expect("original registered parser");
    let root = *role
        .roots
        .get(&copy.root_member)
        .expect("original child selector");
    if let Some(graph) = role.graph.take() {
        role.sealed = Some(graph.seal());
    }
    (
        role.sealed
            .as_ref()
            .expect("same sealed custody")
            .select_same(),
        task,
        root,
    )
}
async fn kill_and_wait(graph: &SealedPhysicalGraph, root: GraphRoot) {
    // Even an unsuccessful signal is no proof. Only this original wait is
    // used before a native closure constructor can inspect its own inventory.
    let _ = bounded(graph.start_kill_root(root)).await;
    bounded(graph.wait_root(root))
        .await
        .expect("SAME original child wait");
}
async fn finish_positive(mut owner: CopyVodOwner, runtime: Arc<NodeResourceRuntime>) {
    let mut ticket = bounded(owner.prepare_retirement(true, deadline()))
        .await
        .expect("original driver accepts exact native closure");
    drop(owner);
    bounded(ticket.finish_retirement(deadline()))
        .await
        .expect("original application destruction and one typed settlement");
    assert_eq!(
        runtime.accounting().expect("same ledger").used,
        ResourceVector::default()
    );
}

#[tokio::test]
async fn unsent_latch_is_dropped_under_seal_and_same_task_joins_never_entered() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("full original R").used;
    let (graph, task, child) = seal_registered(&preparation).await;
    kill_and_wait(&graph, child).await;
    assert_eq!(
        bounded(graph.join_copy_vod(task))
            .await
            .expect("natural original join"),
        CopyTaskTerminal::NeverEntered
    );
    assert_eq!(
        graph.fixture_copy_disposition(task).await,
        Some(CopyStartDisposition::DroppedUnsent)
    );
    assert_eq!(
        bounded(graph.join_copy_vod(task))
            .await
            .expect("same join replay"),
        CopyTaskTerminal::NeverEntered
    );
    assert_eq!(
        graph.fixture_copy_disposition(task).await,
        Some(CopyStartDisposition::DroppedUnsent)
    );
    assert_eq!(graph.fixture_copy_pipe_close_calls(task).await, [1, 1]);
    assert_eq!(
        runtime.accounting().expect("join is not refund").used,
        charge
    );
    drop(graph);
    finish_positive(owner, runtime).await;
}

#[tokio::test]
async fn taken_unsent_latch_cancelled_join_resumes_same_disposition_and_handle() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original R").used;
    let sender = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        role.graph
            .as_ref()
            .expect("original unsealed graph")
            .take_copy_start(
                role.copy_vod
                    .as_ref()
                    .expect("copy")
                    .task
                    .expect("same parser"),
            )
            .await
            .expect("take SAME original sender without sending")
    };
    let (graph, task, child) = seal_registered(&preparation).await;
    // Deliberately pending original receiver, never an aborted parser task.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), graph.join_copy_vod(task))
            .await
            .is_err()
    );
    assert_eq!(
        graph.fixture_copy_disposition(task).await,
        Some(CopyStartDisposition::TakenBeforeSeal)
    );
    assert!(!graph.fixture_copy_task_finished(task).await);
    assert_eq!(
        runtime.accounting().expect("cancelled join held").used,
        charge
    );
    drop(sender);
    kill_and_wait(&graph, child).await;
    assert_eq!(
        bounded(graph.join_copy_vod(task))
            .await
            .expect("same task resumed"),
        CopyTaskTerminal::NeverEntered
    );
    assert_eq!(
        graph.fixture_copy_disposition(task).await,
        Some(CopyStartDisposition::TakenBeforeSeal)
    );
    drop(graph);
    finish_positive(owner, runtime).await;
}

#[tokio::test]
async fn root_only_closure_consumes_actual_unclaimed_pipes_without_inventing_task() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::RootRegistered).await;
    let charge = runtime.accounting().expect("original R").used;
    let (graph, child, root_member, task_member, acquisition, mut image) = {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let copy = role.copy_vod.as_mut().expect("same copy");
        assert!(copy.task.is_none());
        let child = *role
            .roots
            .get(&copy.root_member)
            .expect("retained original root");
        let ids = (copy.root_member, copy.task_member, copy.acquisition_id);
        let image = copy.fixed_image.take();
        role.sealed = Some(role.graph.take().expect("original graph").seal());
        (
            role.sealed.as_ref().expect("sealed").select_same(),
            child,
            ids.0,
            ids.1,
            ids.2,
            image,
        )
    };
    kill_and_wait(&graph, child).await;
    let closure = graph
        .try_copy_root_only_closure(child, root_member, task_member, acquisition, &mut image)
        .await
        .expect("actual waited root and both checked unclaimed-pipe closes");
    assert!(closure.matches(&graph));
    assert_eq!(closure.root(), child);
    assert_eq!(closure.root_member(), root_member);
    assert_eq!(closure.task_member(), task_member);
    assert_eq!(closure.acquisition_id(), acquisition);
    assert!(!closure.close_operation_id().is_nil());
    assert!(image.is_none());
    let snapshot = graph.snapshot().await;
    assert_eq!((snapshot.roots, snapshot.writers), (0, 0));
    assert_eq!(
        runtime
            .accounting()
            .expect("native witness not refund")
            .used,
        charge
    );
    {
        let mut bundle = preparation.state.lock().await;
        bundle.roles[0]
            .copy_vod
            .as_mut()
            .expect("same original carrier")
            .root_only_closure = Some(closure);
    }
    drop(graph);
    finish_positive(owner, runtime).await;
}

#[tokio::test]
async fn poisoned_original_terminal_never_becomes_never_entered_or_refund() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original R").used;
    let (graph, task, child) = seal_registered(&preparation).await;
    graph
        .fixture_copy_fault(task, CopyFixtureFault::PoisonTerminal)
        .await;
    kill_and_wait(&graph, child).await;
    assert!(bounded(graph.join_copy_vod(task)).await.is_err());
    assert_eq!(
        graph.copy_task_hold_reason(task).await,
        Some(CopyTaskHoldReason::TerminalPoisoned)
    );
    assert!(graph.try_copy_process_closure(task).await.is_err());
    assert_eq!(
        runtime.accounting().expect("poison holds full R").used,
        charge
    );
    assert_eq!(graph.snapshot().await.roots, 1);
}

#[tokio::test]
async fn lost_terminal_after_actual_join_is_sticky_not_default_false_summary() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original R").used;
    let (graph, task, child) = seal_registered(&preparation).await;
    kill_and_wait(&graph, child).await;
    assert_eq!(
        bounded(graph.join_copy_vod(task))
            .await
            .expect("actual original task joined"),
        CopyTaskTerminal::NeverEntered
    );
    graph
        .fixture_copy_fault(task, CopyFixtureFault::LoseJoinedTerminal)
        .await;
    assert!(graph.join_copy_vod(task).await.is_err());
    assert_eq!(
        graph.copy_task_hold_reason(task).await,
        Some(CopyTaskHoldReason::TerminalMissing)
    );
    assert!(graph.try_copy_process_closure(task).await.is_err());
    assert_eq!(
        runtime.accounting().expect("lost summary holds").used,
        charge
    );
}

#[tokio::test]
async fn original_pipe_close_result_loss_never_recloses_numeric_descriptor() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original R").used;
    let (graph, task, child) = seal_registered(&preparation).await;
    graph
        .fixture_copy_fault(task, CopyFixtureFault::LosePipeCloseResult)
        .await;
    kill_and_wait(&graph, child).await;
    assert!(bounded(graph.join_copy_vod(task)).await.is_err());
    assert_eq!(
        graph.copy_task_hold_reason(task).await,
        Some(CopyTaskHoldReason::PipeCloseFailed)
    );
    assert_eq!(graph.fixture_copy_pipe_close_calls(task).await, [1, 0]);
    assert!(graph.join_copy_vod(task).await.is_err());
    assert!(graph.try_copy_process_closure(task).await.is_err());
    assert_eq!(graph.fixture_copy_pipe_close_calls(task).await, [1, 0]);
    assert_eq!(
        runtime
            .accounting()
            .expect("actual result uncertainty holds")
            .used,
        charge
    );
}

#[tokio::test]
async fn registered_writer_cannot_be_reclassified_as_root_only_when_selector_lost() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original R").used;
    let (graph, task, child) = seal_registered(&preparation).await;
    kill_and_wait(&graph, child).await;
    assert_eq!(
        bounded(graph.join_copy_vod(task))
            .await
            .expect("original task"),
        CopyTaskTerminal::NeverEntered
    );
    let (root_member, task_member, acquisition) = {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0].copy_vod.as_ref().expect("same copy");
        (copy.root_member, copy.task_member, copy.acquisition_id)
    };
    // Image is owned by the ACTUAL registered record. None is not absence.
    assert!(graph
        .try_copy_root_only_closure(child, root_member, task_member, acquisition, &mut None)
        .await
        .is_err());
    assert_eq!(
        graph.copy_hold_reason().await,
        Some(CopyTaskHoldReason::StartDispositionMismatch)
    );
    assert_eq!(
        runtime.accounting().expect("task selector loss held").used,
        charge
    );
}

#[tokio::test]
async fn actual_publication_sink_panic_loses_task_result_and_holds_original_r() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    {
        let mut bundle = preparation.state.lock().await;
        bundle.roles[0]
            .copy_vod
            .as_mut()
            .expect("original copy")
            .work
            .as_mut()
            .expect("same original prepared parser/sink")
            .sink
            .fixture_panic_next_publication();
    }
    bounded(owner.start(deadline()))
        .await
        .expect("real copy entry");
    let charge = runtime.accounting().expect("original R").used;
    let (graph, task, child) = seal_registered(&preparation).await;
    bounded(async {
        while !graph.fixture_copy_task_finished(task).await {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(bounded(graph.join_copy_vod(task)).await.is_err());
    assert_eq!(
        graph.copy_task_hold_reason(task).await,
        Some(CopyTaskHoldReason::TaskAbandoned)
    );
    kill_and_wait(&graph, child).await;
    assert!(graph.try_copy_process_closure(task).await.is_err());
    assert_eq!(
        runtime.accounting().expect("actual sink panic holds").used,
        charge
    );
}

/// Real single ELF child: malformed stdout causes parser failure, SIGPIPE is
/// ignored and stderr stays open. Post-join kill would deadlock this case.
fn epipe_ignoring_executable(root: &Path) -> std::path::PathBuf {
    let source = root.join("ignores_epipe.c");
    let binary = root.join("ignores_epipe");
    std::fs::write(
        &source,
        br#"#include <signal.h>
#include <unistd.h>
int main(void) {
    signal(SIGPIPE, SIG_IGN);
    const unsigned char invalid[] = {0,0,0,4,'b','a','d','!'};
    (void)write(1, invalid, sizeof invalid);
    for (;;) pause(); /* stderr and stdout remain open until original kill */
}
"#,
    )
    .expect("actual helper source");
    let compiler = std::env::var_os("PLURX_TEST_CC").unwrap_or_else(|| "/usr/bin/cc".into());
    let output = std::process::Command::new(compiler)
        .args(["-O0", "-o"])
        .arg(&binary)
        .arg(&source)
        .output()
        .expect("install a real C compiler for native failure fixture");
    assert!(
        output.status.success(),
        "actual helper compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::canonicalize(binary).expect("actual ELF path")
}

#[tokio::test]
async fn parser_failure_kills_same_epipe_ignoring_root_before_stderr_join() {
    use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcExecutable;
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
    let binary = epipe_ignoring_executable(root.path());
    let identity = crate::source_identity::SourceIdentity::from_metadata(
        &std::fs::metadata(&binary).expect("actual ELF metadata"),
    );
    let digest = hex::encode(Sha256::digest(
        std::fs::read(&binary).expect("actual ELF bytes"),
    ));
    // This is an authentic different fixture image admitted BEFORE entry,
    // not a replacement child, PID, handle, source cell or close operation.
    request.command = CopyPipeCommand::compile(
        request.command.source().clone(),
        request.command.video(),
        SoftwareAvcExecutable::new(binary, identity, &digest)
            .expect("actual verified fixture image"),
        root.path().to_owned(),
        None,
        false,
        crate::transcode::Pacing::unpaced(),
        None,
    )
    .expect("same fixed one-input recipe");
    let mut owner = bounded(runtime.prepare_copy_vod(request, deadline()))
        .await
        .expect("actual fixture R");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    bounded(owner.start(deadline()))
        .await
        .expect("real entered parser and native child");
    let charge = runtime.accounting().expect("full R").used;
    // Natural policy: no preemptive driver abort masks the parser-failure kill.
    let result = bounded(owner.output().stop_producer(false, deadline()))
        .await
        .expect("same native task and child naturally joined after failure kill");
    assert!(!result.completed);
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same retained carrier");
        assert!(
            matches!(copy.terminal, Some(CopyTaskTerminal::Entered(summary)) if !summary.completed)
        );
        let closure = copy
            .process_closure
            .as_ref()
            .expect("actual original checked native closure");
        assert_eq!(
            closure.start_disposition(),
            CopyStartDisposition::TakenBeforeSeal
        );
        assert!(
            matches!(closure.terminal(), CopyTaskTerminal::Entered(summary) if !summary.completed)
        );
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("native closure alone held")
            .used,
        charge
    );
    finish_positive(owner, runtime).await;
}

#[tokio::test]
async fn actual_entered_sink_can_publish_while_join_holds_only_graph_members() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    bounded(owner.start(deadline()))
        .await
        .expect("real original FFmpeg/parser entry");
    let charge = runtime.accounting().expect("original R").used;
    let result = bounded(owner.output().stop_producer(false, deadline()))
        .await
        .expect("no bundle/members inversion during actual publication");
    assert!(
        result.completed && result.segments > 0,
        "requires actual parser/sink publication, not no-entry"
    );
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same original copy");
        assert!(
            matches!(copy.terminal, Some(CopyTaskTerminal::Entered(summary)) if summary.completed)
        );
        let closure = copy
            .process_closure
            .as_ref()
            .expect("original physical closure");
        assert_eq!(
            closure.start_disposition(),
            CopyStartDisposition::TakenBeforeSeal
        );
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("P/parser closure is not R refund")
            .used,
        charge
    );
    finish_positive(owner, runtime).await;
}

#[tokio::test]
async fn physical_requested_layout_includes_current_original_future_and_result_payload() {
    // Requested Rust storage only. Tokio/allocator/native frame peaks remain
    // separately Unknown; a sizeof projection is never their hard bound.
    let bytes = crate::adaptive_resources::physical_graph::copy_vod::copy_task_structural_bytes()
        .expect("checked requested structural projection");
    assert!(bytes >= std::mem::size_of::<CopyTaskTerminal>());
    assert!(bytes >= std::mem::size_of::<CopyStartDisposition>());
    assert!(bytes >= std::mem::size_of::<CopyTaskHoldReason>());
}

#[tokio::test]
async fn entered_terminal_cannot_match_dropped_unsent_disposition() {
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    bounded(owner.start(deadline()))
        .await
        .expect("real copy start");
    let charge = runtime.accounting().expect("original R").used;
    bounded(owner.wait_producer_result(deadline()))
        .await
        .expect("actual parser publication completed");
    let (graph, task, child) = seal_registered(&preparation).await;
    assert!(matches!(
        bounded(graph.join_copy_vod(task))
            .await
            .expect("original entered task joined"),
        CopyTaskTerminal::Entered(_)
    ));
    bounded(graph.wait_root(child))
        .await
        .expect("original root wait");
    graph
        .fixture_copy_fault(task, CopyFixtureFault::ContradictStartDisposition)
        .await;
    assert!(graph.try_copy_process_closure(task).await.is_err());
    assert_eq!(
        graph.copy_task_hold_reason(task).await,
        Some(CopyTaskHoldReason::StartDispositionMismatch)
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("contradiction holds full R")
            .used,
        charge
    );
}

#[tokio::test]
async fn real_exited_root_signal_error_does_not_replace_or_permanently_hold_original_join() {
    use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcExecutable;
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().expect("fixture root");
    let (runtime, mut request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
    let source = root.path().join("exited_invalid.c");
    let binary = root.path().join("exited_invalid");
    std::fs::write(
        &source,
        br#"#include <unistd.h>
int main(void) {
    const unsigned char invalid[] = {0,0,0,4,'b','a','d','!'};
    return write(1, invalid, sizeof invalid) == sizeof invalid ? 0 : 1;
}
"#,
    )
    .expect("actual exited-child source");
    let compiler = std::env::var_os("PLURX_TEST_CC").unwrap_or_else(|| "/usr/bin/cc".into());
    let built = std::process::Command::new(compiler)
        .args(["-O0", "-o"])
        .arg(&binary)
        .arg(&source)
        .output()
        .expect("actual C compiler required");
    assert!(
        built.status.success(),
        "native fixture compilation failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let binary = std::fs::canonicalize(binary).expect("actual ELF");
    let identity = crate::source_identity::SourceIdentity::from_metadata(
        &std::fs::metadata(&binary).expect("actual ELF metadata"),
    );
    let digest = hex::encode(Sha256::digest(
        std::fs::read(&binary).expect("actual ELF bytes"),
    ));
    request.command = CopyPipeCommand::compile(
        request.command.source().clone(),
        request.command.video(),
        SoftwareAvcExecutable::new(binary, identity, &digest).expect("actual ELF binding"),
        root.path().to_owned(),
        None,
        false,
        crate::transcode::Pacing::unpaced(),
        None,
    )
    .expect("same fixed recipe");
    let mut owner = bounded(runtime.prepare_copy_vod(request, deadline()))
        .await
        .expect("original R");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    cancel_at(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let sender = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        role.graph
            .as_ref()
            .expect("original graph")
            .take_copy_start(
                role.copy_vod
                    .as_ref()
                    .expect("copy")
                    .task
                    .expect("original task"),
            )
            .await
            .expect("SAME start sender taken before seal")
    };
    let (graph, task, child) = seal_registered(&preparation).await;
    // The actual root exits while the SAME original parser still awaits its
    // original receiver. This wait, not a signal result, records its exit.
    assert!(bounded(graph.wait_root(child))
        .await
        .expect("actual original wait")
        .success());
    let signal_error = child
        .registered_control()
        .expect("original retained pidfd control")
        .start_kill()
        .expect_err("original reaped pidfd must report a real signal error");
    assert_eq!(signal_error.raw_os_error(), Some(libc::ESRCH));
    assert!(!graph.fixture_copy_task_finished(task).await);
    let charge = runtime
        .accounting()
        .expect("signal failure cannot refund")
        .used;
    sender
        .send(())
        .expect("deliver SAME original latch, never create replacement parser");
    let terminal = bounded(async {
        loop {
            match graph.join_copy_vod(task).await {
                Ok(terminal) => break terminal,
                Err(_) => {
                    assert!(matches!(
                        graph.copy_task_hold_reason(task).await,
                        Some(
                            CopyTaskHoldReason::FailureKillFailed
                                | CopyTaskHoldReason::FailureKillUnavailable
                        )
                    ));
                    assert!(
                        graph.copy_hold_reason().await.is_none(),
                        "signal failure must stay retryable"
                    );
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await;
    assert!(matches!(terminal, CopyTaskTerminal::Entered(summary) if !summary.completed));
    assert!(graph.copy_task_hold_reason(task).await.is_none());
    assert_eq!(
        graph.fixture_copy_disposition(task).await,
        Some(CopyStartDisposition::TakenBeforeSeal)
    );
    assert_eq!(graph.fixture_copy_pipe_close_calls(task).await, [1, 1]);
    let closed = graph
        .try_copy_process_closure(task)
        .await
        .expect("actual wait+same join+checked pipes");
    assert_eq!(closed.terminal(), terminal);
    assert_eq!(closed.task(), task);
    assert_eq!(closed.root(), child);
    assert_eq!(
        runtime
            .accounting()
            .expect("physical closure alone is not settlement")
            .used,
        charge
    );
    // Deliberately direct native fixture: runtime start_released is false,
    // so its independent identity cross-check must not authorize a refund.
}
