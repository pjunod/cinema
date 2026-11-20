//! Local durable material for replicated NP1 attempt reservations.
//!
//! The title fence consumes a slot in the replicated store before a local
//! intent or child is allowed. The local journal keeps verified stage bytes
//! for reuse. Its absence can never grant another slot.

use super::{hash, Hash, MAX_ENCODED_BYTES};
use plurx_core::fs_secure::{open_directory_nofollow_blocking, std_file_identity};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
};

const JOURNAL_REVISION: u32 = 1;
const MAX_RECORD: u64 = 32 * 1024;
const MAX_PROTOCOL: u64 = 1024 * 1024;
const MAX_TERMINAL_ARTIFACT: u64 = 64 * 1024 * 1024;
const STEP: usize = 128 * 1024;
const GATES: &str = "gates.json";
const AUTHORED_MEDIA: &str = "authored.mp4";
const AUTHORED: &str = "authored.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub source: Hash,
    pub probe: Hash,
    pub protocol: Hash,
    pub recipe: Hash,
    pub package: Hash,
    pub bundle: Hash,
    pub muxer: Hash,
    pub ffmpeg: Hash,
    pub source_generation: Hash,
    pub source_dv_level: u8,
    pub maximum_attempts: u8,
}

impl Binding {
    pub fn identity(&self) -> Hash {
        hash(&serde_json::to_vec(self).expect("fixed M7 binding serializes"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    revision: u32,
    pub binding: Hash,
    pub gates: Hash,
    pub closure: Hash,
    pub slot: u8,
    pub target_kbps: u32,
}

impl Intent {
    pub fn new(binding: Binding, gates: Hash, closure: Hash, slot: u8, target_kbps: u32) -> Self {
        Self {
            revision: JOURNAL_REVISION,
            binding: binding.identity(),
            gates,
            closure,
            slot,
            target_kbps,
        }
    }

    pub fn identity(self) -> Hash {
        hash(&serde_json::to_vec(&self).expect("fixed M7 intent serializes"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GateProof {
    pub binding: Hash,
    pub closure: Hash,
    pub baseline_receipt: Hash,
    pub none_receipt: Hash,
    pub zero_receipt: Hash,
    pub elementary: Hash,
    pub trace_configuration: String,
}

impl GateProof {
    pub fn identity(&self) -> Hash {
        hash(&serde_json::to_vec(self).expect("fixed M7 gates serialize"))
    }

    fn matches(&self, binding: Binding) -> bool {
        self.binding == binding.identity()
            && [
                self.closure,
                self.baseline_receipt,
                self.none_receipt,
                self.zero_receipt,
                self.elementary,
            ]
            .into_iter()
            .all(|digest| digest != [0; 32])
            && sha_text(&self.trace_configuration)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EncodeProof {
    pub intent: Intent,
    pub extent: u64,
    pub mp4_sha: Hash,
    pub encoded_payload_bytes: u64,
    pub receipt_sha: Hash,
    pub packet_census: Hash,
    pub trace_configuration: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthoredProof {
    pub intent: Intent,
    pub encoded_mp4_sha: Hash,
    pub authored_mp4_sha: Hash,
    pub extent: u64,
    pub authored_payload_bytes: u64,
    pub packet_census: Hash,
    pub receipt_sha: Hash,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("M7 candidate journal is in use")]
    Busy,
    #[error("M7 candidate journal I/O: {0}")]
    Io(#[from] io::Error),
    #[error("M7 candidate journal binding differs: {0}")]
    Binding(&'static str),
    #[error("M7 candidate slot {0} was consumed without a recoverable verified result")]
    Consumed(u8),
}

pub(super) struct RecoveredEncode {
    pub proof: EncodeProof,
    pub bytes: Vec<u8>,
}

/// Owns the source-level lock and the exact protocol-relative candidate dir.
pub(super) struct Journal {
    experiment: File,
    directory: File,
    _lock: File,
}

impl Journal {
    pub fn open(path: &Path, source: Hash, protocol: Hash) -> Result<Self, Error> {
        let source_path = path
            .parent()
            .ok_or(Error::Binding("source experiment parent"))?;
        if path.file_name().and_then(|n| n.to_str()) != Some(&hex::encode(protocol))
            || source_path.file_name().and_then(|n| n.to_str()) != Some(&hex::encode(source))
        {
            return Err(Error::Binding("source and immutable protocol path"));
        }
        let source = open_directory_nofollow_blocking(source_path)?;
        let experiment = open_directory_nofollow_blocking(path)?;
        let protocol_raw = read(&experiment, "native-protocol.json", MAX_PROTOCOL)?
            .ok_or(Error::Binding("immutable native protocol"))?;
        if hash(&protocol_raw) != protocol {
            return Err(Error::Binding("verified native protocol hash"));
        }

        let lock = open_at(&source, "np1-candidate.lock", libc::O_RDWR | libc::O_CREAT)?;
        if !lock.metadata()?.is_file() || lock.metadata()?.len() != 0 {
            return Err(Error::Binding("held source journal lock"));
        }
        // SAFETY: The source authority owns this open file through Journal's
        // lifetime. A nonblocking lock never queues a title with a dead claim.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return Err(if error.kind() == io::ErrorKind::WouldBlock {
                Error::Busy
            } else {
                error.into()
            });
        }
        let directory = child_directory(&experiment, "candidate", true)?;
        Ok(Self {
            experiment,
            directory,
            _lock: lock,
        })
    }

    pub fn gate(&self, binding: Binding) -> Result<Option<GateProof>, Error> {
        let proof: Option<GateProof> =
            record(&self.directory, GATES).map_err(|e| match consumed(e, 0) {
                Error::Consumed(_) => Error::Binding("valid encoder gate record"),
                other => other,
            })?;
        if proof.as_ref().is_some_and(|proof| !proof.matches(binding)) {
            return Err(Error::Binding("exact independently verified encoder gates"));
        }
        Ok(proof)
    }

    pub fn save_gate(
        &self,
        binding: Binding,
        proof: &GateProof,
        active: impl Fn() -> bool,
    ) -> Result<(), Error> {
        if !proof.matches(binding) {
            return Err(Error::Binding("verified encoder gate record"));
        }
        save_record(&self.directory, GATES, proof, &active)
    }

    /// Check durable media capacity before consuming the replicated slot.
    pub fn admit_bytes(&self) -> Result<(), Error> {
        let mut facts = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: fstatvfs initializes this object on success.
        if unsafe { libc::fstatvfs(self.directory.as_raw_fd(), facts.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: previous call succeeded.
        let facts = unsafe { facts.assume_init() };
        let available = u128::from(facts.f_bavail) * u128::from(facts.f_frsize);
        if available < u128::from(2 * MAX_ENCODED_BYTES + MAX_RECORD) {
            return Err(Error::Busy);
        }
        Ok(())
    }

    /// Called only after this job fenced a new global reservation.
    pub fn begin(&self, intent: Intent, active: impl Fn() -> bool) -> Result<(), Error> {
        slot_number(intent.slot)?;
        let name = intent_name(intent.slot);
        if record::<Intent>(&self.directory, &name)?.is_some() {
            return Err(Error::Consumed(intent.slot));
        }
        save_record(&self.directory, &name, &intent, &active)
    }

    pub fn recover(&self, intent: Intent) -> Result<RecoveredEncode, Error> {
        slot_number(intent.slot)?;
        self.drop_partial(&intent_name(intent.slot))?;
        let expected = self.read_consumed::<Intent>(&intent_name(intent.slot), intent.slot)?;
        if expected != intent {
            return Err(Error::Consumed(intent.slot));
        }
        let name = proof_name(intent.slot);
        let Some(proof) = record::<EncodeProof>(&self.directory, &name)
            .map_err(|error| consumed(error, intent.slot))?
        else {
            self.drop_unverified(&media_name(intent.slot))?;
            self.drop_partial(&name)?;
            return Err(Error::Consumed(intent.slot));
        };
        let bytes = read(&self.directory, &media_name(intent.slot), MAX_ENCODED_BYTES)
            .map_err(|error| consumed(error, intent.slot))?
            .ok_or(Error::Consumed(intent.slot))?;
        if proof.intent != intent
            || proof.extent != bytes.len() as u64
            || proof.mp4_sha != hash(&bytes)
            || !(1..=proof.extent).contains(&proof.encoded_payload_bytes)
            || proof.receipt_sha == [0; 32]
            || proof.packet_census == [0; 32]
            || !sha_text(&proof.trace_configuration)
        {
            return Err(Error::Consumed(intent.slot));
        }
        self.drop_partial(&media_name(intent.slot))?;
        self.drop_partial(&name)?;
        Ok(RecoveredEncode { proof, bytes })
    }

    pub fn seal_encoded(
        &self,
        proof: &EncodeProof,
        bytes: &[u8],
        active: impl Fn() -> bool,
    ) -> Result<(), Error> {
        let slot = proof.intent.slot;
        let intent = self.read_consumed::<Intent>(&intent_name(slot), slot)?;
        if intent != proof.intent
            || bytes.len() as u64 != proof.extent
            || proof.extent == 0
            || proof.extent > MAX_ENCODED_BYTES
            || hash(bytes) != proof.mp4_sha
        {
            return Err(Error::Binding("verified encoded stage"));
        }
        save(&self.directory, &media_name(slot), bytes, &active)?;
        save_record(&self.directory, &proof_name(slot), proof, &active)
    }

    pub fn authored(
        &self,
        intent: Intent,
        encoded_sha: Hash,
    ) -> Result<Option<(AuthoredProof, Vec<u8>)>, Error> {
        let Some(proof) = record::<AuthoredProof>(&self.directory, AUTHORED)
            .map_err(|e| consumed(e, intent.slot))?
        else {
            self.drop_unverified(AUTHORED_MEDIA)?;
            self.drop_partial(AUTHORED)?;
            return Ok(None);
        };
        let bytes = read(&self.directory, AUTHORED_MEDIA, MAX_ENCODED_BYTES)
            .map_err(|e| consumed(e, intent.slot))?
            .ok_or(Error::Consumed(intent.slot))?;
        if proof.intent != intent
            || proof.encoded_mp4_sha != encoded_sha
            || proof.extent != bytes.len() as u64
            || proof.authored_mp4_sha != hash(&bytes)
            || !(1..=proof.extent).contains(&proof.authored_payload_bytes)
            || proof.receipt_sha == [0; 32]
            || proof.packet_census == [0; 32]
        {
            return Err(Error::Consumed(intent.slot));
        }
        self.drop_partial(AUTHORED_MEDIA)?;
        self.drop_partial(AUTHORED)?;
        Ok(Some((proof, bytes)))
    }

    pub fn seal_authored(
        &self,
        proof: &AuthoredProof,
        bytes: &[u8],
        active: impl Fn() -> bool,
    ) -> Result<(), Error> {
        let encoded =
            self.read_consumed::<EncodeProof>(&proof_name(proof.intent.slot), proof.intent.slot)?;
        if encoded.intent != proof.intent
            || encoded.mp4_sha != proof.encoded_mp4_sha
            || bytes.len() as u64 != proof.extent
            || proof.extent == 0
            || proof.extent > MAX_ENCODED_BYTES
            || hash(bytes) != proof.authored_mp4_sha
        {
            return Err(Error::Binding("verified authored stage"));
        }
        save(&self.directory, AUTHORED_MEDIA, bytes, &active)?;
        save_record(&self.directory, AUTHORED, proof, &active)
    }

    fn read_consumed<T: DeserializeOwned>(&self, name: &str, slot: u8) -> Result<T, Error> {
        record(&self.directory, name)
            .map_err(|e| consumed(e, slot))?
            .ok_or(Error::Consumed(slot))
    }

    fn drop_unverified(&self, name: &str) -> Result<(), Error> {
        self.drop_partial(name)?;
        remove(&self.directory, name)
    }

    fn drop_partial(&self, name: &str) -> Result<(), Error> {
        remove(&self.directory, &part_name(name))
    }

    /// Reclaim bulky coded copies after the independently validated native
    /// terminal is durable. Intents, proofs and the replicated ledger stay.
    pub fn reclaim(&self, source: Hash, protocol: Hash) -> Result<(), Error> {
        let completion = child_directory(&self.experiment, "completion", false)?;
        let terminal: serde_json::Value = record(&completion, "terminal.json")?
            .ok_or(Error::Binding("durable comparison completion"))?;
        if terminal["source_sha256"].as_str() != Some(&hex::encode(source))
            || terminal["protocol_sha256"].as_str() != Some(&hex::encode(protocol))
        {
            return Err(Error::Binding("terminal comparison source and protocol"));
        }
        for (key, name) in [
            ("report_sha256", "comparison-result.json"),
            ("binding_sha256", "measurement-binding.json"),
        ] {
            let expected = terminal[key]
                .as_str()
                .ok_or(Error::Binding("terminal artifact digest"))?;
            let bytes = read(&completion, name, MAX_TERMINAL_ARTIFACT)?
                .ok_or(Error::Binding("terminal comparison artifact"))?;
            if hex::encode(hash(&bytes)) != expected {
                return Err(Error::Binding("terminal comparison artifact identity"));
            }
        }
        for slot in 0..4 {
            if record::<Intent>(&self.directory, &intent_name(slot))?.is_some() {
                self.drop_unverified(&media_name(slot))?;
            }
        }
        self.drop_unverified(AUTHORED_MEDIA)
    }
}

fn sha_text(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn slot_number(slot: u8) -> Result<(), Error> {
    if slot < 4 {
        Ok(())
    } else {
        Err(Error::Binding("bounded attempt slot"))
    }
}

fn intent_name(slot: u8) -> String {
    format!("attempt-{slot}.intent.json")
}
fn proof_name(slot: u8) -> String {
    format!("attempt-{slot}.encoded.json")
}
fn media_name(slot: u8) -> String {
    format!("attempt-{slot}.mp4")
}
fn part_name(name: &str) -> String {
    format!(".{name}.part")
}

fn consumed(error: Error, slot: u8) -> Error {
    match error {
        Error::Binding(_) => Error::Consumed(slot),
        Error::Io(ref e)
            if matches!(
                e.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::NotFound
            ) || e.raw_os_error() == Some(libc::ELOOP) =>
        {
            Error::Consumed(slot)
        }
        other => other,
    }
}

fn child_directory(parent: &File, name: &str, create: bool) -> Result<File, Error> {
    let child = cname(name)?;
    if create {
        // SAFETY: fixed name beneath held private experiment capability.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), child.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        parent.sync_all()?;
    }
    open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY).map_err(Into::into)
}

fn cname(name: &str) -> io::Result<CString> {
    if name.is_empty() || name.len() > 80 || name.contains('/') || matches!(name, "." | "..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid journal child name",
        ));
    }
    CString::new(name).map_err(io::Error::other)
}

fn open_at(dir: &File, name: &str, flags: i32) -> io::Result<File> {
    let name = cname(name)?;
    // SAFETY: name is one relative component and dir stays held across
    // openat. O_NOFOLLOW and O_NONBLOCK also cover replaced special files.
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat transfers one valid fd.
    Ok(unsafe { File::from_raw_fd(raw) })
}

fn read(dir: &File, name: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
    let file = match open_at(dir, name, libc::O_RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = std_file_identity(&file)?;
    if !file.metadata()?.is_file() || !(1..=limit).contains(&before.size) {
        return Err(Error::Binding("bounded regular journal child"));
    }
    let mut bytes = Vec::with_capacity(before.size as usize);
    (&file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != before.size || std_file_identity(&file)? != before {
        return Err(Error::Binding("journal child changed during read"));
    }
    Ok(Some(bytes))
}

fn record<T: DeserializeOwned>(dir: &File, name: &str) -> Result<Option<T>, Error> {
    read(dir, name, MAX_RECORD)?
        .map(|raw| {
            serde_json::from_slice(&raw)
                .map_err(|e| Error::Io(io::Error::new(io::ErrorKind::InvalidData, e)))
        })
        .transpose()
}

fn save_record<T: Serialize>(
    dir: &File,
    name: &str,
    record: &T,
    active: &impl Fn() -> bool,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err(Error::Binding("bounded local record"));
    }
    save(dir, name, &bytes, active)
}

fn save(dir: &File, name: &str, bytes: &[u8], active: &impl Fn() -> bool) -> Result<(), Error> {
    if !active() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "candidate custody ended").into());
    }
    let staged_name = part_name(name);
    remove(dir, &staged_name)?;
    let mut staged = open_at(
        dir,
        &staged_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    let result = (|| {
        for chunk in bytes.chunks(STEP) {
            if !active() {
                return Err(
                    io::Error::new(io::ErrorKind::Interrupted, "candidate custody ended").into(),
                );
            }
            staged.write_all(chunk)?;
        }
        staged.sync_all()?;
        if !active() {
            return Err(
                io::Error::new(io::ErrorKind::Interrupted, "candidate custody ended").into(),
            );
        }
        let from = cname(&staged_name)?;
        let to = cname(name)?;
        // SAFETY: both names are held capability-relative and linkat has
        // no replacement semantics. Publish only the fully synced inode.
        if unsafe {
            libc::linkat(
                dir.as_raw_fd(),
                from.as_ptr(),
                dir.as_raw_fd(),
                to.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        dir.sync_all()?;
        Ok(())
    })();
    let cleanup = remove(dir, &staged_name);
    result.and(cleanup)
}

fn remove(dir: &File, name: &str) -> Result<(), Error> {
    match open_at(dir, name, libc::O_RDONLY) {
        Ok(file) if file.metadata()?.is_file() => {}
        Ok(_) => return Err(Error::Binding("cleanup refuses non-regular journal child")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let name = cname(name)?;
    // SAFETY: known journal artifact child beneath the still-held directory.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    dir.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, fs, path::PathBuf};

    struct Experiment {
        _home: tempfile::TempDir,
        path: PathBuf,
        binding: Binding,
    }

    impl Experiment {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let protocol = b"frozen test protocol";
            let binding = Binding {
                source: [1; 32],
                probe: [2; 32],
                protocol: hash(protocol),
                recipe: [4; 32],
                package: [5; 32],
                bundle: [6; 32],
                muxer: [7; 32],
                ffmpeg: [8; 32],
                source_generation: [9; 32],
                source_dv_level: 6,
                maximum_attempts: 3,
            };
            let path = home
                .path()
                .canonicalize()
                .unwrap()
                .join(hex::encode(binding.source))
                .join(hex::encode(binding.protocol));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("native-protocol.json"), protocol).unwrap();
            Self {
                _home: home,
                path,
                binding,
            }
        }

        fn journal(&self) -> Journal {
            Journal::open(&self.path, self.binding.source, self.binding.protocol).unwrap()
        }

        fn intent(&self, slot: u8) -> Intent {
            Intent::new(self.binding, [11; 32], [12; 32], slot, 115_000)
        }
    }

    fn encode_proof(intent: Intent, bytes: &[u8]) -> EncodeProof {
        EncodeProof {
            intent,
            extent: bytes.len() as u64,
            mp4_sha: hash(bytes),
            encoded_payload_bytes: 1,
            receipt_sha: [13; 32],
            packet_census: [14; 32],
            trace_configuration: "a".repeat(64),
        }
    }

    #[test]
    fn interrupted_before_local_intent_creates_no_file_or_encoder_authority() {
        let root = Experiment::new();
        let journal = root.journal();
        let intent = root.intent(0);
        let active_checks = Cell::new(0);
        assert!(matches!(
            journal.begin(intent, || {
                active_checks.set(active_checks.get() + 1);
                active_checks.get() == 1
            }),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::Interrupted
        ));
        assert!(record::<Intent>(&journal.directory, &intent_name(0))
            .unwrap()
            .is_none());
        assert!(!root
            .path
            .join("candidate")
            .join(part_name(&intent_name(0)))
            .exists());
        // Only a subsequent committed global reservation may issue a begin.
        journal.begin(intent, || true).unwrap();
        assert!(matches!(
            journal.begin(intent, || true),
            Err(Error::Consumed(0))
        ));
    }

    #[test]
    fn crash_after_launch_intent_never_reopens_slot_and_discards_unsealed_media() {
        let root = Experiment::new();
        let intent = root.intent(0);
        {
            let journal = root.journal();
            journal.begin(intent, || true).unwrap();
            let media = media_name(0);
            save(
                &journal.directory,
                &media,
                b"unfinished encoded bytes",
                &|| true,
            )
            .unwrap();
            fs::write(
                root.path.join("candidate").join(part_name(&proof_name(0))),
                b"torn proof",
            )
            .unwrap();
        }
        let journal = root.journal();
        assert!(matches!(journal.recover(intent), Err(Error::Consumed(0))));
        assert!(matches!(
            journal.begin(intent, || true),
            Err(Error::Consumed(0))
        ));
        assert!(!root.path.join("candidate").join(media_name(0)).exists());
        assert!(!root
            .path
            .join("candidate")
            .join(part_name(&proof_name(0)))
            .exists());
        let changed = Intent::new(root.binding, [99; 32], [12; 32], 0, 115_000);
        assert!(matches!(journal.recover(changed), Err(Error::Consumed(0))));
    }

    #[test]
    fn completed_slot_and_authored_bytes_are_reused_after_owner_drops() {
        let root = Experiment::new();
        let intent = root.intent(0);
        let encoded = b"encoded clip fixture";
        let authored = b"authored clip fixture";
        let proof = encode_proof(intent, encoded);
        let authored_proof = AuthoredProof {
            intent,
            encoded_mp4_sha: hash(encoded),
            authored_mp4_sha: hash(authored),
            extent: authored.len() as u64,
            authored_payload_bytes: 1,
            packet_census: [21; 32],
            receipt_sha: [22; 32],
        };
        {
            let journal = root.journal();
            assert!(matches!(
                Journal::open(&root.path, root.binding.source, root.binding.protocol),
                Err(Error::Busy)
            ));
            journal.begin(intent, || true).unwrap();
            journal.seal_encoded(&proof, encoded, || true).unwrap();
            journal
                .seal_authored(&authored_proof, authored, || true)
                .unwrap();
        }
        let journal = root.journal();
        let recovered = journal.recover(intent).unwrap();
        assert_eq!(recovered.proof, proof);
        assert_eq!(recovered.bytes, encoded);
        assert_eq!(
            journal.authored(intent, proof.mp4_sha).unwrap(),
            Some((authored_proof, authored.to_vec()))
        );
        assert!(matches!(
            journal.begin(root.intent(4), || true),
            Err(Error::Binding("bounded attempt slot"))
        ));
    }

    #[test]
    fn replay_refuses_bytes_replaced_after_completion() {
        let root = Experiment::new();
        let journal = root.journal();
        let intent = root.intent(0);
        let original = b"verified bytes";
        journal.begin(intent, || true).unwrap();
        journal
            .seal_encoded(&encode_proof(intent, original), original, || true)
            .unwrap();
        fs::write(
            root.path.join("candidate").join(media_name(0)),
            b"changed bytes!",
        )
        .unwrap();
        assert!(matches!(journal.recover(intent), Err(Error::Consumed(0))));
        assert!(matches!(
            journal.begin(intent, || true),
            Err(Error::Consumed(0))
        ));
    }

    #[test]
    fn terminal_reclaim_keeps_history_and_requires_the_exact_report() {
        let root = Experiment::new();
        let journal = root.journal();
        let intent = root.intent(0);
        let encoded = b"encoded fixture";
        journal.begin(intent, || true).unwrap();
        journal
            .seal_encoded(&encode_proof(intent, encoded), encoded, || true)
            .unwrap();
        let completion = root.path.join("completion");
        fs::create_dir(&completion).unwrap();
        fs::write(completion.join("comparison-result.json"), b"report").unwrap();
        fs::write(completion.join("measurement-binding.json"), b"binding").unwrap();
        let terminal = serde_json::json!({
            "source_sha256": hex::encode(root.binding.source),
            "protocol_sha256": hex::encode(root.binding.protocol),
            "report_sha256": hex::encode(hash(b"report")),
            "binding_sha256": hex::encode(hash(b"binding")),
        });
        fs::write(
            completion.join("terminal.json"),
            serde_json::to_vec(&terminal).unwrap(),
        )
        .unwrap();
        fs::write(completion.join("comparison-result.json"), b"wrong").unwrap();
        assert!(journal
            .reclaim(root.binding.source, root.binding.protocol)
            .is_err());
        assert!(journal.recover(intent).is_ok());
        fs::write(completion.join("comparison-result.json"), b"report").unwrap();
        journal
            .reclaim(root.binding.source, root.binding.protocol)
            .unwrap();
        assert!(matches!(journal.recover(intent), Err(Error::Consumed(0))));
        assert!(record::<EncodeProof>(&journal.directory, &proof_name(0))
            .unwrap()
            .is_some());
        assert!(record::<Intent>(&journal.directory, &intent_name(0))
            .unwrap()
            .is_some());
    }
}
