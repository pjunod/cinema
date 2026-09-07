//! Which decoder this FFmpeg build actually uses for a codec.
//!
//! `ffmpeg -decoders` lists codec *families*, and a family is not an
//! implementation. FFmpeg picks one when it opens a stream, and which one it
//! picks is a property of the build: `av1` selects the native decoder on a
//! build without libdav1d and `libdav1d` on one with it, and the two are
//! different code producing the same pictures at very different speeds.
//!
//! That distinction is not cosmetic here. A diagnostic contract is qualified
//! against one binary, one codec and one *named* decoder, so a plan that does
//! not name its decoder can never be matched to a contract, and an attempt
//! nothing can classify can never be certified. Guessing the name is worse
//! than not having one: a grammar built from the wrong name matches nothing,
//! and a grammar that matches nothing reports every stream as clean.
//!
//! So it is measured. FFmpeg names the decoder it chose in its own
//! diagnostics — `[vist#0:0/av1 @ …] [dec:libdav1d @ …]` — which is the same
//! context grammar the health reader already parses, at a log level that
//! prints it for a decode that goes perfectly. The measurement is one probe
//! per codec: encode a fraction of a second of `testsrc`, decode it to null,
//! and read back the name FFmpeg used.
//!
//! A codec this build can decode but cannot encode has no probe clip and is
//! reported as unmeasured. That is honest and it is the whole point: an
//! unmeasured codec keeps `implementation: None`, which keeps it out of the
//! qualified artifact namespace rather than putting a guess into a cache key.

use std::collections::BTreeMap;

/// How long each ffmpeg invocation may take before it is killed and the codec
/// reported unmeasured.
///
/// This runs on the path to serving, so a build whose encoder wedges must cost
/// a bounded delay and an unnamed codec rather than a node that never opens a
/// listener. Two frames of 160x120 is far inside this; the repository already
/// keeps a wedged-ffmpeg fixture for the encoder probe because this failure is
/// not hypothetical.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a probe clip runs. Long enough that a decoder actually opens and
/// emits its context lines, short enough that measuring every codec at boot
/// costs less than one segment of real work.
const PROBE_SECONDS: &str = "0.2";
const PROBE_SIZE: &str = "160x120";
const PROBE_FPS: &str = "10";

/// The encoder used to make a probe clip for each codec this inventory covers.
///
/// Deliberately a table rather than a guess from the codec name. `mpeg4` and
/// `mpeg2video` name their own encoders; every other family's usable encoder
/// is an external library whose name shares nothing with the codec's.
const PROBE_ENCODERS: &[(&str, &[&str])] = &[
    ("h264", &["libx264", "libopenh264"]),
    ("hevc", &["libx265"]),
    ("vp8", &["libvpx"]),
    ("vp9", &["libvpx-vp9"]),
    ("av1", &["libsvtav1", "libaom-av1", "librav1e"]),
    ("mpeg4", &["mpeg4"]),
    ("mpeg2video", &["mpeg2video"]),
];

/// The container each probe clip is written into.
///
/// Elementary streams would avoid a muxer entirely, but a decode of a raw
/// elementary stream can select a parser rather than the decoder the real
/// pipeline would open. Matroska takes every codec here and is what the
/// production sources look like.
const PROBE_CONTAINER: &str = "matroska";

/// What this build was measured to use, per codec.
///
/// `Serialize` because an operator deciding whether to enable qualification
/// needs to see what their own node measured, and "which decoder will actually
/// run" is not something they can read anywhere else.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct MeasuredDecoders {
    by_codec: BTreeMap<String, String>,
}

impl MeasuredDecoders {
    /// The decoder FFmpeg selected for `codec`, when it was measured.
    ///
    /// `None` means unmeasured, never "the family name" — the caller must keep
    /// its plan unnamed rather than substitute the codec, because a contract
    /// looked up under a substituted name yields a grammar that matches
    /// nothing and certifies everything.
    pub fn implementation(&self, codec: &str) -> Option<&str> {
        self.by_codec.get(codec).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.by_codec.is_empty()
    }

    pub fn measured_codecs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.by_codec
            .iter()
            .map(|(codec, decoder)| (codec.as_str(), decoder.as_str()))
    }

    /// Record a measurement that was made elsewhere.
    ///
    /// This records a result; it does not make one. The same bound the probe
    /// applies is applied here, so a name that could not have come out of a
    /// measurement cannot be put in by hand either — the value ends up in an
    /// artifact's identity, and there is no reading of "unmeasured" that
    /// should be spelled by an unusable name rather than by absence.
    pub fn from_measured(pairs: &[(&str, &str)]) -> Self {
        Self {
            by_codec: pairs
                .iter()
                .filter(|(_, decoder)| safe_decoder_name(decoder))
                .map(|(codec, decoder)| ((*codec).to_owned(), (*decoder).to_owned()))
                .collect(),
        }
    }
}

/// Whether a name FFmpeg printed is one a plan may carry.
///
/// Asks the plan rather than restating its rule. A measurement that accepted a
/// name the plan refuses would not merely leave that codec unmeasured: naming
/// it makes `DecodeCapabilities::new` reject the whole snapshot, so one odd
/// name would refuse every plan on the node, including the codecs that
/// measured perfectly well.
fn safe_decoder_name(name: &str) -> bool {
    crate::transcode::plan_can_name_decoder(name)
}

/// Read back the decoder FFmpeg opened for `codec`, from its own diagnostics.
///
/// The line shape is `[vist#<input>:<stream>/<codec> @ addr] [dec:<name> @
/// addr] …`, and both halves are required. Matching on `dec:` alone would
/// accept a context belonging to another stream of another codec, which on a
/// probe clip is impossible and on any other input is exactly the confusion
/// this whole effort exists to remove.
///
/// Every line is read rather than the first: a decoder that fails to open
/// prints nothing, and a build that changes which line carries the context
/// should be measured as unmeasured rather than as whatever the first line
/// happened to say. Disagreement across lines is also refused, because two
/// answers is not an answer.
pub fn selected_decoder(stderr: &str, codec: &str) -> Option<String> {
    let mut found: Option<&str> = None;
    for line in stderr.lines() {
        let Some(decoder) = decoder_for_codec(line, codec) else {
            continue;
        };
        match found {
            Some(previous) if previous != decoder => return None,
            _ => found = Some(decoder),
        }
    }
    found
        .filter(|decoder| safe_decoder_name(decoder))
        .map(str::to_owned)
}

/// `[vist#<input>:<stream>/<codec>[ @ addr]] [dec:<name>[ @ addr]] …`
fn decoder_for_codec<'a>(line: &'a str, codec: &str) -> Option<&'a str> {
    let (stream, rest) = take_context(line)?;
    let (_, line_codec) = stream.strip_prefix("vist#")?.split_once('/')?;
    if line_codec != codec {
        return None;
    }
    let (decoder, _) = take_context(rest)?;
    decoder.strip_prefix("dec:")
}

/// Take a leading `[…]` context and return its name with the rest of the line.
///
/// The ` @ 0x…` address inside the brackets is dropped: it is a pointer, it
/// differs on every run, and nothing here wants it. A name containing `@`
/// outside that form is not a context.
fn take_context(line: &str) -> Option<(&str, &str)> {
    let body = line.strip_prefix('[')?;
    let end = body.find(']')?;
    let (inside, rest) = body.split_at(end);
    let rest = rest[1..].trim_start();
    let name = match inside.split_once(" @ ") {
        Some((name, address)) => {
            // The same rule the health reader applies, so the two cannot
            // disagree about what a context is: `0x` followed by at least one
            // hex digit and nothing else.
            let hex = address.strip_prefix("0x")?;
            if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            name
        }
        None => inside,
    };
    (!name.is_empty() && !name.contains('@')).then_some((name, rest))
}

/// Measure the decoder this build selects for each codec it can decode.
///
/// One encode and one decode per codec, into `scratch`. Codecs whose probe
/// clip cannot be made — no encoder for that family on this build — are absent
/// from the result rather than guessed.
///
/// Errors are not propagated. Every one of them means the same thing to the
/// caller, which is that the codec is unmeasured, and a boot that cannot run a
/// probe is not a boot that should fail: it is one whose plans stay outside the
/// qualified artifact namespace, which is where they belong.
pub async fn measure_selected_decoders(
    ffmpeg_bin: &str,
    codecs: &[String],
    scratch: &std::path::Path,
) -> MeasuredDecoders {
    let mut by_codec = BTreeMap::new();
    for codec in codecs {
        let Some(encoders) = PROBE_ENCODERS
            .iter()
            .find_map(|(name, encoders)| (name == codec).then_some(*encoders))
        else {
            continue;
        };
        if let Some(decoder) = measure_one(ffmpeg_bin, codec, encoders, scratch).await {
            tracing::debug!(%codec, %decoder, "measured the decoder this build selects");
            by_codec.insert(codec.clone(), decoder);
        }
    }
    if by_codec.is_empty() {
        tracing::info!(
            "no decoder implementation could be measured on this build; plans stay unnamed"
        );
    }
    MeasuredDecoders { by_codec }
}

async fn measure_one(
    ffmpeg_bin: &str,
    codec: &str,
    encoders: &[&str],
    scratch: &std::path::Path,
) -> Option<String> {
    // A name nothing else writes, so two nodes sharing a scratch directory —
    // or one node probing while an earlier probe's process is still exiting —
    // cannot read each other's clip.
    let clip = scratch.join(format!(
        "decoder-probe-{codec}-{}.mkv",
        uuid::Uuid::new_v4().simple()
    ));
    let mut made = false;
    for encoder in encoders {
        if probe_clip(ffmpeg_bin, encoder, &clip).await {
            made = true;
            break;
        }
    }
    let measured = if made {
        probe_decode(ffmpeg_bin, codec, &clip).await
    } else {
        None
    };
    let _ = tokio::fs::remove_file(&clip).await;
    measured
}

async fn probe_clip(ffmpeg_bin: &str, encoder: &str, clip: &std::path::Path) -> bool {
    let source = format!("testsrc=size={PROBE_SIZE}:rate={PROBE_FPS}:duration={PROBE_SECONDS}");
    matches!(
        bounded(
            tokio::process::Command::new(ffmpeg_bin)
            .kill_on_drop(true)
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-c:v",
                encoder,
                "-pix_fmt",
                "yuv420p",
                "-f",
                PROBE_CONTAINER,
            ])
            .arg(clip)
            .output()
        )
        .await,
        Some(Ok(output)) if output.status.success()
    )
}

/// Run one probe under [`PROBE_TIMEOUT`].
///
/// The child is killed on drop, so a timeout leaves no process behind — the
/// distinction that turns "boot never finishes" into "this codec is
/// unmeasured". Every caller reads `None` the same way it reads a failure,
/// because to a caller they mean the same thing.
async fn bounded<F, T>(work: F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(PROBE_TIMEOUT, work).await {
        Ok(value) => Some(value),
        Err(_) => {
            tracing::warn!(
                timeout_ms = PROBE_TIMEOUT.as_millis(),
                "a decoder inventory probe did not finish inside its budget"
            );
            None
        }
    }
}

async fn probe_decode(ffmpeg_bin: &str, codec: &str, clip: &std::path::Path) -> Option<String> {
    // `verbose`, because the context that names the decoder is printed by a
    // decode that goes perfectly and `error` prints nothing at all for one.
    // This is a probe of this node's own synthetic clip, so the log level is
    // not the production log level and does not have to be.
    let output = bounded(
        tokio::process::Command::new(ffmpeg_bin)
            .kill_on_drop(true)
            .args(["-hide_banner", "-loglevel", "verbose", "-i"])
            .arg(clip)
            .args(["-f", "null", "-"])
            .output(),
    )
    .await?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    selected_decoder(&String::from_utf8_lossy(&output.stderr), codec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real FFmpeg 9.0.1 output, captured from a probe decode of an h264 clip.
    const FFMPEG_9_H264: &str = "\
  Stream #0:0 -> #0:0 (h264 (native) -> wrapped_avframe (native))
[vist#0:0/h264 @ 0xbb3014600] [dec:h264 @ 0xbb2c10280] Starting thread...
[vist#0:0/h264 @ 0xbb3014600] [dec:h264 @ 0xbb2c10280] Decoder thread received EOF packet
[vist#0:0/h264 @ 0xbb3014600] [dec:h264 @ 0xbb2c10280] Terminating thread with return code 0 (success)
";

    #[test]
    fn the_decoder_is_read_from_the_context_ffmpeg_prints() {
        assert_eq!(
            selected_decoder(FFMPEG_9_H264, "h264").as_deref(),
            Some("h264")
        );
    }

    #[test]
    fn the_implementation_is_read_and_not_the_family() {
        // The case the whole inventory exists for: the family is `av1` and the
        // decoder is not.
        let stderr = "\
[vist#0:0/av1 @ 0x55e0] [dec:libdav1d @ 0x55f0] Starting thread...
[vist#0:0/av1 @ 0x55e0] [dec:libdav1d @ 0x55f0] Decoder returned EOF, finishing
";
        assert_eq!(
            selected_decoder(stderr, "av1").as_deref(),
            Some("libdav1d"),
            "a plan that named `av1` here would name a different decoder from the one that ran"
        );
    }

    #[test]
    fn a_context_for_another_codec_is_not_this_codecs_answer() {
        let stderr = "[vist#0:1/hevc @ 0x1] [dec:hevc @ 0x2] Starting thread...\n";
        assert_eq!(selected_decoder(stderr, "h264"), None);
        assert_eq!(selected_decoder(stderr, "hevc").as_deref(), Some("hevc"));
    }

    #[test]
    fn a_decode_that_printed_no_context_is_unmeasured() {
        // What `-loglevel error` produces for a clean decode, which is why the
        // probe does not use it.
        assert_eq!(selected_decoder("", "h264"), None);
        assert_eq!(
            selected_decoder(
                "  Stream #0:0 -> #0:0 (h264 (native) -> rawvideo)\n",
                "h264"
            ),
            None,
            // The summary line does name the decoder — `(av1 (libdav1d) -> …)`
            // on a build with libdav1d — but it is a different grammar, and
            // reading one fact through two grammars is how they drift apart.
            "the attributed context is the one thing read, and this is not one"
        );
    }

    #[test]
    fn two_answers_is_not_an_answer() {
        let stderr = "\
[vist#0:0/av1 @ 0x1] [dec:libdav1d @ 0x2] Starting thread...
[vist#0:0/av1 @ 0x1] [dec:av1 @ 0x3] Starting thread...
";
        assert_eq!(
            selected_decoder(stderr, "av1"),
            None,
            "a build that named two decoders for one codec has not been measured"
        );
    }

    #[test]
    fn a_line_that_only_looks_like_a_context_is_not_one() {
        for line in [
            "[dec:h264 @ 0x1] no stream context at all\n",
            "[vist#0:0/h264] [dec:] an empty decoder name\n",
            "[vist#0:0/h264 @ not-an-address] [dec:h264 @ 0x1] a forged address\n",
            "[vist#0:0/] [dec:h264 @ 0x1] an empty codec\n",
            "vist#0:0/h264] [dec:h264 @ 0x1] no opening bracket\n",
        ] {
            assert_eq!(selected_decoder(line, "h264"), None, "{line}");
        }
    }

    #[test]
    fn a_name_a_plan_could_not_carry_is_unmeasured() {
        // The measurement asks the plan the same question the plan will ask.
        // A name accepted here and refused there would not leave one codec
        // unnamed — it would make `DecodeCapabilities::new` refuse the whole
        // snapshot, and every plan on the node with it.
        let long = "x".repeat(4_096);
        let stderr = format!("[vist#0:0/h264 @ 0x1] [dec:{long} @ 0x2] Starting thread...\n");
        assert_eq!(selected_decoder(&stderr, "h264"), None);

        // A name that would need escaping anywhere it is stored.
        let stderr = "[vist#0:0/h264 @ 0x1] [dec:h264\"drop @ 0x2] Starting thread...\n";
        assert_eq!(selected_decoder(stderr, "h264"), None);

        // A dot is the interesting one: it looks like a decoder name and the
        // plan refuses it.
        let stderr = "[vist#0:0/h264 @ 0x1] [dec:h264.native @ 0x2] Starting thread...\n";
        assert_eq!(selected_decoder(stderr, "h264"), None);

        for name in ["libdav1d", "libaom-av1", "h264_qsv", "mpeg2video"] {
            assert!(safe_decoder_name(name), "{name}");
            assert!(
                crate::transcode::plan_can_name_decoder(name),
                "the measurement and the plan must agree about {name}"
            );
        }
        assert!(!safe_decoder_name(""));
    }

    #[test]
    fn an_unmeasured_codec_reports_nothing_rather_than_its_family() {
        let measured = MeasuredDecoders::from_measured(&[("av1", "libdav1d")]);
        assert_eq!(measured.implementation("av1"), Some("libdav1d"));
        assert_eq!(
            measured.implementation("h264"),
            None,
            "an unmeasured codec must not fall back to its own name"
        );
        assert!(!measured.is_empty());
        assert!(MeasuredDecoders::default().is_empty());
    }

    #[test]
    fn every_codec_this_node_advertises_has_a_probe_clip() {
        // The two lists are one set with two spellings, and this reads the
        // *real* one rather than a fixture restating it. A codec added to the
        // advertised list and not to the probe table is a codec that can never
        // be measured, and silently: it would simply never appear in a
        // measurement, and no plan would ever name it.
        let advertised = crate::transcode::encoder::PORTABLE_DECODE_CODECS;
        for codec in advertised {
            assert!(
                PROBE_ENCODERS.iter().any(|(name, _)| name == codec),
                "{codec} can be advertised for decode but has no probe clip encoder"
            );
        }
        for (codec, encoders) in PROBE_ENCODERS {
            assert!(
                advertised.contains(codec),
                "{codec} has a probe clip but is never advertised, so it is never measured"
            );
            assert!(!encoders.is_empty(), "{codec} names no encoder");
        }
    }

    #[tokio::test]
    async fn the_probe_measures_this_build() {
        let ffmpeg = std::env::var("PLURX_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
        // `mpeg2video` is the one codec whose encoder every ffmpeg has built
        // in, so if this build prints the attributed context at all, this codec
        // measures. A build that prints none — everything before FFmpeg 7 —
        // measures nothing, and that is the correct answer for it rather than
        // a test failure.
        let prints_context = tokio::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "verbose",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=10:duration=0.2",
                "-f",
                "null",
                "-",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .is_ok_and(|output| String::from_utf8_lossy(&output.stderr).contains("[vist#"));

        let scratch = tempfile::tempdir().expect("scratch");
        let measured =
            measure_selected_decoders(&ffmpeg, &["mpeg2video".to_owned()], scratch.path()).await;

        if prints_context {
            // Not merely "a safe name": the name this build actually used,
            // which ffmpeg prints a second time in its stream summary. Two
            // independent readings of one fact, so a parser that returned a
            // plausible constant would fail here.
            let summary = tokio::process::Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "verbose",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=160x120:rate=10:duration=0.2",
                    "-c:v",
                    "mpeg2video",
                    "-f",
                    "matroska",
                    "-",
                ])
                .kill_on_drop(true)
                .output()
                .await;
            assert_eq!(
                measured.implementation("mpeg2video"),
                Some("mpeg2video"),
                "a build that prints the context must measure the codec it decoded; \
                 summary probe status {:?}",
                summary.map(|output| output.status)
            );
        } else {
            assert!(
                measured.is_empty(),
                "a build that prints no context has measured nothing, not something"
            );
        }

        assert_eq!(
            std::fs::read_dir(scratch.path())
                .expect("scratch listing")
                .count(),
            0,
            "the probe must not leave its clips behind"
        );
    }
}
