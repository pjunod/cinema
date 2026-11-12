//! Host-admin source-open census for a witness the ordinary replay refuses.
//!
//! Any unmediated child launch poisons every armed reader anchor, because the
//! anchor lock can no longer stand for every holder of the source. When the
//! owner then dies before its explicit join, its custody has no live owner
//! and no replayable witness. This model proves physical closure from the
//! source itself: a root helper on the owner host reads the kernel's open
//! counts for the exact source inode on every superblock that reaches it, in
//! two passes. Every open file description counts there, including inherited,
//! mmapped and SCM_RIGHTS in-flight descriptors.
//!
//! The daemon trusts nothing else from the helper. It re-derives the owner,
//! the expired lease, the dead owner process, the anchor and the exact source
//! from its own witness, Store and filesystem view, then uses the ordinary
//! typed settlement. The worker can neither write nor remove host evidence.
use super::{clear_at, physical_stop, reader, settle_records, verify_directory, Record};
use plurx_core::store::{
    background_jobs::{JobPayload, JobToken},
    background_jobs_resources::BackgroundSourceOwner,
    Store,
};
use serde::Deserialize;
use std::{
    ffi::CString,
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};

const MODEL: &str = "source_open_census_v1";
const MAX_PROOFS: usize = 16;
const MAX_SUPERBLOCKS: usize = 32;
/// Two kernel observations at least this far apart.
const MIN_PASS_GAP_MS: i64 = 2_000;
/// A proof older than this is stale; the operator runs the helper again.
const MAX_AGE_MS: i64 = 15 * 60_000;
const MAX_SKEW_MS: i64 = 5_000;

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    version: u32,
    model: String,
    kernel_instance: String,
    owner: Owner,
    anchor: Anchor,
    source: Source,
    passes: [Pass; 2],
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Owner {
    custody_id: String,
    job_id: String,
    node_id: String,
    boot_id: String,
    claim_id: String,
    fence: i64,
}

impl Owner {
    fn matches(&self, token: &JobToken) -> bool {
        token.validate().is_ok()
            && self.job_id == token.job_id
            && self.node_id == token.node_id
            && self.boot_id == token.boot_id
            && self.claim_id == token.claim_id
            && self.fence == token.fence
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Anchor {
    device: u64,
    inode: u64,
    ctime: (i64, i64),
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Source {
    /// The helper's host path; informational. The daemon resolves its own.
    path: String,
    inode: u64,
    size: i64,
    mtime: i64,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Pass {
    at_ms: i64,
    superblocks: Vec<Superblock>,
    /// Locks of any kind on the anchor inode (`/proc/locks`).
    anchor_locks: u32,
    /// NFS silly-renamed (`.nfs*`) entries beside the source.
    silly_renames: u32,
    /// Tasks (outside the helper's own ancestry) naming the source in argv.
    path_references: u32,
    /// Present when the helper could observe the live daemon's PID namespace.
    #[serde(default)]
    owner_teardown: Option<Teardown>,
}

/// The host's view of the dead owner's process tree, used only when the
/// owner's job row is gone and with it the exact source binding.
///
/// The live daemon is PID 1 of its PID namespace. Every process in that
/// namespace started after the owner's lease ended, so none of them is the
/// owner or a survivor of it. When the owner's own namespace init exited, the
/// kernel killed every process in that namespace. A survivor could therefore
/// only live in another namespace whose init is still running: an earlier
/// daemon container would still sit in the daemon's deployment slice, where
/// nothing older than the owner's death may remain. No process outside the
/// daemon's namespace has any file, working directory, root, executable or
/// mapping on a counted superblock.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Teardown {
    /// Inode of the observed daemon's `/proc/<pid>/ns/pid`.
    pid_namespace: u64,
    /// The observed daemon is PID 1 inside that namespace.
    daemon_is_namespace_init: bool,
    /// Processes in that namespace that started at or before the owner's
    /// lease expiry.
    preexisting: u32,
    /// Processes outside that namespace but in the daemon's parent cgroup
    /// (its deployment slice, where an earlier daemon container would still
    /// be) that started at or before the owner's lease expiry.
    preexisting_in_daemon_slice: u32,
    /// Open descriptors, cwd, root, exe or mappings on any counted superblock
    /// held by processes outside that namespace (the helper excluded).
    foreign_storage_holders: u32,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Superblock {
    device: u64,
    inode: u64,
    /// `i_readcount` and `i_writecount` of the in-core inode.
    readers: i64,
    writers: i64,
}

impl Proof {
    fn shape_verified(&self, now_ms: i64) -> bool {
        let [first, second] = &self.passes;
        self.version == 1
            && self.model == MODEL
            && uuid::Uuid::parse_str(&self.owner.custody_id)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.owner.custody_id)
            && self.owner.fence > 0
            && self.source.inode != 0
            && self.source.size >= 0
            && second.at_ms - first.at_ms >= MIN_PASS_GAP_MS
            && now_ms + MAX_SKEW_MS >= second.at_ms
            && now_ms - second.at_ms <= MAX_AGE_MS
            && self.passes.iter().all(|pass| {
                !pass.superblocks.is_empty()
                    && pass.superblocks.len() <= MAX_SUPERBLOCKS
                    && pass.anchor_locks == 0
                    && pass.silly_renames == 0
                    && pass.path_references == 0
                    && pass.superblocks.iter().all(|sb| {
                        sb.inode == self.source.inode && sb.readers == 0 && sb.writers == 0
                    })
            })
            && first
                .superblocks
                .iter()
                .map(|sb| sb.device)
                .eq(second.superblocks.iter().map(|sb| sb.device))
    }
    /// Both passes observed this daemon's own PID namespace with no process
    /// older than the owner's death in it and no foreign storage holder.
    fn owner_teardown_verified(&self, context: &Context<'_>) -> bool {
        let Some(namespace) = context.namespace_init else {
            return false;
        };
        self.passes.iter().all(|pass| {
            pass.owner_teardown.as_ref().is_some_and(|teardown| {
                teardown.pid_namespace == namespace
                    && teardown.daemon_is_namespace_init
                    && teardown.preexisting == 0
                    && teardown.preexisting_in_daemon_slice == 0
                    && teardown.foreign_storage_holders == 0
            })
        })
    }
    fn census_devices(&self) -> impl Iterator<Item = u64> + '_ {
        self.passes[0].superblocks.iter().map(|sb| sb.device)
    }
}

/// The (file, size, mtime) a durable job's payload binds as its source.
fn payload_source(payload: &JobPayload) -> Option<(i64, i64, i64)> {
    match payload {
        JobPayload::EncodedOutputPrepare {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::CopyOutputPrepare {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::TranscodePrepare {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::FragmentIndexBuild {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::SubtitleTranscribe {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::SubtitleTranscribeManaged {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::SubtitleTranslationPrepare {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::SubtitleTextTranslate {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::SubtitleTextTranslateArtifact {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::MediaProbe {
            file_id,
            source_size,
            source_mtime,
            ..
        }
        | JobPayload::DvTitleScan {
            file_id,
            source_size,
            source_mtime,
            ..
        } => Some((*file_id, *source_size, *source_mtime)),
        _ => None,
    }
}

/// Wall-clock start of this daemon process, from the kernel's own record.
fn process_started_ms() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Field 22 (starttime) follows the parenthesised command name.
    let ticks: i64 = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    let boot_s: i64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    // SAFETY: sysconf has no pointer arguments.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    (hz > 0).then(|| boot_s * 1000 + ticks * 1000 / hz as i64)
}

fn read_proofs() -> io::Result<Vec<Proof>> {
    match std::fs::symlink_metadata("/run/plurx-physical-stop/source-census-v1") {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let directory = physical_stop::admin_directory(&[c"source-census-v1"])?;
    // Listing by path is only discovery; each proof is opened through the
    // pinned, administrator-owned directory descriptor.
    let mut names = Vec::new();
    for entry in
        std::fs::read_dir("/run/plurx-physical-stop/source-census-v1")?.take(MAX_PROOFS + 1)
    {
        names.push(entry?.file_name());
    }
    if names.len() > MAX_PROOFS {
        return Err(io::Error::other(
            "host source census inventory exceeds its bound",
        ));
    }
    names.sort();
    let mut proofs = Vec::new();
    for name in names {
        // Temporary `.census-*` files and anything else are not proofs.
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if !uuid::Uuid::parse_str(stem).is_ok_and(|id| id.to_string() == stem) {
            continue;
        }
        let name = CString::new(format!("{stem}.json")).map_err(io::Error::other)?;
        // One untrusted or invalid proof never blocks the others.
        match physical_stop::read_admin_file(&directory, &name)
            .and_then(|bytes| serde_json::from_slice::<Proof>(&bytes).map_err(io::Error::other))
        {
            Ok(proof) if proof.owner.custody_id == stem => proofs.push(proof),
            Ok(_) => tracing::warn!(stem, "host source census names another custody"),
            Err(error) => tracing::warn!(%error, stem, "host source census refused"),
        }
    }
    Ok(proofs)
}

pub(super) async fn replay(
    store: &Arc<dyn Store>,
    witnesses: &Path,
    legacy: &Path,
    node: &str,
) -> Result<(), String> {
    let proofs = read_proofs().map_err(|e| e.to_string())?;
    if proofs.is_empty() {
        return Ok(());
    }
    let kernel = reader::kernel_instance().ok_or("source census kernel identity unavailable")?;
    let started = crate::source_custody::process_started_ms()
        .or_else(process_started_ms)
        .ok_or("daemon process start time unavailable")?;
    let mut result = Ok(());
    for proof in proofs {
        let applied = apply(
            store,
            witnesses,
            legacy,
            node,
            &Context {
                kernel: &kernel,
                process_started_ms: started,
                now_ms: crate::state::clock_ms(),
                namespace_init: namespace_init(),
            },
            &proof,
        )
        .await;
        if let Err(error) = applied {
            tracing::warn!(custody_id = %proof.owner.custody_id, %error,
                "host source census refused; custody stays charged");
            result = Err(error);
        }
    }
    result
}

pub(super) struct Context<'a> {
    pub(super) kernel: &'a str,
    pub(super) process_started_ms: i64,
    pub(super) now_ms: i64,
    /// This daemon's PID namespace inode when it is that namespace's init.
    pub(super) namespace_init: Option<u64>,
}

/// This process's PID namespace inode, when it is PID 1 of that namespace.
fn namespace_init() -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    (std::process::id() == 1)
        .then(|| std::fs::metadata("/proc/self/ns/pid").ok())
        .flatten()
        .map(|metadata| metadata.ino())
}

pub(super) async fn apply(
    store: &Arc<dyn Store>,
    witnesses: &Path,
    legacy: &Path,
    node: &str,
    context: &Context<'_>,
    proof: &Proof,
) -> Result<(), String> {
    if !proof.shape_verified(context.now_ms) || proof.kernel_instance != context.kernel {
        return Err("host source census is stale, partial or nonzero".into());
    }
    let Some(witness) = reader::census_witness(witnesses, &proof.owner.custody_id)? else {
        // Already settled and cleaned; the operator removes host evidence.
        return Ok(());
    };
    verify_directory(witnesses)?;
    let Record::Background { token } = &witness.record else {
        return Err("host source census needs a background witness".into());
    };
    let BackgroundSourceOwner::Job { token: original } = &token.owner else {
        return Err("host source census is limited to a durable job owner".into());
    };
    // A lease that ended before this process existed was issued to an
    // earlier process. The data-directory daemon lock this process holds
    // excludes that process still running.
    if witness.kernel_instance != context.kernel
        || witness.node_id != node
        || token.custody_id != proof.owner.custody_id
        || !proof.owner.matches(original)
        || original.lease_expires_ms >= proof.passes[0].at_ms
        || original.lease_expires_ms >= context.process_started_ms
    {
        return Err("host source census and the exact dead-owner witness differ".into());
    }
    // Poisoning rewrites the anchor, so its ctime no longer matches the
    // witness; the witness's inode is the identity. The ctime the census saw
    // must still hold, so nothing touched the anchor since.
    if witness.anchor_ctime.is_none()
        || proof.anchor.device != witness.anchor_device
        || proof.anchor.inode != witness.anchor_inode
    {
        return Err("host source census names another anchor".into());
    }
    let expected = &proof.anchor;
    // The daemon's own observation: the unchanged anchor inode, no other
    // open file description holding its lock. Held through settlement.
    let anchor = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(witnesses.join(format!("background-{}.anchor", token.custody_id)))
        .map_err(|e| e.to_string())?;
    let metadata = anchor.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || reader::anchor_ctime(&metadata) != expected.ctime
        || !reader::local_filesystem(&anchor)
    {
        return Err("reader anchor changed since the witness".into());
    }
    match anchor.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err("a reader still holds the anchor lock".into())
        }
        Err(error) => return Err(error.to_string()),
    }
    if let Some(current) = store
        .resolve_background_source_custody(token.clone())
        .await
        .map_err(|e| e.to_string())?
    {
        if current.custody_id != token.custody_id
            || !matches!(&current.owner, BackgroundSourceOwner::Job { token: held }
                if proof.owner.matches(held) && held.lease_expires_ms < proof.passes[0].at_ms)
        {
            return Err("authoritative custody differs from the census owner".into());
        }
        let job = store
            .background_job(&original.job_id)
            .await
            .map_err(|e| e.to_string())?;
        let Some(job) = job else {
            // The owner's job row is gone, and with it the exact source
            // binding. Release needs the owner's whole process tree proved
            // gone instead.
            if !proof.owner_teardown_verified(context) {
                return Err("census job unavailable for source binding".into());
            }
            // The owner's own record of the inode it opened still binds.
            if witness.source.is_some_and(|(device, inode)| {
                inode != proof.source.inode
                    || !proof.census_devices().any(|counted| counted == device)
            }) {
                return Err("census source is not the inode the owner opened".into());
            }
            settle_released(store, token, &witness).await?;
            return finish(legacy, witnesses, &witness, anchor, token, proof);
        };
        if job.id != original.job_id
            || job.token.as_ref().is_some_and(|live| {
                proof.owner.matches(live) && live.lease_expires_ms >= proof.passes[0].at_ms
            })
        {
            return Err("census job still holds the owner's unexpired claim".into());
        }
        let (file_id, size, mtime) = job
            .supported_payload()
            .ok()
            .and_then(|payload| payload_source(&payload))
            .ok_or("census job carries no exact source binding")?;
        let file = store
            .get_file(file_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("census source file row unavailable")?;
        // The daemon's own view of the same physical file: inode, size and
        // mtime, on a superblock the census actually counted.
        let seen = std::fs::metadata(&file.path).map_err(|e| e.to_string())?;
        if witness.source.is_some_and(|(device, inode)| {
            seen.dev() != device
                || seen.ino() != inode
                || inode != proof.source.inode
                || !proof.census_devices().any(|counted| counted == device)
        }) {
            return Err("census source is not the inode the owner opened".into());
        }
        if file.size != size
            || file.mtime != mtime
            || proof.source.size != size
            || proof.source.mtime != mtime
            || seen.len() as i64 != size
            || seen.mtime() != mtime
            || seen.ino() != proof.source.inode
            || !proof.census_devices().any(|device| device == seen.dev())
        {
            return Err("census source differs from the job's exact source".into());
        }
        settle_released(store, token, &witness).await?;
    }
    finish(legacy, witnesses, &witness, anchor, token, proof)
}

async fn settle_released(
    store: &Arc<dyn Store>,
    token: &plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
    witness: &reader::CensusWitness,
) -> Result<(), String> {
    settle_records(store, std::slice::from_ref(&witness.record)).await?;
    if store
        .resolve_background_source_custody(token.clone())
        .await
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("census settlement awaits exact release acknowledgement".into());
    }
    Ok(())
}

fn finish(
    legacy: &Path,
    witnesses: &Path,
    witness: &reader::CensusWitness,
    anchor: std::fs::File,
    token: &plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
    proof: &Proof,
) -> Result<(), String> {
    clear_at(legacy, &witness.record)?;
    reader::complete_at(witnesses, &witness.record)?;
    drop(anchor);
    tracing::info!(custody_id = %token.custody_id, source = %proof.source.path,
        "released dead-owner source custody after host source-open census");
    Ok(())
}

#[cfg(test)]
mod tests;
