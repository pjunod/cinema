//! Concrete Unix publication I/O over a held directory descriptor.
//! Callers still owe prearmed journal and ledger binding.
use super::{open_directory_nofollow_blocking, std_file_identity, FileIdentity};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

const INIT_NAME: &str = "init.mp4";
fn parse_segment_name(name: &str) -> Option<u64> {
    name.strip_prefix("seg")?.strip_suffix(".m4s")?.parse().ok()
}
fn segment_name(index: u64) -> String {
    format!("seg{index:05}.m4s")
}

/// Actual held descriptors and intermediate facts survive a failed native job.
/// No serde constructor, pathname, or completed task can manufacture this owner.
#[derive(Debug)]
pub struct PublicationIo {
    // None is held-directory input: it can NEVER fall back to a pathname.
    path: Option<PathBuf>,
    temporary_name: CString,
    destination_name: CString,
    replacement: Option<ExpectedPublishedObject>,
    payload: Vec<u8>,
    logical_end: u64,
    directory: Option<File>,
    file: Option<File>,
    file_close: FileCloseState,
    entered: bool,
    acquired_ready: bool,
    publication_started: bool,
    acquired: Option<FileObservation>,
    published: bool,
    payload_released: bool,
}

/// Exact prior native object selected by the journal fold. These numbers are
/// only a negative check against the actual held directory; they are not a
/// closure receipt or permission to publish without the original arm.
#[derive(Clone, Copy, Debug)]
pub struct ExpectedPublishedObject {
    pub device: u64,
    pub inode: u64,
    pub mount_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileCloseState {
    NotAttempted,
    Closed,
    Failed(i32),
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct FileObservation {
    identity: FileIdentity,
    // st_blocks is in 512-byte units, NOT the preferred st_blksize.
    reported_allocated_bytes: u64,
    /// Linux mount identity of the observed object, when the kernel reports it.
    mount_id: Option<u64>,
}
impl FileObservation {
    pub fn identity(&self) -> FileIdentity {
        self.identity
    }
    pub fn reported_allocated_bytes(&self) -> u64 {
        self.reported_allocated_bytes
    }
    pub fn mount_id(&self) -> Option<u64> {
        self.mount_id
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Logical validation before moving an actual directory. This native request is
/// NOT a journal permit or physical proof; its caller owes the durable binding.
pub struct PublicationRequest {
    temporary_name: CString,
    destination_name: CString,
    replacement: Option<ExpectedPublishedObject>,
    payload: Vec<u8>,
    logical_end: u64,
}
impl PublicationRequest {
    pub fn for_operation(name: &str, payload: Vec<u8>, io_id: uuid::Uuid) -> io::Result<Self> {
        let segment = parse_segment_name(name)
            .and_then(|n| u32::try_from(n).ok())
            .is_some_and(|n| segment_name(u64::from(n)) == name);
        if (name != INIT_NAME && !segment) || io_id.is_nil() {
            return Err(invalid(
                "publication requires a canonical name and nonnil operation",
            ));
        }
        Self::validated(name, payload, io_id)
    }

    /// Fixed metadata names have a separate entry from media publication.
    pub fn for_metadata_operation(
        name: &str,
        payload: Vec<u8>,
        io_id: uuid::Uuid,
    ) -> io::Result<Self> {
        if !matches!(name, "identity.json" | "encoded-process") || io_id.is_nil() {
            return Err(invalid(
                "metadata publication requires a fixed name and nonnil operation",
            ));
        }
        Self::validated(name, payload, io_id)
    }

    /// The rolling copy playlist has one fixed canonical name. Its later
    /// revisions use a separate replacement operation with an exact prior
    /// native identity; this constructor permits only the first publication.
    pub fn for_playlist_operation(payload: Vec<u8>, io_id: uuid::Uuid) -> io::Result<Self> {
        if io_id.is_nil() {
            return Err(invalid("playlist publication requires a nonnil operation"));
        }
        Self::validated("index.m3u8", payload, io_id)
    }

    pub fn for_playlist_replacement(
        payload: Vec<u8>,
        io_id: uuid::Uuid,
        prior: ExpectedPublishedObject,
    ) -> io::Result<Self> {
        let mut request = Self::for_playlist_operation(payload, io_id)?;
        if prior.device == 0 || prior.inode == 0 || prior.mount_id == 0 {
            return Err(invalid("playlist replacement requires exact prior native identity"));
        }
        request.replacement = Some(prior);
        Ok(request)
    }

    fn validated(name: &str, payload: Vec<u8>, io_id: uuid::Uuid) -> io::Result<Self> {
        let logical_end = u64::try_from(payload.len()).map_err(io::Error::other)?;
        Ok(Self {
            temporary_name: CString::new(format!(".{name}.{io_id}.tmp"))
                .expect("canonical name and UUID contain no NUL"),
            destination_name: CString::new(name).expect("canonical name contains no NUL"),
            replacement: None,
            payload,
            logical_end,
        })
    }

    /// Request cell and owned heap storage; excludes Tokio and OS baselines.
    pub fn resident_memory_bytes(&self) -> io::Result<usize> {
        std::mem::size_of::<Self>()
            .checked_add(self.payload.capacity())
            .and_then(|bytes| bytes.checked_add(self.temporary_name.as_bytes_with_nul().len()))
            .and_then(|bytes| bytes.checked_add(self.destination_name.as_bytes_with_nul().len()))
            .ok_or_else(|| invalid("publication request memory accounting overflow"))
    }
}

impl PublicationIo {
    /// Requested heap still held by this original native cell after publish.
    /// The payload capacity is included until its acknowledged release.
    pub(crate) fn retained_heap_bytes(&self) -> Option<usize> {
        self.path
            .as_ref()
            .map_or(Some(0), |path| Some(path.capacity()))?
            .checked_add(self.temporary_name.as_bytes_with_nul().len())?
            .checked_add(self.destination_name.as_bytes_with_nul().len())?
            .checked_add(self.payload.capacity())
    }
    pub fn new(path: PathBuf, name: &str, payload: Vec<u8>) -> io::Result<Self> {
        // Local UUID selection is restricted to the existing legacy profile.
        let request = PublicationRequest::for_operation(name, payload, uuid::Uuid::new_v4())?;
        Ok(Self::from_request(Some(path), None, request))
    }

    /// Infallible ownership move AFTER request validation; a validation failure
    /// must not drop the sole descriptor before its custodian records closure.
    /// Actual finite entry still needs known arm and registered job/latch.
    pub fn from_held_directory(request: PublicationRequest, directory: File) -> Self {
        Self::from_request(None, Some(directory), request)
    }

    fn from_request(
        path: Option<PathBuf>,
        directory: Option<File>,
        request: PublicationRequest,
    ) -> Self {
        Self {
            path,
            temporary_name: request.temporary_name,
            destination_name: request.destination_name,
            replacement: request.replacement,
            payload: request.payload,
            logical_end: request.logical_end,
            directory,
            file: None,
            file_close: FileCloseState::NotAttempted,
            entered: false,
            acquired_ready: false,
            publication_started: false,
            acquired: None,
            published: false,
            payload_released: false,
        }
    }

    pub fn logical_end(&self) -> u64 {
        self.logical_end
    }
    pub fn payload_capacity(&self) -> usize {
        self.payload.capacity()
    }
    pub fn entered(&self) -> bool {
        self.entered
    }
    pub fn published(&self) -> bool {
        self.published
    }
    pub fn observation(&self) -> Option<FileObservation> {
        self.acquired
    }
    pub fn acquired(&self) -> Option<FileObservation> {
        self.acquired
    }
    pub fn has_unresolved_object(&self) -> bool {
        self.file.is_some()
            || matches!(
                self.file_close,
                FileCloseState::Failed(_) | FileCloseState::Unknown
            )
    }
    pub fn temporary_name(&self) -> &CStr {
        &self.temporary_name
    }
    pub fn uses_held_directory(&self) -> bool {
        self.path.is_none()
    }
    pub fn has_held_directory(&self) -> bool {
        self.directory.is_some()
    }

    /// Return the SAME descriptor only after actual success. Errors/cancellation
    /// retain it here. This counted ownership move is NOT a close/refund fact.
    pub fn take_held_directory(&mut self) -> io::Result<File> {
        if self.path.is_some()
            || !self.entered
            || !self.published
            || self.file.is_some()
            || self.file_close != FileCloseState::Closed
        {
            return Err(io::Error::other(
                "held publication directory is not transferable",
            ));
        }
        self.directory
            .take()
            .ok_or_else(|| io::Error::other("held publication directory already moved"))
    }

    /// Release source payload storage only after native success and after the
    /// caller has returned this same directory descriptor to its custodian.
    /// The runtime calls this only after its durable Observe acknowledgment.
    /// Logical length and the native observation remain available afterwards.
    pub(crate) fn release_completed_payload(&mut self) -> Option<usize> {
        if !self.entered
            || !self.publication_started
            || !self.published
            || self.file.is_some()
            || self.file_close != FileCloseState::Closed
            || self.directory.is_some()
            || self.payload_released
        {
            return None;
        }
        let capacity = self.payload.capacity();
        self.payload = Vec::new();
        self.payload_released = true;
        Some(capacity)
    }

    pub fn run(&mut self) -> io::Result<()> {
        self.acquire()?;
        self.publish_acquired()
    }

    /// Enter native acquisition once, retaining any acquired object here.
    pub fn acquire(&mut self) -> io::Result<()> {
        self.acquire_native()?;
        self.acquired_ready = true;
        Ok(())
    }

    /// Publish after successful acquisition. Failure retains descriptor facts.
    pub fn publish_acquired(&mut self) -> io::Result<()> {
        if !self.acquired_ready || self.publication_started || self.file.is_none() {
            return Err(io::Error::other(
                "publication is not ready or its one attempt was already started",
            ));
        }
        self.publication_started = true;
        let result = self.write_and_publish();
        // Even partial writes and rename conflicts retain their real fstat and
        // original descriptor. Never unlink/drop an unknown partial result here.
        let observation = self.observe();
        result?;
        observation?;
        // Close the exact acquired descriptor once. A failed/ambiguous close
        // cannot be retried because the numeric fd may already have been reused.
        // Keep payload and directory custody on every nonzero/unknown result.
        self.close_published_file()
    }

    fn close_published_file(&mut self) -> io::Result<()> {
        if self.file_close != FileCloseState::NotAttempted {
            return Err(io::Error::other(
                "publication file close was already attempted",
            ));
        }
        let Some(file) = self.file.take() else {
            self.file_close = FileCloseState::Unknown;
            return Err(io::Error::other("published file descriptor is unavailable"));
        };
        let fd = file.into_raw_fd();
        // SAFETY: this worker owns the original File, has converted it to a raw
        // descriptor, and performs exactly one close without retrying EINTR.
        let result = unsafe { libc::close(fd) };
        if result == 0 {
            self.file_close = FileCloseState::Closed;
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if let Some(errno) = error.raw_os_error() {
            self.file_close = FileCloseState::Failed(errno);
            Err(error)
        } else {
            self.file_close = FileCloseState::Unknown;
            Err(io::Error::other(
                "publication file close outcome is unknown",
            ))
        }
    }

    fn acquire_native(&mut self) -> io::Result<()> {
        if self.entered {
            return Err(io::Error::other("native publication may enter only once"));
        }
        self.entered = true;
        // Opening this legacy directory is itself native I/O, performed inside
        // the retained real job. It is NOT retroactive finite bootstrap custody.
        if let Some(path) = self.path.as_ref() {
            self.directory = Some(open_directory_nofollow_blocking(path)?);
        }
        let directory = self
            .directory
            .as_ref()
            .ok_or_else(|| io::Error::other("held publication directory unavailable"))?;
        // SAFETY: terminated single-component name, held parent fd, exclusive
        // create; on success the new fd is moved into this retained owner.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                self.temporary_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        self.file = Some(unsafe { File::from_raw_fd(fd) });
        // Immediately record native identity, before write/rename may fail.
        self.observe()
    }

    fn observe(&mut self) -> io::Result<()> {
        let file = self.file.as_ref().expect("acquired file");
        let metadata = file.metadata()?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        };
        let parent = std_file_identity(self.directory.as_ref().expect("held parent"))?;
        if !metadata.is_file() || metadata.nlink() != 1 || identity.device != parent.device {
            return Err(io::Error::other(
                "publication object is not an exclusive regular child",
            ));
        }
        let allocated = metadata
            .blocks()
            .checked_mul(512)
            .ok_or_else(|| io::Error::other("publication allocation observation overflow"))?;
        if self
            .acquired
            .is_some_and(|old| !old.identity.same_inode(identity))
        {
            return Err(io::Error::other("held publication object identity changed"));
        }
        self.acquired = Some(FileObservation {
            identity,
            reported_allocated_bytes: allocated,
            mount_id: linux_mount_id(file),
        });
        Ok(())
    }

    fn write_and_publish(&mut self) -> io::Result<()> {
        let file = self.file.as_mut().expect("acquired file");
        write_bounded(file, &self.payload, self.logical_end)?;
        if file.metadata()?.len() != self.logical_end {
            return Err(io::Error::other(
                "publication length differs from owned payload",
            ));
        }
        let directory = self.directory.as_ref().expect("held directory");
        // A name is not its object. Refuse a changed temporary alias before
        // native rename; the trusted-daemon boundary still excludes an external
        // actor racing between this check and rename.
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let inspected = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                self.temporary_name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if inspected != 0 {
            return Err(io::Error::last_os_error());
        }
        let stat = unsafe { stat.assume_init() };
        let expected = self.acquired.expect("acquired identity");
        #[allow(clippy::unnecessary_cast)]
        let same = stat.st_dev as u64 == expected.identity.device
            && stat.st_ino as u64 == expected.identity.inode
            && stat.st_mode & libc::S_IFMT == libc::S_IFREG;
        if !same {
            return Err(io::Error::other("publication temporary alias changed"));
        }
        if let Some(expected) = self.replacement {
            verify_prior_playlist(directory, &self.destination_name, expected)?;
            // The fixed trusted-software profile excludes another mutator of
            // this child between exact descriptor-relative check and rename.
            // A failed/ambiguous rename stays in the original native cell.
            let result = unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    self.temporary_name.as_ptr(),
                    directory.as_raw_fd(),
                    self.destination_name.as_ptr(),
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
        } else {
            rename_noreplace(directory, &self.temporary_name, &self.destination_name)?;
        }
        self.published = true;
        if self.replacement.is_some() {
            // A failed sync after the atomic swap is an uncertain publication:
            // the native cell and both charged objects stay held for replay.
            directory.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn verify_prior_playlist(
    directory: &File,
    name: &CStr,
    expected: ExpectedPublishedObject,
) -> io::Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    let device = unsafe { libc::makedev(stat.stx_dev_major, stat.stx_dev_minor) };
    if stat.stx_mask & libc::STATX_MNT_ID == 0
        || stat.stx_mode & libc::S_IFMT as u16 != libc::S_IFREG as u16
        || device != expected.device
        || stat.stx_ino != expected.inode
        || stat.stx_mnt_id != expected.mount_id
        || stat.stx_nlink != 1
    {
        return Err(io::Error::other("prior playlist native identity changed"));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_prior_playlist(
    _directory: &File,
    _name: &CStr,
    _expected: ExpectedPublishedObject,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "playlist replacement requires Linux mount identity",
    ))
}

#[cfg(target_os = "linux")]
fn linux_mount_id(file: &File) -> Option<u64> {
    let path = b"\0";
    let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: `file` remains open, the empty path selects that descriptor, and
    // `stat` points to writable storage of the required statx size.
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            path.as_ptr().cast(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    (stat.stx_mask & libc::STATX_MNT_ID != 0).then_some(stat.stx_mnt_id)
}

#[cfg(not(target_os = "linux"))]
fn linux_mount_id(_file: &File) -> Option<u64> {
    None
}

/// Every partial retry stays within the one authorized logical range. There is
/// no append/truncate/seek/mmap/reflink operation in this concrete profile.
fn write_bounded(writer: &mut impl Write, bytes: &[u8], end: u64) -> io::Result<()> {
    if u64::try_from(bytes.len()).ok() != Some(end) {
        return Err(invalid("payload is outside its publication range"));
    }
    let mut offset = 0usize;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        let requested_end = u64::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(u64::try_from(remaining.len()).ok()?));
        if requested_end != Some(end) {
            return Err(invalid("publication write range overflow"));
        }
        match writer.write(remaining) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) if written <= remaining.len() => {
                offset = offset
                    .checked_add(written)
                    .ok_or_else(|| invalid("write offset overflow"))?;
            }
            Ok(_) => return Err(io::Error::other("native writer exceeded requested range")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn rename_noreplace(directory: &File, from: &CStr, to: &CStr) -> io::Result<()> {
    // SAFETY: both single components are terminated and the held directory fd
    // lives through the syscall. No overwrite fallback or hard-link alias.
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as i32
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native no-replace publication unavailable",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests;
