//! Terminal observation for one verified adaptive FEL root. The original
//! VOD driver and existing dormant cleanup retain all physical authority.

use super::{PreparedRootAdmission, Rendition};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, Mutex, OnceLock, Weak,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) struct OriginalProcessedRoot {
    driver: Mutex<OriginalDriver>,
    released_owners: AtomicU8,
    // Set by the actual DV graph before entry and cleared only by its
    // encoder, helper, writer and SourceInput settlement path.
    pub(super) window_pending: AtomicBool,
    pub(super) cancellation: CancellationToken,
}

enum OriginalDriver {
    AwaitingPublication,
    Running(Weak<Rendition>, JoinHandle<()>),
    Joined(Weak<Rendition>),
    Retired,
    NeverAttached,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ProcessedOwner {
    Runtime = 1,
    Admission = 2,
    Instance = 4,
    Rendition = 8,
}

/// Install as a last field of an actual owner. Arc's strong count reaches
/// zero before that value's destructors run and cannot substitute for this.
pub(crate) struct ProcessedOwnerRelease {
    original: Arc<OriginalProcessedRoot>,
    kind: ProcessedOwner,
}

impl std::fmt::Debug for ProcessedOwnerRelease {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("ProcessedOwnerRelease")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Drop for ProcessedOwnerRelease {
    fn drop(&mut self) {
        self.original
            .released_owners
            .fetch_or(self.kind as u8, Ordering::Release);
    }
}

impl OriginalProcessedRoot {
    pub(crate) fn bind_owner(
        self: &Arc<Self>,
        slot: &OnceLock<ProcessedOwnerRelease>,
        kind: ProcessedOwner,
    ) -> Result<(), String> {
        let release = slot.get_or_init(|| ProcessedOwnerRelease {
            original: Arc::clone(self),
            kind,
        });
        if !Arc::ptr_eq(&release.original, self) || release.kind != kind {
            return Err("processed resource already belongs to a different original owner".into());
        }
        Ok(())
    }

    pub(super) fn install_driver(
        self: &Arc<Self>,
        rendition: &Arc<Rendition>,
        spawn: impl FnOnce() -> JoinHandle<()>,
    ) -> Result<(), String> {
        let mut driver = self
            .driver
            .lock()
            .map_err(|_| "processed driver state is uncertain")?;
        if !matches!(*driver, OriginalDriver::AwaitingPublication) {
            return Err("adaptive processed root already owns a driver".into());
        }
        self.bind_owner(
            &rendition.processed_owner_release,
            ProcessedOwner::Rendition,
        )?;
        *driver = OriginalDriver::Running(Arc::downgrade(rendition), spawn());
        Ok(())
    }

    pub(super) async fn retire_driver(&self, rendition: &Arc<Rendition>) -> Result<bool, String> {
        self.cancellation.cancel();
        rendition.kick();
        let joined = {
            let mut driver = self
                .driver
                .lock()
                .map_err(|_| "processed driver state is uncertain")?;
            let exact = Arc::downgrade(rendition);
            match &*driver {
                OriginalDriver::Running(owner, task) if owner.ptr_eq(&exact) => {
                    if !task.is_finished() {
                        return Ok(false);
                    }
                    match std::mem::replace(&mut *driver, OriginalDriver::Unknown) {
                        OriginalDriver::Running(owner, task) => Some((owner, task)),
                        _ => unreachable!("checked under the original driver lock"),
                    }
                }
                OriginalDriver::Joined(owner) if owner.ptr_eq(&exact) => None,
                _ => return Err("processed cleanup does not own this original driver".into()),
            }
        };
        if let Some((owner, task)) = joined {
            task.await
                .map_err(|error| format!("joining original processed driver: {error}"))?;
            *self
                .driver
                .lock()
                .map_err(|_| "processed driver join is uncertain")? = OriginalDriver::Joined(owner);
        }
        if self.window_pending.load(Ordering::Acquire) {
            return Err(
                "the original processed encoder, helper or source remains unresolved".into(),
            );
        }
        // The existing slot additionally guards storage against a registered
        // writer/reaper still owned after the driver returned.
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rendition.slot.retire_storage(),
        )
        .await
        {
            Ok(Ok(())) => Ok(true),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Ok(false),
        }
    }

    pub(super) fn storage_retired(&self, rendition: &Arc<Rendition>) -> Result<(), String> {
        let mut driver = self
            .driver
            .lock()
            .map_err(|_| "processed storage state is uncertain")?;
        let OriginalDriver::Joined(owner) = &*driver else {
            return Err("private storage retired without the original joined driver".into());
        };
        if !owner.ptr_eq(&Arc::downgrade(rendition)) || self.window_pending.load(Ordering::Acquire)
        {
            return Err("private storage retired for a different or uncertain root".into());
        }
        *driver = OriginalDriver::Retired;
        Ok(())
    }

    pub(super) fn owns_rendition(&self, rendition: &Arc<Rendition>) -> bool {
        self.driver.lock().is_ok_and(|driver| match &*driver {
            OriginalDriver::Running(owner, _) | OriginalDriver::Joined(owner) => {
                owner.ptr_eq(&Arc::downgrade(rendition))
            }
            _ => false,
        })
    }
}

impl Rendition {
    pub(super) fn adaptive_processed_root(&self) -> Option<&Arc<OriginalProcessedRoot>> {
        self.recipe
            .encoding
            .as_ref()?
            .dv_runtime
            .as_ref()?
            .adaptive_terminal
            .get()
            .map(|receipt| &receipt.original)
    }
}

pub(crate) struct AdoptedProcessedTerminal {
    original: Arc<OriginalProcessedRoot>,
}

pub(crate) struct ConfirmedAdoptedProcessedTerminal(());

impl AdoptedProcessedTerminal {
    /// The caller already owns and verified the fresh first-window result.
    /// No pathname, catalog, session lookup or cached recipe can mint this.
    pub(crate) fn register(root: &PreparedRootAdmission) -> Result<Self, String> {
        let runtime = root
            .encoding
            .dv_runtime
            .as_ref()
            .ok_or("missing processed runtime")?;
        let instance = root
            .instance_allowance
            .as_ref()
            .ok_or("missing original instance")?;
        let original = Arc::new(OriginalProcessedRoot {
            driver: Mutex::new(OriginalDriver::AwaitingPublication),
            released_owners: AtomicU8::new(0),
            window_pending: AtomicBool::new(false),
            cancellation: CancellationToken::new(),
        });
        original.bind_owner(&runtime.adaptive_terminal, ProcessedOwner::Runtime)?;
        root.permit.bind_processed_owner(&original)?;
        original.bind_owner(&instance.release, ProcessedOwner::Instance)?;
        Ok(Self { original })
    }

    pub(crate) fn requested_heap_bytes() -> usize {
        std::mem::size_of::<([usize; 2], OriginalProcessedRoot)>()
    }

    /// The original staged owner calls this only when it never invoked
    /// attachment. Its already-settled private qualification may be dropped.
    pub(crate) fn no_attachment_entered(&self) -> Result<(), String> {
        let mut driver = self
            .original
            .driver
            .lock()
            .map_err(|_| "processed root state is uncertain")?;
        if !matches!(*driver, OriginalDriver::AwaitingPublication) {
            return Err("processed root already entered VOD attachment".into());
        }
        *driver = OriginalDriver::NeverAttached;
        Ok(())
    }

    pub(crate) fn original(&self) -> &OriginalProcessedRoot {
        &self.original
    }

    pub(crate) fn physical_retirement_observed(&self) -> bool {
        self.original.driver.lock().is_ok_and(|driver| {
            matches!(
                *driver,
                OriginalDriver::Retired | OriginalDriver::NeverAttached
            )
        })
    }

    pub(crate) fn positively_closed(
        &self,
    ) -> Result<Option<ConfirmedAdoptedProcessedTerminal>, String> {
        let driver = self
            .original
            .driver
            .lock()
            .map_err(|_| "processed terminal state is uncertain")?;
        let preattachment_owners = ProcessedOwner::Runtime as u8
            | ProcessedOwner::Admission as u8
            | ProcessedOwner::Instance as u8;
        let owners = match *driver {
            OriginalDriver::Retired => preattachment_owners | ProcessedOwner::Rendition as u8,
            OriginalDriver::NeverAttached => preattachment_owners,
            _ => return Ok(None),
        };
        Ok((!self.original.window_pending.load(Ordering::Acquire)
            && self.original.released_owners.load(Ordering::Acquire) & owners == owners)
            .then_some(ConfirmedAdoptedProcessedTerminal(())))
    }
}

#[cfg(test)]
#[tokio::test]
async fn processed_retirement_waits_for_its_exact_driver_and_rendition() {
    let base = crate::test_tempdir().expect("original processed test root");
    let other_base = crate::test_tempdir().expect("replacement test root");
    let rendition = super::tests::synthetic_rendition(base.path()).await;
    let other = super::tests::synthetic_rendition(other_base.path()).await;
    let original = Arc::new(OriginalProcessedRoot {
        driver: Mutex::new(OriginalDriver::AwaitingPublication),
        released_owners: AtomicU8::new(0),
        window_pending: AtomicBool::new(false),
        cancellation: CancellationToken::new(),
    });
    let (release, wait) = tokio::sync::oneshot::channel();
    original
        .install_driver(&rendition, || {
            tokio::spawn(async move {
                wait.await.expect("original driver release");
            })
        })
        .expect("original installed driver");
    assert!(!original.owns_rendition(&other));
    assert!(
        original.storage_retired(&rendition).is_err(),
        "a live driver proves no close"
    );
    assert!(!original
        .retire_driver(&rendition)
        .await
        .expect("retirement requested"));
    release.send(()).expect("release the original driver");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !original
            .retire_driver(&rendition)
            .await
            .expect("actual driver join")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original joined");
    assert!(
        original.storage_retired(&other).is_err(),
        "an equal-looking root cannot retire this owner"
    );
    tokio::fs::remove_dir_all(rendition.dir.path())
        .await
        .expect("original private directory removed");
    original
        .storage_retired(&rendition)
        .expect("exact native and storage owner");
    let alias = Arc::clone(&rendition);
    drop(rendition);
    let receipt = ProcessedOwner::Rendition as u8;
    assert_eq!(
        original.released_owners.load(Ordering::Acquire) & receipt,
        0,
        "an escaped response can hold the detached root"
    );
    drop(alias);
    assert_eq!(
        original.released_owners.load(Ordering::Acquire) & receipt,
        receipt
    );
}
