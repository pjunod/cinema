//! Media inspection via `ffprobe`.
//!
//! ffprobe-as-subprocess is the pragmatic ground truth every shipping media
//! server uses (ARCHITECTURE §4): it reliably reports codec profiles, bit
//! depth, HDR transfer characteristics, audio layouts, and subtitle streams
//! across every container. The raw JSON is retained verbatim so the Phase 2
//! decision engine can consult fields we don't model yet.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::domain::{AudioStream, DolbyVisionFacts, ProbeResult, SubtitleStream};
use crate::error::ProbeError;

/// The ffprobe binary name; overridable via `PLURX_FFPROBE` for jellyfin-ffmpeg
/// or a pinned path.
fn ffprobe_bin() -> String {
    std::env::var("PLURX_FFPROBE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ffprobe".to_owned())
}

/// A scan probe may read a large media file across a cold NAS mount. This is a
/// hang ceiling, not a healthy-probe latency target.
const SCAN_PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Sixteen times the largest observed streams-and-chapters document, while
/// still keeping a flooding child bounded.
const SCAN_PROBE_MAX_BYTES: usize = 16 * 1024 * 1024;

struct ProbeInvocation {
    program: OsString,
    args: Vec<OsString>,
    wall_time: Duration,
    reporter: Option<String>,
    #[cfg(test)]
    synthetic_stdout: Option<Vec<u8>>,
}

fn probe_invocation(path: &Path) -> ProbeInvocation {
    #[cfg(test)]
    if let Some(invocation) = tests::fixture_invocation(path) {
        return invocation;
    }

    let program = ffprobe_bin();
    ProbeInvocation {
        program: program.clone().into(),
        args: vec![
            "-v".into(),
            "error".into(),
            "-print_format".into(),
            "json".into(),
            "-show_format".into(),
            "-show_streams".into(),
            // Chapters ride along here because the alternative is probing for
            // them when someone presses Play — a second ffprobe of a
            // NAS-mounted file on the click-to-first-frame path, paid every
            // single time, for data that cannot change between scans. A file
            // with no chapters still reports `"chapters": []`, so the key's
            // *presence* is what tells a current probe from one taken before
            // this landed (see `markers_for` in plurxd).
            "-show_chapters".into(),
            path.as_os_str().to_owned(),
        ],
        wall_time: SCAN_PROBE_TIMEOUT,
        reporter: Some(program),
        #[cfg(test)]
        synthetic_stdout: None,
    }
}

/// The field a probe document carries to name the FFprobe build that wrote
/// it. A document without one was written by a build that did not stamp, so
/// the reporter is unknown — which is a distinct value from any known build,
/// never a wildcard that matches one.
pub const REPORTER_FIELD: &str = "plurx_probe_reporter";

/// A reporter name longer than this is truncated. The string comes from an
/// external executable and is stored on every file row.
const REPORTER_MAX_CHARS: usize = 160;

/// A capability probe, not a media read: it either answers at once or it is
/// not going to.
const REPORTER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// `ffprobe -version` prints a few hundred bytes. This is a runaway guard.
const REPORTER_PROBE_MAX_BYTES: u64 = 64 * 1024;

type ReporterCache = std::sync::OnceLock<
    tokio::sync::Mutex<std::collections::HashMap<String, Option<&'static str>>>,
>;
static REPORTERS: ReporterCache = std::sync::OnceLock::new();

/// How one FFprobe build names itself, probed once per executable.
///
/// Two builds describe the same bytes differently — they add derived labels,
/// drop container tags, and estimate durations and bitrates to different
/// precision — so a document is only comparable field-for-field with another
/// the same build produced. Recording which build spoke is what makes that
/// answerable instead of guessed.
///
/// The executable is a parameter rather than this module's own default
/// because the daemon resolves its FFprobe differently (a bundled binary
/// beside the server takes precedence there). A stamp that named a build the
/// document did not come from would be worse than no stamp at all: the
/// comparison would call two reporters one.
pub async fn reporter_identity_of(bin: &str) -> Option<&'static str> {
    // Keyed by the executable's object version, not by its path: an in-place
    // upgrade of FFprobe would otherwise keep stamping fresh documents with
    // the name of the build that was replaced, which is worse than not
    // stamping at all — it would call two reporters one.
    let key = format!("{bin}\0{}", executable_version(bin));
    {
        let cache = REPORTERS.get_or_init(Default::default).lock().await;
        if let Some(known) = cache.get(&key) {
            return *known;
        }
    }
    // Probed with the lock released: this runs on the click-to-play path, and
    // a stalled executable on a hung mount must not queue every other probe
    // and scan in the process behind it.
    let identity = probe_reporter_identity(bin).await;
    // One leak per distinct FFprobe build, of which a process sees one or two.
    // It buys every caller a plain `&'static str` with no clone.
    let identity = identity.map(|name| &*Box::leak(name.into_boxed_str()));
    if identity.is_some() {
        // A failure is never cached. It is nearly always transient or fatal
        // in a way that spawning again reports immediately, and remembering
        // it would leave documents unstamped for the life of the process.
        REPORTERS
            .get_or_init(Default::default)
            .lock()
            .await
            .insert(key, identity);
    }
    identity
}

/// What this process has already read for `bin`, without reading it now.
///
/// An advisory readout must not spawn a subprocess to answer a question about
/// what this node has been doing: a node that has probed nothing has nothing
/// to report, and saying so is the honest answer. The outer `None` means
/// nothing has been probed yet; the inner one means the probe was taken and
/// the executable could not name itself.
pub async fn known_reporter_identity_of(bin: &str) -> Option<Option<&'static str>> {
    let key = format!("{bin}\0{}", executable_version(bin));
    REPORTERS.get()?.lock().await.get(&key).copied()
}

/// Enough of an executable to notice it was replaced. A name resolved through
/// `PATH` has no metadata to read and reports as unresolved, which simply
/// means its identity is cached for the life of the process.
fn executable_version(bin: &str) -> String {
    let Ok(metadata) = std::fs::metadata(bin) else {
        return "unresolved".to_owned();
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("{}:{modified}", metadata.len())
}

/// `ffprobe -version` is a subprocess on the interactive session-start path,
/// so it is bounded exactly like every other capability probe this server
/// spawns: a deadline, a read bound, and a child that dies with its future.
async fn probe_reporter_identity(bin: &str) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new(bin)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut text = String::new();
    let read = tokio::time::timeout(
        REPORTER_PROBE_TIMEOUT,
        (&mut stdout)
            .take(REPORTER_PROBE_MAX_BYTES)
            .read_to_string(&mut text),
    )
    .await;
    // Whatever it had to say has been said or has run out of time. Nothing
    // here waits on a pipe this function stopped reading.
    let _ = child.start_kill();
    let _ = tokio::time::timeout(REPORTER_PROBE_TIMEOUT, child.wait()).await;
    read.ok()?.ok()?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(REPORTER_MAX_CHARS).collect::<String>())
}

/// Record which FFprobe build produced this document.
pub fn stamp_reporter(document: &mut Value, reporter: &str) {
    if let Some(fields) = document.as_object_mut() {
        fields.insert(
            REPORTER_FIELD.to_owned(),
            Value::String(reporter.chars().take(REPORTER_MAX_CHARS).collect()),
        );
    }
}

/// Which FFprobe build produced this document, if it says.
pub fn reporter_of(document: &Value) -> Option<&str> {
    document.get(REPORTER_FIELD).and_then(Value::as_str)
}

/// Distill ffprobe's stderr into one line for the scan report. ffprobe prefixes
/// most complaints with the input path, which the report already prints, so the
/// last non-empty line's text after the final `: ` is the useful part.
fn probe_failure_reason(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let last = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    if last.is_empty() {
        return "ffprobe gave no reason".to_owned();
    }
    // "…/file.mkv: Permission denied" → "Permission denied", but a line with no
    // path prefix is kept whole.
    match last.rsplit_once(": ") {
        Some((_, reason)) if !reason.is_empty() => reason.to_owned(),
        _ => last.to_owned(),
    }
}

/// Probe a file. Returns a best-effort [`ProbeResult`]; unreadable/duration-less
/// files still yield a result (with `None` fields) rather than an error, so a
/// weird file doesn't abort a scan. Errors are reserved for ffprobe being
/// missing or emitting unparseable output.
pub async fn probe(path: &Path) -> Result<ProbeResult, ProbeError> {
    // `-v error` rather than `-v quiet`: on success stderr stays empty, and on
    // failure it holds the one thing worth reporting — *why* ffprobe refused.
    // "Permission denied" and "Invalid data found" are opposite problems with
    // opposite fixes, and an exit code alone tells them apart for nobody.
    let invocation = probe_invocation(path);
    let path_display = path.display().to_string();
    #[cfg(test)]
    let synthetic_stdout = invocation.synthetic_stdout.clone();
    #[cfg(not(test))]
    let synthetic_stdout: Option<Vec<u8>> = None;
    let stdout = if let Some(stdout) = synthetic_stdout {
        stdout
    } else {
        let output = crate::process::bounded::output(
            &invocation.program,
            &invocation.args,
            invocation.wall_time,
            SCAN_PROBE_MAX_BYTES,
        )
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ProbeError::Spawn(error.to_string()),
            std::io::ErrorKind::TimedOut => ProbeError::Transient {
                path: path_display.clone(),
                reason: format!("ffprobe exceeded {} s", invocation.wall_time.as_secs_f64()),
            },
            _ => ProbeError::Transient {
                path: path_display.clone(),
                reason: error.to_string(),
            },
        })?;

        if !output.status.success() {
            return Err(ProbeError::Failed {
                path: path_display,
                code: output.status.code(),
                reason: probe_failure_reason(&output.stderr),
            });
        }
        output.stdout
    };
    let mut json: Value = serde_json::from_slice(&stdout)
        .map_err(|e| ProbeError::Parse(format!("ffprobe json: {e}")))?;
    // Stamped before the document is retained, so a later comparison against a
    // fresh probe can tell an unchanged source from a changed reporter.
    if let Some(reporter_bin) = invocation.reporter {
        if let Some(reporter) = reporter_identity_of(&reporter_bin).await {
            stamp_reporter(&mut json, reporter);
        }
    }
    let mut result = parse_probe_json(&json);
    // Container comes from the extension — the decision engine keys on it
    // ("mkv" → remux, "mp4" → direct) and it's more reliable than ffmpeg's
    // comma-joined format_name.
    result.container = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    Ok(result)
}

/// Pure parser over ffprobe JSON — unit-testable without spawning anything.
pub fn parse_probe_json(json: &Value) -> ProbeResult {
    let mut result = ProbeResult {
        raw_json: serde_json::to_string(json).ok(),
        ..Default::default()
    };

    if let Some(format) = json.get("format") {
        result.duration_ms = format
            .get("duration")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .map(|secs| (secs * 1000.0) as i64);
        result.bitrate = format
            .get("bit_rate")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok());
        result.creation_time = tag(format, "creation_time").and_then(|s| normalize_timestamp(&s));
    }

    let empty = Vec::new();
    let streams = json
        .get("streams")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let (mut audio_i, mut sub_i) = (0i64, 0i64);
    let mut video_seen = false;

    for stream in streams {
        match stream.get("codec_type").and_then(|v| v.as_str()) {
            Some("video") if !video_seen => {
                // Skip attached cover art / thumbnails.
                if is_attached_pic(stream) {
                    continue;
                }
                video_seen = true;
                result.video_codec = str_field(stream, "codec_name");
                result.video_codec_tag = video_codec_tag(stream);
                result.video_profile = str_field(stream, "profile");
                result.width = int_field(stream, "width");
                result.height = int_field(stream, "height");
                result.bit_depth = video_bit_depth(stream);
                result.hdr = detect_hdr(stream);
                result.hdr_format = detect_hdr_format(stream);
                result.dolby_vision = detect_dolby_vision(stream);
            }
            Some("audio") => {
                result.audio_streams.push(AudioStream {
                    index: audio_i,
                    codec: str_field(stream, "codec_name").unwrap_or_default(),
                    channels: int_field(stream, "channels"),
                    language: tag(stream, "language"),
                    title: tag(stream, "title"),
                    default: disposition(stream, "default"),
                });
                audio_i += 1;
            }
            Some("subtitle") => {
                result.subtitle_streams.push(SubtitleStream {
                    index: sub_i,
                    codec: str_field(stream, "codec_name").unwrap_or_default(),
                    language: tag(stream, "language"),
                    title: tag(stream, "title"),
                    default: disposition(stream, "default"),
                    forced: disposition(stream, "forced"),
                    hearing_impaired: disposition(stream, "hearing_impaired"),
                });
                sub_i += 1;
            }
            _ => {}
        }
    }
    result
}

/// Normalize a container timestamp ("2019-06-14T18:22:03.000000Z",
/// "2019-06-14 18:22:03") to local-naive ISO 8601: `YYYY-MM-DDTHH:MM:SS`.
/// Sorting is all this field is for, so the zone marker and sub-second part
/// are dropped rather than converted — and a value that doesn't start with a
/// plausible date is discarded, since cameras write some remarkable garbage
/// (`0000-00-00T00:00:00Z` being the classic).
fn normalize_timestamp(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let date = raw.get(..10)?;
    let bytes = date.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    if !shaped || date.starts_with("0000") {
        return None;
    }
    let time = raw
        .get(11..19)
        .filter(|t| t.len() == 8 && t.as_bytes()[2] == b':' && t.as_bytes()[5] == b':');
    Some(match time {
        Some(time) => format!("{date}T{time}"),
        None => date.to_owned(),
    })
}

fn str_field(stream: &Value, key: &str) -> Option<String> {
    stream
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// Normalize ffprobe's sample-entry spelling without inventing one.
///
/// A tag is useful only when it is the exact four-byte ASCII alphanumeric
/// label carried by the selected stream. ffprobe's zero sentinels mean
/// "unknown", and accepting punctuation, whitespace, or Unicode would make a
/// source-admission value that no ISO-BMFF sample entry can match.
fn video_codec_tag(stream: &Value) -> Option<String> {
    let raw = stream.get("codec_tag_string")?.as_str()?;
    let bytes = raw.as_bytes();
    if bytes.len() != 4 || !bytes.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    let normalized = raw.to_ascii_lowercase();
    (normalized != "0000").then_some(normalized)
}

fn int_field(stream: &Value, key: &str) -> Option<i64> {
    stream.get(key).and_then(|v| v.as_i64())
}

fn tag(stream: &Value, key: &str) -> Option<String> {
    stream
        .get("tags")
        .and_then(|t| t.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

fn disposition(stream: &Value, key: &str) -> bool {
    stream
        .get("disposition")
        .and_then(|d| d.get(key))
        .and_then(|v| v.as_i64())
        .map(|n| n != 0)
        .unwrap_or(false)
}

fn is_attached_pic(stream: &Value) -> bool {
    disposition(stream, "attached_pic")
}

/// Bit depth from `bits_per_raw_sample`, falling back to the pixel format
/// (e.g. `yuv420p10le` → 10).
fn video_bit_depth(stream: &Value) -> Option<i64> {
    if let Some(bits) = stream
        .get("bits_per_raw_sample")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<i64>().ok())
    {
        return Some(bits);
    }
    let pix = stream.get("pix_fmt").and_then(|v| v.as_str())?;
    if pix.contains("12le") || pix.contains("12be") || pix.contains("p012") {
        Some(12)
    } else if pix.contains("10le") || pix.contains("10be") || pix.contains("p010") {
        Some(10)
    } else {
        Some(8)
    }
}

/// Classify HDR from color transfer + Dolby Vision side data.
/// Returns "dolby_vision" | "hdr10" | "hlg" | None (SDR/unknown).
fn detect_hdr(stream: &Value) -> Option<String> {
    // Dolby Vision: DOVI config in side_data, or a DV codec tag/profile.
    let has_dovi_side_data = stream
        .get("side_data_list")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter().any(|sd| {
                sd.get("side_data_type")
                    .and_then(|v| v.as_str())
                    .map(|t| t.contains("DOVI") || t.contains("Dolby Vision"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    let dv_tag = stream
        .get("codec_tag_string")
        .and_then(|v| v.as_str())
        .map(|t| matches!(t, "dvh1" | "dvhe" | "dav1" | "dvav"))
        .unwrap_or(false);
    if has_dovi_side_data || dv_tag {
        return Some("dolby_vision".to_owned());
    }

    match stream.get("color_transfer").and_then(|v| v.as_str()) {
        Some("smpte2084") => Some("hdr10".to_owned()),
        Some("arib-std-b67") => Some("hlg".to_owned()),
        _ => None,
    }
}

/// The Dolby Vision configuration record on this stream, if ffprobe emitted
/// one.
///
/// Every field comes straight off the record and stays `None` when the record
/// did not carry it — a missing `dv_profile` and a `dv_profile` of 0 are
/// different facts, and the whole point of moving these out of the display
/// label is that the difference survives.
pub(crate) fn detect_dolby_vision(stream: &Value) -> DolbyVisionFacts {
    let Some(list) = stream.get("side_data_list").and_then(|v| v.as_array()) else {
        return DolbyVisionFacts::default();
    };
    for sd in list {
        let t = sd
            .get("side_data_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !(t.contains("DOVI") || t.contains("Dolby Vision")) {
            continue;
        }
        let flag = |key: &str| sd.get(key).and_then(|v| v.as_i64()).map(|value| value != 0);
        return DolbyVisionFacts {
            profile: sd.get("dv_profile").and_then(|v| v.as_i64()),
            level: sd.get("dv_level").and_then(|v| v.as_i64()),
            bl_compat_id: sd
                .get("dv_bl_signal_compatibility_id")
                .and_then(|v| v.as_i64()),
            el_present: flag("el_present_flag"),
            rpu_present: flag("rpu_present_flag"),
        };
    }
    DolbyVisionFacts::default()
}

/// The display label for a Dolby Vision source, derived from its facts.
///
/// One function, so the string a viewer reads and the numbers the decider uses
/// can never describe different files. Byte for byte what `detect_hdr_format`
/// built inline before M2 — the wording is load-bearing: `has_compatible_dv_base`
/// still falls back to searching it for rows the backfill has not reached.
pub(crate) fn dolby_vision_label(facts: &DolbyVisionFacts) -> String {
    let mut label = match facts.profile {
        Some(profile) => format!("Dolby Vision · Profile {profile}"),
        None => "Dolby Vision".to_owned(),
    };
    match facts.bl_compat_id {
        Some(1) | Some(6) => label.push_str(" (HDR10-compatible)"),
        Some(4) => label.push_str(" (HLG-compatible)"),
        _ => {}
    }
    label
}

/// A richer, human HDR label for display — the Dolby Vision profile number and
/// compatibility, HDR10+ vs HDR10, HLG. Parallels [`detect_hdr`] (which stays
/// coarse for the decision engine); returns None for SDR.
///
/// Since M2 the Dolby Vision half is derived from
/// [`detect_dolby_vision`]'s facts rather than assembled inline, so the string
/// a viewer reads and the numbers the decider uses cannot describe different
/// files.
fn detect_hdr_format(stream: &Value) -> Option<String> {
    let side = stream.get("side_data_list").and_then(|v| v.as_array());

    // Dolby Vision: the profile and base-layer compatibility come from the
    // DOVI configuration record. Compatibility id tells you what a non-DV
    // client sees: 1 = HDR10, 6 = Blu-ray HDR10, 4 = HLG, 2 = SDR.
    let facts = detect_dolby_vision(stream);
    if !facts.is_empty() {
        return Some(dolby_vision_label(&facts));
    }
    // A record that carried the type but no readable field at all still means
    // Dolby Vision, and still has to be named.
    if let Some(list) = side {
        if list.iter().any(|sd| {
            sd.get("side_data_type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t.contains("DOVI") || t.contains("Dolby Vision"))
        }) {
            return Some("Dolby Vision".to_owned());
        }
    }
    // A DV codec tag with no config record: name it without a profile.
    if stream
        .get("codec_tag_string")
        .and_then(|v| v.as_str())
        .map(|t| matches!(t, "dvh1" | "dvhe" | "dav1" | "dvav"))
        .unwrap_or(false)
    {
        return Some("Dolby Vision".to_owned());
    }

    // HDR10+ carries dynamic metadata (SMPTE 2094-40) as side data.
    if let Some(list) = side {
        if list.iter().any(|sd| {
            sd.get("side_data_type")
                .and_then(|v| v.as_str())
                .map(|t| t.contains("Dynamic Metadata") || t.contains("SMPTE2094"))
                .unwrap_or(false)
        }) {
            return Some("HDR10+".to_owned());
        }
    }

    match stream.get("color_transfer").and_then(|v| v.as_str()) {
        Some("smpte2084") => Some("HDR10".to_owned()),
        Some("arib-std-b67") => Some("HLG".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    #[derive(Clone, Copy)]
    pub(crate) enum FixtureMode {
        SleepDeadline,
        SleepDrop,
        Flood,
        Fail,
        SleepThenJson,
    }

    struct Fixture {
        mode: FixtureMode,
        calls: Arc<AtomicUsize>,
        wall_time: Duration,
    }

    static FIXTURES: OnceLock<Mutex<HashMap<std::path::PathBuf, Fixture>>> = OnceLock::new();

    pub(crate) struct FixtureGuard {
        path: std::path::PathBuf,
        pub(crate) calls: Arc<AtomicUsize>,
    }

    impl Drop for FixtureGuard {
        fn drop(&mut self) {
            FIXTURES
                .get_or_init(Default::default)
                .lock()
                .expect("fixture registry")
                .remove(&self.path);
        }
    }

    pub(crate) fn install_fixture(
        path: &Path,
        mode: FixtureMode,
        wall_time: Duration,
    ) -> FixtureGuard {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let calls = Arc::new(AtomicUsize::new(0));
        let replaced = FIXTURES
            .get_or_init(Default::default)
            .lock()
            .expect("fixture registry")
            .insert(
                path.clone(),
                Fixture {
                    mode,
                    calls: Arc::clone(&calls),
                    wall_time,
                },
            );
        assert!(replaced.is_none(), "one fixture owns each media path");
        FixtureGuard { path, calls }
    }

    pub(super) fn fixture_invocation(path: &Path) -> Option<ProbeInvocation> {
        let registry = FIXTURES.get_or_init(Default::default);
        let registry = registry.lock().expect("fixture registry");
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let fixture = registry.get(&canonical)?;
        let call = fixture.calls.fetch_add(1, Ordering::Relaxed);
        let synthetic_stdout = matches!(fixture.mode, FixtureMode::SleepThenJson) && call > 0;
        let child = match (fixture.mode, call) {
            (FixtureMode::SleepDeadline, _) => "scan::probe::tests::fixture_child_sleep_deadline",
            (FixtureMode::SleepDrop, _) => "scan::probe::tests::fixture_child_sleep_drop",
            (FixtureMode::SleepThenJson, 0) => "scan::probe::tests::fixture_child_sleep_sequence",
            (FixtureMode::Flood, _) => "scan::probe::tests::fixture_child_flood",
            (FixtureMode::Fail, _) => "scan::probe::tests::fixture_child_fail",
            (FixtureMode::SleepThenJson, _) => "scan::probe::tests::fixture_child_json",
        };
        Some(ProbeInvocation {
            program: std::env::current_exe()
                .expect("test executable")
                .into_os_string(),
            args: vec!["--exact".into(), child.into(), "--nocapture".into()],
            wall_time: fixture.wall_time,
            reporter: None,
            synthetic_stdout: synthetic_stdout
                .then(|| br#"{"format":{"duration":"1.0"},"streams":[]}"#.to_vec()),
        })
    }

    fn invoked_as_child(name: &str) -> bool {
        let args = std::env::args().collect::<Vec<_>>();
        args.windows(2)
            .any(|pair| pair[0] == "--exact" && pair[1] == name)
    }

    fn child_pid_file(mode: &str) -> std::path::PathBuf {
        let executable = std::env::current_exe().expect("test executable");
        let name = executable
            .file_name()
            .expect("test executable name")
            .to_string_lossy();
        std::env::temp_dir().join(format!("plurx-scan-probe-{name}-{mode}.pid"))
    }

    #[test]
    fn fixture_child_sleep_deadline() {
        if !invoked_as_child("scan::probe::tests::fixture_child_sleep_deadline") {
            return;
        }
        std::fs::write(
            child_pid_file("sleep-deadline"),
            std::process::id().to_string(),
        )
        .expect("publish child pid");
        std::thread::sleep(Duration::from_secs(300));
    }

    #[test]
    fn fixture_child_sleep_drop() {
        if !invoked_as_child("scan::probe::tests::fixture_child_sleep_drop") {
            return;
        }
        std::fs::write(child_pid_file("sleep-drop"), std::process::id().to_string())
            .expect("publish child pid");
        std::thread::sleep(Duration::from_secs(300));
    }

    #[test]
    fn fixture_child_sleep_sequence() {
        if !invoked_as_child("scan::probe::tests::fixture_child_sleep_sequence") {
            return;
        }
        std::thread::sleep(Duration::from_secs(300));
    }

    #[test]
    fn fixture_child_flood() {
        if !invoked_as_child("scan::probe::tests::fixture_child_flood") {
            return;
        }
        std::fs::write(child_pid_file("flood"), std::process::id().to_string())
            .expect("publish child pid");
        let chunk = vec![b'{'; 64 * 1024];
        let mut stdout = std::io::stdout().lock();
        for _ in 0..1024 {
            stdout.write_all(&chunk).expect("flood stdout");
        }
    }

    #[test]
    fn fixture_child_fail() {
        if !invoked_as_child("scan::probe::tests::fixture_child_fail") {
            return;
        }
        std::io::stderr()
            .write_all(b"/media/fixture.mkv: Permission denied\n")
            .expect("failure stderr");
        std::process::exit(13);
    }

    #[test]
    fn fixture_child_json() {
        if !invoked_as_child("scan::probe::tests::fixture_child_json") {
            return;
        }
        std::io::stdout()
            .write_all(br#"{"format":{"duration":"1.0"},"streams":[]}"#)
            .expect("probe json");
    }

    async fn wait_for_pid(mode: &str) -> u32 {
        let path = child_pid_file(mode);
        for _ in 0..200 {
            if let Ok(pid) = std::fs::read_to_string(&path) {
                return pid.parse().expect("numeric child pid");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture child did not publish {}", path.display());
    }

    async fn assert_pid_gone(pid: u32) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            !crate::process::signal(pid, crate::process::ProcessSignal::Terminate)
                .expect("inspect fixture child"),
            "fixture child {pid} survived for two seconds"
        );
    }

    fn media_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("media fixture");
        let path = directory.path().join("fixture.mkv");
        std::fs::write(&path, b"fixture").expect("media fixture bytes");
        (directory, path)
    }

    #[tokio::test]
    async fn sleeping_probe_is_transient_and_reaped() {
        let _ = std::fs::remove_file(child_pid_file("sleep-deadline"));
        let (_directory, path) = media_fixture();
        let _fixture = install_fixture(
            &path,
            FixtureMode::SleepDeadline,
            Duration::from_millis(200),
        );
        let started = tokio::time::Instant::now();
        let error = probe(&path).await.expect_err("sleeping probe times out");
        assert!(matches!(error, ProbeError::Transient { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = wait_for_pid("sleep-deadline").await;
        assert_pid_gone(pid).await;
    }

    #[tokio::test]
    async fn flooding_probe_is_capped_drained_and_reaped() {
        let _ = std::fs::remove_file(child_pid_file("flood"));
        let (_directory, path) = media_fixture();
        let _fixture = install_fixture(&path, FixtureMode::Flood, Duration::from_secs(5));
        let error = probe(&path).await.expect_err("flood is not JSON");
        assert!(matches!(error, ProbeError::Parse(_)));
        let pid = wait_for_pid("flood").await;
        assert_pid_gone(pid).await;
    }

    #[tokio::test]
    async fn dropping_probe_future_reaps_its_child() {
        let _ = std::fs::remove_file(child_pid_file("sleep-drop"));
        let (_directory, path) = media_fixture();
        let _fixture = install_fixture(&path, FixtureMode::SleepDrop, Duration::from_secs(300));
        let task = tokio::spawn({
            let path = path.clone();
            async move { probe(&path).await }
        });
        let pid = wait_for_pid("sleep-drop").await;
        task.abort();
        let _ = task.await;
        assert_pid_gone(pid).await;
    }

    #[tokio::test]
    async fn failed_probe_keeps_its_stderr_reason() {
        let (_directory, path) = media_fixture();
        let _fixture = install_fixture(&path, FixtureMode::Fail, Duration::from_secs(5));
        let error = probe(&path)
            .await
            .expect_err("fixture exits unsuccessfully");
        match error {
            ProbeError::Failed { reason, .. } => assert_eq!(reason, "Permission denied"),
            other => panic!("expected permanent probe failure, got {other}"),
        }
    }

    #[test]
    fn parses_hdr10_movie() {
        let j = json!({
            "format": { "duration": "7200.5", "bit_rate": "25000000" },
            "streams": [
                { "codec_type": "video", "codec_name": "hevc", "profile": "Main 10",
                  "width": 3840, "height": 2160, "pix_fmt": "yuv420p10le",
                  "color_transfer": "smpte2084" },
                { "codec_type": "audio", "codec_name": "truehd", "channels": 8,
                  "disposition": { "default": 1 }, "tags": { "language": "eng" } },
                { "codec_type": "audio", "codec_name": "ac3", "channels": 6,
                  "tags": { "language": "fre" } },
                { "codec_type": "subtitle", "codec_name": "subrip",
                  "disposition": { "default": 0, "forced": 0 },
                  "tags": { "language": "eng" } }
            ]
        });
        let p = parse_probe_json(&j);
        assert_eq!(p.duration_ms, Some(7_200_500));
        assert_eq!(p.bitrate, Some(25_000_000));
        assert_eq!(p.video_codec.as_deref(), Some("hevc"));
        assert_eq!(p.width, Some(3840));
        assert_eq!(p.bit_depth, Some(10));
        assert_eq!(p.hdr.as_deref(), Some("hdr10"));
        assert_eq!(p.audio_streams.len(), 2);
        assert_eq!(p.audio_streams[0].codec, "truehd");
        assert_eq!(p.audio_streams[0].channels, Some(8));
        assert!(p.audio_streams[0].default);
        assert_eq!(p.audio_streams[1].index, 1);
        assert_eq!(p.subtitle_streams.len(), 1);
        assert_eq!(p.subtitle_streams[0].language.as_deref(), Some("eng"));
    }

    /// SDH is a *disposition*, not a naming convention. Reading the flag the
    /// muxer authored is what lets an accessibility rendition be tagged as
    /// one when its title says nothing — and what stops the tag depending on
    /// whoever named the track.
    #[test]
    fn subtitle_dispositions_include_hearing_impaired() {
        let j = json!({
            "streams": [
                { "codec_type": "subtitle", "codec_name": "subrip",
                  "disposition": { "default": 1, "forced": 0, "hearing_impaired": 1 },
                  "tags": { "language": "eng", "title": "English" } },
                { "codec_type": "subtitle", "codec_name": "subrip",
                  "disposition": { "default": 0, "forced": 1 },
                  "tags": { "language": "eng", "title": "Forced" } }
            ]
        });
        let p = parse_probe_json(&j);
        let sdh = &p.subtitle_streams[0];
        assert!(sdh.hearing_impaired, "a bland title, an authored flag");
        assert!(sdh.default);
        assert!(!sdh.forced);
        // The other track has no such key at all, which is not the same as
        // having it set — an absent disposition is a plain "no".
        assert!(!p.subtitle_streams[1].hearing_impaired);
        assert!(p.subtitle_streams[1].forced);
    }

    /// A library probed before plurx read this flag keeps its stored JSON
    /// until something re-probes it, so the field has to survive being
    /// absent — deserializing to `false`, which routes those tracks back to
    /// the title sniff they always used.
    #[test]
    fn probe_json_written_before_the_flag_existed_still_deserializes() {
        let stored = r#"[{"index":0,"codec":"subrip","language":"eng",
                          "title":"English SDH","default":false,"forced":false}]"#;
        let tracks: Vec<crate::domain::SubtitleStream> =
            serde_json::from_str(stored).expect("old probe rows still load");
        assert_eq!(tracks.len(), 1);
        assert!(!tracks[0].hearing_impaired);
        assert_eq!(tracks[0].title.as_deref(), Some("English SDH"));
    }

    #[test]
    fn hdr_format_reports_dv_profile_and_compat() {
        let j = json!({
            "streams": [{
                "codec_type": "video", "codec_name": "hevc",
                "side_data_list": [{
                    "side_data_type": "DOVI configuration record",
                    "dv_profile": 7, "dv_bl_signal_compatibility_id": 6
                }]
            }]
        });
        let p = parse_probe_json(&j);
        assert_eq!(p.hdr.as_deref(), Some("dolby_vision")); // coarse type unchanged
        assert_eq!(
            p.hdr_format.as_deref(),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)")
        );
    }

    /// The record's own fields, as columns — and the label built from them.
    ///
    /// The two used to be one inline block, so a fact the label had no way to
    /// spell (an enhancement layer, an RPU) simply did not survive the scan.
    /// Those two decide whether a Profile 7 disc can be converted to
    /// single-layer Profile 8.1, which no display string can carry.
    #[test]
    fn the_dolby_vision_record_survives_as_facts_not_only_as_a_label() {
        let j = json!({
            "streams": [{
                "codec_type": "video", "codec_name": "hevc",
                "side_data_list": [{
                    "side_data_type": "DOVI configuration record",
                    "dv_version_major": 1,
                    "dv_profile": 7, "dv_level": 6,
                    "dv_bl_signal_compatibility_id": 6,
                    "rpu_present_flag": 1, "el_present_flag": 1, "bl_present_flag": 1
                }]
            }]
        });
        let p = parse_probe_json(&j);
        assert_eq!(p.dolby_vision.profile, Some(7));
        assert_eq!(p.dolby_vision.level, Some(6));
        assert_eq!(p.dolby_vision.bl_compat_id, Some(6));
        assert_eq!(p.dolby_vision.el_present, Some(true));
        assert_eq!(p.dolby_vision.rpu_present, Some(true));
        // The label is derived from exactly those facts, byte for byte what
        // it was before the columns existed — `has_compatible_dv_base` still
        // falls back to reading it for rows the backfill has not reached.
        assert_eq!(
            p.hdr_format.as_deref(),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)")
        );

        // A single-layer Profile 5: no enhancement layer, no compatible base.
        let j = json!({
            "streams": [{
                "codec_type": "video", "codec_name": "hevc",
                "side_data_list": [{
                    "side_data_type": "DOVI configuration record",
                    "dv_profile": 5, "dv_bl_signal_compatibility_id": 0,
                    "rpu_present_flag": 1, "el_present_flag": 0
                }]
            }]
        });
        let p = parse_probe_json(&j);
        assert_eq!(p.dolby_vision.el_present, Some(false));
        assert_eq!(p.dolby_vision.bl_compat_id, Some(0));
        assert_eq!(p.hdr_format.as_deref(), Some("Dolby Vision · Profile 5"));

        // A record with nothing readable in it is still Dolby Vision, and
        // still gets named — but its facts stay empty, which is what tells
        // the backfill this row needs a real re-probe rather than a re-read.
        let j = json!({
            "streams": [{
                "codec_type": "video", "codec_name": "hevc",
                "side_data_list": [{ "side_data_type": "DOVI configuration record" }]
            }]
        });
        let p = parse_probe_json(&j);
        assert!(p.dolby_vision.is_empty());
        assert_eq!(p.hdr_format.as_deref(), Some("Dolby Vision"));

        // And a non-DV source carries no facts at all — `None` everywhere,
        // never a zero that would read as "the record said so".
        let j = json!({
            "streams": [{ "codec_type": "video", "codec_name": "hevc",
                          "color_transfer": "smpte2084" }]
        });
        assert!(parse_probe_json(&j).dolby_vision.is_empty());
    }

    /// The derived label is the label, byte for byte.
    ///
    /// It has to be. `hdr_format` is an input to `copy_video_args`, which
    /// feeds the fragment index's argv fingerprint, so a re-worded label
    /// re-keys every affected file's index and orphans what was built. The
    /// backfill compares before it writes for the same reason; this is what
    /// makes that comparison usually come out equal.
    ///
    /// It carries a second weight since the Dolby Vision columns became the
    /// authoritative half of that question: `copy_video_args` reads
    /// `bl_compat_id` first and this label only as a fallback, so the two must
    /// agree on which base layers are watchable. This test is what keeps the
    /// derivation honest, and the compatibility-id set in
    /// `transcode::dolby_vision_has_compatible_base` is what it has to stay
    /// honest against.
    #[test]
    fn the_derived_label_is_the_label_the_scan_used_to_write() {
        for (profile, compat, expected) in [
            (
                Some(7),
                Some(6),
                "Dolby Vision · Profile 7 (HDR10-compatible)",
            ),
            (
                Some(8),
                Some(1),
                "Dolby Vision · Profile 8 (HDR10-compatible)",
            ),
            (
                Some(7),
                Some(4),
                "Dolby Vision · Profile 7 (HLG-compatible)",
            ),
            (Some(5), Some(0), "Dolby Vision · Profile 5"),
            (Some(5), None, "Dolby Vision · Profile 5"),
            (Some(4), Some(2), "Dolby Vision · Profile 4"),
            (None, Some(6), "Dolby Vision (HDR10-compatible)"),
            (None, None, "Dolby Vision"),
        ] {
            let facts = DolbyVisionFacts {
                profile,
                bl_compat_id: compat,
                ..DolbyVisionFacts::default()
            };
            assert_eq!(
                dolby_vision_label(&facts),
                expected,
                "{profile:?}/{compat:?}"
            );
        }
    }

    #[test]
    fn hdr_format_plain_hdr10() {
        let j = json!({
            "streams": [{ "codec_type": "video", "codec_name": "hevc",
                          "color_transfer": "smpte2084" }]
        });
        assert_eq!(parse_probe_json(&j).hdr_format.as_deref(), Some("HDR10"));
    }

    #[test]
    fn detects_dolby_vision_via_side_data() {
        let j = json!({
            "streams": [{
                "codec_type": "video", "codec_name": "hevc",
                "side_data_list": [{ "side_data_type": "DOVI configuration record" }]
            }]
        });
        assert_eq!(parse_probe_json(&j).hdr.as_deref(), Some("dolby_vision"));
    }

    #[test]
    fn plain_sdr_has_no_hdr() {
        let j = json!({
            "format": { "duration": "1200.0" },
            "streams": [
                { "codec_type": "video", "codec_name": "h264", "width": 1920,
                  "height": 1080, "pix_fmt": "yuv420p" }
            ]
        });
        let p = parse_probe_json(&j);
        assert_eq!(p.hdr, None);
        assert_eq!(p.bit_depth, Some(8));
        assert!(p.audio_streams.is_empty());
    }

    #[test]
    fn skips_attached_cover_art() {
        let j = json!({
            "streams": [
                { "codec_type": "video", "codec_name": "mjpeg",
                  "disposition": { "attached_pic": 1 } },
                { "codec_type": "video", "codec_name": "h264", "width": 1280, "height": 720 }
            ]
        });
        let p = parse_probe_json(&j);
        assert_eq!(p.video_codec.as_deref(), Some("h264"));
        assert_eq!(p.width, Some(1280));
    }

    #[test]
    fn video_codec_tag_uses_first_non_attached_video() {
        let j = json!({
            "streams": [
                { "codec_type": "video", "codec_name": "mjpeg",
                  "codec_tag_string": "jpeg",
                  "disposition": { "attached_pic": 1 } },
                { "codec_type": "audio", "codec_name": "aac",
                  "codec_tag_string": "mp4a" },
                { "codec_type": "video", "codec_name": "hevc",
                  "codec_tag_string": "HeV1" },
                { "codec_type": "video", "codec_name": "hevc",
                  "codec_tag_string": "hvc1" }
            ]
        });
        assert_eq!(
            parse_probe_json(&j).video_codec_tag.as_deref(),
            Some("hev1")
        );
    }

    #[test]
    fn video_codec_tag_rejects_unknown_and_malformed_values() {
        for value in [
            Value::Null,
            json!(7),
            json!(""),
            json!("0000"),
            json!("hev"),
            json!("hevc1"),
            json!("hv-1"),
            json!("hév1"),
        ] {
            let j = json!({
                "streams": [{
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "codec_tag_string": value
                }]
            });
            assert_eq!(parse_probe_json(&j).video_codec_tag, None, "{value}");
        }

        let missing = json!({
            "streams": [{ "codec_type": "video", "codec_name": "hevc" }]
        });
        assert_eq!(parse_probe_json(&missing).video_codec_tag, None);

        let valid_other = json!({
            "streams": [{
                "codec_type": "video",
                "codec_name": "h264",
                "codec_tag_string": "avc1"
            }]
        });
        assert_eq!(
            parse_probe_json(&valid_other).video_codec_tag.as_deref(),
            Some("avc1")
        );
    }
}
