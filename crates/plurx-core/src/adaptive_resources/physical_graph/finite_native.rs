//! The finite native backend uses the existing durable root and writer graph.
use super::*;
use crate::adaptive_resources::runtime::finite_native::NativeFactoryRef;
use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcCommand;
use crate::cluster::clock::ContinuousDeadline;
use crate::process::contained_launch::{
    FiniteNativeAllowance, FiniteNativeProfile, LaunchFailure, LaunchRefusal,
};
use crate::process::containment::{NativeDomainCreateFailure, NativeDomainLimits};
use std::os::fd::RawFd;

pub(in crate::adaptive_resources) struct NativeRootEntry<'a> {
    pub factory: &'a NativeFactoryRef,
    pub command: &'a SoftwareAvcCommand,
    pub descriptors: &'a [(RawFd, i32)],
    pub allowance: FiniteNativeAllowance,
    pub limits: NativeDomainLimits,
    pub deadline: Instant,
    pub continuous: Option<&'a ContinuousDeadline>,
}

#[cfg(all(test, feature = "fixtures", target_arch = "x86_64"))]
tokio::task_local! {
    pub(crate) static REFUSE_AFTER_NATIVE_SETUP: ();
}

impl NativeRootEntry<'_> {
    fn check_cutoff(&self) -> io::Result<()> {
        if crate::adaptive_resources::runtime::entry_window_closed(self.continuous, self.deadline) {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native root entry cutoff elapsed",
            ))
        } else {
            Ok(())
        }
    }
}

pub(super) struct NativeSetup {
    pub(super) allowance: FiniteNativeAllowance,
    _factory: NativeFactoryRef,
    profile: FiniteNativeProfile,
    refusal: Option<PrecloneRefusal>,
}

enum PrecloneRefusal {
    ProfileOnly,
    Factory(NativeDomainCreateFailure),
    Launch(LaunchFailure),
}

impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) fn finite_trial_hard_pressure(&self) -> io::Result<bool> {
        let custody = self.custody();
        let members = custody
            .members
            .try_lock()
            .map_err(|_| io::Error::other("original finite native graph is busy"))?;
        let [original] = members.roots.as_slice() else {
            return Err(io::Error::other(
                "original finite trial root is unavailable",
            ));
        };
        original.child.finite_trial_hard_pressure()
    }

    pub(super) async fn spawn_finite_native_root(
        &mut self,
        work: ChildWork,
        arm: super::super::runtime::DurableArm,
        entered: &mut bool,
        request: NativeRootEntry<'_>,
    ) -> io::Result<GraphRoot> {
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        request.check_cutoff()?;
        if custody.sealed.load(Ordering::Acquire)
            || *entered
            || !members.roots.is_empty()
            || members.native_setup.is_some()
        {
            return Err(io::Error::other(
                "selected native root launch is already entered or sealed",
            ));
        }
        let allowance = FiniteNativeAllowance {
            handles: request.allowance.handles,
            requested_bytes: request
                .allowance
                .requested_bytes
                .checked_add(request.limits.memory_bytes())
                .ok_or_else(|| io::Error::other("finite native memory allowance overflows"))?,
        };
        budgets::check(
            &self.ledger,
            custody,
            &members,
            budgets::BudgetDelta {
                roots: 1,
                native_root: Some(allowance),
                ..Default::default()
            },
        )?;
        check_kernel_task_capacity(&self.ledger, custody.binding, request.limits)?;
        members
            .roots
            .try_reserve_exact(1)
            .map_err(io::Error::other)?;
        begin_acquisition_with_arm(&self.ledger, custody, &mut members, Some(&arm))?;
        *entered = true;
        members.native_setup = Some(Box::new(NativeSetup {
            allowance,
            _factory: request.factory.clone(),
            profile: FiniteNativeProfile::empty(),
            refusal: None,
        }));
        let result = enter_original_root(custody, &mut members, request, work);
        if result.is_err() {
            members.uncertain_launch = true;
            retain_or_release_unacquired(&self.ledger, custody);
            let _ = cleanup_partial(&mut members);
        }
        result
    }
}

fn check_kernel_task_capacity(
    ledger: &ResourceLedger,
    binding: ReservationBinding,
    limits: NativeDomainLimits,
) -> io::Result<()> {
    let state = ledger
        .inner
        .lock()
        .map_err(|_| io::Error::other("resource ledger unavailable"))?;
    let original = state
        .entries
        .get(&binding.reservation)
        .filter(|entry| entry.binding == binding)
        .ok_or_else(|| io::Error::other("original native root reservation unavailable"))?;
    let support = state
        .native_factory_support
        .filter(|support| support.phase == super::super::FinalizationSupportPhase::Active)
        .ok_or_else(|| io::Error::other("original native factory is not active"))?;
    // The one standing reaper must fit even when the fresh observation
    // predates its activation by a fraction of the sample window.
    let required_tasks = u64::from(limits.tasks()) + u64::from(support.profile.task_slots());
    // Existing durable UnknownProducerPeak task claims serialize all tracked
    // root producers, including UnknownHeld and restarted obligations. This
    // sample checks current headroom without converting root count into a
    // prepaid thread count or accounting for independent external tasks.
    if original.resources.processes != 1
        || original.memory_claim != super::super::MemoryClaim::UnknownProducerPeak
        || state
            .entries
            .values()
            .any(|entry| entry.binding != binding && entry.resources.processes > 0)
        || state
            .capacity_observed_at
            .is_none_or(|at| at.elapsed() > super::super::finalization_support::FRESH_FOR)
        || !matches!(state.cap.kernel_tasks, Metric::Known(tasks) if tasks >= required_tasks)
        || !matches!(state.cap.memory_bytes, Metric::Known(bytes) if bytes >= state.used.peak_memory_bytes)
        || !matches!(state.cap.handles, Metric::Known(handles) if handles >= state.used.handles)
    {
        return Err(io::Error::other(
            "native root exceeds original exclusive capacity",
        ));
    }
    Ok(())
}

pub(in crate::adaptive_resources) fn setup_storage_bytes() -> usize {
    // Profile and original child storage are projected separately. The
    // attempt also reserves its retained setup/failure record before entry.
    std::mem::size_of::<NativeSetup>() - std::mem::size_of::<FiniteNativeProfile>()
}

fn enter_original_root(
    custody: &GraphCustody,
    members: &mut Members,
    request: NativeRootEntry<'_>,
    work: ChildWork,
) -> io::Result<GraphRoot> {
    let setup = members
        .native_setup
        .as_mut()
        .expect("registered original native setup");
    request.factory.verify_finite_limits(request.limits)?;
    if let Err(error) = request.check_cutoff() {
        setup.refusal = Some(PrecloneRefusal::ProfileOnly);
        return Err(error);
    }
    if let Err(error) = setup
        .profile
        .prepare_in_place(request.command, request.descriptors)
    {
        setup.refusal = Some(PrecloneRefusal::ProfileOnly);
        return Err(error);
    }
    if let Err(error) = request.check_cutoff() {
        setup.refusal = Some(PrecloneRefusal::ProfileOnly);
        return Err(error);
    }
    request.factory.verify_finite_limits(request.limits)?;
    let domain = match request.factory.root()?.create_domain(request.limits) {
        Ok(domain) => domain,
        Err(failure) => {
            setup.refusal = Some(PrecloneRefusal::Factory(failure));
            let Some(PrecloneRefusal::Factory(failure)) = setup.refusal.as_ref() else {
                unreachable!("same retained factory refusal");
            };
            return Err(io::Error::other(failure.to_string()));
        }
    };
    let cutoff = request.check_cutoff();
    #[cfg(all(test, feature = "fixtures", target_arch = "x86_64"))]
    let cutoff = if REFUSE_AFTER_NATIVE_SETUP.try_with(|()| ()).is_ok() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "fixture refusal after original native domain setup",
        ))
    } else {
        cutoff
    };
    if let Err(error) = cutoff {
        setup.refusal = Some(PrecloneRefusal::Launch(LaunchFailure {
            domain,
            reason: LaunchRefusal::Refused(io::Error::new(
                error.kind(),
                "cutoff after native domain setup",
            )),
            setup_close_uncertain: false,
        }));
        return Err(error);
    }
    let child = match setup.profile.launch(domain, work) {
        Ok(child) => child,
        Err(failure) => {
            let error = match &failure.reason {
                LaunchRefusal::Unavailable(error) | LaunchRefusal::Refused(error) => {
                    io::Error::new(error.kind(), error.to_string())
                }
            };
            setup.refusal = Some(PrecloneRefusal::Launch(failure));
            return Err(error);
        }
    };
    // The original domain/pidfd/pipes are in the graph before either the
    // registration duplication or a consuming Tokio conversion can fail.
    let mut root = retain_root(
        custody,
        members,
        child::GraphChild::from_native(child),
        None,
    );
    let owned = &mut members.roots[root.index];
    let pid = owned
        .child
        .id()
        .ok_or_else(|| io::Error::other("original native pid unavailable"))?;
    let fd = owned.child.native_pidfd()?;
    let job =
        ChildJob::register_original_native(pid, fd, request.command.program().as_os_str(), work);
    root.control = job.registered_control();
    owned.job = Some(job);
    owned.child.install_native_pipes()?;
    Ok(root)
}

impl SealedPhysicalGraph {
    pub(in crate::adaptive_resources) async fn reap_refused_finite_native_root(
        &self,
    ) -> io::Result<bool> {
        let mut members = self.custody.members.lock().await;
        if !members.uncertain_launch || members.native_setup.is_none() {
            return Ok(false);
        }
        cleanup_partial(&mut members)?;
        Ok(true)
    }
}

fn cleanup_partial(members: &mut Members) -> io::Result<()> {
    let Some(setup) = members.native_setup.as_mut() else {
        return Ok(());
    };
    // A refused setup may have acquired real files before creating a child.
    // Reap only original handles while keeping the graph's full UnknownHeld.
    if let Some(refusal) = setup.refusal.as_mut() {
        match refusal {
            PrecloneRefusal::ProfileOnly => {}
            PrecloneRefusal::Factory(failure) => {
                failure.retry_cleanup()?;
            }
            PrecloneRefusal::Launch(failure) => {
                failure.try_cleanup()?;
            }
        }
    }
    if let Some(root) = members.roots.first_mut() {
        let kill = root.child.start_kill();
        let status = match root.exit_status {
            Some(status) => Some(status),
            None => root.child.try_wait()?,
        };
        let Some(status) = status else {
            return kill;
        };
        root.waited = true;
        root.exit_status = Some(status);
        if !root.child.reap_refused_native()? {
            return kill;
        }
        if let Some(job) = root.job.take() {
            job.close_original_native()?;
        }
    }
    setup.profile.close_original_objects()?;
    Ok(())
}

impl NativeSetup {
    /// Only an original synchronous refusal from a known preclone step can
    /// enter this branch. A lost owner or any successfully cloned root must
    /// stay outside this receipt, even if best-effort reaping later works.
    pub(super) fn close_refused_preclone_setup(&mut self) -> io::Result<bool> {
        let checked = match self.refusal.as_mut() {
            Some(PrecloneRefusal::ProfileOnly) => true,
            Some(PrecloneRefusal::Factory(failure)) => failure.close_checked_preclone_setup()?,
            Some(PrecloneRefusal::Launch(failure)) => failure.try_cleanup()?,
            None => false,
        };
        if !checked {
            return Ok(false);
        }
        self.profile.close_original_objects()?;
        Ok(true)
    }
}

pub(super) fn close_original_root(members: &mut Members, index: usize) -> io::Result<()> {
    let root = &mut members.roots[index];
    if !root.child.is_native() {
        return Ok(());
    }
    if !root.child.try_close_native()? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "original native domain remains live",
        ));
    }
    let setup = members
        .native_setup
        .as_mut()
        .ok_or_else(|| io::Error::other("original native setup missing"))?;
    setup.profile.close_original_objects()?;
    let job = root
        .job
        .as_ref()
        .ok_or_else(|| io::Error::other("original native control unavailable"))?;
    if let Err(error) = job.close_original_native() {
        members.uncertain_launch = true;
        return Err(error);
    }
    members.native_setup = None;
    Ok(())
}
