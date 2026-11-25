//! Exact ordinary contained graph launch/stop qualification.
use super::*;
use crate::adaptive_resources::runtime::finite_vod::{history_basis_for_trial, SoftwareVodRequest};
use std::io::Read as _;
use std::os::fd::{AsRawFd, BorrowedFd};

// These are a supported operating envelope, not the maximum of samples.
// A profile enters this envelope only through verified output and the known
// final receipt of its original ordinary, deliberately stopped owner.
const STARTUP_BOUND: Duration = Duration::from_secs(1);
const CLEANUP_BOUND: Duration = Duration::from_secs(1);
const QUALIFICATION_AGE: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::adaptive_resources::runtime) struct FiniteTrialKey {
    workload: [u8; 32],
    recipe: [u8; 32],
    executable: [u8; 32],
    limit_epoch: u64,
    native_memory: u64,
    native_tasks: u32,
}

/// Capture under the original application admission lock while the exact
/// ordinary producer is the only protected owner. Other acquisitions change
/// the generation and cannot be hidden by subsequent release.
pub struct FiniteQualificationWindow {
    epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    captured: u64,
}

impl FiniteQualificationWindow {
    pub fn capture(epoch: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
        let captured = epoch.load(std::sync::atomic::Ordering::Acquire);
        Self { epoch, captured }
    }

    fn unchanged(&self) -> bool {
        self.captured != u64::MAX
            && self.epoch.load(std::sync::atomic::Ordering::Acquire) == self.captured
    }
}

#[derive(Default)]
pub(in crate::adaptive_resources::runtime) struct FiniteTimingWitness {
    key: Option<FiniteTrialKey>,
    started: Option<Instant>,
    startup_proved: bool,
    stop_requested: Option<Instant>,
    startup_scope: Option<FiniteQualificationWindow>,
    stop_scope: Option<FiniteQualificationWindow>,
}

impl FiniteTimingWitness {
    pub(in crate::adaptive_resources::runtime) fn new(key: Option<FiniteTrialKey>) -> Self {
        Self {
            key,
            ..Self::default()
        }
    }

    pub(in crate::adaptive_resources::runtime) fn set_start_scope(
        &mut self,
        scope: FiniteQualificationWindow,
    ) {
        if self.started.is_none() && !self.startup_proved {
            self.startup_scope = Some(scope);
        }
    }

    pub(in crate::adaptive_resources::runtime) fn set_stop_scope(
        &mut self,
        scope: FiniteQualificationWindow,
    ) {
        if self.stop_requested.is_none() {
            self.stop_scope = Some(scope);
        }
    }

    pub(in crate::adaptive_resources::runtime) fn outside_supported_io(&mut self) {
        self.key = None;
    }

    pub(in crate::adaptive_resources::runtime) fn start(&mut self, now: Instant) {
        if self.key.is_some() {
            self.started.get_or_insert(now);
        }
    }

    pub(in crate::adaptive_resources::runtime) fn verified_publish(&mut self, now: Instant) {
        if let Some(started) = self.started {
            tracing::debug!(target: "plurx_core::adaptive_resources",
                startup_ms = now.saturating_duration_since(started).as_millis(),
                scope_unchanged = self.startup_scope.as_ref().is_some_and(|scope| scope.unchanged()),
                stopped = self.stop_requested.is_some(),
                "finite trial startup witness");
        }
        if let Some(started) = self.started.take() {
            self.startup_proved = self
                .startup_scope
                .take()
                .is_some_and(|scope| scope.unchanged())
                && self.stop_requested.is_none()
                && now
                    .checked_duration_since(started)
                    .is_some_and(|time| time <= STARTUP_BOUND);
        }
    }

    pub(in crate::adaptive_resources::runtime) fn stop(&mut self, now: Instant) {
        if self.startup_proved {
            self.stop_requested.get_or_insert(now);
        }
    }

    pub(in crate::adaptive_resources::runtime) fn take_stopped(
        &mut self,
    ) -> Option<FiniteStoppedProfile> {
        Some(FiniteStoppedProfile {
            key: self.key.take()?,
            stop_requested: self.stop_requested.take()?,
            scope: self.stop_scope.take()?,
        })
    }
}

pub(in crate::adaptive_resources::runtime) struct FiniteStoppedProfile {
    key: FiniteTrialKey,
    stop_requested: Instant,
    scope: FiniteQualificationWindow,
}

#[derive(Clone, Copy)]
pub(in crate::adaptive_resources::runtime) struct QualifiedFiniteTrial {
    key: FiniteTrialKey,
    verified_at: Instant,
}

impl NodeResourceRuntime {
    /// Only original owners that verify memory-backed source and output
    /// descriptors can qualify this recipe. Matching the key does not prove
    /// current I/O backing, which the next original owner checks again.
    pub(in crate::adaptive_resources::runtime) fn finite_trial_key(
        &self,
        request: &SoftwareVodRequest,
    ) -> Option<FiniteTrialKey> {
        let native = request.native_plan.as_ref()?;
        self.rendition_root.as_ref()?;
        if request.sources.is_empty() {
            return None;
        }
        let (workload, recipe) = history_basis_for_trial(request)?;
        Some(FiniteTrialKey {
            workload,
            recipe,
            executable: request.command.executable().sha256(),
            limit_epoch: request.demand.limit_epoch,
            native_memory: native.limits.memory_bytes(),
            native_tasks: native.limits.tasks(),
        })
    }

    pub(in crate::adaptive_resources::runtime) fn qualify_finalized_finite_stop(
        &self,
        stopped: FiniteStoppedProfile,
        now: Instant,
    ) {
        let Ok(mut policy) = self.finite_policy.lock() else {
            return;
        };
        let index = policy
            .qualifications
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.key == stopped.key));
        let qualified = stopped.scope.unchanged()
            && now
                .checked_duration_since(stopped.stop_requested)
                .is_some_and(|elapsed| elapsed <= CLEANUP_BOUND);
        if !qualified {
            if let Some(index) = index {
                policy.qualifications[index] = None;
            }
            return;
        }
        let index = index.or_else(|| {
            policy.qualifications.iter().position(|entry| {
                entry.is_none_or(|entry| {
                    now.saturating_duration_since(entry.verified_at) > QUALIFICATION_AGE
                })
            })
        });
        if let Some(index) = index {
            policy.qualifications[index] = Some(QualifiedFiniteTrial {
                key: stopped.key,
                verified_at: now,
            });
            tracing::debug!(
                target: "plurx_core::adaptive_resources",
                "original finite native start and close qualified the trial envelope"
            );
        }
    }

    pub(in crate::adaptive_resources::runtime) fn qualified_trial_bounds(
        &self,
        key: FiniteTrialKey,
        now: Instant,
    ) -> Option<(Duration, Duration)> {
        self.finite_policy
            .lock()
            .ok()?
            .qualifications
            .iter()
            .flatten()
            .any(|entry| {
                entry.key == key
                    && now
                        .checked_duration_since(entry.verified_at)
                        .is_some_and(|elapsed| elapsed <= QUALIFICATION_AGE)
            })
            .then_some((STARTUP_BOUND, CLEANUP_BOUND))
    }
}

pub(in crate::adaptive_resources::runtime) fn local_memory_filesystem(
    fd: BorrowedFd<'_>,
    mount: &crate::resource_custody::codec_v2::NativeValue<u64>,
) -> bool {
    let crate::resource_custody::codec_v2::NativeValue::Known { value: mount } = mount else {
        return false;
    };
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the borrowed descriptor remains open and fstatfs initializes the output on success.
    let tmpfs = unsafe {
        libc::fstatfs(fd.as_raw_fd(), stat.as_mut_ptr()) == 0
            && stat.assume_init().f_type == libc::TMPFS_MAGIC
    };
    if !tmpfs {
        return false;
    }
    // The kernel makes noswap immutable for a tmpfs instance. A default
    // tmpfs can still share a block device through swap, even though the
    // child leaf itself has swap disabled. Keep this evidence in the
    // original blocking operation and refuse unavailable/truncated rows.
    let Ok(mut file) = std::fs::File::open("/proc/self/mountinfo") else {
        return false;
    };
    let mut data = [0u8; 32 * 1024];
    let mut read = 0;
    while read < data.len() {
        match file.read(&mut data[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(_) => return false,
        }
    }
    data[..read]
        .split_inclusive(|byte| *byte == b'\n')
        .any(|row| tmpfs_mount_disables_swap(row, *mount))
}

fn tmpfs_mount_disables_swap(row: &[u8], mount: u64) -> bool {
    if !row.ends_with(b"\n") {
        return false;
    }
    let Ok(row) = std::str::from_utf8(row) else {
        return false;
    };
    let Some((prefix, suffix)) = row.split_once(" - ") else {
        return false;
    };
    if prefix
        .split_ascii_whitespace()
        .next()
        .and_then(|id| id.parse().ok())
        != Some(mount)
    {
        return false;
    }
    let mut fields = suffix.split_ascii_whitespace();
    fields.next() == Some("tmpfs")
        && fields
            .nth(1)
            .is_some_and(|options| options.split(',').any(|option| option == "noswap"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    #[test]
    fn tmpfs_scope_requires_complete_same_mount_explicit_noswap() {
        let scope = b"32 20 0:101 / /dir rw,nosuid - tmpfs tmpfs rw,mode=700,noswap\n";
        assert!(tmpfs_mount_disables_swap(scope, 32));
        assert!(!tmpfs_mount_disables_swap(scope, 31));
        assert!(!tmpfs_mount_disables_swap(&scope[..scope.len() - 1], 32));
        assert!(!tmpfs_mount_disables_swap(
            b"32 20 0:101 / /noswap rw,noswap - tmpfs tmpfs rw,mode=700\n",
            32,
        ));
    }

    #[test]
    fn intervening_acquisition_invalidates_exact_start_or_stop_window() {
        let epoch = Arc::new(AtomicU64::new(1));
        let key = FiniteTrialKey {
            workload: [1; 32],
            recipe: [2; 32],
            executable: [3; 32],
            limit_epoch: 1,
            native_memory: 64 * 1024 * 1024,
            native_tasks: 4,
        };
        let start = Instant::now();
        let mut stopped = FiniteTimingWitness::new(Some(key));
        stopped.set_start_scope(FiniteQualificationWindow::capture(Arc::clone(&epoch)));
        stopped.start(start);
        epoch.fetch_add(1, Ordering::Release);
        stopped.verified_publish(start + Duration::from_millis(10));
        stopped.set_stop_scope(FiniteQualificationWindow::capture(Arc::clone(&epoch)));
        stopped.stop(start + Duration::from_millis(20));
        assert!(stopped.take_stopped().is_none());

        let mut stopped = FiniteTimingWitness::new(Some(key));
        stopped.set_start_scope(FiniteQualificationWindow::capture(Arc::clone(&epoch)));
        stopped.start(start);
        stopped.verified_publish(start + Duration::from_millis(10));
        stopped.set_stop_scope(FiniteQualificationWindow::capture(Arc::clone(&epoch)));
        stopped.stop(start + Duration::from_millis(20));
        let proof = stopped
            .take_stopped()
            .expect("same candidate start and stop");
        epoch.fetch_add(1, Ordering::Release);
        assert!(
            !proof.scope.unchanged(),
            "a new owner invalidates qualification through final closure"
        );
    }
}
