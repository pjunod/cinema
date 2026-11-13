//! Gap filling end to end on the real copy engine. Every observation here
//! is an Analysis-owned generation run through `copyrange::fill_copy_gap`:
//! its head census child and its primary triple-output child, the observed
//! mux, normalizer, companion verifier, producer slot and reap, against a
//! generated HEVC fixture title. Three fills and the test's own contract
//! census make seven ffmpeg children.
//!
//! What is not real:
//! - The physical phase is an exact, well-formed `AnalysisPhysicalPhase`
//!   value that no store admitted, and each Input is the loopback bridge over
//!   the fixture's bytes with the test byte budget. The store's phase
//!   admission and byte service have their own store tests.
//! - The head is not a `CompletedCopyRange`. That message carries a
//!   `SourceDispatchAssignment` and an `AnalysisSourceInterestAuthority`,
//!   which only plurx-core's Source session store can construct (their fields
//!   are crate-private by design), so building one here needs a whole shared
//!   Source session. The head is instead an Analysis generation retired
//!   early, merged through the same `merge` every observation ends in; the
//!   Source-authority checks of `observe` are not exercised.
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

/// An Analysis generation retired early (the head) and the separately filled
/// gap it leaves merge into one complete set whose rows are exactly a single
/// whole-title Analysis observation's.
#[tokio::test]
async fn an_analysis_filled_head_and_gap_merge_into_the_whole_title_analysis_observation() {
    plurx_core::testfixtures::require_ffmpeg();
    let stop = CancellationToken::new();
    let run = head_and_gap_equal_the_whole_title(stop.clone());
    tokio::pin!(run);
    match tokio::time::timeout(Duration::from_secs(240), &mut run).await {
        Ok(result) => result.expect("the copy generations"),
        Err(_) => {
            // The stop token is every generation's loss token: the running
            // fill retires its child and joins its pipe tasks, Input and
            // reap before it returns, so no ffmpeg outlives the test.
            stop.cancel();
            let settled = run.await;
            panic!("the copy generations did not finish in 240 s; cancelled, they settled with {settled:?}");
        }
    }
}

fn failed(step: &'static str) -> impl Fn(crate::copyrange::CopyGapFillError) -> String {
    move |error| format!("{step}: {error:?}")
}

async fn head_and_gap_equal_the_whole_title(stop: CancellationToken) -> Result<(), String> {
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

    // The init contract every observation of this source must share, from the
    // same head census and normalization a range's first generation runs.
    let mut open = opener(&bytes, &held);
    let census = run_source_census(
        open().await?,
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
    .map_err(|error| format!("head census: {error}"))?;
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
        init_contract_sha256: Some(&init_contract),
        canonical_title_start: 0,
        timescale,
        gap,
        max_rows: plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
        runtime_cache: cache.path(),
        stop: &stop,
    };

    // One whole-title observation: the reference. It is given no contract
    // to check against, as the first generation of a source nothing has
    // observed is (a local viewer's row-less demand), and establishes the
    // same one the census did.
    let whole = fill_copy_gap(
        CopyGapFill {
            init_contract_sha256: None,
            ..fill(RangeGap {
                start: None,
                end: None,
            })
        },
        opener(&bytes, &held),
    )
    .await
    .map_err(failed("whole-title generation"))?;
    assert!(whole.boundaries.begins_title && whole.boundaries.eof);
    assert!(
        whole.rows.len() >= 3,
        "the fixture has several GOPs: {}",
        whole.rows.len()
    );
    let title = CopyObservationSet::from_analysis(NAMESPACE, &source, &phase, &whole)
        .expect("the set a first generation establishes");
    assert!(title.is_complete());
    assert_eq!(title.timescale(), timescale);
    assert_eq!(title.init_contract_sha256(), init_contract);

    // The head: an Analysis generation from the title start, retired once a
    // later row is verified, which leaves the rows a viewer who stopped
    // watching would have left.
    let head = fill_copy_gap(
        fill(RangeGap {
            start: None,
            end: Some(whole.rows[1].dts),
        }),
        opener(&bytes, &held),
    )
    .await
    .map_err(failed("head generation"))?;
    assert!(head.boundaries.begins_title && !head.boundaries.eof);
    assert!(
        head.rows.len() < whole.rows.len(),
        "the head stops before the title ends: {} of {}",
        head.rows.len(),
        whole.rows.len()
    );
    let mut set = set_for(&source, &head, timescale);
    set.merge(observed(&head, timescale)).expect("head rows");
    assert!(!set.is_complete());
    let gap = set.first_gap().expect("the unobserved tail");
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
            init_contract_sha256: Some(set.init_contract_sha256()),
            timescale: set.timescale(),
            canonical_title_start: set.canonical_title_start(),
            ..fill(gap)
        },
        opener(&bytes, &held),
    )
    .await
    .map_err(failed("gap-fill generation"))?;
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
    Ok(())
}
