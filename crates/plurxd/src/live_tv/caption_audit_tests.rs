//! Closed-caption audit of the live graphs: plan L-03 M3, review Q9 / F-ltv-8.
//!
//! ATSC 1.0 carries CEA-608 and CEA-708 as A/53 `cc_data` in the video
//! elementary stream, not as a subtitle stream, so `-sn -dn` in the live
//! command says nothing either way about whether they survive. This module
//! answers that from bytes instead:
//!
//! * a generated fixture — 10 s of 1080i MPEG-2 with B-frames, CC1 pop-on
//!   captions and one CEA-708 service-1 window per caption, `cc_data`
//!   injected in display order exactly where a broadcaster puts it — whose
//!   ground truth is known by construction;
//! * a decoder for what FFmpeg exports as `cc_data` from any output (the
//!   `subcc` side of the `movie` source), which recovers CC1 text and
//!   service-1 text independently of each other and of FFmpeg's own 608
//!   decoder;
//! * the real live argv (`live_ffmpeg_command_for_input`, tuner input, fed
//!   the fixture on stdin) run end to end into HLS segments.
//!
//! The boot probe reuses this fixture, decoder, and production graph path,
//! publishing proof only for the exact FFmpeg build and graph that preserves
//! both caption formats. The software graph and H.264 copy route also run in
//! `make unit`. Hardware audits remain available through the ignored
//! `live_caption_audit_on_this_node` driven by
//! `scripts/live-tv-caption-audit`. What this cannot prove — the
//! service ids real broadcasts use, and what each client shows — is the
//! execution log's GPT prompt.

use super::*;
use crate::live_tv_delivery::resolve_live_delivery;
use std::collections::BTreeMap;

/// 10 s at 30000/1001: 300 coded frame pictures.
const FIXTURE_FRAMES: usize = 300;
const FIXTURE_FRAME_SECONDS: f64 = 1001.0 / 30000.0;
/// ATSC A/53 `cc_count` for 29.97 Hz frame pictures: 9600 bit/s of
/// `cc_data`, twenty constructs per frame.
const CC_COUNT: usize = 20;
/// (first display frame of the caption's bytes, text). Upper-case letters,
/// digits and spaces only: 608's basic set differs from ASCII at 0x2A, 0x5C,
/// 0x5E-0x60 and 0x7B-0x7F, and the fixture must mean the same to both.
const FIXTURE_CAPTIONS: [(usize, &str); 4] = [
    (15, "PLURX CAPTION ONE"),
    (75, "SECOND LINE 2"),
    (135, "THIRD 333"),
    (195, "FOURTH AND LAST"),
];
const FIXTURE_ERASE_FRAME: usize = 270;
/// Each caption's service-1 packet starts this many frames after its CC1
/// bytes, so the two streams are not accidentally the same construct.
const SERVICE1_OFFSET: usize = 3;
/// A decoded cue may land this far from its ground truth, relative to the
/// first `cc_data` of its file: one field is 17 ms, and a deinterlacer that
/// emits one frame per field moves a cue by up to a field.
const CUE_TOLERANCE_SECONDS: f64 = 0.1;

/// (display frame, text) for each caption a fixture stream carries.
type FrameCues = Vec<(usize, &'static str)>;

#[derive(Clone, Debug, PartialEq)]
struct CaptionCue {
    at: f64,
    text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptionVerdict {
    Preserved,
    Partial,
    Dropped,
}

impl CaptionVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preserved => "preserved",
            Self::Partial => "partial",
            Self::Dropped => "dropped",
        }
    }
}

#[derive(Debug)]
struct CaptionTrack {
    /// Output frames that carried any `cc_data`, valid or padding.
    frames_with_cc: usize,
    cc1: Vec<CaptionCue>,
    service1: Vec<CaptionCue>,
}

fn odd_parity(byte: u8) -> u8 {
    let byte = byte & 0x7f;
    if byte.count_ones().is_multiple_of(2) {
        byte | 0x80
    } else {
        byte
    }
}

/// One CC1 byte pair per displayed frame. Each caption is the pop-on shape a
/// broadcaster sends — RCL, ENM, PAC row 15, text, EOC — with every control
/// code doubled, as 608 requires; the cue is shown at its first EOC.
fn cc1_pairs() -> (Vec<[u8; 2]>, FrameCues) {
    let mut pairs = vec![[0x80, 0x80]; FIXTURE_FRAMES];
    let mut cues = Vec::new();
    for (start, text) in FIXTURE_CAPTIONS {
        let mut sequence: Vec<[u8; 2]> = vec![
            [0x14, 0x20],
            [0x14, 0x20],
            [0x14, 0x2e],
            [0x14, 0x2e],
            [0x14, 0x60],
            [0x14, 0x60],
        ];
        for chunk in text.as_bytes().chunks(2) {
            sequence.push([chunk[0], chunk.get(1).copied().unwrap_or(0)]);
        }
        let shown = start + sequence.len();
        sequence.extend([[0x14, 0x2f], [0x14, 0x2f]]);
        for (offset, pair) in sequence.iter().enumerate() {
            pairs[start + offset] = [odd_parity(pair[0]), odd_parity(pair[1])];
        }
        cues.push((shown, text));
    }
    for frame in [FIXTURE_ERASE_FRAME, FIXTURE_ERASE_FRAME + 1] {
        pairs[frame] = [odd_parity(0x14), odd_parity(0x2c)];
    }
    (pairs, cues)
}

/// One DTVCC packet per caption, carrying one service-1 block: clear every
/// window, define window 0 visible, the text, ETX.
fn service1_packets() -> (BTreeMap<usize, Vec<u8>>, FrameCues) {
    let mut packets = BTreeMap::new();
    let mut cues = Vec::new();
    for (sequence, (start, text)) in FIXTURE_CAPTIONS.into_iter().enumerate() {
        let mut block = vec![0x88, 0xff, 0x98, 0x38, 0x00, 0x00, 0x00, 0x1f, 0x11];
        block.extend_from_slice(text.as_bytes());
        block.push(0x03);
        assert!(block.len() < 32, "a service block holds at most 31 bytes");
        let mut packet = vec![0, (1 << 5) | u8::try_from(block.len()).expect("block size")];
        packet.extend(block);
        if packet.len() % 2 == 1 {
            packet.push(0);
        }
        let sequence = u8::try_from(sequence % 4).expect("sequence number");
        packet[0] = (sequence << 6) | u8::try_from(packet.len() / 2).expect("packet size");
        let frame = start + SERVICE1_OFFSET;
        packets.insert(frame, packet);
        cues.push((frame, text));
    }
    (packets, cues)
}

/// The `cc_count` constructs for one displayed frame: the CC1 pair, a null
/// field-2 pair, the frame's DTVCC packet if it has one, then padding.
fn cc_data(frame: usize, cc1: &[[u8; 2]], service1: &BTreeMap<usize, Vec<u8>>) -> Vec<u8> {
    let mut data = Vec::with_capacity(CC_COUNT * 3);
    data.extend([0xfc, cc1[frame][0], cc1[frame][1]]);
    data.extend([0xfd, 0x80, 0x80]);
    if let Some(packet) = service1.get(&frame) {
        for (index, pair) in packet.chunks(2).enumerate() {
            data.extend([if index == 0 { 0xff } else { 0xfe }, pair[0], pair[1]]);
        }
    }
    assert!(data.len() <= CC_COUNT * 3, "one frame's cc_data overflowed");
    while data.len() < CC_COUNT * 3 {
        data.extend([0xfa, 0x00, 0x00]);
    }
    data
}

/// Insert ATSC `user_data` (GA94, `cc_data`) before each picture's first
/// slice, after its extensions. Pictures arrive in coded order; the captions
/// belong to display order, which is the GOP's base plus the picture's
/// `temporal_reference` — the reordering a real MPEG-2 broadcast makes a
/// decoder undo.
fn inject_a53(elementary: &[u8]) -> Vec<u8> {
    let (cc1, _) = cc1_pairs();
    let (service1, _) = service1_packets();
    let mut out = Vec::with_capacity(elementary.len() + FIXTURE_FRAMES * 80);
    let mut copied = 0;
    let mut gop_base = 0usize;
    let mut gop_pictures = 0usize;
    let mut display = None;
    let mut seen = vec![false; FIXTURE_FRAMES];
    let mut index = 0;
    while index + 6 <= elementary.len() {
        if elementary[index..index + 3] != [0, 0, 1] {
            index += 1;
            continue;
        }
        match elementary[index + 3] {
            0xb8 => {
                gop_base += gop_pictures;
                gop_pictures = 0;
            }
            0x00 => {
                let temporal = (usize::from(elementary[index + 4]) << 2)
                    | usize::from(elementary[index + 5] >> 6);
                gop_pictures += 1;
                display = Some(gop_base + temporal);
            }
            0x01 => {
                if let Some(frame) = display.take() {
                    assert!(
                        frame < FIXTURE_FRAMES,
                        "display index {frame} is past the fixture"
                    );
                    assert!(!seen[frame], "display index {frame} appeared twice");
                    seen[frame] = true;
                    out.extend_from_slice(&elementary[copied..index]);
                    out.extend_from_slice(b"\x00\x00\x01\xb2GA94\x03");
                    out.extend([0x40 | u8::try_from(CC_COUNT).expect("cc_count"), 0xff]);
                    out.extend(cc_data(frame, &cc1, &service1));
                    out.push(0xff);
                    copied = index;
                }
            }
            _ => {}
        }
        index += 3;
    }
    out.extend_from_slice(&elementary[copied..]);
    assert!(
        seen.iter().all(|seen| *seen),
        "every displayed frame must carry cc_data"
    );
    out
}

fn fixture_truth() -> (Vec<CaptionCue>, Vec<CaptionCue>) {
    let cue = |(frame, text): (usize, &str)| CaptionCue {
        at: frame as f64 * FIXTURE_FRAME_SECONDS,
        text: text.to_owned(),
    };
    (
        cc1_pairs().1.into_iter().map(cue).collect(),
        service1_packets().1.into_iter().map(cue).collect(),
    )
}

/// The boot caption probe runs these children; nobody is waiting on them.
const CAPTION_PROBE: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("Live TV caption probe");

async fn media_command(command: &mut tokio::process::Command) -> std::process::Output {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(
        Duration::from_secs(180),
        crate::process_control::output_job_owned(command, CAPTION_PROBE),
    )
    .await
    .expect("media command deadline")
    .expect("media command started");
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// A path the `movie` source can take unescaped. Test directories are
/// canonical temporary paths; anything else is copied into one first.
fn movie_safe(path: &Path) -> &str {
    let text = path.to_str().expect("UTF-8 media path");
    assert!(
        !text.contains(['\'', '\\', ':', ',', ';', '[', ']', ' ']),
        "{text} needs filtergraph escaping"
    );
    text
}

/// The captioned fixture, as an ATSC 1.0 transport stream: 1080i MPEG-2 with
/// B-frames and AC-3.
async fn captioned_fixture(system: &SystemInfo, directory: &Path) -> PathBuf {
    let mut generate = tokio::process::Command::new(&system.ffmpeg);
    generate.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1920x1080:rate=60000/1001",
        "-t",
        "10",
        "-vf",
        "tinterlace=mode=interleave_top",
        "-flags",
        "+ilme+ildct",
        "-c:v",
        "mpeg2video",
        "-b:v",
        "6M",
        "-bf",
        "2",
        "-g",
        "15",
        "-f",
        "mpeg2video",
        "pipe:1",
    ]);
    let elementary = media_command(&mut generate).await.stdout;
    let elementary_path = directory.join("captioned.m2v");
    tokio::fs::write(&elementary_path, inject_a53(&elementary))
        .await
        .expect("captioned elementary stream");
    let fixture = directory.join("captioned-608-708.ts");
    let mut mux = tokio::process::Command::new(&system.ffmpeg);
    mux.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-fflags",
        "+genpts",
        "-r",
        "30000/1001",
        "-f",
        "mpegvideo",
        "-i",
    ])
    .arg(&elementary_path)
    .args([
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "10",
        "-c:v",
        "copy",
        "-c:a",
        "ac3",
        "-b:a",
        "192k",
        "-f",
        "mpegts",
        "-y",
    ])
    .arg(&fixture);
    media_command(&mut mux).await;
    #[cfg(test)]
    if let Some(copy) = std::env::var_os("PLURX_CAPTION_FIXTURE_OUT") {
        tokio::fs::copy(&fixture, &copy)
            .await
            .expect("copying the caption fixture out");
    }
    fixture
}

/// Every `cc_data` payload FFmpeg exports from `media`, in presentation
/// order, as (seconds, raw constructs).
async fn caption_packets(system: &SystemInfo, media: &Path) -> Vec<(f64, Vec<u8>)> {
    let mut probe = tokio::process::Command::new(&system.ffprobe);
    probe.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        &format!("movie={}[out0+subcc]", movie_safe(media)),
        "-select_streams",
        "1",
        "-show_packets",
        "-show_data",
    ]);
    let text = String::from_utf8(media_command(&mut probe).await.stdout).expect("ffprobe text");
    let mut packets = Vec::new();
    for block in text.split("[PACKET]").skip(1) {
        let at = block
            .lines()
            .find_map(|line| line.strip_prefix("pts_time="))
            .and_then(|value| value.parse::<f64>().ok())
            .expect("a caption packet has a presentation time");
        let mut data = Vec::new();
        for line in block
            .split_once("data=")
            .map_or("", |(_, rest)| rest)
            .lines()
        {
            let Some((offset, rest)) = line.split_once(": ") else {
                continue;
            };
            if offset.len() != 8 || !offset.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            let hex: String = rest
                .split("  ")
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            for pair in hex.as_bytes().chunks(2) {
                let pair = std::str::from_utf8(pair).expect("hex digits");
                data.push(u8::from_str_radix(pair, 16).expect("hex byte"));
            }
        }
        packets.push((at, data));
    }
    packets.sort_by(|left, right| left.0.total_cmp(&right.0));
    packets
}

/// CC1 pop-on cues: text loaded after ENM, shown at EOC. A control pair
/// repeated in the very next pair is 608's redundancy and is dropped once.
fn decode_cc1(packets: &[(f64, Vec<u8>)]) -> Vec<CaptionCue> {
    let mut cues = Vec::new();
    let mut loading = String::new();
    let mut channel_one = true;
    let mut previous_control = None;
    for (at, data) in packets {
        for construct in data.chunks_exact(3) {
            let valid = construct[0] & 0x04 != 0;
            if !valid || construct[0] & 0x03 != 0 {
                continue;
            }
            let (first, second) = (construct[1] & 0x7f, construct[2] & 0x7f);
            if first == 0 && second == 0 {
                previous_control = None;
                continue;
            }
            if (0x10..=0x1f).contains(&first) {
                if previous_control == Some((first, second)) {
                    previous_control = None;
                    continue;
                }
                previous_control = Some((first, second));
                channel_one = first < 0x18;
                if !channel_one {
                    continue;
                }
                match (first, second) {
                    (0x14, 0x2e) => loading.clear(),
                    (0x14, 0x2f) => cues.push(CaptionCue {
                        at: *at,
                        text: std::mem::take(&mut loading),
                    }),
                    _ => {}
                }
                continue;
            }
            previous_control = None;
            if channel_one {
                loading.extend(
                    [first, second]
                        .into_iter()
                        .filter(|byte| *byte >= 0x20)
                        .map(char::from),
                );
            }
        }
    }
    cues
}

/// DTVCC packets, reassembled from type-3 (start) and type-2 (data)
/// constructs, with the time of the frame that started each.
fn dtvcc_packets(packets: &[(f64, Vec<u8>)]) -> Vec<(f64, Vec<u8>)> {
    let mut assembled: Vec<(f64, Vec<u8>)> = Vec::new();
    let mut open = false;
    for (at, data) in packets {
        for construct in data.chunks_exact(3) {
            let valid = construct[0] & 0x04 != 0;
            let kind = construct[0] & 0x03;
            if !valid || kind < 2 {
                continue;
            }
            if kind == 3 {
                assembled.push((*at, Vec::new()));
                open = true;
            }
            if open {
                if let Some((_, bytes)) = assembled.last_mut() {
                    bytes.extend_from_slice(&construct[1..]);
                }
            }
        }
    }
    assembled
}

/// Each service block in a DTVCC packet, as (service number, block bytes).
fn service_blocks(packet: &[u8]) -> Vec<(u8, &[u8])> {
    let Some((&header, rest)) = packet.split_first() else {
        return Vec::new();
    };
    let size = match usize::from(header & 0x3f) * 2 {
        0 => 128,
        size => size,
    };
    let mut body = &rest[..rest.len().min(size.saturating_sub(1))];
    let mut blocks = Vec::new();
    while let Some((&block_header, remainder)) = body.split_first() {
        let service = block_header >> 5;
        let length = usize::from(block_header & 0x1f);
        if service == 0 || length > remainder.len() {
            break;
        }
        // Extended service numbers (7) carry the real number in the next byte.
        blocks.push((service, &remainder[..length]));
        body = &remainder[length..];
    }
    blocks
}

/// Service-1 text: G0/G1 characters between window commands, one cue per ETX.
fn decode_service1(packets: &[(f64, Vec<u8>)]) -> Vec<CaptionCue> {
    let mut cues = Vec::new();
    for (at, packet) in dtvcc_packets(packets) {
        for (service, block) in service_blocks(&packet) {
            if service != 1 {
                continue;
            }
            let mut text = String::new();
            let mut index = 0;
            while index < block.len() {
                let code = block[index];
                index += match code {
                    0x03 => {
                        cues.push(CaptionCue {
                            at,
                            text: std::mem::take(&mut text),
                        });
                        1
                    }
                    0x00..=0x0f => 1,
                    0x10..=0x17 => 2,
                    0x18..=0x1f => 3,
                    0x20..=0x7f => {
                        text.push(char::from(code));
                        1
                    }
                    0x88..=0x8d => 2,
                    0x90 | 0x92 => 3,
                    0x91 => 4,
                    0x97 => 5,
                    0x98..=0x9f => 7,
                    0x80..=0x9f => 1,
                    _ => {
                        text.push(char::from(code));
                        1
                    }
                };
            }
        }
    }
    cues
}

async fn caption_track(system: &SystemInfo, media: &Path) -> CaptionTrack {
    let packets = caption_packets(system, media).await;
    CaptionTrack {
        frames_with_cc: packets.iter().filter(|(_, data)| !data.is_empty()).count(),
        cc1: decode_cc1(&packets),
        service1: decode_service1(&packets),
    }
}

/// Preserved: every cue, in order, with its text, within tolerance of its
/// ground-truth time relative to the first. Dropped: nothing decoded.
/// Anything between is partial and is reported as such.
fn verdict(decoded: &[CaptionCue], truth: &[CaptionCue]) -> CaptionVerdict {
    if decoded.is_empty() {
        return CaptionVerdict::Dropped;
    }
    let texts_match = decoded.len() == truth.len()
        && decoded
            .iter()
            .zip(truth)
            .all(|(decoded, truth)| decoded.text == truth.text);
    let timing_matches = texts_match
        && decoded.iter().zip(truth).all(|(cue, expected)| {
            let shown = cue.at - decoded[0].at;
            let wanted = expected.at - truth[0].at;
            (shown - wanted).abs() <= CUE_TOLERANCE_SECONDS
        });
    if timing_matches {
        CaptionVerdict::Preserved
    } else {
        CaptionVerdict::Partial
    }
}

/// The playback envelope of a client that decodes H.264 and AC-3 in
/// transport streams, so the planner can choose a video copy route.
fn h264_copy_client() -> LivePlaybackRequest {
    serde_json::from_value(serde_json::json!({
        "v": 1,
        "caps": {
            "v": 2,
            "video": [{"codec":"h264","profiles":["high"],"max_height":1080,"present":[]}],
            "audio": ["ac3", "aac"], "containers": ["mpegts", "fmp4"], "transports": ["hls"]
        },
        "hls_formats": [
            {"container":"mpegts","video":"h264","audio":"ac3"},
            {"container":"mpegts","video":"h264","audio":"aac"}
        ],
        "video_limits": [{"codec":"h264","max_width":1920,"max_height":1080,
            "max_frame_rate":{"num":60,"den":1},"interlaced":false}],
        "audio_limits": [{"codec":"ac3","max_channels":6},{"codec":"aac","max_channels":2}]
    }))
    .expect("live playback envelope")
}

/// One route of the audit matrix.
#[derive(Clone, Copy, Debug)]
struct GraphCase {
    encoder: Option<Encoder>,
    packaging: LivePackaging,
    deinterlace: LiveDeinterlaceOutput,
    max_height: u16,
}

struct GraphRun {
    status: std::process::ExitStatus,
    diagnostic: Option<&'static str>,
    /// The last lines FFmpeg wrote, kept only when it failed: an audit run on
    /// someone else's node has to say why a graph did not run, not only that.
    failure: Option<String>,
    argv: Vec<String>,
    track: Option<CaptionTrack>,
}

/// Plan the route with the production planner from the production probe of
/// `source`, build the production command for it, feed `source` on stdin as
/// the tuner would, and decode the captions in the segments it publishes.
///
/// `a53cc_override` rewrites the value after `-a53cc` and nothing else; it
/// exists for the VideoToolbox re-proof run and is `None` everywhere else.
async fn run_live_graph(
    system: &SystemInfo,
    source: &Path,
    case: GraphCase,
    a53cc_override: Option<&str>,
) -> GraphRun {
    let bytes = tokio::fs::read(source).await.expect("source bytes");
    let root = tempfile::tempdir().expect("graph root");
    let facts = probe_live_source(
        system,
        root.path(),
        &bytes[..bytes.len().min(SOURCE_PREFIX_BYTES)],
    )
    .await
    .expect("production source probe");
    let client = h264_copy_client();
    let mut delivery = resolve_live_delivery(
        &facts,
        case.encoder.is_none().then_some(&client),
        &LiveQualityPolicy {
            max_height: Some(case.max_height),
            max_bitrate_bps: None,
            deinterlace_output: case.deinterlace,
        },
        &LiveExecutionSupport {
            video_encode: true,
            audio_encode: true,
            tone_map: false,
        },
    )
    .expect("production delivery plan");
    let expected = if case.encoder.is_some() {
        LiveTrackAction::Encode
    } else {
        LiveTrackAction::Copy
    };
    assert_eq!(
        delivery.video_action, expected,
        "the planner chose {:?} for {case:?}: {:?}",
        delivery.video_action, delivery.reasons
    );
    delivery.packaging = case.packaging;
    let system = system.clone();
    #[cfg(test)]
    let mut system = system;
    #[cfg(test)]
    {
        system.encoders.forced_idr.qsv = true;
        system.encoders.forced_idr.nvenc = true;
    }
    let plan = LiveTvTranscodePlan::new(&system, delivery, case.encoder, None)
        .expect("live transcode plan");
    let output = root.path().join("live");
    tokio::fs::create_dir_all(&output)
        .await
        .expect("live output");
    let built = live_ffmpeg_command_for_input(&system, &plan, &output, LiveTvFfmpegInput::Tuner)
        .expect("live command");
    let mut argv: Vec<String> = built
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if let Some(value) = a53cc_override {
        let position = argv
            .iter()
            .position(|arg| arg == "-a53cc")
            .expect("the override needs an -a53cc argument to replace");
        argv[position + 1] = value.to_owned();
    }
    let mut command = tokio::process::Command::new(built.as_std().get_program());
    #[cfg(test)]
    command.env_clear();
    for (name, value) in built.as_std().get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        }
    }
    command
        .args(&argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) =
        crate::process_control::spawn_job_owned(&mut command, CAPTION_PROBE)
            .expect("live FFmpeg started");
    let mut stdin = child.stdin.take().expect("live FFmpeg stdin");
    let feed = async move {
        // A graph that dies early closes its stdin; that is its exit status's
        // story to tell, not a write error's.
        let _ = stdin.write_all(&bytes).await;
        drop(stdin);
    };
    let (_, finished) = tokio::join!(
        feed,
        tokio::time::timeout(Duration::from_secs(180), child.wait_with_output())
    );
    let finished = finished
        .expect("live FFmpeg deadline")
        .expect("live FFmpeg exit");
    let diagnostic = live_encoder_diagnostic(&finished.stderr);
    if !finished.status.success() {
        let stderr = String::from_utf8_lossy(&finished.stderr);
        let lines: Vec<&str> = stderr
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        return GraphRun {
            status: finished.status,
            diagnostic,
            failure: Some(lines[lines.len().saturating_sub(3)..].join(" / ")),
            argv,
            track: None,
        };
    }
    let playlist = tokio::fs::read_to_string(output.join("index.m3u8"))
        .await
        .expect("live playlist");
    let mut joined = Vec::new();
    if plan.delivery.packaging == LivePackaging::Fmp4 {
        joined.extend(
            tokio::fs::read(output.join("init.mp4"))
                .await
                .expect("fMP4 init segment"),
        );
    }
    let mut segments = 0;
    for line in playlist
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        joined.extend(
            tokio::fs::read(output.join(line))
                .await
                .expect("live segment"),
        );
        segments += 1;
    }
    assert!(
        segments > 0,
        "the live graph published no segment:\n{playlist}"
    );
    let extension = match plan.delivery.packaging {
        LivePackaging::Mpegts => "ts",
        LivePackaging::Fmp4 => "mp4",
    };
    let media = root.path().join(format!("published.{extension}"));
    tokio::fs::write(&media, joined)
        .await
        .expect("joined segments");
    let track = caption_track(&system, &media).await;
    GraphRun {
        status: finished.status,
        diagnostic,
        failure: None,
        argv,
        track: Some(track),
    }
}

/// A proof belongs to this process's FFmpeg build and this exact live graph.
/// The startup caller publishes it only after both independent decoders match
/// the fixture's known text and timing.
#[derive(Clone, Debug)]
pub(super) struct CaptionProof {
    pub(super) encoder: String,
    pub(super) packaging: LivePackaging,
    pub(super) deinterlace: LiveDeinterlaceOutput,
    pub(super) output_height: u16,
    pub(super) services: Vec<&'static str>,
}

pub(super) async fn probe_available_graphs(system: Arc<SystemInfo>) -> Vec<CaptionProof> {
    let root = tempfile::tempdir().expect("caption probe root");
    let fixture = captioned_fixture(&system, root.path()).await;
    let (cc1_truth, service1_truth) = fixture_truth();
    let mut proofs = Vec::new();
    for encoder in [
        Encoder::Software,
        Encoder::Qsv,
        Encoder::Vaapi,
        Encoder::Nvenc,
        Encoder::VideoToolbox,
    ] {
        if !system.encoders.available(encoder) {
            continue;
        }
        for packaging in [LivePackaging::Mpegts, LivePackaging::Fmp4] {
            for deinterlace in [LiveDeinterlaceOutput::Field, LiveDeinterlaceOutput::Frame] {
                for output_height in [720, 1080] {
                    let case = GraphCase {
                        encoder: Some(encoder),
                        packaging,
                        deinterlace,
                        max_height: output_height,
                    };
                    let system = Arc::clone(&system);
                    let fixture = fixture.clone();
                    let result =
                        tokio::spawn(
                            async move { run_live_graph(&system, &fixture, case, None).await },
                        )
                        .await;
                    let Ok(run) = result else {
                        tracing::warn!(
                            ?encoder,
                            ?packaging,
                            ?deinterlace,
                            output_height,
                            "caption graph probe failed"
                        );
                        continue;
                    };
                    let Some(track) = run.track else {
                        tracing::warn!(?encoder, ?packaging, ?deinterlace, output_height, diagnostic = ?run.diagnostic, failure = ?run.failure, "caption graph did not publish decodable output");
                        continue;
                    };
                    let cc1 = verdict(&track.cc1, &cc1_truth);
                    let service1 = verdict(&track.service1, &service1_truth);
                    tracing::info!(?encoder, ?packaging, ?deinterlace, output_height, frames_with_cc = track.frames_with_cc, exit = %run.status, argv = %run.argv.join(" "), cc1 = cc1.as_str(), service1 = service1.as_str(), "caption graph proof");
                    if cc1 == CaptionVerdict::Preserved && service1 == CaptionVerdict::Preserved {
                        proofs.push(CaptionProof {
                            encoder: encoder.label().to_owned(),
                            packaging,
                            deinterlace,
                            output_height,
                            services: vec!["CC1", "SERVICE1"],
                        });
                    }
                }
            }
        }
    }
    proofs
}

#[cfg(test)]
fn report(label: &str, case: GraphCase, run: &GraphRun) -> (CaptionVerdict, CaptionVerdict) {
    let (cc1_truth, service1_truth) = fixture_truth();
    let (cc1, service1) = run.track.as_ref().map_or(
        (CaptionVerdict::Dropped, CaptionVerdict::Dropped),
        |track| {
            (
                verdict(&track.cc1, &cc1_truth),
                verdict(&track.service1, &service1_truth),
            )
        },
    );
    println!(
        "caption-audit: route={label} packaging={:?} deinterlace={} max_height={} exit={} diagnostic={:?} frames_with_cc={} cc1={} service1={}",
        case.packaging,
        case.deinterlace.as_str(),
        case.max_height,
        run.status,
        run.diagnostic,
        run.track.as_ref().map_or(0, |track| track.frames_with_cc),
        cc1.as_str(),
        service1.as_str(),
    );
    if let Some(failure) = &run.failure {
        println!("caption-audit-failure: route={label} {failure}");
    }
    if let Some(track) = &run.track {
        println!(
            "caption-audit-cues: route={label} cc1={:?} service1={:?}",
            track.cc1, track.service1
        );
    }
    println!("caption-audit-argv: route={label} {}", run.argv.join(" "));
    (cc1, service1)
}

/// An H.264 source carrying the fixture's captions, as an ATSC 1.0 H.264
/// subchannel would, A/53 kept by x264. Deinterlaced one frame per frame:
/// FFmpeg builds before the deinterlacers learned to split `cc_data` between
/// fields copy it onto both (measured on 5.1), which would make the copy
/// route's *source* the defect under test.
#[cfg(test)]
async fn h264_captioned_source(system: &SystemInfo, fixture: &Path, directory: &Path) -> PathBuf {
    let source = directory.join("captioned-h264.ts");
    let mut encode = tokio::process::Command::new(&system.ffmpeg);
    encode
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(fixture)
        .args([
            "-vf",
            "bwdif=mode=send_frame:parity=auto:deint=interlaced,scale=-2:720,format=yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
            "-a53cc",
            "1",
            "-c:a",
            "copy",
            "-f",
            "mpegts",
            "-y",
        ])
        .arg(&source);
    media_command(&mut encode).await;
    source
}

#[cfg(test)]
fn test_system() -> SystemInfo {
    SystemInfo {
        ffmpeg: plurx_core::testfixtures::ffmpeg(),
        ffprobe: plurx_core::testfixtures::ffprobe(),
        ..SystemInfo::default()
    }
}

#[tokio::test]
async fn the_caption_fixture_carries_608_and_708() {
    plurx_core::testfixtures::require_ffmpeg();
    let system = test_system();
    let root = crate::test_tempdir().expect("fixture root");
    let fixture = captioned_fixture(&system, root.path()).await;
    let track = caption_track(&system, &fixture).await;
    let (cc1_truth, service1_truth) = fixture_truth();
    assert_eq!(track.frames_with_cc, FIXTURE_FRAMES);
    assert_eq!(
        verdict(&track.cc1, &cc1_truth),
        CaptionVerdict::Preserved,
        "{track:?}"
    );
    assert_eq!(
        verdict(&track.service1, &service1_truth),
        CaptionVerdict::Preserved,
        "{track:?}"
    );
    // Display order, not coded order: the fixture has B-frames, so a cue
    // assembled from bytes read in coded order would come out scrambled.
    assert_eq!(
        track
            .cc1
            .iter()
            .map(|cue| cue.text.as_str())
            .collect::<Vec<_>>(),
        FIXTURE_CAPTIONS.map(|(_, text)| text)
    );
}

/// The decoder is what every verdict rests on, so it must be able to say
/// "dropped" and "partial" as well as "preserved".
#[test]
fn the_caption_decoder_tells_dropped_and_partial_from_preserved() {
    let (packets, _) = service1_packets();
    let (pairs, _) = cc1_pairs();
    let frames: Vec<(f64, Vec<u8>)> = (0..FIXTURE_FRAMES)
        .map(|frame| {
            (
                frame as f64 * FIXTURE_FRAME_SECONDS,
                cc_data(frame, &pairs, &packets),
            )
        })
        .collect();
    let (cc1_truth, service1_truth) = fixture_truth();
    assert_eq!(packets.len(), FIXTURE_CAPTIONS.len());
    assert_eq!(
        verdict(&decode_cc1(&frames), &cc1_truth),
        CaptionVerdict::Preserved
    );
    assert_eq!(
        verdict(&decode_service1(&frames), &service1_truth),
        CaptionVerdict::Preserved
    );

    // Every construct marked invalid, as `-a53cc 0` leaves nothing at all.
    let silenced: Vec<(f64, Vec<u8>)> = frames
        .iter()
        .map(|(at, data)| {
            let mut data = data.clone();
            for construct in data.chunks_exact_mut(3) {
                construct[0] &= !0x04;
            }
            (*at, data)
        })
        .collect();
    assert_eq!(
        verdict(&decode_cc1(&silenced), &cc1_truth),
        CaptionVerdict::Dropped
    );
    assert_eq!(
        verdict(&decode_service1(&silenced), &service1_truth),
        CaptionVerdict::Dropped
    );

    // Every pair duplicated, as a deinterlacer that copies a frame's
    // cc_data onto both of its fields would: control codes survive (608
    // expects them twice) but every character doubles.
    let doubled: Vec<(f64, Vec<u8>)> = frames
        .iter()
        .flat_map(|(at, data)| {
            [
                (*at, data.clone()),
                (*at + FIXTURE_FRAME_SECONDS / 2.0, data.clone()),
            ]
        })
        .collect();
    assert_eq!(
        verdict(&decode_cc1(&doubled), &cc1_truth),
        CaptionVerdict::Partial
    );

    // A frame lost in the middle of the second caption.
    let (second, _) = FIXTURE_CAPTIONS[1];
    let gapped: Vec<(f64, Vec<u8>)> = frames
        .iter()
        .enumerate()
        .filter(|(frame, _)| *frame != second + 8)
        .map(|(_, frame)| frame.clone())
        .collect();
    assert_eq!(
        verdict(&decode_cc1(&gapped), &cc1_truth),
        CaptionVerdict::Partial
    );
}

/// x264 is the graph every node has; it runs here, on every build, through
/// the production planner and argv, in both deinterlace modes. `send_field`
/// doubles the frame rate, which is exactly where a caption stream would be
/// duplicated or halved if the deinterlacer did not repack `cc_data`.
#[tokio::test]
async fn the_software_live_graph_carries_608_and_708_through_both_deinterlace_modes() {
    plurx_core::testfixtures::require_ffmpeg();
    let system = test_system();
    let root = crate::test_tempdir().expect("fixture root");
    let fixture = captioned_fixture(&system, root.path()).await;
    for (deinterlace, packaging) in [
        (LiveDeinterlaceOutput::Field, LivePackaging::Mpegts),
        (LiveDeinterlaceOutput::Frame, LivePackaging::Fmp4),
    ] {
        let case = GraphCase {
            encoder: Some(Encoder::Software),
            packaging,
            deinterlace,
            max_height: 720,
        };
        let run = run_live_graph(&system, &fixture, case, None).await;
        assert!(run.status.success(), "{}", run.argv.join(" "));
        assert!(run
            .argv
            .iter()
            .any(|arg| arg.contains(&format!("send_{}", deinterlace.as_str()))));
        assert_eq!(
            report("software", case, &run),
            (CaptionVerdict::Preserved, CaptionVerdict::Preserved)
        );
    }
}

/// A copied H.264 video stream carries its SEI untouched; checked rather than
/// assumed, because the claim the review warned about is exactly this kind.
#[tokio::test]
async fn a_copied_h264_route_carries_608_and_708() {
    plurx_core::testfixtures::require_ffmpeg();
    let system = test_system();
    let root = crate::test_tempdir().expect("fixture root");
    let fixture = captioned_fixture(&system, root.path()).await;
    let source = h264_captioned_source(&system, &fixture, root.path()).await;
    let (cc1_truth, service1_truth) = fixture_truth();
    let precondition = caption_track(&system, &source).await;
    assert_eq!(
        verdict(&precondition.cc1, &cc1_truth),
        CaptionVerdict::Preserved
    );
    assert_eq!(
        verdict(&precondition.service1, &service1_truth),
        CaptionVerdict::Preserved
    );
    let case = GraphCase {
        encoder: None,
        packaging: LivePackaging::Mpegts,
        deinterlace: LiveDeinterlaceOutput::Field,
        max_height: 1080,
    };
    let run = run_live_graph(&system, &source, case, None).await;
    assert!(run.status.success(), "{}", run.argv.join(" "));
    assert!(run
        .argv
        .windows(2)
        .any(|pair| pair[0] == "-c:v" && pair[1] == "copy"));
    assert_eq!(
        report("copy", case, &run),
        (CaptionVerdict::Preserved, CaptionVerdict::Preserved)
    );
}

/// The per-graph table in HDHOMERUN-LIVE-TV-STATUS.md and `live_caption_args`
/// must say the same thing: the doc is where the audit result is recorded,
/// and the flags are what production does with it.
#[test]
fn caption_args_per_encoder_match_the_audit_table() {
    let status = include_str!("../../../../docs/features/HDHOMERUN-LIVE-TV-STATUS.md");
    let table: BTreeMap<String, String> = status
        .lines()
        .filter_map(|line| line.strip_prefix("| caption-audit | "))
        .map(|row| {
            let cells: Vec<&str> = row.split(" | ").map(str::trim).collect();
            (
                cells[0].trim_matches('`').to_owned(),
                cells[2].trim_matches('`').to_owned(),
            )
        })
        .collect();
    for encoder in [
        Encoder::Software,
        Encoder::Qsv,
        Encoder::Vaapi,
        Encoder::Nvenc,
        Encoder::VideoToolbox,
    ] {
        let recorded = table
            .get(audit_name(encoder))
            .unwrap_or_else(|| panic!("the audit table has no row for {}", audit_name(encoder)));
        let args = live_caption_args(encoder).join(" ");
        let args = if args.is_empty() {
            "none".to_owned()
        } else {
            args
        };
        assert_eq!(&args, recorded, "{} caption flags", audit_name(encoder));
    }
}

/// The audit's and the status table's name for an encoder family.
#[cfg(test)]
fn audit_name(encoder: Encoder) -> &'static str {
    match encoder {
        Encoder::Software => "software",
        Encoder::Qsv => "qsv",
        Encoder::Vaapi => "vaapi",
        Encoder::Nvenc => "nvenc",
        Encoder::VideoToolbox => "videotoolbox",
    }
}

#[cfg(test)]
fn audit_encoder(name: &str) -> Option<Option<Encoder>> {
    match name {
        "copy" => Some(None),
        "software" => Some(Some(Encoder::Software)),
        "qsv" => Some(Some(Encoder::Qsv)),
        "vaapi" => Some(Some(Encoder::Vaapi)),
        "nvenc" => Some(Some(Encoder::Nvenc)),
        "videotoolbox" => Some(Some(Encoder::VideoToolbox)),
        _ => None,
    }
}

/// The per-graph audit on this node's hardware and FFmpeg.
///
/// `PLURX_CAPTION_AUDIT_ENCODERS` names the routes (`software`, `qsv`,
/// `vaapi`, `nvenc`, `videotoolbox`, `copy`); each runs through both
/// packagings and both deinterlace modes. `PLURX_CAPTION_AUDIT_A53CC=1`
/// re-runs VideoToolbox with `-a53cc 1` to re-prove (or not) the SEI failure
/// its workaround exists for; that run reports and never asserts.
#[tokio::test]
#[ignore = "per-node hardware audit; run scripts/live-tv-caption-audit"]
async fn live_caption_audit_on_this_node() {
    plurx_core::testfixtures::require_ffmpeg();
    let system = test_system();
    let routes =
        std::env::var("PLURX_CAPTION_AUDIT_ENCODERS").unwrap_or_else(|_| "software".into());
    let a53cc = std::env::var("PLURX_CAPTION_AUDIT_A53CC").ok();
    let root = crate::test_tempdir().expect("fixture root");
    let fixture = captioned_fixture(&system, root.path()).await;
    let mut failures = Vec::new();
    for name in routes
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let encoder = audit_encoder(name).unwrap_or_else(|| panic!("unknown route {name}"));
        let (source, deinterlace_modes) = if encoder.is_none() {
            let source = h264_captioned_source(&system, &fixture, root.path()).await;
            let track = caption_track(&system, &source).await;
            let (cc1_truth, service1_truth) = fixture_truth();
            let precondition = (
                verdict(&track.cc1, &cc1_truth),
                verdict(&track.service1, &service1_truth),
            );
            println!(
                "caption-audit: route=copy-source cc1={} service1={}",
                precondition.0.as_str(),
                precondition.1.as_str()
            );
            if precondition != (CaptionVerdict::Preserved, CaptionVerdict::Preserved) {
                failures.push(format!("the copy route's H.264 source: {precondition:?}"));
                continue;
            }
            (source, vec![LiveDeinterlaceOutput::Field])
        } else {
            (
                fixture.clone(),
                vec![LiveDeinterlaceOutput::Field, LiveDeinterlaceOutput::Frame],
            )
        };
        let override_value = (encoder == Some(Encoder::VideoToolbox))
            .then_some(a53cc.as_deref())
            .flatten();
        for deinterlace in deinterlace_modes {
            for packaging in [LivePackaging::Mpegts, LivePackaging::Fmp4] {
                let case = GraphCase {
                    encoder,
                    packaging,
                    deinterlace,
                    max_height: if encoder.is_some() { 720 } else { 1080 },
                };
                let run = run_live_graph(&system, &source, case, override_value).await;
                let label = match override_value {
                    Some(value) => format!("{name}(a53cc={value})"),
                    None => name.to_owned(),
                };
                let verdicts = report(&label, case, &run);
                if override_value.is_some() {
                    continue;
                }
                let expected = if encoder == Some(Encoder::VideoToolbox) {
                    (CaptionVerdict::Dropped, CaptionVerdict::Dropped)
                } else {
                    (CaptionVerdict::Preserved, CaptionVerdict::Preserved)
                };
                if !run.status.success() || verdicts != expected {
                    failures.push(format!(
                        "{label} {case:?}: {verdicts:?}, expected {expected:?}"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "caption audit failures:\n{}",
        failures.join("\n")
    );
}

/// Which 608 channels and 708 services a capture carries, and a sample of
/// each one's text. For a broadcast capture: the service ids an advertised
/// rendition may name come from here, never from a constant.
#[tokio::test]
#[ignore = "summarises PLURX_CAPTION_ANALYZE; run scripts/live-tv-caption-audit analyze FILE"]
async fn summarise_the_captions_in_a_capture() {
    let path = std::env::var_os("PLURX_CAPTION_ANALYZE").expect("PLURX_CAPTION_ANALYZE");
    let system = test_system();
    let root = crate::test_tempdir().expect("analysis root");
    let copy = root.path().join("capture.ts");
    tokio::fs::copy(&path, &copy)
        .await
        .expect("copying the capture");
    let packets = caption_packets(&system, &copy).await;
    // (field, data channel) → printable characters, as 608 sends them.
    let mut channels: BTreeMap<String, String> = BTreeMap::new();
    let mut current = [1u8, 1u8];
    for (_, data) in &packets {
        for construct in data.chunks_exact(3) {
            let kind = construct[0] & 0x03;
            if construct[0] & 0x04 == 0 || kind > 1 {
                continue;
            }
            let field = usize::from(kind);
            let (first, second) = (construct[1] & 0x7f, construct[2] & 0x7f);
            if (0x10..=0x1f).contains(&first) {
                current[field] = if first < 0x18 { 1 } else { 2 };
                continue;
            }
            let name = format!("CC{}", field * 2 + usize::from(current[field]));
            let text = channels.entry(name).or_default();
            text.extend(
                [first, second]
                    .into_iter()
                    .filter(|byte| *byte >= 0x20)
                    .map(char::from),
            );
        }
    }
    let mut services: BTreeMap<u8, String> = BTreeMap::new();
    for (_, packet) in dtvcc_packets(&packets) {
        for (service, block) in service_blocks(&packet) {
            services.entry(service).or_default().extend(
                block
                    .iter()
                    .filter(|byte| (0x20..0x80).contains(*byte))
                    .map(|byte| char::from(*byte)),
            );
        }
    }
    println!("caption-capture: frames_with_cc={}", packets.len());
    for (name, text) in &channels {
        let sample: String = text.chars().take(160).collect();
        println!(
            "caption-capture: 608 {name} chars={} sample={sample:?}",
            text.len()
        );
    }
    for (service, text) in &services {
        let sample: String = text.chars().take(160).collect();
        println!(
            "caption-capture: 708 SERVICE{service} chars={} sample={sample:?}",
            text.len()
        );
    }
}
