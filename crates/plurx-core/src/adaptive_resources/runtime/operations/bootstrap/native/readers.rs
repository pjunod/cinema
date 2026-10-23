//! Original native reader jobs for a retained scratch file. A journal arm and
//! charged lease are prerequisites supplied by the enclosing runtime; this
//! cell owns the actual File, fixed buffer, and original registered jobs.
use super::*;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Notify};
use v2::scratch::{
    ByteRangeV1, DirectoryObservedV1, FileObservedV1, NativeOutcomeV1, OutputKey, ReaderLeaseV1,
};

struct RecycledBuffer {
    slot: Mutex<Option<Vec<u8>>>,
    notify: Arc<Notify>,
}

/// One acknowledged range's original fixed-capacity buffer. It contains no
/// native handle. Dropping it returns the buffer to the SAME reader cell; a
/// second read cannot enter while this chunk is still held by the body.
pub(in crate::adaptive_resources::runtime::operations) struct OwnedReadChunk {
    bytes: Option<Vec<u8>>,
    len: usize,
    recycled: Arc<RecycledBuffer>,
}
impl AsRef<[u8]> for OwnedReadChunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes.as_ref().expect("owned chunk bytes")[..self.len]
    }
}
impl Drop for OwnedReadChunk {
    fn drop(&mut self) {
        if let Some(bytes) = self.bytes.take() {
            let mut slot = self
                .recycled
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            debug_assert!(slot.is_none());
            *slot = Some(bytes);
            drop(slot);
            self.recycled.notify.notify_one();
        }
    }
}

#[derive(Clone)]
pub(in crate::adaptive_resources::runtime::operations) struct ReaderReadFact {
    pub io_id: v2::Id,
    pub join_id: v2::Id,
    pub bytes_read: u64,
    pub outcome: NativeOutcomeV1,
    pub failure: Option<ReaderReadFailure>,
}

/// Fixed-size diagnostic copied from the original positional read error.
#[derive(Clone, Copy)]
pub(in crate::adaptive_resources::runtime::operations) struct ReaderReadFailure {
    pub kind: io::ErrorKind,
    pub errno: Option<i32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Open,
    Read,
    Close,
}
enum JobResult {
    Open {
        directory: File,
        file: Option<File>,
        entered: bool,
        result: io::Result<v2::NativeObject>,
    },
    Read {
        file: File,
        buffer: Vec<u8>,
        entered: bool,
        result: io::Result<usize>,
    },
    Close {
        file: Option<File>,
        entered: bool,
        result: io::Result<()>,
    },
}

pub(in crate::adaptive_resources::runtime::operations) struct ReaderCell {
    lease: ReaderLeaseV1,
    expected_native: v2::NativeObject,
    expected_len: u64,
    directory_observed: DirectoryObservedV1,
    basename: CString,
    capacity: usize,
    cursor: u64,
    file: Option<File>,
    lent_directory: Option<File>,
    buffer: Option<Vec<u8>>,
    recycled: Arc<RecycledBuffer>,
    pending: Option<JoinHandle<JobResult>>,
    pending_kind: Option<JobKind>,
    failed_join: bool,
    opened: Option<v2::NativeObject>,
    open_errno: Option<i32>,
    open_never_entered: bool,
    open_attempted: bool,
    open_acknowledged: bool,
    read_fact: Option<ReaderReadFact>,
    read_error: Option<io::Error>,
    read_acknowledged: bool,
    close_fact: Option<(v2::Id, v2::Id, bool)>,
    close_acknowledged: bool,
}

fn invalid() -> OperationError {
    OperationError::State
}
fn would_block() -> OperationError {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "previous reader chunk still held",
    )
    .into()
}
fn basename(output: OutputKey) -> Result<CString, OperationError> {
    let name = match output {
        OutputKey::Init => "init.mp4".to_owned(),
        OutputKey::Segment { index } => format!("seg{index:05}.m4s"),
        OutputKey::IdentityMetadata => "identity.json".to_owned(),
        OutputKey::EncodedProcessMetadata => "encoded-process".to_owned(),
    };
    CString::new(name).map_err(|_| invalid())
}

impl ReaderCell {
    /// Allocate and register the sole physical read buffer/cell before the
    /// durable OpenReader Arm. The enclosing runtime budgets it first and
    /// releases native entry only after that exact Arm commits.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn new(
        lease: ReaderLeaseV1,
        frozen: &FileObservedV1,
        directory: &DirectoryObservedV1,
    ) -> Result<Self, OperationError> {
        if lease.file_id != frozen.file_id
            || lease.memory_cap == 0
            || frozen.temporary
            || frozen.published.is_none()
            || frozen.native_object.file_type != v2::FileType::Regular
            || directory.held_copy_ids.len() != 1
            || frozen.native_object.filesystem_domain != directory.native_object.filesystem_domain
            || lease.cursor != lease.read_range.start
            || lease.read_range.end_exclusive > frozen.logical_len
        {
            return Err(invalid());
        }
        let capacity = usize::try_from(lease.memory_cap).map_err(|_| invalid())?;
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(capacity).map_err(|_| invalid())?;
        if buffer.capacity() > capacity {
            return Err(invalid());
        }
        buffer.resize(capacity, 0);
        let name = basename(frozen.published.ok_or_else(invalid)?)?;
        Ok(Self {
            cursor: lease.cursor,
            lease,
            expected_native: frozen.native_object.clone(),
            expected_len: frozen.logical_len,
            directory_observed: directory.clone(),
            basename: name,
            capacity,
            file: None,
            lent_directory: None,
            buffer: Some(buffer),
            recycled: Arc::new(RecycledBuffer {
                slot: Mutex::new(None),
                notify: Arc::new(Notify::new()),
            }),
            pending: None,
            pending_kind: None,
            failed_join: false,
            opened: None,
            open_errno: None,
            open_never_entered: false,
            open_attempted: false,
            open_acknowledged: false,
            read_fact: None,
            read_error: None,
            read_acknowledged: false,
            close_fact: None,
            close_acknowledged: false,
        })
    }

    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn lease_id(&self) -> v2::Id {
        self.lease.lease_id
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn lease(
        &self,
    ) -> &ReaderLeaseV1 {
        &self.lease
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn cursor(&self) -> u64 {
        self.cursor
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn fixed_capacity(
        &self,
    ) -> usize {
        self.capacity
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn has_pending_native(
        &self,
    ) -> bool {
        self.pending.is_some() || self.failed_join || self.lent_directory.is_some()
    }
    /// An exact entered openat failure, never an identity mismatch or an
    /// uncertain native join. The original directory must have returned.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn open_rejection_errno(
        &self,
    ) -> Option<i32> {
        if self.pending.is_none() && self.lent_directory.is_none() && !self.failed_join {
            self.open_errno.filter(|errno| *errno > 0)
        } else {
            None
        }
    }
    /// The original open worker joined after its entry latch was refused;
    /// no openat ran, no file was acquired, and the lent child is back home.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn open_never_entered(
        &self,
    ) -> bool {
        self.open_attempted
            && self.open_never_entered
            && self.pending.is_none()
            && self.lent_directory.is_none()
            && self.file.is_none()
            && self.opened.is_none()
            && self.open_errno.is_none()
            && !self.failed_join
    }
    /// A joined original open result with its original child returned. An
    /// identity mismatch is terminal native uncertainty, not a rejected open.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn open_native_terminal(
        &self,
    ) -> bool {
        self.open_attempted
            && self.pending.is_none()
            && self.lent_directory.is_none()
            && !self.failed_join
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn open_mismatched_file_held(
        &self,
    ) -> bool {
        self.open_native_terminal()
            && self.file.is_some()
            && self.opened.is_none()
            && self.open_errno.is_none()
            && !self.open_never_entered
    }
    /// True after the one original close job joined; only close_fact() is a
    /// positive closure observation. A failed close with no File is unknown.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn close_native_terminal(
        &self,
    ) -> bool {
        self.close_fact.is_some() && self.pending.is_none() && !self.failed_join
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn close_unknown(
        &self,
    ) -> bool {
        self.close_native_terminal()
            && self.close_fact.is_some_and(|(_, _, completed)| !completed)
            && self.file.is_none()
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn buffer_returned(
        &self,
    ) -> bool {
        self.buffer.is_some()
            || self
                .recycled
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
    }
    /// Clone this notifier under the bundle lock, then release that lock
    /// before awaiting `notified()`. Drop uses notify_one, preserving a permit.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn recycle_notification(
        &self,
    ) -> Arc<Notify> {
        Arc::clone(&self.recycled.notify)
    }

    /// The original job is stored here before its entry latch is released.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn launch_open(
        &mut self,
        root: &mut RootCell,
    ) -> Result<oneshot::Sender<()>, OperationError> {
        if self.pending.is_some()
            || self.failed_join
            || self.open_attempted
            || self.opened.is_some()
            || self.file.is_some()
            || self.lent_directory.is_some()
        {
            return Err(invalid());
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let name = self.basename.clone();
        let expected = self.expected_native.clone();
        let directory = root.lend_held_child(&self.directory_observed)?;
        let expected_len = self.expected_len;
        let (enter, registered) = oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return JobResult::Open {
                    directory,
                    file: None,
                    entered: false,
                    result: Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "reader open not entered",
                    )),
                };
            }
            let (file, result) = open_exact(&directory, &name, &expected, expected_len);
            JobResult::Open {
                directory,
                file,
                entered: true,
                result,
            }
        }));
        self.pending_kind = Some(JobKind::Open);
        self.open_attempted = true;
        Ok(enter)
    }

    pub(in crate::adaptive_resources::runtime::operations::bootstrap) async fn join_open(
        &mut self,
        root: &mut RootCell,
    ) -> Result<(), OperationError> {
        if self.pending_kind != Some(JobKind::Open) && self.lent_directory.is_none() {
            return Err(invalid());
        }
        if self.pending_kind == Some(JobKind::Open) {
            let job = self.pending.as_mut().ok_or_else(invalid)?.await;
            self.pending = None;
            self.pending_kind = None;
            match job {
                Ok(JobResult::Open {
                    directory,
                    file,
                    entered,
                    result,
                }) => {
                    self.lent_directory = Some(directory);
                    self.file = file;
                    self.open_never_entered = !entered;
                    if entered {
                        self.open_errno = if self.file.is_none() {
                            result.as_ref().err().and_then(io::Error::raw_os_error)
                        } else {
                            None
                        };
                        self.opened = result.ok();
                    }
                }
                _ => {
                    self.failed_join = true;
                    return Err(invalid());
                }
            }
        }
        root.return_held_child(&self.directory_observed, &mut self.lent_directory)
    }

    /// Only exact native identity and a returned original child directory
    /// produce a positive open observation. A failed native entry leaves the
    /// durable arm unresolved rather than manufacturing NoAcquisition.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn open_fact(
        &self,
    ) -> Option<v2::NativeObject> {
        if self.lent_directory.is_none() && !self.failed_join {
            self.opened.clone()
        } else {
            None
        }
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn acknowledge_open(
        &mut self,
    ) -> Result<(), OperationError> {
        if self.open_fact().is_none() || self.open_acknowledged {
            return Err(invalid());
        }
        self.open_acknowledged = true;
        Ok(())
    }

    fn reclaim_buffer(&mut self) -> Result<(), OperationError> {
        if self.buffer.is_none() {
            let mut slot = self
                .recycled
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.buffer = slot.take();
        }
        if self.buffer.is_none() {
            return Err(would_block());
        }
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn launch_read(
        &mut self,
        io_id: v2::Id,
        join_id: v2::Id,
        range: ByteRangeV1,
    ) -> Result<oneshot::Sender<()>, OperationError> {
        if !self.open_acknowledged
            || self.pending.is_some()
            || self.failed_join
            || self.read_fact.is_some()
            || self.read_error.is_some()
            || self.close_fact.is_some()
            || range.start != self.cursor
            || range.end_exclusive < range.start
            || range.end_exclusive > self.lease.read_range.end_exclusive
        {
            return Err(invalid());
        }
        let len = usize::try_from(range.end_exclusive - range.start).map_err(|_| invalid())?;
        if len > self.capacity {
            return Err(invalid());
        }
        self.reclaim_buffer()?;
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let file = self.file.take().ok_or_else(invalid)?;
        let mut buffer = self.buffer.take().ok_or_else(invalid)?;
        let (enter, registered) = oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return JobResult::Read {
                    file,
                    buffer,
                    entered: false,
                    result: Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "reader read not entered",
                    )),
                };
            }
            let result = file.read_at(&mut buffer[..len], range.start);
            JobResult::Read {
                file,
                buffer,
                entered: true,
                result,
            }
        }));
        self.pending_kind = Some(JobKind::Read);
        self.read_fact = Some(ReaderReadFact {
            io_id,
            join_id,
            bytes_read: 0,
            outcome: NativeOutcomeV1::NeverEntered,
            failure: None,
        });
        Ok(enter)
    }

    pub(in crate::adaptive_resources::runtime::operations::bootstrap) async fn join_read(
        &mut self,
    ) -> Result<(), OperationError> {
        if self.pending_kind != Some(JobKind::Read) || self.failed_join {
            return Err(invalid());
        }
        let job = self.pending.as_mut().ok_or_else(invalid)?.await;
        self.pending = None;
        self.pending_kind = None;
        match job {
            Ok(JobResult::Read {
                file,
                buffer,
                entered,
                result,
            }) => {
                self.file = Some(file);
                self.buffer = Some(buffer);
                let fact = self.read_fact.as_mut().ok_or_else(invalid)?;
                if entered {
                    match result {
                        Ok(count) => {
                            fact.bytes_read = u64::try_from(count).map_err(|_| invalid())?;
                            fact.outcome = NativeOutcomeV1::Completed;
                        }
                        Err(error) => {
                            fact.failure = Some(ReaderReadFailure {
                                kind: error.kind(),
                                errno: error.raw_os_error(),
                            });
                            fact.outcome = NativeOutcomeV1::Failed {
                                error_class: "retained reader positional read failed".into(),
                            };
                            self.read_error = Some(error);
                        }
                    }
                }
                Ok(())
            }
            _ => {
                self.failed_join = true;
                Err(invalid())
            }
        }
    }

    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn read_fact(
        &self,
    ) -> Option<ReaderReadFact> {
        self.read_fact
            .as_ref()
            .filter(|f| !matches!(f.outcome, NativeOutcomeV1::NeverEntered))
            .cloned()
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn acknowledge_read(
        &mut self,
        io_id: v2::Id,
    ) -> Result<(), OperationError> {
        let fact = self.read_fact().ok_or_else(invalid)?;
        if fact.io_id != io_id || self.read_acknowledged {
            return Err(invalid());
        }
        self.cursor = self
            .cursor
            .checked_add(fact.bytes_read)
            .ok_or_else(invalid)?;
        self.read_acknowledged = true;
        Ok(())
    }
    /// Move the original OS read error only after its exact ReaderRead Observe
    /// has been durably acknowledged. It cannot be turned into an early refund.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn take_acknowledged_read_error(
        &mut self,
    ) -> Option<io::Error> {
        if self.read_acknowledged
            && self
                .read_fact
                .as_ref()
                .is_some_and(|fact| fact.failure.is_some())
        {
            self.read_error.take()
        } else {
            None
        }
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn take_acknowledged_chunk(
        &mut self,
    ) -> Result<OwnedReadChunk, OperationError> {
        if !self.read_acknowledged {
            return Err(invalid());
        }
        let len = usize::try_from(self.read_fact.as_ref().ok_or_else(invalid)?.bytes_read)
            .map_err(|_| invalid())?;
        let bytes = self.buffer.take().ok_or_else(invalid)?;
        if len > bytes.len() {
            self.buffer = Some(bytes);
            return Err(invalid());
        }
        self.read_fact = None;
        self.read_acknowledged = false;
        Ok(OwnedReadChunk {
            bytes: Some(bytes),
            len,
            recycled: Arc::clone(&self.recycled),
        })
    }

    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn launch_close(
        &mut self,
        io_id: v2::Id,
        join_id: v2::Id,
    ) -> Result<oneshot::Sender<()>, OperationError> {
        if !self.open_acknowledged
            || self.pending.is_some()
            || self.failed_join
            || self.read_fact.is_some()
            || self.close_fact.is_some()
        {
            return Err(invalid());
        }
        // The body must release the same physical buffer before a close may
        // refund its lease. A chunk held by Bytes keeps that memory live.
        self.reclaim_buffer()?;
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let file = self.file.take().ok_or_else(invalid)?;
        let (enter, registered) = oneshot::channel();
        self.pending = Some(runtime.spawn_blocking(move || {
            if registered.blocking_recv().is_err() {
                return JobResult::Close {
                    file: Some(file),
                    entered: false,
                    result: Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "reader close not entered",
                    )),
                };
            }
            let fd = file.into_raw_fd();
            // SAFETY: `into_raw_fd` surrendered the sole File owner. This is
            // exactly one close call; an error never retries a recycled number.
            let rc = unsafe { libc::close(fd) };
            JobResult::Close {
                file: None,
                entered: true,
                result: if rc == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                },
            }
        }));
        self.pending_kind = Some(JobKind::Close);
        self.close_fact = Some((io_id, join_id, false));
        Ok(enter)
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) async fn join_close(
        &mut self,
    ) -> Result<(), OperationError> {
        if self.pending_kind != Some(JobKind::Close) || self.failed_join {
            return Err(invalid());
        }
        let job = self.pending.as_mut().ok_or_else(invalid)?.await;
        self.pending = None;
        self.pending_kind = None;
        match job {
            Ok(JobResult::Close {
                file,
                entered,
                result,
            }) => {
                self.file = file;
                if entered && result.is_ok() {
                    self.close_fact.as_mut().ok_or_else(invalid)?.2 = true;
                }
                Ok(())
            }
            _ => {
                self.failed_join = true;
                Err(invalid())
            }
        }
    }
    /// Only a joined, original close(2) returning zero is positive.
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn close_fact(
        &self,
    ) -> Option<(v2::Id, v2::Id, NativeOutcomeV1)> {
        self.close_fact
            .filter(|(_, _, completed)| *completed)
            .map(|(io, join, _)| (io, join, NativeOutcomeV1::Completed))
    }
    pub(in crate::adaptive_resources::runtime::operations::bootstrap) fn acknowledge_close(
        &mut self,
        io_id: v2::Id,
    ) -> Result<(), OperationError> {
        if self
            .close_fact()
            .is_none_or(|(actual, _, _)| actual != io_id)
            || self.close_acknowledged
        {
            return Err(invalid());
        }
        self.close_acknowledged = true;
        self.buffer = None;
        Ok(())
    }
}

fn open_exact(
    directory: &File,
    name: &CString,
    expected: &v2::NativeObject,
    expected_len: u64,
) -> (Option<File>, io::Result<v2::NativeObject>) {
    // SAFETY: directory remains owned by the worker, name is a NUL-terminated
    // canonical single component, and the returned descriptor is immediately
    // installed in its sole File owner.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return (None, Err(io::Error::last_os_error()));
    }
    // SAFETY: successful openat returned a fresh owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let result = (|| {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len() != expected_len
            || expected.file_type != v2::FileType::Regular
            || expected.device
                != (v2::NativeValue::Known {
                    value: metadata.dev(),
                })
            || expected.inode
                != (v2::NativeValue::Known {
                    value: metadata.ino(),
                })
            || !matches!(
                (&expected.mount_id, steps::mount_id(&file)),
                (v2::NativeValue::Known { value }, v2::NativeValue::Known { value: actual })
                    if *value == actual
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "published file identity changed",
            ));
        }
        Ok(expected.clone())
    })();
    (Some(file), result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn acknowledged_chunk_drop_recycles_the_original_fixed_buffer() {
        let recycled = Arc::new(RecycledBuffer {
            slot: Mutex::new(None),
            notify: Arc::new(Notify::new()),
        });
        let mut original = Vec::new();
        original.try_reserve_exact(64).unwrap();
        let capacity = original.capacity();
        assert!(capacity >= 64);
        original.resize(64, 0);
        original[..4].copy_from_slice(b"data");
        let pointer = original.as_ptr();
        let chunk = OwnedReadChunk {
            bytes: Some(original),
            len: 4,
            recycled: Arc::clone(&recycled),
        };
        assert_eq!(chunk.as_ref(), b"data");
        drop(chunk);
        let returned = recycled.slot.lock().unwrap().take().unwrap();
        assert_eq!(returned.as_ptr(), pointer);
        assert_eq!(returned.capacity(), capacity);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_relative_open_rejects_replaced_published_inode() {
        let target =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/reader-fixtures");
        std::fs::create_dir_all(&target).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("reader-")
            .tempdir_in(&target)
            .unwrap();
        let directory = File::open(temp.path()).unwrap();
        let path = temp.path().join("init.mp4");
        let mut first = File::create(&path).unwrap();
        first.write_all(b"old!").unwrap();
        let metadata = first.metadata().unwrap();
        let expected = v2::NativeObject {
            file_type: v2::FileType::Regular,
            device: v2::NativeValue::Known {
                value: metadata.dev(),
            },
            inode: v2::NativeValue::Known {
                value: metadata.ino(),
            },
            mount_id: steps::mount_id(&first),
            filesystem_domain: v2::FilesystemDomain {
                platform: v2::FilesystemPlatform::Linux,
                domain_id: v2::Id::new(Uuid::new_v4()).unwrap(),
                daemon_owner_id: v2::Id::new(Uuid::new_v4()).unwrap(),
                os_boot: BootIdentity::Known {
                    value: Uuid::new_v4(),
                    provider: "fixture".into(),
                    version: 1,
                },
                mount_namespace: v2::NativeValue::Unknown {
                    reason: "fixture".into(),
                },
            },
        };
        std::fs::remove_file(&path).unwrap();
        let mut replacement = File::create(&path).unwrap();
        replacement.write_all(b"new!").unwrap();
        let (opened, result) =
            open_exact(&directory, &CString::new("init.mp4").unwrap(), &expected, 4);
        assert!(opened.is_some());
        assert!(result.is_err());
    }
}
