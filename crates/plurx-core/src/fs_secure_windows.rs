//! Windows capability-style filesystem operations.
//!
//! Every path is opened one component at a time. After the volume root is
//! opened, each child uses `NtCreateFile` with the previous directory handle
//! as `RootDirectory`; `FILE_OPEN_REPARSE_POINT` makes junctions, symlinks,
//! and mount points observable so they can be refused before traversal.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, SetFileSecurityW, TokenUser, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileBasicInfo, FileDispositionInfoEx, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileIdInfo, FileRenameInfoEx, FileStandardInfo,
    FlushFileBuffers, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetVolumeInformationByHandleW, SetFileInformationByHandle, DELETE, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TEMPORARY, FILE_BASIC_INFO,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_ON_CLOSE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_BOTH_DIR_INFO, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    OPEN_EXISTING, SYNCHRONIZE, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const ERROR_FILE_EXISTS: i32 = 80;
const ERROR_ALREADY_EXISTS: i32 = 183;
const ERROR_NO_MORE_FILES: i32 = 18;
const ERROR_SHARING_VIOLATION: i32 = 32;
const MAX_DIRECTORY_ENTRIES: usize = 120_100;
const MAX_DIRECTORY_DEPTH: usize = 4;

fn invalid_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Stock ffmpeg omits FILE_SHARE_DELETE. A file it still has open is live,
/// not corrupt; Windows cleanup leaves it in the sweep set for a later pass.
pub fn is_sharing_violation(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

fn child_name(name: &str) -> io::Result<OsString> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.encode_utf16().any(|unit| unit == 0)
    {
        return Err(invalid_path("unsafe child filename"));
    }
    Ok(OsString::from(name))
}

fn path_parts(path: &Path) -> io::Result<(PathBuf, Vec<OsString>)> {
    let absolute = path.is_absolute();
    let mut root = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) if absolute => root.push(prefix.as_os_str()),
            Component::RootDir if absolute => root.push(Path::new("\\")),
            Component::CurDir => {}
            Component::Normal(name) => names.push(name.to_owned()),
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(invalid_path("filesystem path contains an unsafe component"));
            }
        }
    }
    if names.is_empty() {
        return Err(invalid_path("secure filesystem path may not be root"));
    }
    if !absolute {
        root = std::env::current_dir()?;
    }
    Ok((root, names))
}

fn wide_nul(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(invalid_path("filesystem path contains a NUL"));
    }
    wide.push(0);
    Ok(wide)
}

fn nt_error(status: i32) -> io::Error {
    // SAFETY: conversion accepts any NTSTATUS and returns its Win32 mapping.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}

fn get_info<T: Default>(handle: HANDLE, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: `value` is writable for exactly the supplied size and `handle`
    // remains live for this call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            class,
            (&raw mut value).cast(),
            std::mem::size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

fn reject_reparse(handle: HANDLE) -> io::Result<()> {
    let basic: FILE_BASIC_INFO = get_info(handle, FileBasicInfo)?;
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Err(io::Error::other(
            "secure filesystem path contains a reparse point",
        ))
    } else {
        Ok(())
    }
}

fn require_current_process_owner(file: &File) -> io::Result<()> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: output pointers are valid and the returned descriptor is freed
    // with LocalFree after its embedded owner SID has been compared.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let result = (|| {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess is a valid pseudo-handle and `token` is a
        // writable output slot closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token_result = (|| {
            let mut bytes = 0_u32;
            // The first call intentionally asks Windows for the required size.
            unsafe {
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut bytes)
            };
            if bytes < std::mem::size_of::<TOKEN_USER>() as u32 {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![0_u64; (bytes as usize).div_ceil(8)];
            // SAFETY: the aligned storage is writable for the exact byte count
            // requested by the preceding GetTokenInformation call.
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    storage.as_mut_ptr().cast(),
                    bytes,
                    &raw mut bytes,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a successful TokenUser query initializes TOKEN_USER at
            // the beginning of the aligned buffer.
            let current = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
            if unsafe { EqualSid(owner, current.User.Sid) } == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "scratch directory is not owned by the current Windows account",
                ));
            }
            Ok(())
        })();
        // SAFETY: OpenProcessToken returned this owned token handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(token) };
        token_result
    })();
    // SAFETY: GetSecurityInfo allocated the descriptor with LocalAlloc.
    unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor.cast()) };
    result
}

fn harden_owned_directory(path: &Path) -> io::Result<()> {
    // Protected DACL: full control for SYSTEM and for the object's owner, with
    // inheritance to every scratch/cache descendant and no inherited ACEs.
    let sddl = OsStr::new("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;OW)")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the SDDL is NUL-terminated and the returned descriptor is freed
    // after SetFileSecurityW consumes it.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let wide = wide_nul(path)?;
    let ok = unsafe {
        SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let result = if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    // SAFETY: the conversion function allocated this descriptor with
    // LocalAlloc and no references survive this call.
    unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor.cast()) };
    result
}

fn open_root(path: &Path) -> io::Result<File> {
    let wide = wide_nul(path)?;
    // SAFETY: the path is NUL-terminated, all pointers remain valid for the
    // call, and a successful handle is immediately transferred into `File`.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a fresh owned handle.
    let file = unsafe { File::from_raw_handle(handle.cast()) };
    reject_reparse(handle)?;
    Ok(file)
}

fn open_relative(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    attributes: u32,
) -> io::Result<File> {
    let mut wide = name.encode_wide().collect::<Vec<_>>();
    if wide.is_empty() || wide.len() > (u16::MAX as usize / 2) || wide.contains(&0) {
        return Err(invalid_path("invalid Windows child name"));
    }
    let unicode = UNICODE_STRING {
        Length: (wide.len() * 2) as u16,
        MaximumLength: (wide.len() * 2) as u16,
        Buffer: wide.as_mut_ptr(),
    };
    let object = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle().cast(),
        ObjectName: &raw const unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null_mut(),
        SecurityQualityOfService: std::ptr::null_mut(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer names a live stack value for the call. The parent
    // is an open directory handle and the returned handle is owned below.
    let result = unsafe {
        NtCreateFile(
            &raw mut handle,
            access,
            &raw const object,
            &raw mut status,
            std::ptr::null(),
            attributes,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if result < 0 {
        return Err(nt_error(result));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("NtCreateFile returned no handle"));
    }
    // SAFETY: NtCreateFile returned a fresh owned handle.
    let file = unsafe { File::from_raw_handle(handle.cast()) };
    reject_reparse(handle)?;
    Ok(file)
}

fn open_directory_child(parent: &File, name: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        FILE_ATTRIBUTE_NORMAL,
    )
}

fn create_directory_child_blocking(parent: &File, name: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
        FILE_ATTRIBUTE_NORMAL,
    )
}

fn open_read_child_blocking(parent: &File, name: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        name,
        FILE_GENERIC_READ | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        FILE_ATTRIBUTE_NORMAL,
    )
}

fn create_write_child_blocking(parent: &File, name: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        FILE_ATTRIBUTE_NORMAL,
    )
}

fn open_path(path: &Path, directory: bool) -> io::Result<File> {
    let (root, mut names) = path_parts(path)?;
    let final_name = names
        .pop()
        .ok_or_else(|| invalid_path("filesystem path has no filename"))?;
    let mut current = open_root(&root)?;
    for name in names {
        current = open_directory_child(&current, &name)?;
    }
    if directory {
        open_directory_child(&current, &final_name)
    } else {
        open_read_child_blocking(&current, &final_name)
    }
}

/// Stable Windows file identity. NTFS exposes a 64-bit file id; the second
/// half is retained so the comparison remains correct on filesystems that
/// populate the full `FILE_ID_128` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub inode_high: u64,
    pub size: u64,
    pub changed_seconds: i64,
    pub changed_nanoseconds: i64,
}

impl FileIdentity {
    pub fn same_inode(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.inode_high == other.inode_high
    }
}

fn handle_identity(handle: HANDLE) -> io::Result<FileIdentity> {
    let id: FILE_ID_INFO = get_info(handle, FileIdInfo)?;
    let basic: FILE_BASIC_INFO = get_info(handle, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = get_info(handle, FileStandardInfo)?;
    let inode = u64::from_le_bytes(id.FileId.Identifier[..8].try_into().expect("eight bytes"));
    let inode_high = u64::from_le_bytes(id.FileId.Identifier[8..].try_into().expect("eight bytes"));
    Ok(FileIdentity {
        device: id.VolumeSerialNumber,
        inode,
        inode_high,
        size: standard.EndOfFile.max(0) as u64,
        changed_seconds: basic.ChangeTime / 10_000_000,
        changed_nanoseconds: (basic.ChangeTime % 10_000_000) * 100,
    })
}

fn file_identity(file: &File) -> io::Result<FileIdentity> {
    handle_identity(file.as_raw_handle().cast())
}

pub fn std_file_identity(file: &File) -> io::Result<FileIdentity> {
    file_identity(file)
}

pub fn std_file_path(file: &File) -> io::Result<PathBuf> {
    let handle = file.as_raw_handle().cast();
    // SAFETY: a null output buffer with size zero asks Windows for the
    // required UTF-16 capacity. The handle remains live for both calls.
    let required = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut wide = vec![0_u16; required as usize + 1];
    // SAFETY: `wide` is writable for the supplied length and the handle is
    // unchanged from the successful sizing call.
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            wide.as_mut_ptr(),
            wide.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 || written as usize >= wide.len() {
        return Err(io::Error::last_os_error());
    }
    wide.truncate(written as usize);
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

pub fn async_file_identity(file: &tokio::fs::File) -> io::Result<FileIdentity> {
    handle_identity(file.as_raw_handle().cast())
}

pub fn open_read_nofollow_blocking(path: &Path) -> io::Result<File> {
    open_path(path, false)
}

pub async fn open_read_nofollow(path: &Path) -> io::Result<tokio::fs::File> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || open_read_nofollow_blocking(&path))
        .await
        .map_err(io::Error::other)?
        .map(tokio::fs::File::from_std)
}

pub async fn read_bounded_regular(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    if max_bytes == 0 || max_bytes > usize::MAX as u64 {
        return Err(invalid_path("invalid bounded-file read ceiling"));
    }
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = open_read_nofollow_blocking(&path)?;
        let before = file_identity(&file)?;
        if before.size > max_bytes {
            return Err(io::Error::other("bounded file exceeds its byte cap"));
        }
        let mut bytes = Vec::with_capacity(before.size as usize);
        Read::by_ref(&mut file)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != before.size || file_identity(&file)? != before {
            return Err(io::Error::other("bounded file changed while it was read"));
        }
        Ok(bytes)
    })
    .await
    .map_err(io::Error::other)?
}

pub fn open_directory_nofollow_blocking(path: &Path) -> io::Result<File> {
    open_path(path, true)
}

pub fn require_mutable_volume_blocking(path: &Path) -> io::Result<()> {
    let directory = open_directory_nofollow_blocking(path)?;
    let mut filesystem = [0_u16; 32];
    // SAFETY: `filesystem` is a writable UTF-16 buffer and the remaining
    // optional outputs are deliberately null. The directory handle stays live.
    if unsafe {
        GetVolumeInformationByHandleW(
            directory.as_raw_handle().cast(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let end = filesystem
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filesystem.len());
    let name = String::from_utf16_lossy(&filesystem[..end]);
    if name.eq_ignore_ascii_case("NTFS") || name.eq_ignore_ascii_case("ReFS") {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "mutable plurx storage requires NTFS or ReFS on Windows; {path:?} is on {name}"
            ),
        ))
    }
}

#[derive(Clone)]
pub struct SecureDirectory {
    file: Arc<File>,
    path: Arc<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecureChildMetadata {
    pub identity: FileIdentity,
    pub is_file: bool,
    pub is_directory: bool,
}

impl SecureDirectory {
    pub async fn open(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        tokio::task::spawn_blocking(move || {
            Ok(Self {
                file: Arc::new(open_directory_nofollow_blocking(&path)?),
                path: Arc::new(path),
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub fn raw_handle(&self) -> HANDLE {
        self.file.as_raw_handle().cast()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity_blocking(&self) -> io::Result<FileIdentity> {
        file_identity(&self.file)
    }

    pub async fn identity(&self) -> io::Result<FileIdentity> {
        let file = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || file_identity(&file))
            .await
            .map_err(io::Error::other)?
    }

    pub async fn sync(&self) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || {
            // SAFETY: the directory handle remains live for the call.
            if unsafe { FlushFileBuffers(directory.as_raw_handle().cast()) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn open_child_directory(&self, name: &str) -> io::Result<Self> {
        let parent = Arc::clone(&self.file);
        let name = child_name(name)?;
        let path = self.path.join(&name);
        tokio::task::spawn_blocking(move || {
            Ok(Self {
                file: Arc::new(open_directory_child(&parent, &name)?),
                path: Arc::new(path),
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn create_child_directory(&self, name: &str) -> io::Result<Self> {
        let parent = Arc::clone(&self.file);
        let name = child_name(name)?;
        let path = self.path.join(&name);
        tokio::task::spawn_blocking(move || {
            let file = match create_directory_child_blocking(&parent, &name) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open_directory_child(&parent, &name)?
                }
                Err(error) => return Err(error),
            };
            Ok(Self {
                file: Arc::new(file),
                path: Arc::new(path),
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn child_metadata(&self, name: &str) -> io::Result<SecureChildMetadata> {
        let parent = Arc::clone(&self.file);
        let name = child_name(name)?;
        tokio::task::spawn_blocking(move || {
            let file = open_relative(
                &parent,
                &name,
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_OPEN,
                0,
                FILE_ATTRIBUTE_NORMAL,
            )?;
            let standard: FILE_STANDARD_INFO =
                get_info(file.as_raw_handle().cast(), FileStandardInfo)?;
            Ok(SecureChildMetadata {
                identity: file_identity(&file)?,
                is_file: !standard.Directory,
                is_directory: standard.Directory,
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn child_names(&self, max_entries: usize) -> io::Result<Vec<String>> {
        if max_entries == 0 || max_entries > MAX_DIRECTORY_ENTRIES {
            return Err(invalid_path("invalid directory-listing bound"));
        }
        let directory = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || child_names_blocking(&directory, max_entries))
            .await
            .map_err(io::Error::other)?
    }

    pub async fn read_bounded_child(&self, name: &str, max_bytes: u64) -> io::Result<Vec<u8>> {
        if max_bytes == 0 || max_bytes > usize::MAX as u64 {
            return Err(invalid_path("invalid bounded-file read ceiling"));
        }
        let directory = Arc::clone(&self.file);
        let name = child_name(name)?;
        tokio::task::spawn_blocking(move || {
            let mut file = open_read_child_blocking(&directory, &name)?;
            let before = file_identity(&file)?;
            if before.size > max_bytes {
                return Err(io::Error::other("bounded child exceeds its byte cap"));
            }
            let mut bytes = Vec::with_capacity(before.size as usize);
            Read::by_ref(&mut file)
                .take(max_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 != before.size || file_identity(&file)? != before {
                return Err(io::Error::other("bounded child changed while it was read"));
            }
            Ok(bytes)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn open_read_child(&self, name: &str) -> io::Result<tokio::fs::File> {
        let directory = Arc::clone(&self.file);
        let name = child_name(name)?;
        tokio::task::spawn_blocking(move || {
            open_read_child_blocking(&directory, &name).map(tokio::fs::File::from_std)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn append_bounded_child(
        &self,
        name: &str,
        bytes: &[u8],
        max_bytes: u64,
    ) -> io::Result<bool> {
        if max_bytes == 0 || bytes.len() as u64 > max_bytes {
            return Err(invalid_path("invalid bounded append ceiling"));
        }
        let directory = Arc::clone(&self.file);
        let name = child_name(name)?;
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut file = open_relative(
                &directory,
                &name,
                FILE_APPEND_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE,
                FILE_ATTRIBUTE_NORMAL,
            )?;
            let size = file_identity(&file)?.size;
            if size.saturating_add(bytes.len() as u64) > max_bytes {
                return Ok(false);
            }
            file.seek(SeekFrom::End(0))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(true)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn unlink_child(&self, name: &str) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let name = child_name(name)?;
        tokio::task::spawn_blocking(move || unlink_child_blocking(&directory, &name))
            .await
            .map_err(io::Error::other)?
    }

    pub async fn atomic_write_child(&self, destination: &str, bytes: &[u8]) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let destination = child_name(destination)?;
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || atomic_write_blocking(&directory, &destination, &bytes))
            .await
            .map_err(io::Error::other)?
    }

    pub async fn rename_child(&self, from: &str, to: &str) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let from = child_name(from)?;
        let to = child_name(to)?;
        tokio::task::spawn_blocking(move || {
            rename_child_blocking(&directory, &from, &directory, &to, true).map(|_| ())
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn rename_child_noreplace(&self, from: &str, to: &str) -> io::Result<bool> {
        let directory = Arc::clone(&self.file);
        let from = child_name(from)?;
        let to = child_name(to)?;
        tokio::task::spawn_blocking(move || {
            rename_child_blocking(&directory, &from, &directory, &to, false)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn remove_child_tree(
        &self,
        name: &str,
        max_entries: usize,
        max_depth: usize,
    ) -> io::Result<()> {
        validate_tree_bounds(max_entries, max_depth)?;
        let parent = Arc::clone(&self.file);
        let name = child_name(name)?;
        tokio::task::spawn_blocking(move || {
            let directory = open_directory_child(&parent, &name)?;
            let mut count = 0;
            count_tree(&directory, &mut count, max_entries, 0, max_depth)?;
            let mut removed = 0;
            remove_tree(&directory, &mut removed, max_entries, 0, max_depth)?;
            unlink_child_blocking(&parent, &name)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn rename_child_to(
        &self,
        from: &str,
        destination: &Path,
        to: &str,
    ) -> io::Result<()> {
        let source = Arc::clone(&self.file);
        let from = child_name(from)?;
        let destination = destination.to_owned();
        let to = child_name(to)?;
        tokio::task::spawn_blocking(move || {
            let destination = open_directory_nofollow_blocking(&destination)?;
            rename_child_blocking(&source, &from, &destination, &to, true).map(|_| ())
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn place_regular_child_from(
        &self,
        source: &SecureDirectory,
        source_name: &str,
        destination_name: &str,
        max_bytes: u64,
    ) -> io::Result<u64> {
        let destination = Arc::clone(&self.file);
        let source = Arc::clone(&source.file);
        let source_name = child_name(source_name)?;
        let destination_name = child_name(destination_name)?;
        tokio::task::spawn_blocking(move || {
            let mut input = open_read_child_blocking(&source, &source_name)?;
            let identity = file_identity(&input)?;
            if identity.size > max_bytes {
                return Err(io::Error::other(
                    "source child exceeds its regular-file bound",
                ));
            }
            let mut output = create_write_child_blocking(&destination, &destination_name)?;
            let result = (|| {
                let copied = io::copy(
                    &mut Read::by_ref(&mut input).take(max_bytes.saturating_add(1)),
                    &mut output,
                )?;
                output.sync_all()?;
                if copied != identity.size || file_identity(&input)? != identity {
                    return Err(io::Error::other("source changed while it was copied"));
                }
                Ok(identity.size)
            })();
            if result.is_err() {
                let _ = unlink_child_blocking(&destination, &destination_name);
            }
            result
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(crate) fn create_new_write_child_blocking(&self, name: &str) -> io::Result<File> {
        create_write_child_blocking(&self.file, &child_name(name)?)
    }

    pub(crate) fn rename_child_blocking(&self, from: &str, to: &str) -> io::Result<()> {
        rename_child_blocking(
            &self.file,
            &child_name(from)?,
            &self.file,
            &child_name(to)?,
            true,
        )
        .map(|_| ())
    }

    pub(crate) fn unlink_child_blocking(&self, name: &str) -> io::Result<()> {
        unlink_child_blocking(&self.file, &child_name(name)?)
    }

    pub async fn create_new_child(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let name = child_name(name)?;
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut file = create_write_child_blocking(&directory, &name)?;
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn rename_child_to_noreplace(
        &self,
        from: &str,
        destination: &SecureDirectory,
        to: &str,
    ) -> io::Result<bool> {
        let source = Arc::clone(&self.file);
        let destination = Arc::clone(&destination.file);
        let from = child_name(from)?;
        let to = child_name(to)?;
        tokio::task::spawn_blocking(move || {
            rename_child_blocking(&source, &from, &destination, &to, false)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn unlink_child_if_identity(
        &self,
        name: &str,
        device: u64,
        inode: u64,
        size: u64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
    ) -> io::Result<bool> {
        let directory = Arc::clone(&self.file);
        let name = child_name(name)?;
        tokio::task::spawn_blocking(move || {
            let file = open_relative(
                &directory,
                &name,
                DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_OPEN,
                0,
                FILE_ATTRIBUTE_NORMAL,
            )?;
            let actual = file_identity(&file)?;
            if actual.device != device
                || actual.inode ^ actual.inode_high.rotate_left(1) != inode
                || actual.size != size
                || actual.changed_seconds != changed_seconds
                || actual.changed_nanoseconds != changed_nanoseconds
            {
                return Ok(false);
            }
            set_disposition(&file, false)?;
            Ok(true)
        })
        .await
        .map_err(io::Error::other)?
    }
}

fn child_names_blocking(directory: &File, max_entries: usize) -> io::Result<Vec<String>> {
    let cursor = open_relative(
        directory,
        OsStr::new("."),
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        FILE_ATTRIBUTE_NORMAL,
    )?;
    let mut storage = vec![0u64; 8_192];
    let mut names = Vec::new();
    let mut class = FileIdBothDirectoryRestartInfo;
    loop {
        // SAFETY: the u64 buffer is aligned for FILE_ID_BOTH_DIR_INFO and the
        // byte length exactly matches its allocation.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                cursor.as_raw_handle().cast(),
                class,
                storage.as_mut_ptr().cast(),
                (storage.len() * std::mem::size_of::<u64>()) as u32,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                break;
            }
            return Err(error);
        }
        class = FileIdBothDirectoryInfo;
        let mut offset = 0usize;
        loop {
            let capacity = storage.len() * std::mem::size_of::<u64>();
            if offset + std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>() > capacity {
                return Err(io::Error::other(
                    "invalid Windows directory enumeration record",
                ));
            }
            // SAFETY: offset was bounds-checked and records returned by
            // Windows are naturally aligned within the aligned buffer.
            let entry = unsafe {
                &*(storage
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_ID_BOTH_DIR_INFO>())
            };
            let units = entry.FileNameLength as usize / 2;
            let name_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            if offset + name_offset + units * 2 > capacity {
                return Err(io::Error::other(
                    "invalid Windows directory filename record",
                ));
            }
            // SAFETY: filename bounds were checked against the returned buffer.
            let wide = unsafe {
                std::slice::from_raw_parts(std::ptr::addr_of!(entry.FileName).cast::<u16>(), units)
            };
            let name = OsString::from_wide(wide);
            if name != "." && name != ".." {
                if names.len() >= max_entries {
                    return Err(io::Error::other("directory exceeded its listing bound"));
                }
                names.push(
                    name.into_string()
                        .map_err(|_| invalid_path("directory child is not valid Unicode"))?,
                );
            }
            if entry.NextEntryOffset == 0 {
                break;
            }
            offset = offset
                .checked_add(entry.NextEntryOffset as usize)
                .ok_or_else(|| io::Error::other("Windows directory offset overflow"))?;
        }
    }
    Ok(names)
}

fn set_disposition(file: &File, on_close: bool) -> io::Result<()> {
    let mut flags = FILE_DISPOSITION_FLAG_DELETE
        | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
        | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE;
    if on_close {
        flags |= FILE_DISPOSITION_FLAG_ON_CLOSE;
    }
    let info = FILE_DISPOSITION_INFO_EX { Flags: flags };
    // SAFETY: `info` has the exact layout and size required by this class.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfoEx,
            (&raw const info).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn unlink_child_blocking(parent: &File, name: &OsStr) -> io::Result<()> {
    let file = open_relative(
        parent,
        name,
        DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        0,
        FILE_ATTRIBUTE_NORMAL,
    )?;
    set_disposition(&file, false)
}

fn rename_child_blocking(
    source_parent: &File,
    source_name: &OsStr,
    destination_parent: &File,
    destination_name: &OsStr,
    replace: bool,
) -> io::Result<bool> {
    let source = open_relative(
        source_parent,
        source_name,
        DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        0,
        FILE_ATTRIBUTE_NORMAL,
    )?;
    let wide = destination_name.encode_wide().collect::<Vec<_>>();
    let bytes = wide.len() * 2;
    let allocation = std::mem::size_of::<FILE_RENAME_INFO>()
        .saturating_add(bytes.saturating_sub(std::mem::size_of::<u16>()));
    let mut storage = vec![0u64; allocation.div_ceil(8)];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: storage is aligned and sized for the header plus the complete
    // UTF-16 filename. Both source and destination handles remain live.
    unsafe {
        (*info).Anonymous.Flags = 2 | u32::from(replace);
        (*info).RootDirectory = destination_parent.as_raw_handle().cast();
        (*info).FileNameLength = bytes as u32;
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            wide.len(),
        );
    }
    // SAFETY: the variable-size record is valid for `allocation` bytes.
    let ok = unsafe {
        SetFileInformationByHandle(
            source.as_raw_handle().cast(),
            FileRenameInfoEx,
            info.cast(),
            allocation as u32,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if !replace
        && (error.kind() == io::ErrorKind::AlreadyExists
            || matches!(
                error.raw_os_error(),
                Some(ERROR_FILE_EXISTS) | Some(ERROR_ALREADY_EXISTS)
            ))
    {
        Ok(false)
    } else {
        Err(error)
    }
}

fn atomic_write_blocking(directory: &File, destination: &OsStr, bytes: &[u8]) -> io::Result<()> {
    let temporary = OsString::from(format!(
        ".{}.{}.part",
        destination.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = create_write_child_blocking(directory, &temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        rename_child_blocking(directory, &temporary, directory, destination, true).map(|_| ())
    })();
    if result.is_err() {
        let _ = unlink_child_blocking(directory, &temporary);
    }
    result
}

fn validate_tree_bounds(max_entries: usize, max_depth: usize) -> io::Result<()> {
    if max_entries == 0
        || max_entries > MAX_DIRECTORY_ENTRIES
        || max_depth == 0
        || max_depth > MAX_DIRECTORY_DEPTH
    {
        Err(invalid_path("invalid cache-tree removal bound"))
    } else {
        Ok(())
    }
}

fn count_tree(
    directory: &File,
    count: &mut usize,
    max_entries: usize,
    depth: usize,
    max_depth: usize,
) -> io::Result<()> {
    for name in child_names_blocking(directory, max_entries)? {
        *count = count.saturating_add(1);
        if *count > max_entries {
            return Err(io::Error::other("cache tree exceeded its entry bound"));
        }
        let name = child_name(&name)?;
        let child = open_relative(
            directory,
            &name,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            0,
            FILE_ATTRIBUTE_NORMAL,
        )?;
        let standard: FILE_STANDARD_INFO =
            get_info(child.as_raw_handle().cast(), FileStandardInfo)?;
        if standard.Directory {
            if depth >= max_depth {
                return Err(io::Error::other("cache tree exceeded its depth bound"));
            }
            count_tree(&child, count, max_entries, depth + 1, max_depth)?;
        }
    }
    Ok(())
}

fn remove_tree(
    directory: &File,
    removed: &mut usize,
    max_entries: usize,
    depth: usize,
    max_depth: usize,
) -> io::Result<()> {
    for name in child_names_blocking(directory, max_entries)? {
        *removed = removed.saturating_add(1);
        if *removed > max_entries {
            return Err(io::Error::other("cache tree exceeded its removal bound"));
        }
        let name = child_name(&name)?;
        let child = open_relative(
            directory,
            &name,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            0,
            FILE_ATTRIBUTE_NORMAL,
        )?;
        let standard: FILE_STANDARD_INFO =
            get_info(child.as_raw_handle().cast(), FileStandardInfo)?;
        if standard.Directory {
            if depth >= max_depth {
                return Err(io::Error::other("cache tree exceeded its depth bound"));
            }
            remove_tree(&child, removed, max_entries, depth + 1, max_depth)?;
        }
        drop(child);
        unlink_child_blocking(directory, &name)?;
    }
    Ok(())
}

pub async fn remove_empty_directory_child(directory: &Path, name: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let name = child_name(name)?;
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        unlink_child_blocking(&directory, &name)
    })
    .await
    .map_err(io::Error::other)?
}

pub fn directory_identity_nofollow_blocking(path: &Path) -> io::Result<FileIdentity> {
    file_identity(&open_directory_nofollow_blocking(path)?)
}

pub fn directory_ancestor_identities_nofollow_blocking(
    path: &Path,
) -> io::Result<Vec<FileIdentity>> {
    let (root, names) = path_parts(path)?;
    let mut directory = open_root(&root)?;
    let mut identities = vec![file_identity(&directory)?];
    for name in names {
        directory = open_directory_child(&directory, &name)?;
        identities.push(file_identity(&directory)?);
    }
    identities.reverse();
    Ok(identities)
}

pub fn directory_source_ancestry_alias_blocking(first: &Path, second: &Path) -> io::Result<bool> {
    let first = directory_ancestor_identities_nofollow_blocking(first)?;
    let second = directory_ancestor_identities_nofollow_blocking(second)?;
    let first_directory = first
        .first()
        .copied()
        .ok_or_else(|| io::Error::other("first directory ancestry is empty"))?;
    let second_directory = second
        .first()
        .copied()
        .ok_or_else(|| io::Error::other("second directory ancestry is empty"))?;
    Ok(first
        .iter()
        .any(|identity| identity.same_inode(second_directory))
        || second
            .iter()
            .any(|identity| identity.same_inode(first_directory)))
}

pub fn regular_file_identity_nofollow_blocking(path: &Path) -> io::Result<FileIdentity> {
    let file = open_read_nofollow_blocking(path)?;
    let standard: FILE_STANDARD_INFO = get_info(file.as_raw_handle().cast(), FileStandardInfo)?;
    if standard.Directory {
        return Err(io::Error::other("protected path is not a regular file"));
    }
    file_identity(&file)
}

pub async fn directory_identity_nofollow(path: &Path) -> io::Result<FileIdentity> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || directory_identity_nofollow_blocking(&path))
        .await
        .map_err(io::Error::other)?
}

pub fn final_name(path: &Path) -> io::Result<OsString> {
    match path.components().next_back() {
        Some(Component::Normal(name)) => Ok(name.to_owned()),
        _ => Err(invalid_path("filesystem path has an unsafe filename")),
    }
}

pub async fn atomic_write_child(
    directory: &Path,
    destination: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let directory = directory.to_owned();
    let destination = child_name(destination)?;
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        atomic_write_blocking(&directory, &destination, &bytes)
    })
    .await
    .map_err(io::Error::other)?
}

fn anonymous_file_in_directory_blocking(directory: &Path) -> io::Result<File> {
    let directory = open_directory_nofollow_blocking(directory)?;
    let name = child_name(&format!(
        ".plurx-snapshot-{}",
        uuid::Uuid::new_v4().simple()
    ))?;
    let file = open_relative(
        &directory,
        &name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        FILE_ATTRIBUTE_TEMPORARY,
    )?;
    set_disposition(&file, true)?;
    Ok(file)
}

pub async fn anonymous_file_in_directory(directory: &Path) -> io::Result<tokio::fs::File> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        anonymous_file_in_directory_blocking(&directory).map(tokio::fs::File::from_std)
    })
    .await
    .map_err(io::Error::other)?
}

pub fn anonymous_memory_file() -> io::Result<tokio::fs::File> {
    anonymous_file_in_directory_blocking(&std::env::temp_dir()).map(tokio::fs::File::from_std)
}

pub fn seal_anonymous_memory_file(file: &tokio::fs::File) -> io::Result<()> {
    let _ = file;
    Ok(())
}

pub async fn rename_child(directory: &Path, from: &str, to: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let from = child_name(from)?;
    let to = child_name(to)?;
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        rename_child_blocking(&directory, &from, &directory, &to, true).map(|_| ())
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn rename_child_between(
    from_directory: &Path,
    from: &str,
    to_directory: &Path,
    to: &str,
) -> io::Result<()> {
    let from_directory = from_directory.to_owned();
    let from = child_name(from)?;
    let to_directory = to_directory.to_owned();
    let to = child_name(to)?;
    tokio::task::spawn_blocking(move || {
        let from_directory = open_directory_nofollow_blocking(&from_directory)?;
        let to_directory = open_directory_nofollow_blocking(&to_directory)?;
        rename_child_blocking(&from_directory, &from, &to_directory, &to, true).map(|_| ())
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn rename_child_noreplace(directory: &Path, from: &str, to: &str) -> io::Result<bool> {
    let directory = directory.to_owned();
    let from = child_name(from)?;
    let to = child_name(to)?;
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        rename_child_blocking(&directory, &from, &directory, &to, false)
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn unlink_child(directory: &Path, name: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let name = child_name(name)?;
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        unlink_child_blocking(&directory, &name)
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn create_directory_child(directory: &Path, name: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let name = child_name(name)?;
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        create_directory_child_blocking(&directory, &name).map(drop)
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn remove_flat_directory_child(
    parent: &Path,
    name: &str,
    max_entries: usize,
) -> io::Result<()> {
    if max_entries == 0 || max_entries > MAX_DIRECTORY_ENTRIES {
        return Err(invalid_path("invalid flat-directory removal bound"));
    }
    let parent = parent.to_owned();
    let name = child_name(name)?;
    tokio::task::spawn_blocking(move || {
        let parent = open_directory_nofollow_blocking(&parent)?;
        let directory = open_directory_child(&parent, &name)?;
        let mut count = 0;
        count_tree(&directory, &mut count, max_entries, 0, 0)?;
        let mut removed = 0;
        remove_tree(&directory, &mut removed, max_entries, 0, 0)?;
        unlink_child_blocking(&parent, &name)
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn remove_bounded_directory_tree_child(
    parent: &Path,
    name: &str,
    max_entries: usize,
    max_depth: usize,
) -> io::Result<()> {
    validate_tree_bounds(max_entries, max_depth)?;
    let parent = parent.to_owned();
    let name = child_name(name)?;
    tokio::task::spawn_blocking(move || {
        let parent = open_directory_nofollow_blocking(&parent)?;
        let directory = open_directory_child(&parent, &name)?;
        let mut count = 0;
        count_tree(&directory, &mut count, max_entries, 0, max_depth)?;
        let mut removed = 0;
        remove_tree(&directory, &mut removed, max_entries, 0, max_depth)?;
        unlink_child_blocking(&parent, &name)
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn restore_child_noreplace(
    directory: &Path,
    quarantine: &str,
    destination: &str,
) -> io::Result<bool> {
    let directory = directory.to_owned();
    let quarantine = child_name(quarantine)?;
    let destination = child_name(destination)?;
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let restored =
            rename_child_blocking(&directory, &quarantine, &directory, &destination, false)?;
        if !restored {
            unlink_child_blocking(&directory, &quarantine)?;
        }
        Ok(restored)
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(feature = "scratch-fault-injection")]
pub mod scratch_fault {
    use std::cell::Cell;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn arm_cleanup_failure() {
        ARMED.with(|armed| armed.set(true));
    }

    pub fn disarm() {
        ARMED.with(|armed| armed.set(false));
    }

    pub(super) fn take() -> bool {
        ARMED.with(|armed| armed.replace(false))
    }
}

fn clear_owned_scratch(
    path: &Path,
    marker_name: Option<&str>,
    expected_marker: &[u8],
    protected: &[FileIdentity],
) -> io::Result<()> {
    let path = std::path::absolute(path)?;
    let directory = open_directory_nofollow_blocking(&path)?;
    require_current_process_owner(&directory)?;
    harden_owned_directory(&path)?;
    let root = file_identity(&directory)?;
    if protected.iter().any(|identity| identity.same_inode(root)) {
        return Err(io::Error::other(
            "scratch root aliases a protected storage identity",
        ));
    }
    let marker = marker_name.map(child_name).transpose()?;
    if let Some(marker) = marker.as_ref() {
        match open_read_child_blocking(&directory, marker) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(4_097)
                    .read_to_end(&mut bytes)?;
                if bytes != expected_marker {
                    return Err(io::Error::other(
                        "scratch ownership marker does not match this installation",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !child_names_blocking(&directory, 2)?.is_empty() {
                    return Err(io::Error::other(
                        "explicit scratch root was populated before ownership could be claimed",
                    ));
                }
                let mut file = create_write_child_blocking(&directory, marker)?;
                file.write_all(expected_marker)?;
                file.sync_all()?;
            }
            Err(error) => return Err(error),
        }
    }
    #[cfg(feature = "scratch-fault-injection")]
    if scratch_fault::take() {
        return Err(io::Error::other("injected scratch cleanup failure"));
    }
    let preserved = marker.as_ref().and_then(|name| name.to_str());
    let names = child_names_blocking(&directory, MAX_DIRECTORY_ENTRIES)?;
    for name in names {
        if Some(name.as_str()) == preserved {
            continue;
        }
        let child = child_name(&name)?;
        let file = open_relative(
            &directory,
            &child,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            0,
            FILE_ATTRIBUTE_NORMAL,
        )?;
        let identity = file_identity(&file)?;
        if protected
            .iter()
            .any(|protected| protected.same_inode(identity))
        {
            return Err(io::Error::other(
                "scratch tree aliases a protected storage identity",
            ));
        }
        let standard: FILE_STANDARD_INFO = get_info(file.as_raw_handle().cast(), FileStandardInfo)?;
        if standard.Directory {
            let mut count = 0;
            count_tree(
                &file,
                &mut count,
                MAX_DIRECTORY_ENTRIES,
                0,
                MAX_DIRECTORY_DEPTH,
            )?;
            let mut removed = 0;
            let removed = remove_tree(
                &file,
                &mut removed,
                MAX_DIRECTORY_ENTRIES,
                0,
                MAX_DIRECTORY_DEPTH,
            );
            if let Err(error) = removed {
                if is_sharing_violation(&error) {
                    continue;
                }
                return Err(error);
            }
        }
        drop(file);
        if let Err(error) = unlink_child_blocking(&directory, &child) {
            if is_sharing_violation(&error) {
                continue;
            }
            return Err(error);
        }
    }
    Ok(())
}

pub fn claim_and_clear_owned_scratch_blocking(
    path: &Path,
    marker_name: &str,
    expected_marker: &[u8],
) -> io::Result<()> {
    claim_and_clear_owned_scratch_with_protected_blocking(path, marker_name, expected_marker, &[])
}

pub fn claim_and_clear_owned_scratch_with_protected_blocking(
    path: &Path,
    marker_name: &str,
    expected_marker: &[u8],
    protected: &[FileIdentity],
) -> io::Result<()> {
    if expected_marker.is_empty() || expected_marker.len() > 4_096 {
        return Err(invalid_path("invalid scratch ownership marker"));
    }
    clear_owned_scratch(path, Some(marker_name), expected_marker, protected)
}

pub fn clear_scratch_blocking(path: &Path) -> io::Result<()> {
    clear_scratch_with_protected_blocking(path, &[])
}

pub fn clear_scratch_with_protected_blocking(
    path: &Path,
    protected: &[FileIdentity],
) -> io::Result<()> {
    clear_owned_scratch(path, None, &[], protected)
}
