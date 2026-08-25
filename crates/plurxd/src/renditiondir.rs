//! A rendition's segments on disk, and the manifest that says which are there.
//!
//! [`crate::titlestore`] is the bookkeeping — planned, materialized, admitted —
//! and it is pure, so every rule about admission and eviction is testable
//! without touching a disk. This is the other half: the bytes those three
//! facts are about, and the narrow set of operations that keep the two from
//! drifting apart.
//!
//! **The manifest is not the truth; the directory is.** That is the whole
//! discipline here. A row saying a segment exists when it does not is a cache
//! *hit* that 404s a viewer mid-film, which is worse than a miss in every
//! case. So the ordering is fixed, in both directions:
//!
//! - materializing writes the bytes first and records them second, so a crash
//!   between the two leaves a segment on disk that the manifest has not
//!   claimed — invisible, and collected by [`RenditionDir::reconcile`];
//! - evicting clears the record first and unlinks second, so a crash between
//!   the two leaves the same harmless shape rather than a claim on bytes that
//!   are gone.
//!
//! This is [`crate::cachekeep`]'s rule — "deleting the directory before the
//! row, everywhere" — read from the other end: whichever operation is running,
//! the failure mode has to be an orphan byte rather than a phantom row.
//!
//! Names and semantics are exactly [`crate::copyseg`]'s, deliberately:
//! `init.mp4`, `segNNNNN.m4s`, tmp-then-rename so a file is absent or
//! complete and never partial. The serving layer, the segment index and the
//! GC then carry on unchanged, which is the same trick that let the copy
//! segmenter replace ffmpeg's muxer without the rest of the daemon noticing.

use std::io;
use std::path::{Path, PathBuf};

use plurx_core::fmp4::{promote_from, segment_name, Fmp4Error, Init, PromotionInputs};
use sha2::{Digest, Sha256};

use crate::titlestore::{Manifest, ReaderWindow, SegState};

/// The initialization segment's name, as every other producer writes it.
pub const INIT_NAME: &str = "init.mp4";

/// The two digests plan §2.2's identity check compares, and the inputs that
/// connect them.
///
/// Two, not one, because they detect different things and only one of them can
/// be compared across generations at all.
///
/// - `muxer_init` is ffmpeg's raw `ftyp`+`moov`. It is byte-identical across
///   generations including `-ss`-started ones — M0-P0 clause (d), 9/9 — so a
///   mismatch here is real pipeline drift: a different ffmpeg, a different
///   argv, a re-fragmenting upgrade. That is the `producer_failed` case.
/// - `served_init` is what is written to `init.mp4` and what a viewer holds.
///   It is the muxer init plus [`PromotionInputs`], and because those inputs
///   are captured once rather than read from whichever fragment a generation
///   landed on, it is byte-identical across generations *by construction*.
///   Checking it is an assertion, not a test — it cannot fire spuriously, and
///   if it ever does, promotion stopped being a pure function of stored facts.
///
/// Re-promoting and rewriting a stored init is not an option this type offers,
/// deliberately: viewers and already-materialized segments hold the old bytes,
/// and quietly replacing them is the outcome §2.2 exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitIdentity {
    pub muxer_init: String,
    pub served_init: String,
    pub promotion: PromotionInputs,
}

/// Why a generation's init was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitRefused {
    /// The muxer init differs from the rendition's. Real pipeline drift.
    MuxerDrift { stored: String, found: String },
    /// The muxer init matched and the served init did not, which means
    /// promotion is no longer a pure function of the stored inputs. Nothing
    /// should be able to cause this; it is here so that if something does, it
    /// is a loud refusal rather than a viewer receiving different bytes.
    PromotionDrift { stored: String, found: String },
}

impl std::fmt::Display for InitRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitRefused::MuxerDrift { stored, found } => write!(
                f,
                "this generation's muxer init is {found}, and the rendition \
                 was built against {stored} — the video pipeline changed under \
                 a rendition a client already holds a playlist for"
            ),
            InitRefused::PromotionDrift { stored, found } => write!(
                f,
                "promoting the stored inputs produced {found} where the \
                 rendition's served init is {stored}"
            ),
        }
    }
}

impl InitIdentity {
    /// Establish a rendition's identity from its first generation.
    pub fn establish(muxer: &Init, promotion: PromotionInputs) -> Result<InitIdentity, Fmp4Error> {
        let mut served = muxer.clone();
        promote_from(&mut served, &promotion)?;
        Ok(InitIdentity {
            muxer_init: digest(&muxer.bytes),
            served_init: digest(&served.bytes),
            promotion,
        })
    }

    /// Build a later generation's served init, refusing rather than serving
    /// bytes that do not match what the rendition promised.
    ///
    /// This is plan §2.2's check. It runs at generation start, against the
    /// muxer init the pipe just emitted, before a single segment is written.
    pub fn served_init_for(&self, muxer: &Init) -> Result<Init, InitRefused> {
        let found = digest(&muxer.bytes);
        if found != self.muxer_init {
            return Err(InitRefused::MuxerDrift {
                stored: self.muxer_init.clone(),
                found,
            });
        }
        let mut served = muxer.clone();
        // A promotion error here is the same class of answer as a digest
        // mismatch: the stored inputs no longer apply to this init.
        if promote_from(&mut served, &self.promotion).is_err() {
            return Err(InitRefused::PromotionDrift {
                stored: self.served_init.clone(),
                found: "unpromotable".to_owned(),
            });
        }
        let served_digest = digest(&served.bytes);
        if served_digest != self.served_init {
            return Err(InitRefused::PromotionDrift {
                stored: self.served_init.clone(),
                found: served_digest,
            });
        }
        Ok(served)
    }
}

/// The index a rendition-owned segment name carries, `None` for any other
/// file. The inverse of [`plurx_core::fmp4::segment_name`], strict on
/// purpose: reconcile owns the rendition's own names, not the directory.
fn parse_segment_name(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("seg")?.strip_suffix(".m4s")?;
    if digits.len() < 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn digest(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// One rendition's directory.
#[derive(Debug)]
pub struct RenditionDir {
    dir: PathBuf,
}

impl RenditionDir {
    pub fn new(dir: impl Into<PathBuf>) -> RenditionDir {
        RenditionDir { dir: dir.into() }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub async fn create(&self) -> io::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await
    }

    fn segment_path(&self, index: u32) -> PathBuf {
        self.dir.join(segment_name(u64::from(index)))
    }

    /// Write a file so that it is absent or complete and never partial.
    async fn publish_file(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let tmp = self.dir.join(format!("{name}.tmp"));
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, self.dir.join(name)).await
    }

    pub async fn write_init(&self, bytes: &[u8]) -> io::Result<()> {
        self.publish_file(INIT_NAME, bytes).await
    }

    pub async fn has_init(&self) -> bool {
        tokio::fs::metadata(self.dir.join(INIT_NAME)).await.is_ok()
    }

    /// Put one segment on disk and then claim it in the manifest.
    ///
    /// Bytes first. A crash between the two leaves a segment nothing has
    /// claimed, which the next [`RenditionDir::reconcile`] collects; the other
    /// order leaves the manifest promising bytes that are not there, and that
    /// promise is served as a cache hit.
    ///
    /// Refused for an index the plan does not contain, rather than writing a
    /// segment no playlist will ever name.
    pub async fn materialize(
        &self,
        manifest: &mut Manifest,
        index: u32,
        bytes: &[u8],
        at_ms: i64,
    ) -> io::Result<()> {
        if manifest.state(index).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "segment {index} is not in this rendition's plan, which has \
                     {} entries",
                    manifest.len()
                ),
            ));
        }
        self.publish_file(&segment_name(u64::from(index)), bytes)
            .await?;
        manifest.materialize(index, bytes.len() as u64, at_ms);
        Ok(())
    }

    /// Give up one segment's bytes: record first, unlink second.
    ///
    /// Answers `false` when the manifest refused — an admitted rendition never
    /// evicts a member, because admission is the promise that every one of
    /// them is present. Nothing is unlinked in that case, so a refusal cannot
    /// take the bytes with it.
    pub async fn evict(&self, manifest: &mut Manifest, index: u32) -> io::Result<bool> {
        let Some(state) = manifest.state(index) else {
            return Ok(false);
        };
        if !manifest.evict(index) {
            return Ok(false);
        }
        match tokio::fs::remove_file(self.segment_path(index)).await {
            Ok(()) => Ok(true),
            // Already gone is the state eviction wanted. The manifest is now
            // right about it either way.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => {
                // The record is already cleared and the bytes are still there.
                // Put the claim back: the manifest's job is to be right about
                // the disk, and the disk still has them. Leaving it cleared
                // walks the node's accounting down toward zero on a read-only
                // or failing filesystem while the disk stays full — and a
                // scheduler reading that accounting stops asking for room and
                // resumes producing into a disk that has none.
                if let SegState::Materialized { bytes, at_ms } = state {
                    manifest.materialize(index, bytes, at_ms);
                }
                Err(error)
            }
        }
    }

    /// Evict coldest-first until `wanted` bytes are free, never touching a
    /// segment inside a reader's window.
    ///
    /// The choice of which segments to give up is [`Manifest`]'s and stays
    /// there — this only carries it out. Answers the bytes actually freed,
    /// which can be short of `wanted` when every remaining segment is spoken
    /// for; a caller that treats short as failure would evict a reader's own
    /// media to satisfy a budget, which is the trade this refuses to make.
    pub async fn make_room(
        &self,
        manifest: &mut Manifest,
        readers: &[ReaderWindow],
        wanted: u64,
    ) -> io::Result<Freed> {
        let mut freed = Freed::default();
        for index in manifest.eviction_candidates(readers, wanted) {
            let bytes = manifest
                .state(index)
                .map(|state| state.bytes())
                .unwrap_or(0);
            match self.evict(manifest, index).await {
                Ok(true) => freed.bytes += bytes,
                Ok(false) => {}
                // Report what was released rather than losing it to the error.
                // A caller that cannot tell 0 from 400 MB has to assume the
                // worst, and the worst assumption on a full disk is to keep
                // evicting.
                Err(error) => {
                    freed.error = Some(error);
                    return Ok(freed);
                }
            }
        }
        Ok(freed)
    }

    /// Make the manifest agree with the disk, and answer what disagreed.
    ///
    /// Run on adopting a directory this process did not write — a restart, a
    /// resurrected rendition, a producer that died mid-write. Both directions
    /// are repaired, and neither is a surprise:
    ///
    /// - a segment on disk the manifest calls planned is *adopted*, not
    ///   deleted. It cost a read of the source to produce and it is exactly
    ///   what the plan asked for; the alternative is throwing away good bytes
    ///   because a crash landed between two operations.
    /// - a segment the manifest calls materialized that is not on disk is
    ///   *forgotten*. This is the phantom row, and the only safe move is to
    ///   stop claiming it.
    ///
    /// Files that are not planned segments are left alone. This owns the
    /// rendition's own names, not the directory.
    pub async fn reconcile(&self, manifest: &mut Manifest, at_ms: i64) -> io::Result<Reconciled> {
        // A rendition with no init is unplayable whatever else survived: every
        // segment under it decodes with parameter sets that are not there.
        // Saying so is the difference between one clear failure and a client
        // fetching a hundred segments that each produce nothing.
        let mut report = Reconciled {
            init_present: self.has_init().await,
            ..Reconciled::default()
        };
        // One directory listing instead of a `metadata()` per planned index
        // (ruling §4.3, carried to M3): a two-hour film plans thousands of
        // segments, and a restart that paid a blocking-pool round trip for
        // every one of them — nearly all absent on a working-set rendition —
        // was the cost this defers. Sizes come off each entry that actually
        // exists; an absent index costs a map miss.
        let mut on_disk_len: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        let planned = manifest.len() as u64;
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(index) = parse_segment_name(&name.to_string_lossy()) else {
                continue;
            };
            if index >= planned {
                continue;
            }
            if let Ok(meta) = entry.metadata().await {
                on_disk_len.insert(index as u32, meta.len());
            }
        }
        for index in 0..manifest.len() as u32 {
            let claimed_bytes = manifest.state(index).and_then(|state| match state {
                SegState::Materialized { bytes, .. } => Some(bytes),
                SegState::Planned => None,
            });
            // Zero length is not a segment, and neither is a segment whose
            // length disagrees with what the manifest recorded. `publish_file`
            // renames a complete file into place, so both shapes are residue
            // of something else -- a truncated restore, or a filesystem that
            // journalled the rename while the data blocks were still unwritten,
            // which is the power-loss case fsync-at-admission bounds but does
            // not eliminate for un-admitted renditions. Adopting either
            // publishes an unplayable segment as a cache hit.
            //
            // The check costs nothing: the recorded length is already in hand.
            let on_disk = on_disk_len
                .get(&index)
                .copied()
                .filter(|len| *len > 0)
                .filter(|len| claimed_bytes.is_none_or(|bytes| *len == bytes));
            let claimed = claimed_bytes.is_some();
            match (on_disk, claimed) {
                (Some(len), false) => {
                    manifest.materialize(index, len, at_ms);
                    report.adopted.push(index);
                }
                (None, true) => {
                    // Straight to the manifest: there is no file to unlink,
                    // and `evict` refuses on an admitted rendition — which is
                    // exactly the case where a missing member most needs
                    // saying out loud rather than being left as a claim.
                    manifest.forget(index);
                    report.forgotten.push(index);
                }
                _ => {}
            }
        }
        Ok(report)
    }

    /// Delete every planned segment and the init, leaving the directory.
    ///
    /// For a rendition being given up whole. Record-then-unlink is not needed
    /// here because nothing survives to be wrong.
    pub async fn purge(&self, manifest: &mut Manifest) -> Freed {
        let mut freed = Freed::default();
        for index in 0..manifest.len() as u32 {
            let bytes = manifest
                .state(index)
                .map(|state| state.bytes())
                .unwrap_or(0);
            match tokio::fs::remove_file(self.segment_path(index)).await {
                // Only bytes that actually went are bytes that were freed. A
                // caller decrementing a node-wide working set by this number
                // would otherwise over-credit itself by the whole size of a
                // rendition whose directory it could not write to.
                Ok(()) => {
                    manifest.forget(index);
                    freed.bytes += bytes;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    manifest.forget(index);
                }
                Err(error) => freed.error = Some(error),
            }
        }
        if let Err(error) = tokio::fs::remove_file(self.dir.join(INIT_NAME)).await {
            if error.kind() != io::ErrorKind::NotFound && freed.error.is_none() {
                freed.error = Some(error);
            }
        }
        freed
    }
}

/// Bytes actually released, and the error that stopped the sweep if one did.
///
/// One type rather than `io::Result<u64>` because the two facts are both
/// needed: a caller that cannot tell "freed nothing" from "freed 400 MB and
/// then hit EROFS" has to assume the worst, and on a full disk the worst
/// assumption is to keep evicting.
#[derive(Debug, Default)]
pub struct Freed {
    pub bytes: u64,
    pub error: Option<io::Error>,
}

impl Freed {
    /// Exercised by this module's tests; serving reads `bytes` and `error`
    /// directly.
    #[allow(dead_code)]
    pub fn is_complete(&self) -> bool {
        self.error.is_none()
    }
}

impl RenditionDir {
    /// Make every byte of this rendition durable, then let the manifest admit
    /// it.
    ///
    /// Admission is the durability boundary, and the only one. The live path
    /// deliberately does not fsync per segment: a materializing producer runs
    /// at several times realtime, an fsync per segment is a throughput cost
    /// paid on every title, and a power loss before admission re-materializes
    /// honestly — the manifest is in memory, so nothing survives to be wrong.
    ///
    /// Admission is different because `admitted` is a promise that outlives
    /// the process: every member is present, and the rendition may be served
    /// as a cache hit for as long as it is kept. A rename can be journalled
    /// while the data blocks are not, on ext4 for a fresh destination as well
    /// as on XFS and btrfs — so without this, "admitted" can mean a directory
    /// of plausible-length half-segments, served weeks later with nothing left
    /// to notice.
    ///
    /// Files first, then the directory: fsyncing the directory makes the names
    /// durable, and a durable name pointing at unwritten blocks is the exact
    /// failure this is here to prevent.
    ///
    /// Once per rendition, off the hot path, bounded by the member count.
    pub async fn make_durable(&self, manifest: &Manifest) -> io::Result<()> {
        for index in 0..manifest.len() as u32 {
            if !manifest
                .state(index)
                .is_some_and(|state| state.is_materialized())
            {
                continue;
            }
            let file = tokio::fs::File::open(self.segment_path(index)).await?;
            file.sync_all().await?;
        }
        // The init is a member like any other, and a missing one is an error
        // exactly as a missing segment is — not a skip. Every segment under a
        // rendition decodes with the init's parameter sets, so a durably
        // admitted directory without one is a promise of an unplayable
        // rendition, kept for weeks with nothing left to notice.
        let init = tokio::fs::File::open(self.dir.join(INIT_NAME)).await?;
        init.sync_all().await?;
        // The directory entry itself. Opening a directory read-only and
        // syncing it is the portable way to make renames durable.
        let dir = tokio::fs::File::open(&self.dir).await?;
        dir.sync_all().await
    }
}

/// What [`RenditionDir::reconcile`] had to repair.
///
/// Both lists empty is the ordinary case and worth nothing. Either list
/// non-empty is worth a log line: `adopted` means a producer died between
/// writing and recording, `forgotten` means bytes went missing under a live
/// manifest, and the second is the one that would have 404'd a viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub adopted: Vec<u32>,
    pub forgotten: Vec<u32>,
    /// Whether `init.mp4` is there. `false` means the rendition is unplayable
    /// however many segments survived, so it is not a repair — it is a reason
    /// to give the whole thing up and produce it again.
    pub init_present: bool,
}

impl Default for Reconciled {
    fn default() -> Reconciled {
        Reconciled {
            adopted: Vec::new(),
            forgotten: Vec::new(),
            init_present: true,
        }
    }
}

impl Reconciled {
    /// Nothing to repair *and* the rendition is playable.
    #[allow(dead_code)] // test-facing; adoption reads the fields directly
    pub fn is_clean(&self) -> bool {
        self.adopted.is_empty() && self.forgotten.is_empty() && self.init_present
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use plurx_core::fmp4::CutClass;
    use plurx_core::fmp4::CutPolicy;
    use plurx_core::segplan::{plan_copy, FragmentIndex, IndexRow, SourceIdentity, TrackDurations};

    use crate::titlestore::Budgets;

    fn manifest(fragments: usize) -> Manifest {
        let mut rows = Vec::new();
        let mut dts = 0u64;
        for i in 0..fragments {
            let duration = if i % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes: 104_452,
                video_bytes: 103_836,
                class: if i % 3 == 0 {
                    CutClass::CleanIdr
                } else {
                    CutClass::Dirty
                },
            });
            dts += duration;
        }
        let index = FragmentIndex::new(
            16_000,
            rows,
            "sha",
            SourceIdentity::new(1, 1, "fingerprint"),
        );
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let ms = fragments as i64 * 1_751;
        Manifest::new(plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        ))
    }

    async fn dir() -> (tempfile::TempDir, RenditionDir) {
        let temp = tempfile::tempdir().expect("tempdir");
        let rendition = RenditionDir::new(temp.path().join("rendition"));
        rendition.create().await.expect("create");
        (temp, rendition)
    }

    #[tokio::test]
    async fn a_segment_is_on_disk_before_the_manifest_claims_it() {
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition
            .materialize(&mut manifest, 0, b"segment bytes", 10)
            .await
            .expect("materialize");

        assert_eq!(
            tokio::fs::read(rendition.path().join(segment_name(0)))
                .await
                .expect("the segment is on disk"),
            b"segment bytes"
        );
        assert!(manifest.state(0).expect("planned").is_materialized());
        assert_eq!(manifest.materialized_bytes(), 13);
    }

    #[tokio::test]
    async fn nothing_partial_is_ever_left_behind() {
        // The serving layer lists this directory. A `.tmp` that survives is a
        // file it can be asked for.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        rendition
            .materialize(&mut manifest, 0, b"bytes", 10)
            .await
            .expect("materialize");

        let mut entries = tokio::fs::read_dir(rendition.path())
            .await
            .expect("listing");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".tmp"), "{name} survived");
        }
    }

    #[tokio::test]
    async fn an_index_the_plan_does_not_contain_is_refused_before_anything_is_written() {
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        let past_the_end = manifest.len() as u32;
        let error = rendition
            .materialize(&mut manifest, past_the_end, b"bytes", 10)
            .await
            .expect_err("an unplanned segment is refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            !rendition
                .path()
                .join(segment_name(u64::from(past_the_end)))
                .exists(),
            "a refused segment must not leave bytes behind"
        );
    }

    #[tokio::test]
    async fn eviction_clears_the_record_before_it_unlinks() {
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition
            .materialize(&mut manifest, 1, b"bytes", 10)
            .await
            .expect("materialize");

        assert!(rendition.evict(&mut manifest, 1).await.expect("evict"));
        assert!(!manifest.state(1).expect("planned").is_materialized());
        assert!(!rendition.path().join(segment_name(1)).exists());
    }

    #[tokio::test]
    async fn an_admitted_rendition_keeps_its_bytes_when_eviction_is_refused() {
        // Admission is the promise that every member is present. A refusal
        // that unlinked anyway would break exactly the promise it is enforcing.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(6);
        for index in 0..manifest.len() as u32 {
            rendition
                .materialize(&mut manifest, index, b"bytes", 10)
                .await
                .expect("materialize");
        }
        let budgets = Budgets {
            working_set_bytes: 1 << 30,
            completed_cache_bytes: 1 << 40,
            admission_share: 1.0,
        };
        manifest.reserve(&budgets).expect("reserve");
        manifest.complete(&budgets).expect("complete");
        assert!(manifest.is_admitted());

        assert!(!rendition.evict(&mut manifest, 0).await.expect("evict"));
        assert!(
            rendition.path().join(segment_name(0)).exists(),
            "a refused eviction must leave the bytes alone"
        );
    }

    #[tokio::test]
    async fn making_room_never_takes_a_reader_s_own_media() {
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(48);
        let count = manifest.len() as u32;
        for index in 0..count {
            rendition
                .materialize(&mut manifest, index, b"0123456789", i64::from(index))
                .await
                .expect("materialize");
        }
        let reader = ReaderWindow {
            back: 1,
            playhead: 3,
            frontier: 4,
            ahead: 1,
        };
        let before = manifest.materialized_bytes();
        let freed = rendition
            .make_room(&mut manifest, &[reader], before)
            .await
            .expect("make room");

        assert!(freed.is_complete(), "the sweep hit an error");
        assert!(freed.bytes > 0, "something must have been given up");
        for index in 0..count {
            if reader.covers(index) {
                assert!(
                    manifest.state(index).expect("planned").is_materialized(),
                    "segment {index} is inside the reader's window"
                );
                assert!(rendition
                    .path()
                    .join(segment_name(u64::from(index)))
                    .exists());
            }
        }
    }

    #[tokio::test]
    async fn a_segment_written_but_never_recorded_is_adopted_rather_than_deleted() {
        // The crash between write and record. Those bytes cost a read of the
        // source and are exactly what the plan asked for.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        tokio::fs::write(rendition.path().join(segment_name(2)), b"orphaned")
            .await
            .expect("write behind the manifest's back");

        let report = rendition
            .reconcile(&mut manifest, 99)
            .await
            .expect("reconcile");
        assert_eq!(report.adopted, vec![2]);
        assert!(report.forgotten.is_empty());
        assert_eq!(manifest.state(2).expect("planned").bytes(), 8);
    }

    #[tokio::test]
    async fn a_claim_on_bytes_that_are_gone_is_forgotten() {
        // The phantom row: a manifest hit that 404s a viewer mid-film.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition
            .materialize(&mut manifest, 3, b"bytes", 10)
            .await
            .expect("materialize");
        tokio::fs::remove_file(rendition.path().join(segment_name(3)))
            .await
            .expect("something else took the file");

        let report = rendition
            .reconcile(&mut manifest, 99)
            .await
            .expect("reconcile");
        assert_eq!(report.forgotten, vec![3]);
        assert!(!manifest.state(3).expect("planned").is_materialized());
    }

    #[tokio::test]
    async fn an_admitted_rendition_that_lost_a_member_still_reports_it() {
        // `evict` refuses on an admitted rendition, and rightly. Reconcile
        // must not inherit that refusal: a member that is really gone has to
        // stop being claimed, and this is the case where saying so matters
        // most.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(6);
        for index in 0..manifest.len() as u32 {
            rendition
                .materialize(&mut manifest, index, b"bytes", 10)
                .await
                .expect("materialize");
        }
        let budgets = Budgets {
            working_set_bytes: 1 << 30,
            completed_cache_bytes: 1 << 40,
            admission_share: 1.0,
        };
        manifest.reserve(&budgets).expect("reserve");
        manifest.complete(&budgets).expect("complete");
        tokio::fs::remove_file(rendition.path().join(segment_name(1)))
            .await
            .expect("a member goes missing under an admitted rendition");

        let report = rendition
            .reconcile(&mut manifest, 99)
            .await
            .expect("reconcile");
        assert_eq!(report.forgotten, vec![1]);
        assert!(!manifest.state(1).expect("planned").is_materialized());
    }

    // ---- durability ------------------------------------------------------

    #[tokio::test]
    async fn a_segment_whose_length_disagrees_with_the_record_is_not_adopted() {
        // The torn write a zero-length check cannot see. `publish_file`
        // renames a complete file into place, so a member that is on disk at
        // the wrong length is residue -- and adopting it publishes an
        // unplayable segment as a cache hit. The recorded length is already in
        // hand, so the check is free.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        rendition
            .materialize(&mut manifest, 0, b"0123456789", 1)
            .await
            .expect("materialize");

        // Truncate it behind the manifest's back.
        tokio::fs::write(rendition.path().join(segment_name(0)), b"012")
            .await
            .expect("truncate");

        let report = rendition
            .reconcile(&mut manifest, 2)
            .await
            .expect("reconcile");
        assert_eq!(report.forgotten, vec![0], "{report:?}");
        assert!(
            !manifest.state(0).expect("state").is_materialized(),
            "a claim on bytes that are not the bytes recorded is still a \
             phantom row"
        );
    }

    #[tokio::test]
    async fn an_adopted_segment_nothing_claimed_keeps_its_own_length() {
        // The other side of the same check: an unclaimed file has no recorded
        // length to disagree with, so it is adopted at whatever length it has.
        // Refusing it would throw away bytes that cost a read of the source.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        tokio::fs::write(rendition.path().join(segment_name(3)), b"0123456789")
            .await
            .expect("orphan");

        let report = rendition
            .reconcile(&mut manifest, 7)
            .await
            .expect("reconcile");
        assert_eq!(report.adopted, vec![3]);
        assert_eq!(manifest.state(3).expect("state").bytes(), 10);
    }

    #[tokio::test]
    async fn making_a_rendition_durable_covers_every_member_and_the_init() {
        // Admission is the durability boundary, so this must not quietly skip
        // a member. It is also the only fsync on the path, so it must not
        // error on a rendition with holes -- an un-admitted one is allowed to
        // have them.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        for index in [0u32, 1, 3] {
            rendition
                .materialize(&mut manifest, index, b"bytes", i64::from(index))
                .await
                .expect("materialize");
        }
        rendition
            .make_durable(&manifest)
            .await
            .expect("a rendition with holes is still syncable");
    }

    #[tokio::test]
    async fn making_a_rendition_durable_fails_loudly_when_a_member_is_missing() {
        // Better a refused admission than one that promises presence for a
        // member nothing can open.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        rendition
            .materialize(&mut manifest, 0, b"bytes", 1)
            .await
            .expect("materialize");
        tokio::fs::remove_file(rendition.path().join(segment_name(0)))
            .await
            .expect("remove");
        assert!(rendition.make_durable(&manifest).await.is_err());
    }

    #[tokio::test]
    async fn making_a_rendition_durable_fails_loudly_when_the_init_is_missing() {
        // The same rule as a missing member, for a sharper reason: every
        // segment decodes with the init's parameter sets, so admitting a
        // directory without one durably promises an unplayable rendition.
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition
            .materialize(&mut manifest, 0, b"bytes", 1)
            .await
            .expect("materialize");
        assert!(
            rendition.make_durable(&manifest).await.is_err(),
            "a missing init must refuse durability, not skip it"
        );
    }

    // ---- init identity (plan §2.2) ---------------------------------------

    /// Two muxer inits standing in for two generations of one rendition.
    ///
    /// The surgery promotion performs is plurx-core's to test, and it does
    /// (`promoting_from_captured_inputs_does_not_depend_on_the_landing_fragment`).
    /// What is under test here is the identity check itself — which digest is
    /// compared, and what is refused — so an init promotion no-ops on is the
    /// right fixture: it isolates the comparison from the surgery.
    fn two_generations() -> (Init, Init) {
        (
            Init {
                bytes: b"ftypmoov-generation-one".to_vec(),
                tracks: Vec::new(),
            },
            Init {
                bytes: b"ftypmoov-after-an-ffmpeg-upgrade".to_vec(),
                tracks: Vec::new(),
            },
        )
    }

    #[test]
    fn a_second_generation_serves_the_same_bytes_as_the_first() {
        let (first, _) = two_generations();
        let identity = InitIdentity::establish(&first, PromotionInputs::default()).expect("first");

        // A later generation emits the same muxer init -- M0 measured that
        // 9/9, including a seeked generation -- and gets the same served init.
        let served = identity.served_init_for(&first).expect("second generation");
        assert_eq!(digest(&served.bytes), identity.served_init);
    }

    #[test]
    fn a_changed_video_pipeline_is_refused_before_a_segment_is_written() {
        // The `producer_failed` case, and the one this check is really for: a
        // different ffmpeg or a different argv under a rendition whose
        // playlist a client already holds.
        let (first, drifted) = two_generations();
        let identity = InitIdentity::establish(&first, PromotionInputs::default()).expect("first");

        match identity.served_init_for(&drifted) {
            Err(InitRefused::MuxerDrift { stored, found }) => {
                assert_eq!(stored, identity.muxer_init);
                assert_ne!(found, stored);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_two_digests_are_not_the_same_fact() {
        // If promotion does something, the served init is not the muxer init,
        // and storing one digest would mean checking the wrong artifact --
        // which is exactly what M0-P0 clause (d) turned out to have measured.
        let (first, _) = two_generations();
        let promoting = PromotionInputs {
            parameter_sets: vec![vec![0x40, 0x01, 0x0c]],
            hdr10_sei: Vec::new(),
        };
        // This init has no hvcC, so promotion no-ops and the two agree...
        let identity = InitIdentity::establish(&first, promoting).expect("establish");
        assert_eq!(identity.muxer_init, identity.served_init);

        // ...which is the honest outcome for a source promotion does nothing
        // to, and is why the digests are stored separately rather than one
        // being derived from the other at read time.
        assert_eq!(identity.muxer_init, digest(&first.bytes));
    }

    #[tokio::test]
    async fn reconciling_a_directory_that_agrees_changes_nothing() {
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        for index in 0..4 {
            rendition
                .materialize(&mut manifest, index, b"bytes", 10)
                .await
                .expect("materialize");
        }
        let before = manifest.clone();
        let report = rendition
            .reconcile(&mut manifest, 99)
            .await
            .expect("reconcile");
        assert!(report.is_clean());
        assert_eq!(
            manifest.materialized_bytes(),
            before.materialized_bytes(),
            "a clean reconcile must not move a byte"
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_planned_segment_is_left_alone() {
        let (_temp, rendition) = dir().await;
        let mut manifest = manifest(24);
        rendition.write_init(b"moov").await.expect("init");
        tokio::fs::write(rendition.path().join("notes.txt"), b"hello")
            .await
            .expect("a file this module does not own");

        rendition
            .reconcile(&mut manifest, 99)
            .await
            .expect("reconcile");
        assert!(rendition.path().join("notes.txt").exists());
        assert!(rendition.has_init().await);
    }
}
