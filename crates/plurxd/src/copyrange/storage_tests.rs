//! A range's owner: its directories, its manifest binding and its retirement.
//!
//! The manifest tests hold the Source manifest to the exact bytes the binary
//! before the owner enum wrote, through a verbatim copy of that binary's
//! manifest declaration. No binary reads a checkpoint manifest back today
//! (observations reach the Analysis worker in memory), so that copy and
//! [`OwnerView`] stand for the two readers: the strict one a binary that
//! knows only Source manifests would be, and the additive one that takes a
//! manifest without `owner` to be a Source owner's.
//!
//! A `SourceDispatchAssignment` can only be made by plurx-core's Source
//! session store, so a Source owner's files are a local owner's with the
//! recorded binding replaced; the directory a Source owner derives is checked
//! on the binding itself.
use super::*;

const NAMESPACE: &str = "catalogue";

fn viewer(user_id: i64) -> LocalCopyAuthority {
    LocalCopyAuthority::of(&crate::state::PlaybackViewerDemand {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id },
        playback_id: "playback-1".into(),
    })
    .expect("a local user's own playback")
}

fn source() -> AnalysisCoordinationSource {
    AnalysisCoordinationSource {
        file_id: 7,
        source_size: 4096,
        source_mtime: 77,
        origin_node_id: "origin-node".into(),
        object_version: "object-version".into(),
        video_identity: "ab".repeat(32),
        contract_version: plurx_core::store::FRAGMENT_INDEX_CONTRACT,
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
            PromotionInputs::default(),
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

/// The manifest exactly as the binary before the owner enum declared it,
/// field for field and in its order. It reads strictly, as that declaration
/// asked (`deny_unknown_fields`).
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

const INCARNATION: &str = "3a1c7c0e-6f2b-4f57-9d0a-0d6c1a2b3c4d";
const REQUEST: &str = "request-1";
const OWNER_KEY: &str = "sharing-owner-key";

fn source_binding() -> OwnerBinding {
    OwnerBinding::Source {
        incarnation: INCARNATION.into(),
        request: REQUEST.into(),
        owner_key: OWNER_KEY.into(),
    }
}

/// What the previous binary wrote for the state `files` holds.
fn previous(files: &CopyRangeFiles, range_id: uuid::Uuid) -> PreviousManifest {
    PreviousManifest {
        version: 1,
        catalog_namespace: NAMESPACE.into(),
        source: source(),
        source_incarnation: INCARNATION.into(),
        source_request: REQUEST.into(),
        source_owner_key: OWNER_KEY.into(),
        range_id,
        range_epoch: 1,
        checkpoint_sequence: files.sequence,
        init_contract_sha256: "c".repeat(64),
        canonical_init_sha256: String::new(),
        av_init_sha256: String::new(),
        source_origin_dts: -7500,
        source_time_base_num: 1,
        source_time_base_den: 90000,
        canonical_title_start: 0,
        timescale: 90000,
        media_origin_ms: -83,
        target_duration: 16,
        begins_title: false,
        eof: false,
        first_frame_rendered: false,
        rows: Vec::new(),
        segments: Vec::new(),
        row_inspections: Vec::new(),
        inspection_packets: None,
        inspection_slices: None,
        inspection_configuration_sha256: None,
        promotion: PromotionInputs::default(),
    }
}

/// Both directions of the Source manifest: the new binary writes the bytes
/// the previous one wrote, so each reads the other's.
#[tokio::test]
async fn a_source_manifest_is_the_previous_binarys_in_both_directions() {
    let roots = Roots::new();
    let range_id = uuid::Uuid::new_v4();
    let mut files = roots
        .create(local("session-1"), range_id)
        .await
        .expect("range files");
    files.owner = source_binding();
    let written = files
        .manifest(90000, -83, RangeBoundaries::default())
        .expect("a Source manifest");
    let old = serde_json::to_vec(&previous(&files, range_id)).expect("the previous manifest");
    assert_eq!(
        String::from_utf8(written.clone()).expect("JSON"),
        String::from_utf8(old.clone()).expect("JSON"),
        "a Source manifest gained, lost or moved a field"
    );

    // New, read by the previous binary's strict declaration: nothing unknown.
    let read: PreviousManifest = serde_json::from_slice(&written).expect("the previous reader");
    assert_eq!(read.source_incarnation, INCARNATION);
    assert_eq!(read.source_request, REQUEST);
    assert_eq!(read.source_owner_key, OWNER_KEY);
    assert_eq!(read.range_id, range_id);

    // Old, read additively by the new binary: no `owner` means Source.
    for bytes in [&old, &written] {
        let owner: OwnerView = serde_json::from_slice(bytes).expect("the additive reader");
        assert_eq!(owner.owner, "source");
        assert_eq!(owner.source_incarnation.as_deref(), Some(INCARNATION));
        assert_eq!(owner.source_request.as_deref(), Some(REQUEST));
        assert_eq!(owner.source_owner_key.as_deref(), Some(OWNER_KEY));
        assert!(owner.local_session.is_none());
        assert!(owner.local_user_id.is_none());
        assert!(owner.local_playback_id.is_none());
    }
}

/// A local manifest names its session and viewer and carries no Source
/// binding. A reader that knows only Source manifests refuses it, and it is
/// written where such a reader never looks.
#[tokio::test]
async fn a_local_manifest_names_its_owner_and_is_not_a_source_manifest() {
    let roots = Roots::new();
    let range_id = uuid::Uuid::new_v4();
    let files = roots
        .create(local("session-1"), range_id)
        .await
        .expect("range files");
    let written = files
        .manifest(90000, -83, RangeBoundaries::default())
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

/// A Source owner's directories are the ones it always had.
#[test]
fn a_source_owner_keeps_its_directories() {
    let binding = source_binding();
    assert_eq!(binding.payload(), ("source-copy-ranges", INCARNATION));
    assert_eq!(binding.observations(), "observations");
}

/// A session that is not one plain directory name, and an authority that
/// names no user, make no range and no directory.
#[tokio::test]
async fn a_local_owner_needs_a_plain_session_and_a_named_viewer() {
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
    assert_eq!(
        roots
            .create(
                CopyRangeOwner::Local {
                    session: "session-1".into(),
                    viewer: LocalCopyAuthority::unattributed(),
                },
                uuid::Uuid::new_v4(),
            )
            .await
            .err(),
        Some("copy_range_binding_invalid".to_owned())
    );
    assert!(!roots.work.path().join("local-copy-ranges").exists());
    assert!(!roots.data.path().join("copy-ranges").exists());
}

/// A generation owned by a local playback is refused before any child when
/// its authority names no user. (A dispatch of a Source-assigned rendition is
/// refused the same way; attaching an assignment needs a Source session.)
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_local_generation_needs_a_named_viewer() {
    let owners = crate::source_generation::SourceRenditionOwners::default();
    let nobody = LocalCopyAuthority::unattributed();
    let (input, _held) = crate::source_input::SourceInput::qualification_fixture(b"source").await;
    let cache = crate::test_tempdir().expect("runtime cache");
    let refused = crate::copyrange::spawn_source_generation(
        input,
        Path::new("/films/film.mkv"),
        &["-i".to_owned(), "/films/film.mkv".to_owned()],
        cache.path(),
        crate::copyrange::CopyGenerationOwner::Local {
            owners: &owners,
            dispatch: owners.begin_generation().expect("a local dispatch"),
            viewer: &nobody,
            permit: None,
        },
        Arc::new(crate::prodrun::ProducerSlot::new()),
    )
    .await
    .err();
    assert_eq!(refused, Some("copy_local_owner_invalid".to_owned()));
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
                // run returns, so no ffmpeg outlives the test.
                stop.cancel();
                let settled = run.await;
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
