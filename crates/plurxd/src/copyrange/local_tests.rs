//! A local range's custody, pressure, lifetime and reaping.
//!
//! The trace test compares what the source domain sees of a local range with
//! what it sees today of a rolling local session of the same file and
//! session, at the same points of one playback, against a real store with a
//! real catalogue file. Both sides register through their production path
//! (`SourceInput::open` for the rolling session, `open_foreground_held` for
//! the range). The range's side then runs the engine's real read bank
//! through [`LocalCopyRange::observe_control`] and [`LocalCopyRange::refresh`]
//! over a bank the test sets; no generation runs. The rolling session's
//! side is its custody calls replayed in the order `maintenance.rs` makes
//! them (`rolling` each evaluation, `refill` before a resume): no rolling
//! session is run.
//!
//! The engine test runs a whole local range, census and primary generation,
//! through its real foreground Inputs, and holds its settlement to leaving
//! nothing behind: no custody row, no pressure, no payload, no manifest.
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

/// The source domain sees a local range as it sees a rolling local session
/// of the same file today: one custody row with the same keys, and the same
/// answer to a background reader at every point of the same playback.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_local_range_charges_the_source_as_a_rolling_local_session_does() {
    use crate::playback_control::RenderState;
    let directory = crate::test_tempdir().expect("source directory");
    let path = directory.path().join("film.mkv");
    std::fs::write(&path, vec![7u8; 64 * 1024]).expect("source bytes");
    let (store, file) = catalogue(&path, 600_000).await;
    let identity = identity();
    let (low, high) = watermarks(1.0);
    let margin = 10_000;
    // Media committed ahead of the viewer at each point of the playback.
    let below_high = low + margin;
    let above_high = high + margin;
    let seek_to = (above_high - (low - margin)) as i64;

    // Today: a rolling local session, by the request its start makes and
    // the custody calls its flow evaluation makes.
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        directory.path().join("workers"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    )
    .with_source_identity(identity.node_id.clone(), identity.boot_id);
    let request = manager
        .foreground_source_request(
            &file,
            SESSION,
            uuid::Uuid::new_v4(),
            ForegroundSourcePolicy::Rolling,
        )
        .expect("a rolling session's request");
    let input = SourceInput::open(request, "rolling-attempt")
        .await
        .expect("the rolling session's Input");
    let observer = input.observer();
    let mut rolling = vec![point(&store, &file, "opened").await];
    // The producer has read: only a reader known to be idle can be held.
    drop(observer.try_active_read());
    observer
        .rolling(Some(below_high), low, high, false, true)
        .await
        .expect("below the high watermark");
    rolling.push(point(&store, &file, "banked below high").await);
    observer
        .rolling(Some(above_high), low, high, true, true)
        .await
        .expect("held above the high watermark");
    rolling.push(point(&store, &file, "held above high").await);
    observer
        .refill()
        .await
        .expect("resumed below the low watermark");
    rolling.push(point(&store, &file, "seek ahead").await);
    observer
        .rolling(Some(above_high), low, high, true, false)
        .await
        .expect("held with no fresh rendering demand");
    rolling.push(point(&store, &file, "held, viewer not rendering").await);
    input.close_and_wait().await.expect("the Input closes");
    released(&store).await;
    rolling.push(point(&store, &file, "closed").await);

    // The range: its own request and Input, and the engine's read bank
    // driven by the session's control state.
    let roots = (
        crate::test_tempdir().expect("work directory"),
        crate::test_tempdir().expect("data directory"),
    );
    let source = crate::fragment_index_cluster::open_source_playback_fence(&file, None)
        .await
        .expect("the held source");
    let request = local_range_source_request(&identity, Arc::clone(&store), &file, SESSION);
    let input = SourceInput::open_foreground_held(
        request,
        source
            .reopen(&file)
            .await
            .expect("an independent descriptor"),
        "local-copy-primary",
    )
    .await
    .expect("the range's Input");
    let files = CopyRangeFiles::create(
        roots.0.path(),
        &roots.1.path().join("copy-ranges").join("observations"),
        NAMESPACE.into(),
        crate::copy_analysis::source_for_copy(
            NAMESPACE,
            &file,
            &"ab".repeat(32),
            source.object_version(),
            NODE,
        )
        .expect("a copy source"),
        CopyRangeOwner::Local {
            session: SESSION.into(),
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
    .expect("the range's files");
    let seek_bootstrap = input
        .seal_seek_bootstrap_limit(super::super::SOURCE_SEEK_BOOTSTRAP_BYTES)
        .expect("a seek bootstrap");
    let range = LocalCopyRange {
        production: CopyRangeProduction::banked_for_test(
            files,
            input.observer(),
            watermarks(1.0),
            seek_bootstrap,
        ),
        source,
        file: Arc::new(file.clone()),
    };
    let bank = |ahead_of_zero_ms: u64| {
        let range = &range;
        async move {
            range
                .production
                .files
                .lock()
                .await
                .bank_for_test(0, ahead_of_zero_ms as f64 / 1000.0);
        }
    };
    let mut ranged = vec![point(&store, &file, "opened").await];
    drop(input.observer().try_active_read());
    bank(below_high).await;
    range
        .observe_control(demand(0, RenderState::Rendering))
        .await
        .expect("an accepted control exchange");
    ranged.push(point(&store, &file, "banked below high").await);
    // More media commits; the session's maintenance re-reads the bank.
    bank(above_high).await;
    range.refresh().await.expect("the session's maintenance");
    ranged.push(point(&store, &file, "held above high").await);
    let mut seeking = demand(0, RenderState::Seeking);
    seeking.seek_target_ms = Some(seek_to);
    range
        .observe_control(seeking)
        .await
        .expect("an accepted seek");
    ranged.push(point(&store, &file, "seek ahead").await);
    bank(above_high + seek_to as u64).await;
    range
        .observe_control(demand(seek_to, RenderState::Waiting))
        .await
        .expect("an accepted exchange of a viewer that is not rendering");
    ranged.push(point(&store, &file, "held, viewer not rendering").await);
    input.close_and_wait().await.expect("the Input closes");
    released(&store).await;
    ranged.push(point(&store, &file, "closed").await);

    assert_eq!(ranged, rolling);
    // And the trace is the one a playback has: charged once, keyed by the
    // session, pressing on the domain except while held with a fresh viewer.
    let keys: Vec<_> = ranged.iter().map(|point| point.owners.len()).collect();
    assert_eq!(keys, [1, 1, 1, 1, 1, 0]);
    let owner = &ranged[0].owners[0];
    assert_eq!(owner.lifetime_id, SESSION);
    assert_eq!(owner.policy, ForegroundSourcePolicy::Rolling);
    assert_eq!(owner.file_id, file.id);
    let pressure: Vec<_> = ranged.iter().map(|point| point.pressure).collect();
    assert_eq!(pressure, [true, true, false, true, true, false]);
}

/// Boot removes every local range manifest a previous process left and
/// nothing else: Source manifests are a sibling it does not open.
#[test]
fn boot_reaps_local_range_manifests_and_nothing_else() {
    let data = crate::test_tempdir().expect("data directory");
    reap_local_range_manifests(data.path()).expect("nothing to reap is not an error");
    let copy_ranges = data.path().join("copy-ranges");
    let local = copy_ranges.join("local-observations");
    let source = copy_ranges.join("observations");
    for manifests in [&local, &source] {
        let range = manifests.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&range).expect("a range's manifests");
        std::fs::write(range.join("checkpoint-1-1.json"), b"{}").expect("a manifest");
        std::fs::write(range.join("current.json"), b"{}").expect("the pointer");
    }
    reap_local_range_manifests(data.path()).expect("the reap");
    assert!(local.is_dir());
    assert_eq!(
        std::fs::read_dir(&local).expect("local manifests").count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(&source)
            .expect("Source manifests")
            .count(),
        1
    );
}

/// A whole local range on the real engine, through its real foreground
/// Inputs: it starts, the session's control, maintenance and rendered report
/// reach it, and the session's retirement leaves nothing behind.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_local_range_runs_under_its_own_request_and_settles_to_nothing() {
    use crate::playback_control::RenderState;
    plurx_core::testfixtures::require_ffmpeg();
    let (store, file) = catalogue(&plurx_core::testfixtures::source("clean-cra"), 12_000).await;
    let identity = identity();
    let work = crate::test_tempdir().expect("work directory");
    let data = crate::test_tempdir().expect("data directory");
    let cache = crate::test_tempdir().expect("runtime cache");
    let stop = CancellationToken::new();
    let started = within(LocalCopyRange::start(LocalCopyRangeStart {
        store: Arc::clone(&store),
        identity: &identity,
        file: Arc::new(file.clone()),
        recipe: CopyRecipe {
            video: plurx_core::transcode::CopyVideoOptions::new(false, false),
            audio_index: None,
            transcode_audio: false,
            audio_delivery: None,
        },
        start_seconds: 0.0,
        session: SESSION,
        owner: LocalPlaybackOwner::of(&crate::state::PlaybackViewerDemand {
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            playback_id: "playback-1".into(),
        })
        .expect("a local user's own named playback"),
        ledger: Arc::new(SourceRenditionOwners::default()),
        work_dir: work.path(),
        observation_root: &data.path().join("copy-ranges").join("observations"),
        runtime_cache: cache.path(),
        catalog_namespace: NAMESPACE.into(),
        node_id: NODE,
        deadline: Instant::now() + Duration::from_secs(120),
        stop: &stop,
    }))
    .await;
    let Some(started) = started else {
        // The stop token ends the head census; a primary generation that
        // had started belongs to a range this test never received.
        stop.cancel();
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
    let payload = work.path().join("local-copy-ranges").join(SESSION);
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
        .observe_control(demand(1_000, RenderState::Rendering))
        .await
        .expect("an accepted control exchange");
    range.refresh().await.expect("the session's maintenance");

    // The session retires.
    within(range.settle())
        .await
        .expect("settlement is bounded")
        .expect("a range that ended well settles well");
    assert!(!range.is_live());
    assert!(range.physical_fence().await.is_err());
    released(&store).await;
    let after = point(&store, &file, "settled").await;
    assert!(after.owners.is_empty());
    assert!(!after.pressure);
    assert_eq!(
        std::fs::read_dir(&payload)
            .expect("the session's payload")
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(data.path().join("copy-ranges").join("local-observations"))
            .expect("local manifests")
            .count(),
        0
    );
    assert!(!data
        .path()
        .join("copy-ranges")
        .join("observations")
        .exists());
}
