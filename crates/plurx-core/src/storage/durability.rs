//! Storage publication barriers, independent of either database backend.
//!
//! Unix uses rename followed by both parent-directory fsyncs. Windows uses
//! MoveFileExW with MOVEFILE_WRITE_THROUGH, without cross-volume copy fallback.
//! Its directory barrier validates the directory; namespace publication must
//! go through `rename`, not a read-only directory FlushFileBuffers call.
//!
//! API contracts: Microsoft documents write access for FlushFileBuffers and
//! write-through MoveFileExW for files and same-volume directory moves:
//! https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers
//! https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw
//! https://learn.microsoft.com/en-us/windows/win32/fileio/moving-directories
//!
//! Windows execution and power-loss qualification remain pending. These
//! documented operations are not a claim that such qualification has run.
use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;

/// Reopen an owned regular artifact without truncation, with the write access
/// required by Windows FlushFileBuffers. Refuse special files and final links.
pub(crate) fn sync_regular(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("artifact has no parent"))?;
    let parent = fs::canonicalize(parent)?;
    let _directory = crate::fs_secure::open_directory_nofollow_blocking(&parent)?;
    let path = parent.join(
        path.file_name()
            .ok_or_else(|| io::Error::other("artifact has no filename"))?,
    );
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("flush requires an owned regular artifact"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.write(true);
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "flush candidate ceased to be a regular artifact",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(io::Error::other("flush refuses a reparse-point artifact"));
        }
    }
    // Rust's Apple File::sync_all implementation calls fcntl(F_FULLFSYNC)
    // directly, rather than substituting fsync on failure. Keep that error
    // visible: publishing an artifact after a weaker flush is not equivalent.
    // https://doc.rust-lang.org/src/std/sys/fs/unix.rs.html
    file.sync_all()
}

/// Flush Unix directory entries. On Windows all namespace publications use
/// `rename`'s write-through move; a read-only directory is validated here but
/// is never passed to FlushFileBuffers as if it had file-write authority.
pub(crate) fn directory_barrier(path: &Path) -> io::Result<()> {
    let canonical = fs::canonicalize(path)?;
    let directory = crate::fs_secure::open_directory_nofollow_blocking(&canonical)?;
    #[cfg(unix)]
    {
        directory.sync_all()
    }
    #[cfg(windows)]
    {
        drop(directory);
        Ok(())
    }
}

/// Publish an owned file or directory on the same volume. Existing regular
/// records may be replaced; callers serialize authority changes with a lease.
pub(crate) fn rename(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    windows_move(source, destination, true)?;
    #[cfg(unix)]
    fs::rename(source, destination)?;
    let source_parent = source
        .parent()
        .ok_or_else(|| io::Error::other("source has no parent"))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent"))?;
    directory_barrier(source_parent)?;
    if source_parent != destination_parent {
        directory_barrier(destination_parent)?;
    }
    Ok(())
}

/// Node identity election must never replace another publisher's complete
/// file. MoveFileExW without REPLACE_EXISTING preserves that atomic choice.
#[cfg(windows)]
pub(crate) fn publish_new_file(source: &Path, destination: &Path) -> io::Result<()> {
    windows_move(source, destination, false)
}

#[cfg(windows)]
fn windows_move(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("publication has no parent"))?;
        let parent = fs::canonicalize(parent)?;
        let _directory = crate::fs_secure::open_directory_nofollow_blocking(&parent)?;
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| io::Error::other("publication has no filename"))?,
        );
        let mut encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if encoded.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication path contains NUL",
            ));
        }
        encoded.push(0);
        Ok(encoded)
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: both complete, NUL-terminated UTF-16 buffers remain live. Omitting
    // COPY_ALLOWED makes a cross-volume move fail instead of copying authority.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_replaces_complete_record_and_moves_directory_contents() {
        let root = tempfile::tempdir().expect("publication root");
        let source = root.path().join("incoming.json");
        let target = root.path().join("authority.json");
        fs::write(&source, b"complete new record").expect("incoming bytes");
        fs::write(&target, b"old record").expect("old bytes");
        sync_regular(&source).expect("write-capable regular flush");
        rename(&source, &target).expect("replace authority");
        assert_eq!(
            fs::read(&target).expect("published bytes"),
            b"complete new record"
        );
        let incoming = root.path().join("incoming");
        let active = root.path().join("active");
        fs::create_dir(&incoming).expect("incoming directory");
        fs::write(incoming.join("image"), b"retained image").expect("image bytes");
        sync_regular(&incoming.join("image")).expect("image flush");
        rename(&incoming, &active).expect("directory publication");
        assert_eq!(
            fs::read(active.join("image")).expect("moved bytes"),
            b"retained image"
        );
    }

    #[cfg(unix)]
    #[test]
    fn regular_flush_refuses_symbolic_artifact() {
        let root = tempfile::tempdir().expect("artifact root");
        let target = root.path().join("target");
        let alias = root.path().join("alias");
        fs::write(&target, b"preserved artifact").expect("target bytes");
        std::os::unix::fs::symlink(&target, &alias).expect("symbolic artifact");
        assert!(sync_regular(&alias).is_err());
        assert_eq!(
            fs::read(&target).expect("preserved target"),
            b"preserved artifact"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_identity_publication_never_replaces_existing_winner() {
        let root = tempfile::tempdir().expect("identity root");
        let incoming = root.path().join("incoming");
        let target = root.path().join("node.id");
        fs::write(&incoming, b"loser").expect("incoming identity");
        fs::write(&target, b"winner").expect("elected identity");
        assert!(publish_new_file(&incoming, &target).is_err());
        assert_eq!(fs::read(&target).expect("winner retained"), b"winner");
        assert_eq!(fs::read(&incoming).expect("loser retained"), b"loser");
    }
}
