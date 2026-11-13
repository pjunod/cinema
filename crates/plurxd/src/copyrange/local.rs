//! A growing copy range owned by a local user's own playback.
//!
//! This is the range half of a local VOD session: the same census, mux,
//! verified kernel, storage and read bank the shared (Source) range runs,
//! opened and paid for the way a local playback opens and pays for its
//! source today.
//!
//! - **Input admission.** A range registers one foreground custody, keyed by
//!   the local playback: the session is the request's lifetime, as it is for
//!   a rolling local session, and the policy is `Rolling`. The head census
//!   reads through an Input lent that custody; the primary generation's
//!   Input takes it over once the census has joined its own. The store holds
//!   one `foreground_source_owners` row for the range from its start to its
//!   end, written once, as a rolling start's is: the playback's one read is
//!   charged once.
//! - **Pressure.** The read bank tells the source domain about its viewer by
//!   the rolling rule (`rolling_source_observation`): watermarks from the
//!   demand's rate, lead from its estimated position, and a viewer fresh
//!   while its last accepted demand is at most 5 s old and its startup is
//!   not protected, whatever it renders. A paused or buffering viewer that
//!   is still reporting lifts pressure while the range is held, as it does
//!   for a rolling session.
//! - **No copy admission.** A local copy takes no encoder-pool permit today,
//!   neither as a rolling session nor as a plan-verified VOD rendition, and
//!   a local range takes none either. A permit here could queue or refuse a
//!   local copy start that never queues today, and would hold background
//!   work back for a pipeline that was not counted against it yesterday.
//!   Shared playback takes one because a remote grant's work is charged to
//!   the origin's governor; a local user's own copy never was.
//! - **No lease, no loop, no timer of its own.** The range is live for as
//!   long as the session that owns it says so. The session's accepted
//!   control exchanges reach [`LocalCopyRange::observe_control`], its
//!   existing maintenance reaches [`LocalCopyRange::refresh`], and its
//!   retirement reaches [`LocalCopyRange::settle`], which joins the child,
//!   every pipe task and the Input before it removes the payload. A range
//!   dropped unsettled is retired: its run task still reaps the child and
//!   closes the Input, and only its files are left, for the next boot.
//!
//! Nothing here is served. Attaching a session to a range is a later step.
use super::{
    run_source_census, spawn_source_generation, CopyClockCensus, CopyGenerationOwner,
    CopyRangeFiles, CopyRangeOwner, CopyRangeProduction, ObservedCopyMux,
};
use crate::{
    copy_analysis::{CopyRecipe, LocalPlaybackOwner},
    copy_normalization::CopyNormalizationContract,
    fragment_index_cluster::SourceFence,
    prodrun::ProducerSlot,
    source_custody::{SourceCustody, SourceIdentity, SourceRequest},
    source_generation::SourceRenditionOwners,
    source_input::SourceInput,
};
use plurx_core::{
    domain::MediaFile,
    store::{background_jobs_resources::ForegroundSourcePolicy, Store},
};
use std::{path::Path, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

/// The foreground source request a local range registers its one custody
/// under. Its lifetime is the session, its incarnation is this range's own
/// and its policy is `Rolling`: the keys a rolling local session of the same
/// file registers, so the source domain sees the playback it has always
/// seen.
pub(crate) fn local_range_source_request(
    identity: &SourceIdentity,
    store: Arc<dyn Store>,
    file: &MediaFile,
    session: &str,
) -> SourceRequest {
    identity.request(
        store,
        file,
        session,
        uuid::Uuid::new_v4(),
        ForegroundSourcePolicy::Rolling,
    )
}

/// The producer attempt a local range's one custody row names.
const RANGE_ATTEMPT: &str = "local-copy-range";

/// Everything a local VOD session hands over to start its range.
pub(crate) struct LocalCopyRangeStart<'a> {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) identity: &'a SourceIdentity,
    pub(crate) file: Arc<MediaFile>,
    pub(crate) recipe: CopyRecipe,
    /// Where the playback starts, in film seconds.
    pub(crate) start_seconds: f64,
    /// The local VOD session the range is the rendition of.
    pub(crate) session: &'a str,
    pub(crate) owner: LocalPlaybackOwner,
    /// The session rendition's dispatch ledger, with no Source viewer.
    pub(crate) ledger: Arc<SourceRenditionOwners>,
    pub(crate) work_dir: &'a Path,
    pub(crate) observation_root: &'a Path,
    pub(crate) runtime_cache: &'a Path,
    pub(crate) catalog_namespace: String,
    pub(crate) node_id: &'a str,
    /// The session's startup deadline, which bounds the head census.
    pub(crate) deadline: Instant,
    /// Cancelled when the session retires while the range is starting.
    pub(crate) stop: &'a CancellationToken,
}

/// A range between its head census and its primary generation: its one
/// custody, the object it holds and what the census established of it.
struct LocalRangeHead {
    custody: Arc<SourceCustody>,
    source: SourceFence,
    census: Arc<CopyClockCensus>,
}

/// A range's files while its start has not handed them to a production. A
/// start abandoned there (its future dropped) removes them on the runtime;
/// a start that fails removes them itself and awaits it.
struct UnstartedFiles(Option<CopyRangeFiles>);
impl UnstartedFiles {
    async fn retire(mut self) {
        if let Some(mut files) = self.0.take() {
            if let Err(error) = files.retire_payload().await {
                tracing::warn!(%error, "an unstarted local copy range's files were not removed");
            }
        }
    }
    fn into_files(mut self) -> CopyRangeFiles {
        self.0.take().expect("unstarted range files")
    }
}
impl Drop for UnstartedFiles {
    fn drop(&mut self) {
        let Some(mut files) = self.0.take() else {
            return;
        };
        // Nothing was produced into them and nothing else holds them.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Err(error) = files.retire_payload().await {
                        tracing::warn!(
                            %error,
                            "an abandoned local copy range's files were not removed"
                        );
                    }
                });
            }
            // No runtime is left to remove them: the next boot does.
            Err(_) => drop(files),
        }
    }
}

/// A started local range. Retained by its session until [`Self::settle`].
pub(crate) struct LocalCopyRange {
    production: Arc<CopyRangeProduction>,
    source: SourceFence,
    file: Arc<MediaFile>,
}

// Constructed by tests until a local VOD session attaches a range.
#[cfg_attr(
    not(all(test, target_os = "linux")),
    expect(dead_code, reason = "no local VOD session starts a range yet")
)]
impl LocalCopyRange {
    /// Run the head census and start the primary generation, both under the
    /// local playback's one foreground custody. Every refusal is the
    /// engine's own typed one; the caller serves that start another way. A
    /// range whose start was stopped once its generation ran is returned
    /// retired, to be settled as any range is.
    pub(crate) async fn start(start: LocalCopyRangeStart<'_>) -> Result<Arc<Self>, String> {
        let head = Self::census(&start).await?;
        Self::primary(start, head).await
    }

    /// Register the range's one custody and run the head census through an
    /// Input lent it. The census joins that Input and its readers before it
    /// returns; the custody and its one row stay.
    async fn census(start: &LocalCopyRangeStart<'_>) -> Result<LocalRangeHead, String> {
        let file = &start.file;
        let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
        let custody = Arc::new(
            local_range_source_request(
                start.identity,
                Arc::clone(&start.store),
                file,
                start.session,
            )
            .object_version(source.object_version())
            .register(RANGE_ATTEMPT)
            .await?,
        );
        let input =
            SourceInput::open_foreground_lent(&custody, source.reopen(file).await?, &file.path)
                .await?;
        let dispatch = match start.ledger.begin_generation() {
            Ok(dispatch) => dispatch,
            Err(error) => {
                let _ = input.close_and_wait().await;
                return Err(error.into());
            }
        };
        let census = Arc::new(
            run_source_census(
                input,
                source.object_version().to_owned(),
                file,
                start.recipe.video,
                CopyGenerationOwner::Local {
                    owners: &start.ledger,
                    dispatch,
                    viewer: &start.owner,
                },
                Arc::new(ProducerSlot::new()),
                start.runtime_cache,
                Some(start.deadline),
                start.stop,
            )
            .await?,
        );
        if census.format_start.is_none() {
            return Err("copy_clock_unknown".into());
        }
        // Every later generation reads this same held object; a census of a
        // different witness cannot establish its clock or normalization.
        if census.source_object_version != source.object_version() {
            return Err("copy_source_changed".into());
        }
        Ok(LocalRangeHead {
            custody,
            source,
            census,
        })
    }

    /// Make the range's files and start its primary generation through an
    /// Input that takes the custody over.
    async fn primary(
        start: LocalCopyRangeStart<'_>,
        head: LocalRangeHead,
    ) -> Result<Arc<Self>, String> {
        let LocalCopyRangeStart {
            file,
            recipe,
            start_seconds,
            session,
            owner,
            ledger,
            work_dir,
            observation_root,
            runtime_cache,
            catalog_namespace,
            node_id,
            stop,
            ..
        } = start;
        let LocalRangeHead {
            custody,
            source,
            census,
        } = head;
        let video = recipe.video;
        let contract = Arc::new(
            CopyNormalizationContract::from_head(
                &file,
                video,
                &census.raw_canonical_init,
                &census.raw_canonical_fragment,
            )
            .map_err(|error| error.to_string())?,
        );
        let observation_source = crate::copy_analysis::source_for_copy(
            &catalog_namespace,
            &file,
            &crate::fragindex::identity_for(&file, video).argv_fingerprint,
            source.object_version(),
            node_id,
        )
        .map_err(|error| error.to_string())?;
        let mux = ObservedCopyMux::new(
            Arc::clone(&contract),
            census.first_dts,
            0,
            recipe.tracks(&file),
            video.retains_hevc_parameter_sets(),
            file.video_codec.as_deref() == Some("hevc"),
        )?;
        let args = recipe.args(&file, start_seconds)?;
        let files = UnstartedFiles(Some(
            CopyRangeFiles::create(
                work_dir,
                observation_root,
                catalog_namespace,
                observation_source,
                CopyRangeOwner::Local {
                    session: session.to_owned(),
                    viewer: owner.clone(),
                },
                uuid::Uuid::new_v4(),
                1,
                census.first_dts,
                0,
                contract.init_contract_sha256().to_owned(),
                contract.promotion().clone(),
            )
            .await?,
        ));
        // The census Input and every reader are joined, so the custody is
        // the range's alone again. Reopen independently against the exact
        // object before the primary operation gets a URL.
        let primary = async {
            if stop.is_cancelled() {
                return Err("copy_range_cancelled".to_owned());
            }
            let custody =
                Arc::try_unwrap(custody).map_err(|_| "copy_source_custody_shared".to_owned())?;
            let input = SourceInput::open_foreground_with_custody(
                custody,
                source.reopen(&file).await?,
                &file.path,
            )
            .await?;
            let seek_bootstrap =
                match input.seal_seek_bootstrap_limit(super::SOURCE_SEEK_BOOTSTRAP_BYTES) {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _ = input.close_and_wait().await;
                        return Err(error);
                    }
                };
            let dispatch = match ledger.begin_generation() {
                Ok(dispatch) => dispatch,
                Err(error) => {
                    let _ = input.close_and_wait().await;
                    return Err(error.to_owned());
                }
            };
            let observer = input.observer();
            let running = spawn_source_generation(
                input,
                &file.path,
                &args,
                runtime_cache,
                CopyGenerationOwner::Local {
                    owners: &ledger,
                    dispatch,
                    viewer: &owner,
                },
                Arc::new(ProducerSlot::new()),
            )
            .await?;
            Ok((running, observer, seek_bootstrap))
        }
        .await;
        let (running, observer, seek_bootstrap) = match primary {
            Ok(started) => started,
            Err(error) => {
                // Nothing was produced: leave no payload or manifest behind.
                files.retire().await;
                return Err(error);
            }
        };
        let production = CopyRangeProduction::new(
            files.into_files(),
            observer,
            (start_seconds * 1000.0).round().max(0.0) as i64,
            crate::transcode::rolling_source_watermarks(1.0),
            seek_bootstrap,
            super::production::ReadBankRule::Rolling,
        );
        production.run_generation(running, mux, census, 0);
        let range = Arc::new(Self {
            production,
            source,
            file,
        });
        // A session that retired while its range started still owns what
        // started: stopped here, the range is settled as any range is.
        if stop.is_cancelled() {
            range.retire();
        }
        Ok(range)
    }

    /// Live until retired or failed, and only over the object it started on.
    pub(crate) fn is_live(&self) -> bool {
        self.production.is_live() && self.source.unchanged()
    }

    /// The path, independently reopened and checked against this range's
    /// held object, so a replacement cannot authorize bytes cut from the old
    /// one.
    pub(crate) async fn physical_fence(&self) -> Result<SourceFence, String> {
        if !self.is_live() {
            return Err("copy_range_retired".into());
        }
        self.source.reopen(&self.file).await
    }

    /// One newly accepted control exchange of the owning session: where the
    /// viewer is, or is seeking to, and at what rate, and whether the
    /// session's startup is protected from a time hold (its lease's
    /// `startup.protects_from_time_hold()`). The read bank holds or refills
    /// the source against it and presses as a rolling session does.
    pub(crate) async fn observe_control(
        &self,
        demand: crate::playback_control::PlaybackDemandSnapshot,
        startup_protected: bool,
    ) -> Result<(), String> {
        self.production
            .observe_rolling_demand(demand, startup_protected)
            .await
    }

    /// Re-read the bank against the last accepted demand as it has aged and
    /// the session's startup protection now stands. The session's existing
    /// maintenance calls this; the range keeps no clock.
    pub(crate) async fn refresh(&self, startup_protected: bool) -> Result<(), String> {
        self.production
            .refresh_rolling_read_bank(startup_protected)
            .await
    }

    /// The session's accepted rendered report.
    pub(crate) async fn mark_rendered(
        &self,
    ) -> Result<Option<Arc<crate::copy_analysis::CopyRangeCheckpoint>>, String> {
        self.production.mark_rendered().await
    }

    /// Stop producing. The session calls this the moment it retires;
    /// [`Self::settle`] joins what it stopped.
    pub(crate) fn retire(&self) {
        self.production.retire();
    }

    /// The session's retirement: stop, join the child, every pipe task and
    /// the Input (which releases the foreground custody), then remove the
    /// payload, the manifests and the session's payload directory once it is
    /// empty. Returns how the range itself ended or, if it ended well, how
    /// its files went; a removal that fails beside a failed range is logged.
    pub(crate) async fn settle(&self) -> Result<(), String> {
        self.retire();
        let outcome = match self.production.wait_stopped().await {
            // A range its session retired was cancelled, not failed.
            Err(error) if error == "copy_range_cancelled" => Ok(()),
            outcome => outcome,
        };
        let (retired, owner_directory) = {
            let mut files = self.production.files.lock().await;
            (
                files.retire_payload().await,
                files.payload_path.parent().map(Path::to_owned),
            )
        };
        if let (Ok(()), Some(directory)) = (&retired, owner_directory) {
            remove_empty_owner_directory(&directory).await;
        }
        match (outcome, retired) {
            (Ok(()), retired) => retired,
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(retirement)) => {
                tracing::warn!(
                    range_error = %error,
                    retirement_error = %retirement,
                    "a failed local copy range's files were not removed"
                );
                Err(error)
            }
        }
    }
}

/// A local range dropped unsettled stops: its run task reaps the child and
/// closes the Input, which releases the custody. Its files stay for the
/// next boot.
impl Drop for LocalCopyRange {
    fn drop(&mut self) {
        self.production.retire();
    }
}

/// Remove `local-copy-ranges/<session>` once its last range is gone. A
/// directory another range of the session has a child in stays.
async fn remove_empty_owner_directory(directory: &Path) {
    let (Some(parent), Some(name)) = (
        directory.parent(),
        directory.file_name().and_then(|name| name.to_str()),
    ) else {
        return;
    };
    match plurx_core::fs_secure::remove_empty_directory_child(parent, name).await {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => {
            tracing::warn!(%error, "a local copy range's session directory was not removed");
        }
    }
}

/// The manifest directories of copy ranges under the data directory: the
/// shared (Source) owner's and the local owner's.
const RANGE_MANIFESTS: [&str; 2] = ["observations", "local-observations"];

/// Remove the checkpoint manifests copy ranges of a previous process left
/// under the data directory, for both owners.
///
/// A range lives and dies with its process. Nothing constructs one but
/// `CopyRangeFiles::create`, nothing reads a checkpoint manifest back, and
/// its payload is in the transcode scratch every boot clears, so no range
/// is ever resumed: after a restart every manifest directory is an orphan
/// of a range that is gone. Orderly retirement removes a range's own; a
/// killed process removes nothing, and this is what does. Called once at
/// boot, beside that scratch clear and under the same daemon lock, before
/// anything can create a range, so a live range's manifests are never here
/// to be taken. Both directories are attempted; the first failure is
/// returned.
pub(crate) fn reap_range_manifests(data_dir: &Path) -> std::io::Result<()> {
    let mut outcome = Ok(());
    for owner in RANGE_MANIFESTS {
        let manifests = data_dir.join("copy-ranges").join(owner);
        let reaped = match std::fs::symlink_metadata(&manifests) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            // Never follow a link someone put where the directory belongs.
            Ok(metadata) if !metadata.is_dir() => Err(std::io::Error::other(
                "copy range manifests are not a directory",
            )),
            Ok(_) => plurx_core::fs_secure::clear_scratch_blocking(&manifests),
        };
        if outcome.is_ok() {
            outcome = reaped;
        }
    }
    outcome
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
