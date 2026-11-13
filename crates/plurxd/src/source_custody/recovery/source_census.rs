//! Host-admin observations plus an independent whole-owner closure proof.
//!
//! Zero open handles on one inode cannot prove that an unconfined surviving
//! child will not open that source (or another source) later. A poisoned
//! witness therefore needs proof that its recorded kernel ended on this
//! machine. Even child exit cannot recall descriptors exported outside the
//! process tree. Census values and pathname metadata never substitute for
//! that closure proof. The immutable witness and authoritative Store still
//! bind the exact custody being settled.
use super::{clear_at, physical_stop, reader, settle_records, verify_directory, Record};
use plurx_core::store::{
    background_jobs::JobToken,
    background_jobs_resources::{BackgroundSourceOwner, ForegroundSourceToken},
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
    /// The witness's kernel boot, when the census ran on a later boot of the
    /// same machine and the host's own journal showed that boot had ended.
    #[serde(default)]
    prior_boot: Option<PriorBoot>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct PriorBoot {
    kernel_instance: String,
}

impl Proof {
    fn witness_kernel_settled(&self, witness_kernel: &str, current: &str) -> bool {
        witness_kernel == current
            || self
                .prior_boot
                .as_ref()
                .is_some_and(|ended| ended.kernel_instance == witness_kernel)
    }
}

/// The dead owner a census names: a durable job's custody or a playback
/// (foreground) source owner. The shapes are disjoint, so the helper's JSON
/// names exactly one.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum Owner {
    Job(JobOwner),
    Foreground(ForegroundOwner),
}

impl Owner {
    fn custody_id(&self) -> &str {
        match self {
            Self::Job(owner) => &owner.custody_id,
            Self::Foreground(owner) => &owner.custody_id,
        }
    }
    fn fence(&self) -> i64 {
        match self {
            Self::Job(owner) => owner.fence,
            Self::Foreground(owner) => owner.fence,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct JobOwner {
    custody_id: String,
    job_id: String,
    node_id: String,
    boot_id: String,
    claim_id: String,
    fence: i64,
}

/// A playback source owner. Its custody is named by the owner id; the
/// remaining fields are the exact registration identity.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct ForegroundOwner {
    custody_id: String,
    lifetime_id: String,
    session_incarnation: String,
    producer_attempt: String,
    node_id: String,
    boot_id: String,
    fence: i64,
}

impl ForegroundOwner {
    fn matches(&self, token: &ForegroundSourceToken) -> bool {
        self.custody_id == token.owner.owner_id
            && self.lifetime_id == token.owner.lifetime_id
            && self.session_incarnation == token.owner.session_incarnation
            && self.producer_attempt == token.owner.producer_attempt
            && self.node_id == token.owner.node_id
            && self.boot_id == token.owner.boot_id
            && self.fence == token.fence
    }
}

impl JobOwner {
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
    /// The helper's host path; diagnostic only, never owner identity.
    path: String,
    inode: u64,
    size: i64,
    #[serde(rename = "mtime")]
    _mtime: i64,
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
    #[serde(default, rename = "owner_teardown")]
    _owner_teardown: Option<serde_json::Value>,
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
            && uuid::Uuid::parse_str(self.owner.custody_id())
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.owner.custody_id())
            && self.owner.fence() > 0
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
    fn owner_stopped(&self, witness: &reader::CensusWitness, context: &Context<'_>) -> bool {
        // Only whole-kernel closure covers unknown children, later opens,
        // different objects and descriptors exported outside the process tree.
        // The administrator's helper verifies this machine's own boot journal.
        witness.kernel_instance != context.kernel
            && self.prior_boot.as_ref().is_some_and(|ended| {
                ended.kernel_instance == witness.kernel_instance
                    && ended.kernel_instance != context.kernel
            })
    }
}

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
            Ok(proof) if proof.owner.custody_id() == stem => proofs.push(proof),
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
            },
            &proof,
        )
        .await;
        if let Err(error) = applied {
            tracing::warn!(custody_id = %proof.owner.custody_id(), %error,
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
    let owner = match &proof.owner {
        Owner::Job(owner) => owner,
        Owner::Foreground(owner) => {
            return apply_foreground(store, witnesses, legacy, node, context, proof, owner).await
        }
    };
    let Some(witness) = reader::census_witness(
        witnesses,
        reader::CensusFamily::Background,
        &owner.custody_id,
    )?
    else {
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
    if !proof.witness_kernel_settled(&witness.kernel_instance, context.kernel)
        || witness.node_id != node
        || token.custody_id != owner.custody_id
        || !owner.matches(original)
        || original.lease_expires_ms >= proof.passes[0].at_ms
        || original.lease_expires_ms >= context.process_started_ms
    {
        return Err("host source census and the exact dead-owner witness differ".into());
    }
    if !proof.owner_stopped(&witness, context) {
        return Err("census cannot prove the exact owner's whole process tree stopped: exact ended-kernel proof required".into());
    }
    let anchor = held_anchor(witnesses, reader::CensusFamily::Background, &witness, proof)?;
    if let Some(current) = store
        .resolve_background_source_custody(token.clone())
        .await
        .map_err(|e| e.to_string())?
    {
        if current.custody_id != token.custody_id
            || !matches!(&current.owner, BackgroundSourceOwner::Job { token: held }
                if owner.matches(held) && held.lease_expires_ms < proof.passes[0].at_ms)
        {
            return Err("authoritative custody differs from the census owner".into());
        }
        settle_released(store, token, &witness).await?;
    }
    finish(
        legacy,
        witnesses,
        &witness,
        anchor,
        &token.custody_id,
        proof,
    )
}

/// The anchor the census observed, as the daemon itself sees it: the
/// witness's inode, unchanged since the census (poisoning rewrote it, so the
/// witness's own ctime no longer matches), and no other open file
/// description holding its lock. The returned descriptor holds the lock
/// through settlement.
fn held_anchor(
    witnesses: &Path,
    family: reader::CensusFamily,
    witness: &reader::CensusWitness,
    proof: &Proof,
) -> Result<std::fs::File, String> {
    if witness.anchor_ctime.is_none()
        || proof.anchor.device != witness.anchor_device
        || proof.anchor.inode != witness.anchor_inode
    {
        return Err("host source census names another anchor".into());
    }
    let expected = &proof.anchor;
    let anchor = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(witnesses.join(format!(
            "{}-{}.anchor",
            family.prefix(),
            proof.owner.custody_id()
        )))
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
    Ok(anchor)
}

/// The exact persisted playback owner can be released only after its witness
/// kernel ended on this machine. Renewed fence/lease checks and acknowledged
/// typed settlement remain mandatory. Current file rows and pathname stats
/// cannot reconstruct the held SourceFence object version or confine a child.
async fn apply_foreground(
    store: &Arc<dyn Store>,
    witnesses: &Path,
    legacy: &Path,
    node: &str,
    context: &Context<'_>,
    proof: &Proof,
    owner: &ForegroundOwner,
) -> Result<(), String> {
    let Some(witness) = reader::census_witness(
        witnesses,
        reader::CensusFamily::Foreground,
        &owner.custody_id,
    )?
    else {
        // Already settled and cleaned; the operator removes host evidence.
        return Ok(());
    };
    verify_directory(witnesses)?;
    let Record::Foreground { token } = &witness.record else {
        return Err("host source census needs a foreground witness".into());
    };
    if !proof.witness_kernel_settled(&witness.kernel_instance, context.kernel)
        || witness.node_id != node
        || token.owner.node_id != node
        || !owner.matches(token)
    {
        return Err("host source census and the exact dead-owner witness differ".into());
    }
    if !proof.owner_stopped(&witness, context) {
        return Err("census cannot prove the exact owner's whole process tree stopped: exact ended-kernel proof required".into());
    }
    let anchor = held_anchor(witnesses, reader::CensusFamily::Foreground, &witness, proof)?;
    if let Some(current) = store
        .resolve_foreground_source(token.clone())
        .await
        .map_err(|e| e.to_string())?
    {
        // Every heartbeat projection advances the row's fence past the
        // activation-time copy in the witness; identity is the exact owner.
        if current.owner != token.owner
            || current.fence < token.fence
            || current.lease_expires_at_ms >= proof.passes[0].at_ms
        {
            return Err("authoritative playback owner may still be live".into());
        }
        // The authoritative token identifies the exact old owner. Its whole
        // process tree is gone, irrespective of source replacement, rescans,
        // object-version encoding, or which files an unknown child opened.
        settle_records(store, std::slice::from_ref(&witness.record)).await?;
        if store
            .resolve_foreground_source(token.clone())
            .await
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("census settlement awaits exact release acknowledgement".into());
        }
    }
    finish(
        legacy,
        witnesses,
        &witness,
        anchor,
        &owner.custody_id,
        proof,
    )
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
    custody_id: &str,
    proof: &Proof,
) -> Result<(), String> {
    clear_at(legacy, &witness.record)?;
    reader::complete_at(witnesses, &witness.record)?;
    drop(anchor);
    tracing::info!(custody_id = %custody_id, source = %proof.source.path,
        "released dead-owner source custody after host source-open census");
    Ok(())
}

#[cfg(test)]
mod tests;
