//! Recovery evidence is distinct from a pre-read activation or a joined receipt.
//! Only a verified reboot of the same physical host proves all former readers,
//! including escaped process groups, stopped. Same-boot evidence stays held.
use super::*;
use plurx_core::store::background_jobs_resources::{
    BackgroundSourceCustodyToken, BackgroundSourceOwner,
};
use serde::{Deserialize, Serialize};
use std::io::Read;

const MAX_BYTES: u64 = 1_048_576;
const MAX_RECORDS: usize = 128;
const MAX_FILES: usize = 4096;

struct Context {
    store: std::sync::Weak<dyn Store>,
    dir: PathBuf,
    authoritative: bool,
}

#[derive(Clone)]
struct ContextSnapshot {
    dir: PathBuf,
    authoritative: bool,
}

static CONTEXTS: std::sync::OnceLock<std::sync::Mutex<Vec<Context>>> = std::sync::OnceLock::new();

/// A source journal belongs to the exact Store selected for this daemon boot.
/// SQLite activation-recovery and unrelated test Stores cannot inherit the
/// authoritative backend's directory or consume its physical-stop evidence.
pub(super) fn configure_backend(
    store: &Arc<dyn Store>,
    data_dir: &Path,
    authoritative: bool,
) -> Result<(), String> {
    if authoritative && data_dir.as_os_str().is_empty() {
        return Err("authoritative source recovery needs a persistent data directory".into());
    }
    let context = Context {
        store: Arc::downgrade(store),
        dir: data_dir.join("source-custody-recovery/v1"),
        authoritative,
    };
    let mut contexts = CONTEXTS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| "source recovery context registry poisoned")?;
    contexts.retain(|context| context.store.strong_count() != 0);
    if let Some(existing) = contexts.iter_mut().find(|context| {
        context
            .store
            .upgrade()
            .is_some_and(|configured| Arc::ptr_eq(&configured, store))
    }) {
        *existing = context;
    } else {
        contexts.push(context);
    }
    Ok(())
}

fn context_snapshot(store: &Arc<dyn Store>) -> Result<Option<ContextSnapshot>, String> {
    let Some(contexts) = CONTEXTS.get() else {
        return Ok(None);
    };
    let mut contexts = contexts
        .lock()
        .map_err(|_| "source recovery context registry poisoned")?;
    contexts.retain(|context| context.store.strong_count() != 0);
    Ok(contexts.iter().find_map(|context| {
        context.store.upgrade().and_then(|configured| {
            Arc::ptr_eq(&configured, store).then(|| ContextSnapshot {
                dir: context.dir.clone(),
                authoritative: context.authoritative,
            })
        })
    }))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    machine_id: String,
    boot_id: String,
    node_id: String,
    records: Vec<Record>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Foreground { token: ForegroundSourceToken },
    Background { token: BackgroundSourceCustodyToken },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    machine_id: String,
    boot_id: String,
}

impl Identity {
    fn current() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            Self::linux_host(|path| std::fs::read_to_string(path))
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    #[cfg(any(target_os = "linux", test))]
    fn linux_host(mut read: impl FnMut(&str) -> std::io::Result<String>) -> Option<Self> {
        // Require an explicit read-only bind from the actual host for native
        // services and containers alike. Missing container markers cannot
        // establish native execution, and image machine IDs are not host proof.
        let machine_id = read("/run/plurx-host-machine-id").ok()?;
        // Some runtimes also virtualize /proc's boot UUID per container.
        // Only the explicitly supplied actual-host boot identity is proof.
        let boot_id = read("/run/plurx-host-boot-id").ok()?;
        let identity = Self {
            machine_id: machine_id.trim().to_owned(),
            boot_id: boot_id.trim().to_owned(),
        };
        identity.valid().then_some(identity)
    }

    fn valid(&self) -> bool {
        self.machine_id.len() == 32
            && self.machine_id.bytes().any(|byte| byte != b'0')
            && self.machine_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            && uuid::Uuid::parse_str(&self.boot_id)
                .is_ok_and(|boot| !boot.is_nil() && boot.to_string() == self.boot_id)
    }
}

impl Record {
    fn same_owner(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Foreground { token: left }, Self::Foreground { token: right }) => {
                left.owner == right.owner
            }
            (Self::Background { token: left }, Self::Background { token: right }) => {
                use plurx_core::store::background_jobs_resources::background_owner_key;
                left.custody_id == right.custody_id
                    && matches!((background_owner_key(&left.owner), background_owner_key(&right.owner)), (Ok(left), Ok(right)) if left == right)
            }
            _ => false,
        }
    }

    fn node(&self) -> &str {
        match self {
            Self::Foreground { token } => &token.owner.node_id,
            Self::Background { token } => match &token.owner {
                BackgroundSourceOwner::Job { token } => &token.node_id,
                BackgroundSourceOwner::Analysis { node_id, .. }
                | BackgroundSourceOwner::Offline { node_id, .. } => node_id,
                BackgroundSourceOwner::ReadAttestation { lease, .. } => &lease.owner_node_id,
                BackgroundSourceOwner::Companion { token } => &token.owner.node_id,
            },
        }
    }

    fn name(&self) -> Result<String, String> {
        let (kind, id) = match self {
            Self::Foreground { token } => ("foreground", &token.owner.owner_id),
            Self::Background { token } => {
                if matches!(token.owner, BackgroundSourceOwner::Companion { .. }) {
                    return Err("companion recovery belongs to foreground custody".into());
                }
                ("background", &token.custody_id)
            }
        };
        uuid::Uuid::parse_str(id).map_err(|error| error.to_string())?;
        Ok(format!("{kind}-{id}.json"))
    }
}

fn private_directory(dir: &Path) -> Result<(), String> {
    private_directory_with_sync(dir, |path| std::fs::File::open(path)?.sync_all())
}

fn private_directory_with_sync(
    dir: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if dir.file_name().is_none_or(|name| name != "v1")
        || dir
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|name| name != "source-custody-recovery")
    {
        return Err("unexpected source recovery directory layout".into());
    }
    let base = dir.parent().ok_or("missing recovery parent")?;
    let data = base
        .parent()
        .ok_or("missing source recovery data directory")?;
    let metadata = std::fs::symlink_metadata(data).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("source recovery data directory must already exist without symlinks".into());
    }
    for path in [base, dir] {
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("source recovery directory is not a private directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }
        // Fsync the new directory's metadata and its name in the parent before
        // proceeding to the next level. Repeat both syncs for an existing
        // directory: an earlier attempt may have failed after mkdir/chmod.
        sync(path).map_err(|error| error.to_string())?;
        sync(path.parent().ok_or("missing publication parent")?)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn verify_directory(dir: &Path) -> Result<(), String> {
    for path in [dir.parent().ok_or("missing recovery parent")?, dir] {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("source recovery directory must not be a symlink".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o077 != 0 {
                return Err("source recovery directory is not private".into());
            }
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<Envelope, String> {
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
        return Err("invalid source recovery record size/type".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err("source recovery evidence must be private and have one link".into());
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("source recovery record grew past its bound".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn write_active_at(dir: &Path, record: Record, identity: Option<Identity>) -> Result<(), String> {
    private_directory(dir)?;
    let path = dir.join(record.name()?);
    if path.try_exists().map_err(|error| error.to_string())? {
        let existing = read(&path)?;
        if existing.version == 1
            && existing.records.len() == 1
            && existing.node_id == record.node()
            && existing.records[0].same_owner(&record)
        {
            // Replaying an activation cannot rewrite the earlier boot proof.
            return Ok(());
        }
        return Err("source recovery identity already has different evidence".into());
    }
    if std::fs::read_dir(dir)
        .map_err(|error| error.to_string())?
        .take(MAX_FILES)
        .count()
        >= MAX_FILES
    {
        return Err("source recovery journal needs operator attention: capacity reached".into());
    }
    let identity = identity.unwrap_or(Identity {
        machine_id: String::new(),
        boot_id: String::new(),
    });
    let envelope = Envelope {
        version: 1,
        machine_id: identity.machine_id,
        boot_id: identity.boot_id,
        node_id: record.node().to_owned(),
        records: vec![record],
    };
    let bytes = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("source recovery record exceeds its bound".into());
    }
    let temporary = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| error.to_string())
}

pub(super) fn foreground(
    store: &Arc<dyn Store>,
    token: &ForegroundSourceToken,
) -> Result<(), String> {
    let Some(context) = context_snapshot(store)?.filter(|context| context.authoritative) else {
        return Ok(());
    };
    write_active_at(
        &context.dir,
        Record::Foreground {
            token: token.clone(),
        },
        Identity::current(),
    )
}

pub(super) fn background(
    store: &Arc<dyn Store>,
    token: &BackgroundSourceCustodyToken,
) -> Result<(), String> {
    let Some(context) = context_snapshot(store)?.filter(|context| context.authoritative) else {
        return Ok(());
    };
    write_active_at(
        &context.dir,
        Record::Background {
            token: token.clone(),
        },
        Identity::current(),
    )
}

pub(super) fn clear(store: &Arc<dyn Store>, receipt: &SettlementReceipt) -> Result<(), String> {
    let Some(context) = context_snapshot(store)?.filter(|context| context.authoritative) else {
        return Ok(());
    };
    let record = match receipt {
        SettlementReceipt::Foreground(token) => Record::Foreground {
            token: token.clone(),
        },
        SettlementReceipt::Background(token) => Record::Background {
            token: token.clone(),
        },
        _ => return Ok(()),
    };
    clear_at(&context.dir, &record)
}

/// Activation success also clears its legacy intent. Only this separate path
/// may remove active evidence, after an authoritative exact-owner absence.
/// An unconfigured/non-authoritative Store returns `true` only because there
/// is no modern metadata cleanup for it. That is not negative authority proof
/// and never touches another Store's recovery evidence.
pub(super) async fn clear_aborted_if_absent(
    store: &Arc<dyn Store>,
    receipt: &SettlementReceipt,
) -> Result<bool, String> {
    let Some(context) = context_snapshot(store)?.filter(|context| context.authoritative) else {
        return Ok(true);
    };
    let record = match receipt {
        SettlementReceipt::ForegroundActivation { token, .. } => {
            if store
                .resolve_foreground_source(token.clone())
                .await
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Ok(false);
            }
            Record::Foreground {
                token: token.clone(),
            }
        }
        SettlementReceipt::BackgroundActivation { token, .. } => {
            if store
                .resolve_background_source_custody(token.clone())
                .await
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Ok(false);
            }
            Record::Background {
                token: token.clone(),
            }
        }
        _ => return Err("abort cleanup requires an exact activation receipt".into()),
    };
    clear_at(&context.dir, &record)?;
    Ok(true)
}

fn clear_at(dir: &Path, record: &Record) -> Result<(), String> {
    let path = dir.join(record.name()?);
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let envelope = read(&path)?;
    if envelope.version != 1
        || envelope.records.len() != 1
        || !envelope.records[0].same_owner(record)
    {
        return Err("source recovery cleanup identity mismatch".into());
    }
    std::fs::remove_file(path).map_err(|error| error.to_string())?;
    std::fs::File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

fn verified(envelope: &Envelope, identity: &Identity, node: &str) -> bool {
    let prior = Identity {
        machine_id: envelope.machine_id.clone(),
        boot_id: envelope.boot_id.clone(),
    };
    envelope.version == 1
        && prior.valid()
        && identity.valid()
        && prior.machine_id == identity.machine_id
        && prior.boot_id != identity.boot_id
        && envelope.node_id == node
        && !envelope.records.is_empty()
        && envelope.records.len() <= MAX_RECORDS
        && envelope
            .records
            .iter()
            .all(|record| record.node() == node && record.name().is_ok())
}

pub(super) async fn replay(store: &Arc<dyn Store>, node: &str) -> Result<(), String> {
    // SQLite activation-recovery is not the cluster's authoritative Store.
    // Its successful local write must never consume replicated-owner evidence.
    let Some(context) = context_snapshot(store)?.filter(|context| context.authoritative) else {
        return Ok(());
    };
    let Some(identity) = Identity::current() else {
        return Ok(());
    };
    replay_at(store, &context.dir, &identity, node).await
}

async fn replay_at(
    store: &Arc<dyn Store>,
    dir: &Path,
    identity: &Identity,
    node: &str,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    verify_directory(dir)?;
    let mut paths = entries
        .take(MAX_FILES + 1)
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if paths.len() > MAX_FILES {
        return Err("source recovery file inventory exceeds its bound".into());
    }
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    // Rotate batches: an unchanged-boot record cannot hide later reboot proof.
    static CURSOR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    let after = CURSOR
        .lock()
        .map_err(|_| "recovery cursor poisoned")?
        .clone();
    let offset = after
        .as_ref()
        .map_or(0, |after| paths.partition_point(|path| path <= after));
    let length = paths.len();
    if length == 0 {
        return Ok(());
    }
    paths.rotate_left(offset % length);
    for path in paths.into_iter().take(MAX_RECORDS) {
        *CURSOR.lock().map_err(|_| "recovery cursor poisoned")? = Some(path.clone());
        let envelope = match read(&path) {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::warn!(%error, "invalid source recovery evidence retained");
                continue;
            }
        };
        if !verified(&envelope, identity, node) {
            continue;
        }
        for record in &envelope.records {
            match record {
                Record::Background { token } => {
                    if store
                        .mark_background_source_settled(token.clone())
                        .await
                        .map_err(|error| error.to_string())?
                        && !store
                            .release_background_source_custody(
                                token.clone(),
                                crate::state::clock_ms(),
                            )
                            .await
                            .map_err(|error| error.to_string())?
                    {
                        return Err(
                            "background reboot recovery release needs reconciliation".into()
                        );
                    }
                }
                Record::Foreground { token } => {
                    if store
                        .mark_foreground_source_settled(token.clone())
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        if let Some(current) = store
                            .resolve_foreground_source(token.clone())
                            .await
                            .map_err(|error| error.to_string())?
                        {
                            if !store
                                .release_foreground_source(current, crate::state::clock_ms())
                                .await
                                .map_err(|error| error.to_string())?
                            {
                                return Err(
                                    "foreground reboot recovery release needs reconciliation"
                                        .into(),
                                );
                            }
                        }
                    }
                }
            }
        }
        // Record removal only follows authoritative settlement acknowledgement.
        std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        std::fs::File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        tracing::info!(
            records = envelope.records.len(),
            "source custody recovered after verified host reboot"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod abort_tests;

#[cfg(test)]
pub(super) fn fail_activation_clear_for_test() -> bool {
    abort_tests::fail_clear()
}
