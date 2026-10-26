//! Linux fixture-only launcher failure classes, scoped to the caller's
//! original async task. No context is global or inherited by spawned tasks.
//! The post-std case uses the SAME command on the real owner thread; it is
//! NOT an injected Tokio syscall failure and grants no native-close proof.
use std::future::Future;
use std::io;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

tokio::task_local! {
    static ORIGINAL_SPAWN_FAULT: Arc<State>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnFaultPoint {
    BeforeOwnerDispatch,
    AfterStdSpawnBeforeTokioRegistration,
}

/// The only owner of the actual standard-library Child is this private
/// teardown guard. Neither PID diagnostics nor fixture cleanup can close or
/// settle a runtime obligation whose launcher returned no handle.
#[derive(Debug)]
struct TeardownChild {
    child: Child,
}
impl Drop for TeardownChild {
    fn drop(&mut self) {
        // try_wait caches an observed status in this SAME Child. Never kill a
        // child already reaped through diagnostics; no bare-PID fallback.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
#[derive(Debug)]
struct State {
    point: SpawnFaultPoint,
    attempts: AtomicUsize,
    child: Mutex<Option<TeardownChild>>,
}
impl State {
    fn teardown(&self) {
        let child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(child);
    }
}

/// One caller-owned fixture scope; only its first launcher call is faulted.
/// Normal return retains the orphan for diagnostics until this scope drops.
/// Cancellation/unwind of run() tears it down even if the scope is retained.
#[derive(Debug)]
pub struct SpawnFaultScope {
    state: Arc<State>,
}
impl SpawnFaultScope {
    pub fn new(point: SpawnFaultPoint) -> Self {
        Self {
            state: Arc::new(State {
                point,
                attempts: AtomicUsize::new(0),
                child: Mutex::new(None),
            }),
        }
    }
    pub async fn run<T>(&self, operation: impl Future<Output = T>) -> T {
        let mut running = RunningScope {
            state: &self.state,
            returned: false,
        };
        let result = ORIGINAL_SPAWN_FAULT
            .scope(Arc::clone(&self.state), operation)
            .await;
        running.returned = true;
        result
    }
    pub fn attempts(&self) -> usize {
        self.state.attempts.load(Ordering::Acquire)
    }
    /// Diagnostic PID only; never use it as a native owner or close witness.
    pub fn pid(&self) -> Option<u32> {
        self.state
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|original| original.child.id())
    }
    /// Inspect only the SAME original Child. Absence returns NotFound;
    /// Ok(None) describes an actual retained child with no observed exit.
    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        self.state
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "fixture did not retain an original child",
                )
            })?
            .child
            .try_wait()
    }
}
impl Drop for SpawnFaultScope {
    fn drop(&mut self) {
        self.state.teardown();
    }
}
struct RunningScope<'a> {
    state: &'a State,
    returned: bool,
}
impl Drop for RunningScope<'_> {
    fn drop(&mut self) {
        if !self.returned {
            self.state.teardown();
        }
    }
}

pub(crate) struct CapturedFault {
    state: Arc<State>,
}
pub(crate) fn capture() -> Option<CapturedFault> {
    ORIGINAL_SPAWN_FAULT
        .try_with(|state| {
            let attempt = state.attempts.fetch_add(1, Ordering::AcqRel);
            (attempt == 0).then(|| CapturedFault {
                state: Arc::clone(state),
            })
        })
        .ok()
        .flatten()
}
pub(crate) fn generic_failure() -> io::Error {
    io::Error::other("fixture launcher failure")
}
impl CapturedFault {
    pub(crate) fn before_owner_dispatch(&self) -> bool {
        self.state.point == SpawnFaultPoint::BeforeOwnerDispatch
    }
    pub(crate) fn after_std_spawn(&self) -> bool {
        self.state.point == SpawnFaultPoint::AfterStdSpawnBeforeTokioRegistration
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "Fixture class simulation executes only the SAME original standard command on the existing owner thread; the original Child stays in its private RAII teardown guard, never a native witness."
    )]
    pub(crate) fn fail_after_std_spawn<T>(
        &self,
        command: &mut std::process::Command,
    ) -> io::Result<T> {
        let original = TeardownChild {
            child: command.spawn()?,
        };
        let mut child = self
            .state
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(child.is_none(), "one targeted original fixture child");
        *child = Some(original);
        Err(generic_failure())
    }
}
