//! Source-only filesystem custody. Every real cache open/metadata job owns
//! the actor's counted response guard, independently of its HTTP waiter.
use super::*;
use std::path::Path;

trait ResourceReadHooks: std::any::Any + Send + Sync {
    fn opened(&self, file: &std::fs::File);
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}
struct NoResourceReadHooks;
impl ResourceReadHooks for NoResourceReadHooks {
    fn opened(&self, _: &std::fs::File) {}
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
static NO_RESOURCE_READ_HOOKS: NoResourceReadHooks = NoResourceReadHooks;
pub(super) struct SourceResourceHookOwner {
    slot: crate::seam_hooks::HookSlot<dyn ResourceReadHooks>,
}
impl Default for SourceResourceHookOwner {
    fn default() -> Self {
        Self {
            slot: crate::seam_hooks::HookSlot::new(&NO_RESOURCE_READ_HOOKS),
        }
    }
}

/// Only the actual actor can construct this from its counted response owner.
/// VOD may pass it into a job, but cannot mint authority from a path or boolean.
#[derive(Clone)]
pub(crate) struct SourceResourceReadCustody {
    guard: Arc<SourceResponseGuard>,
    hooks: Arc<SourceResourceHookOwner>,
}
impl SourceResourceReadCustody {
    pub(super) fn new(
        guard: Arc<SourceResponseGuard>,
        hooks: Arc<SourceResourceHookOwner>,
    ) -> Self {
        Self { guard, hooks }
    }
    pub(crate) fn source(&self) -> Result<&crate::fragment_index_cluster::SourceFence, String> {
        self.guard
            .source
            .as_ref()
            .filter(|source| source.unchanged())
            .ok_or_else(|| "Source physical input changed".to_owned())
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.guard
            .owner
            .state
            .lock()
            .expect("Source resource retirement")
            .retirement_requested
            || self.source().is_err()
    }
    pub(crate) async fn cancelled(&self) {
        self.guard.cancelled().await;
    }
    pub(crate) async fn authorize_work(&self) -> Result<SourceGenerationAuthority, String> {
        if self.is_cancelled() {
            return Err("Source resource retired".into());
        }
        let permission = self
            .guard
            .owner
            .gate
            .retained_ingress(self.guard.assignment())
            .ok_or_else(|| "Source resource assignment changed".to_owned())?;
        let authority = self
            .guard
            .owner
            .gate
            .authorize_generation(&[permission])
            .await?;
        if self.is_cancelled() {
            return Err("Source resource retired during authorization".into());
        }
        authority.validate_before_spawn()?;
        Ok(authority)
    }
    /// All actual cache observations retain the counted Source guard, even if
    /// their async waiter goes away while the filesystem job is blocked.
    pub(crate) async fn read_subtitle(
        &self,
        path: &Path,
        cap: usize,
    ) -> Result<Option<Vec<u8>>, String> {
        let custody = self.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            custody.source()?;
            let _pin = crate::playback_cache::pin(&path).map_err(|error| error.to_string())?;
            let mut file = match plurx_core::fs_secure::open_read_nofollow_blocking(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            custody.hooks.slot.get().opened(&file);
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() > cap as u64 {
                return Ok(None);
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(cap as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            custody.source()?;
            if bytes.len() > cap || custody.is_cancelled() {
                return Err("Source subtitle read expired or exceeded its bound".into());
            }
            Ok(Some(bytes))
        })
        .await
        .map_err(|error| format!("Source subtitle cache read: {error}"))?
    }
    pub(crate) async fn publish_subtitle(&self, path: &Path, bytes: Vec<u8>) -> Result<(), String> {
        let reservation =
            crate::playback_cache::reserve(path, (bytes.len() as u64).saturating_mul(2))
                .await
                .map_err(|error| format!("Source subtitle disk admission: {error}"))?;
        let size = bytes.len() as u64;
        let custody = self.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            custody.source()?;
            if custody.is_cancelled() {
                return Err("Source resource retired".to_owned());
            }
            let directory = path
                .parent()
                .ok_or("Source subtitle cache directory missing")?;
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
            let mut temporary = tempfile::Builder::new()
                .prefix(".tmp-source-caption-")
                .tempfile_in(directory)
                .map_err(|error| error.to_string())?;
            let _temporary_pin =
                crate::playback_cache::pin(temporary.path()).map_err(|error| error.to_string())?;
            temporary
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            custody.source()?;
            if custody.is_cancelled() {
                return Err("Source resource retired".to_owned());
            }
            temporary
                .persist(&path)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .await
        .map_err(|error| format!("Source subtitle cache publication: {error}"))??;
        if let Some(reservation) = reservation {
            reservation.commit(size);
        }
        Ok(())
    }
    pub(crate) async fn open_ready(
        &self,
        path: &Path,
        etag_stem: &str,
        delivery: &Arc<crate::meter::Meter>,
    ) -> std::io::Result<crate::vodserve::SegmentReady> {
        let cache_pin = crate::playback_cache::pin(path)?;
        let path = path.to_owned();
        let custody = self.clone();
        let (file, len) = tokio::task::spawn_blocking(move || {
            if custody
                .guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
            {
                return Err(std::io::Error::other(
                    "Source physical input changed before resource open",
                ));
            }
            let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path)?;
            custody.hooks.slot.get().opened(&file);
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || custody
                    .guard
                    .source
                    .as_ref()
                    .is_none_or(|source| !source.unchanged())
            {
                return Err(std::io::Error::other(
                    "Source resource or input changed during observation",
                ));
            }
            Ok((file, metadata.len()))
        })
        .await
        .map_err(|_| std::io::Error::other("Source resource observation job failed"))??;
        Ok(crate::vodserve::SegmentReady {
            // The materialized rendition caller fills this from its immutable
            // plan/index after opening; init resources have no media duration.
            observed_media_duration_ms: None,
            // Source's actual response/producer custody owns this file. Local
            // retained-cache admission cannot confer authority in this namespace.
            retained_lease: None,
            cache_pin,
            file: tokio::fs::File::from_std(file),
            len,
            etag: format!("{etag_stem}-{len}"),
            delivery: Arc::clone(delivery),
        })
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct SourceResourceJobPause {
    opened: std::sync::atomic::AtomicBool,
    #[cfg(unix)]
    descriptor: std::sync::Mutex<Option<JobDescriptor>>,
    changed: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}
#[cfg(test)]
impl ResourceReadHooks for SourceResourceJobPause {
    fn opened(&self, file: &std::fs::File) {
        // Only Unix has a side-effect-free census of one exact descriptor.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            *self
                .descriptor
                .lock()
                .expect("actual Source job descriptor") = Some(JobDescriptor {
                number: file.as_raw_fd(),
                pinned: file.try_clone().expect("pin the actual Source job file"),
            });
        }
        #[cfg(not(unix))]
        let _ = file;
        self.opened
            .store(true, std::sync::atomic::Ordering::Release);
        self.changed.notify_waiters();
        let mut released = self.released.lock().expect("actual Source read job pause");
        while !*released {
            released = self
                .wake
                .wait(released)
                .expect("actual Source read job resume");
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
#[cfg(test)]
pub(super) struct SourceResourceJobHeld<'a>(&'a SourceResourceJobPause);
#[cfg(test)]
impl Drop for SourceResourceJobHeld<'_> {
    fn drop(&mut self) {
        *self
            .0
            .released
            .lock()
            .expect("actual Source read job release") = true;
        self.0.wake.notify_all();
    }
}
#[cfg(test)]
impl SourceResourceJobPause {
    pub(super) async fn reached(&self) -> SourceResourceJobHeld<'_> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.opened.load(std::sync::atomic::Ordering::Acquire) {
                return SourceResourceJobHeld(self);
            }
            notified.await;
        }
    }
    /// Whether the descriptor the parked job opened is still open.
    #[cfg(unix)]
    pub(super) fn job_descriptor_open(&self) -> bool {
        self.descriptor
            .lock()
            .expect("actual Source job descriptor")
            .as_ref()
            .expect("the parked job recorded its descriptor")
            .is_open()
    }
}
/// The parked job's descriptor number, with a duplicate that keeps the file it
/// names allocated. The census compares the file behind the number with the
/// pinned one, so once the job closes its descriptor a number that a parallel
/// test reopened on another file can never read as the job's descriptor.
#[cfg(all(test, unix))]
struct JobDescriptor {
    number: i32,
    pinned: std::fs::File,
}
#[cfg(all(test, unix))]
impl JobDescriptor {
    // libc's dev_t is i32 on macOS and u64 on Linux; MetadataExt uses u64.
    #[allow(clippy::unnecessary_cast)]
    fn is_open(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        let mut current = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat only writes the stat buffer; an invalid descriptor
        // number returns an error and leaves nothing to read.
        if unsafe { libc::fstat(self.number, current.as_mut_ptr()) } != 0 {
            return false;
        }
        // SAFETY: fstat succeeded, so it initialised the buffer.
        let current = unsafe { current.assume_init() };
        let pinned = self.pinned.metadata().expect("pinned Source job file");
        current.st_dev as u64 == pinned.dev() && current.st_ino as u64 == pinned.ino()
    }
}
#[cfg(test)]
impl SourceResourceHookOwner {
    pub(super) fn pause(&self) -> &SourceResourceJobPause {
        self.slot
            .get_or_install(|| Box::new(SourceResourceJobPause::default()))
            .as_any()
            .downcast_ref::<SourceResourceJobPause>()
            .expect("actual Source file job hook")
    }
}
