//! Filesystem work on media storage, kept off the async worker threads.
//!
//! Library roots, recording roots and held source descriptors usually live on
//! network storage (NFS, SMB). A `hard` NFS mount whose server is unreachable
//! does not fail a `stat`, `open`, `read` or `statvfs`: it blocks the calling
//! thread for the RPC timeout (60 s per retransmit on the fleet's
//! `timeo=600`) and longer. If that thread is a Tokio worker, every task
//! queued on it stops too. Once a handful of media calls have hung on
//! all the workers, Raft receive handlers, the serving monitor and `/metrics`
//! stop with them. That is how voter-a froze for 84 s on 2026-11-23
//! (docs/server/MEDIA-IO-ISOLATION-PLAN.md).
//!
//! [`blocking`] is the one way async code touches media storage
//! synchronously. It runs the closure on Tokio's blocking pool behind a
//! fixed permit pool, so:
//!
//! - a hung mount parks blocking-pool threads, never a worker, and only the
//!   tasks awaiting media I/O wait;
//! - calls through [`blocking`] and [`slot`] hold at most
//!   [`MEDIA_IO_CONCURRENCY`] blocking threads between them, leaving the rest
//!   of the pool (local `tokio::fs`, the store) running. That is not a bound
//!   on every media thread: [`in_place`] checks are counted but not bounded,
//!   and paths that were already on the blocking pool before this module
//!   (the scan walk, `tokio::fs` metadata) still use it directly;
//! - a caller that is cancelled while its call is in the kernel leaves the
//!   permit with the thread until the kernel returns, so the bound stays true.
//!
//! The closure must not take a lock that async code takes on a worker: the
//! lock would then be held for as long as the mount is hung.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use tokio::sync::Semaphore;

/// Media calls that may be in the kernel at once, node-wide.
///
/// Tokio's blocking pool has 512 threads. Sixty-four is far more than the
/// scanner, playback and probes need concurrently on a healthy mount. On a
/// hung mount it is the most blocking threads the mount can hold.
pub const MEDIA_IO_CONCURRENCY: usize = 64;

static PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MEDIA_IO_CONCURRENCY)));
static WAITING: AtomicU64 = AtomicU64::new(0);
static COMPLETED: AtomicU64 = AtomicU64::new(0);
static IN_PLACE: AtomicU64 = AtomicU64::new(0);

/// Run synchronous media-storage work off the async workers. See the module
/// documentation.
///
/// A panic in `work` is resumed in the caller. The only other failure is the
/// runtime shutting down underneath the call, reported as an `io::Error`.
pub async fn blocking<T, F>(work: F) -> std::io::Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    slot().await.run(work).await
}

/// A media slot held by code that is already on a blocking-pool thread.
///
/// Take it around the media calls only. Work that waits on something else,
/// such as a grant or the store, must happen before the slot is taken, or a
/// healthy mount's slots can all be pinned by waits that are not media I/O.
pub struct MediaSlot {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl MediaSlot {
    /// Run `work` on the blocking pool under this slot, which is released
    /// when `work` returns. For a caller that prepares something once it
    /// holds the slot (such as duplicating a descriptor) so queued callers do
    /// not hold it while they wait. Otherwise use [`blocking`].
    ///
    /// A panic in `work` is resumed in the caller. The only other failure is
    /// the runtime shutting down underneath the call.
    pub async fn run<T, F>(self, work: F) -> std::io::Result<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let joined = tokio::task::spawn_blocking(move || {
            let _slot = self;
            work()
        })
        .await;
        match joined {
            Ok(value) => Ok(value),
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(std::io::Error::other(format!(
                "media I/O did not run: {error}"
            ))),
        }
    }
}

impl Drop for MediaSlot {
    fn drop(&mut self) {
        COMPLETED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Take a [`MediaSlot`]. Await it from async code, or `block_on` it from a
/// blocking-pool thread.
pub async fn slot() -> MediaSlot {
    let _waiting = Waiting::enter();
    MediaSlot {
        _permit: Arc::clone(&PERMITS)
            .acquire_owned()
            .await
            .expect("the media I/O semaphore is never closed"),
    }
}

/// [`blocking`] for work that itself returns `io::Result`.
pub async fn blocking_io<T, F>(work: F) -> std::io::Result<T>
where
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    blocking(work).await?
}

/// Run a synchronous media call from code that cannot await, such as the
/// `fstat` that revalidates a held source descriptor inside a filter.
///
/// On a multi-thread runtime worker this is `block_in_place`: the worker's
/// scheduler core passes to another thread before the call enters the
/// kernel, so the rest of the runtime keeps running while it is parked. On a
/// blocking-pool thread, a plain thread, or a current-thread runtime (unit
/// tests) the call runs directly, because there is no scheduler core to hand
/// off.
///
/// Prefer [`blocking`] wherever the caller can await: it is bounded. This is
/// for the descriptor checks spread through synchronous decision code. These
/// calls are not bounded: each parked one holds a thread. They are counted
/// in `plurx_media_io_in_place`, so a freeze driven by them is visible.
pub fn in_place<T>(work: impl FnOnce() -> T) -> T {
    let _counted = InPlace::enter();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// `fstat` of a held media descriptor via [`in_place`]. On NFS this goes to
/// the server whenever the attribute cache has expired.
pub fn fstat(file: &std::fs::File) -> std::io::Result<std::fs::Metadata> {
    in_place(|| file.metadata())
}

/// The [`crate::fs_secure::FileIdentity`] of a held media descriptor, read on
/// the bounded media pool.
///
/// Identity is an `fstat`, and on NFS an `fstat` revalidates with the server
/// once the attribute cache has expired, so it is media I/O like any read.
/// The slot is taken first; then the descriptor is duplicated (`dup` stays in
/// the local kernel), so a queued call holds no extra descriptor and the
/// caller keeps its own.
///
/// Media code reads identity through this or [`identity_in_place`], never
/// through `fs_secure::std_file_identity` directly; a source guard in plurxd
/// fails on new direct calls.
pub async fn identity(file: &std::fs::File) -> std::io::Result<crate::fs_secure::FileIdentity> {
    let slot = slot().await;
    let handle = file.try_clone()?;
    slot.run(move || crate::fs_secure::std_file_identity(&handle))
        .await?
}

/// [`identity`] for code that cannot await: synchronous decision code, and
/// code already on the blocking pool (where it runs directly). Unbounded, like
/// [`in_place`]. Prefer [`identity`] wherever the caller can await, and never
/// call this while holding a lock that async code takes.
pub fn identity_in_place(file: &std::fs::File) -> std::io::Result<crate::fs_secure::FileIdentity> {
    in_place(|| crate::fs_secure::std_file_identity(file))
}

struct InPlace;

impl InPlace {
    fn enter() -> Self {
        IN_PLACE.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for InPlace {
    fn drop(&mut self) {
        IN_PLACE.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Waiting;

impl Waiting {
    fn enter() -> Self {
        WAITING.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Calls in the kernel now, calls queued for a permit, and calls completed.
/// A hung mount shows as `in_flight` pinned at its ceiling with `waiting`
/// rising and `completed` flat.
pub fn observation() -> (usize, u64, u64) {
    (
        MEDIA_IO_CONCURRENCY - PERMITS.available_permits(),
        WAITING.load(Ordering::Relaxed),
        COMPLETED.load(Ordering::Relaxed),
    )
}

pub fn prometheus() -> String {
    let (in_flight, waiting, completed) = observation();
    format!(
        "# HELP plurx_media_io_in_flight Media-storage calls in the kernel now.\n\
         # TYPE plurx_media_io_in_flight gauge\n\
         plurx_media_io_in_flight {in_flight}\n\
         # HELP plurx_media_io_capacity Media-storage calls allowed in the kernel at once.\n\
         # TYPE plurx_media_io_capacity gauge\n\
         plurx_media_io_capacity {MEDIA_IO_CONCURRENCY}\n\
         # HELP plurx_media_io_waiting Media-storage calls waiting for a slot.\n\
         # TYPE plurx_media_io_waiting gauge\n\
         plurx_media_io_waiting {waiting}\n\
         # HELP plurx_media_io_completed_total Media-storage calls that returned.\n\
         # TYPE plurx_media_io_completed_total counter\n\
         plurx_media_io_completed_total {completed}\n\
         # HELP plurx_media_io_in_place Unbounded synchronous media checks in the kernel now.\n\
         # TYPE plurx_media_io_in_place gauge\n\
         plurx_media_io_in_place {in_place}\n",
        in_place = IN_PLACE.load(Ordering::Relaxed),
    )
}

/// A path whose `open` blocks in the kernel until [`HungPath::release`], the
/// way a file on a hard-mounted NFS export blocks while its server is
/// unreachable. Opening a FIFO for reading waits for a writer.
#[cfg(all(unix, any(test, feature = "fixtures")))]
pub mod hung_path {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    pub struct HungPath {
        path: PathBuf,
    }

    impl HungPath {
        pub fn create(path: &Path) -> Self {
            use std::os::unix::ffi::OsStrExt as _;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
            // SAFETY: `name` is a NUL-terminated path that outlives the call.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
            Self {
                path: path.to_owned(),
            }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Unblock every operation parked on the FIFO, until `finished` says
        /// they have all returned.
        ///
        /// Holding the FIFO open for reading and writing counts as a writer:
        /// a parked `open` returns, and closing the descriptor gives the
        /// readers end-of-file. One cycle is not enough. On macOS a single
        /// writer cycle wakes only some of several parked readers, and an
        /// operation queued behind a blocked worker only reaches `open` after
        /// the earlier ones return. So cycles repeat until every operation
        /// has finished. The 30 s cap only keeps a broken fixture from hanging
        /// the test binary forever.
        pub fn release(&self, finished: impl Fn() -> bool) {
            use std::os::unix::fs::OpenOptionsExt as _;
            let cap = std::time::Instant::now() + Duration::from_secs(30);
            while !finished() && std::time::Instant::now() < cap {
                let holder = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&self.path);
                std::thread::sleep(Duration::from_millis(10));
                drop(holder);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// What [`worst_heartbeat_lateness`] saw.
    #[derive(Debug)]
    pub struct Verdict {
        /// The heartbeat's worst late wake; `Duration::MAX` if it stopped.
        pub worst: Duration,
        /// Operations still parked on the hung path when the heartbeat
        /// finished. A test of the fix needs this to equal `parallel`:
        /// otherwise the operations never hung and the heartbeat proves
        /// nothing.
        pub parked: usize,
    }

    /// Run `stall` on a two-worker runtime beside a 10 ms heartbeat. The
    /// stall is `parallel` concurrent media operations on `hung`. Report
    /// the heartbeat's worst late wake while the operations are parked.
    pub fn worst_heartbeat_lateness<F, Fut>(hung: &HungPath, parallel: usize, stall: F) -> Verdict
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let stall = std::sync::Arc::new(stall);
        let returned = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..parallel {
            let stall = std::sync::Arc::clone(&stall);
            let returned = std::sync::Arc::clone(&returned);
            runtime.spawn(async move {
                stall().await;
                returned.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        // The verdict is read on this thread, outside the runtime: when every
        // worker is parked, nothing inside it runs, timers included.
        let (report, verdict) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let mut worst = Duration::ZERO;
            for _ in 0..30 {
                let sleep = tokio::time::sleep(Duration::from_millis(10));
                let deadline = sleep.deadline();
                sleep.await;
                worst = worst.max(tokio::time::Instant::now() - deadline);
            }
            let _ = report.send(worst);
        });
        let worst = verdict
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or(Duration::MAX);
        let parked = parallel - returned.load(std::sync::atomic::Ordering::SeqCst);
        hung.release(|| returned.load(std::sync::atomic::Ordering::SeqCst) == parallel);
        // Every operation has returned, so shutdown joins nothing that is
        // still parked. The timeout only keeps a broken fixture from
        // hanging the test binary.
        runtime.shutdown_timeout(Duration::from_secs(5));
        Verdict { worst, parked }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::hung_path::{worst_heartbeat_lateness, HungPath};
    use super::*;
    use std::time::Duration;

    #[test]
    fn hung_media_opens_do_not_stall_a_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hung = HungPath::create(&dir.path().join("hung"));
        let path = hung.path().to_owned();
        // Four parked opens on two workers: inline, they would hold both.
        let verdict = worst_heartbeat_lateness(&hung, 4, move || {
            let path = path.clone();
            async move {
                let _ = blocking(move || std::fs::read(&path)).await;
            }
        });
        assert_eq!(verdict.parked, 4, "the opens did not hang: {verdict:?}");
        assert!(
            verdict.worst < Duration::from_millis(500),
            "heartbeat was late while media opens were hung: {verdict:?}"
        );
    }

    #[test]
    fn the_fixture_detects_a_hung_open_on_a_worker() {
        // The same stall, run inline on the workers, must be caught. This is
        // what proves the fixture would fail a regression.
        let dir = tempfile::tempdir().expect("tempdir");
        let hung = HungPath::create(&dir.path().join("hung"));
        let path = hung.path().to_owned();
        let verdict = worst_heartbeat_lateness(&hung, 4, move || {
            let path = path.clone();
            async move {
                let _ = std::fs::read(&path);
            }
        });
        assert!(
            verdict.worst >= Duration::from_millis(500),
            "inline open did not stall: {verdict:?}"
        );
    }

    #[tokio::test]
    async fn a_panic_in_media_work_reaches_the_caller() {
        let caught = tokio::spawn(blocking(|| panic!("media work panicked"))).await;
        assert!(caught.expect_err("panic").is_panic());
    }

    /// Holding every slot is the state a hung mount leaves the pool in. An
    /// awaited identity read must then wait for a slot, not run beside the
    /// bound; the in-place read at the same moment shows the test can tell
    /// the two apart.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_awaited_identity_read_queues_on_the_media_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"source bytes").expect("source");
        let file = std::sync::Arc::new(std::fs::File::open(&path).expect("open"));
        let expected = crate::fs_secure::std_file_identity(&file).expect("identity");

        let mut held = Vec::with_capacity(MEDIA_IO_CONCURRENCY);
        for _ in 0..MEDIA_IO_CONCURRENCY {
            held.push(slot().await);
        }
        let awaited = tokio::spawn({
            let file = std::sync::Arc::clone(&file);
            async move { identity(&file).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !awaited.is_finished(),
            "the awaited identity read ran without a media slot"
        );
        assert_eq!(identity_in_place(&file).expect("in place"), expected);

        drop(held);
        let read = tokio::time::timeout(Duration::from_secs(5), awaited)
            .await
            .expect("the read runs once a slot is free")
            .expect("task")
            .expect("identity");
        assert_eq!(read, expected);
    }

    #[tokio::test]
    async fn results_pass_through() {
        assert_eq!(blocking(|| 7).await.expect("ran"), 7);
        let missing = blocking_io(|| std::fs::read("/nonexistent/plurx-media-io")).await;
        assert_eq!(
            missing.expect_err("missing").kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
