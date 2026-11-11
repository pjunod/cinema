//! Kernel lifetime witnesses for explicitly daemon-mediated readers. Works on
//! native macOS/Linux and in containers; no host PID or reboot inference.
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

/// Keep the witness even if a Rust job future is abandoned. Only acknowledged
/// physical join may remove it; otherwise the owning daemon retains it until
/// actual process exit, including detached blocking reader threads.
static OWNED: std::sync::LazyLock<Mutex<HashMap<PathBuf, Arc<Witness>>>> =
    std::sync::LazyLock::new(Default::default);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u32,
    reader_model: String,
    kernel_instance: String,
    activation_id: String,
    node_id: String,
    anchor_nonce: String,
    anchor_device: u64,
    anchor_inode: u64,
    // Older evidence can still be removed following an acknowledged join.
    #[serde(default)]
    anchor_ctime: Option<(i64, i64)>,
    record: Record,
}

pub(crate) struct Witness {
    directory: PathBuf,
    name: String,
    activation_id: String,
    record: Record,
    // Never explicitly unlock, clone or export this descriptor to children.
    _anchor: std::fs::File,
    #[cfg(unix)]
    anchor_ctime: (i64, i64),
    disqualified: Mutex<bool>,
    unknown_escape: std::sync::atomic::AtomicBool,
}

impl Witness {
    pub(crate) fn arm(&self, activation_id: &str) -> Result<(), String> {
        if self.activation_id != activation_id {
            return Err("reader activation nonce mismatch".into());
        }
        let path = self.directory.join(format!("{}.armed", self.name));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(path).map_err(|error| error.to_string())?;
        file.write_all(activation_id.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(&self.directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())
    }

    fn observe_launch(&self, mediated: bool, declared: bool) -> std::io::Result<()> {
        if !mediated && !declared {
            self.unknown_escape
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if mediated {
            return Ok(());
        }
        let disqualified = self
            .disqualified
            .lock()
            .map_err(|_| std::io::Error::other("reader witness poisoned"))?;
        if *disqualified {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let mut disqualified = disqualified;
            use std::os::unix::fs::FileExt;
            // Invalidate the existing locked inode itself. This remains durable
            // even if a receipt path is removed/restored, and never unlocks it.
            // Rewriting saved bytes in place preserves both device and inode.
            // Require a later kernel-assigned change time before allowing exec,
            // including on filesystems whose ctime resolution is one second.
            let started = std::time::Instant::now();
            loop {
                self._anchor.write_all_at(b"!", 0)?;
                self._anchor.sync_all()?;
                if anchor_ctime(&self._anchor.metadata()?) > self.anchor_ctime {
                    break;
                }
                if started.elapsed() >= std::time::Duration::from_secs(2) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "reader invalidation could not advance the anchor change time",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            *disqualified = true;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "reader lifetime anchors need Unix file locks",
            ))
        }
    }
}

#[cfg(unix)]
fn anchor_ctime(metadata: &std::fs::Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.ctime(), metadata.ctime_nsec())
}

#[cfg(unix)]
pub(super) fn local_filesystem(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    let mut information = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs initializes the complete struct on success.
    if unsafe { libc::fstatfs(file.as_raw_fd(), information.as_mut_ptr()) } != 0 {
        return false;
    }
    let information = unsafe { information.assume_init() };
    #[cfg(target_os = "linux")]
    {
        // ext, XFS, btrfs, tmpfs and overlay. Remote/unknown lock semantics do
        // not qualify, even when a nonblocking lock happens to succeed.
        matches!(
            information.f_type as u64,
            0xef53 | 0x58465342 | 0x9123683e | 0x01021994 | 0x794c7630
        )
    }
    #[cfg(target_os = "macos")]
    {
        let filesystem = unsafe { std::ffi::CStr::from_ptr(information.f_fstypename.as_ptr()) };
        matches!(filesystem.to_bytes(), b"apfs" | b"hfs")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    false
}

/// Called for a newly registered exact owner BEFORE activation. It never
/// retrofits an anchor onto a legacy or already-active owner.
pub(super) fn prepare_at(
    directory: &Path,
    record: Record,
    activation_id: &str,
) -> Result<Option<Arc<Witness>>, String> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (directory, record, activation_id);
        Ok(None)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let Some(kernel_instance) = kernel_instance() else {
            return Ok(None);
        };
        let _admission = plurx_core::process::source_lifetime::registration()
            .map_err(|error| error.to_string())?;
        if directory.file_name().is_none_or(|name| name != "v2") {
            return Err("unexpected reader witness directory layout".into());
        }
        // Preserve the strict legacy validator; v2 has its own versioned child.
        private_directory(&directory.with_file_name("v1"))?;
        match std::fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        let metadata = directory
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("reader witness directory is not private".into());
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        std::fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(directory.parent().ok_or("missing reader witness parent")?)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        let directory_file = std::fs::File::open(directory).map_err(|error| error.to_string())?;
        if !local_filesystem(&directory_file) {
            return Ok(None);
        }
        let name = record.name()?.trim_end_matches(".json").to_owned();
        let path = directory.join(format!("{name}.json"));
        let mut owned = OWNED
            .lock()
            .map_err(|_| "physical reader registry poisoned")?;
        if let Some(existing) = owned.get(&path) {
            return Ok(Some(Arc::clone(existing)));
        }
        if path.try_exists().map_err(|error| error.to_string())? {
            return Err("old reader evidence cannot be rewritten as a new activation".into());
        }
        // A second daemon sharing this directory cannot simultaneously reserve
        // the last slots. Retain the directory lock through evidence creation.
        directory_file.lock().map_err(|error| error.to_string())?;
        let entries = std::fs::read_dir(directory).map_err(|error| error.to_string())?;
        inventory(entries, true)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let anchor_path = directory.join(format!("{name}.anchor"));
        let mut anchor = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&anchor_path)
            .map_err(|error| error.to_string())?;
        anchor.try_lock().map_err(|error| error.to_string())?;
        anchor
            .write_all(nonce.as_bytes())
            .and_then(|()| anchor.sync_all())
            .map_err(|error| error.to_string())?;
        let metadata = anchor.metadata().map_err(|error| error.to_string())?;
        let evidence = Evidence {
            version: 2,
            reader_model: "daemon_mediated_v1".into(),
            kernel_instance,
            activation_id: activation_id.to_owned(),
            node_id: record.node().to_owned(),
            anchor_nonce: nonce,
            anchor_device: metadata.dev(),
            anchor_inode: metadata.ino(),
            anchor_ctime: Some(anchor_ctime(&metadata)),
            record: record.clone(),
        };
        let bytes = serde_json::to_vec(&evidence).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("reader evidence exceeds its bound".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        directory_file
            .sync_all()
            .map_err(|error| error.to_string())?;
        let witness = Arc::new(Witness {
            directory: directory.to_owned(),
            name,
            activation_id: activation_id.to_owned(),
            record,
            _anchor: anchor,
            anchor_ctime: anchor_ctime(&metadata),
            disqualified: Mutex::new(false),
            unknown_escape: std::sync::atomic::AtomicBool::new(false),
        });
        owned.insert(path, Arc::clone(&witness));
        Ok(Some(witness))
    }
}

pub(super) fn complete_at(directory: &Path, record: &Record) -> Result<(), String> {
    let name = record.name()?.trim_end_matches(".json").to_owned();
    let evidence = directory.join(format!("{name}.json"));
    let existing = match evidence.try_exists().map_err(|error| error.to_string())? {
        true => Some(read_private_json::<Evidence>(&evidence)?),
        false => None,
    };
    if existing
        .as_ref()
        .is_some_and(|value| !value.record.same_owner(record))
    {
        return Err("reader acknowledgement identity mismatch".into());
    }
    let owned = OWNED
        .lock()
        .map_err(|_| "reader registry poisoned")?
        .get(&evidence)
        .cloned();
    if owned
        .as_ref()
        .is_some_and(|value| !value.record.same_owner(record))
    {
        return Err("owned reader acknowledgement identity mismatch".into());
    }
    if existing.is_none() && owned.is_none() {
        return Ok(());
    }
    // The caller already has authoritative exact-owner acknowledgement. Keep
    // the JSON until subsidiary cleanup succeeds so interrupted retries finish.
    for suffix in ["anchor", "unmediated", "armed", "json"] {
        match std::fs::remove_file(directory.join(format!("{name}.{suffix}"))) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    OWNED
        .lock()
        .map_err(|_| "reader registry poisoned")?
        .remove(&evidence);
    Ok(())
}

fn read_private_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("invalid reader evidence size/type".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err("reader evidence is not private".into());
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("reader evidence grew past its bound".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

thread_local! { static DECLARED_RAW: std::cell::RefCell<Option<BackgroundSourceCustodyToken>> = const { std::cell::RefCell::new(None) }; }

pub(super) fn declared_background_spawn<T>(
    receipt: &BackgroundSourceCustodyToken,
    spawn: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<BackgroundSourceCustodyToken>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DECLARED_RAW.with(|value| {
                value.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(DECLARED_RAW.with(|value| value.replace(Some(receipt.clone()))));
    spawn()
}

pub(super) fn live_background_join_allowed(
    directory: &Path,
    receipt: &BackgroundSourceCustodyToken,
) -> bool {
    let record = Record::Background {
        token: receipt.clone(),
    };
    let Ok(name) = record.name() else {
        return false;
    };
    OWNED
        .lock()
        .ok()
        .and_then(|owned| owned.get(&directory.join(name)).cloned())
        .is_some_and(|witness| {
            witness.record.same_owner(&record)
                && !witness
                    .unknown_escape
                    .load(std::sync::atomic::Ordering::Acquire)
        })
}

pub(super) fn observe_children(mediated: bool) -> std::io::Result<()> {
    if mediated {
        return Ok(());
    }
    let witnesses: Vec<_> = OWNED
        .lock()
        .map_err(|_| std::io::Error::other("reader registry poisoned"))?
        .values()
        .cloned()
        .collect();
    for witness in witnesses {
        let declared = DECLARED_RAW.with(|value| {
            value.borrow().as_ref().is_some_and(|receipt| {
                witness.record.same_owner(&Record::Background {
                    token: receipt.clone(),
                })
            })
        });
        // Declared raw children STILL invalidate the durable anchor. Only the
        // in-memory exact-owner ledger can later prove their explicit joins.
        witness.observe_launch(false, declared)?;
    }
    Ok(())
}

// Each owner has evidence, an anchor, an arm marker and, for older
// witnesses, an optional unmediated marker. Bound physical entries as well
// as owners so unrelated files cannot create unbounded directory scans.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn inventory(entries: std::fs::ReadDir, reserving_owner: bool) -> Result<Vec<PathBuf>, String> {
    const MAX_ENTRIES: usize = 4 * MAX_FILES;
    let mut paths = Vec::new();
    let mut arms = std::collections::HashSet::new();
    let mut count = 0;
    for entry in entries.take(MAX_ENTRIES + 1) {
        count += 1;
        if count > MAX_ENTRIES {
            return Err("reader witness inventory exceeds its entry bound".into());
        }
        let path = entry.map_err(|error| error.to_string())?.path();
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("json") => {
                paths.push(path);
                if paths.len() > MAX_FILES {
                    return Err("reader witness inventory exceeds its owner bound".into());
                }
            }
            Some("armed") if reserving_owner => {
                arms.insert(path);
            }
            _ => {}
        }
    }
    if reserving_owner {
        // A concurrent activation can still add its pending arm marker.
        let pending = paths
            .iter()
            .filter(|path| !arms.contains(&path.with_extension("armed")))
            .count();
        if paths.len() == MAX_FILES || count + pending + 3 > MAX_ENTRIES {
            return Err("reader witness inventory has no capacity for a new owner".into());
        }
    }
    Ok(paths)
}

pub(super) async fn replay_at(
    store: &Arc<dyn Store>,
    directory: &Path,
    legacy: &Path,
    node: &str,
) -> Result<(), String> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (store, directory, legacy, node);
        Ok(())
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let Some(kernel_instance) = kernel_instance() else {
            return Ok(());
        };
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        verify_directory(directory)?;
        let mut paths = inventory(entries, false)?;
        paths.sort();
        static CURSOR: Mutex<Option<PathBuf>> = Mutex::new(None);
        let after = CURSOR.lock().map_err(|_| "reader cursor poisoned")?.clone();
        let offset = after
            .as_ref()
            .map_or(0, |after| paths.partition_point(|path| path <= after));
        let length = paths.len();
        if length == 0 {
            return Ok(());
        }
        paths.rotate_left(offset % length);
        for path in paths.into_iter().take(MAX_RECORDS) {
            *CURSOR.lock().map_err(|_| "reader cursor poisoned")? = Some(path.clone());
            let evidence: Evidence = match read_private_json(&path) {
                Ok(evidence) => evidence,
                Err(error) => {
                    tracing::warn!(%error, "invalid reader evidence retained");
                    continue;
                }
            };
            if evidence.kernel_instance != kernel_instance
                || evidence.version != 2
                || evidence.reader_model != "daemon_mediated_v1"
                || evidence.node_id != node
                || evidence.record.node() != node
                || !uuid::Uuid::parse_str(&evidence.anchor_nonce).is_ok_and(|value| {
                    !value.is_nil() && value.to_string() == evidence.anchor_nonce
                })
            {
                continue;
            }
            let name = evidence.record.name()?.trim_end_matches(".json").to_owned();
            if path.file_name().and_then(|value| value.to_str()) != Some(&format!("{name}.json")) {
                continue;
            }
            let armed = directory.join(format!("{name}.armed"));
            let activation: String = match read_private_text(&armed, 64) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if activation != evidence.activation_id
                || !uuid::Uuid::parse_str(&activation)
                    .is_ok_and(|value| !value.is_nil() && value.to_string() == activation)
            {
                continue;
            }
            let disqualified = directory.join(format!("{name}.unmediated"));
            match disqualified.symlink_metadata() {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            let mut anchor = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(directory.join(format!("{name}.anchor")))
            {
                Ok(anchor) => anchor,
                Err(error) => {
                    tracing::warn!(%error, "missing or unreadable reader anchor retained");
                    continue;
                }
            };
            let metadata = anchor.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.mode() & 0o077 != 0
                || metadata.dev() != evidence.anchor_device
                || metadata.ino() != evidence.anchor_inode
                || metadata.len() > 64
                || !local_filesystem(&anchor)
            {
                continue;
            }
            match anchor.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(error) => {
                    tracing::warn!(%error, "reader lock cannot establish stop proof");
                    continue;
                }
            }
            let Some(original_ctime) = evidence.anchor_ctime else {
                continue;
            };
            if anchor_ctime(&anchor.metadata().map_err(|error| error.to_string())?)
                != original_ctime
            {
                continue;
            }
            let mut nonce = String::new();
            anchor
                .read_to_string(&mut nonce)
                .map_err(|error| error.to_string())?;
            if nonce != evidence.anchor_nonce
                || anchor_ctime(&anchor.metadata().map_err(|error| error.to_string())?)
                    != original_ctime
            {
                continue;
            }
            // The existing inode stays locked through exact-owner settlement,
            // release acknowledgement, and durable evidence cleanup.
            if disqualified.symlink_metadata().is_ok() {
                continue;
            }
            settle_records(store, std::slice::from_ref(&evidence.record)).await?;
            clear_at(legacy, &evidence.record)?;
            complete_at(directory, &evidence.record)?;
            drop(anchor);
            tracing::info!(
                "source custody recovered after verified mediated-reader lifetime ended"
            );
        }
        Ok(())
    }
}

fn read_private_text(path: &Path, limit: u64) -> Result<String, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("invalid private reader marker".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err("reader marker is not private".into());
        }
    }
    let mut value = String::new();
    file.take(limit + 1)
        .read_to_string(&mut value)
        .map_err(|error| error.to_string())?;
    if value.len() as u64 > limit {
        return Err("reader marker grew past its bound".into());
    }
    Ok(value)
}

/// Positive equality of the actual locking kernel prevents a cloned volume
/// from impersonating the old lock. A changed boot is NOT used as same-boot
/// proof, and no image/container marker is treated as host identity.
pub(super) fn kernel_instance() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
        for path in ["/run/plurx-host-boot-id", "/proc/sys/kernel/random/boot_id"] {
            let mut file = match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
            {
                Ok(file) => file,
                Err(_) => continue,
            };
            let mut information = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: successful fstatfs initializes the complete structure.
            if unsafe { libc::fstatfs(file.as_raw_fd(), information.as_mut_ptr()) } != 0 {
                continue;
            }
            let information = unsafe { information.assume_init() };
            // A runtime's tmpfs/bind-file imitation of boot_id is not evidence.
            if information.f_type as u64 != 0x9fa0 {
                continue;
            }
            let mut value = String::new();
            if (&mut file).take(65).read_to_string(&mut value).is_err() {
                continue;
            }
            let value = value.trim();
            if uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
            {
                return Some(format!("linux:{value}"));
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let mut bytes = [0u8; 64];
        let mut length = bytes.len();
        // SAFETY: a fixed NUL-terminated name, bounded writable result buffer,
        // and NULL new-value arguments perform a read-only kernel query.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || length == 0 || length > bytes.len() {
            return None;
        }
        let value = std::str::from_utf8(&bytes[..length])
            .ok()?
            .trim_end_matches('\0')
            .trim()
            .to_ascii_lowercase();
        uuid::Uuid::parse_str(&value)
            .ok()
            .filter(|id| !id.is_nil() && id.to_string() == value)
            .map(|_| format!("macos:{value}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    None
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
