//! Whether a source's in-band HEVC parameter sets agree with its sample entry.
//!
//! Every copied HEVC stream is tagged `hvc1`/`dvh1`, and the copy path has
//! always deleted the in-band VPS/SPS/PPS (NAL types 32-34) on the assumption
//! that the decoder configuration record — the source's `hvcC` — already says
//! everything those units say. For a Blu-ray remux that is true: the in-band
//! sets are byte-for-byte repeats of the record, and deleting them is what
//! fixed the 4K boundary stutter (`docs/streaming/STUTTER-4K.md`).
//!
//! It is false for a chunk-encoded streaming WEB-DL. UNABOMBER (2026) redefines
//! PPS 0 at shot boundaries — 301 transitions between four variants across the
//! film, chroma QP offsets −4/−5, −1/−2, 0/0 against an `hvcC` that says
//! −6/−8 — and repeats the current definition at every keyframe. Deleting the
//! in-band sets leaves every picture decoded against the record's stale PPS,
//! which paints pink and green blotches over the frame while the samples
//! themselves are copied unchanged
//! (`docs/streaming/HEVC-IN-BAND-PARAMETER-SETS.md`).
//!
//! So the copy path needs one fact per source revision: do the in-band sets
//! ever disagree with the record? This module owns that fact — its shape, where
//! it lives, and how it is read. It lives in the file's stored probe JSON under
//! [`PROBE_KEY`], beside the ffprobe facts that already decide the copy argv
//! (`extradata_size` for the empty-`hvcC` promotion), so every node and every
//! copy path reads the same answer from the replicated row with no new column
//! and no new index identity for the files where the answer is "they agree".
//!
//! The measurement itself — a handful of single-keyframe copies spread over
//! the film — runs in the daemon (`plurxd::hevc_census`), because it opens
//! media.
use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::MediaFile;
use crate::fmp4::{self, FragmentReader, InBandParameterSets, Unit};

/// The stored probe document's key for the census record.
pub const PROBE_KEY: &str = "plurx_hevc_parameter_sets";

/// Bumped when the measurement changes meaning. A record from another revision
/// is treated as absent, so the next copy of that file measures it again.
pub const REVISION: u32 = 1;

/// Where in the film the census looks, as fractions of its duration.
///
/// Spread rather than clustered: the files this exists for change their
/// parameter sets at shot boundaries all the way through, and the census only
/// has to catch one disagreement to be right. The opening keyframe is always
/// included because it is the one a short film or an unknown duration still
/// has.
const SAMPLE_FRACTIONS: [f64; 7] = [0.0, 0.1, 0.25, 0.4, 0.55, 0.7, 0.85];

/// What the census concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every observed in-band parameter set repeated the sample entry's, or
    /// the samples carried none. Deleting them loses nothing.
    Constant,
    /// At least one observed in-band parameter set redefines what the sample
    /// entry says. The decoder has to receive them.
    Varying,
}

/// The stored census record for one source revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Census {
    pub revision: u32,
    pub verdict: Verdict,
    /// The source revision measured. A record whose size or mtime no longer
    /// matches the file is stale and reads as absent.
    pub size: i64,
    pub mtime: i64,
    /// Keyframes observed, and how many of them disagreed with the record.
    pub samples: u32,
    pub differing: u32,
}

impl Census {
    /// Conclude from what the sampled keyframes showed.
    ///
    /// One disagreement is conclusive whether or not every sample finished: a
    /// definition the record does not contain cannot be un-observed. Agreement
    /// is only concluded from a complete run with at least one observation,
    /// because an interrupted census that saw nothing wrong has not shown that
    /// nothing is wrong.
    pub fn from_observations(
        source: &MediaFile,
        observations: &[InBandParameterSets],
        complete: bool,
    ) -> Option<Census> {
        let differing = observations
            .iter()
            .filter(|observed| **observed == InBandParameterSets::DifferFromSampleEntry)
            .count();
        let verdict = if differing > 0 {
            Verdict::Varying
        } else if complete && !observations.is_empty() {
            Verdict::Constant
        } else {
            return None;
        };
        Some(Census {
            revision: REVISION,
            verdict,
            size: source.size,
            mtime: source.mtime,
            samples: u32::try_from(observations.len()).unwrap_or(u32::MAX),
            differing: u32::try_from(differing).unwrap_or(u32::MAX),
        })
    }

    /// The record as the JSON value stored under [`PROBE_KEY`].
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a census record always serializes")
    }
}

fn is_hevc(source: &MediaFile) -> bool {
    matches!(source.video_codec.as_deref(), Some("hevc" | "h265"))
}

/// The stored census for exactly this source revision, if there is one.
pub fn stored(source: &MediaFile, probe_json: Option<&str>) -> Option<Census> {
    if !is_hevc(source) {
        return None;
    }
    let raw = probe_json?;
    // Most probes carry no record; skip parsing a large document for them.
    if !raw.contains(PROBE_KEY) {
        return None;
    }
    let document: serde_json::Value = serde_json::from_str(raw).ok()?;
    let census: Census = serde_json::from_value(document.get(PROBE_KEY)?.clone()).ok()?;
    (census.revision == REVISION && census.size == source.size && census.mtime == source.mtime)
        .then_some(census)
}

/// Whether a copy of this file should measure it before deciding its argv.
///
/// Only HEVC with a stored probe: the record is grafted onto that document,
/// and a file whose probe never succeeded has nothing to graft onto.
pub fn needs_census(source: &MediaFile, probe_json: Option<&str>) -> bool {
    is_hevc(source) && probe_json.is_some() && stored(source, probe_json).is_none()
}

/// Whether this file's copies must keep their in-band parameter sets.
///
/// False without a current record, which is the copy path's historical
/// behaviour; the daemon measures before it reads this on every copy path.
pub fn in_band_parameter_sets_vary(source: &MediaFile, probe_json: Option<&str>) -> bool {
    stored(source, probe_json).is_some_and(|census| census.verdict == Verdict::Varying)
}

/// Film times, in seconds, at which the census samples one keyframe each.
pub fn sample_points(duration_ms: Option<i64>) -> Vec<f64> {
    let Some(duration_ms) = duration_ms.filter(|duration| *duration > 0) else {
        return vec![0.0];
    };
    let seconds = duration_ms as f64 / 1_000.0;
    let mut points: Vec<f64> = Vec::with_capacity(SAMPLE_FRACTIONS.len());
    for fraction in SAMPLE_FRACTIONS {
        let at = (seconds * fraction * 1_000.0).floor() / 1_000.0;
        if points.last().is_none_or(|last| at > *last) {
            points.push(at);
        }
    }
    points
}

/// The ffmpeg argv that copies the keyframe at or before `at_seconds` into a
/// fragmented MP4 on stdout.
///
/// No bitstream filter, deliberately: the question is what the source's own
/// samples carry. `-frames:v 1` bounds the read to one packet, and `V:0`
/// (capital) skips an attached cover picture the way the scan probe does.
pub fn sample_args(path: &Path, at_seconds: f64) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-nostdin", "-v", "error"]
        .into_iter()
        .map(OsString::from)
        .collect();
    if at_seconds > 0.0 {
        args.push("-ss".into());
        args.push(format!("{at_seconds:.3}").into());
    }
    args.push("-i".into());
    args.push(path.as_os_str().to_owned());
    for arg in [
        "-map",
        "0:V:0",
        "-frames:v",
        "1",
        "-an",
        "-sn",
        "-dn",
        "-c:v",
        "copy",
        "-tag:v",
        "hvc1",
        "-f",
        "mp4",
        "-movflags",
        "frag_keyframe+empty_moov+default_base_moof",
        "pipe:1",
    ] {
        args.push(arg.into());
    }
    args
}

/// Read one sample's output and say how its in-band parameter sets compare
/// with the sample entry it was muxed under.
pub fn observe(output: &[u8]) -> Result<InBandParameterSets, String> {
    let mut reader = FragmentReader::new();
    reader.push(output);
    let mut init = None;
    loop {
        match reader.next_unit() {
            Ok(Some(Unit::Init(parsed))) => init = Some(parsed),
            Ok(Some(Unit::Fragment(fragment))) => {
                let Some(init) = init.as_ref() else {
                    return Err("a fragment arrived before the moov".into());
                };
                return fmp4::compare_in_band_parameter_sets(init, &fragment)
                    .map_err(|error| error.to_string());
            }
            Ok(Some(Unit::Trailer)) => {}
            Ok(None) => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(if init.is_some() {
        "the sample carried no video fragment".into()
    } else {
        "the sample carried no moov".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hevc() -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 4190,
            item_id: 1,
            path: std::path::PathBuf::from("/m/film.mkv"),
            size: 10_558_879_147,
            mtime: 1_790_348_718,
            duration_ms: Some(6_006_016),
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

    fn probe_with(census: &Census) -> String {
        format!(
            r#"{{"streams":[{{"codec_type":"video","codec_name":"hevc"}}],"{PROBE_KEY}":{}}}"#,
            census.to_json()
        )
    }

    #[test]
    fn one_disagreement_is_conclusive_but_agreement_needs_a_complete_run() {
        use InBandParameterSets::*;
        let file = hevc();
        let varying =
            Census::from_observations(&file, &[MatchSampleEntry, DifferFromSampleEntry], false)
                .expect("a disagreement is conclusive even from an interrupted run");
        assert_eq!(varying.verdict, Verdict::Varying);
        assert_eq!((varying.samples, varying.differing), (2, 1));

        let constant = Census::from_observations(&file, &[MatchSampleEntry, Absent], true)
            .expect("a complete run that agreed");
        assert_eq!(constant.verdict, Verdict::Constant);

        assert_eq!(
            Census::from_observations(&file, &[MatchSampleEntry], false),
            None,
            "an interrupted run that saw nothing wrong has not shown nothing is wrong"
        );
        assert_eq!(Census::from_observations(&file, &[], true), None);
    }

    #[test]
    fn a_record_counts_only_for_the_revision_it_measured() {
        let file = hevc();
        let census =
            Census::from_observations(&file, &[InBandParameterSets::DifferFromSampleEntry], true)
                .expect("census");
        let probe = probe_with(&census);
        assert!(in_band_parameter_sets_vary(&file, Some(&probe)));
        assert!(!needs_census(&file, Some(&probe)));

        // A replaced file keeps its row but not its answer.
        let mut replaced = file.clone();
        replaced.mtime += 1;
        assert!(!in_band_parameter_sets_vary(&replaced, Some(&probe)));
        assert!(needs_census(&replaced, Some(&probe)));

        // A record from another analyzer revision is absent, not believed.
        let mut old = census.clone();
        old.revision = REVISION + 1;
        let probe = probe_with(&old);
        assert!(!in_band_parameter_sets_vary(&file, Some(&probe)));
        assert!(needs_census(&file, Some(&probe)));

        // Not HEVC, or no probe to graft onto: nothing to measure.
        let mut h264 = file.clone();
        h264.video_codec = Some("h264".into());
        assert!(!needs_census(&h264, Some("{}")));
        assert!(!needs_census(&file, None));
        assert!(needs_census(&file, Some("{}")));
    }

    #[test]
    fn sample_points_spread_over_the_film_and_fall_back_to_the_opening() {
        assert_eq!(sample_points(None), vec![0.0]);
        assert_eq!(sample_points(Some(0)), vec![0.0]);
        let points = sample_points(Some(6_006_016));
        assert_eq!(points.len(), SAMPLE_FRACTIONS.len());
        assert_eq!(points[0], 0.0);
        assert!(
            points.windows(2).all(|pair| pair[0] < pair[1]),
            "{points:?}"
        );
        assert!(*points.last().expect("points") < 6_006.016);
        // A film too short to separate the fractions does not sample one
        // keyframe twice.
        let short = sample_points(Some(3));
        assert!(short.windows(2).all(|pair| pair[0] < pair[1]), "{short:?}");
    }

    #[test]
    fn the_sample_argv_copies_one_unfiltered_keyframe() {
        let args: Vec<String> = sample_args(Path::new("/m/film.mkv"), 600.0)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(joined.contains("-ss 600.000 -i /m/film.mkv"), "{joined}");
        assert!(joined.contains("-map 0:V:0 -frames:v 1"), "{joined}");
        assert!(!joined.contains("-bsf"), "{joined}");
        assert!(!sample_args(Path::new("/m/film.mkv"), 0.0)
            .iter()
            .any(|arg| arg == "-ss"));
    }
}
