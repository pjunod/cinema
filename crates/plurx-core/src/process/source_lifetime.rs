//! Source recovery observes every production child launch before input escapes.
//! An access declaration preserves witnesses only after the actual command and
//! the single builder's pinned descriptor manifest pass fail-closed checks.
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

mod arguments;
mod descriptors;
mod policy;
pub use arguments::ArgumentProfile;
pub use descriptors::{
    register_daemon_file, DaemonFileRegistration, DescriptorManifest, InheritedFd, StdioFacts,
    StdioMode,
};
pub use policy::{
    configure_no_input_probe, daemon_roots_configured, is_registered_broker, register_broker,
    set_daemon_roots, BrokerRegistration,
};

static ADMISSION: RwLock<()> = RwLock::new(());
const DESCRIPTOR_HANDOFF: &str = "PLURX_INTERNAL_SOURCE_DESCRIPTOR_HANDOFF";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAccess {
    #[default]
    Unknown,
    NoSource,
    MetadataOnly,
    Brokered,
}
#[derive(Clone, Copy)]
struct Declaration {
    access: SourceAccess,
    tuner_schemes: &'static [&'static str],
}
impl Declaration {
    const fn new(access: SourceAccess) -> Self {
        Self {
            access,
            tuner_schemes: &[],
        }
    }
}
tokio::task_local! { static ACCESS: Declaration; static MANIFEST: Arc<DescriptorManifest>; }

/// Access at admission. `mediated` is the compatibility safety bit consumed
/// by the existing reader observer: true only for a verified non-raw launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchObservation {
    pub mediated: bool,
    pub descriptor_handoff: bool,
    pub purpose: &'static str,
    pub launch: bool,
    pub declared: SourceAccess,
    pub verified: bool,
    pub reason: Option<&'static str>,
}
impl LaunchObservation {
    pub const fn unknown(purpose: &'static str, descriptor_handoff: bool, launch: bool) -> Self {
        Self {
            mediated: false,
            descriptor_handoff,
            purpose,
            launch,
            declared: SourceAccess::Unknown,
            verified: false,
            reason: Some("undeclared"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UnknownLaunch {
    pub purpose: &'static str,
    pub observed_at_ms: u64,
}
/// Counts admission attempts, including refused/failed launches. `mediated`
/// remains the brokered count; other verified classes have explicit counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct LaunchSnapshot {
    pub mediated: u64,
    pub unknown: u64,
    pub verified: u64,
    pub no_source: u64,
    pub metadata_only: u64,
    pub brokered: u64,
    pub unverified: u64,
    pub latest_unknown: Option<UnknownLaunch>,
}
static LAUNCHES: Mutex<LaunchSnapshot> = Mutex::new(LaunchSnapshot {
    mediated: 0,
    unknown: 0,
    verified: 0,
    no_source: 0,
    metadata_only: 0,
    brokered: 0,
    unverified: 0,
    latest_unknown: None,
});
pub fn launch_snapshot() -> LaunchSnapshot {
    *LAUNCHES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn record_launch(observation: &LaunchObservation) {
    let mut snapshot = LAUNCHES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if observation.verified {
        snapshot.verified = snapshot.verified.saturating_add(1);
        let count = match observation.declared {
            SourceAccess::NoSource => &mut snapshot.no_source,
            SourceAccess::MetadataOnly => &mut snapshot.metadata_only,
            SourceAccess::Brokered => &mut snapshot.brokered,
            SourceAccess::Unknown => unreachable!("Unknown can never verify"),
        };
        *count = count.saturating_add(1);
        snapshot.mediated = snapshot.brokered;
    } else {
        snapshot.unknown = snapshot.unknown.saturating_add(1);
        if observation.declared != SourceAccess::Unknown {
            snapshot.unverified = snapshot.unverified.saturating_add(1);
        }
        snapshot.latest_unknown = Some(UnknownLaunch {
            purpose: observation.purpose,
            observed_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        });
    }
}

type Observer = fn(&LaunchObservation) -> io::Result<()>;
static OBSERVER: OnceLock<Observer> = OnceLock::new();
pub fn install_observer(observer: Observer) -> io::Result<()> {
    let installed = OBSERVER.get_or_init(|| observer);
    if !std::ptr::fn_addr_eq(*installed, observer) {
        return Err(io::Error::other(
            "source child observer already configured differently",
        ));
    }
    Ok(())
}

pub async fn no_source<T>(work: impl Future<Output = T>) -> T {
    ACCESS
        .scope(Declaration::new(SourceAccess::NoSource), work)
        .await
}
pub fn no_source_spawn<T>(spawn: impl FnOnce() -> T) -> T {
    ACCESS.sync_scope(Declaration::new(SourceAccess::NoSource), spawn)
}
pub async fn metadata_only<T>(work: impl Future<Output = T>) -> T {
    ACCESS
        .scope(Declaration::new(SourceAccess::MetadataOnly), work)
        .await
}
pub fn metadata_only_spawn<T>(spawn: impl FnOnce() -> T) -> T {
    ACCESS.sync_scope(Declaration::new(SourceAccess::MetadataOnly), spawn)
}
pub async fn brokered<T>(work: impl Future<Output = T>) -> T {
    ACCESS
        .scope(Declaration::new(SourceAccess::Brokered), work)
        .await
}
pub fn brokered_spawn<T>(spawn: impl FnOnce() -> T) -> T {
    ACCESS.sync_scope(Declaration::new(SourceAccess::Brokered), spawn)
}
/// Compatibility aliases retain declaration scope, never bypass verification.
pub async fn mediated<T>(work: impl Future<Output = T>) -> T {
    brokered(work).await
}
pub fn mediated_spawn<T>(spawn: impl FnOnce() -> T) -> T {
    brokered_spawn(spawn)
}

/// Only the named Live TV purposes can use these declared network schemes;
/// every other launch remains subject to exact live loopback broker matching.
pub async fn no_source_tuner<T>(
    schemes: &'static [&'static str],
    work: impl Future<Output = T>,
) -> T {
    ACCESS
        .scope(
            Declaration {
                access: SourceAccess::NoSource,
                tuner_schemes: schemes,
            },
            work,
        )
        .await
}
pub fn no_source_tuner_spawn<T>(schemes: &'static [&'static str], spawn: impl FnOnce() -> T) -> T {
    ACCESS.sync_scope(
        Declaration {
            access: SourceAccess::NoSource,
            tuner_schemes: schemes,
        },
        spawn,
    )
}

/// Called by the single descriptor builder around only its final launcher.
/// Detached tasks inherit neither declaration nor manifest.
pub fn with_descriptors<T>(manifest: Arc<DescriptorManifest>, spawn: impl FnOnce() -> T) -> T {
    MANIFEST.sync_scope(manifest, spawn)
}
pub async fn with_descriptors_async<T>(
    manifest: Arc<DescriptorManifest>,
    work: impl Future<Output = T>,
) -> T {
    MANIFEST.scope(manifest, work).await
}

/// Core-only probe adapter: it wires actual null/piped stdio and binds the
/// empty manifest. File/inherited stdio must use the single daemon builder.
pub fn prepare_stdio(
    command: &mut tokio::process::Command,
    facts: StdioFacts,
) -> io::Result<Arc<DescriptorManifest>> {
    prepare_stdio_std(command.as_std_mut(), facts)
}
pub fn prepare_stdio_std(
    command: &mut std::process::Command,
    facts: StdioFacts,
) -> io::Result<Arc<DescriptorManifest>> {
    fn stream(mode: StdioMode) -> io::Result<std::process::Stdio> {
        match mode {
            StdioMode::Null => Ok(std::process::Stdio::null()),
            StdioMode::Piped => Ok(std::process::Stdio::piped()),
            _ => Err(io::Error::other(
                "nonempty stdio requires the descriptor builder",
            )),
        }
    }
    command
        .stdin(stream(facts.stdin)?)
        .stdout(stream(facts.stdout)?)
        .stderr(stream(facts.stderr)?);
    let manifest = Arc::new(DescriptorManifest::new(Vec::new(), facts, None)?);
    manifest.bind(command)?;
    Ok(manifest)
}

pub fn unmediated_descriptor() -> io::Result<()> {
    OBSERVER.get().map_or(Ok(()), |observer| {
        observer(&LaunchObservation::unknown(
            "descriptor handoff",
            true,
            false,
        ))
    })
}
pub fn registration() -> io::Result<RwLockWriteGuard<'static, ()>> {
    ADMISSION
        .write()
        .map_err(|_| io::Error::other("source registration barrier poisoned"))
}
pub(super) fn before_spawn(
    command: &mut std::process::Command,
    purpose: &'static str,
) -> io::Result<RwLockReadGuard<'static, ()>> {
    let guard = ADMISSION
        .read()
        .map_err(|_| io::Error::other("source spawn barrier poisoned"))?;
    let declaration = ACCESS
        .try_with(|value| *value)
        .unwrap_or(Declaration::new(SourceAccess::Unknown));
    let manifest = MANIFEST.try_with(Arc::clone).ok();
    let verdict = if declaration.access == SourceAccess::Unknown {
        Err(policy::Refusal {
            check: "undeclared",
            enforce: false,
        })
    } else {
        policy::verify(
            command,
            declaration.access,
            manifest.as_deref(),
            purpose,
            declaration.tuner_schemes,
        )
    };
    // Only positively verified, no-input profiles receive this environment.
    // Do it here, after checking the original bound command and immediately
    // before the actual launcher. Neither inherited nor caller loader/runtime
    // overrides may execute code outside the audited probe profile. Unknown
    // media launches retain their environment and continue unqualified.
    if verdict.is_ok()
        && matches!(
            declaration.access,
            SourceAccess::NoSource | SourceAccess::MetadataOnly
        )
    {
        command.env_clear().env("LANG", "C").env("LC_ALL", "C");
    }
    let observation = LaunchObservation {
        mediated: verdict.is_ok(),
        descriptor_handoff: has_descriptor_handoff(command),
        purpose,
        launch: true,
        declared: declaration.access,
        verified: verdict.is_ok(),
        reason: verdict.as_ref().err().map(|error| error.check),
    };
    record_launch(&observation);
    // Poison before any debug refusal. No failed declaration can retain a
    // clean witness, even if a caller catches the debug panic.
    if let Some(observer) = OBSERVER.get() {
        observer(&observation)?;
    }
    if let Err(refusal) = verdict {
        if declaration.access != SourceAccess::Unknown {
            tracing::error!(
                purpose,
                check = refusal.check,
                "source access declaration was not verified"
            );
            if refusal.enforce && cfg!(debug_assertions) {
                panic!(
                    "source access declaration failed: purpose={purpose}, check={}",
                    refusal.check
                );
            }
        }
    }
    Ok(guard)
}

pub fn tag_descriptor_handoff(command: &mut tokio::process::Command) {
    command.env(DESCRIPTOR_HANDOFF, "1");
}
pub fn has_descriptor_handoff(command: &std::process::Command) -> bool {
    command
        .get_envs()
        .any(|(name, value)| name == DESCRIPTOR_HANDOFF && value.is_some())
}
pub(super) fn strip_descriptor_handoff(command: &mut std::process::Command) {
    command.env_remove(DESCRIPTOR_HANDOFF);
}
pub(super) fn restore_descriptor_handoff(command: &mut std::process::Command) {
    command.env(DESCRIPTOR_HANDOFF, "1");
}
