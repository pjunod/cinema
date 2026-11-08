//! Host-admin evidence for the pre-witness native Dolby Vision title reader.
//! The worker cannot create or delete this proof. Its journal supplies the
//! old exact activation, and the authoritative Store supplies the job type.
use super::{clear_at, read_bytes, reader, settle_records, verify_directory, Envelope, Record};
use plurx_core::store::{
    background_jobs::{JobPayload, JobToken},
    background_jobs_resources::BackgroundSourceOwner,
    Store,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::CStr,
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
    sync::Arc,
};

const MAX_PROOF: u64 = 256 * 1024;
// Preserve the scope of this historical source audit. Later activations,
// including other native title scans, cannot inherit this recovery proof.
const ACTIVATION: &str = "50f069fbc8662aa7e3e43fdf9368d7ee1dc9b5356783291f4105e5541a4deb78";
// This is an audited reference build, not a historical PID-to-image mapping.
// The job payload independently binds its scanner ELF through the title digest.
// Native tool capture verifies the 17-input source identity before admission.
const REFERENCE_COMMIT: &str = "1d11f0cdfa269e9b56dec0ffe45a10cefcb26712";
const SOURCE: &str = "4e893cee571416198b3dc1103f993530bc1168035e26dd6e6f045afd06385f13";
const MANIFEST: &str = "0169c14ae9016916cb02f2dce964d78ab53cdf1f8558859af93e61a4346d22a8";
const DAEMON: &str = "ff7be59be5796404b5a904f563d38fc0decae683d6b5c46e26d089a4ecb8472a";
const SCANNER: &str = "c8833ae35a60443e82924d18b6b53b53cc43ee46e29cae7733630146c5e17035";
const RENDERER: &str = "626f130b25707c01ccd22d082f465465b7037d3d7df792b31edce216bab35d6f";
const TITLE_SCANNER: &str = "12315d1fb46df2e0ebe1bbcf549cf246e3ae961bc7ef1e0171a845201bcf947a";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u32,
    model: String,
    kernel_instance: String,
    activation_sha256: String,
    observed_at_ms: i64,
    owner: Owner,
    closure: Closure,
    census: Census,
}

#[derive(Deserialize)]
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Closure {
    source_commit: String,
    native_source_sha256: String,
    manifest_sha256: String,
    daemon_sha256: String,
    scanner_sha256: String,
    renderer_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Census {
    backend: String,
    passes: [Pass; 2],
    images: Vec<Image>,
}

#[derive(Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Pass {
    tasks: u64,
    executables: usize,
    matched_readers: usize,
    snapshot_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    device: u64,
    inode: u64,
    bytes: u64,
    sha256: String,
    matched_roles: Vec<String>,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}

impl Proof {
    fn verified(&self, node: &str, kernel: &str, now_ms: i64) -> bool {
        let c = &self.closure;
        let census = &self.census;
        let pass = &census.passes[0];
        let mut inodes = HashSet::new();
        self.version == 1
            && self.model == "legacy_dv_title_reader_class_zero_v1"
            && self.kernel_instance == kernel
            && self.activation_sha256 == ACTIVATION
            && self.owner.node_id == node
            && self.owner.fence > 0
            && [
                &self.owner.custody_id,
                &self.owner.job_id,
                &self.owner.boot_id,
                &self.owner.claim_id,
            ]
            .into_iter()
            .all(|value| {
                uuid::Uuid::parse_str(value)
                    .is_ok_and(|id| !id.is_nil() && id.to_string() == *value)
            })
            && (1..=now_ms).contains(&self.observed_at_ms)
            && c.source_commit == REFERENCE_COMMIT
            && c.native_source_sha256 == SOURCE
            && c.manifest_sha256 == MANIFEST
            && c.daemon_sha256 == DAEMON
            && c.scanner_sha256 == SCANNER
            && c.renderer_sha256 == RENDERER
            && census.backend == "linux_initial_pidns_executable_markers_v1"
            && *pass == census.passes[1]
            && (1..=65_536).contains(&pass.tasks)
            && (1..=512).contains(&pass.executables)
            && pass.executables as u64 <= pass.tasks
            && pass.executables == census.images.len()
            && pass.matched_readers == 0
            && digest(&pass.snapshot_sha256)
            && census.images.iter().all(|image| {
                image.inode != 0
                    && image.bytes != 0
                    && inodes.insert((image.device, image.inode))
                    && image.matched_roles.is_empty()
                    && digest(&image.sha256)
                    && ![DAEMON, SCANNER, RENDERER].contains(&image.sha256.as_str())
            })
    }
}

fn open_at(parent: &File, name: &CStr, directory: bool) -> io::Result<File> {
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent owns a live descriptor and name is one fixed C string.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor and ownership transfers once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn administrator_owned(file: &File, directory: bool) -> io::Result<bool> {
    let m = file.metadata()?;
    Ok(m.uid() == 0
        && m.mode() & 0o022 == 0
        && reader::local_filesystem(file)
        && if directory {
            m.is_dir()
        } else {
            m.is_file() && m.nlink() == 1 && m.len() <= MAX_PROOF
        })
}

fn read_host_proof() -> io::Result<Proof> {
    // Resolve the complete fixed path through pinned descriptors. An absent
    // optional mount does not change the ordinary worker recovery path.
    std::fs::symlink_metadata("/run/plurx-physical-stop/legacy-dv-title-v1.json")?;
    // Linux reports effective/permitted capabilities per thread. Even a
    // nonroot worker with CAP_CHOWN or CAP_DAC_OVERRIDE could forge a proof.
    let status = std::fs::read_to_string("/proc/thread-self/status")?;
    // SAFETY: geteuid has no pointer arguments or side effects.
    if unsafe { libc::geteuid() } == 0
        || !["CapEff:", "CapPrm:"].into_iter().all(|field| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .is_some_and(|hex| u64::from_str_radix(hex.trim(), 16) == Ok(0))
        })
    {
        return Err(io::Error::other(
            "host physical-stop evidence needs an unprivileged worker",
        ));
    }
    let root = File::open("/")?;
    let mut parent = root;
    for name in [c"run", c"plurx-physical-stop"] {
        if !administrator_owned(&parent, true)? {
            return Err(io::Error::other("untrusted host physical-stop ancestor"));
        }
        parent = open_at(&parent, name, true)?;
    }
    if !administrator_owned(&parent, true)? {
        return Err(io::Error::other("untrusted host physical-stop directory"));
    }
    let file = open_at(&parent, c"legacy-dv-title-v1.json", false)?;
    if !administrator_owned(&file, false)? {
        return Err(io::Error::other("untrusted host physical-stop proof file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROOF + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROOF {
        return Err(io::Error::other("host physical-stop proof exceeds bound"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub(super) async fn replay(
    store: &Arc<dyn Store>,
    journal: &Path,
    node: &str,
) -> Result<(), String> {
    let proof = match read_host_proof() {
        Ok(proof) => proof,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let kernel = reader::kernel_instance().ok_or("physical-stop kernel identity unavailable")?;
    apply(store, journal, node, &kernel, &proof).await
}

async fn apply(
    store: &Arc<dyn Store>,
    journal: &Path,
    node: &str,
    kernel: &str,
    proof: &Proof,
) -> Result<(), String> {
    if !proof.verified(node, kernel, crate::state::clock_ms()) {
        return Err("host physical-stop proof does not match the audited reader census".into());
    }
    let path = journal.join(format!("background-{}.json", proof.owner.custody_id));
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(());
    }
    verify_directory(journal)?;
    let witness = journal
        .with_file_name("v2")
        .join(path.file_name().ok_or("invalid legacy activation name")?);
    for extension in ["json", "anchor", "armed", "unmediated"] {
        match std::fs::symlink_metadata(witness.with_extension(extension)) {
            Ok(_) => {
                return Err(
                    "legacy physical-stop evidence cannot supersede a reader witness".into(),
                );
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let bytes = read_bytes(&path)?;
    if hex::encode(Sha256::digest(&bytes)) != ACTIVATION {
        return Err("historical physical-stop activation bytes differ".into());
    }
    let activation = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    settle_activation(store, journal, node, proof, &activation).await
}

async fn settle_activation(
    store: &Arc<dyn Store>,
    journal: &Path,
    node: &str,
    proof: &Proof,
    activation: &Envelope,
) -> Result<(), String> {
    let [record @ Record::Background { token }] = activation.records.as_slice() else {
        return Err("host physical-stop proof needs one legacy background activation".into());
    };
    let BackgroundSourceOwner::Job { token: original } = &token.owner else {
        return Err("host physical-stop proof is limited to a durable job".into());
    };
    if activation.version != 1
        || activation.node_id != node
        || token.custody_id != proof.owner.custody_id
        || !proof.owner.matches(original)
        || original.lease_expires_ms >= proof.observed_at_ms
    {
        return Err("host physical-stop proof and exact legacy activation differ".into());
    }
    let present = store
        .resolve_background_source_custody(token.clone())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(current) = present {
        if current.custody_id != token.custody_id
            || !matches!(&current.owner, BackgroundSourceOwner::Job { token: held }
                if proof.owner.matches(held) && held.lease_expires_ms < proof.observed_at_ms)
        {
            return Err(
                "authoritative source custody differs from the historical title owner".into(),
            );
        }
        let job = store
            .background_job(&original.job_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("legacy job unavailable for physical-stop classification")?;
        if job.id != original.job_id
            || !job.token.as_ref().is_some_and(|live| {
                proof.owner.matches(live) && live.lease_expires_ms < proof.observed_at_ms
            })
            || !matches!(job.supported_payload(), Ok(JobPayload::DvTitleScan { scanner_digest, .. }) if scanner_digest == TITLE_SCANNER)
        {
            return Err(
                "host physical-stop proof needs the expired exact native DV title job".into(),
            );
        }
        settle_records(store, std::slice::from_ref(record)).await?;
        if store
            .resolve_background_source_custody(token.clone())
            .await
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err(
                "host physical-stop proof retained pending exact release acknowledgement".into(),
            );
        }
    }
    // The daemon clears only its own exact activation after authoritative
    // absence. Host-only evidence remains read-only until an admin removes it.
    clear_at(journal, record)?;
    tracing::info!(custody_id = %token.custody_id, "acknowledged exact legacy DV source custody after host reader census");
    Ok(())
}

#[cfg(test)]
mod tests;
