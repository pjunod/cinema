//! Source recovery observes every production child launch before input escapes.
use std::future::Future;
use std::io;
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
static ADMISSION: RwLock<()> = RwLock::new(());

const DESCRIPTOR_HANDOFF: &str = "PLURX_INTERNAL_SOURCE_DESCRIPTOR_HANDOFF";

type Observer = fn(bool) -> io::Result<()>;
static OBSERVER: OnceLock<Observer> = OnceLock::new();
tokio::task_local! { static MEDIATED: bool; }

pub fn install_observer(observer: Observer) -> io::Result<()> {
    let installed = OBSERVER.get_or_init(|| observer);
    if !std::ptr::fn_addr_eq(*installed, observer) {
        return Err(io::Error::other(
            "source child observer already configured differently",
        ));
    }
    Ok(())
}

/// Audited callers must withhold original descriptors and paths from children.
/// Detached tasks do not inherit the declaration and remain conservative.
pub async fn mediated<T>(work: impl Future<Output = T>) -> T {
    MEDIATED.scope(true, work).await
}

/// The synchronous spawn half of an audited mediated operation. Callers must
/// withhold all original source descriptors and paths, as for [`mediated`].
pub fn mediated_spawn<T>(spawn: impl FnOnce() -> T) -> T {
    MEDIATED.sync_scope(true, spawn)
}

/// Descriptor handoff always disqualifies daemon-only proof, even if a caller
/// also rewrites argv to an HTTP source URL. Call before installing inheritance.
pub fn unmediated_descriptor() -> io::Result<()> {
    OBSERVER.get().map_or(Ok(()), |observer| observer(false))
}

pub fn registration() -> io::Result<RwLockWriteGuard<'static, ()>> {
    ADMISSION
        .write()
        .map_err(|_| io::Error::other("source registration barrier poisoned"))
}

pub(super) fn before_spawn(descriptor_handoff: bool) -> io::Result<RwLockReadGuard<'static, ()>> {
    let guard = ADMISSION
        .read()
        .map_err(|_| io::Error::other("source spawn barrier poisoned"))?;
    if let Some(observer) = OBSERVER.get() {
        observer(!descriptor_handoff && MEDIATED.try_with(|value| *value).unwrap_or(false))?;
    }
    Ok(guard)
}

/// Preserve this private launch metadata through environment sanitization. It
/// is stripped at the actual launcher and is never delivered to a child.
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
