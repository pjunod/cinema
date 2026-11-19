//! Source-service custody remains with the caller that owns the held source.
use super::DecodeFactError;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

pub type ProbeCollection<'a> =
    Pin<Box<dyn Future<Output = Result<(), DecodeFactError>> + Send + 'a>>;

/// The daemon brings its existing foreground reader or background Job owner
/// to this collector. Admission and settlement remain with that exact owner.
#[async_trait::async_trait]
pub trait DecodeFactSourceService: Send + Sync {
    fn register_source(&self, source: Arc<std::fs::File>) -> Result<(), DecodeFactError>;
    fn must_join(&self) -> bool;
    async fn collect(
        &self,
        source: Arc<std::fs::File>,
        #[cfg(target_os = "linux")] pause: Option<(
            crate::process::bounded::SourcePauseController,
            bool,
        )>,
        cancelled: CancellationToken,
        work: ProbeCollection<'_>,
    ) -> Result<(), DecodeFactError>;
}

#[cfg(target_os = "linux")]
pub(super) const MAX_READ: usize = 1024 * 1024;

#[cfg(target_os = "linux")]
pub trait RawProbeRetirement: Send {
    fn declare_spawn(&self, spawn: &mut dyn FnMut());
    fn joined(self: Box<Self>);
}

#[cfg(target_os = "linux")]
pub trait ProbeReadPermission: Send + Sync {
    fn reserve(&self, count: usize, cancel: &CancellationToken) -> Result<(), DecodeFactError>;
    fn raw_retirement(&self) -> Result<Option<Box<dyn RawProbeRetirement>>, DecodeFactError>;
}

#[cfg(target_os = "linux")]
struct Clock {
    control: crate::process::bounded::SourcePauseController,
    paused_before: std::time::Duration,
}

#[cfg(target_os = "linux")]
tokio::task_local! {
    static PERMISSION: Option<Arc<dyn ProbeReadPermission>>;
    static CLOCK: Option<Clock>;
}

#[cfg(target_os = "linux")]
pub async fn with_probe_read_permission<T>(
    permission: Option<Arc<dyn ProbeReadPermission>>,
    work: impl Future<Output = T>,
) -> T {
    PERMISSION.scope(permission, work).await
}

#[cfg(target_os = "linux")]
pub(super) fn read_permission() -> Option<Arc<dyn ProbeReadPermission>> {
    PERMISSION.try_with(Clone::clone).ok().flatten()
}

#[cfg(target_os = "linux")]
pub(super) fn raw_retirement() -> Result<Option<Box<dyn RawProbeRetirement>>, DecodeFactError> {
    read_permission()
        .as_ref()
        .map_or(Ok(None), |permission| permission.raw_retirement())
}

#[cfg(target_os = "linux")]
pub(super) async fn with_deadline_clock<T>(
    control: Option<&crate::process::bounded::SourcePauseController>,
    work: impl Future<Output = T>,
) -> T {
    let clock = control.map(|control| Clock {
        paused_before: control.paused_duration(),
        control: control.clone(),
    });
    CLOCK.scope(clock, work).await
}

#[cfg(target_os = "linux")]
pub(super) fn deadline(original: std::time::Instant) -> std::time::Instant {
    CLOCK
        .try_with(|clock| {
            clock.as_ref().and_then(|clock| {
                original.checked_add(
                    clock
                        .control
                        .paused_duration()
                        .saturating_sub(clock.paused_before),
                )
            })
        })
        .ok()
        .flatten()
        .unwrap_or(original)
}

#[cfg(windows)]
tokio::task_local! { static INPUT_URL: Option<String>; }

#[cfg(windows)]
pub async fn with_probe_input_url<T>(url: Option<String>, work: impl Future<Output = T>) -> T {
    INPUT_URL.scope(url, work).await
}

#[cfg(windows)]
pub(super) fn windows_input_url() -> Option<String> {
    INPUT_URL.try_with(Clone::clone).ok().flatten()
}

#[cfg(target_os = "macos")]
pub type ProbeRangeReader =
    dyn Fn(&mut [u8], u64, &CancellationToken) -> std::io::Result<usize> + Send + Sync;

#[cfg(target_os = "macos")]
tokio::task_local! { static RANGE_READER: Option<Arc<ProbeRangeReader>>; }

#[cfg(target_os = "macos")]
pub async fn with_probe_range_reader<T>(
    reader: Option<Arc<ProbeRangeReader>>,
    work: impl Future<Output = T>,
) -> T {
    RANGE_READER.scope(reader, work).await
}

#[cfg(target_os = "macos")]
pub(super) fn range_reader() -> Option<Arc<ProbeRangeReader>> {
    RANGE_READER.try_with(Clone::clone).ok().flatten()
}
