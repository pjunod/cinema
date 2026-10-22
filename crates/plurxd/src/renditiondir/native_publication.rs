//! Concrete Unix publication custody, not a durable acquisition permission.
//! The legacy caller still owes prearmed bootstrap/journal and ledger binding.
use super::*;
use plurx_core::fs_secure::{open_directory_nofollow_blocking, std_file_identity, FileIdentity};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;

/// Actual held descriptors and intermediate facts survive a failed native job.
/// No serde constructor, pathname, or completed task can manufacture this owner.
#[derive(Debug)]
pub(super) struct PublicationIo {
    path: PathBuf,
    temporary_name: CString,
    destination_name: CString,
    payload: Vec<u8>,
    logical_end: u64,
    directory: Option<File>,
    file: Option<File>,
    entered: bool,
    pub(super) acquired: Option<FileObservation>,
    pub(super) published: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FileObservation {
    pub(super) identity: FileIdentity,
    // st_blocks is in 512-byte units, NOT the preferred st_blksize.
    pub(super) reported_allocated_bytes: u64,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl PublicationIo {
    pub(super) fn new(path: PathBuf, name: &str, payload: Vec<u8>) -> io::Result<Self> {
        let segment = parse_segment_name(name)
            .and_then(|n| u32::try_from(n).ok())
            .is_some_and(|n| segment_name(u64::from(n)) == name);
        if name != INIT_NAME && !segment {
            return Err(invalid(
                "publication requires a canonical init/segment name",
            ));
        }
        let logical_end = u64::try_from(payload.len()).map_err(io::Error::other)?;
        let operation = uuid::Uuid::new_v4();
        Ok(Self {
            path,
            temporary_name: CString::new(format!(".{name}.{operation}.tmp"))
                .expect("canonical name and UUID contain no NUL"),
            destination_name: CString::new(name).expect("canonical name contains no NUL"),
            payload,
            logical_end,
            directory: None,
            file: None,
            entered: false,
            acquired: None,
            published: false,
        })
    }

    pub(super) fn has_unresolved_object(&self) -> bool {
        self.file.is_some()
    }

    pub(super) fn run(&mut self) -> io::Result<()> {
        self.acquire()?;
        let result = self.write_and_publish();
        // Even partial writes and rename conflicts retain their real fstat and
        // original descriptor. Never unlink/drop an unknown partial result here.
        let observation = self.observe();
        result?;
        observation?;
        // No writable descriptor escapes successful native completion. Bytes
        // still belong to the legacy cache, NOT a minted close/refund receipt.
        self.file = None;
        self.payload = Vec::new();
        Ok(())
    }

    fn acquire(&mut self) -> io::Result<()> {
        if self.entered {
            return Err(io::Error::other("native publication may enter only once"));
        }
        self.entered = true;
        // Opening this legacy directory is itself native I/O, performed inside
        // the retained real job. It is NOT retroactive finite bootstrap custody.
        self.directory = Some(open_directory_nofollow_blocking(&self.path)?);
        let directory = self.directory.as_ref().expect("opened directory");
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
        rename_noreplace(directory, &self.temporary_name, &self.destination_name)?;
        self.published = true;
        Ok(())
    }
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
