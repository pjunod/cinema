//! A growing copy range owned by a local user's own playback.
//!
//! This is the range half of a local VOD session: the same census, mux,
//! verified kernel, storage and read bank the shared (Source) range runs,
//! opened and paid for the way a local playback opens and pays for its
//! source today.
//!
//! - **Input admission.** Every Input of the range is opened under one
//!   foreground source request keyed by the local playback: the session is
//!   the request's lifetime, as it is for a rolling local session, and the
//!   policy is `Rolling`. The head census and the primary generation run one
//!   after the other and each Input registers its own custody, so the store
//!   holds at most one `foreground_source_owners` row for the session at any
//!   time: the playback's one read is charged once.
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
//!   every pipe task and the Input before it removes the payload.
//!
//! Nothing here is served. Attaching a session to a range is a later step.
use super::{
    run_source_census, spawn_source_generation, CopyGenerationOwner, CopyRangeFiles,
    CopyRangeOwner, CopyRangeProduction, ObservedCopyMux,
};
use crate::{
    copy_analysis::{CopyRecipe, LocalPlaybackOwner},
    copy_normalization::CopyNormalizationContract,
    fragment_index_cluster::SourceFence,
    prodrun::ProducerSlot,
    source_custody::{SourceIdentity, SourceRequest},
    source_generation::SourceRenditionOwners,
    source_input::SourceInput,
};
use plurx_core::{
    domain::MediaFile,
    store::{background_jobs_resources::ForegroundSourcePolicy, Store},
};
use std::{path::Path, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

/// The foreground source request every Input of a local range is opened
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

/// The read bank's watermarks for a playback rate: the ones a rolling local
/// session and the shared range are held to.
fn watermarks(playback_rate: f64) -> (u64, u64) {
    crate::transcode::rolling_source_watermarks(playback_rate.clamp(0.25, 4.0))
}

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
    /// local playback's own foreground request. Every refusal is the
    /// engine's own typed one; the caller serves that start another way.
    pub(crate) async fn start(start: LocalCopyRangeStart<'_>) -> Result<Arc<Self>, String> {
        let LocalCopyRangeStart {
            store,
            identity,
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
            deadline,
            stop,
        } = start;
        let video = recipe.video;
        let source = crate::fragment_index_cluster::open_source_playback_fence(&file, None).await?;
        let request = local_range_source_request(identity, store, &file, session);
        let local_owner = |dispatch| CopyGenerationOwner::Local {
            owners: &ledger,
            dispatch,
            viewer: &owner,
        };
        let input = SourceInput::open_foreground_held(
            request.clone(),
            source.reopen(&file).await?,
            "local-copy-head",
        )
        .await?;
        let dispatch = match ledger.begin_generation() {
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
                &file,
                video,
                local_owner(dispatch),
                Arc::new(ProducerSlot::new()),
                runtime_cache,
                Some(deadline),
                stop,
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
        let mut files = CopyRangeFiles::create(
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
        .await?;
        // The census Input and every reader are joined. Reopen independently
        // against the exact object before the primary operation gets a URL.
        let primary = async {
            if stop.is_cancelled() {
                return Err("copy_range_cancelled".to_owned());
            }
            let input = SourceInput::open_foreground_held(
                request,
                source.reopen(&file).await?,
                "local-copy-primary",
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
                local_owner(dispatch),
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
                let _ = files.retire_payload().await;
                return Err(error);
            }
        };
        let production = CopyRangeProduction::start(
            files,
            running,
            mux,
            census,
            0,
            observer,
            (start_seconds * 1000.0).round().max(0.0) as i64,
            watermarks(1.0),
            seek_bootstrap,
        );
        Ok(Arc::new(Self {
            production,
            source,
            file,
        }))
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
    /// viewer is, or is seeking to, and at what rate. The read bank holds or
    /// refills the source against it, as the shared range's does.
    pub(crate) async fn observe_control(
        &self,
        demand: crate::playback_control::PlaybackDemandSnapshot,
    ) -> Result<(), String> {
        let watermarks = watermarks(demand.playback_rate);
        self.production.observe_demand(demand, watermarks).await
    }

    /// Re-read the bank against the last accepted demand as it has aged. The
    /// session's existing maintenance calls this; the range keeps no clock.
    pub(crate) async fn refresh(&self) -> Result<(), String> {
        self.production.refresh_read_bank().await
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
    /// payload and the manifests. Returns how the range itself ended.
    pub(crate) async fn settle(&self) -> Result<(), String> {
        self.retire();
        let outcome = self.production.wait_stopped().await;
        let retired = self.production.files.lock().await.retire_payload().await;
        match outcome {
            // A range its session retired was cancelled, not failed.
            Err(error) if error != "copy_range_cancelled" => Err(error),
            _ => retired,
        }
    }
}

/// Remove what local ranges of a previous process left under the data
/// directory. A range lives and dies with its process: its payload is in
/// the transcode scratch, which every boot clears, and nothing reads a
/// checkpoint manifest back, so after a restart every local manifest
/// directory is an orphan of a session that is gone. Called once at boot,
/// beside that scratch clear and under the same daemon lock. Source
/// manifests are a sibling directory and are not touched.
pub(crate) fn reap_local_range_manifests(data_dir: &Path) -> std::io::Result<()> {
    let manifests = data_dir.join("copy-ranges").join("local-observations");
    match std::fs::symlink_metadata(&manifests) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        // Never follow a link someone put where the directory belongs.
        Ok(metadata) if !metadata.is_dir() => Err(std::io::Error::other(
            "local copy range manifests are not a directory",
        )),
        Ok(_) => plurx_core::fs_secure::clear_scratch_blocking(&manifests),
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
