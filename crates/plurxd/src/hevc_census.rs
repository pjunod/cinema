//! Measure, once per source revision, whether an HEVC file's in-band parameter
//! sets agree with its sample entry, and remember the answer in its probe.
//!
//! [`plurx_core::transcode::hevc_census`] explains why the copy path needs the
//! answer and owns its stored shape. This half opens the media: a handful of
//! single-keyframe copies spread over the film, run concurrently under one
//! budget, then grafted onto the stored probe so every node and every later
//! copy decides from the same record.
//!
//! It runs on the playback copy decision path, the first time a file without a
//! current record is played, because that is where the answer is needed and
//! the file is known to be readable. Background index discovery reads the
//! stored probe without measuring, so the library is never swept; the
//! preparation a play queues names the identity the census chose. Measured on the NAS-mounted
//! library: seven samples of a cold 2160p film cost about a second. A census
//! that fails or overruns records nothing and answers with the stored probe
//! unchanged, which is the copy path's behaviour before this existed. A run
//! with any failed sample is incomplete and can only record a disagreement it
//! actually saw. A failure is not retried for [`RETRY_AFTER_FAILURE`] so a file
//! that cannot be read does not charge every request for the attempt.
use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use plurx_core::domain::MediaFile;
use plurx_core::error::StoreError;
use plurx_core::fmp4::InBandParameterSets;
use plurx_core::store::Store;
use plurx_core::transcode::hevc_census::{self, Census};

/// One keyframe copy. A cold NAS seek plus one 4K keyframe is well under a
/// second; this is the ceiling for a stalled mount.
const SAMPLE_WALL_TIME: Duration = Duration::from_secs(5);
/// Every sample, concurrently. The playback request waits on this at most once
/// per source revision.
const CENSUS_BUDGET: Duration = Duration::from_secs(6);
/// One keyframe of the largest source plus its moov.
const SAMPLE_MAX_BYTES: usize = 64 * 1024 * 1024;
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(600);

/// One census per file at a time: concurrent requests for the same title wait
/// for the first and then read its record.
static IN_FLIGHT: LazyLock<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// A source revision: (file id, size, mtime).
type Revision = (i64, i64, i64);
/// Source revisions whose census recently failed.
static FAILED: LazyLock<Mutex<HashMap<Revision, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The file's stored probe, with its HEVC parameter-set census recorded first
/// when it has none for this revision.
///
/// Every copy path reads its probe through this before building a
/// [`plurx_core::transcode::CopyVideoOptions`], so the decision that keeps or
/// deletes the in-band parameter sets is made from the same record wherever
/// the copy runs. A store error reading the probe is returned; everything
/// about the census itself is best-effort and falls back to the stored probe.
pub(crate) async fn probe_json_for_copy(
    store: &dyn Store,
    file: &MediaFile,
) -> Result<Option<String>, StoreError> {
    let probe_json = store.get_file_probe_json(file.id).await?;
    if !hevc_census::needs_census(file, probe_json.as_deref()) || recently_failed(file) {
        return Ok(probe_json);
    }
    let lock = {
        let mut in_flight = IN_FLIGHT
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Arc::clone(in_flight.entry(file.id).or_default())
    };
    let measuring = lock.lock().await;
    let answer = measure_and_record(store, file).await;
    drop(measuring);
    // Nobody else waits on this file's lock: forget it, so the map holds only
    // files being measured rather than every HEVC title ever played.
    {
        let mut in_flight = IN_FLIGHT
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if in_flight
            .get(&file.id)
            .is_some_and(|held| Arc::ptr_eq(held, &lock) && Arc::strong_count(held) == 2)
        {
            in_flight.remove(&file.id);
        }
    }
    answer
}

async fn measure_and_record(
    store: &dyn Store,
    file: &MediaFile,
) -> Result<Option<String>, StoreError> {
    // Another request may have recorded it while this one waited.
    let probe_json = store.get_file_probe_json(file.id).await?;
    if !hevc_census::needs_census(file, probe_json.as_deref()) || recently_failed(file) {
        return Ok(probe_json);
    }
    let started = Instant::now();
    let Some(census) = measure(file).await else {
        note_failure(file);
        return Ok(probe_json);
    };
    match store
        .merge_file_probe_hevc_parameter_sets(file.id, file.size, file.mtime, &census.to_json())
        .await
    {
        Ok(true) => {
            tracing::info!(
                file_id = file.id,
                verdict = ?census.verdict,
                samples = census.samples,
                differing = census.differing,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "HEVC parameter-set census recorded"
            );
            store.get_file_probe_json(file.id).await
        }
        Ok(false) => {
            // The row no longer describes the revision measured: a rescan
            // replaced it, or its probe was cleared. The next copy measures
            // the revision it finds.
            tracing::info!(
                file_id = file.id,
                "HEVC parameter-set census not recorded: the file changed while it was measured"
            );
            note_failure(file);
            Ok(probe_json)
        }
        Err(error) => {
            tracing::warn!(file_id = file.id, %error, "recording the HEVC parameter-set census");
            note_failure(file);
            Ok(probe_json)
        }
    }
}

fn recently_failed(file: &MediaFile) -> bool {
    let mut failed = FAILED.lock().unwrap_or_else(|poison| poison.into_inner());
    failed.retain(|_, at| at.elapsed() < RETRY_AFTER_FAILURE);
    failed.contains_key(&(file.id, file.size, file.mtime))
}

fn note_failure(file: &MediaFile) {
    FAILED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert((file.id, file.size, file.mtime), Instant::now());
}

/// Sample the film and conclude, or `None` when nothing conclusive came back.
async fn measure(file: &MediaFile) -> Option<Census> {
    let mut samples = tokio::task::JoinSet::new();
    for at in hevc_census::sample_points(file.duration_ms) {
        let args = hevc_census::sample_args(&file.path, at);
        samples.spawn(sample(args));
    }
    let deadline = tokio::time::Instant::now() + CENSUS_BUDGET;
    let mut observations = Vec::new();
    let mut failures = Vec::new();
    let mut timed_out = false;
    loop {
        match tokio::time::timeout_at(deadline, samples.join_next()).await {
            Ok(Some(Ok(Ok(observed)))) => observations.push(observed),
            Ok(Some(Ok(Err(reason)))) => failures.push(reason),
            Ok(Some(Err(joined))) => failures.push(joined.to_string()),
            Ok(None) => break,
            Err(_) => {
                samples.abort_all();
                timed_out = true;
                break;
            }
        }
    }
    conclude(file, &observations, &failures, timed_out)
}

/// What a run of samples may record.
///
/// A sample that failed is a part of the film the census did not see, so the
/// run is incomplete whether it failed fast or overran the budget: a later
/// keyframe is exactly where a chunk-encoded film redefines what its opening
/// one repeated. Only a disagreement survives a partial run.
fn conclude(
    file: &MediaFile,
    observations: &[InBandParameterSets],
    failures: &[String],
    timed_out: bool,
) -> Option<Census> {
    let complete = !timed_out && failures.is_empty();
    let census = Census::from_observations(file, observations, complete);
    if census.is_none() {
        tracing::warn!(
            file_id = file.id,
            observed = observations.len(),
            complete,
            failures = ?failures,
            "HEVC parameter-set census was inconclusive; copying with the stored decision"
        );
    } else if !failures.is_empty() {
        tracing::debug!(file_id = file.id, failures = ?failures, "HEVC census samples failed");
    }
    census
}

async fn sample(args: Vec<OsString>) -> Result<InBandParameterSets, String> {
    let output = plurx_core::process::bounded::output(
        crate::ffmpeg::ffmpeg_bin(),
        &args,
        SAMPLE_WALL_TIME,
        SAMPLE_MAX_BYTES,
        // A viewer is waiting on the copy decision this answers.
        plurx_core::process::ChildWork::realtime("HEVC parameter-set census"),
    )
    .await
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg exited {:?}: {}",
            output.status.code(),
            stderr.lines().last().unwrap_or_default()
        ));
    }
    hevc_census::observe(&output.stdout)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use plurx_core::fmp4::{FragmentReader, Segmenter, Unit};
    use plurx_core::testfixtures;
    use plurx_core::transcode::{self, CopyVideoOptions};
    use std::process::Command;

    /// A real encode whose PPS 0 is redefined halfway through, the shape of
    /// the chunk-encoded WEB-DL that painted UNABOMBER pink and green: two
    /// x265 passes with different chroma QP offsets, each repeating its
    /// headers at every keyframe, joined into one Matroska track whose
    /// `hvcC` keeps the first pass's definition.
    fn changing_pps_fixture(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        testfixtures::require_ffmpeg();
        for (name, cb, cr) in [("first", -6, -8), ("second", -1, -2)] {
            let mut encode = Command::new(testfixtures::ffmpeg());
            encode
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=160x96:rate=24:duration=2",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:v",
                    "libx265",
                    "-preset",
                    "ultrafast",
                    "-x265-params",
                ])
                .arg(format!(
                    "keyint=24:min-keyint=24:open-gop=0:bframes=0:repeat-headers=1:\
                     scenecut=0:cbqpoffs={cb}:crqpoffs={cr}:log-level=none:pools=1"
                ))
                .arg(dir.join(format!("{name}.mkv")));
            testfixtures::run(&mut encode);
        }
        let list = dir.join("concat.txt");
        std::fs::write(&list, "file 'first.mkv'\nfile 'second.mkv'\n").expect("concat list");
        let changing = dir.join("changing.mkv");
        let mut mux = Command::new(testfixtures::ffmpeg());
        mux.args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-c", "copy"])
            .arg(&changing);
        testfixtures::run(&mut mux);
        (dir.join("first.mkv"), changing)
    }

    fn hevc_file(path: &std::path::Path, duration_ms: i64) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 4190,
            item_id: 1,
            path: path.to_owned(),
            size: std::fs::metadata(path).expect("fixture").len() as i64,
            mtime: 1,
            duration_ms: Some(duration_ms),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn hevc_file_at(path: std::path::PathBuf) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 4191,
            item_id: 1,
            path,
            size: 1,
            mtime: 1,
            duration_ms: Some(1_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    /// Decoded-frame MD5s, in presentation order.
    fn frame_hashes(path: &std::path::Path) -> Vec<String> {
        let out = Command::new(testfixtures::ffmpeg())
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-f", "framemd5", "-"])
            .output()
            .expect("decoding a fixture");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("framemd5 is text")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| line.rsplit(',').next().expect("hash").trim().to_owned())
            .collect()
    }

    /// Run the production copy argv for `options`, cut it with the production
    /// segmenter exactly as a copy session does, and write the init plus
    /// every segment as one playable MP4.
    fn served_copy(
        file: &MediaFile,
        options: CopyVideoOptions,
        out: &std::path::Path,
    ) -> std::path::PathBuf {
        let args = transcode::copy_index_pipe_args(file, options);
        let output = Command::new(testfixtures::ffmpeg())
            .args(&args)
            .output()
            .expect("running the copy pipe");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut reader = FragmentReader::new();
        reader.push(&output.stdout);
        let mut served = Vec::new();
        let mut segmenter: Option<Segmenter> = None;
        while let Some(unit) = reader.next_unit().expect("reading the copy pipe") {
            match unit {
                Unit::Init(init) => {
                    served.extend_from_slice(&init.bytes);
                    let timescale = init.video().map_or(1, |video| video.timescale);
                    let policy = plurx_core::fmp4::CutPolicy::new(2, 2, 64 << 20, 6, timescale);
                    segmenter = Some(
                        Segmenter::new(init, policy)
                            .retaining_hevc_parameter_sets(options.retains_hevc_parameter_sets()),
                    );
                }
                Unit::Fragment(fragment) => {
                    let segmenter = segmenter.as_mut().expect("init first");
                    if let Some(published) = segmenter.push(fragment).expect("segmenting") {
                        served.extend_from_slice(&published.segment.bytes);
                    }
                }
                Unit::Trailer => {}
            }
        }
        for published in segmenter.expect("init").finish().expect("finishing") {
            served.extend_from_slice(&published.segment.bytes);
        }
        std::fs::write(out, &served).expect("writing the served copy");
        out.to_owned()
    }

    #[tokio::test]
    async fn a_redefined_pps_is_measured_retained_and_decodes_to_the_source_pixels() {
        let temp = crate::test_tempdir().expect("tempdir");
        let (stable_path, changing_path) = changing_pps_fixture(temp.path());
        let stable = hevc_file(&stable_path, 2_000);
        let changing = hevc_file(&changing_path, 4_000);

        // The census: a film that only repeats its record agrees with it; one
        // that redefines PPS 0 does not.
        let stable_census = measure(&stable).await.expect("stable census");
        assert_eq!(stable_census.verdict, hevc_census::Verdict::Constant);
        let changing_census = measure(&changing).await.expect("changing census");
        assert_eq!(changing_census.verdict, hevc_census::Verdict::Varying);
        assert!(changing_census.differing >= 1, "{changing_census:?}");

        // The record reaches the copy decision through the stored probe.
        let probe = format!(
            r#"{{"streams":[],"{}":{}}}"#,
            hevc_census::PROBE_KEY,
            changing_census.to_json()
        );
        let retaining = CopyVideoOptions::from_probe(&changing, Some(&probe), false, false);
        assert!(retaining.retains_hevc_parameter_sets());
        let historical = CopyVideoOptions::from_probe(&changing, Some("{}"), false, false);
        assert!(!historical.retains_hevc_parameter_sets());
        let argv = transcode::copy_video_args(&changing, retaining).join(" ");
        assert!(
            !argv.contains("32-34"),
            "retained copies keep types 32-34: {argv}"
        );

        // Pixels. The source decodes one way; the historical copy deletes the
        // redefinition and decodes the second half against the stale PPS; the
        // retaining copy is frame-for-frame the source.
        let source = frame_hashes(&changing_path);
        let deleted = frame_hashes(&served_copy(
            &changing,
            historical,
            &temp.path().join("deleted.mp4"),
        ));
        let kept = frame_hashes(&served_copy(
            &changing,
            retaining,
            &temp.path().join("kept.mp4"),
        ));
        assert_eq!(source.len(), 96, "two seconds at 24 fps, twice");
        assert_ne!(deleted, source, "the fixture must reproduce the corruption");
        assert_eq!(
            kept, source,
            "retained parameter sets must decode to the source"
        );

        // And a file that only repeats its record keeps today's bytes: the
        // census says constant, so the argv is the historical one.
        let stable_probe = format!(
            r#"{{"streams":[],"{}":{}}}"#,
            hevc_census::PROBE_KEY,
            stable_census.to_json()
        );
        assert_eq!(
            CopyVideoOptions::from_probe(&stable, Some(&stable_probe), false, false),
            CopyVideoOptions::from_probe(&stable, Some("{}"), false, false),
        );
    }

    #[tokio::test]
    async fn a_retained_film_still_indexes_as_vod_presentable() {
        // The index compares each clean fragment's promotion inputs. With the
        // redefinitions travelling in band, those carry different PPS bytes
        // at every shot change; promotion never copies a type the record
        // already configures, so they must not count as a disagreement.
        let temp = crate::test_tempdir().expect("tempdir");
        let (_, changing_path) = changing_pps_fixture(temp.path());
        let changing = hevc_file(&changing_path, 4_000);
        let census = measure(&changing).await.expect("census");
        let probe = format!(
            r#"{{"streams":[],"{}":{}}}"#,
            hevc_census::PROBE_KEY,
            census.to_json()
        );
        let retaining = CopyVideoOptions::from_probe(&changing, Some(&probe), false, false);
        assert!(retaining.retains_hevc_parameter_sets());
        let outcome =
            crate::fragindex::build(&changing, retaining, temp.path(), Duration::from_secs(60))
                .await;
        let crate::fragindex::IndexOutcome::Built(index) = outcome else {
            panic!("a retaining copy must index: {outcome:?}");
        };
        assert!(index.parameter_sets_constant, "{:?}", index.promotion);
        assert!(
            index.promotion.parameter_sets.is_empty(),
            "{:?}",
            index.promotion
        );
    }

    #[test]
    fn a_census_with_a_failed_sample_cannot_conclude_constant() {
        use InBandParameterSets::*;
        let file = hevc_file_at(std::path::PathBuf::from("/m/film.mkv"));
        let failed = ["ffmpeg exited Some(1): seek failed".to_owned()];
        // Every sample agreed and none failed: agreement.
        assert_eq!(
            conclude(&file, &[MatchSampleEntry, Absent], &[], false).map(|c| c.verdict),
            Some(hevc_census::Verdict::Constant)
        );
        // The opening keyframe agreed but a later one was never seen.
        assert_eq!(conclude(&file, &[MatchSampleEntry], &failed, false), None);
        // Or the budget ran out before it was.
        assert_eq!(conclude(&file, &[MatchSampleEntry], &[], true), None);
        // A disagreement that was seen stands either way.
        assert_eq!(
            conclude(
                &file,
                &[MatchSampleEntry, DifferFromSampleEntry],
                &failed,
                true
            )
            .map(|c| c.verdict),
            Some(hevc_census::Verdict::Varying)
        );
    }

    #[tokio::test]
    async fn an_unreadable_file_records_nothing_and_is_not_retried_at_once() {
        let temp = crate::test_tempdir().expect("tempdir");
        let missing = temp.path().join("gone.mkv");
        let mut file = hevc_file_at(missing);
        file.id = 4191;
        testfixtures::require_ffmpeg();
        assert_eq!(measure(&file).await, None);
        note_failure(&file);
        assert!(recently_failed(&file));
        let mut replaced = file.clone();
        replaced.mtime += 1;
        assert!(
            !recently_failed(&replaced),
            "a new revision is measured afresh"
        );
    }
}
