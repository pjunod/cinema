//! Concrete native factory support and pure finite package projections.
//! Projection creates no native resource, reservation, task or global owner.
use super::finalization_support::{FinalizationSupportLease, SupportPayloadRef};
use super::*;
use crate::adaptive_resources::finalization_support::FinalizationSupportProfile;
use crate::process::contained_launch::{FiniteNativeAllowance, FiniteNativeProfile};
use crate::process::containment::{NativeDomainIdentity, NativeDomainLimits, NativeDomainRoot};

/// One daemon-process owner. The detached original reaper can outlive any
/// request or Tokio executor. Keep its original support lease charged for
/// the same process lifetime, even if this runtime is subsequently replaced.
static NATIVE_FACTORY: tokio::sync::Mutex<Option<NativeFactoryOwner>> =
    tokio::sync::Mutex::const_new(None);

struct NativeFactoryOwner {
    ledger: u64,
    lease: FinalizationSupportLease,
    original: Option<NativeFactoryRef>,
}

pub(super) struct NativeFactoryPayload {
    root: std::io::Result<NativeDomainRoot>,
    path: PathBuf,
    reaper_ready: bool,
}
impl NativeFactoryPayload {
    pub(super) fn open(
        path: &std::path::Path,
        identity: NativeDomainIdentity,
        limits: NativeDomainLimits,
    ) -> Self {
        let root = NativeDomainRoot::open_finite(path, identity, limits);
        let reaper_ready =
            root.is_ok() && crate::process::contained_launch::prepare_reaper().is_ok();
        Self {
            root,
            path: path.to_owned(),
            reaper_ready,
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeFactoryRef(SupportPayloadRef<NativeFactoryPayload>);
impl NativeFactoryRef {
    pub(crate) fn root(&self) -> std::io::Result<&NativeDomainRoot> {
        let root = self
            .0
            .root
            .as_ref()
            .map_err(|error| std::io::Error::from(error.kind()))?;
        if !self.0.reaper_ready {
            return Err(std::io::Error::other("original native reaper unavailable"));
        }
        Ok(root)
    }
    pub(crate) fn verify_finite_limits(&self, expected: NativeDomainLimits) -> std::io::Result<()> {
        self.root()?.verify_finite_limits(&self.0.path, expected)
    }
}

fn factory_support_profile(
    path: &std::path::Path,
) -> Result<FinalizationSupportProfile, AdmissionRefusal> {
    let (factory_bytes, handles, factory_records) = NativeDomainRoot::factory_projection();
    let (reaper_bytes, reaper_records) = crate::process::contained_launch::reaper_projection()
        .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
    let bytes = super::finalization_support::native_factory_support_storage_bytes()
        .and_then(|bytes| bytes.checked_add(factory_bytes))
        .and_then(|bytes| bytes.checked_add(path.as_os_str().len()))
        .and_then(|bytes| bytes.checked_add(reaper_bytes))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
    FinalizationSupportProfile::native_factory(
        ResourceVector {
            peak_memory_bytes: bytes,
            handles,
            ..ResourceVector::default()
        },
        factory_records
            .checked_add(reaper_records)
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?,
        // Original native reaper thread.
        1,
    )
}

pub(super) struct FiniteNativeRoot {
    path: PathBuf,
    identity: NativeDomainIdentity,
    limits: NativeDomainLimits,
}

/// Opaque immutable request selection. Original runtime and captured package
/// must still match at preparation; it grants no native entry or close proof.
pub struct FiniteNativePlan {
    ledger: u64,
    snapshot: Arc<finite_vod::FiniteEngineSnapshot>,
    executable: [u8; 32],
    pub(crate) allowance: FiniteNativeAllowance,
    pub(crate) limits: NativeDomainLimits,
}

impl NodeResourceRuntime {
    /// Immutable startup discovery only: no factory, domain, reservation or
    /// reaper is created. An override names an existing delegated OS location.
    pub fn configure_finite_native_root(&mut self, override_path: Option<PathBuf>) {
        use std::io::Read;
        fn bounded_text(path: &std::path::Path) -> Option<String> {
            let file = std::fs::File::open(path).ok()?;
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 4096 {
                return None;
            }
            String::from_utf8(bytes).ok()
        }
        let candidate = override_path.or_else(|| {
            let membership = bounded_text(std::path::Path::new("/proc/thread-self/cgroup"))?;
            let relative = membership
                .lines()
                .find_map(|line| line.strip_prefix("0::"))?;
            if !relative.starts_with('/') || relative.contains("..") {
                return None;
            }
            Some(PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
        });
        self.native_root = candidate.and_then(|path| {
            if !crate::process::contained_launch::finite_native_startup_available() {
                return None;
            }
            let (identity, limits) = NativeDomainRoot::probe_finite_startup(&path).ok()?;
            Some(FiniteNativeRoot {
                path,
                identity,
                limits,
            })
        });
    }

    /// Pure structural projection from immutable startup metadata and the
    /// already-captured recipe. Repeated calls leave ledger and handles alone.
    pub fn finite_native_plan(
        &self,
        command: &finite_vod::SoftwareAvcCommand,
    ) -> Option<FiniteNativePlan> {
        let limits = self.native_root.as_ref()?.limits;
        Some(FiniteNativePlan {
            ledger: self.ledger.id,
            snapshot: command.engine_snapshot(),
            executable: command.executable().sha256(),
            allowance: finite_graph_allowance(command)?,
            limits,
        })
    }

    /// Explicit actual preparation. Serialize the process-wide constructor
    /// before checking capacity or reserving support. There is no await from
    /// original prepayment through recording activation and exact result.
    pub(super) async fn prepare_native_factory(
        self: &Arc<Self>,
        deadline: Instant,
        continuous: Option<&crate::cluster::clock::ContinuousDeadline>,
    ) -> Result<NativeFactoryRef, AdmissionRefusal> {
        let root = self
            .native_root
            .as_ref()
            .ok_or(AdmissionRefusal::Unavailable)?;
        let mut slot = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            NATIVE_FACTORY.lock(),
        )
        .await
        .map_err(|_| AdmissionRefusal::Unavailable)?;
        if super::entry_window_closed(continuous, deadline) {
            return Err(AdmissionRefusal::Unavailable);
        }
        if let Some(owner) = slot.as_ref() {
            if owner.ledger != self.ledger.id {
                return Err(AdmissionRefusal::Unavailable);
            }
            return owner
                .original
                .as_ref()
                .filter(|original| original.root().is_ok())
                .cloned()
                .ok_or(AdmissionRefusal::Unavailable);
        }
        let profile = factory_support_profile(&root.path)?;
        let epoch = self.refresh_admission_capacity(deadline).await?;
        if super::entry_window_closed(continuous, deadline) {
            return Err(AdmissionRefusal::Unavailable);
        }
        let lease = self.reserve_support(profile, epoch)?;
        let owner = slot.insert(NativeFactoryOwner {
            ledger: self.ledger.id,
            lease,
            original: None,
        });
        owner.original =
            match owner
                .lease
                .activate_native_factory(&root.path, root.identity, root.limits)
            {
                Ok(original) => Some(NativeFactoryRef(original)),
                Err(error) => {
                    owner.lease.mark_native_factory_unknown();
                    return Err(error);
                }
            };
        let original = owner
            .original
            .as_ref()
            .expect("original factory activation recorded");
        if original.root().is_err() {
            owner.lease.mark_native_factory_unknown();
            return Err(AdmissionRefusal::Unavailable);
        }
        Ok(original.clone())
    }
}

fn finite_graph_allowance(
    command: &finite_vod::SoftwareAvcCommand,
) -> Option<FiniteNativeAllowance> {
    let mut allowance = FiniteNativeProfile::allowance(command)?;
    allowance.requested_bytes = allowance.requested_bytes.checked_add(
        u64::try_from(super::physical_graph::finite_native::setup_storage_bytes()).ok()?,
    )?;
    Some(allowance)
}

impl FiniteNativePlan {
    /// Extra handles over the trusted three-pipe root and requested bytes
    /// including the original kernel memory ceiling. The producer claim still
    /// retains its unknown peak.
    pub fn requested_resources(&self) -> Option<(u64, u64)> {
        Some((
            self.allowance.handles.checked_sub(3)?,
            self.allowance
                .requested_bytes
                .checked_add(self.limits.memory_bytes())?,
        ))
    }
    pub(super) fn matches(
        &self,
        runtime: &NodeResourceRuntime,
        command: &finite_vod::SoftwareAvcCommand,
    ) -> bool {
        self.ledger == runtime.ledger.id
            && self.executable == command.executable().sha256()
            && Arc::ptr_eq(&self.snapshot, &command.engine_snapshot())
            && finite_graph_allowance(command) == Some(self.allowance)
            && runtime
                .native_root
                .as_ref()
                .is_some_and(|root| root.limits == self.limits)
    }
}
