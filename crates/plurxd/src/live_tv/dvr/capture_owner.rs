//! Actual capture acquisition, native operations and original task custody.
//! No inventory/physical receipt or complete memory-profile claim is made here.
use super::{SinkWriter, SinkWriterOutcome};
use crate::serving_fence::RestartAdmission;
use futures_util::FutureExt;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::task::JoinHandle;

// This is an admission bound, not a KnownMemoryPeak. Unknown originals retain
// their occupied slot. A full table refuses before any file/native operation.
const CAPTURE_SLOTS: usize = 64;
const TRANSPORT_SLOTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JoinState {
    Pending,
    Joined,
    Unknown,
}

pub(super) struct OriginalTask<T> {
    state: Mutex<TaskState<T>>,
    retired: AtomicBool,
}
struct TaskState<T> {
    original: Option<JoinHandle<T>>,
    joined: bool,
    failure: Option<tokio::task::JoinError>,
}
impl<T> Default for OriginalTask<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(TaskState {
                original: None,
                joined: false,
                failure: None,
            }),
            retired: AtomicBool::new(false),
        }
    }
}
impl<T> OriginalTask<T> {
    pub(super) fn install(&self, original: JoinHandle<T>) {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(s.original.is_none() && !s.joined && s.failure.is_none());
        s.original = Some(original);
    }
    #[cfg(test)]
    pub(super) fn identity(&self) -> Option<tokio::task::Id> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .original
            .as_ref()
            .map(JoinHandle::id)
    }
    pub(super) fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }
    pub(super) fn observe(&self) -> JoinState {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.failure.is_some() {
            return JoinState::Unknown;
        }
        if s.joined {
            return JoinState::Joined;
        }
        let Some(task) = s.original.as_mut() else {
            return JoinState::Pending;
        };
        match task.now_or_never() {
            None => JoinState::Pending,
            Some(Ok(output)) => {
                // Original output and join allocation die before the scalar
                // completion marker can permit a close/removal.
                drop(output);
                drop(s.original.take());
                s.joined = true;
                JoinState::Joined
            }
            Some(Err(error)) => {
                // Keep the same completed failed handle, never poll it twice.
                s.failure = Some(error);
                JoinState::Unknown
            }
        }
    }
}
impl<T> Drop for OriginalTask<T> {
    fn drop(&mut self) {
        let s = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Lifecycle loss cannot detach the only original handle. Production
        // cells have resident custody; this also protects failed setup/tests.
        if !s.joined {
            if let Some(task) = s.original.take() {
                std::mem::forget(task);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpKind {
    Open,
    Write,
    Read,
    Sync,
    Close,
}
enum NativeOutput {
    Opened { created: bool },
    Written(usize),
    Read(Vec<u8>),
    Synced,
    Closed { native: usize, error: Option<i32> },
}
struct NativeState {
    // This cell exists before open. Native jobs lock it BEFORE acquisition;
    // publication after successful open is a nonallocating Option assignment.
    file: Arc<Mutex<Option<std::fs::File>>>,
    opened: bool,
    created: bool,
    original: Option<JoinHandle<io::Result<NativeOutput>>>,
    kind: Option<OpKind>,
    failure: Option<tokio::task::JoinError>,
    close_uncertainty: Option<(usize, Option<i32>)>,
    synced: bool,
    closed: bool,
    io_failure: Option<io::ErrorKind>,
    inflight_bytes: u64,
}
struct NativeFile {
    state: Mutex<NativeState>,
    #[cfg(test)]
    hooks: Arc<NativeHooks>,
}
impl NativeFile {
    fn new() -> Self {
        Self {
            state: Mutex::new(NativeState {
                file: Arc::new(Mutex::new(None)),
                opened: false,
                created: false,
                original: None,
                kind: None,
                failure: None,
                close_uncertainty: None,
                synced: false,
                closed: false,
                io_failure: None,
                inflight_bytes: 0,
            }),
            #[cfg(test)]
            hooks: Arc::new(NativeHooks::default()),
        }
    }
    fn start(
        s: &mut NativeState,
        kind: OpKind,
        work: impl FnOnce() -> io::Result<NativeOutput> + Send + 'static,
    ) {
        assert!(s.original.is_none() && s.failure.is_none() && s.close_uncertainty.is_none());
        let (enter, registered) = tokio::sync::oneshot::channel();
        let original = tokio::task::spawn_blocking(move || {
            registered
                .blocking_recv()
                .map_err(|_| io::Error::from(io::ErrorKind::Interrupted))?;
            work()
        });
        s.kind = Some(kind);
        s.original = Some(original);
        let _ = enter.send(());
    }
    fn poll_original(
        s: &mut NativeState,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<Option<NativeOutput>>> {
        if s.failure.is_some() || s.close_uncertainty.is_some() {
            return Poll::Ready(Err(io::ErrorKind::Other.into()));
        }
        let Some(original) = s.original.as_mut() else {
            return Poll::Ready(Ok(None));
        };
        match Pin::new(original).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => {
                s.failure = Some(error);
                Poll::Ready(Err(io::ErrorKind::Other.into()))
            }
            Poll::Ready(Ok(result)) => {
                if let Ok(NativeOutput::Closed {
                    native,
                    error: Some(error),
                }) = &result
                {
                    s.close_uncertainty = Some((*native, Some(*error)));
                    // Keep SAME completed failed close handle and scalar result.
                    return Poll::Pending;
                }
                drop(s.original.take());
                if s.kind == Some(OpKind::Write) {
                    s.inflight_bytes = 0;
                }
                if s.kind == Some(OpKind::Sync) {
                    s.synced = true;
                }
                s.kind = None;
                if let Err(error) = &result {
                    s.io_failure = Some(error.kind());
                }
                Poll::Ready(result.map(Some))
            }
        }
    }
    fn start_open(&self, path: PathBuf, identity: bool) -> io::Result<()> {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.closed || s.opened || s.original.is_some() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let cell = Arc::clone(&s.file);
        #[cfg(test)]
        let hooks = Arc::clone(&self.hooks);
        Self::start(&mut s, OpKind::Open, move || {
            let mut published = cell
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true).read(identity);
            // No allocation or fallible operation between acquired File and
            // its publication into the already-rooted original cell.
            let created = match options.open(&path) {
                Ok(file) => {
                    *published = Some(file);
                    true
                }
                Err(error) if identity && error.kind() == io::ErrorKind::AlreadyExists => {
                    *published = Some(std::fs::OpenOptions::new().read(true).open(&path)?);
                    false
                }
                Err(error) => return Err(error),
            };
            #[cfg(test)]
            {
                if let Some(gate) = hooks
                    .after_open
                    .lock()
                    .expect("fixture operation succeeds: start open")
                    .clone()
                {
                    gate.wait();
                }
                assert!(
                    !hooks.panic_after_open.load(Ordering::Acquire),
                    "injected panic after root publication"
                );
            }
            Ok(NativeOutput::Opened { created })
        });
        Ok(())
    }
    fn poll_open(&self, cx: &mut Context<'_>) -> Poll<io::Result<bool>> {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.opened {
            return Poll::Ready(Ok(s.created));
        }
        match Self::poll_original(&mut s, cx) {
            Poll::Ready(Ok(Some(NativeOutput::Opened { created }))) => {
                s.opened = true;
                s.created = created;
                Poll::Ready(Ok(created))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            _ => Poll::Pending,
        }
    }
    fn poll_write(&self, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.original.is_none() {
            if !s.opened || s.closed {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            if bytes.len() as u64 > super::DVR_SINK_QUEUE_BYTES {
                return Poll::Ready(Err(io::ErrorKind::InvalidInput.into()));
            }
            let file = Arc::clone(&s.file);
            s.inflight_bytes = bytes.len() as u64;
            let bytes = bytes.to_vec();
            // Track the actual selected Vec capacity; allocator/executor and
            // constructor peaks remain Unknown, not inferred from wire length.
            let capacity = bytes.capacity() as u64;
            if capacity > super::DVR_SINK_QUEUE_BYTES {
                drop(bytes);
                s.inflight_bytes = 0;
                return Poll::Ready(Err(io::ErrorKind::OutOfMemory.into()));
            }
            s.inflight_bytes = capacity;
            #[cfg(test)]
            let hooks = Arc::clone(&self.hooks);
            s.synced = false;
            Self::start(&mut s, OpKind::Write, move || {
                #[cfg(test)]
                if let Some(gate) = hooks
                    .write
                    .lock()
                    .expect("fixture operation succeeds: poll write")
                    .clone()
                {
                    gate.wait();
                }
                let file = file
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let file = file.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
                let len = bytes.len();
                (&*file).write_all(&bytes)?;
                Ok(NativeOutput::Written(len))
            });
        }
        match Self::poll_original(&mut s, cx) {
            Poll::Ready(Ok(Some(NativeOutput::Written(n)))) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            _ => Poll::Pending,
        }
    }
    fn poll_read_identity(&self, cx: &mut Context<'_>) -> Poll<io::Result<Vec<u8>>> {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.original.is_none() {
            if !s.opened || s.closed {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            let file = Arc::clone(&s.file);
            #[cfg(test)]
            let hooks = Arc::clone(&self.hooks);
            Self::start(&mut s, OpKind::Read, move || {
                use std::io::{Read, Seek};
                #[cfg(test)]
                if let Some(gate) = hooks
                    .read
                    .lock()
                    .expect("fixture operation succeeds: poll read identity")
                    .clone()
                {
                    gate.wait();
                }
                let mut file = file
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let file = file.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
                file.seek(std::io::SeekFrom::Start(0))?;
                let mut value = Vec::with_capacity(33);
                file.take(33).read_to_end(&mut value)?;
                Ok(NativeOutput::Read(value))
            });
        }
        match Self::poll_original(&mut s, cx) {
            Poll::Ready(Ok(Some(NativeOutput::Read(value)))) => Poll::Ready(Ok(value)),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            _ => Poll::Pending,
        }
    }
    fn poll_flush(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match Self::poll_original(&mut s, cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
        }
    }
    fn poll_sync(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        std::task::ready!(self.poll_flush(cx))?;
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.synced {
            return Poll::Ready(Ok(()));
        }
        if !s.opened || s.closed {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if s.original.is_none() {
            let file = Arc::clone(&s.file);
            #[cfg(test)]
            let hooks = Arc::clone(&self.hooks);
            Self::start(&mut s, OpKind::Sync, move || {
                #[cfg(test)]
                if let Some(gate) = hooks
                    .sync
                    .lock()
                    .expect("fixture operation succeeds: poll sync")
                    .clone()
                {
                    gate.wait();
                }
                let file = file
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                file.as_ref().ok_or(io::ErrorKind::BrokenPipe)?.sync_all()?;
                Ok(NativeOutput::Synced)
            });
        }
        match Self::poll_original(&mut s, cx) {
            Poll::Ready(Ok(Some(NativeOutput::Synced))) => Poll::Ready(Ok(())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            _ => Poll::Pending,
        }
    }
    async fn storage_identity(
        self: &Arc<Self>,
        root: &str,
        stopped: Option<&AtomicBool>,
    ) -> io::Result<String> {
        use tokio::io::AsyncWriteExt;
        let file = self;
        let demand = || {
            if stopped.is_some_and(|s| s.load(Ordering::Acquire)) {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        };
        demand()?;
        file.start_open(PathBuf::from(root).join(".plurx-storage-id"), true)?;
        let created = std::future::poll_fn(|cx| file.poll_open(cx)).await?;
        demand()?;
        if created {
            let id = uuid::Uuid::new_v4().simple().to_string();
            let mut writer = NativeWriter {
                file: Arc::clone(file),
            };
            writer.write_all(id.as_bytes()).await?;
            demand()?;
            writer.sync_all().await?;
            demand()?;
        }
        let value = std::future::poll_fn(|cx| file.poll_read_identity(cx)).await?;
        demand()?;
        // Actual positive native close precedes publication of this identity.
        let closed = std::future::poll_fn(|cx| file.poll_close(cx)).await;
        if !closed {
            return Err(io::ErrorKind::Other.into());
        }
        if value.len() != 32 || !value.iter().all(u8::is_ascii_hexdigit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DVR storage identity is incomplete or invalid",
            ));
        }
        String::from_utf8(value).map_err(|_| io::ErrorKind::InvalidData.into())
    }

    fn poll_close(&self, cx: &mut Context<'_>) -> Poll<bool> {
        {
            let s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.closed {
                return Poll::Ready(true);
            }
            if s.failure.is_some() || s.close_uncertainty.is_some() {
                return Poll::Ready(false);
            }
            if s.kind == Some(OpKind::Open) {
                drop(s);
                match self.poll_open(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(_) => {}
                }
            }
        }
        let closing = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .kind
            == Some(OpKind::Close);
        if !closing {
            if self.poll_sync(cx).is_pending() {
                return Poll::Pending;
            }
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.failure.is_some() {
                return Poll::Ready(false);
            }
            if !s.opened {
                s.closed = true;
                return Poll::Ready(true);
            }
            // Every operation's F/output/JH has died. The native cell has one
            // reference; the close worker receives that same cell behind latch.
            if Arc::strong_count(&s.file) != 1 {
                return Poll::Pending;
            }
            let file = Arc::clone(&s.file);
            #[cfg(test)]
            let hooks = Arc::clone(&self.hooks);
            Self::start(&mut s, OpKind::Close, move || {
                #[cfg(test)]
                if let Some(gate) = hooks
                    .close
                    .lock()
                    .expect("fixture operation succeeds: poll close")
                    .clone()
                {
                    gate.wait();
                }
                let mut file = file
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let file = file.take().ok_or(io::ErrorKind::BrokenPipe)?;
                let result = close_original(file);
                #[cfg(test)]
                {
                    hooks.close_calls.fetch_add(1, Ordering::AcqRel);
                    if hooks.fail_close.load(Ordering::Acquire) {
                        if let NativeOutput::Closed { native, .. } = result {
                            return Ok(NativeOutput::Closed {
                                native,
                                error: Some(-1),
                            });
                        }
                    }
                }
                Ok(result)
            });
        }
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match Self::poll_original(&mut s, cx) {
            Poll::Ready(Ok(Some(NativeOutput::Closed { error: None, .. }))) => {
                s.closed = true;
                Poll::Ready(true)
            }
            Poll::Ready(Err(_)) => {
                s.close_uncertainty = Some((0, None));
                Poll::Ready(false)
            }
            _ => Poll::Pending,
        }
    }
}
fn close_original(file: std::fs::File) -> NativeOutput {
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        let fd = file.into_raw_fd();
        // SAFETY: one original File, no outstanding native job, close once.
        let rc = unsafe { libc::close(fd) };
        NativeOutput::Closed {
            native: fd as usize,
            error: (rc != 0).then(|| io::Error::last_os_error().raw_os_error().unwrap_or(-1)),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        let handle = file.into_raw_handle();
        // SAFETY: same uniquely owned original native handle, close once.
        let rc = unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        NativeOutput::Closed {
            native: handle as usize,
            error: (rc == 0).then(|| io::Error::last_os_error().raw_os_error().unwrap_or(-1)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::mem::forget(file);
        NativeOutput::Closed {
            native: 0,
            error: Some(-1),
        }
    }
}

pub(super) struct NativeWriter {
    file: Arc<NativeFile>,
}
impl tokio::io::AsyncWrite for NativeWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.file.poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.file.poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.file.poll_flush(cx)
    }
}
impl SinkWriter for NativeWriter {
    fn sync_all<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(std::future::poll_fn(move |cx| self.file.poll_sync(cx)))
    }
}

pub(super) struct CaptureEntry {
    recording: String,
    attempt: i64,
    // The root keeps admission outside this whole original allocation, so
    // every actual child and this Arc/header die before the token releases.
    native: Arc<NativeFile>,
    identity: Arc<NativeFile>,
    pub(super) writer: Arc<OriginalTask<SinkWriterOutcome>>,
    abandoned: AtomicBool,
    stopped: AtomicBool,
    acquisition_done: AtomicBool,
    cancel: tokio_util::sync::CancellationToken,
    writer_installed: AtomicBool,
    closed: AtomicBool,
}
pub(super) struct CaptureLease {
    entry: Arc<CaptureEntry>,
}
impl CaptureLease {
    pub(super) async fn storage_identity(&self, root: &str) -> io::Result<String> {
        self.entry
            .identity
            .storage_identity(root, Some(&self.entry.stopped))
            .await
    }
    pub(super) fn demand_alive(&self) -> bool {
        !self.entry.stopped.load(Ordering::Acquire)
    }
    pub(super) fn cancellation(&self) -> tokio_util::sync::CancellationToken {
        self.entry.cancel.clone()
    }
    pub(super) async fn open(&self, path: PathBuf) -> io::Result<NativeWriter> {
        if !self.demand_alive() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.entry.native.start_open(path, false)?;
        std::future::poll_fn(|cx| self.entry.native.poll_open(cx)).await?;
        if !self.demand_alive() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(NativeWriter {
            file: Arc::clone(&self.entry.native),
        })
    }
    pub(super) fn install_writer(&self, task: JoinHandle<SinkWriterOutcome>) -> bool {
        self.entry.writer.install(task);
        self.entry.writer_installed.store(true, Ordering::Release);
        self.entry.acquisition_done.store(true, Ordering::Release);
        self.demand_alive()
    }
    pub(super) fn settled(&self) -> bool {
        self.entry.closed.load(Ordering::Acquire)
    }
}
impl Drop for CaptureLease {
    fn drop(&mut self) {
        self.entry.acquisition_done.store(true, Ordering::Release);
        self.entry.abandoned.store(true, Ordering::Release);
        self.entry.cancel.cancel();
    }
}

struct CaptureRecord {
    entry: Arc<CaptureEntry>,
    admission: RestartAdmission,
}
impl std::ops::Deref for CaptureRecord {
    type Target = CaptureEntry;
    fn deref(&self) -> &CaptureEntry {
        &self.entry
    }
}

struct TransportOriginal {
    worker: Arc<OriginalTask<Result<(), super::LiveTvError>>>,
    transport: Arc<super::DvrTransport>,
    registry: std::sync::Weak<Mutex<super::super::LiveTvRegistry>>,
    cleanup: OriginalTask<()>,
    cleanup_started: bool,
}

pub(crate) struct CaptureOwners {
    entries: Mutex<Vec<CaptureRecord>>,
    transports: Mutex<Vec<TransportOriginal>>,
    finalizers: Mutex<Vec<FinalizerRecord>>,
    stops: Mutex<Vec<String>>,
    sealed: AtomicBool,
    stopping: AtomicBool,
    driver: OriginalTask<()>,
    shutdown_deadline: Mutex<Option<tokio::time::Instant>>,
}
impl CaptureOwners {
    pub(crate) fn new() -> Arc<Self> {
        let root = Arc::new(Self {
            entries: Mutex::new(Vec::with_capacity(CAPTURE_SLOTS)),
            transports: Mutex::new(Vec::with_capacity(TRANSPORT_SLOTS)),
            finalizers: Mutex::new(Vec::with_capacity(CAPTURE_SLOTS)),
            stops: Mutex::new(Vec::with_capacity(CAPTURE_SLOTS)),
            sealed: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            driver: OriginalTask::default(),
            shutdown_deadline: Mutex::new(None),
        });
        let weak = Arc::downgrade(&root);
        let (enter, registered) = tokio::sync::oneshot::channel();
        let driver = tokio::spawn(async move {
            if registered.await.is_err() {
                return;
            }
            loop {
                let Some(root) = weak.upgrade() else {
                    return;
                };
                root.drive();
                let stop = root.stopping.load(Ordering::Acquire);
                drop(root);
                if stop {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        root.driver.install(driver);
        let _ = enter.send(());
        root
    }
    pub(super) fn reserve(
        &self,
        recording: &str,
        attempt: i64,
        admission: RestartAdmission,
    ) -> Result<CaptureLease, super::LiveTvError> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.sealed.load(Ordering::Acquire)
            || entries.len() == CAPTURE_SLOTS
            || self
                .finalizers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|f| f.entry.recording == recording)
            || self
                .stops
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|id| id == recording)
            || entries.iter().any(|e| {
                e.recording == recording
                    && (!e.closed.load(Ordering::Acquire)
                        || !e.acquisition_done.load(Ordering::Acquire))
            })
        {
            return Err(super::LiveTvError::Capacity(
                "original recording cleanup still owns capture capacity".into(),
            ));
        }
        let entry = Arc::new(CaptureEntry {
            recording: recording.to_owned(),
            attempt,
            native: Arc::new(NativeFile::new()),
            identity: Arc::new(NativeFile::new()),
            writer: Arc::new(OriginalTask::default()),
            abandoned: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            acquisition_done: AtomicBool::new(false),
            cancel: tokio_util::sync::CancellationToken::new(),
            writer_installed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        entries.push(CaptureRecord {
            entry: Arc::clone(&entry),
            admission,
        });
        Ok(CaptureLease { entry })
    }
    pub(super) fn begin_finalization(
        self: &Arc<Self>,
        recording: &str,
        through: i64,
        admission: RestartAdmission,
    ) -> Result<FinalizationFence, super::LiveTvError> {
        self.drive();
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut finalizers = self
            .finalizers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.sealed.load(Ordering::Acquire)
            || finalizers.len() == CAPTURE_SLOTS
            || finalizers.iter().any(|f| f.entry.recording == recording)
            || entries.iter().any(|e| {
                e.recording == recording
                    && (e.attempt > through
                        || !e.closed.load(Ordering::Acquire)
                        || !e.acquisition_done.load(Ordering::Acquire))
            })
        {
            return Err(super::LiveTvError::OwnerUnavailable(
                "original capture file is still settling".into(),
            ));
        }
        let entry = Arc::new(FinalizerEntry {
            recording: recording.to_owned(),
            identity: Arc::new(NativeFile::new()),
            abandoned: AtomicBool::new(false),
        });
        finalizers.push(FinalizerRecord {
            entry: Arc::clone(&entry),
            admission,
        });
        Ok(FinalizationFence { entry })
    }
    pub(super) fn begin_stop(
        self: &Arc<Self>,
        recording: &str,
        through: i64,
    ) -> Result<StopFence, super::LiveTvError> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut stops = self
            .stops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stops.len() == CAPTURE_SLOTS
            || stops.iter().any(|id| id == recording)
            || entries
                .iter()
                .any(|e| e.recording == recording && e.attempt > through)
        {
            return Err(super::LiveTvError::OwnerUnavailable(
                "original recording admission is still settling".into(),
            ));
        }
        stops.push(recording.to_owned());
        for e in entries
            .iter()
            .filter(|e| e.recording == recording && e.attempt <= through)
        {
            e.stopped.store(true, Ordering::Release);
            e.cancel.cancel();
            if !e.writer_installed.load(Ordering::Acquire) {
                e.abandoned.store(true, Ordering::Release);
            }
        }
        Ok(StopFence {
            root: Arc::clone(self),
            recording: recording.to_owned(),
        })
    }
    pub(super) fn unsettled(&self, recording: &str) -> bool {
        self.drive();
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|e| {
                e.recording == recording
                    && (!e.closed.load(Ordering::Acquire)
                        || !e.acquisition_done.load(Ordering::Acquire))
            })
    }
    pub(super) fn permits_finalization(&self, recording: &str, through: i64) -> bool {
        self.drive();
        !self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|e| {
                e.recording == recording
                    && (e.attempt > through
                        || !e.closed.load(Ordering::Acquire)
                        || !e.acquisition_done.load(Ordering::Acquire))
            })
    }
    pub(super) fn reserve_transport(
        &self,
        transport: &Arc<super::DvrTransport>,
        registry: &Arc<Mutex<super::super::LiveTvRegistry>>,
    ) -> Result<Arc<OriginalTask<Result<(), super::LiveTvError>>>, super::LiveTvError> {
        let mut cells = self
            .transports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.sealed.load(Ordering::Acquire) || cells.len() == TRANSPORT_SLOTS {
            return Err(super::LiveTvError::Capacity(
                "original tuner tasks are still closing".into(),
            ));
        }
        let cell = Arc::new(OriginalTask::default());
        cells.push(TransportOriginal {
            worker: Arc::clone(&cell),
            transport: Arc::clone(transport),
            registry: Arc::downgrade(registry),
            cleanup: OriginalTask::default(),
            cleanup_started: false,
        });
        Ok(cell)
    }
    fn drive(&self) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for e in entries.iter() {
            // Observe acquisition completion BEFORE writer publication. Its
            // release store follows installation, so true cannot be combined
            // with an earlier stale "no writer" observation.
            let acquisition_done = e.acquisition_done.load(Ordering::Acquire);
            let writer = if e.writer_installed.load(Ordering::Acquire) {
                e.writer.observe()
            } else if acquisition_done && e.abandoned.load(Ordering::Acquire) {
                JoinState::Joined
            } else {
                JoinState::Pending
            };
            if writer == JoinState::Joined {
                let identity = std::future::poll_fn(|cx| e.identity.poll_close(cx)).now_or_never();
                let native = std::future::poll_fn(|cx| e.native.poll_close(cx)).now_or_never();
                if identity == Some(true) && native == Some(true) {
                    e.closed.store(true, Ordering::Release);
                }
            }
        }
        // The SAME admission stays outside Entry until its actual payload,
        // strong aliases and Arc/header allocation have been destroyed.
        let mut index = 0;
        while index < entries.len() {
            let e = &entries[index];
            let exclusive = Arc::strong_count(&e.entry) == 1
                && Arc::weak_count(&e.entry) == 0
                && Arc::strong_count(&e.native) == 1
                && Arc::weak_count(&e.native) == 0
                && Arc::strong_count(&e.identity) == 1
                && Arc::weak_count(&e.identity) == 0
                && Arc::strong_count(&e.writer) == 1
                && Arc::weak_count(&e.writer) == 0;
            let writer_destroyed = !e.writer_installed.load(Ordering::Acquire)
                || e.writer.observe() == JoinState::Joined;
            if e.closed.load(Ordering::Acquire)
                && e.acquisition_done.load(Ordering::Acquire)
                && exclusive
                && writer_destroyed
            {
                let CaptureRecord { entry, admission } = entries.swap_remove(index);
                match Arc::try_unwrap(entry) {
                    Ok(entry) => {
                        drop(entry);
                        drop(admission);
                    }
                    Err(entry) => entries.push(CaptureRecord { entry, admission }),
                }
            } else {
                index += 1;
            }
        }
        let mut finalizers = self
            .finalizers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut index = 0;
        while index < finalizers.len() {
            let f = &finalizers[index].entry;
            let closed = f.abandoned.load(Ordering::Acquire)
                && std::future::poll_fn(|cx| f.identity.poll_close(cx)).now_or_never()
                    == Some(true);
            if closed
                && Arc::strong_count(f) == 1
                && Arc::weak_count(f) == 0
                && Arc::strong_count(&f.identity) == 1
                && Arc::weak_count(&f.identity) == 0
            {
                let FinalizerRecord { entry, admission } = finalizers.swap_remove(index);
                match Arc::try_unwrap(entry) {
                    Ok(entry) => {
                        drop(entry);
                        drop(admission);
                    }
                    Err(entry) => finalizers.push(FinalizerRecord { entry, admission }),
                }
            } else {
                index += 1;
            }
        }
        drop(finalizers);
        let mut transports = self
            .transports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for row in transports.iter_mut() {
            if row.worker.observe() == JoinState::Joined && !row.cleanup_started {
                let registry = row.registry.clone();
                let transport = Arc::clone(&row.transport);
                let (enter, registered) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    if registered.await.is_ok() {
                        loop {
                            super::close_transport_original(
                                registry.clone(),
                                Arc::clone(&transport),
                            )
                            .await;
                            if transport.is_closed() {
                                break;
                            }
                            // Same original cleanup F and original native work;
                            // an observer deadline is not task/native renewal.
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                });
                row.cleanup.install(task);
                row.cleanup_started = true;
                let _ = enter.send(());
            }
        }
        transports.retain(|row| {
            !(row.worker.retired.load(Ordering::Acquire)
                && row.cleanup_started
                && row.cleanup.observe() == JoinState::Joined)
        });
    }
    pub(crate) fn seal(&self, deadline: tokio::time::Instant) {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Install the first deadline under the SAME seal lock. A concurrent
        // later observer cannot replace the original continuous deadline.
        self.shutdown_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(deadline);
        self.sealed.store(true, Ordering::Release);
        for e in entries.iter() {
            e.stopped.store(true, Ordering::Release);
            e.cancel.cancel();
            if !e.writer_installed.load(Ordering::Acquire) {
                e.abandoned.store(true, Ordering::Release);
            }
        }
    }
    pub(crate) async fn shutdown(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), super::LiveTvError> {
        self.seal(deadline);
        let deadline = *self
            .shutdown_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(deadline);
        loop {
            self.drive();
            // Drop each guard before acquiring another; admission and finalizer
            // installation otherwise use entries -> finalizers order.
            let entries_empty = {
                self.entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
            };
            let finalizers_empty = {
                self.finalizers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
            };
            let transports_empty = {
                self.transports
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
            };
            let stops_empty = {
                self.stops
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
            };
            let empty = entries_empty && finalizers_empty && transports_empty && stops_empty;
            if empty {
                self.stopping.store(true, Ordering::Release);
                if self.driver.observe() == JoinState::Joined {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(super::LiveTvError::StreamFailed(
                    "original recording or tuner work remains owned".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for CaptureOwners {
    fn drop(&mut self) {
        // Preserve the ACTUAL original File/operation/task/admission graph if
        // lifecycle authority disappears. No detached-handle or marker refund.
        let entries = self
            .entries
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !entries.is_empty() {
            std::mem::forget(std::mem::take(entries));
        }
        let finalizers = self
            .finalizers
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !finalizers.is_empty() {
            std::mem::forget(std::mem::take(finalizers));
        }
        let tasks = self
            .transports
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !tasks.is_empty() {
            std::mem::forget(std::mem::take(tasks));
        }
    }
}

struct FinalizerEntry {
    recording: String,
    identity: Arc<NativeFile>,
    abandoned: AtomicBool,
}
struct FinalizerRecord {
    entry: Arc<FinalizerEntry>,
    admission: RestartAdmission,
}
pub(super) struct FinalizationFence {
    entry: Arc<FinalizerEntry>,
}
impl FinalizationFence {
    pub(super) async fn storage_identity(&self, root: &str) -> io::Result<String> {
        self.entry.identity.storage_identity(root, None).await
    }
}
impl Drop for FinalizationFence {
    fn drop(&mut self) {
        self.entry.abandoned.store(true, Ordering::Release);
    }
}
pub(super) struct StopFence {
    root: Arc<CaptureOwners>,
    recording: String,
}
impl Drop for StopFence {
    fn drop(&mut self) {
        self.root
            .stops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|id| id != &self.recording);
    }
}
#[cfg(test)]
#[derive(Default)]
struct NativeHooks {
    after_open: Mutex<Option<Arc<BlockingGate>>>,
    write: Mutex<Option<Arc<BlockingGate>>>,
    read: Mutex<Option<Arc<BlockingGate>>>,
    sync: Mutex<Option<Arc<BlockingGate>>>,
    close: Mutex<Option<Arc<BlockingGate>>>,
    panic_after_open: AtomicBool,
    fail_close: AtomicBool,
    close_calls: std::sync::atomic::AtomicUsize,
}
#[cfg(test)]
#[derive(Default)]
struct BlockingGate {
    entered: AtomicBool,
    released: Mutex<bool>,
    changed: std::sync::Condvar,
}
#[cfg(test)]
impl BlockingGate {
    fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        let mut released = self
            .released
            .lock()
            .expect("fixture operation succeeds: wait");
        while !*released {
            released = self
                .changed
                .wait(released)
                .expect("fixture operation succeeds: wait");
        }
    }
    fn release(&self) {
        *self
            .released
            .lock()
            .expect("fixture operation succeeds: release") = true;
        self.changed.notify_all();
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.entered.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original native operation entered");
    }
}
#[cfg(test)]
mod tests;
