//! A range's owner: its directories, its manifest binding and its retirement.
//!
//! The Source manifest is held to a committed golden file,
//! `source_manifest_4b125e6.json`: a populated manifest as the binary before
//! the owner enum (`4b125e6`) wrote it, derived from that commit's manifest
//! declaration, with fixed sample values for the three fields its Source
//! binding supplies. No binary reads a checkpoint manifest back today
//! (observations reach the Analysis worker in memory), so
//! [`PreviousManifest`] and [`OwnerView`] stand for the two readers: the
//! strict one a binary that knows only Source manifests would be, and the
//! additive one that takes a manifest without `owner` to be a Source owner's.
//!
//! A `SourceDispatchAssignment` is made only by a Source session, so the
//! tests of a real Source owner are [`a_source_owner_through_its_real_binding`],
//! which the Source actor fixture calls with the assignment it holds.
use super::*;

const NAMESPACE: &str = "catalogue";
const GOLDEN: &str = include_str!("source_manifest_4b125e6.json");
/// The sample values the golden file carries where a Source binding's own
/// incarnation, request and owner key go.
const GOLDEN_INCARNATION: &str = "00000000-0000-4000-8000-000000000001";
const GOLDEN_REQUEST: &str = "golden-source-request";
const GOLDEN_OWNER_KEY: &str = "golden-source-owner-key";
const GOLDEN_RANGE: &str = "11111111-2222-4333-8444-555555555555";

fn local_viewer(user_id: i64, playback_id: &str) -> crate::state::PlaybackViewerDemand {
    crate::state::PlaybackViewerDemand {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id },
        playback_id: playback_id.into(),
    }
}

fn viewer(user_id: i64) -> LocalPlaybackOwner {
    LocalPlaybackOwner::of(&local_viewer(user_id, "playback-1"))
        .expect("a local user's own named playback")
}

fn source() -> AnalysisCoordinationSource {
    AnalysisCoordinationSource {
        file_id: 7,
        source_size: 4096,
        source_mtime: 77,
        origin_node_id: "origin-node".into(),
        object_version: "object-version".into(),
        video_identity: "ab".repeat(32),
        // The golden file's, not the current contract's: a contract bump
        // must not rewrite what an earlier binary wrote.
        contract_version: 3,
        requested_generation: "normal".into(),
    }
}

fn origin() -> SourceTimestamp {
    SourceTimestamp {
        dts: -7500,
        time_base_num: 1,
        time_base_den: 90000,
    }
}

fn promotion() -> PromotionInputs {
    PromotionInputs {
        indexed_hdr10_metadata: true,
        parameter_sets: vec![vec![64, 1], vec![66, 1]],
        hdr10_sei: vec![vec![78, 1, 137]],
        ..PromotionInputs::default()
    }
}

struct Roots {
    work: tempfile::TempDir,
    data: tempfile::TempDir,
}
impl Roots {
    fn new() -> Self {
        Self {
            work: crate::test_tempdir().expect("work directory"),
            data: crate::test_tempdir().expect("data directory"),
        }
    }
    fn observations(&self) -> PathBuf {
        self.data.path().join("copy-ranges").join("observations")
    }
    async fn create(
        &self,
        owner: CopyRangeOwner,
        range_id: uuid::Uuid,
    ) -> Result<CopyRangeFiles, String> {
        CopyRangeFiles::create(
            self.work.path(),
            &self.observations(),
            NAMESPACE.into(),
            source(),
            owner,
            range_id,
            1,
            origin(),
            0,
            "c".repeat(64),
            promotion(),
        )
        .await
    }
}

fn local(session: &str) -> CopyRangeOwner {
    CopyRangeOwner::Local {
        session: session.to_owned(),
        viewer: viewer(7),
    }
}

/// The manifest exactly as `4b125e6` declared it, field for field and in its
/// order. It reads strictly, as that declaration asked
/// (`deny_unknown_fields`).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousManifest {
    version: u32,
    catalog_namespace: String,
    source: AnalysisCoordinationSource,
    source_incarnation: String,
    source_request: String,
    source_owner_key: String,
    range_id: uuid::Uuid,
    range_epoch: u64,
    checkpoint_sequence: u64,
    init_contract_sha256: String,
    canonical_init_sha256: String,
    av_init_sha256: String,
    source_origin_dts: i64,
    source_time_base_num: u32,
    source_time_base_den: u32,
    canonical_title_start: u64,
    timescale: u32,
    media_origin_ms: i64,
    target_duration: u32,
    begins_title: bool,
    eof: bool,
    first_frame_rendered: bool,
    rows: Vec<IndexRow>,
    segments: Vec<StoredCopySegment>,
    row_inspections: Vec<StoredInspection>,
    inspection_packets: Option<u64>,
    inspection_slices: Option<u64>,
    inspection_configuration_sha256: Option<String>,
    promotion: PromotionInputs,
}

/// The owner a manifest names, read additively: one without `owner` is a
/// Source owner's, which is every manifest written before the field existed.
#[derive(Deserialize)]
struct OwnerView {
    #[serde(default = "source_owner")]
    owner: String,
    source_incarnation: Option<String>,
    source_request: Option<String>,
    source_owner_key: Option<String>,
    local_session: Option<String>,
    local_user_id: Option<i64>,
    local_playback_id: Option<String>,
}
fn source_owner() -> String {
    "source".into()
}

/// Give `files` the state the golden manifest records: two rows in two
/// segments, their inspections, the range inspection, both inits, a rendered
/// third checkpoint.
fn populate_as_golden(files: &mut CopyRangeFiles) {
    use plurx_core::fmp4::CutClass;
    files.sequence = 3;
    files.canonical_init_sha256 = "a".repeat(64);
    files.av_init_sha256 = "b".repeat(64);
    files.rendered = true;
    files.rows = vec![
        IndexRow {
            dts: 0,
            duration: 360_000,
            bytes: 100_000,
            video_bytes: 99_400,
            class: CutClass::CleanIdr,
        },
        IndexRow {
            dts: 360_000,
            duration: 225_000,
            bytes: 50_000,
            video_bytes: 49_700,
            class: CutClass::CleanCra,
        },
    ];
    files.segments = vec![
        StoredCopySegment {
            index: 0,
            name: "segment-0.m4s".into(),
            bytes: 123_456,
            sha256: "d".repeat(64),
            seconds: 4.0,
            first_row: 0,
            rows: 1,
        },
        StoredCopySegment {
            index: 1,
            name: "segment-1.m4s".into(),
            bytes: 65_432,
            sha256: "e".repeat(64),
            seconds: 2.5,
            first_row: 1,
            rows: 1,
        },
    ];
    files.row_inspections = vec![
        StoredInspection {
            dts: 0,
            duration: 360_000,
            packets: 96,
            slices: 96,
            configuration_sha256: "f".repeat(64),
            hdr10_mastering: Some(
                "G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)".into(),
            ),
            hdr10_light: Some([1000, 400]),
            hdr10_conflict: false,
        },
        StoredInspection {
            dts: 360_000,
            duration: 225_000,
            packets: 60,
            slices: 60,
            configuration_sha256: "f".repeat(64),
            hdr10_mastering: None,
            hdr10_light: None,
            hdr10_conflict: false,
        },
    ];
    files.inspection = Some(RangeInspection {
        packets: 156,
        slices: 156,
        configuration_sha256: "f".repeat(64),
    });
}

fn golden_boundaries() -> RangeBoundaries {
    RangeBoundaries {
        begins_title: true,
        eof: true,
    }
}

/// The golden file is a manifest the previous binary's strict declaration
/// reads and writes back unchanged, and the additive reader takes it for a
/// Source owner's: old, read by new.
#[test]
fn the_golden_source_manifest_reads_strictly_and_as_a_source_owners() {
    let golden = GOLDEN.trim_end();
    let read: PreviousManifest = serde_json::from_str(golden).expect("the previous reader");
    assert_eq!(read.source_incarnation, GOLDEN_INCARNATION);
    assert_eq!(read.rows.len(), 2);
    assert_eq!(read.segments.len(), 2);
    assert_eq!(read.row_inspections.len(), 2);
    assert_eq!(read.promotion, promotion());
    assert_eq!(read.source, source());
    assert_eq!(
        serde_json::to_string(&read).expect("the previous writer"),
        golden,
        "the golden file is not what the previous declaration writes"
    );
    let owner: OwnerView = serde_json::from_str(golden).expect("the additive reader");
    assert_eq!(owner.owner, "source");
    assert_eq!(
        owner.source_incarnation.as_deref(),
        Some(GOLDEN_INCARNATION)
    );
    assert_eq!(owner.source_request.as_deref(), Some(GOLDEN_REQUEST));
    assert_eq!(owner.source_owner_key.as_deref(), Some(GOLDEN_OWNER_KEY));
    assert!(owner.local_session.is_none());
    assert!(owner.local_user_id.is_none());
    assert!(owner.local_playback_id.is_none());
}

/// A real Source owner, made from the assignment of a real Source session:
/// its directories are the ones it always had, and the manifest it writes
/// through `CopyRangeOwner::Source`'s own binding is the golden file's bytes
/// with this assignment's incarnation, request and owner key in place of the
/// sample ones. New, read by old: the previous strict reader takes it.
///
/// A local generation is also refused on a rendition this assignment is
/// attached to, before any child.
pub(crate) async fn a_source_owner_through_its_real_binding(assignment: &SourceDispatchAssignment) {
    let roots = Roots::new();
    let range_id: uuid::Uuid = GOLDEN_RANGE.parse().expect("the golden range");
    let mut files = roots
        .create(
            CopyRangeOwner::Source(Box::new(assignment.clone())),
            range_id,
        )
        .await
        .expect("a Source owner's range files");
    let incarnation = assignment.binding().incarnation_id().to_string();
    assert_eq!(
        files.payload_path,
        roots
            .work
            .path()
            .join("source-copy-ranges")
            .join(&incarnation)
            .join(GOLDEN_RANGE)
    );
    assert!(files.payload_path.is_dir());
    assert_eq!(
        files.observation_path,
        roots.observations().join(GOLDEN_RANGE)
    );
    assert!(files.observation_path.is_dir());
    assert!(!roots.work.path().join("local-copy-ranges").exists());
    assert!(!roots
        .data
        .path()
        .join("copy-ranges")
        .join("local-observations")
        .exists());

    populate_as_golden(&mut files);
    let written = String::from_utf8(
        files
            .manifest(90000, -83, golden_boundaries())
            .expect("a Source manifest"),
    )
    .expect("JSON");
    let quoted = |value: &str| serde_json::to_string(value).expect("a JSON string");
    let mut expected = GOLDEN.trim_end().to_owned();
    for (sample, actual) in [
        (GOLDEN_INCARNATION, incarnation.as_str()),
        (GOLDEN_REQUEST, assignment.binding().request_id()),
        (
            GOLDEN_OWNER_KEY,
            assignment.binding().principal().owner_key().as_str(),
        ),
    ] {
        assert_eq!(expected.matches(&quoted(sample)).count(), 1, "{sample}");
        expected = expected.replace(&quoted(sample), &quoted(actual));
    }
    assert_eq!(
        written, expected,
        "a Source manifest gained, lost, moved or re-encoded a field"
    );
    let read: PreviousManifest = serde_json::from_str(&written).expect("the previous reader");
    assert_eq!(read.source_incarnation, incarnation);
    let owner: OwnerView = serde_json::from_str(&written).expect("the additive reader");
    assert_eq!(owner.owner, "source");

    files.retire_payload().await.expect("retirement");
    assert!(!files.payload_path.exists());
    assert!(!files.observation_path.exists());

    #[cfg(target_os = "linux")]
    {
        let owners = crate::source_generation::SourceRenditionOwners::default();
        let _source_owner = owners.attach(assignment).expect("a Source viewer attaches");
        let local = viewer(7);
        let (input, _held) =
            crate::source_input::SourceInput::qualification_fixture(b"source").await;
        let cache = crate::test_tempdir().expect("runtime cache");
        let refused = crate::copyrange::spawn_source_generation(
            input,
            Path::new("/films/film.mkv"),
            &["-i".to_owned(), "/films/film.mkv".to_owned()],
            cache.path(),
            crate::copyrange::CopyGenerationOwner::Local {
                owners: &owners,
                dispatch: owners.begin_generation().expect("a dispatch"),
                viewer: &local,
                permit: None,
            },
            Arc::new(crate::prodrun::ProducerSlot::new()),
        )
        .await
        .err();
        assert_eq!(refused, Some("copy_local_owner_invalid".to_owned()));
    }
}

/// A local manifest names its session and viewer and carries no Source
/// binding. A reader that knows only Source manifests refuses it, and it is
/// written where such a reader never looks.
#[tokio::test]
async fn a_local_manifest_names_its_owner_and_is_not_a_source_manifest() {
    let roots = Roots::new();
    let range_id = uuid::Uuid::new_v4();
    let mut files = roots
        .create(local("session-1"), range_id)
        .await
        .expect("range files");
    populate_as_golden(&mut files);
    let written = files
        .manifest(90000, -83, golden_boundaries())
        .expect("a local manifest");
    let owner: OwnerView = serde_json::from_slice(&written).expect("the additive reader");
    assert_eq!(owner.owner, "local");
    assert_eq!(owner.local_session.as_deref(), Some("session-1"));
    assert_eq!(owner.local_user_id, Some(7));
    assert_eq!(owner.local_playback_id.as_deref(), Some("playback-1"));
    assert!(owner.source_incarnation.is_none());
    assert!(owner.source_request.is_none());
    assert!(owner.source_owner_key.is_none());
    assert!(serde_json::from_slice::<PreviousManifest>(&written).is_err());

    let range = range_id.to_string();
    let copy_ranges = roots.data.path().join("copy-ranges");
    assert_eq!(
        files.observation_path,
        copy_ranges.join("local-observations").join(&range)
    );
    assert!(files.observation_path.is_dir());
    assert!(!copy_ranges.join("observations").exists());
    assert_eq!(
        files.payload_path,
        roots
            .work
            .path()
            .join("local-copy-ranges")
            .join("session-1")
            .join(&range)
    );
    assert!(files.payload_path.is_dir());
    assert!(!roots.work.path().join("source-copy-ranges").exists());
}

/// A session that is not one plain directory name makes no range and no
/// directory.
#[tokio::test]
async fn a_local_owner_needs_a_plain_session() {
    let roots = Roots::new();
    let long = "s".repeat(129);
    for session in ["", "..", "a/b", "a.b", long.as_str()] {
        assert_eq!(
            roots
                .create(local(session), uuid::Uuid::new_v4())
                .await
                .err(),
            Some("copy_range_binding_invalid".to_owned()),
            "{session:?}"
        );
    }
    assert!(!roots.work.path().join("local-copy-ranges").exists());
    assert!(!roots.data.path().join("copy-ranges").exists());
}

/// Only a live local viewer with a named playback makes a range owner. The
/// authorities that carry a durable request across a restart have no way to
/// become one: the owner has no other constructor.
#[test]
fn a_local_range_owner_is_only_a_live_named_local_playback() {
    let owner = LocalPlaybackOwner::of(&local_viewer(7, "playback-1")).expect("a named playback");
    assert_eq!(owner.authority().user_id(), 7);
    assert_eq!(owner.authority().playback_id(), "playback-1");
    assert!(LocalPlaybackOwner::of(&local_viewer(7, "")).is_err());
    assert!(LocalPlaybackOwner::of(&local_viewer(0, "playback-1")).is_err());
    let shared = crate::state::PlaybackViewerDemand {
        principal: plurx_core::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::new_v4(),
            &"a".repeat(64),
        )
        .expect("a sharing principal"),
        playback_id: "playback-1".into(),
    };
    assert!(LocalPlaybackOwner::of(&shared).is_err());
}

#[cfg(target_os = "linux")]
mod engine {
    //! A local owner on the real engine: the head census and the primary
    //! triple-output child both run as a local playback's generations, and
    //! the range they cut is created, committed and retired under a local
    //! owner. The Input is the loopback bridge over the fixture's bytes; a
    //! local playback's own Input admission is not exercised here.
    use super::*;
    use crate::copy_analysis::CopyRecipe;
    use crate::copy_normalization::CopyNormalizationContract;
    use crate::copyrange::{
        run_source_census, spawn_source_generation, CopyClockCensus, CopyGenerationOwner,
        CopyMuxEvent, ObservedCopyMux,
    };
    use crate::prodrun::ProducerSlot;
    use crate::source_generation::SourceRenditionOwners;
    use crate::source_input::SourceInput;
    use plurx_core::domain::MediaFile;
    use plurx_core::transcode::CopyVideoOptions;
    use std::time::Duration;

    fn media_file(path: PathBuf) -> MediaFile {
        let metadata = std::fs::metadata(&path).expect("fixture metadata");
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 1,
            item_id: 1,
            path,
            size: metadata.len() as i64,
            mtime: metadata
                .modified()
                .expect("fixture mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            duration_ms: Some(12_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main".into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(1_000_000),
            audio_streams: Vec::new(),
            subtitle_streams: Vec::new(),
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    /// What the production task does with each batch of mux events.
    async fn apply(
        files: &mut CopyRangeFiles,
        events: Vec<CopyMuxEvent>,
        census: &CopyClockCensus,
        stop: &CancellationToken,
    ) -> Result<(), String> {
        for event in events {
            match event {
                CopyMuxEvent::Init { canonical, av } => files.set_init(canonical, av, stop).await?,
                CopyMuxEvent::Origin {
                    first_packet,
                    canonical_dts,
                    av_dts,
                    timescale,
                } => {
                    census.verify_seek_landing(first_packet, 0)?;
                    files.set_origin(census.media_origin_ms(
                        canonical_dts,
                        0,
                        av_dts,
                        timescale,
                    )?)?;
                }
                CopyMuxEvent::Segment(output) => {
                    files.commit_segment(output, stop).await?;
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_local_owner_creates_commits_and_retires_a_range_on_the_engine() {
        plurx_core::testfixtures::require_ffmpeg();
        let stop = CancellationToken::new();
        let run = local_range(stop.clone());
        tokio::pin!(run);
        match tokio::time::timeout(Duration::from_secs(240), &mut run).await {
            Ok(result) => result.expect("the local range"),
            Err(_) => {
                // The stop token ends the running generation: it retires its
                // child and joins its pipe tasks, Input and reap before the
                // run returns, so no ffmpeg outlives the test. That wait is
                // bounded too: a settlement that never comes fails the test
                // instead of hanging it.
                stop.cancel();
                let settled = tokio::time::timeout(Duration::from_secs(60), &mut run).await;
                panic!("the local range did not finish in 240 s; cancelled, it settled with {settled:?}");
            }
        }
    }

    async fn local_range(stop: CancellationToken) -> Result<(), String> {
        let path = plurx_core::testfixtures::source("clean-cra");
        let bytes = std::fs::read(&path).expect("fixture bytes");
        let file = media_file(path);
        let cache = crate::test_tempdir().expect("runtime cache");
        let roots = Roots::new();
        let recipe = CopyRecipe {
            video: CopyVideoOptions::new(false, false),
            audio_index: None,
            transcode_audio: false,
            audio_delivery: None,
        };
        let object_version = crate::fragment_index_cluster::inspect_source(&file).await?;
        let observed_source = crate::copy_analysis::source_for_copy(
            NAMESPACE,
            &file,
            &crate::fragindex::identity_for(&file, recipe.video).argv_fingerprint,
            &object_version,
            "origin-node",
        )
        .map_err(|error| format!("copy source: {error}"))?;
        // The local VOD session: its viewer and its rendition's dispatch
        // ledger, which no Source assignment is attached to.
        let viewer = viewer(7);
        let owners = SourceRenditionOwners::default();
        let mut held = Vec::new();

        let (input, keep) = SourceInput::qualification_fixture(&bytes).await;
        held.push(keep);
        let census = run_source_census(
            input,
            object_version,
            &file,
            recipe.video,
            CopyGenerationOwner::Local {
                owners: &owners,
                dispatch: owners.begin_generation().map_err(str::to_owned)?,
                viewer: &viewer,
                permit: None,
            },
            Arc::new(ProducerSlot::new()),
            cache.path(),
            None,
            &stop,
        )
        .await
        .map_err(|error| format!("head census: {error}"))?;
        let contract = Arc::new(
            CopyNormalizationContract::from_head(
                &file,
                recipe.video,
                &census.raw_canonical_init,
                &census.raw_canonical_fragment,
            )
            .map_err(|error| format!("head contract: {error}"))?,
        );
        let mut mux = ObservedCopyMux::new(
            Arc::clone(&contract),
            census.first_dts,
            0,
            recipe.tracks(&file),
            recipe.video.retains_hevc_parameter_sets(),
            true,
        )?;

        let session = uuid::Uuid::new_v4().to_string();
        let range_id = uuid::Uuid::new_v4();
        let mut files = CopyRangeFiles::create(
            roots.work.path(),
            &roots.observations(),
            NAMESPACE.into(),
            observed_source,
            CopyRangeOwner::Local {
                session: session.clone(),
                viewer: viewer.clone(),
            },
            range_id,
            1,
            census.first_dts,
            0,
            contract.init_contract_sha256().to_owned(),
            contract.promotion().clone(),
        )
        .await?;
        let payload = roots
            .work
            .path()
            .join("local-copy-ranges")
            .join(&session)
            .join(range_id.to_string());
        let manifests = roots
            .data
            .path()
            .join("copy-ranges")
            .join("local-observations")
            .join(range_id.to_string());
        assert_eq!(files.payload_path, payload);

        let (input, keep) = SourceInput::qualification_fixture(&bytes).await;
        held.push(keep);
        let args = recipe.args(&file, 0.0).map_err(str::to_owned)?;
        let mut running = spawn_source_generation(
            input,
            &file.path,
            &args,
            cache.path(),
            CopyGenerationOwner::Local {
                owners: &owners,
                dispatch: owners.begin_generation().map_err(str::to_owned)?,
                viewer: &viewer,
                permit: None,
            },
            Arc::new(ProducerSlot::new()),
        )
        .await?;
        let observed = async {
            loop {
                let pipes = running
                    .pipes
                    .as_mut()
                    .ok_or("copy_output_readers_missing")?;
                let event = tokio::select! {biased;
                    ()=stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),
                    event=pipes.next()=>event.ok_or("copy_output_readers_lost")?,
                };
                let eof = pipes.all_eof();
                apply(&mut files, mux.feed(event)?, &census, &stop).await?;
                if eof {
                    break;
                }
            }
            running
                .slot
                .request_registered_completion(&running.registration)
                .await
                .map_err(|_| "copy_reap_unavailable".to_owned())?;
            let status = tokio::select! {biased;
                ()=stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),
                status=running.registration.wait_child_exit()=>status,
            };
            if !status.success() {
                return Err("copy_child_failed".to_owned());
            }
            running.join_completed_outputs().await?;
            let (tail, inspection) = mux.finish()?;
            apply(&mut files, tail, &census, &stop).await?;
            files.finish(inspection, &stop).await?;
            Ok::<(), String>(())
        }
        .await;
        // Every pipe task, the Input and the reap join here, whatever the
        // generation observed.
        let settled = running.settle(observed.is_ok()).await;
        observed?;
        if !settled?.exit_success() {
            return Err("copy_child_failed".into());
        }

        // Committed: the whole title's bytes and rows, under the local owner.
        assert!(files.complete());
        let segments = files.segments().to_vec();
        assert!(!segments.is_empty());
        assert!(payload.join("init.mp4").is_file());
        for segment in &segments {
            assert_eq!(
                std::fs::metadata(payload.join(&segment.name))
                    .expect("a committed segment")
                    .len(),
                segment.bytes
            );
        }
        assert!(String::from_utf8(files.playlist())
            .expect("a playlist")
            .ends_with("#EXT-X-ENDLIST\n"));
        let rendered = files
            .mark_rendered(&stop)
            .await?
            .expect("the rendered checkpoint");
        assert!(rendered.first_frame_rendered);
        assert!(rendered.boundaries.begins_title && rendered.boundaries.eof);
        assert_eq!(
            rendered.rows.len(),
            segments.iter().map(|segment| segment.rows).sum::<usize>()
        );
        assert_eq!(rendered.manifest_path.parent(), Some(manifests.as_path()));
        let manifest = std::fs::read(&rendered.manifest_path).expect("the checkpoint manifest");
        assert_eq!(
            hex::encode(Sha256::digest(&manifest)),
            rendered.manifest_sha256
        );
        let owner: OwnerView = serde_json::from_slice(&manifest).expect("the additive reader");
        assert_eq!(owner.owner, "local");
        assert_eq!(owner.local_session.as_deref(), Some(session.as_str()));
        assert_eq!(owner.local_user_id, Some(7));
        assert_eq!(owner.local_playback_id.as_deref(), Some("playback-1"));
        assert!(owner.source_incarnation.is_none());
        assert!(!roots.work.path().join("source-copy-ranges").exists());
        assert!(!roots
            .data
            .path()
            .join("copy-ranges")
            .join("observations")
            .exists());

        // Retired: the payload and the manifests are gone, and the retired
        // publisher commits nothing more.
        files.retire_payload().await?;
        assert!(!payload.exists());
        assert!(!manifests.exists());
        files.retire_payload().await?;
        drop(held);
        Ok(())
    }
}
