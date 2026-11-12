//! A local range's custody, pressure, lifetime and reaping.
//!
//! The trace test compares what the source domain sees of a local range with
//! what it sees today of a rolling local session of the same file and
//! session, at the same points of one playback, against a real store with a
//! real catalogue file. Both sides register through their production path
//! (`SourceInput::open` for the rolling session; `LocalCopyRange::census`,
//! the first half of `LocalCopyRange::start`, for the range). Both sides'
//! source observations come from the one rule, `rolling_source_observation`:
//! the rolling side calls it as `maintenance.rs` does, with the flow's hold
//! (`rolling` each evaluation, `refill` before a resume), and the range's
//! read bank calls it through `observe_control` and `refresh`. The range's
//! committed media is set by hand, because the fixture title is shorter than
//! the low watermark; no generation runs on that side and no rolling session
//! is run.
//!
//! The custody test runs a range's census and primary on the real engine and
//! samples the store between them: one row, registered once, from census to
//! settlement. The drop test drops a running range unsettled. The engine
//! test runs a whole local range and holds its settlement to leaving nothing
//! behind: no custody row, no pressure, no payload, no manifest, no session
//! directory. The panic test unwinds a run task. The reaping test kills
//! ranges of both owners and boots.
use super::*;
use plurx_core::store::background_jobs_resources::ForegroundSourceOwner;
use std::time::Duration;

const NODE: &str = "origin-node";
const SESSION: &str = "local-session-1";
const NAMESPACE: &str = "catalogue";

fn identity() -> SourceIdentity {
    SourceIdentity {
        node_id: NODE.into(),
        boot_id: uuid::Uuid::new_v4(),
    }
}

/// A sqlite store whose catalogue holds `path` as one HEVC file.
async fn catalogue(path: &Path, duration_ms: i64) -> (Arc<dyn Store>, MediaFile) {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    let library = store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "Local ranges".to_owned(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: Vec::new(),
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "A local range".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let metadata = std::fs::metadata(path).expect("source metadata");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    let file_id = store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            metadata.len() as i64,
            mtime,
            &plurx_core::domain::ProbeResult {
                duration_ms: Some(duration_ms),
                container: Some("mkv".to_owned()),
                video_codec: Some("hevc".to_owned()),
                video_profile: Some("Main".to_owned()),
                width: Some(640),
                height: Some(360),
                bit_depth: Some(8),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    let file = store
        .get_file(file_id)
        .await
        .expect("read file")
        .expect("stored file");
    (store, file)
}

/// What identifies a custody row to the source domain. The row's own id,
/// its incarnation and its producer attempt are per registration.
#[derive(Debug, PartialEq, Eq)]
struct OwnerKeys {
    lifetime_id: String,
    node_id: String,
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    source_generation: String,
    policy: ForegroundSourcePolicy,
}
impl From<ForegroundSourceOwner> for OwnerKeys {
    fn from(owner: ForegroundSourceOwner) -> Self {
        Self {
            lifetime_id: owner.lifetime_id,
            node_id: owner.node_id,
            file_id: owner.file_id,
            source_size: owner.source_size,
            source_mtime: owner.source_mtime,
            source_generation: owner.source_generation,
            policy: owner.policy,
        }
    }
}

/// One point of a trace: the custody rows of the node and what the domain
/// answers a background reader of the file.
#[derive(Debug, PartialEq, Eq)]
struct TracePoint {
    at: &'static str,
    owners: Vec<OwnerKeys>,
    pressure: bool,
}

async fn point(store: &Arc<dyn Store>, file: &MediaFile, at: &'static str) -> TracePoint {
    TracePoint {
        at,
        owners: store
            .list_foreground_source_owners(NODE)
            .await
            .expect("custody rows")
            .into_iter()
            .map(|token| token.owner.into())
            .collect(),
        pressure: store
            .foreground_source_pressure(file.id, crate::fragment_index_cluster::unix_ms())
            .await
            .expect("pressure"),
    }
}

/// The one bound every wait of these tests runs under. `None` is the bound.
async fn within<T>(wait: impl std::future::Future<Output = T>) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(120), wait)
        .await
        .ok()
}

/// A closed Input releases its custody from its own owner task. Wait for
/// the row to go, on the signal that task raises.
async fn released(store: &Arc<dyn Store>) {
    let gone = async {
        loop {
            let changed = crate::source_custody::source_pressure_changed().notified();
            if store
                .list_foreground_source_owners(NODE)
                .await
                .expect("custody rows")
                .is_empty()
            {
                return;
            }
            changed.await;
        }
    };
    within(gone)
        .await
        .expect("a closed Input releases its custody");
}

fn demand(
    position_ms: i64,
    render_state: crate::playback_control::RenderState,
) -> crate::playback_control::PlaybackDemandSnapshot {
    let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
        crate::playback_control::ClientPlatform::Web,
    );
    snapshot.position_ms = position_ms;
    snapshot.buffered_from_ms = Some(position_ms);
    snapshot.buffered_through_ms = position_ms;
    snapshot.playback_rate = 1.0;
    snapshot.render_state = render_state;
    snapshot.seek_target_ms = None;
    snapshot
}

/// A local user's own named playback.
fn local_owner() -> LocalPlaybackOwner {
    LocalPlaybackOwner::of(&crate::state::PlaybackViewerDemand {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "playback-1".into(),
    })
    .expect("a local user's own named playback")
}

/// What a local VOD session hands a range's start, over temporary roots.
struct Session {
    store: Arc<dyn Store>,
    file: MediaFile,
    identity: SourceIdentity,
    work: tempfile::TempDir,
    data: tempfile::TempDir,
    cache: tempfile::TempDir,
    observation_root: std::path::PathBuf,
    stop: CancellationToken,
}
impl Session {
    async fn on(path: &Path, duration_ms: i64) -> Self {
        let (store, file) = catalogue(path, duration_ms).await;
        let data = crate::test_tempdir().expect("data directory");
        Self {
            store,
            file,
            identity: identity(),
            work: crate::test_tempdir().expect("work directory"),
            observation_root: data.path().join("copy-ranges").join("observations"),
            data,
            cache: crate::test_tempdir().expect("runtime cache"),
            stop: CancellationToken::new(),
        }
    }
    fn start(&self) -> LocalCopyRangeStart<'_> {
        LocalCopyRangeStart {
            store: Arc::clone(&self.store),
            identity: &self.identity,
            file: Arc::new(self.file.clone()),
            recipe: CopyRecipe {
                video: plurx_core::transcode::CopyVideoOptions::new(false, false),
                audio_index: None,
                transcode_audio: false,
                audio_delivery: None,
            },
            start_seconds: 0.0,
            session: SESSION,
            owner: local_owner(),
            ledger: Arc::new(SourceRenditionOwners::default()),
            work_dir: self.work.path(),
            observation_root: &self.observation_root,
            runtime_cache: self.cache.path(),
            catalog_namespace: NAMESPACE.into(),
            node_id: NODE,
            deadline: Instant::now() + Duration::from_secs(120),
            stop: &self.stop,
        }
    }
    /// The census half of a start, bounded.
    async fn census(&self, start: &LocalCopyRangeStart<'_>) -> LocalRangeHead {
        within(LocalCopyRange::census(start))
            .await
            .expect("the census ends in 120 s")
            .expect("a local range's census")
    }
    /// The node's custody rows, by owner id and attempt.
    async fn rows(&self) -> Vec<(String, String)> {
        self.store
            .list_foreground_source_owners(NODE)
            .await
            .expect("custody rows")
            .into_iter()
            .map(|token| (token.owner.owner_id, token.owner.producer_attempt))
            .collect()
    }
}

/// Close a range's source reads: the bank's own hold, through its custody.
/// Its generation cannot read past its open until a refill, so it runs for
/// as long as the test needs it to.
async fn hold_reads(custody: &SourceCustody) {
    let (low, high) = crate::transcode::rolling_source_watermarks(1.0);
    custody
        .observer()
        .rolling(Some(high), low, high, true, true)
        .await
        .expect("the range's reads are held");
}

/// Say this much media is committed from the range's origin.
async fn bank(range: &LocalCopyRange, committed_ms: u64) {
    let mut files = range.production.files.lock().await;
    files.set_origin(0).expect("the range's origin");
    files.segments = vec![super::super::storage::StoredCopySegment {
        index: 0,
        name: "segment-0.m4s".into(),
        bytes: 0,
        sha256: String::new(),
        seconds: committed_ms as f64 / 1000.0,
        first_row: 0,
        rows: 0,
    }];
    files.committed_segments = 1;
}

/// The files of a range under `session`'s roots.
async fn range_files(session: &Session, source: &SourceFence) -> CopyRangeFiles {
    CopyRangeFiles::create(
        session.work.path(),
        &session.observation_root,
        NAMESPACE.into(),
        crate::copy_analysis::source_for_copy(
            NAMESPACE,
            &session.file,
            &"ab".repeat(32),
            source.object_version(),
            NODE,
        )
        .expect("a copy source"),
        CopyRangeOwner::Local {
            session: SESSION.into(),
            viewer: local_owner(),
        },
        uuid::Uuid::new_v4(),
        1,
        plurx_core::segplan::ranges::SourceTimestamp {
            dts: 0,
            time_base_num: 1,
            time_base_den: 90000,
        },
        0,
        "c".repeat(64),
        plurx_core::fmp4::PromotionInputs::default(),
    )
    .await
    .expect("the range's files")
}

/// The source domain sees a local range as it sees a rolling local session
/// of the same file today: one custody row with the same keys, and the same
/// answer to a background reader at every point of the same playback,
/// including a buffering or paused viewer that is still reporting, which
/// lifts pressure while the producer is held.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_local_range_charges_the_source_as_a_rolling_local_session_does() {
    use crate::playback_control::{PlaybackDemand, RenderState};
    use crate::transcode::rolling_source_observation;
    plurx_core::testfixtures::require_ffmpeg();
    let session = Session::on(&plurx_core::testfixtures::source("clean-cra"), 12_000).await;
    let (store, file) = (&session.store, &session.file);
    let (low, high) = crate::transcode::rolling_source_watermarks(1.0);
    let margin = 10_000;
    // Media committed ahead of the viewer at each point of the playback.
    let below_high = low + margin;
    let above_high = high + margin;
    let seek_to = (above_high - (low - margin)) as i64;
    let mut paused = demand(seek_to, RenderState::Rendering);
    paused.demand = PlaybackDemand::Hold;
    let mut seeking = demand(0, RenderState::Seeking);
    seeking.seek_target_ms = Some(seek_to);

    // Today: a rolling local session, by the request its start makes and
    // the custody calls its flow evaluation makes, each observation the one
    // `maintenance.rs` computes from its lease and its produced end.
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(store),
        session.work.path().join("workers"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    )
    .with_source_identity(session.identity.node_id.clone(), session.identity.boot_id);
    let request = manager
        .foreground_source_request(
            file,
            SESSION,
            uuid::Uuid::new_v4(),
            ForegroundSourcePolicy::Rolling,
        )
        .expect("a rolling session's request");
    let input = SourceInput::open(request, "rolling-attempt")
        .await
        .expect("the rolling session's Input");
    let observer = input.observer();
    let evaluate = |demand: &crate::playback_control::PlaybackDemandSnapshot,
                    produced_end_ms: u64,
                    startup_protected: bool,
                    held: bool| {
        let observation = rolling_source_observation(
            Some(demand),
            Some(Duration::ZERO),
            startup_protected,
            0,
            Some(produced_end_ms as i64),
            held,
        );
        let observer = observer.clone();
        async move {
            observer
                .rolling(
                    observation.lead_ms,
                    observation.low_ms,
                    observation.high_ms,
                    observation.held,
                    observation.fresh,
                )
                .await
                .expect("a flow evaluation's source observation");
        }
    };
    let mut rolling = vec![point(store, file, "opened").await];
    // The producer has read: only a reader known to be idle can be held.
    drop(observer.try_active_read());
    evaluate(&demand(0, RenderState::Rendering), below_high, false, false).await;
    rolling.push(point(store, file, "banked below high").await);
    evaluate(&demand(0, RenderState::Rendering), above_high, false, true).await;
    rolling.push(point(store, file, "held above high").await);
    // A resume refills before its signal and observes after it.
    observer
        .refill()
        .await
        .expect("resumed below the low watermark");
    evaluate(&seeking, above_high, false, false).await;
    rolling.push(point(store, file, "seek ahead").await);
    let ahead = above_high + seek_to as u64;
    evaluate(&demand(seek_to, RenderState::Waiting), ahead, false, true).await;
    rolling.push(point(store, file, "held, viewer buffering").await);
    evaluate(&paused, ahead, false, true).await;
    rolling.push(point(store, file, "held, viewer paused").await);
    evaluate(&paused, ahead, true, true).await;
    rolling.push(point(store, file, "held, startup protected").await);
    input.close_and_wait().await.expect("the Input closes");
    released(store).await;
    rolling.push(point(store, file, "closed").await);

    // The range: its start's own census under its one custody, then the
    // primary Input that takes the custody over, as the start's primary
    // does, and the engine's read bank driven by the session's control.
    let start = session.start();
    let head = session.census(&start).await;
    let Ok(custody) = Arc::try_unwrap(head.custody) else {
        panic!("the census joined its Input");
    };
    let input = SourceInput::open_foreground_with_custody(
        custody,
        head.source
            .reopen(file)
            .await
            .expect("an independent descriptor"),
        &file.path,
    )
    .await
    .expect("the range's primary Input");
    let seek_bootstrap = input
        .seal_seek_bootstrap_limit(super::super::SOURCE_SEEK_BOOTSTRAP_BYTES)
        .expect("a seek bootstrap");
    let range = LocalCopyRange {
        production: CopyRangeProduction::new(
            range_files(&session, &head.source).await,
            input.observer(),
            0,
            (low, high),
            seek_bootstrap,
            super::super::production::ReadBankRule::Rolling,
        ),
        source: head.source,
        file: Arc::new(file.clone()),
    };
    let mut ranged = vec![point(store, file, "opened").await];
    drop(input.observer().try_active_read());
    bank(&range, below_high).await;
    range
        .observe_control(demand(0, RenderState::Rendering), false)
        .await
        .expect("an accepted control exchange");
    ranged.push(point(store, file, "banked below high").await);
    // More media commits; the session's maintenance re-reads the bank.
    bank(&range, above_high).await;
    range
        .refresh(false)
        .await
        .expect("the session's maintenance");
    ranged.push(point(store, file, "held above high").await);
    range
        .observe_control(seeking, false)
        .await
        .expect("an accepted seek");
    ranged.push(point(store, file, "seek ahead").await);
    bank(&range, ahead).await;
    range
        .observe_control(demand(seek_to, RenderState::Waiting), false)
        .await
        .expect("an accepted exchange of a buffering viewer");
    ranged.push(point(store, file, "held, viewer buffering").await);
    range
        .observe_control(paused, false)
        .await
        .expect("an accepted exchange of a paused viewer");
    ranged.push(point(store, file, "held, viewer paused").await);
    range
        .refresh(true)
        .await
        .expect("the session's maintenance while its startup is protected");
    ranged.push(point(store, file, "held, startup protected").await);
    input.close_and_wait().await.expect("the Input closes");
    released(store).await;
    ranged.push(point(store, file, "closed").await);

    assert_eq!(ranged, rolling);
    // And the trace is the one a playback has: charged once, keyed by the
    // session, pressing on the domain except while held with a viewer whose
    // last report is fresh and whose startup is not protected.
    let keys: Vec<_> = ranged.iter().map(|point| point.owners.len()).collect();
    assert_eq!(keys, [1, 1, 1, 1, 1, 1, 1, 0]);
    let owner = &ranged[0].owners[0];
    assert_eq!(owner.lifetime_id, SESSION);
    assert_eq!(owner.policy, ForegroundSourcePolicy::Rolling);
    assert_eq!(owner.file_id, file.id);
    let pressure: Vec<_> = ranged.iter().map(|point| point.pressure).collect();
    assert_eq!(
        pressure,
        [true, true, false, true, false, false, true, false]
    );
}

/// A range registers one custody for its census and its primary: the row
/// the census reads under is the row the primary generation reads under,
/// never released and never registered again between them, and its
/// settlement releases it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_local_range_reads_its_census_and_primary_under_one_custody() {
    plurx_core::testfixtures::require_ffmpeg();
    let session = Session::on(&plurx_core::testfixtures::source("clean-cra"), 12_000).await;
    let start = session.start();
    let head = session.census(&start).await;
    // The census has joined its Input; the custody it read under stays.
    let rows = session.rows().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, "local-copy-range");
    assert!(
        point(&session.store, &session.file, "census")
            .await
            .pressure,
        "a starting range presses as a rolling start does"
    );
    hold_reads(&head.custody).await;
    let range = within(LocalCopyRange::primary(start, head))
        .await
        .expect("the primary starts in 120 s")
        .expect("a local range");
    assert!(range.is_live());
    assert_eq!(
        session.rows().await,
        rows,
        "the primary took the census's custody over"
    );
    within(range.settle())
        .await
        .expect("settlement is bounded")
        .expect("a range its session retired settles well");
    released(&session.store).await;
    assert!(session.rows().await.is_empty());
}

/// A range its session drops unsettled stops: its run task reaps the child
/// and closes the Input, which releases the custody. Its files are left for
/// the next boot.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_dropped_local_range_releases_its_custody_and_leaves_its_files() {
    plurx_core::testfixtures::require_ffmpeg();
    let session = Session::on(&plurx_core::testfixtures::source("clean-cra"), 12_000).await;
    let start = session.start();
    let head = session.census(&start).await;
    // Held, the generation could only end by being stopped.
    hold_reads(&head.custody).await;
    let range = within(LocalCopyRange::primary(start, head))
        .await
        .expect("the primary starts in 120 s")
        .expect("a local range");
    assert_eq!(session.rows().await.len(), 1);
    drop(range);
    released(&session.store).await;
    let payload = session.work.path().join("local-copy-ranges").join(SESSION);
    assert_eq!(
        std::fs::read_dir(&payload)
            .expect("the session's payload stays")
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_dir(
            session
                .data
                .path()
                .join("copy-ranges")
                .join("local-observations")
        )
        .expect("local manifests stay")
        .count(),
        1
    );
}

/// A run task that unwinds before it stored an outcome still ends the
/// range: its settlement returns `copy_range_panicked` instead of waiting
/// for an outcome no task will store.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_run_task_that_panics_ends_its_range() {
    let directory = crate::test_tempdir().expect("source directory");
    let path = directory.path().join("film.mkv");
    std::fs::write(&path, vec![7u8; 64 * 1024]).expect("source bytes");
    let session = Session::on(&path, 600_000).await;
    let source = crate::fragment_index_cluster::open_source_playback_fence(&session.file, None)
        .await
        .expect("the held source");
    let custody = local_range_source_request(
        &session.identity,
        Arc::clone(&session.store),
        &session.file,
        SESSION,
    )
    .object_version(source.object_version())
    .register("local-copy-range")
    .await
    .expect("the range's custody");
    let input = SourceInput::open_foreground_with_custody(
        custody,
        source.reopen(&session.file).await.expect("a descriptor"),
        &session.file.path,
    )
    .await
    .expect("the range's Input");
    let production = CopyRangeProduction::new(
        range_files(&session, &source).await,
        input.observer(),
        0,
        crate::transcode::rolling_source_watermarks(1.0),
        input
            .seal_seek_bootstrap_limit(super::super::SOURCE_SEEK_BOOTSTRAP_BYTES)
            .expect("a seek bootstrap"),
        super::super::production::ReadBankRule::Rolling,
    );
    let task = {
        let production = Arc::clone(&production);
        tokio::spawn(async move {
            let _ended = production.end_on_drop();
            panic!("a run task unwinds");
        })
    };
    assert!(task.await.expect_err("the task panicked").is_panic());
    assert_eq!(
        within(production.wait_stopped())
            .await
            .expect("the range's end is bounded by its task's"),
        Err("copy_range_panicked".to_owned())
    );
    assert!(!production.is_live());
    input.close_and_wait().await.expect("the Input closes");
    released(&session.store).await;
}

/// A killed process leaves its ranges' files behind: nothing retired them.
/// The next boot removes them all, payloads with the scratch and manifests
/// with [`reap_range_manifests`], for the shared owner and the local one. A
/// range made after that boot is live, and its files are kept: nothing but
/// its own retirement removes them.
#[tokio::test]
async fn boot_removes_a_killed_ranges_files_and_a_live_ranges_are_kept() {
    let work = crate::test_tempdir().expect("transcode scratch");
    let data = crate::test_tempdir().expect("data directory");
    let observations = data.path().join("copy-ranges").join("observations");
    reap_range_manifests(data.path()).expect("nothing to reap is not an error");
    let local_range = |session: &'static str| {
        let (work, observations) = (work.path().to_owned(), observations.clone());
        async move {
            CopyRangeFiles::create(
                &work,
                &observations,
                NAMESPACE.into(),
                plurx_core::store::analysis_coordination::AnalysisCoordinationSource {
                    file_id: 7,
                    source_size: 4096,
                    source_mtime: 77,
                    origin_node_id: NODE.into(),
                    object_version: "object-version".into(),
                    video_identity: "ab".repeat(32),
                    contract_version: plurx_core::store::FRAGMENT_INDEX_CONTRACT,
                    requested_generation: "normal".into(),
                },
                CopyRangeOwner::Local {
                    session: session.into(),
                    viewer: LocalPlaybackOwner::of(&crate::state::PlaybackViewerDemand {
                        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                            user_id: 7,
                        },
                        playback_id: "playback-1".into(),
                    })
                    .expect("a local user's own named playback"),
                },
                uuid::Uuid::new_v4(),
                1,
                plurx_core::segplan::ranges::SourceTimestamp {
                    dts: 0,
                    time_base_num: 1,
                    time_base_den: 90000,
                },
                0,
                "c".repeat(64),
                plurx_core::fmp4::PromotionInputs::default(),
            )
            .await
            .expect("a range's files")
        }
    };
    let populate = |payload: &Path, manifests: &Path| {
        std::fs::write(payload.join("init.mp4"), b"init").expect("a payload file");
        std::fs::write(manifests.join("checkpoint-1-1.json"), b"{}").expect("a manifest");
        std::fs::write(manifests.join("current.json"), b"{}").expect("the pointer");
    };

    // The previous process: a local range through the real storage, and a
    // shared range laid out as the Source owner lays it out (only a Source
    // session makes that owner). Both die unretired.
    let killed_local = local_range("killed-session").await;
    let (killed_local_payload, killed_local_manifests) = (
        killed_local.payload_path.clone(),
        data.path()
            .join("copy-ranges")
            .join("local-observations")
            .join(killed_local.payload_path.file_name().expect("a range")),
    );
    populate(&killed_local_payload, &killed_local_manifests);
    drop(killed_local);
    let range = uuid::Uuid::new_v4().to_string();
    let killed_source_payload = work
        .path()
        .join("source-copy-ranges")
        .join(uuid::Uuid::new_v4().to_string())
        .join(&range);
    let killed_source_manifests = observations.join(&range);
    for directory in [&killed_source_payload, &killed_source_manifests] {
        std::fs::create_dir_all(directory).expect("a Source range's directory");
    }
    populate(&killed_source_payload, &killed_source_manifests);

    // The next boot: the scratch clear, then the manifest reap beside it.
    plurx_core::fs_secure::clear_scratch_blocking(work.path()).expect("the scratch clear");
    reap_range_manifests(data.path()).expect("the reap");
    for gone in [
        &killed_local_payload,
        &killed_local_manifests,
        &killed_source_payload,
        &killed_source_manifests,
    ] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    for owner in ["observations", "local-observations"] {
        assert_eq!(
            std::fs::read_dir(data.path().join("copy-ranges").join(owner))
                .expect("the manifest directory itself stays")
                .count(),
            0,
            "{owner}"
        );
    }

    // This process's range is live: its files are where it put them.
    let mut live = local_range("live-session").await;
    let live_manifests = data
        .path()
        .join("copy-ranges")
        .join("local-observations")
        .join(live.payload_path.file_name().expect("a range"));
    populate(&live.payload_path, &live_manifests);
    assert!(live.payload_path.join("init.mp4").is_file());
    assert!(live_manifests.join("current.json").is_file());
    // Only its own retirement removes them.
    live.retire_payload().await.expect("retirement");
    assert!(!live.payload_path.exists());
    assert!(!live_manifests.exists());
}

/// A whole local range on the real engine, through its real foreground
/// Inputs: it starts, the session's control, maintenance and rendered report
/// reach it, and the session's retirement leaves nothing behind, not even
/// the session's payload directory.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_local_range_runs_under_its_own_request_and_settles_to_nothing() {
    use crate::playback_control::RenderState;
    plurx_core::testfixtures::require_ffmpeg();
    let session = Session::on(&plurx_core::testfixtures::source("clean-cra"), 12_000).await;
    let started = within(LocalCopyRange::start(session.start())).await;
    let Some(started) = started else {
        // The stop token ends the head census; a primary generation that
        // had started belongs to a range this test never received, which
        // its drop retires.
        session.stop.cancel();
        panic!("the local range did not start in 120 s");
    };
    let range = started.expect("a local range");
    let ended = within(range.production.wait_stopped()).await;
    if ended.is_none() {
        // Retirement stops the child and joins it, its pipe tasks and its
        // Input, so no ffmpeg outlives the test.
        let settled = within(range.settle()).await;
        panic!(
            "the local range did not reach its end in 120 s; retired, it settled with {settled:?}"
        );
    }
    ended
        .expect("bounded above")
        .expect("the range reaches the end of the title");

    // The whole title is committed and the range is still its session's.
    assert!(range.is_live());
    range
        .physical_fence()
        .await
        .expect("the held object is unchanged");
    let payload = session.work.path().join("local-copy-ranges").join(SESSION);
    assert_eq!(
        std::fs::read_dir(&payload)
            .expect("the session's payload")
            .count(),
        1
    );
    let rendered = range
        .mark_rendered()
        .await
        .expect("the rendered report")
        .expect("a rendered checkpoint");
    assert!(rendered.first_frame_rendered);
    assert!(rendered.boundaries.begins_title && rendered.boundaries.eof);
    range
        .observe_control(demand(1_000, RenderState::Rendering), false)
        .await
        .expect("an accepted control exchange");
    range
        .refresh(false)
        .await
        .expect("the session's maintenance");

    // The session retires.
    within(range.settle())
        .await
        .expect("settlement is bounded")
        .expect("a range that ended well settles well");
    assert!(!range.is_live());
    assert!(range.physical_fence().await.is_err());
    released(&session.store).await;
    let after = point(&session.store, &session.file, "settled").await;
    assert!(after.owners.is_empty());
    assert!(!after.pressure);
    assert!(!payload.exists(), "the session's payload directory goes");
    assert!(session.work.path().join("local-copy-ranges").is_dir());
    assert_eq!(
        std::fs::read_dir(
            session
                .data
                .path()
                .join("copy-ranges")
                .join("local-observations")
        )
        .expect("local manifests")
        .count(),
        0
    );
    assert!(!session.observation_root.exists());
}
