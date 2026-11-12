//! Gap filling end to end on the real copy engine: every observation here is
//! one real ffmpeg generation run through `copyrange::fill_copy_gap` (head
//! census, triple-output child, observed mux, normalizer, companion verifier,
//! producer slot and reap) against a generated HEVC fixture title.
//!
//! What is not real: the physical phase is an exact, well-formed
//! `AnalysisPhysicalPhase` value that no store admitted, and each Input is the
//! loopback bridge over the fixture's bytes with the test byte budget. The
//! store's phase admission and byte service have their own store tests.
use super::*;
use crate::copy_normalization::CopyNormalizationContract;
use crate::copyrange::{fill_copy_gap, run_source_census, CopyGapFill, CopyGenerationOwner};
use crate::prodrun::ProducerSlot;
use crate::source_input::SourceInput;
use plurx_core::domain::MediaFile;
use plurx_core::segplan::ranges::RangeGap;
use plurx_core::store::analysis_coordination::AnalysisCoordinationClaim;
use plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken;
use plurx_core::transcode::CopyVideoOptions;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const NODE: &str = "origin-node";
const NAMESPACE: &str = "catalogue";

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

/// A fresh loopback Input of the fixture's exact bytes for each child, as a
/// phase's `open` yields. The held files stay alive until the test ends.
fn opener(
    bytes: &Arc<Vec<u8>>,
    held: &Arc<Mutex<Vec<std::fs::File>>>,
) -> impl FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SourceInput, String>>>>
{
    let bytes = Arc::clone(bytes);
    let held = Arc::clone(held);
    move || {
        let bytes = Arc::clone(&bytes);
        let held = Arc::clone(&held);
        Box::pin(async move {
            let (input, file) = SourceInput::qualification_fixture(&bytes).await;
            held.lock().expect("held fixture files").push(file);
            Ok(input)
        })
    }
}

fn observed(rows: &AnalysisCopyRows, timescale: u32) -> ObservedRows<'_> {
    ObservedRows {
        init: &rows.init,
        timescale,
        source_origin: rows.source_origin,
        canonical_title_start: rows.canonical_title_start,
        rows: &rows.rows,
        boundaries: rows.boundaries,
        inspection: rows.inspection.as_ref(),
        row_inspections: &rows.row_inspections,
        promotion: &rows.promotion,
    }
}

fn set_for(
    source: &AnalysisCoordinationSource,
    rows: &AnalysisCopyRows,
    timescale: u32,
) -> CopyObservationSet {
    CopyObservationSet::empty(
        NAMESPACE,
        source,
        &rows.init,
        timescale,
        rows.source_origin,
        rows.canonical_title_start,
        &rows.promotion,
    )
    .expect("an empty set of this source")
}

/// A head range a viewer played and a separately filled gap merge into one
/// complete set whose rows are exactly a single whole-title observation's.
#[tokio::test]
async fn a_played_head_and_a_filled_gap_merge_into_the_whole_title_observation() {
    plurx_core::testfixtures::require_ffmpeg();
    tokio::time::timeout(
        Duration::from_secs(240),
        head_and_gap_equal_the_whole_title(),
    )
    .await
    .expect("three copy generations of a twelve second fixture finish");
}

async fn head_and_gap_equal_the_whole_title() {
    let path = plurx_core::testfixtures::source("clean-cra");
    let bytes = Arc::new(std::fs::read(&path).expect("fixture bytes"));
    let held = Arc::new(Mutex::new(Vec::new()));
    let file = media_file(path);
    let cache = crate::test_tempdir().expect("runtime cache");
    let recipe = CopyRecipe {
        video: CopyVideoOptions::new(false, false),
        audio_index: None,
        transcode_audio: false,
        audio_delivery: None,
    };
    let video_identity = crate::fragindex::identity_for(&file, recipe.video).argv_fingerprint;
    let object_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .expect("fixture object version");
    let source = source_for_copy(NAMESPACE, &file, &video_identity, &object_version, NODE)
        .expect("copy source");
    let claim = AnalysisCoordinationClaim {
        request_id: uuid::Uuid::new_v4().to_string(),
        node_id: NODE.into(),
        fence: 1,
    };
    let phase = AnalysisPhysicalPhase {
        custody: BackgroundSourceCustodyToken {
            custody_id: uuid::Uuid::new_v4().to_string(),
            owner: claim.physical_owner(),
        },
        claim,
        source: source.clone(),
        phase_sequence: 1,
    };
    let stop = CancellationToken::new();

    // The init contract every observation of this source must share, from the
    // same head census and normalization a range's first generation runs.
    let mut open = opener(&bytes, &held);
    let census = run_source_census(
        open().await.expect("census input"),
        object_version.clone(),
        &file,
        recipe.video,
        CopyGenerationOwner::Analysis { phase: &phase },
        Arc::new(ProducerSlot::new()),
        cache.path(),
        None,
        &stop,
    )
    .await
    .expect("head census");
    let timescale = census
        .raw_canonical_init
        .video()
        .expect("fixture video track")
        .timescale;
    let init_contract = CopyNormalizationContract::from_head(
        &file,
        recipe.video,
        &census.raw_canonical_init,
        &census.raw_canonical_fragment,
    )
    .expect("head normalization contract")
    .init_contract_sha256()
    .to_owned();
    let fill = |gap: RangeGap| CopyGapFill {
        file: &file,
        recipe: &recipe,
        phase: &phase,
        init_contract_sha256: &init_contract,
        canonical_title_start: 0,
        timescale,
        gap,
        max_rows: plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
        runtime_cache: cache.path(),
        stop: &stop,
    };

    // One whole-title observation: the reference.
    let whole = fill_copy_gap(
        fill(RangeGap {
            start: None,
            end: None,
        }),
        opener(&bytes, &held),
    )
    .await
    .expect("whole-title generation");
    assert!(whole.boundaries.begins_title && whole.boundaries.eof);
    assert!(
        whole.rows.len() >= 3,
        "the fixture has several GOPs: {}",
        whole.rows.len()
    );
    let mut title = set_for(&source, &whole, timescale);
    title
        .observe_analysis(&phase, &whole)
        .expect("whole-title rows");
    assert!(title.is_complete());
    assert_eq!(title.init_contract_sha256(), init_contract);

    // A playback-style head: from the title start, retired once a later row
    // is verified, as a viewer who stopped watching leaves it.
    let head = fill_copy_gap(
        fill(RangeGap {
            start: None,
            end: Some(whole.rows[1].dts),
        }),
        opener(&bytes, &held),
    )
    .await
    .expect("head generation");
    assert!(head.boundaries.begins_title && !head.boundaries.eof);
    assert!(
        head.rows.len() < whole.rows.len(),
        "the head stops before the title ends: {} of {}",
        head.rows.len(),
        whole.rows.len()
    );
    let mut set = set_for(&source, &head, timescale);
    set.merge(observed(&head, timescale))
        .expect("played head rows");
    assert!(!set.is_complete());
    let gap = set.first_gap().expect("the unplayed tail");
    assert_eq!(
        gap,
        RangeGap {
            start: head.rows.last().map(|row| row.dts + row.duration),
            end: None,
        }
    );

    // The gap, filled separately by a seeked Analysis generation.
    let filled = fill_copy_gap(
        CopyGapFill {
            init_contract_sha256: set.init_contract_sha256(),
            timescale: set.timescale(),
            canonical_title_start: set.canonical_title_start(),
            ..fill(gap)
        },
        opener(&bytes, &held),
    )
    .await
    .expect("gap-fill generation");
    assert!(filled.boundaries.eof && !filled.boundaries.begins_title);
    // Whatever the seek landed on before the gap is a row the head observed,
    // and it is the same row.
    for row in filled
        .rows
        .iter()
        .filter(|row| gap.start.is_some_and(|start| row.dts < start))
    {
        assert!(head.rows.contains(row), "overlap differs at {}", row.dts);
    }
    set.observe_analysis(&phase, &filled)
        .expect("gap rows agree with the head where they overlap");
    assert!(set.is_complete());
    assert_eq!(set.first_gap(), None);
    assert_eq!(set.rows.rows(), whole.rows.as_slice());
    assert_eq!(set.rows.rows(), title.rows.rows());
    assert_eq!(set.inspections, title.inspections);
    assert_eq!(set.init_contract_sha256(), title.init_contract_sha256());

    // Overlap equality is checked, not assumed: the whole title observed
    // again is accepted, and one differing row is refused without a change.
    set.observe_analysis(&phase, &whole)
        .expect("an identical overlapping observation");
    let mut differing = whole.rows.clone();
    differing[0].bytes += 1;
    let contradiction = AnalysisCopyRows {
        init: Arc::clone(&whole.init),
        source_origin: whole.source_origin,
        canonical_title_start: whole.canonical_title_start,
        rows: differing,
        boundaries: whole.boundaries,
        inspection: whole.inspection.clone(),
        row_inspections: whole.row_inspections.clone(),
        promotion: whole.promotion.clone(),
    };
    assert!(matches!(
        set.observe_analysis(&phase, &contradiction),
        Err(CopyObservationError::Coverage)
    ));
    assert_eq!(set.rows.rows(), whole.rows.as_slice());
}
