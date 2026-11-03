//! Pacing inside the existing strict-source operation owner. This future
//! neither registers a source owner nor spawns a driver/cleanup task.
use super::DecodeFactError;
use plurx_core::process::bounded::{self, SourcePauseController, SourcePausePhase};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

struct Clock {
    control: SourcePauseController,
    paused_before: Duration,
}
tokio::task_local! {static CLOCK:Clock;}

/// Legacy launch checks share this collection's verified-pause baseline.
/// The outer active timer retains the finite owner wall and cancellation.
pub(super) fn deadline(original: Instant) -> Instant {
    CLOCK
        .try_with(|clock| {
            original
                .checked_add(
                    clock
                        .control
                        .paused_duration()
                        .saturating_sub(clock.paused_before),
                )
                .unwrap_or(original)
        })
        .unwrap_or(original)
}

pub(super) async fn run<T>(
    control: SourcePauseController,
    inherited: bool,
    budget: Option<Arc<crate::background_jobs::SourceReadBudget>>,
    cancel: CancellationToken,
    work: impl std::future::Future<Output = Result<T, DecodeFactError>>,
) -> Result<T, DecodeFactError> {
    let clock = Clock {
        paused_before: control.paused_duration(),
        control: control.clone(),
    };
    CLOCK.scope(clock,bounded::pausable(control.clone(),async {
        // The existing scanner/operation driver owns pause and its exact fence.
        if inherited {return work.await;}
        let (finished,completion)=tokio::sync::watch::channel(false);
        let owner_budget=budget.clone();
        let driving=drive(control,budget,cancel.clone(),completion);
        tokio::pin!(work);tokio::pin!(driving);
        tokio::select! {
            ()=cancel.cancelled()=>{
                // A possibly committed pause command is fenced by cancelling
                // this exact common owner before dropping its RPC waiter.
                if let Some(budget)=owner_budget.as_ref(){budget.cancellation().cancel();}
                let _=work.await;
                Err(DecodeFactError::Cancelled)
            }
            result=&mut work=>{
                let _=finished.send(true);
                // A completed stage cannot hand the next stage a paused Job
                // slot. The same driver resolves any in-flight pause CAS and
                // reacquires it before the scope can return successfully.
                if let Err(error)=driving.await {
                    if let Some(budget)=owner_budget.as_ref(){budget.cancellation().cancel();}
                    cancel.cancel();return Err(error);
                }
                result
            }
            result=&mut driving=>{
                cancel.cancel();
                // A failed/lost pause acknowledgement may already have
                // committed. Fence every later read of this common owner;
                // fallback cannot silently reuse a paused Job source row.
                if let Some(budget)=owner_budget.as_ref(){budget.cancellation().cancel();}
                let _=work.await; // actual child/broker/read cleanup remains owned
                match result {Err(error)=>Err(error),Ok(())=>Err(DecodeFactError::Cancelled)}
            }
        }
    })).await
}

async fn drive(
    control: SourcePauseController,
    budget: Option<Arc<crate::background_jobs::SourceReadBudget>>,
    cancel: CancellationToken,
    mut completed: tokio::sync::watch::Receiver<bool>,
) -> Result<(), DecodeFactError> {
    let mut slot_paused = false;
    loop {
        if cancel.is_cancelled()
            || budget
                .as_ref()
                .is_some_and(|budget| budget.cancellation().is_cancelled())
        {
            return Err(DecodeFactError::Cancelled);
        }
        if control.pause_budget_exhausted() {
            return Err(DecodeFactError::Deadline);
        }
        let pressure = match budget.as_ref() {
            Some(budget) => !budget
                .source_may_continue()
                .await
                .map_err(DecodeFactError::SourceService)?,
            None => false,
        };
        let quota = control.quota_waiters() != 0
            || budget
                .as_ref()
                .is_some_and(|budget| budget.pacing_waiters() != 0);
        if pressure || quota {
            if pressure && control.observation().phase == SourcePausePhase::Running {
                if let Some(budget) = budget.as_ref() {
                    budget.invalidate_grant();
                }
            }
            control.request_pause();
        }
        let observation = control.observation();
        if observation.phase == SourcePausePhase::Paused && !slot_paused {
            if let Some(budget) = budget.as_ref() {
                if !budget
                    .pause_verified()
                    .await
                    .map_err(DecodeFactError::SourceService)?
                {
                    return Err(DecodeFactError::SourceService(
                        "exact source pause authority lost".into(),
                    ));
                }
            }
            slot_paused = true;
        }
        if slot_paused && !pressure && !quota {
            let reacquired = match budget.as_ref() {
                Some(budget) => budget
                    .resume_verified()
                    .await
                    .map_err(DecodeFactError::SourceService)?,
                None => true,
            };
            if reacquired {
                if let Some(budget) = budget.as_ref() {
                    budget.invalidate_grant();
                }
                if !control.resume(observation.revision) {
                    return Err(DecodeFactError::SourceService(
                        "source pause revision changed before resume".into(),
                    ));
                }
                slot_paused = false;
            }
        }
        if *completed.borrow_and_update()
            && !slot_paused
            && control.observation().phase == SourcePausePhase::Running
        {
            return Ok(());
        }
        tokio::select! {
            ()=cancel.cancelled()=>return Err(DecodeFactError::Cancelled),
            changed=completed.changed()=>{if changed.is_err(){return Err(DecodeFactError::Cancelled);}},
            ()=crate::source_custody::source_pressure_changed().notified()=>{},
            ()=tokio::time::sleep(Duration::from_millis(100))=>{},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn inherited_owner_does_not_install_a_competing_driver() {
        let control = SourcePauseController::new(Duration::from_secs(1));
        let quota = control.finite_quota_wait();
        let observed = control.clone();
        run(control, true, None, CancellationToken::new(), async move {
            assert_eq!(observed.observation().phase, SourcePausePhase::Running);
            Ok(())
        })
        .await
        .expect("caller retains its existing driver");
        drop(quota);
    }
    #[tokio::test]
    async fn finite_quota_only_hold_resumes_same_controller_without_a_new_owner() {
        let control = SourcePauseController::new(Duration::from_secs(1));
        let observed = control.clone();
        run(control, false, None, CancellationToken::new(), async move {
            let hold = observed.finite_quota_wait();
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if observed.observation().phase == SourcePausePhase::Paused {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("same driver observes quota hold");
            let revision = observed.observation().revision;
            drop(hold);
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if observed.observation().phase == SourcePausePhase::Running {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("quota completion resumes same controller");
            assert_eq!(observed.observation().revision, revision);
            Ok(())
        })
        .await
        .expect("owned operation joined");
    }
}
