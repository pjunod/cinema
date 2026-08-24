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

// M3 attaches this to the delivery path; nothing outside the tests calls it
// yet. The allow comes out with those callers.
#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};

use plurx_core::fmp4::segment_name;

use crate::titlestore::{Manifest, ReaderWindow, SegState};

/// The initialization segment's name, as every other producer writes it.
pub const INIT_NAME: &str = "init.mp4";

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
        for index in 0..manifest.len() as u32 {
            // Zero length is not a segment. `publish_file` renames a complete
            // file into place, so a zero-length one is the residue of
            // something else — a truncated restore, a filesystem that
            // journalled the rename and not the data — and adopting it
            // publishes an unplayable segment as a cache hit.
            let on_disk = tokio::fs::metadata(self.segment_path(index))
                .await
                .ok()
                .filter(|meta| meta.len() > 0);
            let claimed = manifest
                .state(index)
                .map(|state| state.is_materialized())
                .unwrap_or(false);
            match (on_disk, claimed) {
                (Some(meta), false) => {
                    manifest.materialize(index, meta.len(), at_ms);
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
    pub fn is_complete(&self) -> bool {
        self.error.is_none()
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
