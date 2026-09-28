//! The subtitle-source store: PGS tracks kept on this node, so a start does
//! not have to read a whole remux to learn what its subtitles say.
//!
//! Design: `docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md` §6. Every
//! PGS track is otherwise read twice more, in full, by two consumers that each
//! demux the whole source — `pgs_overlay::prepare_stage` for the overlay's
//! `.sup`, and `subtitles::ensure_burn_file` for the burn sidecar — on a 79.5
//! GB remux that is 402 s each to produce 18,866 bytes. The fragment-index
//! pass already reads every one of those packets; the producer,
//! [`crate::subtitle_ride_along`], keeps them here, and this module is
//! everything that reads them.
//!
//! A file whose index pass has not ridden on this node has no directory here,
//! so every lookup for it misses and both consumers fall through to the
//! extraction they have always run.
//!
//! The rules it keeps:
//!
//! - **A miss is never an error.** A missing directory, a manifest this build
//!   cannot read, a `.sup` swept or republished between the manifest read and
//!   the open, or a `.sup` whose bytes no longer match the manifest's sha256 —
//!   each is a miss that falls through to today's extraction.
//! - **Validity is checked at use, against a live `fstat`.** The overlay keeps
//!   its size+mtime rule. The burn path additionally requires `(dev, ino)` to
//!   match the file it has open, which rejects a file replaced in place with a
//!   new inode without using ctime: a hardlink or `chmod` from an importer
//!   moves ctime, and would otherwise make the burn path miss on that file for
//!   good while nothing ever re-rides the pass.
//! - **Readers never write into the store**, apart from the best-effort
//!   `.access` marker, so they cannot race the producer.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::MediaFile;
use plurx_core::store::SubtitleSourcePublication;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::peer_transport::{PeerAuthMode, PeerTransport};

pub(crate) use crate::fragment_index_cluster::SourceStamp;

/// The store's home under `<cache>/runtime`. The version is in the name so a
/// future layout can live beside this one rather than having to read it.
pub(crate) const STORE_DIR: &str = "subtitle-source-v1";
/// The manifest format this build reads and would write.
pub(crate) const MANIFEST_VERSION: u32 = 1;
pub(crate) const MANIFEST_NAME: &str = "manifest.json";
/// A manifest is a few hundred bytes per track; anything near this is not one.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// The largest stored track a reader will open, matching the overlay's own
/// per-track cap. The producer refuses to keep anything larger.
pub(crate) const MAX_TRACK_BYTES: u64 = 256 * 1024 * 1024;
/// A `transient` track stops holding the latch open after this many passes.
pub(crate) const TRANSIENT_ATTEMPTS: u32 = 3;
/// Hex characters of the sha256 carried in a stored track's file name.
const SHA_PREFIX_CHARS: usize = 16;
/// A safety rail on the store's total size, not a working budget: a real PGS
/// track is single-digit megabytes, so this is set well above any real
/// library's footprint and exists so a bad producer cannot fill the disk.
pub(crate) const MAX_STORE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Directories examined per sweep call.
pub(crate) const SWEEP_PAGE: usize = 256;
/// How long a directory with no readable manifest is left alone before the
/// sweep treats it as abandoned. Generous, because the only writer that can
/// leave one behind is a publish interrupted between its steps.
const ABANDONED_GRACE: Duration = Duration::from_secs(60 * 60);

/// The store root for a node's runtime cache — the same `<cache>/runtime` the
/// fragment-index blobs live under (`fragment_index_cluster::cache_root`).
pub(crate) fn store_root(runtime_cache: &Path) -> PathBuf {
    runtime_cache.join(STORE_DIR)
}

/// One file's directory. Node-local, keyed by the file row's id.
pub(crate) fn file_dir(root: &Path, file_id: i64) -> PathBuf {
    root.join(format!("f{file_id}"))
}

/// A fixed, bounded table of locks shared by producers, hydrators and the
/// sweeper. Hash collisions only serialize unrelated files; they cannot let
/// two writers of one file swap its manifest concurrently.
pub(crate) fn file_lock(root: &Path, file_id: i64) -> &'static tokio::sync::Mutex<()> {
    const LOCKS: usize = 256;
    static TABLE: std::sync::OnceLock<Vec<tokio::sync::Mutex<()>>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| (0..LOCKS).map(|_| tokio::sync::Mutex::new(())).collect());
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hash);
    file_id.hash(&mut hash);
    &table[(hash.finish() as usize) % LOCKS]
}

/// Merge a pass or one hydrated representation into the manifest currently
/// on disk. Only incoming `(ordinal, format)` pairs replace old pairs.
/// Caller holds `file_lock`; a different local inode discards all old rows.
pub(crate) fn merge_manifest(mut incoming: Manifest, previous: Option<&Manifest>) -> Manifest {
    let Some(previous) = previous.filter(|old| {
        old.version == MANIFEST_VERSION
            && old.file_id == incoming.file_id
            && old.source == incoming.source
    }) else {
        return incoming;
    };
    for ordinal in &previous.ordinals {
        if !incoming.ordinals.contains(ordinal) {
            incoming.ordinals.push(*ordinal);
        }
    }
    for old_track in &previous.tracks {
        if let Some(new_track) = incoming
            .tracks
            .iter_mut()
            .find(|track| track.ordinal == old_track.ordinal)
        {
            let old_reps = if old_track.representations.is_empty() {
                old_track
                    .representation(RepresentationFormat::Sup)
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                old_track.representations.clone()
            };
            for representation in old_reps {
                if !new_track
                    .representations
                    .iter()
                    .any(|new| new.format == representation.format)
                {
                    new_track.representations.push(representation);
                }
            }
            if let Some(primary) = new_track.representations.first() {
                new_track.verdict = primary.verdict;
                new_track.attempts = primary.attempts;
                new_track.file = primary.file.clone();
                new_track.sha256 = primary.sha256.clone();
            }
        } else {
            incoming.tracks.push(old_track.clone());
        }
    }
    incoming.ordinals.sort_unstable();
    incoming.ordinals.dedup();
    incoming.tracks.sort_by_key(|track| track.ordinal);
    incoming
}

/// The content-named file a kept track is stored under:
/// `s<ordinal>-<sha256 prefix>.sup`. Content naming means no field has to be
/// parsed back out of a name, and a republish never overwrites bytes a reader
/// may have open.
#[cfg(test)]
pub(crate) fn sup_file_name(ordinal: i64, sha256: &str) -> Option<String> {
    (ordinal >= 0 && is_sha256(sha256))
        .then(|| format!("s{ordinal}-{}.sup", &sha256[..SHA_PREFIX_CHARS]))
}

/// Content-named artefact for one representation. The legacy `.sup` spelling
/// is deliberately retained so manifests written before text support remain
/// readable without moving their files.
pub(crate) fn representation_file_name(
    ordinal: i64,
    format: RepresentationFormat,
    sha256: &str,
) -> Option<String> {
    (ordinal >= 0 && is_sha256(sha256)).then(|| {
        format!(
            "s{ordinal}-{}.{}",
            &sha256[..SHA_PREFIX_CHARS],
            format.extension()
        )
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// What one pass concluded about one track.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// The track's `.sup` is complete and stored.
    Kept,
    /// A real track with no cues at all.
    Empty,
    /// The pass saw the track and it was not intact. Final for this source.
    Malformed,
    /// The pass could not tell — an OS error on a slave, or no evidence at
    /// all. Retried on a later pass until [`TRANSIENT_ATTEMPTS`].
    Transient,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrackKind {
    #[default]
    Pgs,
    Text,
    TextStyled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RepresentationFormat {
    Sup,
    Webvtt,
    Matroska,
}

impl RepresentationFormat {
    pub(crate) fn publication_name(self) -> &'static str {
        match self {
            Self::Sup => "sup",
            Self::Webvtt => "webvtt",
            Self::Matroska => "matroska",
        }
    }

    pub(crate) fn from_publication_name(name: &str) -> Option<Self> {
        match name {
            "sup" => Some(Self::Sup),
            "webvtt" => Some(Self::Webvtt),
            "matroska" => Some(Self::Matroska),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RepresentationOrigin {
    #[default]
    Extracted,
    Hydrated,
}

impl RepresentationFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Sup => "sup",
            Self::Webvtt => "vtt",
            Self::Matroska => "mks",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RepresentationEntry {
    pub(crate) format: RepresentationFormat,
    #[serde(default)]
    pub(crate) origin: RepresentationOrigin,
    pub(crate) verdict: Verdict,
    pub(crate) attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,
    #[serde(default)]
    pub(crate) bytes: u64,
}

impl RepresentationEntry {
    pub(crate) fn settled(&self) -> bool {
        settled_verdict(self.verdict, self.attempts)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrackEntry {
    /// The track's subtitle ordinal — the `N` in `-map 0:s:N`, the same index
    /// both consumers are asked for.
    pub(crate) ordinal: i64,
    #[serde(default)]
    pub(crate) kind: TrackKind,
    #[serde(default)]
    pub(crate) representations: Vec<RepresentationEntry>,
    pub(crate) verdict: Verdict,
    /// Passes that have tried this track for this source identity.
    pub(crate) attempts: u32,
    /// The stored file's name, for a `kept` track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file: Option<String>,
    /// The stored file's full sha256, for a `kept` track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) version: u32,
    pub(crate) file_id: i64,
    /// The source identity the pass read: size and mtime for everyone,
    /// `(dev, ino)` for the burn path.
    pub(crate) source: SourceStamp,
    /// The PGS subtitle ordinals the held-fd probe found on the file.
    pub(crate) ordinals: Vec<i64>,
    pub(crate) tracks: Vec<TrackEntry>,
}

/// Only the version, read first, so a manifest from a future layout is a miss
/// by its version rather than by whatever field it renamed.
#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

/// Which identity a consumer holds the store to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consumer {
    /// `pgs_overlay::prepare_stage`: size + mtime, as its own generation key.
    Overlay,
    /// `subtitles::ensure_burn_file`: size + mtime + `(dev, ino)` of the file
    /// the session holds open. Never ctime.
    Burn,
    /// The VTT-keyed whole-track sidecar, with the overlay's live stamp rule.
    Vtt,
}

impl Consumer {
    const ALL: [Self; 3] = [Self::Overlay, Self::Burn, Self::Vtt];

    fn label(self) -> &'static str {
        match self {
            Self::Overlay => "overlay",
            Self::Burn => "burn",
            Self::Vtt => "vtt",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

impl TrackEntry {
    /// Nothing more to do for this track: `kept`, `empty` or `malformed`, or
    /// `transient` with its attempts used.
    pub(crate) fn settled(&self) -> bool {
        if self.representations.is_empty() {
            settled_verdict(self.verdict, self.attempts)
        } else {
            self.representations
                .iter()
                .all(RepresentationEntry::settled)
        }
    }

    pub(crate) fn representation(
        &self,
        format: RepresentationFormat,
    ) -> Option<RepresentationEntry> {
        if let Some(representation) = self
            .representations
            .iter()
            .find(|entry| entry.format == format)
        {
            return Some(representation.clone());
        }
        (self.kind == TrackKind::Pgs
            && self.representations.is_empty()
            && format == RepresentationFormat::Sup)
            .then(|| RepresentationEntry {
                format,
                origin: RepresentationOrigin::Extracted,
                verdict: self.verdict,
                attempts: self.attempts,
                file: self.file.clone(),
                sha256: self.sha256.clone(),
                bytes: 0,
            })
    }
}

fn settled_verdict(verdict: Verdict, attempts: u32) -> bool {
    match verdict {
        Verdict::Kept | Verdict::Empty | Verdict::Malformed => true,
        Verdict::Transient => attempts >= TRANSIENT_ATTEMPTS,
    }
}

/// Coverage is per eligible ordinal and comes only from a finished source
/// extraction. A hydrated row can serve peers but cannot suppress a missing
/// whole-track read for another ordinal.
pub(crate) fn extracted_ordinal_covered(rows: &[SubtitleSourcePublication], ordinal: i64) -> bool {
    rows.iter().any(|row| {
        row.ordinal == ordinal
            && row.origin == "extracted"
            && match row.verdict.as_str() {
                "kept" | "empty" | "malformed" => true,
                "transient" => row.attempts >= i64::from(TRANSIENT_ATTEMPTS),
                _ => false,
            }
    })
}

impl Manifest {
    pub(crate) fn track(&self, ordinal: i64) -> Option<&TrackEntry> {
        self.tracks.iter().find(|track| track.ordinal == ordinal)
    }

    /// Whether this manifest's source is the file `live` describes, by the
    /// rule `consumer` holds it to.
    pub(crate) fn source_matches(&self, live: &SourceStamp, consumer: Consumer) -> bool {
        if self.source.size != live.size || self.source.mtime != live.mtime {
            return false;
        }
        match consumer {
            Consumer::Overlay | Consumer::Vtt => true,
            // A platform with no `(dev, ino)` (the Windows port) falls back to
            // size + mtime. Where the live file has one, the manifest must
            // carry the same one: a manifest without it cannot vouch for the
            // inode, and the burn path does not guess.
            Consumer::Burn => match (live.dev, live.ino) {
                (Some(dev), Some(ino)) => {
                    self.source.dev == Some(dev) && self.source.ino == Some(ino)
                }
                _ => true,
            },
        }
    }

    /// Every probed track has a settled verdict: `kept`, `empty` or
    /// `malformed`, or `transient` that has used its attempts.
    #[cfg(test)]
    pub(crate) fn latched(&self) -> bool {
        self.ordinals
            .iter()
            .all(|ordinal| self.track(*ordinal).is_some_and(TrackEntry::settled))
    }

    /// The design's *current*: this build's version, this file, a source
    /// that matches a live `fstat` by the consumer's rule, and a settled
    /// verdict for every probed track.
    #[cfg(test)]
    pub(crate) fn is_current(&self, file_id: i64, live: &SourceStamp, consumer: Consumer) -> bool {
        self.version == MANIFEST_VERSION
            && self.file_id == file_id
            && self.source_matches(live, consumer)
            && self.latched()
    }
}

/// Parse a manifest, or say why it is not one this build can use.
pub(crate) fn parse_manifest(bytes: &[u8]) -> Result<Manifest, MissReason> {
    let version: VersionOnly = serde_json::from_slice(bytes).map_err(|_| MissReason::Stale)?;
    if version.version != MANIFEST_VERSION {
        return Err(MissReason::Stale);
    }
    serde_json::from_slice(bytes).map_err(|_| MissReason::Stale)
}

/// Whether the consumers may read the store, and where it is.
///
/// The consumer half of the off switch: with `enabled` false both consumers
/// ignore the store entirely, so a wrong artifact a bad producer published is
/// out of service with one setting and no redeploy.
///
/// The switch is read **lazily**, by a lookup, and only once that lookup is
/// actually going to consult the store. A warm overlay generation or a warm
/// burn sidecar is served without the setting ever being read: the store has
/// nothing to save on those paths, so they pay nothing for it.
#[derive(Clone)]
pub(crate) struct StoreAccess {
    root: PathBuf,
    switch: Switch,
    /// This node's id, when the caller knows it: a miss with no directory is
    /// then classified (`never_indexed`, `hydrated_only`, `absent`) off the
    /// request path. Without it such a miss is `absent`.
    node_id: Option<String>,
    membership: Option<MembershipManager>,
    jobs: Option<std::sync::Arc<crate::state::JobManager>>,
}

#[derive(Clone)]
enum Switch {
    #[cfg(test)]
    Fixed(bool),
    /// `subtitles.stored_sources`, read when a lookup first needs it.
    Setting(std::sync::Arc<dyn plurx_core::store::Store>),
}

impl StoreAccess {
    /// A switch already decided, for tests.
    #[cfg(test)]
    pub(crate) fn new(root: PathBuf, enabled: bool) -> Self {
        Self {
            root,
            switch: Switch::Fixed(enabled),
            node_id: None,
            membership: None,
            jobs: None,
        }
    }

    /// The store ignored, as when the switch is off.
    #[cfg(test)]
    pub(crate) fn off() -> Self {
        Self::new(PathBuf::new(), false)
    }

    /// This node's store under `runtime_cache`, with the switch read from
    /// `store` when a lookup first needs it.
    ///
    /// A store read that fails takes the store out of the answer rather than
    /// failing the caller: ignoring the store is exactly the behaviour that
    /// shipped before it, so the cost of an unreadable setting is one
    /// extraction the store might have saved.
    pub(crate) fn from_setting(
        store: std::sync::Arc<dyn plurx_core::store::Store>,
        runtime_cache: &Path,
    ) -> Self {
        Self {
            root: store_root(runtime_cache),
            switch: Switch::Setting(store),
            node_id: None,
            membership: None,
            jobs: None,
        }
    }

    /// Name the node this access serves, so a miss with no directory can be
    /// classified — in a detached task, never on the request path.
    pub(crate) fn on_node(mut self, node_id: Option<&str>) -> Self {
        self.node_id = node_id.map(str::to_owned);
        self
    }

    pub(crate) fn with_membership(mut self, membership: MembershipManager) -> Self {
        self.membership = Some(membership);
        self
    }

    pub(crate) fn with_jobs(mut self, jobs: std::sync::Arc<crate::state::JobManager>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    pub(crate) fn membership(&self) -> Option<&MembershipManager> {
        self.membership.as_ref()
    }

    pub(crate) fn jobs(&self) -> Option<&std::sync::Arc<crate::state::JobManager>> {
        self.jobs.as_ref()
    }

    pub(crate) fn catalog(&self) -> Option<&dyn plurx_core::store::Store> {
        match &self.switch {
            Switch::Setting(store) => Some(store.as_ref()),
            #[cfg(test)]
            Switch::Fixed(_) => None,
        }
    }

    pub(crate) fn store(&self) -> Option<std::sync::Arc<dyn plurx_core::store::Store>> {
        match &self.switch {
            Switch::Setting(store) => Some(std::sync::Arc::clone(store)),
            #[cfg(test)]
            Switch::Fixed(_) => None,
        }
    }

    /// The operator's saved manual enable switch. Readiness probes are shown
    /// in Developer settings but never consulted when this value is set.
    pub(crate) async fn cluster_enabled(&self) -> bool {
        let Some(store) = self.catalog() else {
            return false;
        };
        store
            .get_setting(plurx_core::store::keys::SUBTITLE_CLUSTER_SOURCES)
            .await
            .ok()
            .is_some_and(|value| plurx_core::store::stored_switch(value.as_deref(), false))
    }

    pub(crate) fn node_id(&self) -> Option<&str> {
        self.node_id.as_deref()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    async fn is_enabled(&self) -> bool {
        match &self.switch {
            #[cfg(test)]
            Switch::Fixed(enabled) => *enabled,
            Switch::Setting(store) => enabled(store.as_ref()).await,
        }
    }
}

/// Where a consumer's live `fstat` comes from. Taken only after the switch,
/// the MPEG-TS rule and the manifest have all said the store could answer, so
/// a miss on any of those never touches the media mount.
pub(crate) enum Live<'a> {
    /// The descriptor a burn session holds open: `(dev, ino)` of that inode.
    Handle(&'a std::fs::File),
    /// The overlay's rule, taken the way its own `source_is_current` takes it.
    Path(&'a Path),
    /// A stamp taken already, for tests.
    #[cfg(test)]
    Stamp(SourceStamp),
}

impl Live<'_> {
    async fn stamp(self) -> Option<SourceStamp> {
        let metadata = match self {
            Self::Handle(handle) => handle.metadata().ok()?,
            Self::Path(path) => tokio::fs::metadata(path).await.ok()?,
            #[cfg(test)]
            Self::Stamp(stamp) => return Some(stamp),
        };
        Some(crate::fragment_index_cluster::source_stamp(&metadata))
    }
}

/// `subtitles.stored_sources`, parsed the way every other switch is. On by
/// default: keeping and reading the tracks is the point of the store.
///
/// One switch for both sides. It stops the producer
/// ([`crate::subtitle_ride_along::RideAlongGate::open`]) and makes both
/// consumers ignore what is already stored, so a wrong artifact a bad build
/// published is out of service without a redeploy. An unreadable setting is
/// off for both.
pub(crate) async fn enabled(store: &dyn plurx_core::store::Store) -> bool {
    match store
        .get_setting(plurx_core::store::keys::SUBTITLE_STORED_SOURCES)
        .await
    {
        Ok(value) => plurx_core::store::stored_switch(value.as_deref(), true),
        Err(error) => {
            tracing::debug!(%error, "reading the stored-subtitle switch; ignoring the store");
            false
        }
    }
}

/// Why a lookup fell through to today's extraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MissReason {
    /// No directory, no manifest, no entry for this ordinal, or an entry
    /// whose verdict left nothing to use (`malformed`, `transient`), or a
    /// stored file that is gone by the time it is opened.
    Absent,
    /// A manifest exists and is not current: another source identity, another
    /// format version, an unreadable document, or a latch still open.
    Stale,
    /// The switch is off.
    Disabled,
    /// A burn of an MPEG-TS source: the ride-along has no `-copyts`, and a
    /// discontinuous transport stream was never shown to agree.
    Mpegts,
    /// The stored bytes do not hash to the manifest's sha256.
    HashMismatch,
    /// No directory, and this node holds no fragment index for the file's
    /// current source: the pass that fills the store has never run here.
    NeverIndexed,
    /// No directory, and every fragment index this node holds for the file's
    /// current source was built by another node — a location row for this
    /// node whose `built_by_node_id` is a peer. The node hydrated the index,
    /// so the pass that keeps PGS tracks never ran here (§6.6).
    HydratedOnly,
    /// The overlay asked about a track the store holds as `empty`. The burn
    /// path can answer "nothing to burn" from that; the overlay cannot, so it
    /// demuxes the source as before and the lookup is a fall-through.
    EmptyForOverlay,
}

impl MissReason {
    const ALL: [Self; 8] = [
        Self::Absent,
        Self::Stale,
        Self::Disabled,
        Self::Mpegts,
        Self::HashMismatch,
        Self::NeverIndexed,
        Self::HydratedOnly,
        Self::EmptyForOverlay,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Stale => "stale",
            Self::Disabled => "disabled",
            Self::Mpegts => "mpegts",
            Self::HashMismatch => "hash_mismatch",
            Self::NeverIndexed => "never_indexed",
            Self::HydratedOnly => "hydrated_only",
            Self::EmptyForOverlay => "empty_track",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Why a burn derivation from a stored track fell back to the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// ffmpeg could not derive the `.mks`, or produced nothing.
    DeriveFailed,
    /// The derived `.mks` would exceed the burn sidecar's bound.
    OverBound,
    /// The derivation did not finish inside its own short budget — a stalled
    /// read of the cache disk, say — and was killed.
    TimedOut,
}

impl Fallback {
    const ALL: [Self; 3] = [Self::DeriveFailed, Self::OverBound, Self::TimedOut];

    fn label(self) -> &'static str {
        match self {
            Self::DeriveFailed => "derive_failed",
            Self::OverBound => "over_bound",
            Self::TimedOut => "timed_out",
        }
    }
}

/// A kept track, not yet opened. Opening it is what verifies it.
#[derive(Clone, Debug)]
pub(crate) struct KeptTrack {
    consumer: Consumer,
    dir: PathBuf,
    path: PathBuf,
    sha256: String,
    format: RepresentationFormat,
}

impl KeptTrack {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn format(&self) -> RepresentationFormat {
        self.format
    }
}

/// What the store can say about one track.
#[derive(Debug)]
pub(crate) enum Lookup {
    Kept(KeptTrack),
    /// A real track with no cues. Carries the directory for its `.access`.
    Empty(PathBuf),
    Miss(MissReason),
}

/// Whether a container is, or cannot be shown not to be, an MPEG transport
/// stream. The scanner records the lowercased extension, so this reads the
/// extensions a transport stream travels under; a file with none is treated
/// as one, because the exclusion exists for what was never tested.
pub(crate) fn is_mpegts_container(file: &MediaFile) -> bool {
    match file.container.as_deref() {
        Some(container) => matches!(
            container,
            "ts" | "m2ts" | "mts" | "m2t" | "mpegts" | "tp" | "trp"
        ),
        None => true,
    }
}

/// Look one track up for a consumer.
///
/// Cheapest question first, and the media mount last: the switch, the MPEG-TS
/// rule, then the manifest on the cache disk, and only when all three say the
/// store could answer, the live `fstat` of the source. An overlay lookup of an
/// `empty` track is a fall-through ([`MissReason::EmptyForOverlay`]): the
/// overlay has no "nothing to show" answer and demuxes the source.
///
/// A miss or an `empty` answer is counted here. A `kept` answer is counted
/// when it is opened — [`open_verified`] or [`copy_verified`] — because that
/// is when the store's bytes are actually relied on, and when a hash mismatch
/// or a swept file turns it into a miss after all.
pub(crate) async fn lookup(
    access: &StoreAccess,
    consumer: Consumer,
    file: &MediaFile,
    ordinal: i64,
    live: Live<'_>,
) -> Lookup {
    let mut no_directory = false;
    let result = match classify(access, consumer, file, ordinal, live, &mut no_directory).await {
        Lookup::Empty(_) if consumer == Consumer::Overlay => {
            Lookup::Miss(MissReason::EmptyForOverlay)
        }
        other => other,
    };
    if no_directory {
        // Answered at once; why there is no directory is counted later.
        classify_no_directory_later(access, consumer, file);
        return result;
    }
    match &result {
        Lookup::Kept(_) => {}
        Lookup::Empty(_) => record_empty(consumer),
        Lookup::Miss(reason) => record_miss(consumer, *reason),
    }
    result
}

/// Fetch one representation from a reachable holder. Call this from a
/// detached subtitle flight, never from a segment or session-start handler:
/// the peer budget can be eight seconds. A false answer leaves the caller's
/// existing miss path intact.
pub(crate) async fn hydrate_from_peers(
    access: &StoreAccess,
    file: &MediaFile,
    ordinal: i64,
    format: RepresentationFormat,
) -> bool {
    let (Some(catalog), Some(node_id), Some(membership)) = (
        access.catalog(),
        access.node_id(),
        access.membership.as_ref(),
    ) else {
        return false;
    };
    if !access.is_enabled().await || ordinal < 0 || file.size < 0 {
        return false;
    }
    let rows = match catalog
        .list_subtitle_source_publications(file.id, file.size, file.mtime)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(file_id = file.id, %error, "listing subtitle-source holders");
            return false;
        }
    };
    if !rows.iter().any(|row| {
        row.ordinal == ordinal && row.format == format.publication_name() && row.verdict == "kept"
    }) {
        return false;
    }

    // A new held descriptor is attested on this node. `object_version` is
    // intentionally not sent across hosts: its dev/ino values are local.
    let attested =
        match crate::fragment_index_cluster::attest_source(node_id, file, None, &|_| {}).await {
            Ok(attested) => attested,
            Err(error) => {
                tracing::debug!(file_id = file.id, %error, "attesting hydration source");
                return false;
            }
        };
    let Ok(metadata) = attested.handle.metadata() else {
        return false;
    };
    let bound = crate::fragment_index_cluster::source_stamp(&metadata);
    if bound.size != file.size as u64 || bound.mtime != file.mtime {
        return false;
    }
    let peers = match membership.media_peers().await {
        Ok(peers) => peers,
        Err(error) => {
            tracing::debug!(file_id = file.id, %error, "listing subtitle-source peers");
            return false;
        }
    };
    let reachable: std::collections::HashMap<_, _> = peers
        .into_iter()
        .filter(|peer| peer.reachable && peer.node_id != node_id)
        .filter_map(|peer| peer.http_base.map(|base| (peer.node_id, base)))
        .collect();
    let transport = PeerTransport::new(membership.clone());
    for row in rows {
        if row.ordinal != ordinal
            || row.format != format.publication_name()
            || row.verdict != "kept"
            || row.source_attestation != attested.observation.source_sha256
            || row.bytes <= 0
            || row.bytes as u64 > MAX_TRACK_BYTES
        {
            continue;
        }
        let Some(base) = reachable.get(&row.node_id) else {
            continue;
        };
        let path = format!(
            "/internal/media/subtitle-source/{}/{}/{}",
            file.id,
            ordinal,
            format.publication_name()
        );
        let response = transport
            .request(
                &row.node_id,
                base,
                reqwest::Method::GET,
                &path,
                Vec::new(),
                tokio::time::Instant::now() + Duration::from_secs(8),
                MAX_TRACK_BYTES as usize,
                PeerAuthMode::ExactRequest,
            )
            .await;
        let Ok(response) = response else { continue };
        match peer_artifact_verdict(&row, &response) {
            PeerArtifactVerdict::Forget => {
                forget_stale_publication(catalog, &row).await;
                continue;
            }
            PeerArtifactVerdict::Skip => continue,
            PeerArtifactVerdict::Accept => {}
        }
        if install_hydrated(access, file, &attested.handle, bound, &row, &response.body).await {
            crate::telemetry::record_subtitle_source(
                crate::telemetry::SubtitleSourceMetric::HydrationFetchedBytes(
                    response.body.len() as u64
                ),
            );
            crate::telemetry::record_subtitle_source(
                crate::telemetry::SubtitleSourceMetric::LookupHydrated,
            );
            return true;
        }
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerArtifactVerdict {
    Accept,
    Skip,
    Forget,
}

fn peer_artifact_verdict(
    row: &SubtitleSourcePublication,
    response: &crate::http::peer_transport::PeerResponse,
) -> PeerArtifactVerdict {
    if response.status == reqwest::StatusCode::NOT_FOUND {
        return PeerArtifactVerdict::Forget;
    }
    if !response.status.is_success() {
        return PeerArtifactVerdict::Skip;
    }
    if response.body.len() as i64 != row.bytes
        || hex::encode(Sha256::digest(&response.body)) != row.sha256
    {
        return PeerArtifactVerdict::Forget;
    }
    PeerArtifactVerdict::Accept
}

async fn forget_stale_publication(
    catalog: &dyn plurx_core::store::Store,
    row: &SubtitleSourcePublication,
) {
    if let Err(error) = catalog
        .delete_subtitle_source_publication(
            row.file_id,
            row.source_size,
            row.source_mtime,
            &row.node_id,
            row.ordinal,
            &row.format,
        )
        .await
    {
        tracing::debug!(file_id = row.file_id, ordinal = row.ordinal, %error, "forgetting stale subtitle-source holder");
    }
}

async fn install_hydrated(
    access: &StoreAccess,
    file: &MediaFile,
    held: &std::fs::File,
    bound: SourceStamp,
    row: &SubtitleSourcePublication,
    bytes: &[u8],
) -> bool {
    use tokio::io::AsyncWriteExt;

    let Some(format) = RepresentationFormat::from_publication_name(&row.format) else {
        return false;
    };
    let Some(name) = representation_file_name(row.ordinal, format, &row.sha256) else {
        return false;
    };
    let dir = file_dir(&access.root, file.id);
    let _guard = file_lock(&access.root, file.id).lock().await;
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return false;
    }
    let old = read_manifest(&dir).await;
    if old.as_ref().is_some_and(|manifest| {
        manifest.version == MANIFEST_VERSION
            && manifest.file_id == file.id
            && manifest.source == bound
            && manifest.track(row.ordinal).is_some_and(|track| {
                track.representation(format).is_some_and(|representation| {
                    representation.origin == RepresentationOrigin::Extracted
                        && representation.verdict == Verdict::Kept
                })
            })
    }) {
        return true;
    }
    let temporary = dir.join(format!(".hydrate-{}.tmp", uuid::Uuid::new_v4().simple()));
    let write = async {
        let mut staged = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        staged.write_all(bytes).await?;
        staged.sync_all().await?;
        drop(staged);
        tokio::fs::rename(&temporary, dir.join(&name)).await
    }
    .await;
    if write.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return false;
    }

    // The original descriptor remains open from attestation through rename.
    // Also compare the current pathname, since replacing the directory entry
    // can leave a held inode's fstat unchanged.
    if !hydration_source_still_bound(held, &file.path, bound).await {
        if !old.as_ref().is_some_and(|manifest| {
            manifest.tracks.iter().any(|track| {
                track
                    .representations
                    .iter()
                    .any(|representation| representation.file.as_deref() == Some(name.as_str()))
            })
        }) {
            let _ = tokio::fs::remove_file(dir.join(&name)).await;
        }
        return false;
    }

    let kind = match row.kind.as_str() {
        "pgs" => TrackKind::Pgs,
        "text" => TrackKind::Text,
        "text_styled" => TrackKind::TextStyled,
        _ => return false,
    };
    let attempts = row.attempts.clamp(0, i64::from(u32::MAX)) as u32;
    let representation = RepresentationEntry {
        format,
        origin: RepresentationOrigin::Hydrated,
        verdict: Verdict::Kept,
        attempts,
        file: Some(name.clone()),
        sha256: Some(row.sha256.clone()),
        bytes: bytes.len() as u64,
    };
    let incoming = Manifest {
        version: MANIFEST_VERSION,
        file_id: file.id,
        source: bound,
        ordinals: vec![row.ordinal],
        tracks: vec![TrackEntry {
            ordinal: row.ordinal,
            kind,
            representations: vec![representation],
            verdict: Verdict::Kept,
            attempts,
            file: Some(name.clone()),
            sha256: Some(row.sha256.clone()),
        }],
    };
    let manifest = merge_manifest(incoming, old.as_ref());
    if write_manifest_atomic(&dir, &manifest).await.is_err() {
        return false;
    }
    let mut publication = row.clone();
    publication.node_id = access.node_id.clone().unwrap_or_default();
    publication.origin = "hydrated".to_owned();
    publication.published_at_ms = crate::fragment_index_cluster::unix_ms();
    if let Some(catalog) = access.catalog() {
        if let Err(error) = catalog
            .upsert_subtitle_source_publication(&publication)
            .await
        {
            tracing::debug!(file_id = file.id, ordinal = row.ordinal, %error, "publishing hydrated subtitle-source row");
        }
    }
    record_access(&dir).await;
    true
}

async fn hydration_source_still_bound(
    held: &std::fs::File,
    path: &Path,
    bound: SourceStamp,
) -> bool {
    let held_after = held
        .metadata()
        .ok()
        .map(|metadata| crate::fragment_index_cluster::source_stamp(&metadata));
    let path_after = tokio::fs::metadata(path)
        .await
        .ok()
        .map(|metadata| crate::fragment_index_cluster::source_stamp(&metadata));
    held_after == Some(bound) && path_after == Some(bound)
}

/// Whether the store holds `ordinal` as a real track with no cues, for the
/// burn path's rule — the HTTP HDR guard's question, asked only when it is
/// about to refuse. Nothing is counted: this is not a consumer's lookup.
pub(crate) async fn stored_as_empty(
    access: &StoreAccess,
    file: &MediaFile,
    ordinal: i64,
    live: Live<'_>,
) -> bool {
    matches!(
        classify(access, Consumer::Burn, file, ordinal, live, &mut false).await,
        Lookup::Empty(_)
    )
}

/// Classifications in flight. Past the bound a miss with no directory is
/// simply `absent`: the reason is a counter, and a burst of misses must not
/// become a burst of catalog reads.
static CLASSIFYING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const MAX_CLASSIFYING: usize = 8;

/// Count a miss with no directory under the reason it deserves, without the
/// request waiting for it. The catalog read runs in a detached task, bounded
/// in number; when there is no store or no node to ask about, or the bound is
/// reached, the miss is counted `absent` at once.
fn classify_no_directory_later(access: &StoreAccess, consumer: Consumer, file: &MediaFile) {
    let (Switch::Setting(store), Some(node_id)) = (&access.switch, access.node_id.clone()) else {
        record_miss(consumer, MissReason::Absent);
        return;
    };
    if CLASSIFYING.fetch_add(1, Ordering::AcqRel) >= MAX_CLASSIFYING {
        CLASSIFYING.fetch_sub(1, Ordering::AcqRel);
        record_miss(consumer, MissReason::Absent);
        return;
    }
    let store = std::sync::Arc::clone(store);
    let (file_id, size, mtime) = (file.id, file.size, file.mtime);
    tokio::spawn(async move {
        let reason = classify_no_directory(store.as_ref(), &node_id, file_id, size, mtime).await;
        record_miss(consumer, reason);
        CLASSIFYING.fetch_sub(1, Ordering::AcqRel);
    });
}

/// Why a file has no directory here, from this node's own holdings for the
/// file's current source: an index in this node's own table (built here, by
/// either path) is `absent` — the pass ran and kept nothing, or ran before the
/// producer shipped or while the switch was off; otherwise the cluster's
/// location rows say whether this node only hydrated a peer's index
/// (`hydrated_only`) or holds none at all (`never_indexed`). A read that fails
/// is `absent`: the reason is a counter, never a decision.
pub(crate) async fn classify_no_directory(
    store: &dyn plurx_core::store::Store,
    node_id: &str,
    file_id: i64,
    size: i64,
    mtime: i64,
) -> MissReason {
    match store
        .holds_fragment_index_for_source(file_id, size, mtime)
        .await
    {
        Ok(true) => return MissReason::Absent,
        Ok(false) => {}
        Err(error) => {
            tracing::debug!(file_id, %error, "classifying a stored-subtitle miss");
            return MissReason::Absent;
        }
    }
    match store
        .fragment_index_builders_held_by(node_id, file_id, size, mtime)
        .await
    {
        Ok(builders) => absent_reason_from(node_id, &builders),
        Err(error) => {
            tracing::debug!(file_id, %error, "classifying a stored-subtitle miss");
            MissReason::Absent
        }
    }
}

async fn classify(
    access: &StoreAccess,
    consumer: Consumer,
    file: &MediaFile,
    ordinal: i64,
    live: Live<'_>,
    no_directory: &mut bool,
) -> Lookup {
    if !access.is_enabled().await {
        return Lookup::Miss(MissReason::Disabled);
    }
    if consumer == Consumer::Burn && is_mpegts_container(file) {
        return Lookup::Miss(MissReason::Mpegts);
    }
    let dir = file_dir(&access.root, file.id);
    let bytes = match plurx_core::fs_secure::read_bounded_regular(
        &dir.join(MANIFEST_NAME),
        MAX_MANIFEST_BYTES,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            *no_directory = true;
            return Lookup::Miss(MissReason::Absent);
        }
        Err(_) => return Lookup::Miss(MissReason::Stale),
    };
    let manifest = match parse_manifest(&bytes) {
        Ok(manifest) => manifest,
        Err(reason) => return Lookup::Miss(reason),
    };
    let Some(live) = live.stamp().await else {
        return Lookup::Miss(MissReason::Stale);
    };
    // A partial manifest can answer one representation while another track
    // or representation is still being produced or has failed. The old
    // whole-file latch remains available to producer scheduling, but it must
    // not hide a verified, current artefact from a consumer.
    if manifest.version != MANIFEST_VERSION
        || manifest.file_id != file.id
        || !manifest.source_matches(&live, consumer)
    {
        return Lookup::Miss(MissReason::Stale);
    }
    let Some(track) = manifest.track(ordinal) else {
        return Lookup::Miss(MissReason::Absent);
    };
    let format = match consumer {
        Consumer::Vtt => RepresentationFormat::Webvtt,
        Consumer::Overlay => RepresentationFormat::Sup,
        Consumer::Burn if track.kind == TrackKind::TextStyled => RepresentationFormat::Matroska,
        Consumer::Burn => RepresentationFormat::Sup,
    };
    let Some(representation) = track.representation(format) else {
        return Lookup::Miss(MissReason::Absent);
    };
    match representation.verdict {
        Verdict::Empty => Lookup::Empty(dir),
        Verdict::Malformed | Verdict::Transient => Lookup::Miss(MissReason::Absent),
        Verdict::Kept => {
            // The name is derived, not trusted: a manifest naming anything
            // other than this track's content name — a path, another track's
            // file — is not one this build wrote.
            let (Some(name), Some(sha256)) = (
                representation.file.as_deref(),
                representation.sha256.as_deref(),
            ) else {
                return Lookup::Miss(MissReason::Stale);
            };
            if representation_file_name(ordinal, format, sha256).as_deref() != Some(name) {
                return Lookup::Miss(MissReason::Stale);
            }
            Lookup::Kept(KeptTrack {
                consumer,
                path: dir.join(name),
                dir,
                sha256: sha256.to_owned(),
                format,
            })
        }
    }
}

/// From the cluster's rows alone: `never_indexed` when this node holds no
/// index for the source,
/// `hydrated_only` when every one it holds was built by a peer, and `absent`
/// when it built one itself — the pass ran here and kept nothing, or ran
/// before the producer shipped or while the switch was off.
fn absent_reason_from(node_id: &str, builders: &[String]) -> MissReason {
    if builders.is_empty() {
        MissReason::NeverIndexed
    } else if builders.iter().any(|builder| builder == node_id) {
        MissReason::Absent
    } else {
        MissReason::HydratedOnly
    }
}

/// Best-effort LRU marker for one file's directory, exactly as the overlay
/// cache keeps one: atime is meaningless on relatime/noatime mounts, and
/// serving must not fail because this bookkeeping write did.
pub(crate) async fn record_access(dir: &Path) {
    crate::pgs_overlay::record_access(dir).await;
}

enum Opened {
    Verified(std::fs::File),
    Miss(MissReason),
}

/// Open a kept track and verify its bytes against the manifest's sha256.
///
/// The handle returned is the one that was hashed, rewound to its start, so
/// a consumer that reads through it reads exactly the bytes that were
/// verified whatever happens to the name afterwards. `None` is a miss, and
/// has been counted as one.
pub(crate) async fn open_verified(kept: &KeptTrack) -> Option<std::fs::File> {
    let path = kept.path.clone();
    let expected = kept.sha256.clone();
    let opened = tokio::task::spawn_blocking(move || open_and_hash(&path, &expected, None))
        .await
        .unwrap_or(Opened::Miss(MissReason::Absent));
    finish_open(kept, opened).await
}

/// Open only a representation named by this node's readable manifest for a
/// peer request. The returned descriptor has already been hashed and rewound;
/// callers can stream it without trusting a path or a publication row.
pub(crate) async fn open_verified_for_peer(
    root: &Path,
    file_id: i64,
    ordinal: i64,
    format: RepresentationFormat,
) -> Option<(std::fs::File, u64)> {
    let dir = file_dir(root, file_id);
    let manifest = read_manifest(&dir).await?;
    if manifest.file_id != file_id {
        return None;
    }
    let representation = manifest.track(ordinal)?.representation(format)?;
    if representation.verdict != Verdict::Kept {
        return None;
    }
    let name = representation.file.as_deref()?;
    let sha256 = representation.sha256.as_deref()?;
    if representation_file_name(ordinal, format, sha256).as_deref() != Some(name) {
        return None;
    }
    let path = dir.join(name);
    let expected = sha256.to_owned();
    let opened = tokio::task::spawn_blocking(move || open_and_hash(&path, &expected, None))
        .await
        .ok()?;
    let Opened::Verified(file) = opened else {
        return None;
    };
    let bytes = file.metadata().ok()?.len();
    Some((file, bytes))
}

/// Copy a kept track to `destination` while verifying it, in one read.
///
/// `destination` must not exist. On any miss it is removed again, so the
/// caller is left exactly where it was and runs today's extraction.
pub(crate) async fn copy_verified(kept: &KeptTrack, destination: &Path) -> bool {
    let path = kept.path.clone();
    let expected = kept.sha256.clone();
    let target = destination.to_owned();
    let opened =
        tokio::task::spawn_blocking(move || open_and_hash(&path, &expected, Some(&target)))
            .await
            .unwrap_or(Opened::Miss(MissReason::Absent));
    let verified = finish_open(kept, opened).await.is_some();
    if !verified {
        let _ = tokio::fs::remove_file(destination).await;
    }
    verified
}

async fn finish_open(kept: &KeptTrack, opened: Opened) -> Option<std::fs::File> {
    match opened {
        Opened::Verified(file) => {
            record_hit(kept.consumer);
            record_access(&kept.dir).await;
            Some(file)
        }
        Opened::Miss(reason) => {
            record_miss(kept.consumer, reason);
            None
        }
    }
}

fn open_and_hash(path: &Path, expected: &str, copy_to: Option<&Path>) -> Opened {
    use std::io::{Read, Seek, Write};

    let mut file = match plurx_core::fs_secure::open_read_nofollow_blocking(path) {
        Ok(file) => file,
        // Swept or republished between the manifest read and this open, or
        // never there: a miss either way, never an error.
        Err(_) => return Opened::Miss(MissReason::Absent),
    };
    let Ok(metadata) = file.metadata() else {
        return Opened::Miss(MissReason::Absent);
    };
    // A stored track that is not a bounded, non-empty regular file cannot be
    // the bytes the manifest hashed.
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_TRACK_BYTES {
        return Opened::Miss(MissReason::HashMismatch);
    }
    let mut copy = match copy_to {
        Some(target) => match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
        {
            Ok(copy) => Some(copy),
            Err(_) => return Opened::Miss(MissReason::Absent),
        },
        None => None,
    };
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut total = 0_u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => return Opened::Miss(MissReason::Absent),
        };
        total = total.saturating_add(read as u64);
        if total > MAX_TRACK_BYTES {
            return Opened::Miss(MissReason::HashMismatch);
        }
        hash.update(&buffer[..read]);
        if let Some(copy) = copy.as_mut() {
            if copy.write_all(&buffer[..read]).is_err() {
                return Opened::Miss(MissReason::Absent);
            }
        }
    }
    if hex::encode(hash.finalize()) != expected {
        return Opened::Miss(MissReason::HashMismatch);
    }
    if let Some(copy) = copy.as_mut() {
        if copy.flush().is_err() {
            return Opened::Miss(MissReason::Absent);
        }
    }
    if file.rewind().is_err() {
        return Opened::Miss(MissReason::Absent);
    }
    Opened::Verified(file)
}

// ---------------------------------------------------------------------------
// Counters.

static LOOKUP_HITS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static LOOKUP_EMPTY: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static LOOKUP_MISSES: [[AtomicU64; 8]; 3] = [const { [const { AtomicU64::new(0) }; 8] }; 3];
static FALLBACKS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

fn record_hit(consumer: Consumer) {
    LOOKUP_HITS[consumer.index()].fetch_add(1, Ordering::Relaxed);
    crate::telemetry::record_subtitle_source(crate::telemetry::SubtitleSourceMetric::LookupHit);
}

fn record_empty(consumer: Consumer) {
    LOOKUP_EMPTY[consumer.index()].fetch_add(1, Ordering::Relaxed);
    crate::telemetry::record_subtitle_source(crate::telemetry::SubtitleSourceMetric::LookupHit);
}

fn record_miss(consumer: Consumer, reason: MissReason) {
    LOOKUP_MISSES[consumer.index()][reason.index()].fetch_add(1, Ordering::Relaxed);
    crate::telemetry::record_subtitle_source(crate::telemetry::SubtitleSourceMetric::LookupMiss);
}

/// Count a `kept` lookup whose bytes this caller did not open itself: it
/// joined a flight another caller owns, or found the sidecar published by the
/// time it enlisted. The store still answered for it — the owner reads the
/// same current manifest — so it is a hit, not a lookup that went uncounted.
pub(crate) fn record_kept_joined(consumer: Consumer) {
    record_hit(consumer);
}

/// Count a burn derivation that fell back to reading the source.
pub(crate) fn record_fallback(reason: Fallback) {
    FALLBACKS[reason as usize].fetch_add(1, Ordering::Relaxed);
}

/// Lookups answered since this process started, as `(hits, empty, misses)`
/// summed over both consumers. For the Developer card.
pub(crate) fn lookup_snapshot() -> (u64, u64, u64) {
    let sum = |cells: &[AtomicU64; 3]| -> u64 {
        cells.iter().map(|cell| cell.load(Ordering::Relaxed)).sum()
    };
    let misses = LOOKUP_MISSES
        .iter()
        .flat_map(|row| row.iter())
        .map(|cell| cell.load(Ordering::Relaxed))
        .sum();
    (sum(&LOOKUP_HITS), sum(&LOOKUP_EMPTY), misses)
}

#[cfg(test)]
pub(crate) fn hits_for_test(consumer: Consumer) -> u64 {
    LOOKUP_HITS[consumer.index()].load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn misses_for_test(consumer: Consumer, reason: MissReason) -> u64 {
    LOOKUP_MISSES[consumer.index()][reason.index()].load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn fallbacks_for_test(reason: Fallback) -> u64 {
    FALLBACKS[reason as usize].load(Ordering::Relaxed)
}

/// Prometheus exposition for the store's consumers.
pub(crate) fn prometheus() -> String {
    use std::fmt::Write as _;

    let mut out = String::from(
        "# HELP plurx_subtitle_source_lookups_total Stored-subtitle lookups by consumer and outcome.\n\
         # TYPE plurx_subtitle_source_lookups_total counter\n",
    );
    for consumer in Consumer::ALL {
        let misses: u64 = LOOKUP_MISSES[consumer.index()]
            .iter()
            .map(|cell| cell.load(Ordering::Relaxed))
            .sum();
        for (outcome, count) in [
            ("hit", LOOKUP_HITS[consumer.index()].load(Ordering::Relaxed)),
            (
                "empty",
                LOOKUP_EMPTY[consumer.index()].load(Ordering::Relaxed),
            ),
            ("miss", misses),
        ] {
            let _ = writeln!(
                out,
                "plurx_subtitle_source_lookups_total{{consumer=\"{}\",outcome=\"{outcome}\"}} {count}",
                consumer.label()
            );
        }
    }
    out.push_str(
        "# HELP plurx_subtitle_source_misses_total Stored-subtitle lookups that fell through to extraction, by reason. never_indexed and hydrated_only split the lookups that found no directory by this node's own fragment-index holdings (none at all, or only a peer's), classified after the request has moved on.\n\
         # TYPE plurx_subtitle_source_misses_total counter\n",
    );
    for consumer in Consumer::ALL {
        for reason in MissReason::ALL {
            let _ = writeln!(
                out,
                "plurx_subtitle_source_misses_total{{consumer=\"{}\",reason=\"{}\"}} {}",
                consumer.label(),
                reason.label(),
                LOOKUP_MISSES[consumer.index()][reason.index()].load(Ordering::Relaxed)
            );
        }
    }
    out.push_str(
        "# HELP plurx_subtitle_source_fallbacks_total Burn sidecars derived from a stored track that fell back to reading the source, by reason.\n\
         # TYPE plurx_subtitle_source_fallbacks_total counter\n",
    );
    for reason in Fallback::ALL {
        let _ = writeln!(
            out,
            "plurx_subtitle_source_fallbacks_total{{reason=\"{}\"}} {}",
            reason.label(),
            FALLBACKS[reason as usize].load(Ordering::Relaxed)
        );
    }
    if let Some(footprint) = footprint() {
        let _ = write!(
            out,
            "# HELP plurx_subtitle_source_store_bytes Bytes the subtitle-source store occupies on this node, as its last sweep walk measured.\n\
             # TYPE plurx_subtitle_source_store_bytes gauge\n\
             plurx_subtitle_source_store_bytes {}\n\
             # HELP plurx_subtitle_source_store_directories Files the subtitle-source store holds a directory for on this node.\n\
             # TYPE plurx_subtitle_source_store_directories gauge\n\
             plurx_subtitle_source_store_directories {}\n",
            footprint.bytes, footprint.directories
        );
    }
    out.push_str(&crate::subtitle_ride_along::prometheus());
    out
}

// ---------------------------------------------------------------------------
// The sweep.

/// What one sweep call did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepOutcome {
    /// Directories removed because their file row is gone, their source moved,
    /// or they were abandoned without a manifest.
    pub(crate) removed: usize,
    /// Directories removed to bring the store under its size cap.
    pub(crate) evicted: usize,
    /// Deletes that failed and are left for the next pass — on Windows, a
    /// reader holding a stored file without `FILE_SHARE_DELETE`.
    pub(crate) deferred: usize,
    /// Stored tracks no manifest names, removed from directories that stay:
    /// what a publish interrupted between its rename and its manifest swap
    /// leaves behind.
    pub(crate) unnamed: usize,
    /// Where the next call starts: `f<id>` of the last directory examined,
    /// or empty once the walk has wrapped.
    pub(crate) next: String,
    /// What the store occupies after this call, measured when the walk
    /// wrapped (and the cap was enforced); `None` on a page that did not.
    pub(crate) footprint: Option<Footprint>,
}

/// Reconcile one page of the store with the catalog, and enforce the size cap
/// once per full walk.
///
/// Its own rule and cursor, beside `fragment_index_cluster::sweep_local_orphans`
/// rather than inside it: that sweep considers only `*.idx`/`*.tmp` blobs, and
/// `<cache>/subs` is LRU-pruned at 256 entries, so neither would ever reclaim
/// a directory here.
///
/// - a directory whose file row is gone, or whose row's size/mtime no longer
///   match the manifest, is deleted;
/// - reading a row that **fails** stops the sweep without deleting anything,
///   and leaves the cursor where it was — a catalog that cannot be read is
///   not evidence that a file is gone;
/// - a delete that fails is counted as deferred and retried next pass;
/// - at most `limit` directories are examined per call.
pub(crate) async fn sweep(
    store: &dyn plurx_core::store::Store,
    node_id: &str,
    root: &Path,
    cursor: Option<&str>,
    limit: usize,
    max_bytes: u64,
) -> Result<SweepOutcome, String> {
    sweep_with_rows(
        root,
        cursor,
        limit,
        max_bytes,
        Some((store, node_id)),
        |file_id| async move {
            store
                .get_file(file_id)
                .await
                .map(|row| row.map(|file| (file.size, file.mtime)))
                .map_err(|error| error.to_string())
        },
    )
    .await
}

/// The sweep with the catalog read injected, so a test can make it fail.
#[cfg(test)]
pub(crate) async fn sweep_with<F, Fut>(
    root: &Path,
    cursor: Option<&str>,
    limit: usize,
    max_bytes: u64,
    row: F,
) -> Result<SweepOutcome, String>
where
    F: Fn(i64) -> Fut,
    Fut: std::future::Future<Output = Result<Option<(i64, i64)>, String>>,
{
    sweep_with_rows(root, cursor, limit, max_bytes, None, row).await
}

async fn sweep_with_rows<F, Fut>(
    root: &Path,
    cursor: Option<&str>,
    limit: usize,
    max_bytes: u64,
    publication_owner: Option<(&dyn plurx_core::store::Store, &str)>,
    row: F,
) -> Result<SweepOutcome, String>
where
    F: Fn(i64) -> Fut,
    Fut: std::future::Future<Output = Result<Option<(i64, i64)>, String>>,
{
    let after = cursor
        .and_then(|cursor| cursor.strip_prefix('f'))
        .and_then(|id| id.parse::<i64>().ok());
    let mut ids = match list_file_dirs(root).await {
        Ok(ids) => ids,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SweepOutcome::default());
        }
        Err(error) => return Err(format!("list {}: {error}", root.display())),
    };
    ids.retain(|id| after.is_none_or(|after| *id > after));
    let page_len = limit.max(1).min(ids.len());
    let exhausted = page_len == ids.len();
    let mut outcome = SweepOutcome::default();
    let mut last = after;
    for file_id in ids.into_iter().take(page_len) {
        let _guard = file_lock(root, file_id).lock().await;
        let dir = file_dir(root, file_id);
        let doomed = match read_manifest(&dir).await {
            Some(manifest) => match row(file_id).await? {
                None => true,
                Some((size, mtime)) => {
                    let moved = manifest.source.size != size.max(0) as u64
                        || manifest.source.mtime != mtime;
                    if !moved {
                        outcome.unnamed += remove_unnamed_tracks(&dir, &manifest).await;
                    }
                    moved
                }
            },
            // No manifest this build can read. A publish writes its `.sup`
            // files before the manifest, so a young directory may be one in
            // progress; an old one is abandoned.
            None => older_than(&dir, ABANDONED_GRACE).await,
        };
        last = Some(file_id);
        if doomed {
            match remove_dir(&dir).await {
                Ok(()) => {
                    outcome.removed += 1;
                    delete_swept_publications(publication_owner, file_id).await;
                }
                Err(error) => {
                    tracing::debug!(dir = %dir.display(), %error, "stored subtitle directory is busy; retrying next pass");
                    outcome.deferred += 1;
                }
            }
        }
    }
    if exhausted {
        let (evicted, deferred, footprint) = enforce_cap(root, max_bytes, publication_owner).await;
        outcome.footprint = Some(footprint);
        outcome.evicted = evicted;
        outcome.deferred += deferred;
        outcome.next = String::new();
    } else {
        outcome.next = last.map(|id| format!("f{id}")).unwrap_or_default();
    }
    Ok(outcome)
}

async fn delete_swept_publications(
    publication_owner: Option<(&dyn plurx_core::store::Store, &str)>,
    file_id: i64,
) {
    if let Some((catalog, node_id)) = publication_owner {
        if let Err(error) = catalog
            .delete_subtitle_source_publications(file_id, node_id)
            .await
        {
            tracing::debug!(file_id, %error, "deleting swept subtitle-source publications");
        }
    }
}

async fn list_file_dirs(root: &Path) -> std::io::Result<Vec<i64>> {
    let mut entries = tokio::fs::read_dir(root).await?;
    let mut ids = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix('f'))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<i64>().ok())
        else {
            continue;
        };
        if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

pub(crate) async fn read_manifest(dir: &Path) -> Option<Manifest> {
    let bytes =
        plurx_core::fs_secure::read_bounded_regular(&dir.join(MANIFEST_NAME), MAX_MANIFEST_BYTES)
            .await
            .ok()?;
    parse_manifest(&bytes).ok()
}

async fn write_manifest_atomic(dir: &Path, manifest: &Manifest) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let bytes = serde_json::to_vec(manifest).map_err(std::io::Error::other)?;
    let temporary = dir.join(format!(
        ".{MANIFEST_NAME}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = async {
        let mut staged = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        staged.write_all(&bytes).await?;
        staged.sync_all().await?;
        drop(staged);
        tokio::fs::rename(&temporary, dir.join(MANIFEST_NAME)).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(temporary).await;
    }
    result
}

/// Remove `.sup` files the manifest does not name and that are older than
/// the abandonment grace — never a young one, which may be a publish between
/// its rename and its manifest swap. Returns how many went.
async fn remove_unnamed_tracks(dir: &Path, manifest: &Manifest) -> usize {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if ![".sup", ".vtt", ".mks"]
            .iter()
            .any(|extension| name.ends_with(extension))
            || manifest.tracks.iter().any(|track| {
                track.file.as_deref() == Some(name)
                    || track
                        .representations
                        .iter()
                        .any(|representation| representation.file.as_deref() == Some(name))
            })
        {
            continue;
        }
        let path = entry.path();
        if older_than(&path, ABANDONED_GRACE).await && tokio::fs::remove_file(&path).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

async fn older_than(path: &Path, age: Duration) -> bool {
    tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed >= age)
}

async fn remove_dir(dir: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Evict whole directories, least recently accessed first, until the store is
/// at or under `max_bytes`. Returns `(evicted, deferred, footprint)`.
/// Also measures the store as it walks, so the footprint the product shows
/// costs no second walk.
async fn enforce_cap(
    root: &Path,
    max_bytes: u64,
    publication_owner: Option<(&dyn plurx_core::store::Store, &str)>,
) -> (usize, usize, Footprint) {
    let Ok(ids) = list_file_dirs(root).await else {
        return (
            0,
            0,
            Footprint {
                measured_at_ms: unix_ms(),
                ..Footprint::default()
            },
        );
    };
    let mut dirs = Vec::with_capacity(ids.len());
    for file_id in ids {
        let dir = file_dir(root, file_id);
        let accessed = match tokio::fs::metadata(dir.join(".access")).await {
            Ok(access) => access.modified().unwrap_or(std::time::UNIX_EPOCH),
            Err(_) => tokio::fs::metadata(&dir)
                .await
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH),
        };
        let size = directory_bytes(&dir).await;
        dirs.push((accessed, size, file_id, dir));
    }
    let mut total: u64 = dirs.iter().map(|(_, size, _, _)| *size).sum();
    let mut remaining = dirs.len() as u64;
    let footprint = |total: u64, remaining: u64| Footprint {
        bytes: total,
        directories: remaining,
        measured_at_ms: unix_ms(),
    };
    if total <= max_bytes {
        return (0, 0, footprint(total, remaining));
    }
    dirs.sort_by_key(|(accessed, _, _, _)| *accessed);
    let (mut evicted, mut deferred) = (0, 0);
    for (_, size, file_id, dir) in dirs {
        if total <= max_bytes {
            break;
        }
        let _guard = file_lock(root, file_id).lock().await;
        match remove_dir(&dir).await {
            Ok(()) => {
                evicted += 1;
                remaining = remaining.saturating_sub(1);
                total = total.saturating_sub(size);
                delete_swept_publications(publication_owner, file_id).await;
            }
            // Still counted against the cap, so the next-oldest goes instead
            // and this one is retried on the next pass.
            Err(_) => deferred += 1,
        }
    }
    (evicted, deferred, footprint(total, remaining))
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// What the store occupies on this node's disk, as the last full sweep walk
/// measured it: the bytes of every `f<id>/` directory and how many there are.
/// Stages in flight are not counted; [`crate::subtitle_ride_along::active_rides`]
/// reports those.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Footprint {
    pub bytes: u64,
    pub directories: u64,
    pub measured_at_ms: i64,
}

static FOOTPRINT: std::sync::Mutex<Option<Footprint>> = std::sync::Mutex::new(None);

/// Remember the footprint a completed sweep walk measured.
pub(crate) fn record_footprint(footprint: Footprint) {
    *FOOTPRINT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(footprint);
}

/// The footprint the last completed sweep walk measured, if one has run.
pub(crate) fn footprint() -> Option<Footprint> {
    *FOOTPRINT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Everything the product shows about the store and its producer on this
/// node: what it occupies, its cap, what is being kept right now, and what
/// the ride-along has done since this process started. For the Maintenance
/// card, which is where background work says what it costs and where to turn
/// it off.
#[derive(Clone, Debug, Serialize)]
pub struct StoreDiagnostics {
    /// `None` until the store's sweep has walked it once in this process.
    pub footprint: Option<Footprint>,
    /// How long ago that walk was, by this node's clock.
    pub footprint_age_ms: Option<i64>,
    pub cap_bytes: u64,
    pub riding: Vec<crate::subtitle_ride_along::ActiveRide>,
    pub tracks_attempted: u64,
    pub kept: u64,
    pub empty: u64,
    pub malformed: u64,
    pub transient: u64,
    pub bytes_written: u64,
    pub manifests_published: u64,
    /// Files whose riding pass did not build its index, indexed without the
    /// ride-along until the next restart.
    pub files_not_riding: usize,
    /// Finished riding passes whose tracks were not published because the
    /// switch was turned off while they ran.
    pub discarded_switch_off: u64,
    /// Whether the next index pass on this node would keep PGS tracks.
    pub gate: GateState,
}

/// The ride-along gate as the Maintenance card shows it: open, or closed with
/// the reason — the switch, the self-test, the filesystem or the free space.
#[derive(Clone, Debug, Serialize)]
pub struct GateState {
    pub open: bool,
    pub reason: Option<String>,
}

pub(crate) fn diagnostics(switch_on: bool, runtime_cache: &Path) -> StoreDiagnostics {
    let gate = match crate::subtitle_ride_along::gate_verdict(switch_on, runtime_cache) {
        Ok(()) => GateState {
            open: true,
            reason: None,
        },
        Err(reason) => GateState {
            open: false,
            reason: Some(reason),
        },
    };
    let (attempted, [kept, empty, malformed, transient], written, published) =
        crate::subtitle_ride_along::snapshot();
    let footprint = footprint();
    StoreDiagnostics {
        footprint_age_ms: footprint
            .map(|footprint| unix_ms().saturating_sub(footprint.measured_at_ms).max(0)),
        footprint,
        cap_bytes: MAX_STORE_BYTES,
        riding: crate::subtitle_ride_along::active_rides(),
        tracks_attempted: attempted,
        kept,
        empty,
        malformed,
        transient,
        bytes_written: written,
        manifests_published: published,
        files_not_riding: crate::subtitle_ride_along::failed_ride_count(),
        discarded_switch_off: crate::subtitle_ride_along::discarded(
            crate::subtitle_ride_along::Discard::SwitchOff,
        ),
        gate,
    }
}

/// A file directory is flat — a manifest, `.sup` files and `.access`.
async fn directory_bytes(dir: &Path) -> u64 {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut total = 0_u64;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(metadata) = entry.metadata().await {
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

#[cfg(test)]
pub(crate) mod testing {
    //! Hand-written manifests, for the readers' tests: a test that wants a
    //! particular store state authors it rather than running a pass.

    use super::*;

    /// Held by every test that asserts an exact change in the process-global
    /// lookup counters, and by every test that moves the burn ones, so the
    /// deltas one test reads are its own.
    pub(crate) fn counter_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Store `bytes` as `ordinal`'s kept track and return its entry.
    pub(crate) fn kept(dir: &Path, ordinal: i64, bytes: &[u8]) -> TrackEntry {
        let sha256 = hex::encode(Sha256::digest(bytes));
        let name = sup_file_name(ordinal, &sha256).expect("valid name");
        std::fs::create_dir_all(dir).expect("store directory");
        std::fs::write(dir.join(&name), bytes).expect("stored track");
        TrackEntry {
            ordinal,
            kind: TrackKind::Pgs,
            representations: Vec::new(),
            verdict: Verdict::Kept,
            attempts: 1,
            file: Some(name),
            sha256: Some(sha256),
        }
    }

    pub(crate) fn settled(ordinal: i64, verdict: Verdict) -> TrackEntry {
        TrackEntry {
            ordinal,
            kind: TrackKind::Pgs,
            representations: Vec::new(),
            verdict,
            attempts: 1,
            file: None,
            sha256: None,
        }
    }

    /// The live stamp of a real file, as a consumer would take it.
    pub(crate) fn stamp_of(path: &Path) -> SourceStamp {
        crate::fragment_index_cluster::source_stamp(&std::fs::metadata(path).expect("source"))
    }

    pub(crate) fn write_manifest(
        root: &Path,
        file_id: i64,
        source: SourceStamp,
        tracks: Vec<TrackEntry>,
    ) -> PathBuf {
        let dir = file_dir(root, file_id);
        std::fs::create_dir_all(&dir).expect("store directory");
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            file_id,
            source,
            ordinals: tracks.iter().map(|track| track.ordinal).collect(),
            tracks,
        };
        std::fs::write(
            dir.join(MANIFEST_NAME),
            serde_json::to_vec(&manifest).expect("manifest"),
        )
        .expect("write manifest");
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use plurx_core::store::ClusterFragmentIndexStore;

    fn publication(
        ordinal: i64,
        origin: &str,
        verdict: &str,
        attempts: i64,
    ) -> SubtitleSourcePublication {
        SubtitleSourcePublication {
            file_id: 7,
            source_size: 10,
            source_mtime: 20,
            source_attestation: "a".repeat(64),
            node_id: "nuc4".into(),
            ordinal,
            kind: "text".into(),
            format: "webvtt".into(),
            verdict: verdict.into(),
            attempts,
            origin: origin.into(),
            sha256: if verdict == "kept" {
                "b".repeat(64)
            } else {
                String::new()
            },
            bytes: if verdict == "kept" { 12 } else { 0 },
            published_at_ms: 1,
        }
    }

    #[test]
    fn old_pgs_only_manifest_does_not_count_as_text_coverage() {
        let mut pgs = publication(0, "extracted", "kept", 1);
        pgs.kind = "pgs".into();
        pgs.format = "sup".into();
        assert!(extracted_ordinal_covered(&[pgs.clone()], 0));
        assert!(!extracted_ordinal_covered(&[pgs], 1));
        assert!(!extracted_ordinal_covered(
            &[publication(1, "hydrated", "kept", 1)],
            1
        ));
    }

    #[test]
    fn malformed_empty_and_exhausted_transient_verdicts_take_their_documented_paths() {
        for verdict in ["malformed", "empty"] {
            assert!(extracted_ordinal_covered(
                &[publication(2, "extracted", verdict, 1)],
                2
            ));
        }
        assert!(!extracted_ordinal_covered(
            &[publication(2, "extracted", "transient", 2)],
            2
        ));
        assert!(extracted_ordinal_covered(
            &[publication(2, "extracted", "transient", 3)],
            2
        ));
    }

    #[tokio::test]
    async fn hydrate_two_different_ordinals_concurrently_and_retain_both() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let dir = file_dir(&root, 7);
        tokio::fs::create_dir_all(&dir).await.expect("directory");
        let source = stamp(10, 20, Some(1), Some(2));
        let publish = |ordinal| {
            let root = root.clone();
            let dir = dir.clone();
            async move {
                let _guard = file_lock(&root, 7).lock().await;
                let previous = read_manifest(&dir).await;
                let incoming = Manifest {
                    version: MANIFEST_VERSION,
                    file_id: 7,
                    source,
                    ordinals: vec![ordinal],
                    tracks: vec![TrackEntry {
                        ordinal,
                        kind: TrackKind::Text,
                        representations: vec![RepresentationEntry {
                            format: RepresentationFormat::Webvtt,
                            origin: RepresentationOrigin::Hydrated,
                            verdict: Verdict::Empty,
                            attempts: 1,
                            file: None,
                            sha256: None,
                            bytes: 0,
                        }],
                        verdict: Verdict::Empty,
                        attempts: 1,
                        file: None,
                        sha256: None,
                    }],
                };
                write_manifest_atomic(&dir, &merge_manifest(incoming, previous.as_ref()))
                    .await
                    .expect("manifest");
            }
        };
        tokio::join!(publish(0), publish(1));
        let merged = read_manifest(&dir).await.expect("merged");
        assert_eq!(merged.ordinals, vec![0, 1]);
        assert!(merged.track(0).is_some());
        assert!(merged.track(1).is_some());
    }

    #[tokio::test]
    async fn replace_receiver_source_during_hydration_discards_the_entry() {
        let base = crate::test_tempdir().expect("source");
        let path = base.path().join("source.mkv");
        std::fs::write(&path, b"first").expect("first");
        let held = std::fs::File::open(&path).expect("held source");
        let bound = crate::fragment_index_cluster::source_stamp(&held.metadata().expect("fstat"));
        assert!(hydration_source_still_bound(&held, &path, bound).await);
        let replacement = base.path().join("replacement.mkv");
        std::fs::write(&replacement, b"first").expect("replacement");
        std::fs::rename(&replacement, &path).expect("replace");
        assert!(!hydration_source_still_bound(&held, &path, bound).await);
    }

    #[tokio::test]
    async fn hydrate_between_different_inodes_binds_burn_to_the_receivers_inode() {
        let base = crate::test_tempdir().expect("store");
        let runtime = base.path().join("runtime");
        let publisher = base.path().join("publisher.mkv");
        let receiver = base.path().join("receiver.mkv");
        std::fs::write(&publisher, b"same source bytes").expect("publisher");
        std::fs::write(&receiver, b"same source bytes").expect("receiver");
        let publisher_stamp = stamp_of(&publisher);
        let receiver_stamp = stamp_of(&receiver);
        assert_ne!(publisher_stamp.ino, receiver_stamp.ino);
        let file = media_file(
            72,
            receiver.clone(),
            receiver_stamp.size as i64,
            receiver_stamp.mtime,
        );
        let catalog: std::sync::Arc<dyn plurx_core::store::Store> =
            std::sync::Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("catalog"));
        let access = StoreAccess::from_setting(catalog.clone(), &runtime).on_node(Some("receiver"));
        let held = std::fs::File::open(&receiver).expect("receiver fd");
        let bytes = b"PG receiver bytes";
        let mut row = publication(0, "extracted", "kept", 1);
        row.file_id = file.id;
        row.source_size = file.size;
        row.source_mtime = file.mtime;
        row.node_id = "publisher".into();
        row.kind = "pgs".into();
        row.format = "sup".into();
        row.sha256 = hex::encode(Sha256::digest(bytes));
        row.bytes = bytes.len() as i64;
        assert!(install_hydrated(&access, &file, &held, receiver_stamp, &row, bytes).await);
        assert!(matches!(
            lookup(
                &access,
                Consumer::Burn,
                &file,
                0,
                Live::Handle(&std::fs::File::open(&receiver).expect("receiver fd"))
            )
            .await,
            Lookup::Kept(_)
        ));
        assert!(matches!(
            lookup(
                &access,
                Consumer::Burn,
                &file,
                0,
                Live::Handle(&std::fs::File::open(&publisher).expect("publisher fd"))
            )
            .await,
            Lookup::Miss(MissReason::Stale)
        ));
    }

    #[tokio::test]
    async fn stale_row_sha_mismatch_is_forgotten_and_the_next_holder_tried() {
        let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
        let mut stale = publication(0, "extracted", "kept", 1);
        stale.node_id = "first".into();
        let mut next = stale.clone();
        next.node_id = "second".into();
        next.sha256 = hex::encode(Sha256::digest(b"verified"));
        next.bytes = 8;
        catalog
            .upsert_subtitle_source_publication(&stale)
            .await
            .expect("first");
        catalog
            .upsert_subtitle_source_publication(&next)
            .await
            .expect("second");
        let bad = crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::OK,
            body: b"invalid!".to_vec(),
        };
        assert_eq!(
            peer_artifact_verdict(&stale, &bad),
            PeerArtifactVerdict::Forget
        );
        forget_stale_publication(&catalog, &stale).await;
        let good = crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::OK,
            body: b"verified".to_vec(),
        };
        assert_eq!(
            peer_artifact_verdict(&next, &good),
            PeerArtifactVerdict::Accept
        );
        let rows = catalog
            .list_subtitle_source_publications(7, 10, 20)
            .await
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].node_id, "second");
    }

    #[test]
    fn a_404_holder_is_skipped_and_forgotten() {
        let holder = publication(0, "extracted", "kept", 1);
        let missing = crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::NOT_FOUND,
            body: Vec::new(),
        };
        assert_eq!(
            peer_artifact_verdict(&holder, &missing),
            PeerArtifactVerdict::Forget
        );
        let unavailable = crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            body: Vec::new(),
        };
        assert_eq!(
            peer_artifact_verdict(&holder, &unavailable),
            PeerArtifactVerdict::Skip
        );
    }

    #[tokio::test]
    async fn the_sweep_deletes_its_publication_rows_with_the_directory() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        write_manifest(
            &root,
            7,
            stamp(10, 20, None, None),
            vec![settled(0, Verdict::Empty)],
        );
        let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
        let row = publication(0, "extracted", "empty", 1);
        catalog
            .upsert_subtitle_source_publication(&row)
            .await
            .expect("row");
        let outcome = sweep(&catalog, "nuc4", &root, None, 256, u64::MAX)
            .await
            .expect("sweep");
        assert_eq!(outcome.removed, 1);
        assert!(!file_dir(&root, 7).exists());
        assert!(catalog
            .list_subtitle_source_publications(7, 10, 20)
            .await
            .expect("rows")
            .is_empty());
    }

    #[test]
    fn legacy_pgs_manifest_reads_unchanged() {
        let json = r#"{"version":1,"file_id":7,"source":{"size":10,"mtime":20,"dev":null,"ino":null},"ordinals":[0],"tracks":[{"ordinal":0,"verdict":"empty","attempts":1}]}"#;
        let manifest = parse_manifest(json.as_bytes()).expect("legacy manifest");
        let track = manifest.track(0).expect("PGS track");
        assert_eq!(track.kind, TrackKind::Pgs);
        assert!(track.representations.is_empty());
        assert_eq!(
            track
                .representation(RepresentationFormat::Sup)
                .expect("legacy sup")
                .verdict,
            Verdict::Empty
        );
    }

    #[test]
    fn text_entry_misses_for_legacy_reader() {
        // A pre-M1 reader ignores the new fields and can only open the
        // legacy `.sup` name. A text entry carries no such name.
        let entry = TrackEntry {
            ordinal: 1,
            kind: TrackKind::Text,
            representations: vec![RepresentationEntry {
                format: RepresentationFormat::Webvtt,
                origin: RepresentationOrigin::Extracted,
                verdict: Verdict::Kept,
                attempts: 1,
                file: Some("s1-0123456789abcdef.vtt".into()),
                sha256: Some("0".repeat(64)),
                bytes: 8,
            }],
            verdict: Verdict::Kept,
            attempts: 1,
            file: None,
            sha256: None,
        };
        let legacy: serde_json::Value = serde_json::to_value(&entry).expect("serialize");
        assert!(legacy.get("file").is_none());
        assert!(entry.representation(RepresentationFormat::Sup).is_none());
    }

    #[tokio::test]
    async fn partial_styled_vtt_kept_mks_malformed_serves_vtt_not_burn() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let source = base.path().join("source.mkv");
        std::fs::write(&source, b"source bytes").expect("source");
        let live = stamp_of(&source);
        let file = media_file(99, source, live.size as i64, live.mtime);
        let dir = file_dir(&root, file.id);
        std::fs::create_dir_all(&dir).expect("directory");
        let bytes = b"WEBVTT\n\n";
        let sha256 = hex::encode(Sha256::digest(bytes));
        let name =
            representation_file_name(0, RepresentationFormat::Webvtt, &sha256).expect("name");
        std::fs::write(dir.join(&name), bytes).expect("VTT");
        let entry = TrackEntry {
            ordinal: 0,
            kind: TrackKind::TextStyled,
            representations: vec![
                RepresentationEntry {
                    format: RepresentationFormat::Webvtt,
                    origin: RepresentationOrigin::Extracted,
                    verdict: Verdict::Kept,
                    attempts: 1,
                    file: Some(name),
                    sha256: Some(sha256),
                    bytes: bytes.len() as u64,
                },
                RepresentationEntry {
                    format: RepresentationFormat::Matroska,
                    origin: RepresentationOrigin::Extracted,
                    verdict: Verdict::Malformed,
                    attempts: 1,
                    file: None,
                    sha256: None,
                    bytes: 0,
                },
            ],
            verdict: Verdict::Kept,
            attempts: 1,
            file: None,
            sha256: None,
        };
        write_manifest(&root, file.id, live, vec![entry]);
        let access = StoreAccess::new(root, true);
        assert!(matches!(
            lookup(&access, Consumer::Vtt, &file, 0, Live::Stamp(live)).await,
            Lookup::Kept(_)
        ));
        assert!(matches!(
            lookup(&access, Consumer::Burn, &file, 0, Live::Stamp(live)).await,
            Lookup::Miss(MissReason::Absent)
        ));
    }

    fn media_file(id: i64, path: PathBuf, size: i64, mtime: i64) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id,
            item_id: 1,
            path,
            size,
            mtime,
            duration_ms: Some(60_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn stamp(size: u64, mtime: i64, dev: Option<u64>, ino: Option<u64>) -> SourceStamp {
        SourceStamp {
            size,
            mtime,
            dev,
            ino,
        }
    }

    #[test]
    fn a_manifest_round_trips_and_an_unknown_version_is_a_miss() {
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            file_id: 42,
            source: stamp(10, 20, Some(1), Some(2)),
            ordinals: vec![0, 3],
            tracks: vec![
                TrackEntry {
                    ordinal: 0,
                    kind: TrackKind::Pgs,
                    representations: Vec::new(),
                    verdict: Verdict::Kept,
                    attempts: 1,
                    file: Some(sup_file_name(0, &"a".repeat(64)).expect("name")),
                    sha256: Some("a".repeat(64)),
                },
                settled(3, Verdict::Empty),
            ],
        };
        let bytes = serde_json::to_vec(&manifest).expect("encode");
        assert_eq!(parse_manifest(&bytes), Ok(manifest.clone()));
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.contains("\"verdict\":\"kept\""), "{text}");
        assert!(
            text.contains("\"file\":\"s0-aaaaaaaaaaaaaaaa.sup\""),
            "{text}"
        );

        // A future layout is a miss by its version, whatever else it changed.
        let future = serde_json::json!({"version": MANIFEST_VERSION + 1, "entirely": "different"});
        assert_eq!(
            parse_manifest(future.to_string().as_bytes()),
            Err(MissReason::Stale)
        );
        let mut older = serde_json::to_value(&manifest).expect("value");
        older["version"] = serde_json::json!(0);
        assert_eq!(
            parse_manifest(older.to_string().as_bytes()),
            Err(MissReason::Stale)
        );
        assert_eq!(parse_manifest(b"{"), Err(MissReason::Stale));
        assert_eq!(parse_manifest(b""), Err(MissReason::Stale));
    }

    #[test]
    fn current_needs_the_source_and_a_settled_verdict_for_every_probed_track() {
        let live = stamp(10, 20, Some(1), Some(2));
        let mut manifest = Manifest {
            version: MANIFEST_VERSION,
            file_id: 42,
            source: live,
            ordinals: vec![0, 1, 2],
            tracks: vec![
                settled(0, Verdict::Empty),
                settled(1, Verdict::Malformed),
                TrackEntry {
                    attempts: TRANSIENT_ATTEMPTS - 1,
                    ..settled(2, Verdict::Transient)
                },
            ],
        };
        assert!(
            !manifest.is_current(42, &live, Consumer::Overlay),
            "a transient track with attempts left holds the latch open"
        );
        manifest.tracks[2].attempts = TRANSIENT_ATTEMPTS;
        assert!(manifest.is_current(42, &live, Consumer::Overlay));
        assert!(manifest.is_current(42, &live, Consumer::Burn));

        assert!(
            !manifest.is_current(43, &live, Consumer::Overlay),
            "another file"
        );
        let mut missing = manifest.clone();
        missing.tracks.pop();
        assert!(
            !missing.is_current(42, &live, Consumer::Overlay),
            "a probed track with no verdict is not settled"
        );
        let mut versioned = manifest.clone();
        versioned.version += 1;
        assert!(!versioned.is_current(42, &live, Consumer::Overlay));

        for moved in [
            stamp(11, 20, Some(1), Some(2)),
            stamp(10, 21, Some(1), Some(2)),
        ] {
            assert!(!manifest.is_current(42, &moved, Consumer::Overlay));
            assert!(!manifest.is_current(42, &moved, Consumer::Burn));
        }
    }

    /// The burn path holds the inode, the overlay does not — and neither
    /// looks at ctime, which a hardlink or `chmod` moves without touching a
    /// byte.
    #[test]
    fn an_inode_change_is_stale_for_burn_but_current_for_the_overlay() {
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            file_id: 42,
            source: stamp(10, 20, Some(1), Some(2)),
            ordinals: vec![0],
            tracks: vec![settled(0, Verdict::Empty)],
        };
        for replaced in [
            stamp(10, 20, Some(1), Some(3)),
            stamp(10, 20, Some(9), Some(2)),
        ] {
            assert!(manifest.is_current(42, &replaced, Consumer::Overlay));
            assert!(!manifest.is_current(42, &replaced, Consumer::Burn));
        }
        // A platform with no inode identity falls back to size + mtime.
        assert!(manifest.is_current(42, &stamp(10, 20, None, None), Consumer::Burn));
        // A manifest that cannot vouch for the inode is not taken on trust
        // by the burn path where the live file has one.
        let unvouched = Manifest {
            source: stamp(10, 20, None, None),
            ..manifest.clone()
        };
        assert!(!unvouched.is_current(42, &stamp(10, 20, Some(1), Some(2)), Consumer::Burn));
        assert!(unvouched.is_current(42, &stamp(10, 20, Some(1), Some(2)), Consumer::Overlay));
    }

    #[tokio::test]
    async fn a_kept_track_is_a_hit_only_when_its_bytes_hash_to_the_manifest() {
        let _counters = counter_lock().lock().await;
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let source = base.path().join("source.mkv");
        std::fs::write(&source, b"source bytes").expect("source");
        let live = stamp_of(&source);
        let file = media_file(7, source.clone(), live.size as i64, live.mtime);
        let access = StoreAccess::new(root.clone(), true);
        let dir = file_dir(&root, 7);
        let entry = kept(&dir, 0, b"PGS bytes");
        write_manifest(&root, 7, live, vec![entry.clone()]);

        for consumer in [Consumer::Overlay, Consumer::Burn] {
            let Lookup::Kept(kept_track) =
                lookup(&access, consumer, &file, 0, Live::Stamp(live)).await
            else {
                panic!("a current kept track is found for {consumer:?}");
            };
            let before = hits_for_test(consumer);
            let mut opened = open_verified(&kept_track).await.expect("verified");
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut opened, &mut bytes).expect("read");
            assert_eq!(bytes, b"PGS bytes", "the handle is rewound to the start");
            assert!(hits_for_test(consumer) > before);
        }
        assert!(
            dir.join(".access").exists(),
            "a hit records its access marker"
        );

        // Same name, different bytes: the manifest's sha256 is the authority.
        let path = dir.join(entry.file.as_deref().expect("name"));
        std::fs::write(&path, b"PGS bytez").expect("tamper");
        let Lookup::Kept(kept_track) =
            lookup(&access, Consumer::Burn, &file, 0, Live::Stamp(live)).await
        else {
            panic!("the manifest still names it");
        };
        let before = misses_for_test(Consumer::Burn, MissReason::HashMismatch);
        assert!(
            open_verified(&kept_track).await.is_none(),
            "a mismatch is a miss"
        );
        assert!(misses_for_test(Consumer::Burn, MissReason::HashMismatch) > before);
        let copy = base.path().join("copied.sup");
        assert!(!copy_verified(&kept_track, &copy).await);
        assert!(!copy.exists(), "a refused copy leaves nothing behind");

        // Swept between the manifest read and the open: a miss, not an error.
        std::fs::remove_file(&path).expect("sweep the track");
        let before = misses_for_test(Consumer::Overlay, MissReason::Absent);
        assert!(open_verified(&kept_track).await.is_none());
        assert!(misses_for_test(Consumer::Overlay, MissReason::Absent) >= before);
    }

    #[tokio::test]
    async fn every_other_state_is_a_miss_with_its_reason() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let source = base.path().join("source.mkv");
        std::fs::write(&source, b"source bytes").expect("source");
        let live = stamp_of(&source);
        let file = media_file(9, source.clone(), live.size as i64, live.mtime);
        let on = StoreAccess::new(root.clone(), true);
        let miss = |lookup: Lookup| match lookup {
            Lookup::Miss(reason) => reason,
            other => panic!("expected a miss, got {other:?}"),
        };

        assert_eq!(
            miss(lookup(&on, Consumer::Overlay, &file, 0, Live::Stamp(live)).await),
            MissReason::Absent,
            "no directory"
        );
        let dir = file_dir(&root, 9);
        let entry = kept(&dir, 0, b"PGS");
        write_manifest(
            &root,
            9,
            live,
            vec![
                entry.clone(),
                settled(1, Verdict::Empty),
                settled(2, Verdict::Malformed),
            ],
        );
        assert!(matches!(
            lookup(&on, Consumer::Burn, &file, 1, Live::Stamp(live)).await,
            Lookup::Empty(_)
        ));
        // The overlay cannot answer "nothing to show", so for it an empty
        // track is a fall-through to the demux, counted as one.
        let (empty_before, fall_before) = (
            LOOKUP_EMPTY[Consumer::Overlay.index()].load(Ordering::Relaxed),
            misses_for_test(Consumer::Overlay, MissReason::EmptyForOverlay),
        );
        assert_eq!(
            miss(lookup(&on, Consumer::Overlay, &file, 1, Live::Stamp(live)).await),
            MissReason::EmptyForOverlay
        );
        assert_eq!(
            LOOKUP_EMPTY[Consumer::Overlay.index()].load(Ordering::Relaxed),
            empty_before,
            "never counted as an answered empty for the overlay"
        );
        assert!(misses_for_test(Consumer::Overlay, MissReason::EmptyForOverlay) > fall_before);
        assert_eq!(
            miss(lookup(&on, Consumer::Burn, &file, 2, Live::Stamp(live)).await),
            MissReason::Absent,
            "a malformed track left nothing to use"
        );
        assert_eq!(
            miss(lookup(&on, Consumer::Burn, &file, 5, Live::Stamp(live)).await),
            MissReason::Absent,
            "an ordinal the probe never saw"
        );
        assert_eq!(
            miss(
                lookup(
                    &StoreAccess::off(),
                    Consumer::Overlay,
                    &file,
                    0,
                    Live::Stamp(live)
                )
                .await
            ),
            MissReason::Disabled
        );
        assert_eq!(
            miss(
                lookup(
                    &StoreAccess::new(root.clone(), false),
                    Consumer::Burn,
                    &file,
                    0,
                    Live::Stamp(live)
                )
                .await
            ),
            MissReason::Disabled,
            "off ignores a valid store"
        );

        // A source whose size or mtime moved.
        for moved in [
            SourceStamp {
                size: live.size + 1,
                ..live
            },
            SourceStamp {
                mtime: live.mtime + 1,
                ..live
            },
        ] {
            for consumer in Consumer::ALL {
                assert_eq!(
                    miss(lookup(&on, consumer, &file, 0, Live::Stamp(moved)).await),
                    MissReason::Stale
                );
            }
        }

        // MPEG-TS never reaches the store for a burn; the overlay has no such
        // restriction.
        for container in [Some("m2ts"), Some("ts"), None] {
            let mut transport = file.clone();
            transport.container = container.map(str::to_owned);
            assert_eq!(
                miss(lookup(&on, Consumer::Burn, &transport, 0, Live::Stamp(live)).await),
                MissReason::Mpegts
            );
            assert!(matches!(
                lookup(&on, Consumer::Overlay, &transport, 0, Live::Stamp(live)).await,
                Lookup::Kept(_)
            ));
        }

        // A manifest naming a file that is not this track's content name.
        let mut wrong = entry.clone();
        wrong.file = Some("../elsewhere.sup".into());
        write_manifest(&root, 9, live, vec![wrong]);
        assert_eq!(
            miss(lookup(&on, Consumer::Overlay, &file, 0, Live::Stamp(live)).await),
            MissReason::Stale
        );
        std::fs::write(dir.join(MANIFEST_NAME), b"not json").expect("torn");
        assert_eq!(
            miss(lookup(&on, Consumer::Overlay, &file, 0, Live::Stamp(live)).await),
            MissReason::Stale
        );
    }

    /// Cheapest question first and the media mount last, and the switch read
    /// when a lookup needs it rather than when the access was built.
    #[tokio::test]
    async fn the_switch_and_the_cache_disk_answer_before_the_media_mount() {
        use plurx_core::store::Store;
        let base = crate::test_tempdir().expect("store");
        let runtime = base.path().join("runtime");
        let root = store_root(&runtime);
        // A source on a mount that is not there: stating it would fail.
        let gone = base.path().join("unmounted").join("source.mkv");
        let file = media_file(11, gone.clone(), 10, 20);
        let miss = |lookup: Lookup| match lookup {
            Lookup::Miss(reason) => reason,
            other => panic!("expected a miss, got {other:?}"),
        };
        assert_eq!(
            miss(
                lookup(
                    &StoreAccess::new(root.clone(), false),
                    Consumer::Overlay,
                    &file,
                    0,
                    Live::Path(&gone)
                )
                .await
            ),
            MissReason::Disabled,
            "off never reaches the manifest or the source"
        );
        assert_eq!(
            miss(
                lookup(
                    &StoreAccess::new(root.clone(), true),
                    Consumer::Overlay,
                    &file,
                    0,
                    Live::Path(&gone)
                )
                .await
            ),
            MissReason::Absent,
            "no manifest is a miss on the cache disk; the source is never stated"
        );

        let store: std::sync::Arc<dyn Store> = std::sync::Arc::new(
            plurx_core::store::SqliteStore::open_in_memory().expect("settings store"),
        );
        let access = StoreAccess::from_setting(std::sync::Arc::clone(&store), &runtime);
        store
            .put_setting(plurx_core::store::keys::SUBTITLE_STORED_SOURCES, "0")
            .await
            .expect("switch off");
        assert_eq!(
            miss(lookup(&access, Consumer::Burn, &file, 0, Live::Path(&gone)).await),
            MissReason::Disabled,
            "the switch is read when the lookup needs it"
        );
        store
            .put_setting(plurx_core::store::keys::SUBTITLE_STORED_SOURCES, "1")
            .await
            .expect("switch on");
        assert_eq!(
            miss(lookup(&access, Consumer::Overlay, &file, 0, Live::Path(&gone)).await),
            MissReason::Absent
        );
    }

    /// A miss with no directory says why, from the cluster's rows.
    #[test]
    fn a_miss_with_no_directory_is_classified_by_who_built_the_index() {
        let here = "node-a".to_owned();
        assert_eq!(absent_reason_from(&here, &[]), MissReason::NeverIndexed);
        assert_eq!(
            absent_reason_from(&here, &["node-b".to_owned()]),
            MissReason::HydratedOnly,
            "every index this node holds came from a peer"
        );
        assert_eq!(
            absent_reason_from(&here, &["node-a".to_owned(), "node-b".to_owned()]),
            MissReason::Absent,
            "the pass ran here at least once"
        );
    }

    async fn counted(consumer: Consumer, reason: MissReason, above: u64) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while misses_for_test(consumer, reason) <= above {
            assert!(
                std::time::Instant::now() < deadline,
                "{reason:?} was never counted"
            );
            tokio::task::yield_now().await;
        }
    }

    /// The request is answered before the catalog is asked: the lookup
    /// returns `absent` at once and the reason is counted by a detached task.
    /// A node whose own index table holds an index for the source — the
    /// non-cluster path's only record — is `absent`, not `never_indexed`.
    #[tokio::test]
    async fn a_miss_with_no_directory_is_classified_off_the_request_path() {
        use plurx_core::store::Store;

        let base = crate::test_tempdir().expect("store");
        let runtime = base.path().join("runtime");
        let source = base.path().join("source.mkv");
        std::fs::write(&source, b"source bytes").expect("source");
        let live = stamp_of(&source);
        let file = media_file(31, source.clone(), live.size as i64, live.mtime);
        let catalog: std::sync::Arc<dyn Store> =
            std::sync::Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("catalog"));
        let access = StoreAccess::from_setting(std::sync::Arc::clone(&catalog), &runtime)
            .on_node(Some("node-a"));

        let before = misses_for_test(Consumer::Overlay, MissReason::NeverIndexed);
        assert!(matches!(
            lookup(&access, Consumer::Overlay, &file, 0, Live::Stamp(live)).await,
            Lookup::Miss(MissReason::Absent)
        ));
        counted(Consumer::Overlay, MissReason::NeverIndexed, before).await;

        // An index this node built itself.
        let index = plurx_core::segplan::FragmentIndex::new(
            16_000,
            vec![plurx_core::segplan::IndexRow {
                dts: 0,
                duration: 28_016,
                bytes: 104_452,
                video_bytes: 103_836,
                class: plurx_core::fmp4::CutClass::CleanIdr,
            }],
            "abc123",
            // The index keys its source by the scanner's size and mtime.
            plurx_core::segplan::SourceIdentity::new(live.size, live.mtime, "fingerprint"),
        );
        catalog
            .put_fragment_index(file.id, &index)
            .await
            .expect("local index");
        assert_eq!(
            classify_no_directory(catalog.as_ref(), "node-a", file.id, file.size, file.mtime).await,
            MissReason::Absent
        );

        // Without a node to ask about, the miss is counted `absent` at once.
        let before = misses_for_test(Consumer::Burn, MissReason::Absent);
        assert!(matches!(
            lookup(
                &access.clone().on_node(None),
                Consumer::Burn,
                &file,
                0,
                Live::Stamp(live)
            )
            .await,
            Lookup::Miss(MissReason::Absent)
        ));
        assert!(misses_for_test(Consumer::Burn, MissReason::Absent) > before);
    }

    /// The HDR guard's question: only an `empty` entry for a current source
    /// answers yes, and nothing is counted.
    #[tokio::test]
    async fn stored_as_empty_is_yes_only_for_a_current_empty_track() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let source = base.path().join("source.mkv");
        std::fs::write(&source, b"source bytes").expect("source");
        let live = stamp_of(&source);
        let file = media_file(41, source.clone(), live.size as i64, live.mtime);
        let on = StoreAccess::new(root.clone(), true);
        assert!(
            !stored_as_empty(&on, &file, 1, Live::Stamp(live)).await,
            "no directory"
        );
        let dir = file_dir(&root, 41);
        write_manifest(
            &root,
            41,
            live,
            vec![kept(&dir, 0, b"PG"), settled(1, Verdict::Empty)],
        );
        assert!(stored_as_empty(&on, &file, 1, Live::Stamp(live)).await);
        assert!(
            !stored_as_empty(&on, &file, 0, Live::Stamp(live)).await,
            "kept"
        );
        assert!(
            !stored_as_empty(
                &StoreAccess::new(root.clone(), false),
                &file,
                1,
                Live::Stamp(live)
            )
            .await,
            "off"
        );
        let moved = SourceStamp {
            size: live.size + 1,
            ..live
        };
        assert!(
            !stored_as_empty(&on, &file, 1, Live::Stamp(moved)).await,
            "stale"
        );
    }

    /// A publish interrupted between placing a track and swapping the
    /// manifest leaves a `.sup` nothing names. The sweep removes it once it
    /// is older than the grace, and never a young one or a named one.
    #[tokio::test]
    async fn the_sweep_removes_old_tracks_the_manifest_does_not_name() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let dir = file_dir(&root, 1);
        let named = kept(&dir, 0, b"PG named");
        write_manifest(&root, 1, stamp(10, 1, None, None), vec![named.clone()]);
        let old = dir.join("s0-0123456789abcdef.sup");
        let young = dir.join("s1-fedcba9876543210.sup");
        std::fs::write(&old, b"PG orphan").expect("orphan");
        std::fs::write(&young, b"PG in flight").expect("young");
        let long_ago = std::time::SystemTime::now() - ABANDONED_GRACE - Duration::from_secs(60);
        for path in [&old, &dir.join(named.file.as_deref().expect("name"))] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open")
                .set_modified(long_ago)
                .expect("backdate");
        }
        static KNOWN: [(i64, i64, i64); 1] = [(1, 10, 1)];
        let outcome = sweep_with(&root, None, 256, u64::MAX, rows(&KNOWN))
            .await
            .expect("sweep");
        assert_eq!(outcome.unnamed, 1, "{outcome:?}");
        assert!(!old.exists(), "the old orphan goes");
        assert!(young.exists(), "a young one may be a publish in flight");
        assert!(dir.join(named.file.as_deref().expect("name")).exists());
    }

    #[test]
    fn the_exposition_names_every_outcome_and_reason() {
        let text = prometheus();
        for consumer in ["overlay", "burn"] {
            for outcome in ["hit", "empty", "miss"] {
                assert!(text.contains(&format!(
                    "plurx_subtitle_source_lookups_total{{consumer=\"{consumer}\",outcome=\"{outcome}\"}}"
                )));
            }
            for reason in [
                "absent",
                "stale",
                "disabled",
                "mpegts",
                "hash_mismatch",
                "never_indexed",
                "hydrated_only",
                "empty_track",
            ] {
                assert!(text.contains(&format!(
                    "plurx_subtitle_source_misses_total{{consumer=\"{consumer}\",reason=\"{reason}\"}}"
                )));
            }
        }
        for reason in ["derive_failed", "over_bound", "timed_out"] {
            assert!(text.contains(&format!(
                "plurx_subtitle_source_fallbacks_total{{reason=\"{reason}\"}}"
            )));
        }
    }

    type RowRead = std::future::Ready<Result<Option<(i64, i64)>, String>>;

    fn rows(known: &'static [(i64, i64, i64)]) -> impl Fn(i64) -> RowRead {
        move |id| {
            std::future::ready(Ok(known
                .iter()
                .find(|(file_id, _, _)| *file_id == id)
                .map(|(_, size, mtime)| (*size, *mtime))))
        }
    }

    #[tokio::test]
    async fn the_sweep_removes_what_the_catalog_no_longer_describes() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let stamp = |size, mtime| stamp(size, mtime, None, None);
        write_manifest(&root, 1, stamp(10, 1), vec![settled(0, Verdict::Empty)]);
        write_manifest(&root, 2, stamp(20, 2), vec![settled(0, Verdict::Empty)]);
        write_manifest(&root, 3, stamp(30, 3), vec![settled(0, Verdict::Empty)]);
        write_manifest(&root, 4, stamp(40, 4), vec![settled(0, Verdict::Empty)]);
        // A young directory with no manifest yet is a publish in progress.
        std::fs::create_dir_all(file_dir(&root, 5)).expect("in progress");
        // Not this store's shape at all: left alone.
        std::fs::create_dir_all(root.join(".stage-xyz")).expect("stage");

        // 1 is current; 2's row is gone; 3's size moved; 4's mtime moved.
        static KNOWN: [(i64, i64, i64); 4] = [(1, 10, 1), (3, 31, 3), (4, 40, 5), (5, 50, 5)];
        let outcome = sweep_with(&root, None, 256, u64::MAX, rows(&KNOWN))
            .await
            .expect("sweep");
        assert_eq!(outcome.removed, 3, "{outcome:?}");
        assert_eq!(outcome.next, "", "a short store is walked whole and wraps");
        // PR 3: a walk that wraps measures what is left, for the product.
        let footprint = outcome.footprint.expect("measured when the walk wrapped");
        assert_eq!(footprint.directories, 2, "f1 and the young f5 remain");
        assert_eq!(
            footprint.bytes,
            directory_bytes(&file_dir(&root, 1)).await + directory_bytes(&file_dir(&root, 5)).await
        );
        assert!(file_dir(&root, 1).exists(), "a current directory is kept");
        for gone in [2, 3, 4] {
            assert!(!file_dir(&root, gone).exists(), "f{gone} should be gone");
        }
        assert!(file_dir(&root, 5).exists());
        assert!(root.join(".stage-xyz").exists());
    }

    #[tokio::test]
    async fn a_catalog_read_that_fails_stops_the_sweep_without_deleting() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        for id in [1, 2, 3] {
            write_manifest(
                &root,
                id,
                stamp(1, 1, None, None),
                vec![settled(0, Verdict::Empty)],
            );
        }
        let error = sweep_with(&root, None, 256, u64::MAX, |id| async move {
            if id == 2 {
                Err("the catalog is unavailable".to_owned())
            } else {
                Ok(None)
            }
        })
        .await
        .expect_err("a failed read stops the sweep");
        assert!(error.contains("unavailable"), "{error}");
        assert!(
            !file_dir(&root, 1).exists(),
            "work before the failure stands"
        );
        assert!(
            file_dir(&root, 2).exists(),
            "an unreadable row is not evidence that the file is gone"
        );
        assert!(file_dir(&root, 3).exists(), "nothing after it is touched");
    }

    #[tokio::test]
    async fn the_sweep_pages_by_its_own_cursor() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        for id in [3, 10, 200] {
            write_manifest(
                &root,
                id,
                stamp(1, 1, None, None),
                vec![settled(0, Verdict::Empty)],
            );
        }
        static KNOWN: [(i64, i64, i64); 3] = [(3, 1, 1), (10, 1, 1), (200, 1, 1)];
        let first = sweep_with(&root, None, 2, u64::MAX, rows(&KNOWN))
            .await
            .expect("first page");
        assert_eq!(first.next, "f10", "ordered numerically, not by name");
        let second = sweep_with(&root, Some(&first.next), 2, u64::MAX, rows(&KNOWN))
            .await
            .expect("second page");
        assert_eq!(second.next, "", "the walk wraps");
    }

    #[tokio::test]
    async fn over_the_cap_the_least_recently_accessed_directory_goes_first() {
        let base = crate::test_tempdir().expect("store");
        let root = base.path().join(STORE_DIR);
        let mut known = Vec::new();
        for id in [1_i64, 2, 3] {
            let dir = file_dir(&root, id);
            let entry = kept(&dir, 0, &[b'x'; 1000]);
            write_manifest(&root, id, stamp(1, 1, None, None), vec![entry]);
            known.push((id, 1_i64, 1_i64));
        }
        // Access order 2, then 3, then 1: 2 is the oldest.
        for id in [2, 3, 1] {
            record_access(&file_dir(&root, id)).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let one_dir = directory_bytes(&file_dir(&root, 1)).await;
        let known: &'static [(i64, i64, i64)] = Box::leak(known.into_boxed_slice());
        let outcome = sweep_with(&root, None, 256, one_dir * 2 + 1, rows(known))
            .await
            .expect("sweep");
        assert_eq!(outcome.evicted, 1, "{outcome:?}");
        assert!(!file_dir(&root, 2).exists(), "the oldest access is evicted");
        assert!(file_dir(&root, 1).exists());
        assert!(file_dir(&root, 3).exists());

        let outcome = sweep_with(&root, None, 256, one_dir, rows(known))
            .await
            .expect("sweep");
        assert_eq!(outcome.evicted, 1);
        assert!(!file_dir(&root, 3).exists(), "then the next oldest");
        assert!(file_dir(&root, 1).exists(), "the newest survives");
    }
}
