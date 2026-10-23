//! Concrete sync I/O over a held directory descriptor.
//!
//! Requests name a small, fixed set of canonical VOD files and carry their
//! expected identities. This is an operation over already-held authority; it
//! never resolves the directory through a pathname or manufactures a receipt.
use super::{openat_owned, std_file_identity, FileIdentity};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use uuid::Uuid;

const MAX_SYNC_FILES: usize = 16;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn canonical_name(name: &str) -> bool {
    if name == "init.mp4" || name == "identity.json" || name == "encoded-process" {
        return true;
    }
    let Some(number) = name
        .strip_prefix("seg")
        .and_then(|rest| rest.strip_suffix(".m4s"))
    else {
        return false;
    };
    let Ok(index) = number.parse::<u32>() else {
        return false;
    };
    format!("seg{index:05}.m4s") == name
}

/// One expected file in a sync request. Identity is caller-supplied audit data;
/// validation occurs against the descriptor actually opened by `SyncIo`.
#[derive(Debug)]
pub struct SyncFile {
    name: CString,
    expected: FileIdentity,
    expected_mount_id: Option<u64>,
}

impl SyncFile {
    pub fn new(
        name: &str,
        expected: FileIdentity,
        expected_mount_id: Option<u64>,
    ) -> io::Result<Self> {
        if !canonical_name(name) {
            return Err(invalid("sync requires a canonical VOD file name"));
        }
        Ok(Self {
            name: CString::new(name).map_err(|_| invalid("sync file name contains NUL"))?,
            expected,
            expected_mount_id,
        })
    }
}

/// Validated, bounded description of one directory sync operation.
pub struct SyncRequest {
    io_id: Uuid,
    files: Vec<SyncFile>,
}

impl SyncRequest {
    pub fn new(io_id: Uuid, files: Vec<SyncFile>) -> io::Result<Self> {
        if io_id.is_nil() || files.is_empty() || files.len() > MAX_SYNC_FILES {
            return Err(invalid("sync requires a nonnil operation and 1–16 files"));
        }
        for (index, file) in files.iter().enumerate() {
            if files[..index]
                .iter()
                .any(|previous| previous.name == file.name)
            {
                return Err(invalid("sync file names must be unique"));
            }
        }
        Ok(Self { io_id, files })
    }

    /// Request cell, vector allocation, and owned canonical-name storage;
    /// excludes Tokio and operating-system baselines.
    pub fn resident_memory_bytes(&self) -> io::Result<usize> {
        let bytes = std::mem::size_of::<Self>()
            .checked_add(
                self.files
                    .capacity()
                    .checked_mul(std::mem::size_of::<SyncFile>())
                    .ok_or_else(|| invalid("sync request memory accounting overflow"))?,
            )
            .ok_or_else(|| invalid("sync request memory accounting overflow"))?;
        self.files.iter().try_fold(bytes, |total, file| {
            total
                .checked_add(file.name.as_bytes_with_nul().len())
                .ok_or_else(|| invalid("sync request memory accounting overflow"))
        })
    }
}

/// Owns the same held directory and any file whose validation or sync failed.
#[derive(Debug)]
pub struct SyncIo {
    io_id: Uuid,
    files: Vec<SyncFile>,
    directory: Option<File>,
    current_file: Option<File>,
    entered: bool,
    completed: bool,
}

impl SyncIo {
    /// Requested file-name/vector heap retained by the original completed
    /// sync cell. No native close authority follows from this measurement.
    pub(crate) fn retained_heap_bytes(&self) -> Option<usize> {
        let mut bytes = self
            .files
            .capacity()
            .checked_mul(std::mem::size_of::<SyncFile>())?;
        for file in &self.files {
            bytes = bytes.checked_add(file.name.as_bytes_with_nul().len())?;
        }
        Some(bytes)
    }
    /// Move the caller's already-held directory into this operation.
    pub fn from_held_directory(request: SyncRequest, directory: File) -> Self {
        Self {
            io_id: request.io_id,
            files: request.files,
            directory: Some(directory),
            current_file: None,
            entered: false,
            completed: false,
        }
    }

    /// Enter once, syncing each validated file and then the held directory.
    /// A failed entered operation retains the actual current file, if opened,
    /// and cannot be replayed.
    pub fn run(&mut self) -> io::Result<()> {
        if self.entered || self.completed || self.directory.is_none() {
            return Err(invalid(
                "sync operation has already been entered or completed",
            ));
        }
        self.entered = true;
        let directory = self
            .directory
            .as_ref()
            .ok_or_else(|| invalid("sync directory is unavailable"))?;
        for index in 0..self.files.len() {
            let file_name = &self.files[index].name;
            let fd = openat_owned(
                directory.as_raw_fd(),
                file_name,
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )?;
            self.current_file = Some(File::from(fd));
            let actual = self
                .current_file
                .as_ref()
                .expect("just installed opened sync file");
            let metadata = actual.metadata()?;
            let actual_identity = std_file_identity(actual)?;
            let expected = self.files[index].expected;
            let mount_matches = self.files[index]
                .expected_mount_id
                .is_none_or(|expected| linux_mount_id(actual) == Some(expected));
            if !metadata.file_type().is_file()
                || !actual_identity.same_inode(expected)
                || actual_identity.size != expected.size
                || !mount_matches
            {
                return Err(invalid("opened sync file identity does not match request"));
            }
            actual.sync_all()?;
            drop(self.current_file.take());
        }
        self.directory
            .as_ref()
            .expect("directory held until successful operation")
            .sync_all()?;
        self.completed = true;
        Ok(())
    }

    pub fn io_id(&self) -> Uuid {
        self.io_id
    }

    pub fn entered(&self) -> bool {
        self.entered
    }

    pub fn completed(&self) -> bool {
        self.completed
    }

    pub fn current_file(&self) -> Option<&File> {
        self.current_file.as_ref()
    }

    pub fn has_held_directory(&self) -> bool {
        self.directory.is_some()
    }

    /// Return the original descriptor only after every file and the directory
    /// have synced successfully.
    pub fn take_held_directory(&mut self) -> Option<File> {
        self.completed.then(|| self.directory.take()).flatten()
    }
}

#[cfg(target_os = "linux")]
fn linux_mount_id(file: &File) -> Option<u64> {
    let empty_path = b"\0";
    let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: `file` is held throughout the descriptor-relative statx call,
    // and `stat` points to writable storage of the expected ABI type.
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            empty_path.as_ptr().cast(),
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

#[cfg(test)]
mod tests;
